use std::collections::HashMap;

use reimer_ast::{self as ast, Expression as AstExpression, Item, TypeNameKind};
use reimer_diagnostics::Span;
use reimer_hir::{self as hir, ExpressionKind, FunctionId, LocalId, PlaceKind, TypeDefinitionKind};
use reimer_types::{Type, TypeId};

use crate::walk::{self, Visitor};

/// A human-readable inferred type attached to source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeHint {
    /// Source range described by the type.
    pub span: Span,
    /// Compact source-like type or signature.
    pub label: String,
    /// Additional semantic context.
    pub detail: String,
}

/// A source use and its declaration location in the same document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DefinitionLink {
    /// Source range that refers to a declaration.
    pub use_span: Span,
    /// Exact declaration-name source range.
    pub target_span: Span,
}

pub(crate) fn index(
    syntax: &ast::Program,
    typed: &hir::Program,
) -> (Vec<TypeHint>, Vec<DefinitionLink>) {
    let syntax_index = SyntaxIndex::build(syntax);
    let mut type_hints = Vec::new();
    let mut definitions = syntax_index.type_definition_links();
    let function_targets = function_targets(typed, &syntax_index);

    for function in &typed.functions {
        let Some(ast_function) = syntax_index.function(function.span) else {
            continue;
        };
        type_hints.push(TypeHint {
            span: ast_function.name.span,
            label: function_signature(function, typed),
            detail: "function signature after type resolution".to_owned(),
        });

        let mut local_targets = HashMap::new();
        for (parameter, ast_parameter) in function.parameters.iter().zip(&ast_function.parameters) {
            local_targets.insert(parameter.local, ast_parameter.name.span);
            type_hints.push(TypeHint {
                span: ast_parameter.name.span,
                label: type_label(parameter.ty, typed),
                detail: "parameter type".to_owned(),
            });
        }
        collect_local_targets(
            &function.body,
            &syntax_index,
            typed,
            &mut local_targets,
            &mut type_hints,
        );
        let mut indexer = HirIndexer {
            typed,
            syntax_index: &syntax_index,
            local_targets: &local_targets,
            function_targets: &function_targets,
            type_hints: &mut type_hints,
            definitions: &mut definitions,
        };
        indexer.block(&function.body);
    }

    for function in &typed.extern_functions {
        if let Some(name_span) = syntax_index.function_name(function.span) {
            type_hints.push(TypeHint {
                span: name_span,
                label: extern_signature(function, typed),
                detail: format!("native function using the `{}` ABI", function.abi),
            });
        }
    }
    sort_and_deduplicate(&mut type_hints, &mut definitions);
    (type_hints, definitions)
}

pub(crate) fn syntax_definitions(syntax: &ast::Program) -> Vec<DefinitionLink> {
    SyntaxIndex::build(syntax).type_definition_links()
}

fn sort_and_deduplicate(type_hints: &mut Vec<TypeHint>, definitions: &mut Vec<DefinitionLink>) {
    type_hints.sort_by(|left, right| {
        left.span
            .start
            .cmp(&right.span.start)
            .then_with(|| left.span.end.cmp(&right.span.end))
            .then_with(|| left.label.cmp(&right.label))
    });
    type_hints.dedup_by(|left, right| left.span == right.span && left.label == right.label);
    definitions.sort_by_key(|link| {
        (
            link.use_span.start,
            link.use_span.end,
            link.target_span.start,
        )
    });
    definitions.dedup();
}

struct SyntaxIndex<'ast> {
    functions: HashMap<(usize, usize), &'ast ast::Function>,
    function_names: HashMap<(usize, usize), Span>,
    let_names: HashMap<(usize, usize), Span>,
    pattern_names: HashMap<(usize, usize), Span>,
    call_callees: HashMap<(usize, usize), Span>,
    type_uses: Vec<(String, Span)>,
    type_targets: HashMap<String, Span>,
}

impl<'ast> SyntaxIndex<'ast> {
    fn build(program: &'ast ast::Program) -> Self {
        let mut index = Self {
            functions: HashMap::new(),
            function_names: HashMap::new(),
            let_names: HashMap::new(),
            pattern_names: HashMap::new(),
            call_callees: HashMap::new(),
            type_uses: Vec::new(),
            type_targets: HashMap::new(),
        };
        for item in &program.items {
            match item {
                Item::Function(function) => index.insert_function(function),
                Item::ExternFunction(function) => {
                    index
                        .function_names
                        .insert(span_key(function.span), function.name.span);
                }
                Item::Struct(declaration) => {
                    index
                        .type_targets
                        .insert(declaration.name.name.clone(), declaration.name.span);
                }
                Item::Enum(declaration) => {
                    index
                        .type_targets
                        .insert(declaration.name.name.clone(), declaration.name.span);
                }
                Item::Trait(declaration) => {
                    index
                        .type_targets
                        .insert(declaration.name.name.clone(), declaration.name.span);
                }
                Item::Impl(declaration) => {
                    for method in &declaration.methods {
                        index.insert_function(method);
                    }
                }
                Item::Import(_) | Item::Constant(_) | Item::Comptime(_) => {}
            }
        }
        walk::program(&mut index, program);
        index
    }

    fn insert_function(&mut self, function: &'ast ast::Function) {
        self.functions.insert(span_key(function.span), function);
        self.function_names
            .insert(span_key(function.span), function.name.span);
    }

    fn function(&self, span: Span) -> Option<&'ast ast::Function> {
        self.functions.get(&span_key(span)).copied()
    }

    fn function_name(&self, span: Span) -> Option<Span> {
        self.function_names.get(&span_key(span)).copied()
    }

    fn type_definition_links(&self) -> Vec<DefinitionLink> {
        self.type_uses
            .iter()
            .filter_map(|(name, use_span)| {
                self.type_targets
                    .get(name)
                    .copied()
                    .map(|target_span| DefinitionLink {
                        use_span: *use_span,
                        target_span,
                    })
            })
            .collect()
    }
}

impl Visitor for SyntaxIndex<'_> {
    fn statement(&mut self, statement: &ast::Statement) {
        if let ast::Statement::Let(binding) = statement {
            self.let_names
                .insert(span_key(binding.span), binding.name.span);
        }
    }

    fn expression(&mut self, expression: &AstExpression) {
        if let AstExpression::Call(call) = expression {
            self.call_callees
                .insert(span_key(call.span), call.callee.span());
        }
    }

    fn pattern(&mut self, pattern: &ast::Pattern) {
        if let ast::Pattern::Identifier { name, span, .. } = pattern {
            self.pattern_names.insert(span_key(*span), name.span);
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
            self.type_uses.push((name.name.clone(), name.span));
        }
    }
}

fn function_targets(typed: &hir::Program, syntax: &SyntaxIndex<'_>) -> HashMap<FunctionId, Span> {
    let mut targets = HashMap::new();
    for function in &typed.functions {
        if let Some(span) = syntax.function_name(function.span) {
            targets.insert(function.id, span);
        }
    }
    for function in &typed.extern_functions {
        if let Some(span) = syntax.function_name(function.span) {
            targets.insert(function.id, span);
        }
    }
    targets
}

fn collect_local_targets(
    block: &hir::Block,
    syntax: &SyntaxIndex<'_>,
    typed: &hir::Program,
    local_targets: &mut HashMap<LocalId, Span>,
    type_hints: &mut Vec<TypeHint>,
) {
    for statement in &block.statements {
        match statement {
            hir::Statement::Let {
                local, ty, span, ..
            } => {
                if let Some(name_span) = syntax.let_names.get(&span_key(*span)).copied() {
                    local_targets.insert(*local, name_span);
                    type_hints.push(TypeHint {
                        span: name_span,
                        label: type_label(*ty, typed),
                        detail: "inferred local binding type".to_owned(),
                    });
                }
            }
            hir::Statement::While { body, .. } | hir::Statement::For { body, .. } => {
                collect_local_targets(body, syntax, typed, local_targets, type_hints);
            }
            _ => {}
        }
        if let hir::Statement::For { pattern, .. } = statement {
            collect_pattern_targets(pattern, syntax, typed, local_targets, type_hints);
        }
    }
    if let Some(tail) = &block.tail {
        collect_nested_pattern_targets(tail, syntax, typed, local_targets, type_hints);
    }
}

fn collect_nested_pattern_targets(
    expression: &hir::Expression,
    syntax: &SyntaxIndex<'_>,
    typed: &hir::Program,
    local_targets: &mut HashMap<LocalId, Span>,
    type_hints: &mut Vec<TypeHint>,
) {
    match &expression.kind {
        ExpressionKind::Match(matching) => {
            for arm in &matching.arms {
                collect_pattern_targets(&arm.pattern, syntax, typed, local_targets, type_hints);
                collect_nested_pattern_targets(&arm.body, syntax, typed, local_targets, type_hints);
            }
        }
        ExpressionKind::If(conditional) => {
            if let Some(else_branch) = &conditional.else_branch {
                collect_nested_pattern_targets(
                    else_branch,
                    syntax,
                    typed,
                    local_targets,
                    type_hints,
                );
            }
        }
        ExpressionKind::Block(block) => {
            collect_local_targets(block, syntax, typed, local_targets, type_hints);
        }
        ExpressionKind::Loop(looping) => {
            collect_local_targets(&looping.body, syntax, typed, local_targets, type_hints);
        }
        _ => {}
    }
}

fn collect_pattern_targets(
    pattern: &hir::Pattern,
    syntax: &SyntaxIndex<'_>,
    typed: &hir::Program,
    local_targets: &mut HashMap<LocalId, Span>,
    type_hints: &mut Vec<TypeHint>,
) {
    match &pattern.kind {
        hir::PatternKind::Binding { local, .. } => {
            if let Some(name_span) = syntax.pattern_names.get(&span_key(pattern.span)).copied() {
                local_targets.insert(*local, name_span);
                type_hints.push(TypeHint {
                    span: name_span,
                    label: type_label(pattern.ty, typed),
                    detail: "pattern binding type".to_owned(),
                });
            }
        }
        hir::PatternKind::Tuple(elements)
        | hir::PatternKind::Enum {
            fields: elements, ..
        } => {
            for element in elements {
                collect_pattern_targets(element, syntax, typed, local_targets, type_hints);
            }
        }
        hir::PatternKind::Wildcard
        | hir::PatternKind::Integer(_)
        | hir::PatternKind::Float32(_)
        | hir::PatternKind::Float64(_)
        | hir::PatternKind::Character(_)
        | hir::PatternKind::Boolean(_) => {}
    }
}

struct HirIndexer<'context> {
    typed: &'context hir::Program,
    syntax_index: &'context SyntaxIndex<'context>,
    local_targets: &'context HashMap<LocalId, Span>,
    function_targets: &'context HashMap<FunctionId, Span>,
    type_hints: &'context mut Vec<TypeHint>,
    definitions: &'context mut Vec<DefinitionLink>,
}

impl HirIndexer<'_> {
    fn block(&mut self, block: &hir::Block) {
        for statement in &block.statements {
            match statement {
                hir::Statement::Let { initializer, .. }
                | hir::Statement::Expression(initializer)
                | hir::Statement::Defer {
                    action: initializer,
                    ..
                } => self.expression(initializer),
                hir::Statement::Return { value, .. } | hir::Statement::Break { value, .. } => {
                    if let Some(value) = value {
                        self.expression(value);
                    }
                }
                hir::Statement::While {
                    condition, body, ..
                } => {
                    self.expression(condition);
                    self.block(body);
                }
                hir::Statement::For { iterable, body, .. } => {
                    self.expression(iterable);
                    self.block(body);
                }
                hir::Statement::Continue(_) => {}
            }
        }
        if let Some(tail) = &block.tail {
            self.expression(tail);
        }
    }

    fn expression(&mut self, expression: &hir::Expression) {
        self.type_hints.push(TypeHint {
            span: expression.span,
            label: type_label(expression.ty, self.typed),
            detail: "inferred expression type".to_owned(),
        });
        self.expression_kind(expression);
    }

    fn expression_kind(&mut self, expression: &hir::Expression) {
        match &expression.kind {
            ExpressionKind::Local(local) => self.link_local(expression.span, *local),
            ExpressionKind::Call {
                function,
                arguments,
            } => {
                if let Some(target_span) = self.function_targets.get(function).copied()
                    && let Some(use_span) = self
                        .syntax_index
                        .call_callees
                        .get(&span_key(expression.span))
                        .copied()
                {
                    self.definitions.push(DefinitionLink {
                        use_span,
                        target_span,
                    });
                }
                for argument in arguments {
                    self.expression(argument);
                }
            }
            ExpressionKind::Function(function) => {
                if let Some(target_span) = self.function_targets.get(function).copied() {
                    self.definitions.push(DefinitionLink {
                        use_span: expression.span,
                        target_span,
                    });
                }
            }
            ExpressionKind::IndirectCall { callee, arguments } => {
                self.expression(callee);
                for argument in arguments {
                    self.expression(argument);
                }
            }
            ExpressionKind::Tuple(elements)
            | ExpressionKind::Array(elements)
            | ExpressionKind::Struct(elements) => {
                for element in elements {
                    self.expression(element);
                }
            }
            ExpressionKind::Enum { fields, .. } => {
                for field in fields {
                    self.expression(field);
                }
            }
            ExpressionKind::Unary { operand, .. }
            | ExpressionKind::Dereference(operand)
            | ExpressionKind::StringData(operand)
            | ExpressionKind::StringLength(operand)
            | ExpressionKind::SliceLength(operand) => self.expression(operand),
            ExpressionKind::StringFromParts { data, length }
            | ExpressionKind::SliceFromParts { data, length } => {
                self.expression(data);
                self.expression(length);
            }
            ExpressionKind::Binary { left, right, .. } => {
                self.expression(left);
                self.expression(right);
            }
            ExpressionKind::If(_)
            | ExpressionKind::Match(_)
            | ExpressionKind::Loop(_)
            | ExpressionKind::Block(_) => self.control_expression(&expression.kind),
            ExpressionKind::Assign { .. }
            | ExpressionKind::Cast { .. }
            | ExpressionKind::Borrow { .. }
            | ExpressionKind::Panic { .. }
            | ExpressionKind::AllocateBytes { .. }
            | ExpressionKind::DeallocateBytes { .. }
            | ExpressionKind::ThreadSpawn { .. }
            | ExpressionKind::ThreadJoin { .. }
            | ExpressionKind::ThreadScope { .. }
            | ExpressionKind::SynchronizationCreate { .. }
            | ExpressionKind::SynchronizationLoad { .. }
            | ExpressionKind::SynchronizationReplace { .. }
            | ExpressionKind::ThreadLocalStore { .. }
            | ExpressionKind::ChannelCreate { .. }
            | ExpressionKind::ChannelSend { .. }
            | ExpressionKind::ChannelReceive { .. }
            | ExpressionKind::JobSubmit { .. }
            | ExpressionKind::JobWait { .. }
            | ExpressionKind::ParallelFor { .. }
            | ExpressionKind::Try { .. }
            | ExpressionKind::Field { .. }
            | ExpressionKind::Index { .. } => self.effect_expression(&expression.kind),
            ExpressionKind::Integer(_)
            | ExpressionKind::Float32(_)
            | ExpressionKind::Float64(_)
            | ExpressionKind::Character(_)
            | ExpressionKind::String(_)
            | ExpressionKind::CString(_)
            | ExpressionKind::Boolean(_)
            | ExpressionKind::Unit
            | ExpressionKind::TypeStride { .. } => {}
        }
    }

    fn link_local(&mut self, use_span: Span, local: LocalId) {
        if let Some(target_span) = self.local_targets.get(&local).copied() {
            self.definitions.push(DefinitionLink {
                use_span,
                target_span,
            });
        }
    }

    fn control_expression(&mut self, kind: &ExpressionKind) {
        match kind {
            ExpressionKind::If(conditional) => {
                self.expression(&conditional.condition);
                self.block(&conditional.then_branch);
                if let Some(else_branch) = &conditional.else_branch {
                    self.expression(else_branch);
                }
            }
            ExpressionKind::Match(matching) => {
                self.expression(&matching.scrutinee);
                for arm in &matching.arms {
                    if let Some(guard) = &arm.guard {
                        self.expression(guard);
                    }
                    self.expression(&arm.body);
                }
            }
            ExpressionKind::Loop(looping) => self.block(&looping.body),
            ExpressionKind::Block(block) => self.block(block),
            _ => {}
        }
    }

    fn effect_expression(&mut self, kind: &ExpressionKind) {
        match kind {
            ExpressionKind::Assign { target, value, .. } => {
                self.place(target);
                self.expression(value);
            }
            ExpressionKind::Cast { value, .. } | ExpressionKind::Try { value, .. } => {
                self.expression(value);
            }
            ExpressionKind::Borrow { place, .. } => self.place(place),
            ExpressionKind::Panic { message } => self.expression(message),
            ExpressionKind::AllocateBytes {
                allocator, length, ..
            } => {
                self.expression(allocator);
                self.expression(length);
            }
            ExpressionKind::DeallocateBytes {
                allocator,
                data,
                length,
            } => {
                self.expression(allocator);
                self.expression(data);
                self.expression(length);
            }
            ExpressionKind::ThreadSpawn {
                callback, argument, ..
            }
            | ExpressionKind::ThreadScope {
                callback, argument, ..
            } => {
                self.expression(callback);
                self.expression(argument);
            }
            ExpressionKind::SynchronizationCreate { value, .. } => self.expression(value),
            ExpressionKind::ThreadJoin { handle, .. }
            | ExpressionKind::SynchronizationLoad { handle, .. }
            | ExpressionKind::ChannelReceive { handle, .. }
            | ExpressionKind::JobWait { handle, .. } => self.expression(handle),
            ExpressionKind::SynchronizationReplace { handle, value, .. }
            | ExpressionKind::ThreadLocalStore { handle, value }
            | ExpressionKind::ChannelSend { handle, value, .. } => {
                self.expression(handle);
                self.expression(value);
            }
            ExpressionKind::ChannelCreate {
                probe, capacity, ..
            } => {
                self.expression(probe);
                self.expression(capacity);
            }
            ExpressionKind::JobSubmit {
                pool,
                callback,
                argument,
                ..
            } => {
                self.expression(pool);
                self.expression(callback);
                self.expression(argument);
            }
            ExpressionKind::ParallelFor {
                pool,
                slice,
                callback,
                minimum_chunk,
                ..
            } => {
                self.expression(pool);
                self.expression(slice);
                self.expression(callback);
                self.expression(minimum_chunk);
            }
            ExpressionKind::Field { base, .. } => self.expression(base),
            ExpressionKind::Index { base, index } => {
                self.expression(base);
                self.expression(index);
            }
            _ => {}
        }
    }

    fn place(&mut self, place: &hir::Place) {
        self.type_hints.push(TypeHint {
            span: place.span,
            label: type_label(place.ty, self.typed),
            detail: "assignable place type".to_owned(),
        });
        match &place.kind {
            PlaceKind::Local(local) => {
                if let Some(target_span) = self.local_targets.get(local).copied() {
                    self.definitions.push(DefinitionLink {
                        use_span: place.span,
                        target_span,
                    });
                }
            }
            PlaceKind::Field { base, .. } | PlaceKind::Index { base, .. } => {
                self.expression(base);
                if let PlaceKind::Index { index, .. } = &place.kind {
                    self.expression(index);
                }
            }
            PlaceKind::Dereference { pointer } => self.expression(pointer),
        }
    }
}

fn function_signature(function: &hir::Function, program: &hir::Program) -> String {
    let parameters = function
        .parameters
        .iter()
        .map(|parameter| format!("{}: {}", parameter.name, type_label(parameter.ty, program)))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "fn {}({parameters}) -> {}",
        function.name,
        type_label(function.return_type, program)
    )
}

fn extern_signature(function: &hir::ExternFunction, program: &hir::Program) -> String {
    let parameters = function
        .parameters
        .iter()
        .map(|parameter| format!("{}: {}", parameter.name, type_label(parameter.ty, program)))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "extern \"{}\" fn {}({parameters}) -> {}",
        function.abi,
        function.name,
        type_label(function.return_type, program)
    )
}

fn type_label(ty: Type, program: &hir::Program) -> String {
    type_label_at_depth(ty, program, 0)
}

fn type_label_at_depth(ty: Type, program: &hir::Program, depth: usize) -> String {
    if depth >= 16 {
        return ty.to_string();
    }
    let Some(definition) = definition(ty, program) else {
        return ty.to_string();
    };
    match &definition.kind {
        TypeDefinitionKind::Struct { .. } | TypeDefinitionKind::Enum { .. } => {
            definition.name.clone().unwrap_or_else(|| ty.to_string())
        }
        TypeDefinitionKind::Tuple { elements } => format!(
            "({})",
            elements
                .iter()
                .map(|element| type_label_at_depth(*element, program, depth + 1))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        TypeDefinitionKind::Array { element, length } => format!(
            "[{}; {length}]",
            type_label_at_depth(*element, program, depth + 1)
        ),
        TypeDefinitionKind::Reference { target, mutable } => {
            let mutability = if *mutable { "mut " } else { "" };
            format!(
                "&{mutability}{}",
                type_label_at_depth(*target, program, depth + 1)
            )
        }
        TypeDefinitionKind::RawPointer { target, mutable } => {
            let mutability = if *mutable { "mut " } else { "const " };
            format!(
                "*{mutability}{}",
                type_label_at_depth(*target, program, depth + 1)
            )
        }
        TypeDefinitionKind::Slice { element, mutable } => {
            let mutability = if *mutable { "mut " } else { "" };
            format!(
                "&{mutability}[{}]",
                type_label_at_depth(*element, program, depth + 1)
            )
        }
        TypeDefinitionKind::Function {
            parameters,
            return_type,
        } => format!(
            "fn({}) -> {}",
            parameters
                .iter()
                .map(|parameter| type_label_at_depth(*parameter, program, depth + 1))
                .collect::<Vec<_>>()
                .join(", "),
            type_label_at_depth(*return_type, program, depth + 1)
        ),
    }
}

fn definition(ty: Type, program: &hir::Program) -> Option<&hir::TypeDefinition> {
    let (Type::Struct(id)
    | Type::Enum(id)
    | Type::Tuple(id)
    | Type::Array(id)
    | Type::Reference(id)
    | Type::RawPointer(id)
    | Type::Slice(id)
    | Type::Function(id)) = ty
    else {
        return None;
    };
    definition_by_id(id, program)
}

fn definition_by_id(id: TypeId, program: &hir::Program) -> Option<&hir::TypeDefinition> {
    program.types.get(usize::try_from(id.0).ok()?)
}

const fn span_key(span: Span) -> (usize, usize) {
    (span.start, span.end)
}
