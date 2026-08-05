use std::collections::HashMap;

use reimer_ast::{
    self as ast, BinaryOperator, Expression, FieldName, Item, Statement, UnaryOperator,
};
use reimer_diagnostics::Span;

/// Statically known quantity associated with one allocation-like operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocationQuantity {
    /// The operation reserves exactly this many bytes.
    Exact(u128),
    /// The operation can initialize no more than this many bytes.
    AtMost(u128),
    /// This many bytes are reserved on every iteration of an enclosing loop.
    PerIteration(u128),
    /// The amount depends on a runtime value or unknown element layout.
    Dynamic,
}

/// A source-level allocation estimate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllocationEstimate {
    /// Allocation-like operation name.
    pub operation: String,
    /// Best-effort allocator identity.
    pub allocator: String,
    /// Known reservation quantity.
    pub quantity: AllocationQuantity,
    /// Complete call span.
    pub span: Span,
    /// Assumptions and limitations of the estimate.
    pub explanation: String,
}

/// Aggregated reservations for one allocator in one function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllocatorSummary {
    /// Function containing the operations.
    pub function: String,
    /// Best-effort allocator identity.
    pub allocator: String,
    /// Sum of exact, non-loop reservation sites.
    pub known_bytes_per_call: u128,
    /// Sum reserved on each iteration across syntactic loop sites.
    pub known_bytes_per_iteration: u128,
    /// Number of operations whose size is not statically known.
    pub dynamic_operations: usize,
    /// Function name source span.
    pub span: Span,
}

pub(crate) fn estimate(program: &ast::Program) -> (Vec<AllocationEstimate>, Vec<AllocatorSummary>) {
    let mut allocations = Vec::new();
    let mut summaries = Vec::new();
    for function in functions(program) {
        let mut estimator = Estimator::new(function);
        estimator.scan_block(&function.body, 0);
        let (function_allocations, function_summaries) = estimator.finish();
        allocations.extend(function_allocations);
        summaries.extend(function_summaries);
    }
    (allocations, summaries)
}

fn functions(program: &ast::Program) -> impl Iterator<Item = &ast::Function> {
    program.items.iter().flat_map(|item| match item {
        Item::Function(function) => std::slice::from_ref(function),
        Item::Impl(declaration) => declaration.methods.as_slice(),
        _ => &[],
    })
}

#[derive(Debug, Clone)]
struct AllocatorIdentity {
    label: String,
}

#[derive(Debug, Clone, Copy)]
enum AllocationSource {
    Argument(usize),
    CallerFixedBuffer,
    Destination,
}

struct AllocationSpecification {
    source: AllocationSource,
    amount: Option<u128>,
    upper_bound: bool,
    explanation: &'static str,
}

struct Estimator<'function> {
    function: &'function ast::Function,
    allocators: HashMap<String, AllocatorIdentity>,
    allocations: Vec<AllocationEstimate>,
}

impl<'function> Estimator<'function> {
    fn new(function: &'function ast::Function) -> Self {
        Self {
            function,
            allocators: HashMap::new(),
            allocations: Vec::new(),
        }
    }

    fn finish(self) -> (Vec<AllocationEstimate>, Vec<AllocatorSummary>) {
        let mut by_allocator: HashMap<String, AllocatorSummary> = HashMap::new();
        for estimate in &self.allocations {
            let summary = by_allocator
                .entry(estimate.allocator.clone())
                .or_insert_with(|| AllocatorSummary {
                    function: self.function.name.name.clone(),
                    allocator: estimate.allocator.clone(),
                    known_bytes_per_call: 0,
                    known_bytes_per_iteration: 0,
                    dynamic_operations: 0,
                    span: self.function.name.span,
                });
            match estimate.quantity {
                AllocationQuantity::Exact(bytes) | AllocationQuantity::AtMost(bytes) => {
                    summary.known_bytes_per_call =
                        summary.known_bytes_per_call.saturating_add(bytes);
                }
                AllocationQuantity::PerIteration(bytes) => {
                    summary.known_bytes_per_iteration =
                        summary.known_bytes_per_iteration.saturating_add(bytes);
                }
                AllocationQuantity::Dynamic => {
                    summary.dynamic_operations = summary.dynamic_operations.saturating_add(1);
                }
            }
        }
        let mut summaries: Vec<_> = by_allocator.into_values().collect();
        summaries.sort_by(|left, right| left.allocator.cmp(&right.allocator));
        (self.allocations, summaries)
    }

    fn scan_block(&mut self, block: &ast::Block, loop_depth: usize) {
        for statement in &block.statements {
            match statement {
                Statement::Let(binding) => {
                    self.record_allocator_binding(&binding.name.name, &binding.initializer);
                    self.scan_expression(&binding.initializer, loop_depth);
                }
                Statement::Expression(statement) => {
                    self.scan_expression(&statement.expression, loop_depth);
                }
                Statement::Defer(statement) => {
                    self.scan_expression(&statement.action, loop_depth);
                }
                Statement::Return(statement) => {
                    if let Some(value) = &statement.value {
                        self.scan_expression(value, loop_depth);
                    }
                }
                Statement::While(statement) => {
                    self.scan_expression(&statement.condition, loop_depth);
                    self.scan_block(&statement.body, loop_depth.saturating_add(1));
                }
                Statement::For(statement) => {
                    self.scan_expression(&statement.iterable, loop_depth);
                    self.scan_block(&statement.body, loop_depth.saturating_add(1));
                }
                Statement::Break(statement) => {
                    if let Some(value) = &statement.value {
                        self.scan_expression(value, loop_depth);
                    }
                }
                Statement::Continue(_) => {}
            }
        }
        if let Some(tail) = &block.tail {
            self.scan_expression(tail, loop_depth);
        }
    }

    fn scan_expression(&mut self, expression: &Expression, loop_depth: usize) {
        match expression {
            Expression::Call(call) => {
                self.record_allocation(call, loop_depth);
                self.scan_expression(&call.callee, loop_depth);
                for argument in &call.arguments {
                    self.scan_expression(argument, loop_depth);
                }
            }
            Expression::Tuple(tuple) => {
                for element in &tuple.elements {
                    self.scan_expression(element, loop_depth);
                }
            }
            Expression::PackExpansion(expansion) => {
                self.scan_expression(&expansion.template, loop_depth);
            }
            Expression::Array(array) => match &array.kind {
                reimer_ast::ArrayExpressionKind::List(elements) => {
                    for element in elements {
                        self.scan_expression(element, loop_depth);
                    }
                }
                reimer_ast::ArrayExpressionKind::Repeat { value, length } => {
                    self.scan_expression(value, loop_depth);
                    self.scan_expression(length, loop_depth);
                }
            },
            Expression::Struct(structure) => {
                for field in &structure.fields {
                    self.scan_expression(&field.value, loop_depth);
                }
            }
            Expression::FormattedString(formatted) => {
                for fragment in &formatted.fragments {
                    if let ast::FormattedStringFragment::Display(expression)
                    | ast::FormattedStringFragment::Debug(expression) = fragment
                    {
                        self.scan_expression(expression, loop_depth);
                    }
                }
            }
            Expression::Unary(unary) => self.scan_expression(&unary.operand, loop_depth),
            Expression::Binary(binary) => {
                self.scan_expression(&binary.left, loop_depth);
                self.scan_expression(&binary.right, loop_depth);
            }
            Expression::If(conditional) => {
                self.scan_expression(&conditional.condition, loop_depth);
                self.scan_block(&conditional.then_branch, loop_depth);
                if let Some(else_branch) = &conditional.else_branch {
                    self.scan_expression(else_branch, loop_depth);
                }
            }
            Expression::Match(matching) => {
                self.scan_expression(&matching.scrutinee, loop_depth);
                for arm in &matching.arms {
                    if let Some(guard) = &arm.guard {
                        self.scan_expression(guard, loop_depth);
                    }
                    self.scan_expression(&arm.body, loop_depth);
                }
            }
            Expression::Loop(looping) => {
                self.scan_block(&looping.body, loop_depth.saturating_add(1));
            }
            Expression::Unsafe(block) | Expression::Block(block) => {
                self.scan_block(block, loop_depth);
            }
            Expression::Assignment(assignment) => {
                self.scan_expression(&assignment.target, loop_depth);
                self.scan_expression(&assignment.value, loop_depth);
            }
            Expression::Cast(cast) => self.scan_expression(&cast.value, loop_depth),
            Expression::Field(field) => self.scan_expression(&field.base, loop_depth),
            Expression::Index(index) => {
                self.scan_expression(&index.base, loop_depth);
                for index in &index.indices {
                    self.scan_expression(index, loop_depth);
                }
            }
            Expression::Try { value, .. } => self.scan_expression(value, loop_depth),
            Expression::GenericFunction(function) => {
                for argument in &function.generic_arguments {
                    if let ast::GenericArgument::Const(value) = argument {
                        self.scan_expression(value, loop_depth);
                    }
                }
            }
            Expression::Integer(_)
            | Expression::Float(_)
            | Expression::Character(_)
            | Expression::String(_)
            | Expression::CString(_)
            | Expression::Boolean(_)
            | Expression::Unit(_)
            | Expression::Path(_) => {}
        }
    }

    fn record_allocator_binding(&mut self, binding: &str, initializer: &Expression) {
        let initializer = unwrap_try(initializer);
        let Expression::Call(call) = initializer else {
            return;
        };
        let Some(operation) = call_name(&call.callee) else {
            return;
        };
        let label = match operation {
            "general_allocator" => Some(format!("general allocator `{binding}`")),
            "page_allocator" => Some(format!("page allocator `{binding}`")),
            "init" if callee_contains_name(&call.callee, "ArenaAllocator") => {
                Some(format!("arena `{binding}`"))
            }
            "init" if callee_contains_name(&call.callee, "FixedBufferAllocator") => {
                Some(format!("fixed buffer `{binding}`"))
            }
            "allocator" => root_name(&call.callee)
                .and_then(|name| self.allocators.get(name))
                .map(|identity| identity.label.clone()),
            _ => None,
        };
        if let Some(label) = label {
            self.allocators
                .insert(binding.to_owned(), AllocatorIdentity { label });
        }
    }

    fn record_allocation(&mut self, call: &ast::CallExpression, loop_depth: usize) {
        let Some(operation) = call_name(&call.callee) else {
            return;
        };
        let Some(specification) = allocation_specification(call, operation) else {
            return;
        };
        let quantity = match (specification.amount, loop_depth) {
            (Some(bytes), 0) if specification.upper_bound => AllocationQuantity::AtMost(bytes),
            (Some(bytes), 0) => AllocationQuantity::Exact(bytes),
            (Some(bytes), _) => AllocationQuantity::PerIteration(bytes),
            (None, _) => AllocationQuantity::Dynamic,
        };
        let allocator = match specification.source {
            AllocationSource::Argument(index) => call.arguments.get(index).map_or_else(
                || "implicit allocator".to_owned(),
                |argument| self.allocator_label(argument),
            ),
            AllocationSource::CallerFixedBuffer => "caller-provided fixed buffer".to_owned(),
            AllocationSource::Destination => root_name(&call.callee).map_or_else(
                || "allocator retained by destination String".to_owned(),
                |destination| format!("allocator retained by String `{destination}`"),
            ),
        };
        let explanation = if loop_depth == 0 {
            format!(
                "static estimate: {}; control-flow frequency is not modeled",
                specification.explanation
            )
        } else {
            format!(
                "static estimate per loop iteration: {}; iteration count is not known",
                specification.explanation
            )
        };
        self.allocations.push(AllocationEstimate {
            operation: operation.to_owned(),
            allocator,
            quantity,
            span: call.span,
            explanation,
        });
    }

    fn allocator_label(&self, expression: &Expression) -> String {
        if let Some(name) = root_name(expression) {
            return self.allocators.get(name).map_or_else(
                || format!("allocator `{name}`"),
                |identity| identity.label.clone(),
            );
        }
        let expression = unwrap_try(expression);
        if let Expression::Call(call) = expression
            && call_name(&call.callee) == Some("allocator")
            && let Some(name) = root_name(&call.callee)
        {
            return self.allocators.get(name).map_or_else(
                || format!("allocator from `{name}`"),
                |identity| identity.label.clone(),
            );
        }
        "unknown allocator".to_owned()
    }
}

fn allocation_specification(
    call: &ast::CallExpression,
    operation: &str,
) -> Option<AllocationSpecification> {
    let exact = |allocator_index, amount, explanation| AllocationSpecification {
        source: AllocationSource::Argument(allocator_index),
        amount,
        upper_bound: false,
        explanation,
    };
    match operation {
        "allocate_bytes" => Some(exact(0, argument_bytes(call, 1), "byte reservation")),
        "allocate_aligned_bytes" => Some(exact(
            0,
            argument_bytes(call, 1),
            "aligned byte reservation; allocator padding is excluded",
        )),
        "read" | "read_exact" | "read_line" | "read_to_end" => Some(exact(
            0,
            argument_bytes(call, call.arguments.len().saturating_sub(1)),
            "input buffer reservation",
        )),
        "init" if callee_contains_name(&call.callee, "FixedBufferAllocator") => {
            Some(AllocationSpecification {
                source: AllocationSource::CallerFixedBuffer,
                amount: argument_bytes(call, 1),
                upper_bound: true,
                explanation: "fixed backing-store capacity",
            })
        }
        "with_capacity_in" => Some(exact(
            0,
            argument_bytes(call, 1),
            "container capacity; element size is unknown",
        )),
        "with_capacity" if callee_contains_name(&call.callee, "String") => {
            Some(exact(0, argument_bytes(call, 1), "owned UTF-8 capacity"))
        }
        "from" if callee_contains_name(&call.callee, "String") => {
            Some(exact(0, argument_bytes(call, 1), "owned UTF-8 copy"))
        }
        "clone_in" => Some(exact(0, None, "owned UTF-8 clone")),
        "concat" => Some(exact(
            0,
            sum_argument_bytes(call, &[1, 2]),
            "concatenated UTF-8 bytes",
        )),
        "concat3" => Some(exact(
            0,
            sum_argument_bytes(call, &[1, 2, 3]),
            "concatenated UTF-8 bytes",
        )),
        "repeat" => Some(exact(
            0,
            argument_bytes(call, 1)
                .zip(call.arguments.get(2).and_then(constant_value))
                .and_then(|(bytes, count)| bytes.checked_mul(count)),
            "repeated UTF-8 bytes",
        )),
        "join_strings" => Some(exact(0, None, "joined UTF-8 bytes")),
        "to_lowercase" | "to_uppercase" => Some(AllocationSpecification {
            source: AllocationSource::Argument(0),
            amount: Some(12),
            upper_bound: true,
            explanation: "full Unicode case mapping",
        }),
        "push_format" => Some(AllocationSpecification {
            source: AllocationSource::Destination,
            amount: None,
            upper_bound: false,
            explanation: "destination String growth; available spare capacity is unknown",
        }),
        _ => None,
    }
}

fn argument_bytes(call: &ast::CallExpression, index: usize) -> Option<u128> {
    call.arguments.get(index).and_then(constant_bytes)
}

fn sum_argument_bytes(call: &ast::CallExpression, indices: &[usize]) -> Option<u128> {
    indices.iter().try_fold(0_u128, |total, index| {
        total.checked_add(argument_bytes(call, *index)?)
    })
}

fn constant_value(expression: &Expression) -> Option<u128> {
    match expression {
        Expression::Integer(literal) => Some(literal.value),
        Expression::Unary(unary) if unary.operator == UnaryOperator::Borrow => {
            constant_value(&unary.operand)
        }
        Expression::Binary(binary) => {
            let left = constant_value(&binary.left)?;
            let right = constant_value(&binary.right)?;
            match binary.operator {
                BinaryOperator::Add => left.checked_add(right),
                BinaryOperator::Subtract => left.checked_sub(right),
                BinaryOperator::Multiply => left.checked_mul(right),
                BinaryOperator::Divide => left.checked_div(right),
                BinaryOperator::Remainder => left.checked_rem(right),
                BinaryOperator::BitAnd => Some(left & right),
                BinaryOperator::BitXor => Some(left ^ right),
                BinaryOperator::BitOr => Some(left | right),
                BinaryOperator::ShiftLeft => u32::try_from(right)
                    .ok()
                    .and_then(|shift| left.checked_shl(shift)),
                BinaryOperator::ShiftRight => u32::try_from(right)
                    .ok()
                    .and_then(|shift| left.checked_shr(shift)),
                BinaryOperator::Equal
                | BinaryOperator::NotEqual
                | BinaryOperator::Less
                | BinaryOperator::LessEqual
                | BinaryOperator::Greater
                | BinaryOperator::GreaterEqual
                | BinaryOperator::And
                | BinaryOperator::Or => None,
            }
        }
        Expression::Cast(cast) => constant_value(&cast.value),
        _ => None,
    }
}

fn constant_bytes(expression: &Expression) -> Option<u128> {
    match expression {
        Expression::String(literal) => u128::try_from(literal.value.len()).ok(),
        Expression::CString(literal) => u128::try_from(literal.value.len().saturating_add(1)).ok(),
        _ => constant_value(expression),
    }
}

fn unwrap_try(expression: &Expression) -> &Expression {
    match expression {
        Expression::Try { value, .. } => unwrap_try(value),
        _ => expression,
    }
}

fn call_name(callee: &Expression) -> Option<&str> {
    match callee {
        Expression::Path(path) => path.segments.last().map(|segment| segment.name.as_str()),
        Expression::Field(field) => match &field.field {
            FieldName::Named(name) => Some(name.name.as_str()),
            FieldName::TupleIndex { .. } => None,
        },
        _ => None,
    }
}

fn callee_contains_name(callee: &Expression, expected: &str) -> bool {
    match callee {
        Expression::Path(path) => path.segments.iter().any(|segment| segment.name == expected),
        Expression::Field(field) => {
            matches!(
                &field.field,
                FieldName::Named(name) if name.name == expected
            ) || callee_contains_name(&field.base, expected)
        }
        _ => false,
    }
}

fn root_name(expression: &Expression) -> Option<&str> {
    match expression {
        Expression::Path(path) if path.segments.len() == 1 => {
            path.segments.first().map(|segment| segment.name.as_str())
        }
        Expression::Field(field) => root_name(&field.base),
        Expression::Index(index) => root_name(&index.base),
        Expression::Unary(unary) => root_name(&unary.operand),
        Expression::Call(call) => root_name(&call.callee),
        Expression::Try { value, .. } => root_name(value),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use reimer_lexer::lex;
    use reimer_parser::parse;

    use super::{AllocationQuantity, estimate};

    #[test]
    fn estimate_should_evaluate_checked_constant_arithmetic() {
        let source = "fn main() { let bytes = allocate_bytes(&allocator, 32 * 4); }";
        let syntax =
            parse(&lex(source).expect("fixture should lex")).expect("fixture should parse");

        let (allocations, _) = estimate(&syntax);

        assert_eq!(allocations[0].quantity, AllocationQuantity::Exact(128));
    }

    #[test]
    fn estimate_should_report_aligned_logical_bytes_without_padding() {
        let source = "fn main() { let bytes = allocate_aligned_bytes(&allocator, 96, 64); }";
        let syntax =
            parse(&lex(source).expect("fixture should lex")).expect("fixture should parse");

        let (allocations, _) = estimate(&syntax);

        assert_eq!(allocations[0].quantity, AllocationQuantity::Exact(96));
        assert!(allocations[0].explanation.contains("padding is excluded"));
    }

    #[test]
    fn estimate_should_label_loop_allocations_per_iteration() {
        let source = "fn main() { loop { let bytes = allocate_bytes(&allocator, 16); break; } }";
        let syntax =
            parse(&lex(source).expect("fixture should lex")).expect("fixture should parse");

        let (allocations, _) = estimate(&syntax);

        assert_eq!(
            allocations[0].quantity,
            AllocationQuantity::PerIteration(16)
        );
    }

    #[test]
    fn estimate_should_count_utf8_bytes_copied_into_an_owned_string() {
        let source = "fn main() { let text = String::from(&allocator, \"\u{00e1}\"); }";
        let syntax =
            parse(&lex(source).expect("fixture should lex")).expect("fixture should parse");

        let (allocations, _) = estimate(&syntax);

        assert_eq!(allocations[0].quantity, AllocationQuantity::Exact(2));
    }

    #[test]
    fn estimate_should_combine_literal_concatenation_and_repetition_sizes() {
        let source = r#"
            fn main() {
                let joined = concat(&allocator, "hello", " world");
                let repeated = repeat(&allocator, "na", 3);
            }
        "#;
        let syntax =
            parse(&lex(source).expect("fixture should lex")).expect("fixture should parse");

        let (allocations, _) = estimate(&syntax);

        assert_eq!(allocations[0].quantity, AllocationQuantity::Exact(11));
        assert_eq!(allocations[1].quantity, AllocationQuantity::Exact(6));
    }

    #[test]
    fn estimate_should_bound_full_unicode_case_mapping() {
        let source = "fn main() { let text = to_uppercase(&allocator, '\u{00df}'); }";
        let syntax =
            parse(&lex(source).expect("fixture should lex")).expect("fixture should parse");

        let (allocations, _) = estimate(&syntax);

        assert_eq!(allocations[0].quantity, AllocationQuantity::AtMost(12));
    }

    #[test]
    fn estimate_should_attribute_interpolation_growth_to_the_destination_string() {
        let source = r#"fn main() { message.push_format(f"hello {name}")?; }"#;
        let syntax =
            parse(&lex(source).expect("fixture should lex")).expect("fixture should parse");

        let (allocations, _) = estimate(&syntax);

        assert_eq!(allocations[0].quantity, AllocationQuantity::Dynamic);
        assert_eq!(
            allocations[0].allocator,
            "allocator retained by String `message`"
        );
    }
}
