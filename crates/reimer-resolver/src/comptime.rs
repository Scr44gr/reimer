use std::collections::{BTreeMap, HashMap, HashSet};

use reimer_ast::{
    self as ast, AssignmentOperator, BinaryOperator, Expression, GenericArgument, GenericParameter,
    Pattern, Statement, TypeName, UnaryOperator,
};
use reimer_diagnostics::{Diagnostic, Span};

const DEFAULT_STEP_LIMIT: u64 = 1_000_000;
const DEFAULT_MEMORY_LIMIT: usize = 16 * 1024 * 1024;
const DEFAULT_CALL_DEPTH_LIMIT: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Value {
    Integer(Integer),
    Float(u64),
    Boolean(bool),
    Character(char),
    String(String),
    Unit,
    Tuple(Vec<Value>),
    Array(Vec<Value>),
    Record(BTreeMap<String, Value>),
}

impl Value {
    pub(crate) fn as_non_negative_u128(&self) -> Option<u128> {
        let Self::Integer(value) = self else {
            return None;
        };
        (!value.negative).then_some(value.magnitude)
    }

    pub(crate) fn as_integer(&self) -> Option<(bool, u128)> {
        let Self::Integer(value) = self else {
            return None;
        };
        Some((value.negative, value.magnitude))
    }

    fn memory_size(&self) -> usize {
        match self {
            Self::Integer(_) | Self::Float(_) => 16,
            Self::Boolean(_) => 1,
            Self::Character(_) => 4,
            Self::String(value) => value.len(),
            Self::Unit => 0,
            Self::Tuple(values) | Self::Array(values) => values.iter().map(Self::memory_size).sum(),
            Self::Record(fields) => fields
                .iter()
                .map(|(name, value)| name.len().saturating_add(value.memory_size()))
                .sum(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Integer {
    pub(crate) negative: bool,
    pub(crate) magnitude: u128,
}

impl Integer {
    fn positive(magnitude: u128) -> Self {
        Self {
            negative: false,
            magnitude,
        }
    }

    fn normalized(self) -> Self {
        if self.magnitude == 0 {
            Self::positive(0)
        } else {
            self
        }
    }

    fn negate(self) -> Self {
        Self {
            negative: !self.negative,
            magnitude: self.magnitude,
        }
        .normalized()
    }

    fn add(self, other: Self) -> Option<Self> {
        if self.negative == other.negative {
            Some(
                Self {
                    negative: self.negative,
                    magnitude: self.magnitude.checked_add(other.magnitude)?,
                }
                .normalized(),
            )
        } else if self.magnitude >= other.magnitude {
            Some(
                Self {
                    negative: self.negative,
                    magnitude: self.magnitude - other.magnitude,
                }
                .normalized(),
            )
        } else {
            Some(
                Self {
                    negative: other.negative,
                    magnitude: other.magnitude - self.magnitude,
                }
                .normalized(),
            )
        }
    }

    fn multiply(self, other: Self) -> Option<Self> {
        Some(
            Self {
                negative: self.negative != other.negative,
                magnitude: self.magnitude.checked_mul(other.magnitude)?,
            }
            .normalized(),
        )
    }

    fn divide(self, other: Self) -> Option<Self> {
        Some(
            Self {
                negative: self.negative != other.negative,
                magnitude: self.magnitude.checked_div(other.magnitude)?,
            }
            .normalized(),
        )
    }

    fn remainder(self, other: Self) -> Option<Self> {
        Some(
            Self {
                negative: self.negative,
                magnitude: self.magnitude.checked_rem(other.magnitude)?,
            }
            .normalized(),
        )
    }
}

impl From<u128> for Integer {
    fn from(magnitude: u128) -> Self {
        Self::positive(magnitude)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct EvaluatedConstant {
    pub(crate) value: Value,
    pub(crate) span: Span,
}

pub(crate) struct Evaluation {
    pub(crate) constants: HashMap<String, EvaluatedConstant>,
    pub(crate) diagnostics: Vec<Diagnostic>,
}

pub(crate) enum IntrinsicResult {
    NotFound,
    Deferred,
    Value(Value),
    Error { message: String, help: &'static str },
}

pub(crate) trait Metadata {
    fn evaluate(
        &mut self,
        path: &ast::Path,
        arguments: &[GenericArgument],
        type_bindings: &HashMap<String, TypeName>,
    ) -> IntrinsicResult;
}

pub(crate) struct UnavailableMetadata;

impl Metadata for UnavailableMetadata {
    fn evaluate(
        &mut self,
        path: &ast::Path,
        _arguments: &[GenericArgument],
        _type_bindings: &HashMap<String, TypeName>,
    ) -> IntrinsicResult {
        if is_metadata_intrinsic(path) {
            IntrinsicResult::Deferred
        } else {
            IntrinsicResult::NotFound
        }
    }
}

pub(crate) fn evaluate(
    program: &ast::Program,
    metadata: &mut impl Metadata,
    seed: HashMap<String, EvaluatedConstant>,
    emit_errors: bool,
    run_blocks: bool,
) -> Evaluation {
    Evaluator::new(program, metadata, seed, emit_errors).evaluate(run_blocks)
}

struct Evaluator<'ast, 'metadata, M> {
    program: &'ast ast::Program,
    metadata: &'metadata mut M,
    functions: HashMap<&'ast str, &'ast ast::Function>,
    declarations: HashMap<&'ast str, &'ast ast::ConstantDeclaration>,
    constants: HashMap<String, EvaluatedConstant>,
    evaluating: HashSet<String>,
    diagnostics: Vec<Diagnostic>,
    emit_errors: bool,
    steps: u64,
    memory: usize,
    call_depth: usize,
}

impl<'ast, 'metadata, M: Metadata> Evaluator<'ast, 'metadata, M> {
    fn new(
        program: &'ast ast::Program,
        metadata: &'metadata mut M,
        constants: HashMap<String, EvaluatedConstant>,
        emit_errors: bool,
    ) -> Self {
        let functions = program
            .items
            .iter()
            .filter_map(|item| {
                let ast::Item::Function(function) = item else {
                    return None;
                };
                function
                    .is_comptime
                    .then_some((function.name.name.as_str(), function))
            })
            .collect();
        let declarations = program
            .items
            .iter()
            .filter_map(|item| {
                let ast::Item::Constant(declaration) = item else {
                    return None;
                };
                Some((declaration.name.name.as_str(), declaration))
            })
            .collect();
        Self {
            program,
            metadata,
            functions,
            declarations,
            constants,
            evaluating: HashSet::new(),
            diagnostics: Vec::new(),
            emit_errors,
            steps: 0,
            memory: 0,
            call_depth: 0,
        }
    }

    fn evaluate(mut self, run_blocks: bool) -> Evaluation {
        for item in &self.program.items {
            if let ast::Item::Constant(declaration) = item {
                let _ = self.evaluate_constant(&declaration.name.name);
            }
        }
        if run_blocks {
            for item in &self.program.items {
                let ast::Item::Comptime(block) = item else {
                    continue;
                };
                let mut frame = Frame::default();
                match self.evaluate_block(&block.body, &mut frame) {
                    Ok(Flow::Value(_)) | Err(EvalFailure::Reported | EvalFailure::Deferred) => {}
                    Ok(Flow::Return(_) | Flow::Break(_) | Flow::Continue)
                    | Err(EvalFailure::Return(_) | EvalFailure::Break(_) | EvalFailure::Continue) => self.report(
                        "E7011",
                        "control flow cannot escape a top-level `comptime` block",
                        block.span,
                        "keep `return`, `break`, and `continue` inside their matching construct",
                    ),
                }
            }
        }
        Evaluation {
            constants: self.constants,
            diagnostics: self.diagnostics,
        }
    }

    fn evaluate_constant(&mut self, name: &str) -> EvalResult<Value> {
        if let Some(value) = self.constants.get(name) {
            return Ok(value.value.clone());
        }
        let Some(declaration) = self.declarations.get(name).copied() else {
            return Err(EvalFailure::Deferred);
        };
        if !self.evaluating.insert(name.to_owned()) {
            return self.fail(
                "E7014",
                format!("compile-time constant `{name}` depends on itself"),
                declaration.span,
                "remove the constant cycle",
            );
        }
        let mut frame = Frame::default();
        let result = self.evaluate_expression(&declaration.value, &mut frame);
        self.evaluating.remove(name);
        let value = result?;
        self.charge_memory(&value, declaration.span)?;
        self.constants.insert(
            name.to_owned(),
            EvaluatedConstant {
                value: value.clone(),
                span: declaration.span,
            },
        );
        Ok(value)
    }

    fn evaluate_expression(
        &mut self,
        expression: &Expression,
        frame: &mut Frame,
    ) -> EvalResult<Value> {
        self.step(expression.span())?;
        match expression {
            Expression::Integer(literal) => Ok(Value::Integer(Integer::positive(literal.value))),
            Expression::Float(literal) => Ok(Value::Float(literal.bits)),
            Expression::Character(literal) => Ok(Value::Character(literal.value)),
            Expression::String(literal) | Expression::CString(literal) => {
                Ok(Value::String(literal.value.clone()))
            }
            Expression::Boolean(literal) => Ok(Value::Boolean(literal.value)),
            Expression::Unit(_) => Ok(Value::Unit),
            Expression::Tuple(tuple) => self
                .evaluate_values(&tuple.elements, frame)
                .map(Value::Tuple),
            Expression::Array(array) => self
                .evaluate_values(&array.elements, frame)
                .map(Value::Array),
            Expression::Struct(structure) => {
                let mut fields = BTreeMap::new();
                for field in &structure.fields {
                    fields.insert(
                        field.name.name.clone(),
                        self.evaluate_expression(&field.value, frame)?,
                    );
                }
                Ok(Value::Record(fields))
            }
            Expression::Path(path) => self.evaluate_path(path, frame),
            Expression::Unary(unary) => {
                let value = self.evaluate_expression(&unary.operand, frame)?;
                self.evaluate_unary(unary.operator, value, unary.span)
            }
            Expression::Binary(binary) => self.evaluate_binary_expression(binary, frame),
            Expression::Call(call) => self.evaluate_call(call, frame),
            Expression::If(conditional) => self.evaluate_conditional(conditional, frame),
            Expression::Match(matching) => self.evaluate_match(matching, frame),
            Expression::Loop(looping) => self.evaluate_loop(looping, frame),
            Expression::Block(block) => self.block_value(block, frame),
            Expression::Unsafe(block) => self.fail(
                "E7012",
                "`unsafe` is forbidden during compile-time evaluation",
                block.span,
                "move native or raw-pointer work to runtime code",
            ),
            Expression::Assignment(assignment) => self.evaluate_assignment(assignment, frame),
            Expression::Cast(cast) => {
                let value = self.evaluate_expression(&cast.value, frame)?;
                self.evaluate_cast(value, &cast.target, cast.span)
            }
            Expression::Field(field) => self.evaluate_field(field, frame),
            Expression::Index(index) => self.evaluate_index(index, frame),
            Expression::Try { span, .. } => self.fail(
                "E7011",
                "`?` is not available for compile-time-only values",
                *span,
                "handle the value explicitly before entering `comptime`",
            ),
        }
    }

    fn evaluate_conditional(
        &mut self,
        conditional: &ast::IfExpression,
        frame: &mut Frame,
    ) -> EvalResult<Value> {
        let condition = self.evaluate_expression(&conditional.condition, frame)?;
        let Value::Boolean(condition) = condition else {
            return self.type_error("if condition", conditional.condition.span());
        };
        if condition {
            self.block_value(&conditional.then_branch, frame)
        } else if let Some(alternative) = &conditional.else_branch {
            self.evaluate_expression(alternative, frame)
        } else {
            Ok(Value::Unit)
        }
    }

    fn evaluate_match(
        &mut self,
        matching: &ast::MatchExpression,
        frame: &mut Frame,
    ) -> EvalResult<Value> {
        let scrutinee = self.evaluate_expression(&matching.scrutinee, frame)?;
        for arm in &matching.arms {
            frame.push_scope();
            let matched = self.match_pattern(&arm.pattern, &scrutinee, frame)?;
            let guard = if matched {
                arm.guard
                    .as_ref()
                    .map(|guard| self.evaluate_expression(guard, frame))
                    .transpose()?
                    .is_none_or(|value| value == Value::Boolean(true))
            } else {
                false
            };
            if matched && guard {
                let value = self.evaluate_expression(&arm.body, frame);
                frame.pop_scope();
                return value;
            }
            frame.pop_scope();
        }
        self.fail(
            "E7011",
            "non-exhaustive match reached during compile-time evaluation",
            matching.span,
            "add an arm that covers this value",
        )
    }

    fn evaluate_loop(
        &mut self,
        looping: &ast::LoopExpression,
        frame: &mut Frame,
    ) -> EvalResult<Value> {
        loop {
            match self.evaluate_block(&looping.body, frame) {
                Ok(Flow::Value(_) | Flow::Continue) | Err(EvalFailure::Continue) => {}
                Ok(Flow::Break(value)) | Err(EvalFailure::Break(value)) => {
                    return Ok(value.unwrap_or(Value::Unit));
                }
                Ok(Flow::Return(value)) | Err(EvalFailure::Return(value)) => {
                    return Err(EvalFailure::Return(value));
                }
                Err(failure) => return Err(failure),
            }
        }
    }

    fn evaluate_assignment(
        &mut self,
        assignment: &ast::AssignmentExpression,
        frame: &mut Frame,
    ) -> EvalResult<Value> {
        let ast::Expression::Path(path) = &assignment.target else {
            return self.fail(
                "E7011",
                "compile-time assignment requires a local binding",
                assignment.target.span(),
                "assign to a local variable",
            );
        };
        let Some(name) = single_path_name(path) else {
            return self.type_error("assignment target", path.span);
        };
        let right = self.evaluate_expression(&assignment.value, frame)?;
        let value = match assignment.operator {
            AssignmentOperator::Assign => right,
            operator => {
                let Some(current) = frame.lookup(name).cloned() else {
                    return self.unknown_name(name, path.span);
                };
                self.evaluate_binary(
                    assignment_binary_operator(operator),
                    current,
                    right,
                    assignment.span,
                )?
            }
        };
        if !frame.assign(name, value.clone()) {
            return self.fail(
                "E7011",
                format!("cannot assign to immutable compile-time binding `{name}`"),
                path.span,
                "declare it with `let mut`",
            );
        }
        Ok(value)
    }

    fn evaluate_field(
        &mut self,
        field: &ast::FieldExpression,
        frame: &mut Frame,
    ) -> EvalResult<Value> {
        let base = self.evaluate_expression(&field.base, frame)?;
        match (&base, &field.field) {
            (Value::Record(fields), ast::FieldName::Named(name)) => fields
                .get(&name.name)
                .cloned()
                .ok_or(EvalFailure::Reported)
                .or_else(|_| self.unknown_name(&name.name, name.span)),
            (Value::Tuple(values), ast::FieldName::TupleIndex { index, span }) => values
                .get(usize::try_from(*index).map_err(|_| EvalFailure::Reported)?)
                .cloned()
                .ok_or_else(|| {
                    self.report(
                        "E7011",
                        "tuple field is outside the compile-time value",
                        *span,
                        "use a valid tuple field index",
                    );
                    EvalFailure::Reported
                }),
            _ => self.type_error("field access", field.span),
        }
    }

    fn evaluate_index(
        &mut self,
        index: &ast::IndexExpression,
        frame: &mut Frame,
    ) -> EvalResult<Value> {
        let base = self.evaluate_expression(&index.base, frame)?;
        let Some(source_index) = index.indices.first() else {
            return self.type_error("index", index.span);
        };
        let index_value = self.evaluate_expression(source_index, frame)?;
        let Some(index_value) = index_value.as_non_negative_u128() else {
            return self.type_error("index", source_index.span());
        };
        let Ok(index_value) = usize::try_from(index_value) else {
            return self.out_of_bounds(source_index.span());
        };
        match base {
            Value::Array(values) | Value::Tuple(values) => values
                .get(index_value)
                .cloned()
                .ok_or(EvalFailure::Reported)
                .or_else(|_| self.out_of_bounds(source_index.span())),
            _ => self.type_error("index", index.span),
        }
    }

    fn evaluate_values(
        &mut self,
        expressions: &[Expression],
        frame: &mut Frame,
    ) -> EvalResult<Vec<Value>> {
        expressions
            .iter()
            .map(|expression| self.evaluate_expression(expression, frame))
            .collect()
    }

    fn evaluate_path(&mut self, path: &ast::Path, frame: &Frame) -> EvalResult<Value> {
        let Some(name) = single_path_name(path) else {
            return self.unknown_name(&path.display(), path.span);
        };
        if let Some(value) = frame.lookup(name) {
            return Ok(value.clone());
        }
        self.evaluate_constant(name)
            .or_else(|failure| match failure {
                EvalFailure::Deferred if !self.declarations.contains_key(name) => {
                    self.unknown_name(name, path.span)
                }
                other => Err(other),
            })
    }

    fn evaluate_unary(
        &mut self,
        operator: UnaryOperator,
        value: Value,
        span: Span,
    ) -> EvalResult<Value> {
        match (operator, value) {
            (UnaryOperator::Negate, Value::Integer(value)) => Ok(Value::Integer(value.negate())),
            (UnaryOperator::Negate, Value::Float(bits)) => {
                Ok(Value::Float((-f64::from_bits(bits)).to_bits()))
            }
            (UnaryOperator::Not, Value::Boolean(value)) => Ok(Value::Boolean(!value)),
            (UnaryOperator::Not, Value::Integer(value)) if !value.negative => {
                Ok(Value::Integer(Integer::positive(!value.magnitude)))
            }
            (UnaryOperator::Borrow | UnaryOperator::BorrowMut | UnaryOperator::Dereference, _) => {
                self.fail(
                    "E7012",
                    "references and pointers are forbidden in compile-time values",
                    span,
                    "use owned scalar or aggregate compile-time values",
                )
            }
            _ => self.type_error("unary operation", span),
        }
    }

    fn evaluate_binary_expression(
        &mut self,
        binary: &ast::BinaryExpression,
        frame: &mut Frame,
    ) -> EvalResult<Value> {
        let left = self.evaluate_expression(&binary.left, frame)?;
        if binary.operator == BinaryOperator::And && left == Value::Boolean(false) {
            return Ok(Value::Boolean(false));
        }
        if binary.operator == BinaryOperator::Or && left == Value::Boolean(true) {
            return Ok(Value::Boolean(true));
        }
        let right = self.evaluate_expression(&binary.right, frame)?;
        self.evaluate_binary(binary.operator, left, right, binary.span)
    }

    fn evaluate_binary(
        &mut self,
        operator: BinaryOperator,
        left: Value,
        right: Value,
        span: Span,
    ) -> EvalResult<Value> {
        match (left, right) {
            (Value::Integer(left), Value::Integer(right)) => {
                self.evaluate_integer_binary(operator, left, right, span)
            }
            (Value::Float(left), Value::Float(right)) => {
                let left = f64::from_bits(left);
                let right = f64::from_bits(right);
                let value = match operator {
                    BinaryOperator::Add => Value::Float((left + right).to_bits()),
                    BinaryOperator::Subtract => Value::Float((left - right).to_bits()),
                    BinaryOperator::Multiply => Value::Float((left * right).to_bits()),
                    BinaryOperator::Divide => Value::Float((left / right).to_bits()),
                    BinaryOperator::Equal => Value::Boolean(float_equals(left, right)),
                    BinaryOperator::NotEqual => Value::Boolean(!float_equals(left, right)),
                    BinaryOperator::Less => Value::Boolean(left < right),
                    BinaryOperator::LessEqual => Value::Boolean(left <= right),
                    BinaryOperator::Greater => Value::Boolean(left > right),
                    BinaryOperator::GreaterEqual => Value::Boolean(left >= right),
                    _ => return self.type_error("floating-point operation", span),
                };
                Ok(value)
            }
            (Value::Boolean(left), Value::Boolean(right)) => match operator {
                BinaryOperator::And => Ok(Value::Boolean(left && right)),
                BinaryOperator::Or => Ok(Value::Boolean(left || right)),
                BinaryOperator::Equal => Ok(Value::Boolean(left == right)),
                BinaryOperator::NotEqual => Ok(Value::Boolean(left != right)),
                _ => self.type_error("boolean operation", span),
            },
            (left, right) if matches!(operator, BinaryOperator::Equal) => {
                Ok(Value::Boolean(left == right))
            }
            (left, right) if matches!(operator, BinaryOperator::NotEqual) => {
                Ok(Value::Boolean(left != right))
            }
            _ => self.type_error("binary operation", span),
        }
    }

    fn evaluate_integer_binary(
        &mut self,
        operator: BinaryOperator,
        left: Integer,
        right: Integer,
        span: Span,
    ) -> EvalResult<Value> {
        let result = match operator {
            BinaryOperator::Add => left.add(right).map(Value::Integer),
            BinaryOperator::Subtract => left.add(right.negate()).map(Value::Integer),
            BinaryOperator::Multiply => left.multiply(right).map(Value::Integer),
            BinaryOperator::Divide => left.divide(right).map(Value::Integer),
            BinaryOperator::Remainder => left.remainder(right).map(Value::Integer),
            BinaryOperator::BitAnd if !left.negative && !right.negative => Some(Value::Integer(
                Integer::positive(left.magnitude & right.magnitude),
            )),
            BinaryOperator::BitXor if !left.negative && !right.negative => Some(Value::Integer(
                Integer::positive(left.magnitude ^ right.magnitude),
            )),
            BinaryOperator::BitOr if !left.negative && !right.negative => Some(Value::Integer(
                Integer::positive(left.magnitude | right.magnitude),
            )),
            BinaryOperator::ShiftLeft if !left.negative && !right.negative => {
                u32::try_from(right.magnitude)
                    .ok()
                    .and_then(|shift| left.magnitude.checked_shl(shift))
                    .map(Integer::positive)
                    .map(Value::Integer)
            }
            BinaryOperator::ShiftRight if !left.negative && !right.negative => {
                u32::try_from(right.magnitude)
                    .ok()
                    .and_then(|shift| left.magnitude.checked_shr(shift))
                    .map(Integer::positive)
                    .map(Value::Integer)
            }
            BinaryOperator::Equal => return Ok(Value::Boolean(left == right)),
            BinaryOperator::NotEqual => return Ok(Value::Boolean(left != right)),
            BinaryOperator::Less => {
                return Ok(Value::Boolean(compare_integer(left, right).is_lt()));
            }
            BinaryOperator::LessEqual => {
                return Ok(Value::Boolean(!compare_integer(left, right).is_gt()));
            }
            BinaryOperator::Greater => {
                return Ok(Value::Boolean(compare_integer(left, right).is_gt()));
            }
            BinaryOperator::GreaterEqual => {
                return Ok(Value::Boolean(!compare_integer(left, right).is_lt()));
            }
            BinaryOperator::And | BinaryOperator::Or => {
                return self.type_error("logical operation", span);
            }
            _ => None,
        };
        result.ok_or(EvalFailure::Reported).or_else(|_| {
            self.fail(
                "E7011",
                "compile-time integer operation overflowed or used an invalid operand",
                span,
                "reduce the value, avoid division by zero, and use a valid shift",
            )
        })
    }

    fn evaluate_call(
        &mut self,
        call: &ast::CallExpression,
        frame: &mut Frame,
    ) -> EvalResult<Value> {
        let Expression::Path(path) = &call.callee else {
            return self.fail(
                "E7012",
                "indirect calls are forbidden during compile-time evaluation",
                call.span,
                "call a declared `comptime fn` directly",
            );
        };
        if let Some(result) = self.evaluate_builtin_call(path, call, frame) {
            return result;
        }
        let Some(name) = single_path_name(path) else {
            return self.forbidden_call(&path.display(), call.span);
        };
        let Some(function) = self.functions.get(name).copied() else {
            return self.forbidden_call(name, call.span);
        };
        if self.call_depth >= DEFAULT_CALL_DEPTH_LIMIT {
            return self.fail(
                "E7014",
                "compile-time call depth limit exceeded",
                call.span,
                "rewrite the recursion iteratively or reduce its depth",
            );
        }
        if function.parameters.len() != call.arguments.len() {
            return self.fail(
                "E7011",
                format!(
                    "compile-time function `{name}` expects {} argument(s), but {} were provided",
                    function.parameters.len(),
                    call.arguments.len()
                ),
                call.span,
                "pass exactly the declared arguments",
            );
        }
        let arguments = self.evaluate_values(&call.arguments, frame)?;
        let mut function_frame = Frame::default();
        self.bind_generic_arguments(
            function,
            &call.generic_arguments,
            frame,
            &mut function_frame,
        )?;
        for (parameter, value) in function.parameters.iter().zip(arguments) {
            if !value_matches_type(&value, &parameter.ty, &function_frame.type_bindings) {
                return self.fail(
                    "E7011",
                    format!(
                        "argument for compile-time parameter `{}` does not match its declared type",
                        parameter.name.name
                    ),
                    parameter.span,
                    "pass a compile-time value compatible with the parameter type",
                );
            }
            function_frame.insert(
                parameter.name.name.clone(),
                Variable {
                    mutable: false,
                    value,
                },
            );
        }
        self.call_depth += 1;
        let result = self.evaluate_block(&function.body, &mut function_frame);
        self.call_depth -= 1;
        let value = match result {
            Ok(Flow::Value(value) | Flow::Return(value)) | Err(EvalFailure::Return(value)) => value,
            Ok(Flow::Break(_) | Flow::Continue)
            | Err(EvalFailure::Break(_) | EvalFailure::Continue) => self.fail(
                "E7011",
                "loop control escaped a compile-time function",
                function.span,
                "keep `break` and `continue` inside their loop",
            )?,
            Err(failure) => return Err(failure),
        };
        let matches_return = function.return_type.as_ref().map_or_else(
            || value == Value::Unit,
            |return_type| value_matches_type(&value, return_type, &function_frame.type_bindings),
        );
        if !matches_return {
            return self.fail(
                "E7011",
                format!(
                    "compile-time function `{name}` produced a value incompatible with its return type"
                ),
                function.body.span,
                "return a value matching the declared compile-time return type",
            );
        }
        Ok(value)
    }

    fn evaluate_builtin_call(
        &mut self,
        path: &ast::Path,
        call: &ast::CallExpression,
        frame: &mut Frame,
    ) -> Option<EvalResult<Value>> {
        if single_path_name(path) == Some("assert") {
            return Some(self.evaluate_assert(call, frame));
        }
        if single_path_name(path) == Some("panic") {
            let message = call
                .arguments
                .first()
                .map(|argument| self.evaluate_expression(argument, frame))
                .transpose();
            return Some(message.and_then(|message| {
                let message = message
                    .and_then(|value| match value {
                        Value::String(message) => Some(message),
                        _ => None,
                    })
                    .unwrap_or_else(|| "compile-time panic".to_owned());
                self.fail(
                    "E7013",
                    message,
                    call.span,
                    "remove the failing compile-time path",
                )
            }));
        }
        match self
            .metadata
            .evaluate(path, &call.generic_arguments, &frame.type_bindings)
        {
            IntrinsicResult::Value(value) => Some(Ok(value)),
            IntrinsicResult::Deferred => Some(Err(EvalFailure::Deferred)),
            IntrinsicResult::Error { message, help } => {
                Some(self.fail("E7015", message, call.span, help))
            }
            IntrinsicResult::NotFound => None,
        }
    }

    fn evaluate_assert(
        &mut self,
        call: &ast::CallExpression,
        frame: &mut Frame,
    ) -> EvalResult<Value> {
        let Some(condition) = call.arguments.first() else {
            return self.fail(
                "E7011",
                "`assert` expects one condition",
                call.span,
                "write `assert(condition)`",
            );
        };
        let value = self.evaluate_expression(condition, frame)?;
        if value == Value::Boolean(true) {
            Ok(Value::Unit)
        } else if value == Value::Boolean(false) {
            self.fail(
                "E7013",
                "compile-time assertion failed",
                condition.span(),
                "make the asserted invariant true",
            )
        } else {
            self.type_error("assert condition", condition.span())
        }
    }

    fn evaluate_cast(&mut self, value: Value, target: &TypeName, span: Span) -> EvalResult<Value> {
        let TypeName {
            kind: ast::TypeNameKind::Path(path),
            ..
        } = target
        else {
            return self.type_error("compile-time cast target", span);
        };
        let Some(target) = single_path_name(path) else {
            return self.type_error("compile-time cast target", span);
        };
        if integer_width(target).is_some() {
            let Value::Integer(integer) = value else {
                return self.type_error("integer cast", span);
            };
            if integer_fits(integer, target) {
                return Ok(Value::Integer(integer));
            }
            return self.fail(
                "E7011",
                format!("compile-time integer does not fit in `{target}`"),
                span,
                "use a wider integer type or reduce the value",
            );
        }
        match (target, value) {
            ("f32", Value::Float(bits)) => {
                let value = narrow_f64(f64::from_bits(bits));
                Ok(Value::Float(f64::from(value).to_bits()))
            }
            ("f64", Value::Float(bits)) => Ok(Value::Float(bits)),
            ("f32", Value::Integer(integer)) => {
                let value = narrow_f64(integer_as_f64(integer));
                Ok(Value::Float(f64::from(value).to_bits()))
            }
            ("f64", Value::Integer(integer)) => Ok(Value::Float(integer_as_f64(integer).to_bits())),
            ("char", Value::Character(value)) => Ok(Value::Character(value)),
            ("char", Value::Integer(integer)) if !integer.negative => {
                let value = u32::try_from(integer.magnitude)
                    .ok()
                    .and_then(char::from_u32)
                    .ok_or(EvalFailure::Reported)
                    .or_else(|_| {
                        self.fail(
                            "E7011",
                            "compile-time integer is not a Unicode scalar value",
                            span,
                            "use a value from 0 through 0x10FFFF excluding surrogates",
                        )
                    })?;
                Ok(Value::Character(value))
            }
            ("bool", Value::Boolean(value)) => Ok(Value::Boolean(value)),
            ("str" | "cstr", Value::String(value)) => Ok(Value::String(value)),
            ("()", Value::Unit) => Ok(Value::Unit),
            _ => self.type_error("compile-time cast", span),
        }
    }

    fn bind_generic_arguments(
        &mut self,
        function: &ast::Function,
        arguments: &[GenericArgument],
        source_frame: &Frame,
        target_frame: &mut Frame,
    ) -> EvalResult<()> {
        if arguments.len() > function.generic_parameters.len() {
            return self.fail(
                "E7011",
                "too many compile-time generic arguments",
                function.span,
                "remove the extra generic arguments",
            );
        }
        for (parameter, argument) in function.generic_parameters.iter().zip(arguments) {
            match (parameter, argument) {
                (GenericParameter::Type { name, .. }, GenericArgument::Type(type_name)) => {
                    target_frame.type_bindings.insert(
                        name.name.clone(),
                        substitute_type(type_name, &source_frame.type_bindings),
                    );
                }
                (GenericParameter::Const { name, .. }, GenericArgument::Const(expression)) => {
                    let value = self.evaluate_expression(expression, &mut source_frame.clone())?;
                    target_frame.insert(
                        name.name.clone(),
                        Variable {
                            mutable: false,
                            value,
                        },
                    );
                }
                _ => {
                    return self.fail(
                        "E7011",
                        "compile-time generic argument has the wrong kind",
                        argument.span(),
                        "match type arguments to type parameters and values to const parameters",
                    );
                }
            }
        }
        for parameter in function.generic_parameters.iter().skip(arguments.len()) {
            match parameter {
                GenericParameter::Type {
                    name,
                    default: Some(default),
                    ..
                } => {
                    target_frame.type_bindings.insert(
                        name.name.clone(),
                        substitute_type(default, &source_frame.type_bindings),
                    );
                }
                GenericParameter::Const {
                    name,
                    default: Some(default),
                    ..
                } => {
                    let value = self.evaluate_expression(default, &mut source_frame.clone())?;
                    target_frame.insert(
                        name.name.clone(),
                        Variable {
                            mutable: false,
                            value,
                        },
                    );
                }
                parameter => {
                    return self.fail(
                        "E7011",
                        format!(
                            "cannot infer compile-time generic parameter `{}`",
                            parameter.name().name
                        ),
                        function.span,
                        "provide the generic argument explicitly",
                    );
                }
            }
        }
        Ok(())
    }

    fn evaluate_block(&mut self, block: &ast::Block, frame: &mut Frame) -> EvalResult<Flow> {
        frame.push_scope();
        for statement in &block.statements {
            self.step(statement_span(statement))?;
            let flow = self.evaluate_statement(statement, frame)?;
            if !matches!(flow, Flow::Value(_)) {
                frame.pop_scope();
                return Ok(flow);
            }
        }
        let value = block
            .tail
            .as_deref()
            .map(|tail| self.evaluate_expression(tail, frame))
            .transpose()?
            .unwrap_or(Value::Unit);
        frame.pop_scope();
        Ok(Flow::Value(value))
    }

    fn block_value(&mut self, block: &ast::Block, frame: &mut Frame) -> EvalResult<Value> {
        match self.evaluate_block(block, frame)? {
            Flow::Value(value) => Ok(value),
            Flow::Return(value) => Err(EvalFailure::Return(value)),
            Flow::Break(value) => Err(EvalFailure::Break(value)),
            Flow::Continue => Err(EvalFailure::Continue),
        }
    }

    fn evaluate_statement(&mut self, statement: &Statement, frame: &mut Frame) -> EvalResult<Flow> {
        match statement {
            Statement::Let(binding) => {
                let value = self.evaluate_expression(&binding.initializer, frame)?;
                frame.insert(
                    binding.name.name.clone(),
                    Variable {
                        mutable: binding.mutable,
                        value,
                    },
                );
                Ok(Flow::Value(Value::Unit))
            }
            Statement::Expression(statement) => self
                .evaluate_expression(&statement.expression, frame)
                .map(Flow::Value),
            Statement::Return(statement) => {
                let value = statement
                    .value
                    .as_ref()
                    .map(|value| self.evaluate_expression(value, frame))
                    .transpose()?
                    .unwrap_or(Value::Unit);
                Ok(Flow::Return(value))
            }
            Statement::While(statement) => {
                loop {
                    let condition = self.evaluate_expression(&statement.condition, frame)?;
                    let Value::Boolean(condition) = condition else {
                        return self.type_error("while condition", statement.condition.span());
                    };
                    if !condition {
                        break;
                    }
                    match self.evaluate_block(&statement.body, frame) {
                        Ok(Flow::Value(_) | Flow::Continue) | Err(EvalFailure::Continue) => {}
                        Ok(Flow::Break(_)) | Err(EvalFailure::Break(_)) => break,
                        Ok(Flow::Return(value)) | Err(EvalFailure::Return(value)) => {
                            return Ok(Flow::Return(value));
                        }
                        Err(failure) => return Err(failure),
                    }
                }
                Ok(Flow::Value(Value::Unit))
            }
            Statement::For(statement) => {
                let iterable = self.evaluate_expression(&statement.iterable, frame)?;
                let (Value::Array(values) | Value::Tuple(values)) = iterable else {
                    return self.type_error("for iterable", statement.iterable.span());
                };
                for value in values {
                    frame.push_scope();
                    if !self.match_pattern(&statement.pattern, &value, frame)? {
                        frame.pop_scope();
                        return self.fail(
                            "E7011",
                            "refutable pattern failed in a compile-time `for` loop",
                            statement.pattern.span(),
                            "use an irrefutable loop pattern",
                        );
                    }
                    let flow = self.evaluate_block(&statement.body, frame);
                    frame.pop_scope();
                    match flow {
                        Ok(Flow::Value(_) | Flow::Continue) | Err(EvalFailure::Continue) => {}
                        Ok(Flow::Break(_)) | Err(EvalFailure::Break(_)) => break,
                        Ok(Flow::Return(value)) | Err(EvalFailure::Return(value)) => {
                            return Ok(Flow::Return(value));
                        }
                        Err(failure) => return Err(failure),
                    }
                }
                Ok(Flow::Value(Value::Unit))
            }
            Statement::Break(statement) => {
                let value = statement
                    .value
                    .as_ref()
                    .map(|value| self.evaluate_expression(value, frame))
                    .transpose()?;
                Ok(Flow::Break(value))
            }
            Statement::Continue(_) => Ok(Flow::Continue),
            Statement::Defer(statement) => self.fail(
                "E7012",
                "`defer` is not available during compile-time evaluation",
                statement.span,
                "keep compile-time cleanup explicit and pure",
            ),
        }
    }

    fn match_pattern(
        &mut self,
        pattern: &Pattern,
        value: &Value,
        frame: &mut Frame,
    ) -> EvalResult<bool> {
        let matched = match pattern {
            Pattern::Wildcard(_) => true,
            Pattern::Identifier { mutable, name, .. } => {
                frame.insert(
                    name.name.clone(),
                    Variable {
                        mutable: *mutable,
                        value: value.clone(),
                    },
                );
                true
            }
            Pattern::Integer {
                value: expected,
                negative,
                ..
            } => {
                value
                    == &Value::Integer(Integer {
                        negative: *negative,
                        magnitude: *expected,
                    })
            }
            Pattern::Float { bits, negative, .. } => {
                let expected = if *negative {
                    (-f64::from_bits(*bits)).to_bits()
                } else {
                    *bits
                };
                value == &Value::Float(expected)
            }
            Pattern::Character(expected) => value == &Value::Character(expected.value),
            Pattern::Boolean(expected) => value == &Value::Boolean(expected.value),
            Pattern::Tuple { elements, .. } => {
                let Value::Tuple(values) = value else {
                    return Ok(false);
                };
                if elements.len() == values.len() {
                    for (element, value) in elements.iter().zip(values) {
                        if !self.match_pattern(element, value, frame)? {
                            return Ok(false);
                        }
                    }
                    true
                } else {
                    false
                }
            }
            Pattern::Path(_) | Pattern::EnumTuple { .. } | Pattern::EnumStruct { .. } => {
                return self.fail(
                    "E7011",
                    "enum patterns are not compile-time values yet",
                    pattern.span(),
                    "match scalar, tuple, or reflected descriptor values",
                );
            }
        };
        Ok(matched)
    }

    fn step(&mut self, span: Span) -> EvalResult<()> {
        self.steps = self.steps.saturating_add(1);
        if self.steps > DEFAULT_STEP_LIMIT {
            self.fail(
                "E7014",
                "compile-time step limit exceeded",
                span,
                "reduce the loop or recursion work performed by the compiler",
            )
        } else {
            Ok(())
        }
    }

    fn charge_memory(&mut self, value: &Value, span: Span) -> EvalResult<()> {
        self.memory = self.memory.saturating_add(value.memory_size());
        if self.memory > DEFAULT_MEMORY_LIMIT {
            self.fail(
                "E7014",
                "compile-time memory limit exceeded",
                span,
                "reduce constant data or construct it at runtime",
            )
        } else {
            Ok(())
        }
    }

    fn forbidden_call<T>(&mut self, name: &str, span: Span) -> EvalResult<T> {
        self.fail(
            "E7012",
            format!("runtime function `{name}` cannot be called during compile-time evaluation"),
            span,
            "call only pure `comptime fn` functions and metadata intrinsics",
        )
    }

    fn unknown_name<T>(&mut self, name: &str, span: Span) -> EvalResult<T> {
        self.fail(
            "E7010",
            format!("unknown compile-time name `{name}`"),
            span,
            "declare a constant, local binding, or `comptime fn`",
        )
    }

    fn type_error<T>(&mut self, role: &str, span: Span) -> EvalResult<T> {
        self.fail(
            "E7011",
            format!("invalid compile-time value for {role}"),
            span,
            "use values and operators with compatible compile-time types",
        )
    }

    fn out_of_bounds<T>(&mut self, span: Span) -> EvalResult<T> {
        self.fail(
            "E7011",
            "compile-time index is out of bounds",
            span,
            "use an index inside the aggregate length",
        )
    }

    fn fail<T>(
        &mut self,
        code: &'static str,
        message: impl Into<String>,
        span: Span,
        help: impl Into<String>,
    ) -> EvalResult<T> {
        self.report(code, message, span, help);
        Err(EvalFailure::Reported)
    }

    fn report(
        &mut self,
        code: &'static str,
        message: impl Into<String>,
        span: Span,
        help: impl Into<String>,
    ) {
        if self.emit_errors {
            self.diagnostics
                .push(Diagnostic::error(code, message, span).with_help(help));
        }
    }
}

#[derive(Debug, Clone)]
struct Variable {
    mutable: bool,
    value: Value,
}

#[derive(Debug, Clone, Default)]
struct Frame {
    scopes: Vec<HashMap<String, Variable>>,
    type_bindings: HashMap<String, TypeName>,
}

impl Frame {
    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        let _ = self.scopes.pop();
    }

    fn insert(&mut self, name: String, value: Variable) {
        if self.scopes.is_empty() {
            self.push_scope();
        }
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name, value);
        }
    }

    fn lookup(&self, name: &str) -> Option<&Value> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).map(|variable| &variable.value))
    }

    fn assign(&mut self, name: &str, value: Value) -> bool {
        let Some(variable) = self
            .scopes
            .iter_mut()
            .rev()
            .find_map(|scope| scope.get_mut(name))
        else {
            return false;
        };
        if !variable.mutable {
            return false;
        }
        variable.value = value;
        true
    }
}

enum Flow {
    Value(Value),
    Return(Value),
    Break(Option<Value>),
    Continue,
}

enum EvalFailure {
    Deferred,
    Reported,
    Return(Value),
    Break(Option<Value>),
    Continue,
}

type EvalResult<T> = Result<T, EvalFailure>;

fn compare_integer(left: Integer, right: Integer) -> std::cmp::Ordering {
    match (left.negative, right.negative) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        (false, false) => left.magnitude.cmp(&right.magnitude),
        (true, true) => right.magnitude.cmp(&left.magnitude),
    }
}

fn float_equals(left: f64, right: f64) -> bool {
    left.partial_cmp(&right)
        .is_some_and(std::cmp::Ordering::is_eq)
}

fn integer_width(name: &str) -> Option<(u32, bool)> {
    Some(match name {
        "i8" => (8, true),
        "i16" => (16, true),
        "i32" => (32, true),
        "i64" => (64, true),
        "i128" => (128, true),
        "isize" => (usize::BITS, true),
        "u8" => (8, false),
        "u16" => (16, false),
        "u32" => (32, false),
        "u64" => (64, false),
        "u128" => (128, false),
        "usize" => (usize::BITS, false),
        _ => return None,
    })
}

fn value_matches_type(
    value: &Value,
    type_name: &TypeName,
    bindings: &HashMap<String, TypeName>,
) -> bool {
    match &type_name.kind {
        ast::TypeNameKind::Unit => value == &Value::Unit,
        ast::TypeNameKind::Path(path) => {
            let Some(name) = single_path_name(path) else {
                return false;
            };
            if let Some(bound) = bindings.get(name) {
                return value_matches_type(value, bound, bindings);
            }
            match name {
                "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64"
                | "u128" | "usize" => {
                    let Value::Integer(integer) = value else {
                        return false;
                    };
                    integer_fits(*integer, name)
                }
                "f32" | "f64" => matches!(value, Value::Float(_)),
                "bool" => matches!(value, Value::Boolean(_)),
                "char" => matches!(value, Value::Character(_)),
                "str" | "cstr" => matches!(value, Value::String(_)),
                _ => matches!(value, Value::Record(_)),
            }
        }
        ast::TypeNameKind::Tuple(elements) => {
            let Value::Tuple(values) = value else {
                return false;
            };
            values.len() == elements.len()
                && values
                    .iter()
                    .zip(elements)
                    .all(|(value, element)| value_matches_type(value, element, bindings))
        }
        ast::TypeNameKind::Array { element, .. } => {
            let Value::Array(values) = value else {
                return false;
            };
            values
                .iter()
                .all(|value| value_matches_type(value, element, bindings))
        }
        ast::TypeNameKind::Generic { path, .. } => single_path_name(path).is_some_and(|name| {
            bindings
                .get(name)
                .is_some_and(|bound| value_matches_type(value, bound, bindings))
        }),
        ast::TypeNameKind::Function { .. }
        | ast::TypeNameKind::Slice(_)
        | ast::TypeNameKind::Reference { .. }
        | ast::TypeNameKind::RawPointer { .. } => false,
    }
}

fn integer_fits(value: Integer, target: &str) -> bool {
    let Some((bits, signed)) = integer_width(target) else {
        return false;
    };
    if value.negative {
        return signed && value.magnitude <= 1_u128 << (bits - 1);
    }
    let maximum = if signed {
        (1_u128 << (bits - 1)) - 1
    } else if bits == 128 {
        u128::MAX
    } else {
        (1_u128 << bits) - 1
    };
    value.magnitude <= maximum
}

#[expect(
    clippy::cast_precision_loss,
    reason = "source-level integer-to-float casts intentionally use IEEE rounding"
)]
fn integer_as_f64(value: Integer) -> f64 {
    let magnitude = value.magnitude as f64;
    if value.negative {
        -magnitude
    } else {
        magnitude
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "source-level f32 casts intentionally apply IEEE narrowing"
)]
fn narrow_f64(value: f64) -> f32 {
    value as f32
}

fn assignment_binary_operator(operator: AssignmentOperator) -> BinaryOperator {
    match operator {
        AssignmentOperator::Assign | AssignmentOperator::Add => BinaryOperator::Add,
        AssignmentOperator::Subtract => BinaryOperator::Subtract,
        AssignmentOperator::Multiply => BinaryOperator::Multiply,
        AssignmentOperator::Divide => BinaryOperator::Divide,
        AssignmentOperator::Remainder => BinaryOperator::Remainder,
        AssignmentOperator::BitAnd => BinaryOperator::BitAnd,
        AssignmentOperator::BitXor => BinaryOperator::BitXor,
        AssignmentOperator::BitOr => BinaryOperator::BitOr,
        AssignmentOperator::ShiftLeft => BinaryOperator::ShiftLeft,
        AssignmentOperator::ShiftRight => BinaryOperator::ShiftRight,
    }
}

fn statement_span(statement: &Statement) -> Span {
    match statement {
        Statement::Let(statement) => statement.span,
        Statement::Expression(statement) => statement.span,
        Statement::Defer(statement) => statement.span,
        Statement::Return(statement) => statement.span,
        Statement::While(statement) => statement.span,
        Statement::For(statement) => statement.span,
        Statement::Break(statement) => statement.span,
        Statement::Continue(span) => *span,
    }
}

fn single_path_name(path: &ast::Path) -> Option<&str> {
    let [name] = path.segments.as_slice() else {
        return None;
    };
    Some(&name.name)
}

fn is_metadata_intrinsic(path: &ast::Path) -> bool {
    matches!(
        path.segments.as_slice(),
        [name] if matches!(name.name.as_str(), "size_of" | "align_of" | "fields" | "variants")
    ) || matches!(
        path.segments.as_slice(),
        [namespace, name]
            if namespace.name == "meta"
                && matches!(
                    name.name.as_str(),
                    "name" | "fields" | "variants" | "traits"
                )
    )
}

fn substitute_type(type_name: &TypeName, bindings: &HashMap<String, TypeName>) -> TypeName {
    let ast::TypeNameKind::Path(path) = &type_name.kind else {
        return type_name.clone();
    };
    let Some(name) = single_path_name(path) else {
        return type_name.clone();
    };
    bindings
        .get(name)
        .cloned()
        .unwrap_or_else(|| type_name.clone())
}
