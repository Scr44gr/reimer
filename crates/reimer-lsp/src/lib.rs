//! Document model and protocol-independent editor operations for the language
//! server binary.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use reimer_ast::Item;
use reimer_lint::{
    AllocationQuantity, AllocatorSummary, Analysis, Finding, Fix, Severity, analyze,
    apply_spelling_fixes, index_typed, lint_typed, organize_imports,
};
use reimer_project::{LockMode, Project, ProjectError};
use tower_lsp::lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeLens, Command, CompletionItem,
    CompletionItemKind, Diagnostic, DiagnosticSeverity, DocumentSymbol, Documentation, Hover,
    HoverContents, InlayHint, InlayHintKind, InlayHintLabel, Location, MarkupContent, MarkupKind,
    NumberOrString, Position, Range, SymbolKind, TextEdit, Url, WorkspaceEdit,
};

/// One immutable source snapshot and all indexes derived from it.
#[derive(Debug)]
pub struct Document {
    uri: Url,
    text: Arc<str>,
    lines: LineIndex,
    analysis: Analysis,
}

impl Document {
    /// Analyzes a newly opened or changed document.
    #[must_use]
    pub fn new(uri: Url, text: String) -> Self {
        let text: Arc<str> = text.into();
        let lines = LineIndex::new(Arc::clone(&text));
        let mut analysis = analyze(&text);
        if syntax_has_imports(&analysis) {
            analysis
                .findings
                .retain(|finding| finding.severity != Severity::Error);
        }
        let mut document = Self {
            uri,
            text,
            lines,
            analysis,
        };
        document.refresh_package();
        document
    }

    /// Returns the source snapshot.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns diagnostics in Language Server Protocol coordinates.
    #[must_use]
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        self.analysis
            .findings
            .iter()
            .map(|finding| Diagnostic {
                range: self.lines.range(finding.span),
                severity: Some(match finding.severity {
                    Severity::Error => DiagnosticSeverity::ERROR,
                    Severity::Warning => DiagnosticSeverity::WARNING,
                    Severity::Information => DiagnosticSeverity::INFORMATION,
                    Severity::Hint => DiagnosticSeverity::HINT,
                }),
                code: Some(NumberOrString::String(finding.code.clone())),
                code_description: None,
                source: Some(
                    if finding.code.starts_with('E') {
                        "Reimer compiler"
                    } else {
                        "Reimer linter"
                    }
                    .to_owned(),
                ),
                message: finding.help.as_ref().map_or_else(
                    || finding.message.clone(),
                    |help| format!("{}\n\nHelp: {help}", finding.message),
                ),
                related_information: None,
                tags: None,
                data: None,
            })
            .collect()
    }

    /// Finds inferred type and allocation information at a cursor position.
    #[must_use]
    pub fn hover(&self, position: Position) -> Option<Hover> {
        let byte = self.lines.byte(position)?;
        let type_hint = narrowest_containing(&self.analysis.type_hints, byte, |hint| hint.span);
        let allocation =
            narrowest_containing(&self.analysis.allocations, byte, |estimate| estimate.span);
        if type_hint.is_none() && allocation.is_none() {
            return None;
        }

        let mut markdown = String::new();
        let mut span = None;
        if let Some(hint) = type_hint {
            markdown.push_str("```reimer\n");
            markdown.push_str(&hint.label);
            markdown.push_str("\n```\n\n");
            markdown.push_str(&hint.detail);
            span = Some(hint.span);
        }
        if let Some(estimate) = allocation {
            if !markdown.is_empty() {
                markdown.push_str("\n\n---\n\n");
            }
            markdown.push_str("**Static allocator estimate:** ");
            markdown.push_str(&quantity_label(estimate.quantity));
            markdown.push_str("\n\n");
            markdown.push_str("Allocator: ");
            markdown.push_str(&estimate.allocator);
            markdown.push_str("\n\n");
            markdown.push_str(&estimate.explanation);
            span = Some(estimate.span);
        }
        Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: markdown,
            }),
            range: span.map(|span| self.lines.range(span)),
        })
    }

    /// Resolves a local or top-level use to its declaration.
    #[must_use]
    pub fn definition(&self, position: Position) -> Option<Location> {
        let byte = self.lines.byte(position)?;
        let link = narrowest_containing(&self.analysis.definitions, byte, |link| link.use_span)?;
        Some(Location {
            uri: self.uri.clone(),
            range: self.lines.range(link.target_span),
        })
    }

    /// Returns hierarchical symbols for the outline and breadcrumb views.
    #[must_use]
    pub fn document_symbols(&self) -> Vec<DocumentSymbol> {
        self.analysis
            .syntax
            .as_ref()
            .map_or_else(Vec::new, |syntax| {
                syntax
                    .items
                    .iter()
                    .filter_map(|item| self.item_symbol(item))
                    .collect()
            })
    }

    /// Returns context-free language and document completions.
    #[must_use]
    pub fn completions(&self) -> Vec<CompletionItem> {
        let mut items = language_completions();
        let mut seen: HashSet<String> = items.iter().map(|item| item.label.clone()).collect();
        if let Some(syntax) = &self.analysis.syntax {
            for item in &syntax.items {
                let (name, kind, detail) = match item {
                    Item::Function(function) => (
                        &function.name.name,
                        CompletionItemKind::FUNCTION,
                        "function",
                    ),
                    Item::ExternFunction(function) => (
                        &function.name.name,
                        CompletionItemKind::FUNCTION,
                        "native function",
                    ),
                    Item::Struct(declaration) => {
                        (&declaration.name.name, CompletionItemKind::STRUCT, "struct")
                    }
                    Item::Enum(declaration) => {
                        (&declaration.name.name, CompletionItemKind::ENUM, "enum")
                    }
                    Item::Trait(declaration) => (
                        &declaration.name.name,
                        CompletionItemKind::INTERFACE,
                        "trait",
                    ),
                    Item::Constant(declaration) => (
                        &declaration.name.name,
                        CompletionItemKind::CONSTANT,
                        "compile-time constant",
                    ),
                    Item::Import(_) | Item::Impl(_) | Item::Comptime(_) => continue,
                };
                if seen.insert(name.clone()) {
                    items.push(simple_completion(name, kind, detail));
                }
            }
        }
        if let Ok(tokens) = reimer_lexer::lex(&self.text) {
            for token in tokens {
                if let reimer_lexer::TokenKind::Identifier(identifier) = token.kind
                    && seen.insert(identifier.clone())
                {
                    items.push(simple_completion(
                        &identifier,
                        CompletionItemKind::VARIABLE,
                        "identifier in this document",
                    ));
                }
            }
        }
        items.sort_by(|left, right| left.label.cmp(&right.label));
        items
    }

    /// Returns quick fixes and the canonical organize-imports action.
    #[must_use]
    pub fn code_actions(&self, requested_range: Range) -> Vec<CodeActionOrCommand> {
        let mut actions = Vec::new();
        if let Some(syntax) = &self.analysis.syntax
            && let Some(fix) = organize_imports(&self.text, syntax)
        {
            actions.push(CodeActionOrCommand::CodeAction(self.action_from_fix(
                &fix,
                CodeActionKind::SOURCE_ORGANIZE_IMPORTS,
                false,
                None,
            )));
        }
        for finding in &self.analysis.findings {
            if !ranges_overlap(self.lines.range(finding.span), requested_range) {
                continue;
            }
            for fix in &finding.fixes {
                actions.push(CodeActionOrCommand::CodeAction(self.action_from_fix(
                    fix,
                    CodeActionKind::QUICKFIX,
                    true,
                    Some(finding),
                )));
            }
        }
        actions
    }

    /// Returns inferred local types and allocation estimates as inlay hints.
    #[must_use]
    pub fn inlay_hints(&self, requested_range: Range) -> Vec<InlayHint> {
        let mut hints = Vec::new();
        for hint in &self.analysis.type_hints {
            if hint.detail != "inferred local binding type"
                || !ranges_overlap(self.lines.range(hint.span), requested_range)
                || binding_has_annotation(&self.text, hint.span)
            {
                continue;
            }
            hints.push(InlayHint {
                position: self.lines.position(hint.span.end),
                label: InlayHintLabel::String(format!(": {}", hint.label)),
                kind: Some(InlayHintKind::TYPE),
                text_edits: None,
                tooltip: Some(tower_lsp::lsp_types::InlayHintTooltip::MarkupContent(
                    MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: "Type inferred by the compiler resolver.".to_owned(),
                    },
                )),
                padding_left: None,
                padding_right: Some(true),
                data: None,
            });
        }
        for estimate in &self.analysis.allocations {
            if !ranges_overlap(self.lines.range(estimate.span), requested_range) {
                continue;
            }
            hints.push(InlayHint {
                position: self.lines.position(estimate.span.end),
                label: InlayHintLabel::String(format!(
                    " · {} (static estimate)",
                    quantity_label(estimate.quantity)
                )),
                kind: Some(InlayHintKind::TYPE),
                text_edits: None,
                tooltip: Some(tower_lsp::lsp_types::InlayHintTooltip::MarkupContent(
                    MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: format!(
                            "**Allocator:** {}\n\n{}",
                            estimate.allocator, estimate.explanation
                        ),
                    },
                )),
                padding_left: Some(true),
                padding_right: None,
                data: None,
            });
        }
        hints
    }

    /// Returns one static allocator summary lens per function.
    #[must_use]
    pub fn code_lenses(&self) -> Vec<CodeLens> {
        let mut grouped: HashMap<(usize, usize), Vec<&AllocatorSummary>> = HashMap::new();
        for summary in &self.analysis.allocator_summaries {
            grouped
                .entry((summary.span.start, summary.span.end))
                .or_default()
                .push(summary);
        }
        let mut lenses = Vec::new();
        for (span, summaries) in grouped {
            let known: u128 = summaries
                .iter()
                .map(|summary| summary.known_bytes_per_call)
                .sum();
            let per_iteration: u128 = summaries
                .iter()
                .map(|summary| summary.known_bytes_per_iteration)
                .sum();
            let dynamic: usize = summaries
                .iter()
                .map(|summary| summary.dynamic_operations)
                .sum();
            let title = summary_title(known, per_iteration, dynamic);
            let details = summaries
                .iter()
                .map(|summary| {
                    format!(
                        "{}: {} B/call, {} B/iteration, {} dynamic operation(s)",
                        summary.allocator,
                        summary.known_bytes_per_call,
                        summary.known_bytes_per_iteration,
                        summary.dynamic_operations
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            lenses.push(CodeLens {
                range: self
                    .lines
                    .range(reimer_diagnostics::Span::new(span.0, span.1)),
                command: Some(Command {
                    title,
                    command: "reimer.showAllocatorEstimate".to_owned(),
                    arguments: Some(vec![serde_json::Value::String(details)]),
                }),
                data: None,
            });
        }
        lenses.sort_by_key(|lens| (lens.range.start.line, lens.range.start.character));
        lenses
    }

    fn refresh_package(&mut self) {
        let Ok(path) = self.uri.to_file_path() else {
            return;
        };
        let package = match Project::open(&path, LockMode::Use) {
            Ok(project) => reimer_package::load_graph_with_overlay(
                &project.source_graph(&path),
                &path,
                &self.text,
            ),
            Err(ProjectError::ManifestNotFound { .. }) => {
                reimer_package::load_with_overlay(&path, &self.text)
            }
            Err(error) => {
                self.analysis
                    .findings
                    .retain(|finding| finding.severity != Severity::Error);
                self.analysis.findings.push(Finding {
                    code: "E4011".to_owned(),
                    severity: Severity::Error,
                    message: error.to_string(),
                    span: reimer_diagnostics::Span::empty(0),
                    help: Some("fix the nearest reimer.toml or regenerate reimer.lock".to_owned()),
                    fixes: Vec::new(),
                });
                return;
            }
        };
        let package = match package {
            Ok(package) => package,
            Err(diagnostics) => {
                self.analysis
                    .findings
                    .retain(|finding| finding.severity != Severity::Error);
                self.analysis.findings.extend(
                    diagnostics
                        .into_iter()
                        .filter(|diagnostic| diagnostic.path == path)
                        .map(|diagnostic| compiler_finding(diagnostic.diagnostic)),
                );
                return;
            }
        };
        let resolved = if syntax_has_main(&self.analysis) {
            reimer_resolver::resolve(&package.program)
        } else {
            reimer_resolver::resolve_library(&package.program)
        };
        match resolved {
            Ok(typed) => {
                self.analysis
                    .findings
                    .retain(|finding| finding.severity != Severity::Error);
                self.analysis.findings.extend(lint_typed(&typed));
                if let Some(syntax) = &self.analysis.syntax {
                    let (type_hints, definitions) = index_typed(syntax, &typed);
                    self.analysis.type_hints = type_hints;
                    self.analysis.definitions = definitions;
                }
                self.analysis.typed = Some(typed);
            }
            Err(diagnostics) => {
                self.analysis
                    .findings
                    .retain(|finding| finding.severity != Severity::Error);
                self.analysis.findings.extend(
                    package
                        .map_diagnostics(diagnostics)
                        .into_iter()
                        .filter(|diagnostic| diagnostic.path == path)
                        .map(|diagnostic| compiler_finding(diagnostic.diagnostic)),
                );
                if let Some(syntax) = &self.analysis.syntax {
                    apply_spelling_fixes(&self.text, syntax, &mut self.analysis.findings);
                }
            }
        }
    }

    fn action_from_fix(
        &self,
        fix: &Fix,
        kind: CodeActionKind,
        preferred: bool,
        finding: Option<&Finding>,
    ) -> CodeAction {
        let edit = TextEdit {
            range: self.lines.range(fix.span),
            new_text: fix.replacement.clone(),
        };
        let mut changes = HashMap::new();
        changes.insert(self.uri.clone(), vec![edit]);
        CodeAction {
            title: fix.title.clone(),
            kind: Some(kind),
            diagnostics: finding.map(|finding| {
                vec![Diagnostic {
                    range: self.lines.range(finding.span),
                    severity: None,
                    code: Some(NumberOrString::String(finding.code.clone())),
                    code_description: None,
                    source: Some("Reimer linter".to_owned()),
                    message: finding.message.clone(),
                    related_information: None,
                    tags: None,
                    data: None,
                }]
            }),
            edit: Some(WorkspaceEdit {
                changes: Some(changes),
                document_changes: None,
                change_annotations: None,
            }),
            command: None,
            is_preferred: Some(preferred),
            disabled: None,
            data: None,
        }
    }

    fn item_symbol(&self, item: &Item) -> Option<DocumentSymbol> {
        let (name, detail, kind, span, selection_span, children) = match item {
            Item::Import(_) => return None,
            Item::Function(function) => (
                function.name.name.clone(),
                Some("function".to_owned()),
                SymbolKind::FUNCTION,
                function.span,
                function.name.span,
                None,
            ),
            Item::ExternFunction(function) => (
                function.name.name.clone(),
                Some(format!("extern \"{}\" function", function.abi)),
                SymbolKind::FUNCTION,
                function.span,
                function.name.span,
                None,
            ),
            Item::Struct(declaration) => (
                declaration.name.name.clone(),
                Some("struct".to_owned()),
                SymbolKind::STRUCT,
                declaration.span,
                declaration.name.span,
                Some(self.field_symbols(&declaration.fields)),
            ),
            Item::Enum(declaration) => (
                declaration.name.name.clone(),
                Some("enum".to_owned()),
                SymbolKind::ENUM,
                declaration.span,
                declaration.name.span,
                Some(self.variant_symbols(&declaration.variants)),
            ),
            Item::Trait(declaration) => (
                declaration.name.name.clone(),
                Some("trait".to_owned()),
                SymbolKind::INTERFACE,
                declaration.span,
                declaration.name.span,
                Some(self.trait_method_symbols(&declaration.methods)),
            ),
            Item::Impl(declaration) => (
                "impl".to_owned(),
                Some("implementation".to_owned()),
                SymbolKind::NAMESPACE,
                declaration.span,
                declaration.target.span,
                Some(self.method_symbols(&declaration.methods)),
            ),
            Item::Constant(declaration) => (
                declaration.name.name.clone(),
                Some("compile-time constant".to_owned()),
                SymbolKind::CONSTANT,
                declaration.span,
                declaration.name.span,
                None,
            ),
            Item::Comptime(block) => (
                "comptime".to_owned(),
                Some("compile-time assertion block".to_owned()),
                SymbolKind::NAMESPACE,
                block.span,
                block.span,
                None,
            ),
        };
        Some(self.symbol(name, detail, kind, span, selection_span, children))
    }

    fn field_symbols(&self, fields: &[reimer_ast::StructField]) -> Vec<DocumentSymbol> {
        fields
            .iter()
            .map(|field| {
                self.symbol(
                    field.name.name.clone(),
                    Some("field".to_owned()),
                    SymbolKind::FIELD,
                    field.span,
                    field.name.span,
                    None,
                )
            })
            .collect()
    }

    fn variant_symbols(&self, variants: &[reimer_ast::EnumVariant]) -> Vec<DocumentSymbol> {
        variants
            .iter()
            .map(|variant| {
                self.symbol(
                    variant.name.name.clone(),
                    Some("variant".to_owned()),
                    SymbolKind::ENUM_MEMBER,
                    variant.span,
                    variant.name.span,
                    None,
                )
            })
            .collect()
    }

    fn trait_method_symbols(&self, methods: &[reimer_ast::TraitMethod]) -> Vec<DocumentSymbol> {
        methods
            .iter()
            .map(|method| {
                self.symbol(
                    method.name.name.clone(),
                    Some("required method".to_owned()),
                    SymbolKind::METHOD,
                    method.span,
                    method.name.span,
                    None,
                )
            })
            .collect()
    }

    fn method_symbols(&self, methods: &[reimer_ast::Function]) -> Vec<DocumentSymbol> {
        methods
            .iter()
            .map(|method| {
                self.symbol(
                    method.name.name.clone(),
                    Some("method".to_owned()),
                    SymbolKind::METHOD,
                    method.span,
                    method.name.span,
                    None,
                )
            })
            .collect()
    }

    fn symbol(
        &self,
        name: String,
        detail: Option<String>,
        kind: SymbolKind,
        span: reimer_diagnostics::Span,
        selection_span: reimer_diagnostics::Span,
        children: Option<Vec<DocumentSymbol>>,
    ) -> DocumentSymbol {
        #[expect(
            deprecated,
            reason = "LSP retains this field for protocol compatibility"
        )]
        DocumentSymbol {
            name,
            detail,
            kind,
            tags: None,
            deprecated: None,
            range: self.lines.range(span),
            selection_range: self.lines.range(selection_span),
            children,
        }
    }
}

fn syntax_has_imports(analysis: &Analysis) -> bool {
    analysis.syntax.as_ref().is_some_and(|syntax| {
        syntax
            .items
            .iter()
            .any(|item| matches!(item, Item::Import(_)))
    })
}

fn syntax_has_main(analysis: &Analysis) -> bool {
    analysis.syntax.as_ref().is_some_and(|syntax| {
        syntax
            .items
            .iter()
            .any(|item| matches!(item, Item::Function(function) if function.name.name == "main"))
    })
}

fn compiler_finding(diagnostic: reimer_diagnostics::Diagnostic) -> Finding {
    Finding {
        code: diagnostic.code.to_owned(),
        severity: Severity::Error,
        message: diagnostic.message,
        span: diagnostic.span,
        help: diagnostic.help,
        fixes: Vec::new(),
    }
}

fn narrowest_containing<T>(
    values: &[T],
    byte: usize,
    span: impl Fn(&T) -> reimer_diagnostics::Span,
) -> Option<&T> {
    values
        .iter()
        .filter(|value| {
            let span = span(value);
            byte >= span.start && byte <= span.end
        })
        .min_by_key(|value| {
            let span = span(value);
            span.end.saturating_sub(span.start)
        })
}

fn quantity_label(quantity: AllocationQuantity) -> String {
    match quantity {
        AllocationQuantity::Exact(bytes) => format!("{bytes} B reserved"),
        AllocationQuantity::AtMost(bytes) => format!("up to {bytes} B"),
        AllocationQuantity::PerIteration(bytes) => format!("{bytes} B/iteration"),
        AllocationQuantity::Dynamic => "runtime-sized reservation".to_owned(),
    }
}

fn summary_title(known: u128, per_iteration: u128, dynamic: usize) -> String {
    let mut parts = vec![format!("{known} B/call")];
    if per_iteration != 0 {
        parts.push(format!("{per_iteration} B/iteration"));
    }
    if dynamic != 0 {
        parts.push(format!("{dynamic} dynamic"));
    }
    format!("Static allocator estimate: {}", parts.join(" + "))
}

fn ranges_overlap(left: Range, right: Range) -> bool {
    left.start <= right.end && right.start <= left.end
}

fn binding_has_annotation(source: &str, name_span: reimer_diagnostics::Span) -> bool {
    source
        .get(name_span.end..)
        .and_then(|suffix| suffix.find(['=', ';']).map(|end| &suffix[..end]))
        .is_some_and(|between| between.contains(':'))
}

fn simple_completion(label: &str, kind: CompletionItemKind, detail: &str) -> CompletionItem {
    CompletionItem {
        label: label.to_owned(),
        kind: Some(kind),
        detail: Some(detail.to_owned()),
        ..CompletionItem::default()
    }
}

const LANGUAGE_KEYWORDS: &[&str] = &[
    "as", "break", "comptime", "const", "continue", "defer", "else", "enum", "extern", "false",
    "fn", "for", "from", "if", "impl", "import", "in", "let", "loop", "match", "mut", "pub",
    "return", "struct", "trait", "true", "unsafe", "where",
];

const PRIMITIVE_TYPES: &[&str] = &[
    "bool", "char", "cstr", "f32", "f64", "i8", "i16", "i32", "i64", "i128", "isize", "str", "u8",
    "u16", "u32", "u64", "u128", "usize",
];

const STANDARD_SYMBOLS: &[(&str, CompletionItemKind, &str)] = &[
    ("print", CompletionItemKind::FUNCTION, "std::io"),
    ("println", CompletionItemKind::FUNCTION, "std::io"),
    ("eprint", CompletionItemKind::FUNCTION, "std::io"),
    ("eprintln", CompletionItemKind::FUNCTION, "std::io"),
    ("stdin", CompletionItemKind::FUNCTION, "std::io"),
    ("stdout", CompletionItemKind::FUNCTION, "std::io"),
    ("stderr", CompletionItemKind::FUNCTION, "std::io"),
    ("read", CompletionItemKind::METHOD, "Stdin"),
    ("read_exact", CompletionItemKind::METHOD, "Stdin"),
    ("read_line", CompletionItemKind::METHOD, "Stdin"),
    ("read_to_end", CompletionItemKind::METHOD, "Stdin"),
    ("read_line_string", CompletionItemKind::METHOD, "Stdin"),
    ("read_to_string", CompletionItemKind::METHOD, "Stdin"),
    (
        "size_of",
        CompletionItemKind::FUNCTION,
        "compile-time metadata",
    ),
    (
        "align_of",
        CompletionItemKind::FUNCTION,
        "compile-time metadata",
    ),
    ("name", CompletionItemKind::FUNCTION, "meta"),
    ("fields", CompletionItemKind::FUNCTION, "meta"),
    ("variants", CompletionItemKind::FUNCTION, "meta"),
    ("traits", CompletionItemKind::FUNCTION, "meta"),
    (
        "general_allocator",
        CompletionItemKind::FUNCTION,
        "std::alloc",
    ),
    ("page_allocator", CompletionItemKind::FUNCTION, "std::alloc"),
    ("allocate_bytes", CompletionItemKind::FUNCTION, "std::alloc"),
    ("Option", CompletionItemKind::ENUM, "core"),
    ("Result", CompletionItemKind::ENUM, "core"),
    ("String", CompletionItemKind::STRUCT, "std::string"),
    ("from", CompletionItemKind::METHOD, "String::from"),
    ("as_str", CompletionItemKind::METHOD, "String::as_str"),
    ("clone_in", CompletionItemKind::METHOD, "String::clone_in"),
    ("Thread", CompletionItemKind::STRUCT, "std::thread"),
    ("scope", CompletionItemKind::FUNCTION, "std::thread"),
    ("Mutex", CompletionItemKind::STRUCT, "std::thread"),
    ("RwLock", CompletionItemKind::STRUCT, "std::thread"),
    ("Channel", CompletionItemKind::STRUCT, "std::thread"),
    ("Barrier", CompletionItemKind::STRUCT, "std::thread"),
    ("Semaphore", CompletionItemKind::STRUCT, "std::thread"),
    ("AtomicU64", CompletionItemKind::STRUCT, "std::thread"),
    ("AtomicI64", CompletionItemKind::STRUCT, "std::thread"),
    ("AtomicUsize", CompletionItemKind::STRUCT, "std::thread"),
    ("AtomicIsize", CompletionItemKind::STRUCT, "std::thread"),
    ("AtomicBool", CompletionItemKind::STRUCT, "std::thread"),
    ("ThreadLocal", CompletionItemKind::STRUCT, "std::thread"),
    ("JobPool", CompletionItemKind::STRUCT, "std::job"),
    ("JobPoolConfig", CompletionItemKind::STRUCT, "std::job"),
    ("Job", CompletionItemKind::STRUCT, "std::job"),
    ("chunk_len", CompletionItemKind::FUNCTION, "std::job"),
    ("parallel_for_mut", CompletionItemKind::FUNCTION, "std::job"),
    (
        "parallel_for_array_mut",
        CompletionItemKind::FUNCTION,
        "std::job",
    ),
    (
        "deinit",
        CompletionItemKind::METHOD,
        "owned resource cleanup",
    ),
];

fn language_completions() -> Vec<CompletionItem> {
    let mut items = LANGUAGE_KEYWORDS
        .iter()
        .copied()
        .map(|keyword| simple_completion(keyword, CompletionItemKind::KEYWORD, "keyword"))
        .chain(PRIMITIVE_TYPES.iter().copied().map(|primitive| {
            simple_completion(
                primitive,
                CompletionItemKind::TYPE_PARAMETER,
                "primitive type",
            )
        }))
        .chain(
            STANDARD_SYMBOLS
                .iter()
                .copied()
                .map(|(name, kind, module)| simple_completion(name, kind, module)),
        )
        .collect::<Vec<_>>();
    for (label, insertion) in [
        ("@derive", "@derive(${1:Copy, Eq})"),
        ("@repr", "@repr(${1:C})"),
        ("@align", "@align(${1:16})"),
        ("@inline", "@inline"),
        ("@test", "@test"),
        ("@must_use", "@must_use"),
    ] {
        items.push(CompletionItem {
            label: label.to_owned(),
            kind: Some(CompletionItemKind::SNIPPET),
            detail: Some("built-in attribute".to_owned()),
            insert_text: Some(insertion.to_owned()),
            insert_text_format: Some(tower_lsp::lsp_types::InsertTextFormat::SNIPPET),
            ..CompletionItem::default()
        });
    }
    items.push(CompletionItem {
        label: "fn".to_owned(),
        kind: Some(CompletionItemKind::SNIPPET),
        detail: Some("function declaration".to_owned()),
        insert_text: Some("fn ${1:name}(${2}) -> ${3:()} {\n    ${0}\n}".to_owned()),
        insert_text_format: Some(tower_lsp::lsp_types::InsertTextFormat::SNIPPET),
        text_edit: None,
        additional_text_edits: None,
        command: None,
        data: None,
        documentation: Some(Documentation::String(
            "Declare a function with an explicit return type.".to_owned(),
        )),
        filter_text: Some("function".to_owned()),
        sort_text: Some("0_fn_snippet".to_owned()),
        preselect: None,
        insert_text_mode: None,
        label_details: None,
        tags: None,
        deprecated: None,
        commit_characters: None,
    });
    items
}

/// Maps UTF-8 byte offsets to LSP UTF-16 positions.
#[derive(Debug)]
struct LineIndex {
    source: Arc<str>,
    starts: Vec<usize>,
}

impl LineIndex {
    fn new(source: Arc<str>) -> Self {
        let mut starts = vec![0];
        starts.extend(
            source
                .bytes()
                .enumerate()
                .filter_map(|(index, byte)| (byte == b'\n').then_some(index + 1)),
        );
        Self { source, starts }
    }

    fn byte(&self, position: Position) -> Option<usize> {
        let line = usize::try_from(position.line).ok()?;
        let start = *self.starts.get(line)?;
        let end = self
            .starts
            .get(line + 1)
            .copied()
            .unwrap_or(self.source.len());
        let line_text = self.source.get(start..end)?;
        let target = usize::try_from(position.character).ok()?;
        let mut utf16 = 0;
        for (offset, character) in line_text.char_indices() {
            if utf16 >= target {
                return Some(start + offset);
            }
            utf16 += character.len_utf16();
            if utf16 > target {
                return Some(start + offset + character.len_utf8());
            }
        }
        Some(end)
    }

    fn position(&self, byte: usize) -> Position {
        let byte = byte.min(self.source.len());
        let line = self
            .starts
            .partition_point(|start| *start <= byte)
            .saturating_sub(1);
        let start = self.starts.get(line).copied().unwrap_or(0);
        let character = self
            .source
            .get(start..byte)
            .map_or(0, |text| text.encode_utf16().count());
        Position::new(
            u32::try_from(line).unwrap_or(u32::MAX),
            u32::try_from(character).unwrap_or(u32::MAX),
        )
    }

    fn range(&self, span: reimer_diagnostics::Span) -> Range {
        Range::new(self.position(span.start), self.position(span.end))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use tower_lsp::lsp_types::{DiagnosticSeverity, Position, Url};

    use super::{Document, LineIndex};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let nonce = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir()
                .join(format!("language-server-{}-{nonce}", std::process::id()));
            fs::create_dir_all(&root).expect("fixture directory should be created");
            Self { root }
        }

        fn write(&self, relative: &str, contents: &str) -> PathBuf {
            let path = self.root.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("fixture parent should be created");
            }
            fs::write(&path, contents).expect("fixture should be written");
            path
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn line_index_should_count_non_bmp_characters_as_two_utf16_units() {
        let source = "a🦀b\n";
        let lines = LineIndex::new(Arc::from(source));

        let position = lines.position("a🦀".len());

        assert_eq!(position, Position::new(0, 3));
    }

    #[test]
    fn document_should_offer_an_organize_imports_action() {
        let source = "import z;\nimport a;\nfn main() {}\n".to_owned();
        let document = Document::new(
            Url::parse("untitled:main.reim").expect("URL should parse"),
            source,
        );

        let actions = document.code_actions(tower_lsp::lsp_types::Range::new(
            Position::new(0, 0),
            Position::new(3, 0),
        ));

        assert!(!actions.is_empty());
    }

    #[test]
    fn document_should_complete_runtime_and_comptime_symbols() {
        let document = Document::new(
            Url::parse("untitled:main.reim").expect("URL should parse"),
            "fn main() {}\n".to_owned(),
        );

        let labels = document
            .completions()
            .into_iter()
            .map(|item| item.label)
            .collect::<Vec<_>>();

        let required = [
            "Thread",
            "AtomicBool",
            "JobPool",
            "parallel_for_mut",
            "comptime",
            "size_of",
            "fields",
            "@derive",
        ];
        assert!(
            required
                .iter()
                .all(|required| labels.iter().any(|label| label == required))
        );
    }

    #[test]
    fn document_should_return_a_definition_for_a_local_use() {
        let source = "fn main() -> i32 { let answer = 42; answer }".to_owned();
        let document = Document::new(
            Url::parse("untitled:main.reim").expect("URL should parse"),
            source,
        );

        let definition = document.definition(Position::new(0, 42));

        assert!(definition.is_some());
    }

    #[test]
    fn document_should_hover_an_inferred_local_type() {
        let source = "fn main() -> i32 { let answer = 42; answer }".to_owned();
        let document = Document::new(
            Url::parse("untitled:main.reim").expect("URL should parse"),
            source,
        );

        let hover = document
            .hover(Position::new(0, 42))
            .expect("local use should have hover information");
        let tower_lsp::lsp_types::HoverContents::Markup(contents) = hover.contents else {
            panic!("hover should use markdown");
        };

        assert!(contents.value.contains("i32"));
    }

    #[test]
    fn document_should_show_an_inlay_hint_for_an_inferred_binding() {
        let source = "fn main() -> i32 { let answer = 42; answer }".to_owned();
        let document = Document::new(
            Url::parse("untitled:main.reim").expect("URL should parse"),
            source,
        );

        let hints = document.inlay_hints(tower_lsp::lsp_types::Range::new(
            Position::new(0, 0),
            Position::new(0, 48),
        ));

        assert!(hints.iter().any(|hint| {
            matches!(
                &hint.label,
                tower_lsp::lsp_types::InlayHintLabel::String(label) if label == ": i32"
            )
        }));
    }

    #[test]
    fn document_should_resolve_imports_from_a_manifest_dependency() {
        let fixture = Fixture::new();
        fixture.write(
            "app/reimer.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n\
             [dependencies]\nmath = { path = \"../math\" }\n",
        );
        let source = "from math import answer;\nfn main() -> i32 { answer() }\n";
        let main = fixture.write("app/src/main.reim", source);
        fixture.write(
            "math/reimer.toml",
            "[package]\nname = \"math\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
        );
        fixture.write("math/src/package.reim", "pub fn answer() -> i32 { 42 }\n");
        let uri = Url::from_file_path(main).expect("file URL should be created");

        let document = Document::new(uri, source.to_owned());

        assert!(
            document
                .diagnostics()
                .iter()
                .all(|diagnostic| diagnostic.severity != Some(DiagnosticSeverity::ERROR))
        );
    }

    #[test]
    fn document_should_infer_types_in_an_unsaved_file_with_imports() {
        let fixture = Fixture::new();
        fixture.write(
            "app/reimer.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
        );
        let saved = "from std::io import print;\nfn main() -> i32 { 0 }\n";
        let main = fixture.write("app/src/main.reim", saved);
        let overlay = "from std::io import print;\nfn main() -> i32 { let answer = 42; answer }\n";
        let uri = Url::from_file_path(main).expect("file URL should be created");

        let document = Document::new(uri, overlay.to_owned());
        let hover = document
            .hover(Position::new(1, 42))
            .expect("unsaved local use should have hover information");
        let tower_lsp::lsp_types::HoverContents::Markup(contents) = hover.contents else {
            panic!("hover should use markdown");
        };

        assert!(contents.value.contains("i32"));
        assert!(
            document
                .diagnostics()
                .iter()
                .all(|diagnostic| diagnostic.severity != Some(DiagnosticSeverity::ERROR))
        );
    }

    #[test]
    fn document_should_lint_must_use_calls_from_a_manifest_dependency() {
        let fixture = Fixture::new();
        fixture.write(
            "app/reimer.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n\
             [dependencies]\nmath = { path = \"../math\" }\n",
        );
        let source = "from math import checked;\nfn main() -> i32 { checked(); 0 }\n";
        let main = fixture.write("app/src/main.reim", source);
        fixture.write(
            "math/reimer.toml",
            "[package]\nname = \"math\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
        );
        fixture.write(
            "math/src/package.reim",
            "@must_use\npub fn checked() -> i32 { 42 }\n",
        );
        let uri = Url::from_file_path(main).expect("file URL should be created");

        let document = Document::new(uri, source.to_owned());

        assert!(document.diagnostics().iter().any(|diagnostic| {
            diagnostic.code.as_ref().is_some_and(|code| {
                code == &tower_lsp::lsp_types::NumberOrString::String("L2020".to_owned())
            })
        }));
    }
}
