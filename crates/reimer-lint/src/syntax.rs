use std::collections::HashSet;

use reimer_ast::{
    self as ast, BinaryOperator, Expression, FieldName, ImportKind, Item, Pattern, Statement,
    TypeNameKind,
};
use reimer_diagnostics::Span;

use crate::walk::{self, Visitor};
use crate::{Finding, Fix, Severity, organize_imports};

pub(crate) fn lint(source: &str, program: &ast::Program) -> Vec<Finding> {
    let mut findings = Vec::new();
    if let Some(fix) = organize_imports(source, program) {
        findings.push(Finding {
            code: "L1001".to_owned(),
            severity: Severity::Hint,
            message: "imports are not in canonical order".to_owned(),
            span: fix.span,
            help: Some(
                "place `std` imports first and sort `::` paths and imported names".to_owned(),
            ),
            fixes: vec![fix],
        });
    }

    let consuming_methods = consuming_method_names(program);
    for function in functions(program) {
        let mut linter = FunctionLinter::new(source, &consuming_methods);
        walk::function_declaration(&mut linter, function);
        findings.extend(linter.finish());
    }
    findings
}

pub(crate) fn attach_spelling_fixes(
    source: &str,
    program: &ast::Program,
    findings: &mut [Finding],
) {
    let candidates = spelling_candidates(program);
    for finding in findings {
        if !matches!(
            finding.code.as_str(),
            "E3005" | "E3102" | "E3118" | "E3121" | "E3123" | "E6003"
        ) {
            continue;
        }
        let Some((misspelled, span)) = misspelled_identifier(source, finding.span) else {
            continue;
        };
        let Some(candidate) = closest_name(misspelled, &candidates) else {
            continue;
        };
        finding.help = Some(match &finding.help {
            Some(help) => format!("{help}; did you mean `{candidate}`?"),
            None => format!("did you mean `{candidate}`?"),
        });
        finding.fixes.push(Fix {
            title: format!("Replace with `{candidate}`"),
            span,
            replacement: candidate.to_owned(),
        });
    }
}

fn functions(program: &ast::Program) -> impl Iterator<Item = &ast::Function> {
    program.items.iter().flat_map(|item| match item {
        Item::Function(function) => std::slice::from_ref(function),
        Item::Impl(declaration) => declaration.methods.as_slice(),
        _ => &[],
    })
}

fn consuming_method_names(program: &ast::Program) -> HashSet<&str> {
    program
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Impl(declaration) => Some(declaration.methods.as_slice()),
            _ => None,
        })
        .flatten()
        .filter(|method| {
            method.parameters.first().is_some_and(|receiver| {
                receiver.name.name == "self"
                    && !matches!(receiver.ty.kind, TypeNameKind::Reference { .. })
            })
        })
        .map(|method| method.name.name.as_str())
        .collect()
}

struct MutableBinding {
    name: String,
    span: Span,
}

struct ResourceBinding {
    name: String,
    span: Span,
    kind: &'static str,
}

struct FunctionLinter<'source, 'program> {
    source: &'source str,
    consuming_methods: &'program HashSet<&'program str>,
    findings: Vec<Finding>,
    mutable_bindings: Vec<MutableBinding>,
    assigned: HashSet<String>,
    resources: Vec<ResourceBinding>,
    released: HashSet<String>,
    transferred: HashSet<String>,
}

impl<'source, 'program> FunctionLinter<'source, 'program> {
    fn new(source: &'source str, consuming_methods: &'program HashSet<&'program str>) -> Self {
        Self {
            source,
            consuming_methods,
            findings: Vec::new(),
            mutable_bindings: Vec::new(),
            assigned: HashSet::new(),
            resources: Vec::new(),
            released: HashSet::new(),
            transferred: HashSet::new(),
        }
    }

    fn finish(mut self) -> Vec<Finding> {
        for binding in self.mutable_bindings {
            if self.assigned.contains(&binding.name) {
                continue;
            }
            let fixes = mut_keyword_span(self.source, binding.span)
                .map(|span| Fix {
                    title: "Remove unnecessary `mut`".to_owned(),
                    span,
                    replacement: String::new(),
                })
                .into_iter()
                .collect();
            self.findings.push(Finding {
                code: "L2001".to_owned(),
                severity: Severity::Hint,
                message: format!("`{}` is declared mutable but never assigned", binding.name),
                span: binding.span,
                help: Some("remove `mut` to document that the binding is immutable".to_owned()),
                fixes,
            });
        }
        for resource in self.resources {
            if self.released.contains(&resource.name) || self.transferred.contains(&resource.name) {
                continue;
            }
            self.findings.push(Finding {
                code: "L2010".to_owned(),
                severity: Severity::Warning,
                message: format!(
                    "{} `{}` has no visible cleanup or ownership transfer",
                    resource.kind, resource.name
                ),
                span: resource.span,
                help: Some(
                    "call `.deinit()` (preferably through `defer`) or return/move the owner"
                        .to_owned(),
                ),
                fixes: Vec::new(),
            });
        }
        self.findings
    }

    fn lint_boolean_comparison(&mut self, binary: &ast::BinaryExpression) {
        if !matches!(
            binary.operator,
            BinaryOperator::Equal | BinaryOperator::NotEqual
        ) {
            return;
        }
        let (value, boolean) = match (&binary.left, &binary.right) {
            (value, Expression::Boolean(boolean)) | (Expression::Boolean(boolean), value) => {
                (value, boolean.value)
            }
            _ => return,
        };
        let positive = (binary.operator == BinaryOperator::Equal) == boolean;
        let Some(value_text) = self.source.get(value.span().start..value.span().end) else {
            return;
        };
        let replacement = if positive {
            value_text.to_owned()
        } else {
            format!("!({value_text})")
        };
        self.findings.push(Finding {
            code: "L2002".to_owned(),
            severity: Severity::Hint,
            message: "boolean comparison is redundant".to_owned(),
            span: binary.span,
            help: Some("use the boolean value directly".to_owned()),
            fixes: vec![Fix {
                title: "Simplify boolean comparison".to_owned(),
                span: binary.span,
                replacement,
            }],
        });
    }

    fn record_call(&mut self, call: &ast::CallExpression) {
        let Some(name) = call_name(&call.callee) else {
            return;
        };
        if matches!(name, "Ok" | "Err" | "Some") {
            for argument in &call.arguments {
                if let Some(owner) = owned_root_name(argument) {
                    self.transferred.insert(owner.to_owned());
                }
            }
        }
        if let Expression::Field(field) = &call.callee
            && let Some(owner) = root_name(&field.base)
        {
            // A method signature may require `&mut self`; the syntax-only lint
            // stays conservative until typed effect information is available.
            self.assigned.insert(owner.to_owned());
            if self.consuming_methods.contains(name) {
                self.transferred.insert(owner.to_owned());
            }
        }
        if name == "deinit" {
            if let Expression::Field(field) = &call.callee
                && let Some(owner) = root_name(&field.base)
            {
                self.released.insert(owner.to_owned());
            }
        } else if matches!(
            name,
            "deinit_bytes"
                | "arena_allocator_deinit"
                | "fixed_buffer_allocator_deinit"
                | "string_deinit"
        ) && let Some(owner) = call.arguments.first().and_then(root_name)
        {
            self.released.insert(owner.to_owned());
        }
    }
}

impl Visitor for FunctionLinter<'_, '_> {
    fn statement(&mut self, statement: &Statement) {
        match statement {
            Statement::Let(binding) => {
                if binding.mutable {
                    self.mutable_bindings.push(MutableBinding {
                        name: binding.name.name.clone(),
                        span: binding.name.span,
                    });
                }
                if let Some(kind) = resource_kind(&binding.initializer) {
                    self.resources.push(ResourceBinding {
                        name: binding.name.name.clone(),
                        span: binding.name.span,
                        kind,
                    });
                }
            }
            Statement::Return(statement) => {
                if let Some(name) = statement.value.as_ref().and_then(root_name) {
                    self.transferred.insert(name.to_owned());
                }
            }
            Statement::While(statement)
                if matches!(
                    statement.condition,
                    Expression::Boolean(ast::BooleanLiteral { value: true, .. })
                ) =>
            {
                self.findings.push(Finding {
                    code: "L2003".to_owned(),
                    severity: Severity::Hint,
                    message: "`while true` obscures an unconditional loop".to_owned(),
                    span: statement.condition.span(),
                    help: Some("use `loop { ... }`".to_owned()),
                    fixes: Vec::new(),
                });
            }
            _ => {}
        }
    }

    fn expression(&mut self, expression: &Expression) {
        match expression {
            Expression::Assignment(assignment) => {
                if let Some(name) = root_name(&assignment.target) {
                    self.assigned.insert(name.to_owned());
                }
            }
            Expression::Unary(unary) if unary.operator == ast::UnaryOperator::BorrowMut => {
                if let Some(name) = root_name(&unary.operand) {
                    self.assigned.insert(name.to_owned());
                }
            }
            Expression::Binary(binary) => self.lint_boolean_comparison(binary),
            Expression::Match(matching) => {
                if let Expression::Path(path) = &matching.scrutinee
                    && path.segments.len() == 1
                    && let Some(name) = path.segments.first()
                {
                    self.transferred.insert(name.name.clone());
                }
            }
            Expression::Struct(structure) => {
                for field in &structure.fields {
                    if let Some(name) = owned_root_name(&field.value) {
                        self.transferred.insert(name.to_owned());
                    }
                }
            }
            Expression::Unsafe(block) if block.statements.is_empty() && block.tail.is_none() => {
                self.findings.push(Finding {
                    code: "L2004".to_owned(),
                    severity: Severity::Warning,
                    message: "empty `unsafe` block has no effect".to_owned(),
                    span: block.span,
                    help: Some(
                        "remove the block or place the required unsafe operation inside it"
                            .to_owned(),
                    ),
                    fixes: vec![Fix {
                        title: "Replace empty unsafe block with unit".to_owned(),
                        span: block.span,
                        replacement: "()".to_owned(),
                    }],
                });
            }
            Expression::Call(call) => self.record_call(call),
            _ => {}
        }
    }
}

fn mut_keyword_span(source: &str, name_span: Span) -> Option<Span> {
    let line_start = source
        .get(..name_span.start)?
        .rfind('\n')
        .map_or(0, |index| index + 1);
    let prefix = source.get(line_start..name_span.start)?;
    let position = prefix.rfind("mut")?;
    let start = line_start + position;
    let before_ok = prefix
        .get(..position)
        .and_then(|before| before.chars().next_back())
        .is_none_or(|character| !is_identifier_continue(character));
    let after = source.get(start + 3..name_span.start)?;
    if !before_ok || !after.chars().all(char::is_whitespace) {
        return None;
    }
    Some(Span::new(start, name_span.start))
}

fn resource_kind(expression: &Expression) -> Option<&'static str> {
    let expression = match expression {
        Expression::Try { value, .. } => value,
        _ => expression,
    };
    let Expression::Call(call) = expression else {
        return None;
    };
    match call_name(&call.callee)? {
        "allocate_bytes" => Some("owned allocation"),
        "read" | "read_exact" | "read_line" | "read_to_end" => Some("owned input buffer"),
        "from" | "with_capacity" if callee_contains_name(&call.callee, "String") => {
            Some("owned string")
        }
        "clone_in" => Some("owned string clone"),
        "concat" | "concat3" | "repeat" | "join_strings" | "to_lowercase" | "to_uppercase" => {
            Some("owned string")
        }
        "init" if callee_contains_name(&call.callee, "ArenaAllocator") => Some("arena allocator"),
        "init" if callee_contains_name(&call.callee, "FixedBufferAllocator") => {
            Some("fixed-buffer allocator")
        }
        _ => None,
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
        Expression::Field(field) => callee_contains_name(&field.base, expected),
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
        Expression::Try { value, .. } => root_name(value),
        _ => None,
    }
}

fn owned_root_name(expression: &Expression) -> Option<&str> {
    match expression {
        Expression::Unary(unary)
            if matches!(
                unary.operator,
                ast::UnaryOperator::Borrow | ast::UnaryOperator::BorrowMut
            ) =>
        {
            None
        }
        Expression::Try { value, .. } => owned_root_name(value),
        Expression::Cast(cast) => owned_root_name(&cast.value),
        _ => root_name(expression),
    }
}

fn spelling_candidates(program: &ast::Program) -> HashSet<String> {
    let mut collector = NameCollector {
        names: primitive_names().map(str::to_owned).collect(),
    };
    walk::program(&mut collector, program);
    for item in &program.items {
        if let Item::Import(import) = item {
            match &import.kind {
                ImportKind::Module { path, alias } => {
                    if let Some(name) = alias.as_ref().or_else(|| path.segments.last()) {
                        collector.names.insert(name.name.clone());
                    }
                }
                ImportKind::Symbols { names, .. } => {
                    for imported in names {
                        collector.names.insert(
                            imported
                                .alias
                                .as_ref()
                                .unwrap_or(&imported.name)
                                .name
                                .clone(),
                        );
                    }
                }
            }
        }
    }
    collector.names
}

struct NameCollector {
    names: HashSet<String>,
}

impl Visitor for NameCollector {
    fn item(&mut self, item: &Item) {
        let name = match item {
            Item::Function(function) => Some(&function.name),
            Item::ExternFunction(function) => Some(&function.name),
            Item::Struct(declaration) => Some(&declaration.name),
            Item::Enum(declaration) => Some(&declaration.name),
            Item::TypeAlias(declaration) => Some(&declaration.name),
            Item::Trait(declaration) => Some(&declaration.name),
            Item::Constant(declaration) => Some(&declaration.name),
            Item::Static(declaration) => Some(&declaration.name),
            Item::Import(_) | Item::Impl(_) | Item::Comptime(_) => None,
        };
        if let Some(name) = name {
            self.names.insert(name.name.clone());
        }
        match item {
            Item::Struct(declaration) => {
                self.names.extend(
                    declaration
                        .fields
                        .iter()
                        .map(|field| field.name.name.clone()),
                );
            }
            Item::Enum(declaration) => {
                for variant in &declaration.variants {
                    self.names.insert(variant.name.name.clone());
                    if let ast::EnumVariantPayload::Struct(fields) = &variant.payload {
                        self.names
                            .extend(fields.iter().map(|field| field.name.name.clone()));
                    }
                }
            }
            _ => {}
        }
    }

    fn function(&mut self, function: &ast::Function) {
        self.names.extend(
            function
                .parameters
                .iter()
                .map(|parameter| parameter.name.name.clone()),
        );
    }

    fn statement(&mut self, statement: &Statement) {
        if let Statement::Let(binding) = statement {
            self.names.insert(binding.name.name.clone());
        }
    }

    fn pattern(&mut self, pattern: &Pattern) {
        if let Pattern::Identifier { name, .. } = pattern {
            self.names.insert(name.name.clone());
        }
    }

    fn type_name(&mut self, type_name: &ast::TypeName) {
        let path = match &type_name.kind {
            TypeNameKind::Path(path) | TypeNameKind::Generic { path, .. } => Some(path),
            _ => None,
        };
        if let Some(path) = path
            && let Some(name) = path.segments.last()
        {
            self.names.insert(name.name.clone());
        }
    }
}

fn primitive_names() -> impl Iterator<Item = &'static str> {
    [
        "i8",
        "i16",
        "i32",
        "i64",
        "i128",
        "isize",
        "u8",
        "u16",
        "u32",
        "u64",
        "u128",
        "usize",
        "f32",
        "f64",
        "bool",
        "char",
        "str",
        "cstr",
        "c_char",
        "c_schar",
        "c_uchar",
        "c_short",
        "c_ushort",
        "c_int",
        "c_uint",
        "c_long",
        "c_ulong",
        "c_longlong",
        "c_ulonglong",
        "c_float",
        "c_double",
        "c_size",
        "c_ptrdiff",
        "Option",
        "Result",
        "Some",
        "None",
        "Ok",
        "Err",
    ]
    .into_iter()
}

fn misspelled_identifier(source: &str, span: Span) -> Option<(&str, Span)> {
    let text = source.get(span.start..span.end)?.trim();
    let suffix = text.rsplit("::").next()?;
    if suffix.is_empty() || !suffix.chars().all(is_identifier_continue) {
        return None;
    }
    let relative = text.len().checked_sub(suffix.len())?;
    let leading_whitespace = source
        .get(span.start..)?
        .chars()
        .take_while(|character| character.is_whitespace())
        .map(char::len_utf8)
        .sum::<usize>();
    let start = span.start + leading_whitespace + relative;
    Some((suffix, Span::new(start, start + suffix.len())))
}

fn closest_name<'names>(
    misspelled: &str,
    candidates: &'names HashSet<String>,
) -> Option<&'names str> {
    let maximum = (misspelled.chars().count() / 3).clamp(1, 3);
    candidates
        .iter()
        .filter(|candidate| candidate.as_str() != misspelled)
        .filter_map(|candidate| {
            let distance = edit_distance(misspelled, candidate);
            (distance <= maximum).then_some((distance, candidate.as_str()))
        })
        .min_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(right.1)))
        .map(|(_, candidate)| candidate)
}

fn edit_distance(left: &str, right: &str) -> usize {
    let right: Vec<_> = right.chars().collect();
    let mut previous: Vec<_> = (0..=right.len()).collect();
    let mut current = vec![0; right.len() + 1];
    for (left_index, left_character) in left.chars().enumerate() {
        current[0] = left_index + 1;
        for (right_index, right_character) in right.iter().enumerate() {
            let substitution = usize::from(left_character != *right_character);
            current[right_index + 1] = (previous[right_index + 1] + 1)
                .min(current[right_index] + 1)
                .min(previous[right_index] + substitution);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

fn is_identifier_continue(character: char) -> bool {
    character == '_' || character.is_alphanumeric()
}

#[cfg(test)]
mod tests {
    use reimer_lexer::lex;
    use reimer_parser::parse;

    use super::lint;

    #[test]
    fn lint_should_offer_to_remove_unused_mutability() {
        let source = "fn main() -> i32 { let mut answer = 42; answer }";
        let syntax =
            parse(&lex(source).expect("fixture should lex")).expect("fixture should parse");

        let findings = lint(source, &syntax);

        assert!(findings.iter().any(|finding| finding.code == "L2001"));
    }

    #[test]
    fn lint_should_not_report_a_resource_that_is_cleaned_with_defer() {
        let source = "fn main() -> i32 { let bytes = allocate_bytes(&allocator, 64)?; defer bytes.deinit(); 0 }";
        let syntax =
            parse(&lex(source).expect("fixture should lex")).expect("fixture should parse");

        let findings = lint(source, &syntax);

        assert!(!findings.iter().any(|finding| finding.code == "L2010"));
    }

    #[test]
    fn lint_should_treat_match_scrutinee_as_ownership_transfer() {
        let source = "
            fn main() -> i32 {
                let allocation = allocate_bytes(&allocator, 64);
                let bytes = match allocation {
                    Ok(value) => value,
                    Err(_) => { return 1; },
                };
                defer bytes.deinit();
                0
            }
        ";
        let syntax =
            parse(&lex(source).expect("fixture should lex")).expect("fixture should parse");

        let findings = lint(source, &syntax);

        assert!(!findings.iter().any(|finding| finding.code == "L2010"));
    }

    #[test]
    fn lint_should_treat_owned_struct_field_as_ownership_transfer() {
        let source = "
            struct Buffer { storage: OwnedBytes }
            fn create() -> Result<Buffer, AllocError> {
                let storage = allocate_bytes(&allocator, 64)?;
                Ok(Buffer { storage: storage })
            }
        ";
        let syntax =
            parse(&lex(source).expect("fixture should lex")).expect("fixture should parse");

        let findings = lint(source, &syntax);

        assert!(!findings.iter().any(|finding| finding.code == "L2010"));
    }

    #[test]
    fn lint_should_treat_result_payload_as_ownership_transfer() {
        let source = "
            fn create() -> Result<String, AllocError> {
                let output = String::with_capacity(&allocator, 64)?;
                Ok(output)
            }
        ";
        let syntax =
            parse(&lex(source).expect("fixture should lex")).expect("fixture should parse");

        let findings = lint(source, &syntax);

        assert!(!findings.iter().any(|finding| finding.code == "L2010"));
    }

    #[test]
    fn lint_should_keep_resource_warning_for_borrowed_match_scrutinee() {
        let source = "
            fn main() -> i32 {
                let allocation = allocate_bytes(&allocator, 64);
                match &allocation { _ => () }
                0
            }
        ";
        let syntax =
            parse(&lex(source).expect("fixture should lex")).expect("fixture should parse");

        let findings = lint(source, &syntax);

        assert!(findings.iter().any(|finding| finding.code == "L2010"));
    }

    #[test]
    fn lint_should_treat_by_value_method_receiver_as_ownership_transfer() {
        let source = "
            struct Buffer { value: i32 }
            impl Buffer {
                fn into_value(self) -> i32 { self.value }
            }
            fn main() -> i32 {
                let buffer = read_to_end(&allocator)?;
                buffer.into_value()
            }
        ";
        let syntax =
            parse(&lex(source).expect("fixture should lex")).expect("fixture should parse");

        let findings = lint(source, &syntax);

        assert!(!findings.iter().any(|finding| finding.code == "L2010"));
    }

    #[test]
    fn lint_should_keep_resource_warning_for_borrowed_method_receiver() {
        let source = "
            struct Buffer { value: i32 }
            impl Buffer {
                fn value(&self) -> i32 { self.value }
            }
            fn main() -> i32 {
                let buffer = read_to_end(&allocator)?;
                buffer.value()
            }
        ";
        let syntax =
            parse(&lex(source).expect("fixture should lex")).expect("fixture should parse");

        let findings = lint(source, &syntax);

        assert!(findings.iter().any(|finding| finding.code == "L2010"));
    }

    #[test]
    fn lint_should_preserve_mutability_for_a_method_receiver() {
        let source = "fn main() { let mut values = [1]; values.push(2); }";
        let syntax =
            parse(&lex(source).expect("fixture should lex")).expect("fixture should parse");

        let findings = lint(source, &syntax);

        assert!(!findings.iter().any(|finding| finding.code == "L2001"));
    }
}
