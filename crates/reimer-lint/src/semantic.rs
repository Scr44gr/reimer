use std::collections::HashMap;

use reimer_ast::{self as ast, Expression as AstExpression, Item, TypeNameKind};
use reimer_diagnostics::Span;
use reimer_hir::{
    self as hir, ExpressionKind, FunctionId, IntegerAdditionMode, LocalId, PlaceKind, StaticId,
    TypeDefinitionKind,
};
use reimer_types::{Type, TypeId};

use crate::walk::{self, Visitor};

/// A human-readable inferred type attached to source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeHint {
    /// Source range described by the type.
    pub span: Span,
    /// Compact source-like type or signature.
    pub label: String,
    /// User-facing documentation for the resolved type or callable.
    pub documentation: String,
    kind: TypeHintKind,
}

impl TypeHint {
    /// Returns whether this is the type of an unannotated local binding.
    #[must_use]
    pub const fn show_as_inlay(&self) -> bool {
        matches!(self.kind, TypeHintKind::LocalBinding)
    }

    /// Ranks equally narrow hover candidates by source-level usefulness.
    #[must_use]
    pub const fn hover_priority(&self) -> u8 {
        match self.kind {
            TypeHintKind::Callable | TypeHintKind::Binding | TypeHintKind::LocalBinding => 0,
            TypeHintKind::Place => 1,
            TypeHintKind::Value => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TypeHintKind {
    Callable,
    Binding,
    LocalBinding,
    Value,
    Place,
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
    source: &str,
    syntax: &ast::Program,
    typed: &hir::Program,
) -> (Vec<TypeHint>, Vec<DefinitionLink>) {
    let documentation = local_function_documentation(source, typed);
    index_with_documentation(syntax, typed, &documentation)
}

pub(crate) fn attach_type_documentation(source: &str, typed: &mut hir::Program) {
    for definition in &mut typed.types {
        if definition.name.is_some() {
            definition.documentation =
                reimer_package::documentation_before(source, definition.span.start);
        }
    }
    for value in &mut typed.statics {
        value.documentation = reimer_package::documentation_before(source, value.span.start);
    }
}

pub(crate) fn index_with_documentation(
    syntax: &ast::Program,
    typed: &hir::Program,
    documentation: &[(FunctionId, String)],
) -> (Vec<TypeHint>, Vec<DefinitionLink>) {
    let syntax_index = SyntaxIndex::build(syntax);
    let mut type_hints = Vec::new();
    let mut definitions = syntax_index.type_definition_links();
    let function_targets = function_targets(typed, &syntax_index);
    let static_targets = static_targets(typed, &syntax_index);
    let callable_hints = callable_hints(typed, documentation);
    definitions.extend(
        function_targets
            .values()
            .chain(static_targets.values())
            .copied()
            .map(self_definition_link),
    );

    for value in &typed.statics {
        let Some(name_span) = syntax_index.static_name(value.span) else {
            continue;
        };
        type_hints.push(TypeHint {
            span: name_span,
            label: static_signature(value, typed),
            documentation: static_documentation(value, typed),
            kind: TypeHintKind::Binding,
        });
    }

    for function in &typed.functions {
        let Some(ast_function) = syntax_index.function(function.span) else {
            continue;
        };
        type_hints.push(TypeHint {
            span: ast_function.name.span,
            label: function_signature(function, typed),
            documentation: function_documentation(documentation, function.id)
                .unwrap_or("Callable Reimer function.")
                .to_owned(),
            kind: TypeHintKind::Callable,
        });

        let mut local_targets = HashMap::new();
        let mut local_types = HashMap::new();
        for (parameter, ast_parameter) in function.parameters.iter().zip(&ast_function.parameters) {
            local_targets.insert(parameter.local, ast_parameter.name.span);
            local_types.insert(parameter.local, parameter.ty);
            type_hints.push(resolved_type_hint(
                ast_parameter.name.span,
                parameter.ty,
                typed,
                TypeHintKind::Binding,
            ));
        }
        collect_local_targets(
            &function.body,
            &syntax_index,
            typed,
            &mut local_targets,
            &mut local_types,
            &mut type_hints,
        );
        definitions.extend(local_targets.values().copied().map(self_definition_link));
        let mut indexer = HirIndexer {
            typed,
            syntax_index: &syntax_index,
            local_targets: &local_targets,
            local_types: &local_types,
            function_targets: &function_targets,
            static_targets: &static_targets,
            callable_hints: &callable_hints,
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
                documentation: function_documentation(documentation, function.id).map_or_else(
                    || {
                        format!(
                            "Native function using the `{}` application binary interface.",
                            function.abi
                        )
                    },
                    str::to_owned,
                ),
                kind: TypeHintKind::Callable,
            });
        }
    }
    sort_and_deduplicate(&mut type_hints, &mut definitions);
    (type_hints, definitions)
}

pub(crate) fn syntax_definitions(syntax: &ast::Program) -> Vec<DefinitionLink> {
    SyntaxIndex::build(syntax).type_definition_links()
}

fn local_function_documentation(source: &str, typed: &hir::Program) -> Vec<(FunctionId, String)> {
    typed
        .functions
        .iter()
        .map(|function| (function.id, function.span))
        .chain(
            typed
                .extern_functions
                .iter()
                .map(|function| (function.id, function.span)),
        )
        .filter_map(|(id, span)| {
            reimer_package::documentation_before(source, span.start)
                .map(|documentation| (id, documentation))
        })
        .collect()
}

#[derive(Debug, Clone)]
struct CallableHint {
    label: String,
    documentation: String,
}

fn callable_hints(
    typed: &hir::Program,
    documentation: &[(FunctionId, String)],
) -> HashMap<FunctionId, CallableHint> {
    let functions = typed.functions.iter().map(|function| {
        (
            function.id,
            CallableHint {
                label: function_signature(function, typed),
                documentation: function_documentation(documentation, function.id)
                    .unwrap_or("Callable Reimer function.")
                    .to_owned(),
            },
        )
    });
    let extern_functions = typed.extern_functions.iter().map(|function| {
        (
            function.id,
            CallableHint {
                label: extern_signature(function, typed),
                documentation: function_documentation(documentation, function.id).map_or_else(
                    || {
                        format!(
                            "Native function using the `{}` application binary interface.",
                            function.abi
                        )
                    },
                    str::to_owned,
                ),
            },
        )
    });
    functions.chain(extern_functions).collect()
}

fn function_documentation(
    documentation: &[(FunctionId, String)],
    function: FunctionId,
) -> Option<&str> {
    documentation
        .iter()
        .find(|(candidate, _)| *candidate == function)
        .map(|(_, documentation)| documentation.as_str())
}

fn static_targets(typed: &hir::Program, syntax_index: &SyntaxIndex<'_>) -> HashMap<StaticId, Span> {
    typed
        .statics
        .iter()
        .filter_map(|value| {
            syntax_index
                .static_name(value.span)
                .map(|span| (value.id, span))
        })
        .collect()
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

fn self_definition_link(span: Span) -> DefinitionLink {
    DefinitionLink {
        use_span: span,
        target_span: span,
    }
}

struct SyntaxIndex<'ast> {
    functions: HashMap<(usize, usize), &'ast ast::Function>,
    function_names: HashMap<(usize, usize), Span>,
    static_names: HashMap<(usize, usize), Span>,
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
            static_names: HashMap::new(),
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
                Item::TypeAlias(declaration) => {
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
                Item::Static(declaration) => {
                    index
                        .static_names
                        .insert(span_key(declaration.span), declaration.name.span);
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

    fn static_name(&self, span: Span) -> Option<Span> {
        self.static_names.get(&span_key(span)).copied()
    }

    fn type_definition_links(&self) -> Vec<DefinitionLink> {
        self.type_targets
            .values()
            .copied()
            .map(self_definition_link)
            .chain(self.type_uses.iter().filter_map(|(name, use_span)| {
                self.type_targets
                    .get(name)
                    .copied()
                    .map(|target_span| DefinitionLink {
                        use_span: *use_span,
                        target_span,
                    })
            }))
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
    local_types: &mut HashMap<LocalId, Type>,
    type_hints: &mut Vec<TypeHint>,
) {
    for statement in &block.statements {
        match statement {
            hir::Statement::Let {
                local,
                ty,
                initializer,
                span,
                ..
            } => {
                if let Some(name_span) = syntax.let_names.get(&span_key(*span)).copied() {
                    local_targets.insert(*local, name_span);
                    local_types.insert(*local, *ty);
                    type_hints.push(resolved_type_hint(
                        name_span,
                        *ty,
                        typed,
                        TypeHintKind::LocalBinding,
                    ));
                }
                collect_nested_pattern_targets(
                    initializer,
                    syntax,
                    typed,
                    local_targets,
                    local_types,
                    type_hints,
                );
            }
            hir::Statement::Expression(expression)
            | hir::Statement::Defer {
                action: expression, ..
            } => collect_nested_pattern_targets(
                expression,
                syntax,
                typed,
                local_targets,
                local_types,
                type_hints,
            ),
            hir::Statement::Return { value, .. } | hir::Statement::Break { value, .. } => {
                if let Some(value) = value {
                    collect_nested_pattern_targets(
                        value,
                        syntax,
                        typed,
                        local_targets,
                        local_types,
                        type_hints,
                    );
                }
            }
            hir::Statement::While {
                condition, body, ..
            } => {
                collect_nested_pattern_targets(
                    condition,
                    syntax,
                    typed,
                    local_targets,
                    local_types,
                    type_hints,
                );
                collect_local_targets(body, syntax, typed, local_targets, local_types, type_hints);
            }
            hir::Statement::For {
                pattern,
                iterable,
                body,
                ..
            } => {
                collect_pattern_targets(
                    pattern,
                    syntax,
                    typed,
                    local_targets,
                    local_types,
                    type_hints,
                );
                collect_nested_pattern_targets(
                    iterable,
                    syntax,
                    typed,
                    local_targets,
                    local_types,
                    type_hints,
                );
                collect_local_targets(body, syntax, typed, local_targets, local_types, type_hints);
            }
            hir::Statement::Continue(_) => {}
        }
    }
    if let Some(tail) = &block.tail {
        collect_nested_pattern_targets(tail, syntax, typed, local_targets, local_types, type_hints);
    }
}

fn collect_nested_pattern_targets(
    expression: &hir::Expression,
    syntax: &SyntaxIndex<'_>,
    typed: &hir::Program,
    local_targets: &mut HashMap<LocalId, Span>,
    local_types: &mut HashMap<LocalId, Type>,
    type_hints: &mut Vec<TypeHint>,
) {
    match &expression.kind {
        ExpressionKind::Match(matching) => {
            for arm in &matching.arms {
                collect_pattern_targets(
                    &arm.pattern,
                    syntax,
                    typed,
                    local_targets,
                    local_types,
                    type_hints,
                );
                collect_nested_pattern_targets(
                    &arm.body,
                    syntax,
                    typed,
                    local_targets,
                    local_types,
                    type_hints,
                );
            }
        }
        ExpressionKind::If(conditional) => {
            collect_local_targets(
                &conditional.then_branch,
                syntax,
                typed,
                local_targets,
                local_types,
                type_hints,
            );
            if let Some(else_branch) = &conditional.else_branch {
                collect_nested_pattern_targets(
                    else_branch,
                    syntax,
                    typed,
                    local_targets,
                    local_types,
                    type_hints,
                );
            }
        }
        ExpressionKind::Block(block) => {
            collect_local_targets(block, syntax, typed, local_targets, local_types, type_hints);
        }
        ExpressionKind::Loop(looping) => {
            collect_local_targets(
                &looping.body,
                syntax,
                typed,
                local_targets,
                local_types,
                type_hints,
            );
        }
        _ => {}
    }
}

fn collect_pattern_targets(
    pattern: &hir::Pattern,
    syntax: &SyntaxIndex<'_>,
    typed: &hir::Program,
    local_targets: &mut HashMap<LocalId, Span>,
    local_types: &mut HashMap<LocalId, Type>,
    type_hints: &mut Vec<TypeHint>,
) {
    match &pattern.kind {
        hir::PatternKind::Binding { local, .. } => {
            if let Some(name_span) = syntax.pattern_names.get(&span_key(pattern.span)).copied() {
                local_targets.insert(*local, name_span);
                local_types.insert(*local, pattern.ty);
                type_hints.push(resolved_type_hint(
                    name_span,
                    pattern.ty,
                    typed,
                    TypeHintKind::Binding,
                ));
            }
        }
        hir::PatternKind::Tuple(elements)
        | hir::PatternKind::Enum {
            fields: elements, ..
        } => {
            for element in elements {
                collect_pattern_targets(
                    element,
                    syntax,
                    typed,
                    local_targets,
                    local_types,
                    type_hints,
                );
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
    local_types: &'context HashMap<LocalId, Type>,
    function_targets: &'context HashMap<FunctionId, Span>,
    static_targets: &'context HashMap<StaticId, Span>,
    callable_hints: &'context HashMap<FunctionId, CallableHint>,
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
        self.type_hints.push(resolved_type_hint(
            expression.span,
            expression.ty,
            self.typed,
            TypeHintKind::Value,
        ));
        self.expression_kind(expression);
    }

    fn expression_kind(&mut self, expression: &hir::Expression) {
        match &expression.kind {
            ExpressionKind::Local(local) => self.link_local(expression.span, *local),
            ExpressionKind::Static(value) => self.link_static(expression.span, *value),
            ExpressionKind::Call {
                function,
                arguments,
            } => self.direct_call(*function, arguments, expression.span),
            ExpressionKind::Function(function) => {
                self.function_value(*function, expression.span);
            }
            ExpressionKind::IndirectCall { callee, arguments } => {
                self.expression(callee);
                for argument in arguments {
                    self.expression(argument);
                }
            }
            ExpressionKind::FormatPush {
                function, calls, ..
            } => self.format_push(*function, calls, expression.span),
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
            | ExpressionKind::SliceFromParts { data, length }
            | ExpressionKind::HashValue {
                value: data,
                seed: length,
            } => self.expression_pair(data, length),
            ExpressionKind::Binary { left, right, .. } => self.expression_pair(left, right),
            ExpressionKind::IntegerAddition { mode, left, right } => {
                self.integer_addition(*mode, left, right, expression.span);
            }
            ExpressionKind::StringBytes(source) => {
                self.string_iteration("bytes", source, expression.ty, expression.span);
            }
            ExpressionKind::StringChars(source) => {
                self.string_iteration("chars", source, expression.ty, expression.span);
            }
            ExpressionKind::CharsNext { iterator } => {
                self.chars_next(iterator, expression.ty, expression.span);
            }
            ExpressionKind::SliceGet {
                slice,
                index,
                reference_type,
                mutable,
            } => self.slice_get(slice, index, *reference_type, *mutable, expression.span),
            ExpressionKind::Assert { .. } => self.assertion(expression),
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

    fn expression_pair(&mut self, left: &hir::Expression, right: &hir::Expression) {
        self.expression(left);
        self.expression(right);
    }

    fn link_local(&mut self, use_span: Span, local: LocalId) {
        if let Some(target_span) = self.local_targets.get(&local).copied() {
            self.definitions.push(DefinitionLink {
                use_span,
                target_span,
            });
        }
        if let Some(ty) = self.local_types.get(&local).copied() {
            self.type_hints.push(resolved_type_hint(
                use_span,
                ty,
                self.typed,
                TypeHintKind::Binding,
            ));
        }
    }

    fn format_push(&mut self, function: FunctionId, calls: &[hir::Expression], span: Span) {
        if let Some(use_span) = self.syntax_index.call_callees.get(&span_key(span)).copied() {
            self.add_callable_hint(function, use_span);
            if let Some(target_span) = self.function_targets.get(&function).copied() {
                self.definitions.push(DefinitionLink {
                    use_span,
                    target_span,
                });
            }
        }
        for call in calls {
            self.expression_kind(call);
        }
    }

    fn link_static(&mut self, use_span: Span, value: StaticId) {
        if let Some(target_span) = self.static_targets.get(&value).copied() {
            self.definitions.push(DefinitionLink {
                use_span,
                target_span,
            });
        }
        if let Some(value) = self
            .typed
            .statics
            .iter()
            .find(|candidate| candidate.id == value)
        {
            self.type_hints.push(TypeHint {
                span: use_span,
                label: static_signature(value, self.typed),
                documentation: static_documentation(value, self.typed),
                kind: TypeHintKind::Binding,
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

    fn assertion(&mut self, expression: &hir::Expression) {
        let ExpressionKind::Assert {
            mode,
            condition,
            message,
        } = &expression.kind
        else {
            return;
        };
        if let Some(callee) = self
            .syntax_index
            .call_callees
            .get(&span_key(expression.span))
            .copied()
        {
            let (name, documentation) = match mode {
                hir::AssertionMode::Always => (
                    "assert",
                    "Checks `condition` in every build profile and panics with `message` when it is false.",
                ),
                hir::AssertionMode::Debug => (
                    "debug_assert",
                    "Checks `condition` only in debug builds. Optimized builds do not evaluate the condition or message.",
                ),
            };
            self.type_hints.push(TypeHint {
                span: callee,
                label: format!(
                    "fn {name}(condition: bool, message: str = \"assertion failed\") -> ()"
                ),
                documentation: documentation.to_owned(),
                kind: TypeHintKind::Callable,
            });
        }
        self.expression(condition);
        self.expression(message);
    }

    fn place(&mut self, place: &hir::Place) {
        self.type_hints.push(resolved_type_hint(
            place.span,
            place.ty,
            self.typed,
            TypeHintKind::Place,
        ));
        match &place.kind {
            PlaceKind::Local(local) => self.link_local(place.span, *local),
            PlaceKind::Static(value) => self.link_static(place.span, *value),
            PlaceKind::Field { base, .. } | PlaceKind::Index { base, .. } => {
                self.expression(base);
                if let PlaceKind::Index { index, .. } = &place.kind {
                    self.expression(index);
                }
            }
            PlaceKind::Dereference { pointer } => self.expression(pointer),
        }
    }

    fn add_callable_hint(&mut self, function: FunctionId, span: Span) {
        let Some(hint) = self.callable_hints.get(&function) else {
            return;
        };
        self.type_hints.push(TypeHint {
            span,
            label: hint.label.clone(),
            documentation: hint.documentation.clone(),
            kind: TypeHintKind::Callable,
        });
    }

    fn integer_addition(
        &mut self,
        mode: IntegerAdditionMode,
        left: &hir::Expression,
        right: &hir::Expression,
        span: Span,
    ) {
        if let Some(use_span) = self.syntax_index.call_callees.get(&span_key(span)).copied() {
            let integer = type_label(left.ty, self.typed);
            let (name, result, documentation) = match mode {
                IntegerAdditionMode::Wrapping => (
                    "wrapping_add",
                    integer.clone(),
                    format!("Adds two `{integer}` values modulo 2^N. This operation never panics."),
                ),
                IntegerAdditionMode::Checked => (
                    "checked_add",
                    format!("Option<{integer}>"),
                    format!(
                        "Returns `Some(sum)` when two `{integer}` values can be added without overflow; otherwise returns `None`."
                    ),
                ),
                IntegerAdditionMode::Saturating => (
                    "saturating_add",
                    integer.clone(),
                    format!(
                        "Adds two `{integer}` values and clamps overflow to the nearest `{integer}` bound."
                    ),
                ),
            };
            self.type_hints.push(TypeHint {
                span: use_span,
                label: format!("fn {name}(self: {integer}, right: {integer}) -> {result}"),
                documentation,
                kind: TypeHintKind::Callable,
            });
        }
        self.expression(left);
        self.expression(right);
    }

    fn slice_get(
        &mut self,
        slice: &hir::Expression,
        index: &hir::Expression,
        reference_type: Type,
        mutable: bool,
        span: Span,
    ) {
        if let Some(use_span) = self.syntax_index.call_callees.get(&span_key(span)).copied() {
            let name = if mutable { "get_mut" } else { "get" };
            let slice_label = type_label(slice.ty, self.typed);
            let reference_label = type_label(reference_type, self.typed);
            let mutability = if mutable { "mutable " } else { "" };
            self.type_hints.push(TypeHint {
                span: use_span,
                label: format!(
                    "fn {name}(self: {slice_label}, index: usize) -> Option<{reference_label}>"
                ),
                documentation: format!(
                    "Returns `Some({mutability}reference)` when `index` is within the slice; otherwise returns `None`. No bounds panic is raised."
                ),
                kind: TypeHintKind::Callable,
            });
        }
        self.expression(slice);
        self.expression(index);
    }

    fn string_iteration(
        &mut self,
        name: &str,
        source: &hir::Expression,
        result_type: Type,
        span: Span,
    ) {
        if let Some(use_span) = self.syntax_index.call_callees.get(&span_key(span)).copied() {
            let result = type_label(result_type, self.typed);
            let documentation = if name == "bytes" {
                "Returns an immutable borrowed slice containing the exact UTF-8 encoding bytes. No allocation or decoding is performed."
            } else {
                "Creates a forward iterator that decodes the string as Unicode scalar values without allocating."
            };
            self.type_hints.push(TypeHint {
                span: use_span,
                label: format!("fn {name}(self: str) -> {result}"),
                documentation: documentation.to_owned(),
                kind: TypeHintKind::Callable,
            });
        }
        self.expression(source);
    }

    fn chars_next(&mut self, iterator: &hir::Place, result_type: Type, span: Span) {
        if let Some(use_span) = self.syntax_index.call_callees.get(&span_key(span)).copied() {
            self.type_hints.push(TypeHint {
                span: use_span,
                label: format!(
                    "fn next(self: &mut Chars) -> {}",
                    type_label(result_type, self.typed)
                ),
                documentation: "Returns the next decoded Unicode scalar as `Some(char)`, or `None` after the string is exhausted.".to_owned(),
                kind: TypeHintKind::Callable,
            });
        }
        self.place(iterator);
    }

    fn direct_call(&mut self, function: FunctionId, arguments: &[hir::Expression], span: Span) {
        if let Some(use_span) = self.syntax_index.call_callees.get(&span_key(span)).copied() {
            self.add_callable_hint(function, use_span);
            if let Some(target_span) = self.function_targets.get(&function).copied() {
                self.definitions.push(DefinitionLink {
                    use_span,
                    target_span,
                });
            }
        }
        for argument in arguments {
            self.expression(argument);
        }
    }

    fn function_value(&mut self, function: FunctionId, span: Span) {
        self.add_callable_hint(function, span);
        if let Some(target_span) = self.function_targets.get(&function).copied() {
            self.definitions.push(DefinitionLink {
                use_span: span,
                target_span,
            });
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
        reimer_package::display_symbol_name(&function.name),
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
        reimer_package::display_symbol_name(&function.name),
        type_label(function.return_type, program)
    )
}

fn static_signature(value: &hir::Static, program: &hir::Program) -> String {
    let visibility = if value.is_public { "pub " } else { "" };
    let mutability = if value.mutable { "mut " } else { "" };
    format!(
        "{visibility}static {mutability}{}: {}",
        reimer_package::display_symbol_name(&value.name),
        type_label(value.ty, program)
    )
}

fn static_documentation(value: &hir::Static, program: &hir::Program) -> String {
    value.documentation.clone().unwrap_or_else(|| {
        let label = type_label(value.ty, program);
        if value.mutable {
            format!(
                "Stable-address mutable storage containing `{label}`. Every access requires `unsafe`; prefer atomics, locks, or an encapsulated synchronization API for concurrent state."
            )
        } else {
            format!("Stable-address immutable storage containing `{label}`.")
        }
    })
}

fn resolved_type_hint(
    span: Span,
    ty: Type,
    program: &hir::Program,
    kind: TypeHintKind,
) -> TypeHint {
    TypeHint {
        span,
        label: type_label(ty, program),
        documentation: type_documentation(ty, program),
        kind,
    }
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
        TypeDefinitionKind::Struct { .. } => definition.name.as_ref().map_or_else(
            || ty.to_string(),
            |name| reimer_package::display_symbol_name(name),
        ),
        TypeDefinitionKind::Enum { variants } => definition.name.as_ref().map_or_else(
            || ty.to_string(),
            |name| {
                intrinsic_enum_label(name, variants, program, depth)
                    .unwrap_or_else(|| reimer_package::display_symbol_name(name))
            },
        ),
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

fn intrinsic_enum_label(
    name: &str,
    variants: &[hir::EnumVariant],
    program: &hir::Program,
    depth: usize,
) -> Option<String> {
    match (name, variants) {
        (
            "Option",
            [
                hir::EnumVariant {
                    fields: hir::EnumVariantFields::Tuple(value),
                    ..
                },
                hir::EnumVariant {
                    fields: hir::EnumVariantFields::Unit,
                    ..
                },
            ],
        ) if value.len() == 1 => Some(format!(
            "Option<{}>",
            type_label_at_depth(value[0], program, depth + 1)
        )),
        (
            "Result",
            [
                hir::EnumVariant {
                    fields: hir::EnumVariantFields::Tuple(success),
                    ..
                },
                hir::EnumVariant {
                    fields: hir::EnumVariantFields::Tuple(error),
                    ..
                },
            ],
        ) if success.len() == 1 && error.len() == 1 => Some(format!(
            "Result<{}, {}>",
            type_label_at_depth(success[0], program, depth + 1),
            type_label_at_depth(error[0], program, depth + 1)
        )),
        _ => None,
    }
}

fn type_documentation(ty: Type, program: &hir::Program) -> String {
    if let Some(documentation) = primitive_documentation(ty) {
        return documentation.to_owned();
    }

    let label = type_label(ty, program);
    let Some(definition) = definition(ty, program) else {
        return format!("Value of type `{label}`.");
    };
    if let Some(documentation) = &definition.documentation {
        return documentation.clone();
    }
    match &definition.kind {
        TypeDefinitionKind::Struct { fields } => named_struct_documentation(&label, fields.len()),
        TypeDefinitionKind::Enum { variants } => {
            let names = variants
                .iter()
                .take(8)
                .map(|variant| format!("`{}`", variant.name))
                .collect::<Vec<_>>()
                .join(", ");
            format!("Enumeration with variants: {names}.")
        }
        TypeDefinitionKind::Tuple { elements } => {
            format!("Tuple containing {} ordered values.", elements.len())
        }
        TypeDefinitionKind::Array { element, length } => format!(
            "Fixed-size array containing {length} values of type `{}`.",
            type_label_at_depth(*element, program, 1)
        ),
        TypeDefinitionKind::Reference { target, mutable } => {
            let access = if *mutable { "Mutable" } else { "Immutable" };
            format!(
                "{access} borrowed reference to `{}`. The reference does not own the value.",
                type_label_at_depth(*target, program, 1)
            )
        }
        TypeDefinitionKind::RawPointer { target, mutable } => {
            let access = if *mutable { "mutable" } else { "const" };
            format!(
                "Unsafe raw {access} pointer to `{}`. Dereferencing requires an `unsafe` block.",
                type_label_at_depth(*target, program, 1)
            )
        }
        TypeDefinitionKind::Slice { element, mutable } => {
            let access = if *mutable { "Mutable" } else { "Immutable" };
            format!(
                "{access} borrowed slice of `{}` values. Its length is known at runtime and indexing is bounds-checked.",
                type_label_at_depth(*element, program, 1)
            )
        }
        TypeDefinitionKind::Function {
            parameters,
            return_type,
        } => format!(
            "Function value accepting {} argument(s) and returning `{}`.",
            parameters.len(),
            type_label_at_depth(*return_type, program, 1)
        ),
    }
}

const fn primitive_documentation(ty: Type) -> Option<&'static str> {
    match ty {
        Type::I8 => Some("8-bit signed integer. Range: `-128` to `127`."),
        Type::I16 => Some("16-bit signed integer. Range: `-32,768` to `32,767`."),
        Type::I32 => Some("32-bit signed integer. Range: `-2,147,483,648` to `2,147,483,647`."),
        Type::I64 => Some(
            "64-bit signed integer. Range: `-9,223,372,036,854,775,808` to `9,223,372,036,854,775,807`.",
        ),
        Type::I128 => Some(
            "128-bit signed integer. Range: `-170,141,183,460,469,231,731,687,303,715,884,105,728` to `170,141,183,460,469,231,731,687,303,715,884,105,727`.",
        ),
        Type::Isize => {
            Some("Pointer-sized signed integer. Its range follows the active compilation target.")
        }
        Type::U8 => Some("8-bit unsigned integer. Range: `0` to `255`."),
        Type::U16 => Some("16-bit unsigned integer. Range: `0` to `65,535`."),
        Type::U32 => Some("32-bit unsigned integer. Range: `0` to `4,294,967,295`."),
        Type::U64 => Some("64-bit unsigned integer. Range: `0` to `18,446,744,073,709,551,615`."),
        Type::U128 => Some(
            "128-bit unsigned integer. Range: `0` to `340,282,366,920,938,463,463,374,607,431,768,211,455`.",
        ),
        Type::Usize => Some(
            "Pointer-sized unsigned integer used for sizes and indices. Its range follows the active compilation target.",
        ),
        Type::F32 => Some(
            "32-bit IEEE 754 floating-point number with approximately 6 to 9 decimal digits of precision.",
        ),
        Type::F64 => Some(
            "64-bit IEEE 754 floating-point number with approximately 15 to 17 decimal digits of precision.",
        ),
        Type::Bool => Some("Boolean value: `true` or `false`."),
        Type::Char => Some("Unicode scalar value stored as a single character."),
        Type::Str => Some(
            "Immutable, non-owning UTF-8 string view represented by a pointer and a byte length.",
        ),
        Type::CStr => Some("Borrowed pointer to a NUL-terminated C byte string."),
        Type::Unit => Some("Unit type with one value, `()`, used when no result is returned."),
        Type::Never => Some("Never type for expressions that do not return to their caller."),
        Type::Struct(_)
        | Type::Enum(_)
        | Type::Tuple(_)
        | Type::Array(_)
        | Type::Reference(_)
        | Type::RawPointer(_)
        | Type::Slice(_)
        | Type::Function(_) => None,
    }
}

fn named_struct_documentation(label: &str, field_count: usize) -> String {
    let base = label.split('<').next().unwrap_or(label);
    match base {
        "std::tensor::TensorViewMut" => {
            "Mutable, non-owning tensor view. Writes update the underlying tensor storage."
                .to_owned()
        }
        "std::tensor::TensorView" => "Immutable, non-owning view over tensor storage.".to_owned(),
        "std::tensor::tensor" => {
            "Owned, row-major tensor with a fixed rank and allocator-backed storage.".to_owned()
        }
        "std::collections::Vec" => {
            "Growable contiguous collection backed by an explicit allocator.".to_owned()
        }
        "std::collections::HashMap" => {
            "Flat, allocator-backed hash table with grouped control metadata. Lookup, insertion, and removal are expected O(1).".to_owned()
        }
        "std::collections::HashSet" => {
            "Flat, allocator-backed collection of unique values with expected O(1) membership tests.".to_owned()
        }
        "std::collections::RingBuffer" => {
            "Fixed-capacity first-in, first-out circular buffer.".to_owned()
        }
        "Chars" => {
            "Forward, allocation-free iterator over the Unicode scalar values in a UTF-8 string."
                .to_owned()
        }
        "std::string::String" => {
            "Owned, growable UTF-8 string backed by an explicit allocator.".to_owned()
        }
        "std::alloc::Allocator" => {
            "Handle to an allocation strategy used by allocator-aware APIs.".to_owned()
        }
        _ => format!("Struct value containing {field_count} field(s)."),
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
