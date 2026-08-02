//! Document model and protocol-independent editor operations for the language
//! server binary.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use reimer_ast::Item;
use reimer_lint::{
    AllocationQuantity, AllocatorSummary, Analysis, Finding, Fix, Severity, analyze,
    apply_spelling_fixes, index_typed_with_documentation, lint_typed, organize_imports,
};
use reimer_project::{LockMode, Project, ProjectError};
use tower_lsp::lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeLens, Command, CompletionItem,
    CompletionItemKind, Diagnostic, DiagnosticSeverity, DocumentSymbol, Documentation, Hover,
    HoverContents, InlayHint, InlayHintKind, InlayHintLabel, Location, MarkupContent, MarkupKind,
    NumberOrString, Position, PrepareRenameResponse, Range, SymbolKind, TextEdit, Url,
    WorkspaceEdit,
};

/// One immutable source snapshot and all indexes derived from it.
#[derive(Debug)]
pub struct Document {
    uri: Url,
    text: Arc<str>,
    lines: LineIndex,
    analysis: Analysis,
    source_paths: HashSet<PathBuf>,
    package_loaded: bool,
}

impl Document {
    /// Analyzes a newly opened or changed document.
    #[must_use]
    pub fn new(uri: Url, text: String) -> Self {
        Self::new_with_overlays(uri, text, &[])
    }

    fn new_with_overlays(uri: Url, text: String, overlays: &[(PathBuf, String)]) -> Self {
        let text: Arc<str> = text.into();
        let lines = LineIndex::new(Arc::clone(&text));
        let mut analysis = analyze(&text);
        if syntax_has_imports(&analysis) {
            analysis
                .findings
                .retain(|finding| finding.severity != Severity::Error);
        }
        let source_paths = uri.to_file_path().ok().into_iter().collect::<HashSet<_>>();
        let mut document = Self {
            uri,
            text,
            lines,
            analysis,
            source_paths,
            package_loaded: false,
        };
        document.refresh_package(overlays);
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
        let type_hint = narrowest_type_hint(&self.analysis.type_hints, byte);
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
            markdown.push_str(&hint.documentation);
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

    /// Returns the declaration range and current spelling for a semantic rename.
    #[must_use]
    pub fn prepare_rename(&self, position: Position) -> Option<PrepareRenameResponse> {
        let target = self.rename_target(position)?;
        let placeholder = self.text.get(target.start..target.end)?.to_owned();
        Some(PrepareRenameResponse::RangeWithPlaceholder {
            range: self.lines.range(target),
            placeholder,
        })
    }

    /// Renames one compiler-resolved symbol and all of its uses in this document.
    #[must_use]
    pub fn rename(&self, position: Position, new_name: &str) -> Option<WorkspaceEdit> {
        if !is_identifier(new_name) {
            return None;
        }
        let target = self.rename_target(position)?;
        let mut spans = self
            .analysis
            .definitions
            .iter()
            .filter(|link| link.target_span == target)
            .map(|link| link.use_span)
            .chain(std::iter::once(target))
            .collect::<Vec<_>>();
        spans.sort_by_key(|span| (span.start, span.end));
        spans.dedup();
        let edits = spans
            .into_iter()
            .map(|span| TextEdit {
                range: self.lines.range(span),
                new_text: new_name.to_owned(),
            })
            .collect();
        let mut changes = HashMap::new();
        changes.insert(self.uri.clone(), edits);
        Some(WorkspaceEdit {
            changes: Some(changes),
            document_changes: None,
            change_annotations: None,
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
                let (name, kind, detail, span) = match item {
                    Item::Function(function) => (
                        &function.name.name,
                        CompletionItemKind::FUNCTION,
                        "function",
                        function.span,
                    ),
                    Item::ExternFunction(function) => (
                        &function.name.name,
                        CompletionItemKind::FUNCTION,
                        "native function",
                        function.span,
                    ),
                    Item::Struct(declaration) => (
                        &declaration.name.name,
                        CompletionItemKind::STRUCT,
                        "struct",
                        declaration.span,
                    ),
                    Item::Enum(declaration) => (
                        &declaration.name.name,
                        CompletionItemKind::ENUM,
                        "enum",
                        declaration.span,
                    ),
                    Item::TypeAlias(declaration) => (
                        &declaration.name.name,
                        CompletionItemKind::TYPE_PARAMETER,
                        "type alias",
                        declaration.span,
                    ),
                    Item::Trait(declaration) => (
                        &declaration.name.name,
                        CompletionItemKind::INTERFACE,
                        "trait",
                        declaration.span,
                    ),
                    Item::Constant(declaration) => (
                        &declaration.name.name,
                        CompletionItemKind::CONSTANT,
                        "compile-time constant",
                        declaration.span,
                    ),
                    Item::Static(declaration) => (
                        &declaration.name.name,
                        CompletionItemKind::VARIABLE,
                        if declaration.mutable {
                            "mutable static"
                        } else {
                            "static"
                        },
                        declaration.span,
                    ),
                    Item::Import(_) | Item::Impl(_) | Item::Comptime(_) => continue,
                };
                if seen.insert(name.clone()) {
                    let documentation =
                        reimer_package::documentation_before(&self.text, span.start);
                    items.push(documented_completion(
                        name,
                        kind,
                        detail,
                        documentation.as_deref(),
                    ));
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
            if !hint.show_as_inlay()
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
                        value: hint.documentation.clone(),
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

    fn refresh_package(&mut self, overlays: &[(PathBuf, String)]) {
        let Ok(path) = self.uri.to_file_path() else {
            return;
        };
        let overlays = overlays_with_document(&path, &self.text, overlays);
        let Some(package) = self.load_package_for_document(&path, &overlays) else {
            return;
        };
        let resolved = if syntax_has_main(&self.analysis) {
            reimer_resolver::resolve(&package.program)
        } else {
            reimer_resolver::resolve_library(&package.program)
        };
        match resolved {
            Ok(mut typed) => {
                attach_package_documentation(&mut typed, &package);
                self.analysis
                    .findings
                    .retain(|finding| finding.severity != Severity::Error);
                self.analysis.findings.extend(lint_typed(&typed));
                if let Some(syntax) = &self.analysis.syntax {
                    let documentation: Vec<_> = typed
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
                            package
                                .documentation(span)
                                .map(|documentation| (id, documentation))
                        })
                        .collect();
                    let (type_hints, definitions) =
                        index_typed_with_documentation(syntax, &typed, &documentation);
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

    fn load_package_for_document(
        &mut self,
        path: &Path,
        overlays: &[(PathBuf, String)],
    ) -> Option<reimer_package::Package> {
        let result = if reimer_package::is_standard_library_source(path) {
            reimer_package::load_standard_with_overlays(path, overlays)
        } else {
            match Project::open(path, LockMode::Use) {
                Ok(project) => {
                    reimer_package::load_graph_with_overlays(&project.source_graph(path), overlays)
                }
                Err(ProjectError::ManifestNotFound { .. }) => {
                    reimer_package::load_with_overlays(path, overlays)
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
                        help: Some(
                            "fix the nearest reimer.toml or regenerate reimer.lock".to_owned(),
                        ),
                        fixes: Vec::new(),
                    });
                    return None;
                }
            }
        };
        let package = match result {
            Ok(package) => package,
            Err(diagnostics) => {
                self.source_paths
                    .extend(diagnostics.iter().map(|diagnostic| diagnostic.path.clone()));
                self.analysis
                    .findings
                    .retain(|finding| finding.severity != Severity::Error);
                self.analysis.findings.extend(
                    diagnostics
                        .into_iter()
                        .filter(|diagnostic| diagnostic.path == path)
                        .map(|diagnostic| compiler_finding(diagnostic.diagnostic)),
                );
                return None;
            }
        };
        self.package_loaded = true;
        self.source_paths
            .extend(package.source_paths().map(Path::to_path_buf));
        Some(package)
    }

    fn rename_target(&self, position: Position) -> Option<reimer_diagnostics::Span> {
        let byte = self.lines.byte(position)?;
        narrowest_containing(&self.analysis.definitions, byte, |link| link.use_span)
            .map(|link| link.target_span)
            .filter(|span| span.end <= self.text.len())
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
            Item::TypeAlias(declaration) => (
                declaration.name.name.clone(),
                Some("type alias".to_owned()),
                SymbolKind::TYPE_PARAMETER,
                declaration.span,
                declaration.name.span,
                None,
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
            Item::Static(declaration) => (
                declaration.name.name.clone(),
                Some(if declaration.mutable {
                    "mutable static".to_owned()
                } else {
                    "static".to_owned()
                }),
                SymbolKind::VARIABLE,
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

/// Compiler snapshots for every open editor document.
///
/// Changes rebuild only the changed document and open documents whose last
/// package snapshot loaded that source path.
#[derive(Debug, Default)]
pub struct Workspace {
    documents: HashMap<Url, Document>,
}

impl Workspace {
    /// Returns one currently open document.
    #[must_use]
    pub fn get(&self, uri: &Url) -> Option<&Document> {
        self.documents.get(uri)
    }

    /// Replaces one open source snapshot and rebuilds its open dependants.
    pub fn update(&mut self, uri: Url, text: String) -> Vec<Url> {
        let changed_path = uri.to_file_path().ok();
        let mut affected = self
            .documents
            .iter()
            .filter(|(candidate, document)| {
                *candidate == &uri
                    || changed_path
                        .as_deref()
                        .is_some_and(|path| document.source_paths.contains(path))
            })
            .map(|(candidate, _)| candidate.clone())
            .collect::<HashSet<_>>();
        affected.insert(uri.clone());

        let mut snapshots = self.snapshots();
        snapshots.insert(uri, text);
        self.rebuild(affected, &snapshots)
    }

    /// Rebuilds documents affected by files changed outside the editor.
    pub fn refresh_paths(&mut self, paths: &[PathBuf]) -> Vec<Url> {
        let refresh_all = paths
            .iter()
            .any(|path| path.extension().and_then(|extension| extension.to_str()) != Some("reim"));
        let affected = self
            .documents
            .iter()
            .filter(|(_, document)| {
                refresh_all
                    || paths
                        .iter()
                        .any(|path| document.source_paths.contains(path))
            })
            .map(|(uri, _)| uri.clone())
            .collect();
        let snapshots = self.snapshots();
        self.rebuild(affected, &snapshots)
    }

    /// Removes one editor overlay and refreshes open dependants from disk.
    pub fn close(&mut self, uri: &Url) -> Vec<Url> {
        let closed_path = uri.to_file_path().ok();
        self.documents.remove(uri);
        let affected = self
            .documents
            .iter()
            .filter(|(_, document)| {
                closed_path
                    .as_deref()
                    .is_some_and(|path| document.source_paths.contains(path))
            })
            .map(|(candidate, _)| candidate.clone())
            .collect();
        let snapshots = self.snapshots();
        self.rebuild(affected, &snapshots)
    }

    fn snapshots(&self) -> HashMap<Url, String> {
        self.documents
            .iter()
            .map(|(uri, document)| (uri.clone(), document.text().to_owned()))
            .collect()
    }

    fn rebuild(&mut self, affected: HashSet<Url>, snapshots: &HashMap<Url, String>) -> Vec<Url> {
        let overlays = snapshots
            .iter()
            .filter_map(|(uri, text)| uri.to_file_path().ok().map(|path| (path, text.clone())))
            .collect::<Vec<_>>();
        let mut affected = affected.into_iter().collect::<Vec<_>>();
        affected.sort_by(|left, right| left.as_str().cmp(right.as_str()));

        for uri in &affected {
            let Some(text) = snapshots.get(uri) else {
                continue;
            };
            let previous_paths = self
                .documents
                .get(uri)
                .map(|document| document.source_paths.clone())
                .unwrap_or_default();
            let mut document = Document::new_with_overlays(uri.clone(), text.clone(), &overlays);
            if !document.package_loaded {
                document.source_paths.extend(previous_paths);
            }
            self.documents.insert(uri.clone(), document);
        }
        affected
    }
}

fn attach_package_documentation(
    typed: &mut reimer_hir::Program,
    package: &reimer_package::Package,
) {
    for definition in &mut typed.types {
        if definition.name.is_some() {
            definition.documentation = package.documentation(definition.span);
        }
    }
    for value in &mut typed.statics {
        value.documentation = package.documentation(value.span);
    }
}

fn overlays_with_document(
    path: &Path,
    text: &str,
    overlays: &[(PathBuf, String)],
) -> Vec<(PathBuf, String)> {
    let mut overlays = overlays.to_vec();
    if let Some((_, source)) = overlays
        .iter_mut()
        .find(|(overlay_path, _)| overlay_path == path)
    {
        text.clone_into(source);
    } else {
        overlays.push((path.to_path_buf(), text.to_owned()));
    }
    overlays
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

fn is_identifier(name: &str) -> bool {
    let Ok(tokens) = reimer_lexer::lex(name) else {
        return false;
    };
    matches!(
        tokens.as_slice(),
        [
            reimer_lexer::Token {
                kind: reimer_lexer::TokenKind::Identifier(_),
                span,
            },
            reimer_lexer::Token {
                kind: reimer_lexer::TokenKind::Eof,
                ..
            }
        ] if span.start == 0 && span.end == name.len()
    )
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

fn narrowest_type_hint(
    hints: &[reimer_lint::TypeHint],
    byte: usize,
) -> Option<&reimer_lint::TypeHint> {
    hints
        .iter()
        .filter(|hint| byte >= hint.span.start && byte <= hint.span.end)
        .min_by_key(|hint| {
            (
                hint.span.end.saturating_sub(hint.span.start),
                hint.hover_priority(),
            )
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

fn documented_completion(
    label: &str,
    kind: CompletionItemKind,
    detail: &str,
    documentation: Option<&str>,
) -> CompletionItem {
    let mut item = simple_completion(label, kind, detail);
    item.documentation = documentation.map(|value| {
        Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: value.to_owned(),
        })
    });
    item
}

const LANGUAGE_KEYWORDS: &[&str] = &[
    "as", "break", "comptime", "const", "continue", "defer", "else", "enum", "extern", "false",
    "fn", "for", "from", "if", "impl", "import", "in", "let", "loop", "match", "mut", "pub",
    "return", "struct", "trait", "true", "type", "unsafe", "where",
];

const PRIMITIVE_TYPES: &[&str] = &[
    "bool",
    "char",
    "cstr",
    "f32",
    "f64",
    "i8",
    "i16",
    "i32",
    "i64",
    "i128",
    "isize",
    "str",
    "u8",
    "u16",
    "u32",
    "u64",
    "u128",
    "usize",
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
];

const STANDARD_SYMBOLS: &[(&str, CompletionItemKind, &str)] = &[
    ("assert", CompletionItemKind::FUNCTION, "language intrinsic"),
    (
        "debug_assert",
        CompletionItemKind::FUNCTION,
        "language intrinsic",
    ),
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
    ("File", CompletionItemKind::STRUCT, "std::fs"),
    ("FileBuffer", CompletionItemKind::STRUCT, "std::fs"),
    ("FileError", CompletionItemKind::ENUM, "std::fs"),
    ("open", CompletionItemKind::FUNCTION, "std::fs"),
    ("create", CompletionItemKind::FUNCTION, "std::fs"),
    ("append", CompletionItemKind::FUNCTION, "std::fs"),
    ("exists", CompletionItemKind::FUNCTION, "std::fs"),
    ("remove_file", CompletionItemKind::FUNCTION, "std::fs"),
    ("rename", CompletionItemKind::FUNCTION, "std::fs"),
    ("write_string", CompletionItemKind::FUNCTION, "std::fs"),
    ("Vec2", CompletionItemKind::STRUCT, "std::math"),
    ("Vec3", CompletionItemKind::STRUCT, "std::math"),
    ("Vec4", CompletionItemKind::STRUCT, "std::math"),
    ("PI", CompletionItemKind::CONSTANT, "std::math"),
    ("TAU", CompletionItemKind::CONSTANT, "std::math"),
    ("E", CompletionItemKind::CONSTANT, "std::math"),
    ("absolute", CompletionItemKind::FUNCTION, "std::math"),
    ("square_root", CompletionItemKind::FUNCTION, "std::math"),
    ("floor", CompletionItemKind::FUNCTION, "std::math"),
    ("ceil", CompletionItemKind::FUNCTION, "std::math"),
    ("round", CompletionItemKind::FUNCTION, "std::math"),
    ("sine", CompletionItemKind::FUNCTION, "std::math"),
    ("cosine", CompletionItemKind::FUNCTION, "std::math"),
    ("tangent", CompletionItemKind::FUNCTION, "std::math"),
    ("exponential", CompletionItemKind::FUNCTION, "std::math"),
    (
        "natural_logarithm",
        CompletionItemKind::FUNCTION,
        "std::math",
    ),
    ("power", CompletionItemKind::FUNCTION, "std::math"),
    ("minimum", CompletionItemKind::FUNCTION, "std::math"),
    ("maximum", CompletionItemKind::FUNCTION, "std::math"),
    ("clamp", CompletionItemKind::FUNCTION, "std::math"),
    ("Void", CompletionItemKind::TYPE_PARAMETER, "std::c"),
    ("Char", CompletionItemKind::TYPE_PARAMETER, "std::c"),
    ("SignedChar", CompletionItemKind::TYPE_PARAMETER, "std::c"),
    ("UnsignedChar", CompletionItemKind::TYPE_PARAMETER, "std::c"),
    ("Short", CompletionItemKind::TYPE_PARAMETER, "std::c"),
    (
        "UnsignedShort",
        CompletionItemKind::TYPE_PARAMETER,
        "std::c",
    ),
    ("Int", CompletionItemKind::TYPE_PARAMETER, "std::c"),
    ("UnsignedInt", CompletionItemKind::TYPE_PARAMETER, "std::c"),
    ("Long", CompletionItemKind::TYPE_PARAMETER, "std::c"),
    ("UnsignedLong", CompletionItemKind::TYPE_PARAMETER, "std::c"),
    ("LongLong", CompletionItemKind::TYPE_PARAMETER, "std::c"),
    (
        "UnsignedLongLong",
        CompletionItemKind::TYPE_PARAMETER,
        "std::c",
    ),
    ("Float", CompletionItemKind::TYPE_PARAMETER, "std::c"),
    ("Double", CompletionItemKind::TYPE_PARAMETER, "std::c"),
    ("Size", CompletionItemKind::TYPE_PARAMETER, "std::c"),
    ("PtrDiff", CompletionItemKind::TYPE_PARAMETER, "std::c"),
    ("Str", CompletionItemKind::TYPE_PARAMETER, "std::c"),
    ("Bool", CompletionItemKind::TYPE_PARAMETER, "std::c"),
    ("ConstBuffer", CompletionItemKind::STRUCT, "std::c"),
    ("Buffer", CompletionItemKind::STRUCT, "std::c"),
    ("null", CompletionItemKind::FUNCTION, "std::c"),
    ("null_const", CompletionItemKind::FUNCTION, "std::c"),
    ("is_null", CompletionItemKind::FUNCTION, "std::c"),
    ("is_null_mut", CompletionItemKind::FUNCTION, "std::c"),
    ("int_from_bool", CompletionItemKind::FUNCTION, "std::c"),
    ("bool_from_int", CompletionItemKind::FUNCTION, "std::c"),
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
    ("OperatingSystem", CompletionItemKind::ENUM, "std::target"),
    ("os", CompletionItemKind::FUNCTION, "std::target"),
    ("EnvError", CompletionItemKind::ENUM, "std::env"),
    ("Arguments", CompletionItemKind::STRUCT, "std::env"),
    ("args", CompletionItemKind::FUNCTION, "std::env"),
    ("argument", CompletionItemKind::FUNCTION, "std::env"),
    ("var", CompletionItemKind::FUNCTION, "std::env"),
    ("current_dir", CompletionItemKind::FUNCTION, "std::env"),
    ("current_exe", CompletionItemKind::FUNCTION, "std::env"),
    ("ProcessError", CompletionItemKind::ENUM, "std::process"),
    ("ExitStatus", CompletionItemKind::STRUCT, "std::process"),
    ("Command", CompletionItemKind::STRUCT, "std::process"),
    ("Child", CompletionItemKind::STRUCT, "std::process"),
    ("id", CompletionItemKind::FUNCTION, "std::process"),
    ("exit", CompletionItemKind::FUNCTION, "std::process"),
    ("arg", CompletionItemKind::METHOD, "Command::arg"),
    ("with_arg", CompletionItemKind::METHOD, "Command::with_arg"),
    ("env", CompletionItemKind::METHOD, "Command::env"),
    ("with_env", CompletionItemKind::METHOD, "Command::with_env"),
    (
        "env_remove",
        CompletionItemKind::METHOD,
        "Command::env_remove",
    ),
    (
        "env_clear",
        CompletionItemKind::METHOD,
        "Command::env_clear",
    ),
    (
        "without_env",
        CompletionItemKind::METHOD,
        "Command::without_env",
    ),
    (
        "with_cleared_env",
        CompletionItemKind::METHOD,
        "Command::with_cleared_env",
    ),
    (
        "with_current_dir",
        CompletionItemKind::METHOD,
        "Command::with_current_dir",
    ),
    ("status", CompletionItemKind::METHOD, "Command::status"),
    ("spawn", CompletionItemKind::METHOD, "Command::spawn"),
    ("wait", CompletionItemKind::METHOD, "Child::wait"),
    ("kill", CompletionItemKind::METHOD, "Child::kill"),
    ("Duration", CompletionItemKind::STRUCT, "std::time"),
    ("Instant", CompletionItemKind::STRUCT, "std::time"),
    ("time", CompletionItemKind::FUNCTION, "std::time"),
    ("unix_time", CompletionItemKind::FUNCTION, "std::time"),
    ("monotonic", CompletionItemKind::FUNCTION, "std::time"),
    ("perf_counter", CompletionItemKind::FUNCTION, "std::time"),
    ("sleep", CompletionItemKind::FUNCTION, "std::time"),
    ("sleep_seconds", CompletionItemKind::FUNCTION, "std::time"),
    (
        "sleep_milliseconds",
        CompletionItemKind::FUNCTION,
        "std::time",
    ),
    (
        "from_seconds",
        CompletionItemKind::METHOD,
        "Duration::from_seconds",
    ),
    (
        "from_milliseconds",
        CompletionItemKind::METHOD,
        "Duration::from_milliseconds",
    ),
    (
        "from_microseconds",
        CompletionItemKind::METHOD,
        "Duration::from_microseconds",
    ),
    (
        "from_nanoseconds",
        CompletionItemKind::METHOD,
        "Duration::from_nanoseconds",
    ),
    (
        "as_seconds",
        CompletionItemKind::METHOD,
        "Duration::as_seconds",
    ),
    (
        "subsec_nanoseconds",
        CompletionItemKind::METHOD,
        "Duration::subsec_nanoseconds",
    ),
    (
        "as_seconds_f64",
        CompletionItemKind::METHOD,
        "Duration::as_seconds_f64",
    ),
    (
        "as_milliseconds",
        CompletionItemKind::METHOD,
        "Duration::as_milliseconds",
    ),
    (
        "as_microseconds",
        CompletionItemKind::METHOD,
        "Duration::as_microseconds",
    ),
    (
        "as_nanoseconds",
        CompletionItemKind::METHOD,
        "Duration::as_nanoseconds",
    ),
    ("is_zero", CompletionItemKind::METHOD, "Duration::is_zero"),
    ("now", CompletionItemKind::METHOD, "Instant::now"),
    ("elapsed", CompletionItemKind::METHOD, "Instant::elapsed"),
    (
        "duration_since",
        CompletionItemKind::METHOD,
        "Instant::duration_since",
    ),
    ("Option", CompletionItemKind::ENUM, "core"),
    ("Result", CompletionItemKind::ENUM, "core"),
    ("String", CompletionItemKind::STRUCT, "std::string"),
    ("Debug", CompletionItemKind::INTERFACE, "std::fmt"),
    ("Display", CompletionItemKind::INTERFACE, "std::fmt"),
    ("Formatter", CompletionItemKind::STRUCT, "std::fmt"),
    ("FormatArgs", CompletionItemKind::STRUCT, "std::fmt"),
    (
        "FormatError",
        CompletionItemKind::TYPE_PARAMETER,
        "std::fmt",
    ),
    (
        "with_capacity",
        CompletionItemKind::METHOD,
        "String::with_capacity",
    ),
    ("from", CompletionItemKind::METHOD, "String::from"),
    ("as_str", CompletionItemKind::METHOD, "String::as_str"),
    ("clone_in", CompletionItemKind::METHOD, "String::clone_in"),
    ("push_str", CompletionItemKind::METHOD, "String::push_str"),
    (
        "push_string",
        CompletionItemKind::METHOD,
        "String::push_string",
    ),
    ("push_char", CompletionItemKind::METHOD, "String::push_char"),
    ("push_bool", CompletionItemKind::METHOD, "String::push_bool"),
    ("push_i128", CompletionItemKind::METHOD, "String::push_i128"),
    ("push_u128", CompletionItemKind::METHOD, "String::push_u128"),
    ("push_f32", CompletionItemKind::METHOD, "String::push_f32"),
    ("push_f64", CompletionItemKind::METHOD, "String::push_f64"),
    (
        "push_format",
        CompletionItemKind::METHOD,
        "String::push_format",
    ),
    (
        "write_str",
        CompletionItemKind::METHOD,
        "Formatter::write_str",
    ),
    (
        "write_char",
        CompletionItemKind::METHOD,
        "Formatter::write_char",
    ),
    (
        "write_bool",
        CompletionItemKind::METHOD,
        "Formatter::write_bool",
    ),
    (
        "write_i128",
        CompletionItemKind::METHOD,
        "Formatter::write_i128",
    ),
    (
        "write_u128",
        CompletionItemKind::METHOD,
        "Formatter::write_u128",
    ),
    (
        "write_f32",
        CompletionItemKind::METHOD,
        "Formatter::write_f32",
    ),
    (
        "write_f64",
        CompletionItemKind::METHOD,
        "Formatter::write_f64",
    ),
    ("fmt_debug", CompletionItemKind::METHOD, "Debug::fmt_debug"),
    ("concat", CompletionItemKind::FUNCTION, "std::string"),
    ("concat3", CompletionItemKind::FUNCTION, "std::string"),
    ("repeat", CompletionItemKind::FUNCTION, "std::string"),
    ("join_strings", CompletionItemKind::FUNCTION, "std::string"),
    ("char_count", CompletionItemKind::FUNCTION, "std::string"),
    ("starts_with", CompletionItemKind::FUNCTION, "std::string"),
    ("ends_with", CompletionItemKind::FUNCTION, "std::string"),
    ("contains", CompletionItemKind::FUNCTION, "std::string"),
    ("find", CompletionItemKind::FUNCTION, "std::string"),
    (
        "is_char_boundary",
        CompletionItemKind::FUNCTION,
        "std::string",
    ),
    ("to_lowercase", CompletionItemKind::FUNCTION, "std::string"),
    ("to_uppercase", CompletionItemKind::FUNCTION, "std::string"),
    (
        "wrapping_add",
        CompletionItemKind::METHOD,
        "integer overflow",
    ),
    (
        "checked_add",
        CompletionItemKind::METHOD,
        "integer overflow",
    ),
    (
        "saturating_add",
        CompletionItemKind::METHOD,
        "integer overflow",
    ),
    (
        "get",
        CompletionItemKind::METHOD,
        "recoverable slice access",
    ),
    (
        "get_mut",
        CompletionItemKind::METHOD,
        "recoverable mutable slice access",
    ),
    ("bytes", CompletionItemKind::METHOD, "UTF-8 byte view"),
    (
        "chars",
        CompletionItemKind::METHOD,
        "Unicode scalar iteration",
    ),
    ("next", CompletionItemKind::METHOD, "iterator advancement"),
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
        ("@derive(Pod)", "@repr(C)\n@derive(Pod)"),
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

    use tower_lsp::lsp_types::{DiagnosticSeverity, NumberOrString, Position, Url};

    use super::{Document, LineIndex, Workspace};

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
            "assert",
            "debug_assert",
            "Thread",
            "AtomicBool",
            "JobPool",
            "parallel_for_mut",
            "Duration",
            "Instant",
            "perf_counter",
            "sleep",
            "wrapping_add",
            "checked_add",
            "saturating_add",
            "Debug",
            "Display",
            "Formatter",
            "push_format",
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
    fn document_should_explain_assertion_intrinsics_on_hover() {
        let source = "fn main() -> i32 {
            assert(true, \"required invariant\");
            debug_assert(true, \"debug invariant\");
            42
        }";
        let document = Document::new(
            Url::parse("untitled:main.reim").expect("URL should parse"),
            source.to_owned(),
        );
        let lines = LineIndex::new(Arc::from(source));

        let assert_position = lines.position(
            source
                .find("assert(true")
                .expect("assert call should exist"),
        );
        let debug_position = lines.position(
            source
                .find("debug_assert")
                .expect("debug assertion call should exist"),
        );
        let assert_hover = document
            .hover(assert_position)
            .expect("assert call should have hover");
        let debug_hover = document
            .hover(debug_position)
            .expect("debug assertion call should have hover");
        let tower_lsp::lsp_types::HoverContents::Markup(assert_contents) = assert_hover.contents
        else {
            panic!("assert hover should use markdown");
        };
        let tower_lsp::lsp_types::HoverContents::Markup(debug_contents) = debug_hover.contents
        else {
            panic!("debug assertion hover should use markdown");
        };

        assert!(assert_contents.value.contains("fn assert(condition: bool"));
        assert!(assert_contents.value.contains("every build profile"));
        assert!(
            debug_contents
                .value
                .contains("fn debug_assert(condition: bool")
        );
        assert!(
            debug_contents
                .value
                .contains("Optimized builds do not evaluate")
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
    fn document_should_rename_only_the_resolved_symbol() {
        let source = "fn first() -> i32 { let value = 1; value }\n\
                      fn second() -> i32 { let value = 2; value }\n\
                      fn main() -> i32 { first() + second() }\n"
            .to_owned();
        let lines = LineIndex::new(Arc::from(source.as_str()));
        let declaration = source.find("value = 1").expect("declaration should exist");
        let document = Document::new(
            Url::parse("untitled:rename.reim").expect("URL should parse"),
            source.clone(),
        );

        let prepared = document
            .prepare_rename(lines.position(declaration))
            .expect("local declaration should be renameable");
        let tower_lsp::lsp_types::PrepareRenameResponse::RangeWithPlaceholder {
            placeholder, ..
        } = prepared
        else {
            panic!("prepare rename should include the current spelling");
        };
        assert_eq!(placeholder, "value");

        let edit = document
            .rename(lines.position(declaration), "answer")
            .expect("valid rename should produce an edit");
        let edits = edit
            .changes
            .expect("rename should contain document changes")
            .remove(&Url::parse("untitled:rename.reim").expect("URL should parse"))
            .expect("the active document should be edited");
        let changed_offsets = edits
            .iter()
            .map(|edit| {
                document
                    .lines
                    .byte(edit.range.start)
                    .expect("edit position should map to a byte")
            })
            .collect::<Vec<_>>();

        assert_eq!(edits.len(), 2);
        assert!(edits.iter().all(|edit| edit.new_text == "answer"));
        assert!(changed_offsets.iter().all(|offset| {
            source
                .get(*offset..)
                .is_some_and(|tail| tail.starts_with("value"))
        }));
        assert!(
            changed_offsets
                .iter()
                .all(|offset| *offset < source.find("fn second").expect("function should exist"))
        );
        assert!(document.rename(lines.position(declaration), "fn").is_none());
    }

    #[test]
    fn document_should_rename_a_nominal_type_and_its_uses() {
        let source = "struct Player { score: i32 }\n\
                      fn keep(player: Player) -> Player { player }\n"
            .to_owned();
        let lines = LineIndex::new(Arc::from(source.as_str()));
        let declaration = source.find("Player").expect("type should exist");
        let document = Document::new(
            Url::parse("untitled:type-rename.reim").expect("URL should parse"),
            source,
        );

        let edit = document
            .rename(lines.position(declaration), "Competitor")
            .expect("nominal type should be renameable");
        let edits = edit
            .changes
            .expect("rename should contain document changes")
            .remove(&Url::parse("untitled:type-rename.reim").expect("URL should parse"))
            .expect("the active document should be edited");

        assert_eq!(edits.len(), 3);
        assert!(edits.iter().all(|edit| edit.new_text == "Competitor"));
    }

    #[test]
    fn workspace_should_propagate_unsaved_dependency_types() {
        let fixture = Fixture::new();
        let main_source = "from values import answer;\nfn main() -> i32 { answer() }\n";
        let values_source = "pub fn answer() -> i32 { 42 }\n";
        let main_path = fixture.write("main.reim", main_source);
        let values_path = fixture.write("values.reim", values_source);
        let unrelated_path = fixture.write("unrelated.reim", "pub fn unrelated() -> i32 { 7 }\n");
        let main_uri = Url::from_file_path(&main_path).expect("main path should become a URL");
        let values_uri =
            Url::from_file_path(&values_path).expect("dependency path should become a URL");
        let unrelated_uri =
            Url::from_file_path(&unrelated_path).expect("unrelated path should become a URL");
        let mut workspace = Workspace::default();

        workspace.update(main_uri.clone(), main_source.to_owned());
        workspace.update(
            unrelated_uri.clone(),
            "pub fn unrelated() -> i32 { 7 }\n".to_owned(),
        );
        assert!(
            workspace
                .get(&main_uri)
                .expect("main document should be open")
                .diagnostics()
                .iter()
                .all(|diagnostic| diagnostic.severity != Some(DiagnosticSeverity::ERROR))
        );

        let affected = workspace.update(
            values_uri.clone(),
            "pub fn answer() -> bool { true }\n".to_owned(),
        );
        assert!(affected.contains(&main_uri));
        assert!(affected.contains(&values_uri));
        assert!(!affected.contains(&unrelated_uri));
        assert!(
            workspace
                .get(&main_uri)
                .expect("main document should remain open")
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.severity == Some(DiagnosticSeverity::ERROR))
        );

        workspace.update(values_uri, values_source.to_owned());
        assert!(
            workspace
                .get(&main_uri)
                .expect("main document should remain open")
                .diagnostics()
                .iter()
                .all(|diagnostic| diagnostic.severity != Some(DiagnosticSeverity::ERROR))
        );
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
        assert!(contents.value.contains("32-bit signed integer"));
        assert!(
            contents
                .value
                .contains("`-2,147,483,648` to `2,147,483,647`")
        );
        assert!(!contents.value.to_lowercase().contains("inferred"));
    }

    #[test]
    fn document_should_describe_explicit_integer_overflow_methods() {
        let source = "fn calculate() -> u8 {
                          let maximum: u8 = 255;
                          let checked = maximum.checked_add(1);
                          let saturated = maximum.saturating_add(1);
                          match checked {
                              Some(value) => value,
                              None => saturated,
                          }
                      }
                      fn main() -> i32 { calculate() as i32 }"
            .to_owned();
        let method_offset = source
            .find("saturating_add")
            .expect("method call should exist");
        let position = LineIndex::new(Arc::from(source.as_str())).position(method_offset);
        let checked_offset = source
            .rfind("checked {")
            .expect("checked local use should exist");
        let checked_position = LineIndex::new(Arc::from(source.as_str())).position(checked_offset);
        let document = Document::new(
            Url::parse("untitled:overflow.reim").expect("URL should parse"),
            source,
        );

        let hover = document
            .hover(position)
            .expect("integer method should have hover information");
        let tower_lsp::lsp_types::HoverContents::Markup(contents) = hover.contents else {
            panic!("hover should use markdown");
        };

        assert!(
            contents
                .value
                .contains("fn saturating_add(self: u8, right: u8) -> u8")
        );
        assert!(contents.value.contains("clamps overflow"));
        assert!(contents.value.contains("`u8` bound"));

        let checked_hover = document
            .hover(checked_position)
            .expect("checked result should have hover information");
        let tower_lsp::lsp_types::HoverContents::Markup(checked_contents) = checked_hover.contents
        else {
            panic!("hover should use markdown");
        };
        assert!(checked_contents.value.contains("Option<u8>"));
        assert!(checked_contents.value.contains("`Some`"));
        assert!(checked_contents.value.contains("`None`"));
    }

    #[test]
    fn document_should_describe_recoverable_slice_access() {
        let source = "fn main() -> i32 {\n\
                          let values: [i32; 2] = [20, 22];\n\
                          let slice: &[i32] = &values;\n\
                          let found = slice.get(0);\n\
                          match found {\n\
                              Some(value) => *value,\n\
                              None => 0,\n\
                          }\n\
                      }\n"
        .to_owned();
        let method_offset = source.find("get(0)").expect("slice method should exist");
        let method_position = LineIndex::new(Arc::from(source.as_str())).position(method_offset);
        let result_offset = source.rfind("found {").expect("result use should exist");
        let result_position = LineIndex::new(Arc::from(source.as_str())).position(result_offset);
        let document = Document::new(
            Url::parse("untitled:slice-access.reim").expect("URL should parse"),
            source,
        );

        let method_hover = document
            .hover(method_position)
            .expect("slice method should have hover information");
        let tower_lsp::lsp_types::HoverContents::Markup(method_contents) = method_hover.contents
        else {
            panic!("hover should use markdown");
        };
        assert!(
            method_contents
                .value
                .contains("fn get(self: &[i32], index: usize) -> Option<&i32>")
        );
        assert!(method_contents.value.contains("No bounds panic"));

        let result_hover = document
            .hover(result_position)
            .expect("slice access result should have hover information");
        let tower_lsp::lsp_types::HoverContents::Markup(result_contents) = result_hover.contents
        else {
            panic!("hover should use markdown");
        };
        assert!(result_contents.value.contains("Option<&i32>"));
        assert!(result_contents.value.contains("`Some`"));
        assert!(result_contents.value.contains("`None`"));
    }

    #[test]
    fn document_should_describe_utf8_views_and_character_iteration() {
        let source = "fn main() -> i32 {\n\
                          let text: str = \"Aé🦀\";\n\
                          let bytes = text.bytes();\n\
                          let mut characters = text.chars();\n\
                          let value = characters.next();\n\
                          match value {\n\
                              Some(character) => character as i32,\n\
                              None => 0,\n\
                          }\n\
                      }\n"
        .to_owned();
        let lines = LineIndex::new(Arc::from(source.as_str()));
        let bytes_position =
            lines.position(source.find("bytes()").expect("bytes call should exist"));
        let chars_position =
            lines.position(source.find("chars()").expect("chars call should exist"));
        let next_position = lines.position(source.find("next()").expect("next call should exist"));
        let document = Document::new(
            Url::parse("untitled:utf8.reim").expect("URL should parse"),
            source,
        );

        let bytes_hover = document
            .hover(bytes_position)
            .expect("bytes method should have hover information");
        let tower_lsp::lsp_types::HoverContents::Markup(bytes_contents) = bytes_hover.contents
        else {
            panic!("hover should use markdown");
        };
        assert!(
            bytes_contents
                .value
                .contains("fn bytes(self: str) -> &[u8]")
        );
        assert!(bytes_contents.value.contains("No allocation or decoding"));

        let chars_hover = document
            .hover(chars_position)
            .expect("chars method should have hover information");
        let tower_lsp::lsp_types::HoverContents::Markup(chars_contents) = chars_hover.contents
        else {
            panic!("hover should use markdown");
        };
        assert!(
            chars_contents
                .value
                .contains("fn chars(self: str) -> Chars")
        );
        assert!(chars_contents.value.contains("Unicode scalar values"));

        let next_hover = document
            .hover(next_position)
            .expect("next method should have hover information");
        let tower_lsp::lsp_types::HoverContents::Markup(next_contents) = next_hover.contents else {
            panic!("hover should use markdown");
        };
        assert!(
            next_contents
                .value
                .contains("fn next(self: &mut Chars) -> Option<char>")
        );
        assert!(next_contents.value.contains("exhausted"));
        assert!(!next_contents.value.contains("__module_"));
    }

    #[test]
    fn document_should_show_function_documentation_at_calls_and_completions() {
        let source = "/// Adds two signed integers.\n\
                      ///\n\
                      /// # Arguments\n\
                      /// - `left`: first value\n\
                      fn add(left: i32, right: i32) -> i32 { left + right }\n\
                      fn main() -> i32 { add(20, 22) }\n"
            .to_owned();
        let call_offset = source.rfind("add(20").expect("function call should exist");
        let position = LineIndex::new(Arc::from(source.as_str())).position(call_offset);
        let document = Document::new(
            Url::parse("untitled:documented.reim").expect("URL should parse"),
            source,
        );

        let hover = document
            .hover(position)
            .expect("documented function call should have hover information");
        let tower_lsp::lsp_types::HoverContents::Markup(contents) = hover.contents else {
            panic!("hover should use markdown");
        };
        assert!(contents.value.contains("fn add(left: i32, right: i32)"));
        assert!(contents.value.contains("Adds two signed integers."));
        assert!(contents.value.contains("# Arguments"));

        let completion = document
            .completions()
            .into_iter()
            .find(|item| item.label == "add")
            .expect("documented function should be completed");
        let Some(tower_lsp::lsp_types::Documentation::MarkupContent(documentation)) =
            completion.documentation
        else {
            panic!("completion documentation should use markdown");
        };
        assert!(documentation.value.contains("Adds two signed integers."));
    }

    #[test]
    fn document_should_show_generic_struct_documentation_on_values() {
        let source = "/// Stores one value without changing its type.\n\
                      struct MyAwesomeStruct<T> { value: T }\n\
                      fn main() -> i32 {\n\
                          let item: MyAwesomeStruct<i32> = MyAwesomeStruct { value: 42 };\n\
                          item.value\n\
                      }\n"
        .to_owned();
        let use_offset = source.rfind("item.value").expect("local use should exist");
        let position = LineIndex::new(Arc::from(source.as_str())).position(use_offset);
        let document = Document::new(
            Url::parse("untitled:documented-struct.reim").expect("URL should parse"),
            source,
        );

        let hover = document
            .hover(position)
            .expect("generic struct value should have hover information");
        let tower_lsp::lsp_types::HoverContents::Markup(contents) = hover.contents else {
            panic!("hover should use markdown");
        };

        assert!(
            contents.value.contains("MyAwesomeStruct<i32>"),
            "unexpected hover contents: {}",
            contents.value
        );
        assert!(
            contents
                .value
                .contains("Stores one value without changing its type.")
        );
        assert!(!contents.value.contains("__module_"));
    }

    #[test]
    fn document_should_show_static_signatures_and_documentation() {
        let source = "/// Stores the canonical answer at a stable address.\n\
                      static ANSWER: i32 = 42;\n\
                      fn main() -> i32 { ANSWER }\n"
            .to_owned();
        let use_offset = source.rfind("ANSWER").expect("static use should exist");
        let position = LineIndex::new(Arc::from(source.as_str())).position(use_offset);
        let document = Document::new(
            Url::parse("untitled:documented-static.reim").expect("URL should parse"),
            source,
        );

        let hover = document
            .hover(position)
            .expect("static use should have hover information");
        let tower_lsp::lsp_types::HoverContents::Markup(contents) = hover.contents else {
            panic!("hover should use markdown");
        };

        assert!(
            contents.value.contains("static ANSWER: i32"),
            "unexpected hover contents: {}",
            contents.value
        );
        assert!(
            contents
                .value
                .contains("Stores the canonical answer at a stable address.")
        );
    }

    #[test]
    fn document_should_hide_internal_module_names_in_generic_type_hovers() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("examples")
            .join("m7_tensor.reim");
        let source = fs::read_to_string(&path).expect("tensor example should be readable");
        let use_offset = source
            .find("output_view[0, 1]")
            .expect("tensor view use should exist");
        let position = LineIndex::new(Arc::from(source.as_str())).position(use_offset);
        let document = Document::new(
            Url::from_file_path(path).expect("example URL should be created"),
            source,
        );

        let hover = document
            .hover(position)
            .expect("tensor view use should have hover information");
        let tower_lsp::lsp_types::HoverContents::Markup(contents) = hover.contents else {
            panic!("hover should use markdown");
        };

        assert!(
            contents
                .value
                .contains("std::tensor::TensorViewMut<f32, 2>")
        );
        assert!(
            contents.value.contains("Mutable, non-owning tensor view"),
            "unexpected hover contents: {}",
            contents.value
        );
        assert!(!contents.value.contains("__module_"));
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
    fn nested_package_modules_should_keep_their_real_module_identity() {
        let fixture = Fixture::new();
        fixture.write(
            "graphics/reimer.toml",
            "[package]\nname = \"graphics\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
        );
        fixture.write(
            "graphics/src/package.reim",
            "pub import self::raw as raw;\n",
        );
        fixture.write(
            "graphics/src/raw/package.reim",
            "pub import self::types as types;\n\
             pub import self::functions as functions;\n\
             pub import self::dispatch as dispatch;\n",
        );
        let types_source = "import std::c;\n\
                            pub type SDL_AsyncIOTaskType = c::Int;\n\
                            @repr(C) pub struct Task { pub kind: SDL_AsyncIOTaskType }\n";
        let types = fixture.write("graphics/src/raw/types.reim", types_source);
        let functions_source = "import self::types as types;\n\
                                @link(\"native\") extern \"C\" {\n\
                                    pub fn submit(value: types::SDL_AsyncIOTaskType);\n\
                                }\n";
        let functions = fixture.write("graphics/src/raw/functions.reim", functions_source);
        let dispatch_source = "pub type Callback = fn(i32) -> i32;\n\
                               pub fn load(address: *const u8) -> Callback {\n\
                                    unsafe { address as Callback }\n\
                               }\n";
        let dispatch = fixture.write("graphics/src/raw/dispatch.reim", dispatch_source);

        for (path, source) in [
            (types, types_source),
            (functions, functions_source),
            (dispatch, dispatch_source),
        ] {
            let document = Document::new(
                Url::from_file_path(path).expect("file URL should be created"),
                source.to_owned(),
            );
            let errors = document
                .diagnostics()
                .into_iter()
                .filter(|diagnostic| diagnostic.severity == Some(DiagnosticSeverity::ERROR))
                .map(|diagnostic| diagnostic.message)
                .collect::<Vec<_>>();
            assert!(errors.is_empty(), "unexpected diagnostics: {errors:#?}");
        }
    }

    #[test]
    fn standard_slice_document_should_keep_its_trusted_module_identity() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("std")
            .join("slice.reim");
        let source = fs::read_to_string(&path).expect("standard slice module should be readable");
        let document = Document::new(
            Url::from_file_path(path).expect("standard slice URL should be created"),
            source,
        );
        let diagnostics = document
            .diagnostics()
            .into_iter()
            .filter(|diagnostic| {
                matches!(
                    diagnostic.code,
                    Some(NumberOrString::String(ref code)) if code == "E3154" || code == "L2010"
                )
            })
            .map(|diagnostic| diagnostic.message)
            .collect::<Vec<_>>();

        assert!(
            diagnostics.is_empty(),
            "unexpected standard-library diagnostics: {diagnostics:#?}"
        );
    }

    #[test]
    fn standard_library_documents_should_not_report_internal_access_or_ownership_leaks() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("std");
        let mut paths = fs::read_dir(&root)
            .expect("standard library directory should be readable")
            .map(|entry| {
                entry
                    .expect("standard library entry should be readable")
                    .path()
            })
            .filter(|path| {
                path.extension().and_then(|extension| extension.to_str()) == Some("reim")
            })
            .collect::<Vec<_>>();
        paths.sort();
        let mut unexpected = Vec::new();
        for path in paths {
            let source = fs::read_to_string(&path).expect("standard module should be readable");
            let document = Document::new(
                Url::from_file_path(&path).expect("standard module URL should be created"),
                source,
            );
            for diagnostic in document.diagnostics() {
                let is_internal_access_error = matches!(
                    diagnostic.code,
                    Some(NumberOrString::String(ref code))
                        if matches!(code.as_str(), "E3149" | "E3150" | "E3151" | "E3154")
                );
                let is_false_owner_warning = matches!(
                    diagnostic.code,
                    Some(NumberOrString::String(ref code)) if code == "L2010"
                );
                if is_internal_access_error || is_false_owner_warning {
                    unexpected.push(format!("{}: {}", path.display(), diagnostic.message));
                }
            }
        }

        assert!(
            unexpected.is_empty(),
            "unexpected standard-library diagnostics: {unexpected:#?}"
        );
    }

    #[test]
    fn document_should_show_documentation_for_imported_function_calls() {
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
        fixture.write(
            "math/src/package.reim",
            "/// Returns the canonical demonstration value.\npub fn answer() -> i32 { 42 }\n",
        );
        let call_offset = source
            .rfind("answer()")
            .expect("function call should exist");
        let position = LineIndex::new(Arc::from(source)).position(call_offset);
        let document = Document::new(
            Url::from_file_path(main).expect("file URL should be created"),
            source.to_owned(),
        );

        let hover = document
            .hover(position)
            .expect("imported function call should have hover information");
        let tower_lsp::lsp_types::HoverContents::Markup(contents) = hover.contents else {
            panic!("hover should use markdown");
        };

        assert!(
            contents.value.contains("fn answer() -> i32"),
            "unexpected hover contents: {}",
            contents.value
        );
        assert!(
            contents
                .value
                .contains("Returns the canonical demonstration value.")
        );
        assert!(!contents.value.contains("__module_"));
    }

    #[test]
    fn imgui_example_should_show_safe_facade_documentation_on_hover() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("examples")
            .join("imgui_demo")
            .join("src")
            .join("main.reim");
        let source = fs::read_to_string(&path).expect("Dear ImGui example should be readable");
        let call_offset = source
            .find("SdlOpenGl::create")
            .expect("safe Dear ImGui constructor should exist");
        let position = LineIndex::new(Arc::from(source.as_str())).position(call_offset);
        let document = Document::new(
            Url::from_file_path(path).expect("example URL should be created"),
            source,
        );

        let hover = document
            .hover(position)
            .expect("safe Dear ImGui constructor should have hover information");
        let tower_lsp::lsp_types::HoverContents::Markup(contents) = hover.contents else {
            panic!("hover should use markdown");
        };

        assert!(
            contents
                .value
                .contains("SdlOpenGl::create(gl_context: &gl::GlContext)"),
            "unexpected hover contents: {}",
            contents.value
        );
        assert!(
            contents
                .value
                .contains("Creates a Dear ImGui context and initializes the official SDL3")
        );
        assert!(!contents.value.contains("__module_"));
    }

    #[test]
    fn generated_imgui_completion_should_include_upstream_documentation() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("vendor")
            .join("imgui")
            .join("src")
            .join("raw")
            .join("functions.reim");
        let source =
            fs::read_to_string(&path).expect("generated Dear ImGui API should be readable");
        let document = Document::new(
            Url::from_file_path(path).expect("generated API URL should be created"),
            source,
        );
        let completion = document
            .completions()
            .into_iter()
            .find(|item| item.label == "ImGui_ShowDemoWindow")
            .expect("Dear ImGui demo function should be completed");
        let Some(tower_lsp::lsp_types::Documentation::MarkupContent(documentation)) =
            completion.documentation
        else {
            panic!("completion documentation should use markdown");
        };

        assert!(
            documentation
                .value
                .contains("create Demo window. demonstrate most ImGui features")
        );
    }

    #[test]
    fn document_should_show_documentation_for_imported_generic_struct_values() {
        let fixture = Fixture::new();
        fixture.write(
            "app/reimer.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n\
             [dependencies]\nmodels = { path = \"../models\" }\n",
        );
        let source = "from models import Documented;\n\
                      fn main() -> i32 {\n\
                          let item: Documented<i32> = Documented { value: 42 };\n\
                          item.value\n\
                      }\n";
        let main = fixture.write("app/src/main.reim", source);
        fixture.write(
            "models/reimer.toml",
            "[package]\nname = \"models\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
        );
        fixture.write(
            "models/src/package.reim",
            "/// Carries a documented value across a package boundary.\n\
             pub struct Documented<T> { pub value: T }\n",
        );
        let use_offset = source.rfind("item.value").expect("local use should exist");
        let position = LineIndex::new(Arc::from(source)).position(use_offset);
        let document = Document::new(
            Url::from_file_path(main).expect("file URL should be created"),
            source.to_owned(),
        );

        let hover = document
            .hover(position)
            .expect("imported generic struct value should have hover information");
        let tower_lsp::lsp_types::HoverContents::Markup(contents) = hover.contents else {
            panic!("hover should use markdown");
        };

        assert!(
            contents.value.contains("Documented<i32>"),
            "unexpected hover contents: {}",
            contents.value
        );
        assert!(
            contents
                .value
                .contains("Carries a documented value across a package boundary.")
        );
        assert!(!contents.value.contains("__module_"));
    }

    #[test]
    fn document_should_show_documentation_for_imported_statics() {
        let fixture = Fixture::new();
        fixture.write(
            "app/reimer.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n\
             [dependencies]\nvalues = { path = \"../values\" }\n",
        );
        let source = "from values import ANSWER;\nfn main() -> i32 { ANSWER }\n";
        let main = fixture.write("app/src/main.reim", source);
        fixture.write(
            "values/reimer.toml",
            "[package]\nname = \"values\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
        );
        fixture.write(
            "values/src/package.reim",
            "/// Stores a shared immutable answer.\npub static ANSWER: i32 = 42;\n",
        );
        let use_offset = source.rfind("ANSWER").expect("static use should exist");
        let position = LineIndex::new(Arc::from(source)).position(use_offset);
        let document = Document::new(
            Url::from_file_path(main).expect("file URL should be created"),
            source.to_owned(),
        );

        let hover = document
            .hover(position)
            .expect("imported static should have hover information");
        let tower_lsp::lsp_types::HoverContents::Markup(contents) = hover.contents else {
            panic!("hover should use markdown");
        };

        assert!(
            contents.value.contains("pub static ANSWER: i32"),
            "unexpected hover contents: {}",
            contents.value
        );
        assert!(contents.value.contains("Stores a shared immutable answer."));
        assert!(!contents.value.contains("__module_"));
    }

    #[test]
    fn document_should_show_standard_library_documentation() {
        let source = "from std::io import println;\nfn main() -> i32 { println(\"hello\"); 0 }\n";
        let fixture = Fixture::new();
        let main = fixture.write("main.reim", source);
        let call_offset = source
            .rfind("println(\"hello\")")
            .expect("standard output call should exist");
        let position = LineIndex::new(Arc::from(source)).position(call_offset);
        let document = Document::new(
            Url::from_file_path(main).expect("file URL should be created"),
            source.to_owned(),
        );

        let hover = document
            .hover(position)
            .expect("standard output call should have hover information");
        let tower_lsp::lsp_types::HoverContents::Markup(contents) = hover.contents else {
            panic!("hover should use markdown");
        };

        assert!(contents.value.contains("fn std::io::println"));
        assert!(
            contents
                .value
                .contains("Writes all UTF-8 bytes to standard output and appends a newline.")
        );
    }

    #[test]
    fn document_should_show_typed_target_documentation() {
        let source = "from std::target import OperatingSystem, os;
fn main() -> i32 {
    let current = os();
    match current {
        OperatingSystem::Windows => 42,
        OperatingSystem::Linux => 42,
        OperatingSystem::MacOs => 42,
        OperatingSystem::FreeBsd => 42,
        OperatingSystem::Other => 42,
    }
}
";
        let fixture = Fixture::new();
        let main = fixture.write("main.reim", source);
        let lines = LineIndex::new(Arc::from(source));
        let document = Document::new(
            Url::from_file_path(main).expect("file URL should be created"),
            source.to_owned(),
        );
        let call_position = lines.position(source.find("os()").expect("target call should exist"));
        let binding_position =
            lines.position(source.find("current =").expect("binding should exist"));

        let call_hover = document
            .hover(call_position)
            .expect("target call should have hover information");
        let binding_hover = document
            .hover(binding_position)
            .expect("target binding should have hover information");
        let tower_lsp::lsp_types::HoverContents::Markup(call_contents) = call_hover.contents else {
            panic!("target call hover should use markdown");
        };
        let tower_lsp::lsp_types::HoverContents::Markup(binding_contents) = binding_hover.contents
        else {
            panic!("target binding hover should use markdown");
        };

        assert!(call_contents.value.contains("fn std::target::os"));
        assert!(
            call_contents
                .value
                .contains("Returns the operating system targeted by the current native build.")
        );
        assert!(binding_contents.value.contains("OperatingSystem"));
        assert!(!binding_contents.value.contains("__module_"));
    }

    #[test]
    fn document_should_show_time_signatures_and_documentation() {
        let source = "from std::time import Duration, Instant, sleep;
fn main() -> i32 {
    let duration = Duration::from_milliseconds(1);
    let started = Instant::now();
    sleep(duration);
    let elapsed = started.elapsed();
    if elapsed.as_milliseconds() >= 1 { 42 } else { 0 }
}
";
        let fixture = Fixture::new();
        let main = fixture.write("main.reim", source);
        let lines = LineIndex::new(Arc::from(source));
        let document = Document::new(
            Url::from_file_path(main).expect("file URL should be created"),
            source.to_owned(),
        );
        let call_position =
            lines.position(source.find("sleep(duration").expect("sleep should exist"));
        let binding_position = lines.position(
            source
                .find("duration =")
                .expect("duration binding should exist"),
        );

        let call_hover = document
            .hover(call_position)
            .expect("sleep call should have hover information");
        let binding_hover = document
            .hover(binding_position)
            .expect("duration binding should have hover information");
        let tower_lsp::lsp_types::HoverContents::Markup(call_contents) = call_hover.contents else {
            panic!("sleep call hover should use markdown");
        };
        let tower_lsp::lsp_types::HoverContents::Markup(binding_contents) = binding_hover.contents
        else {
            panic!("duration binding hover should use markdown");
        };

        assert!(call_contents.value.contains("fn std::time::sleep"));
        assert!(
            call_contents
                .value
                .contains("Blocks the current native thread")
        );
        assert!(binding_contents.value.contains("Duration"));
        assert!(
            binding_contents
                .value
                .contains("A non-negative span of time with nanosecond precision.")
        );
        assert!(!binding_contents.value.contains("__module_"));
    }

    #[test]
    fn document_should_show_environment_and_process_apis() {
        let source = "from std::env import args;
from std::process import Command;
fn main() -> i32 {
    let arguments = args();
    let command = Command::new(\"echo\");
    match command {
        Ok(command) => {
            command.deinit();
            arguments.len() as i32
        },
        Err(_) => 0,
    }
}
";
        let fixture = Fixture::new();
        let main = fixture.write("main.reim", source);
        let lines = LineIndex::new(Arc::from(source));
        let document = Document::new(
            Url::from_file_path(main).expect("file URL should be created"),
            source.to_owned(),
        );
        let args_position = lines.position(source.find("args()").expect("args call should exist"));
        let command_position = lines.position(
            source
                .find("Command::new")
                .expect("command constructor should exist"),
        );

        let args_hover = document
            .hover(args_position)
            .expect("args call should have hover information");
        let command_hover = document
            .hover(command_position)
            .expect("command constructor should have hover information");
        let tower_lsp::lsp_types::HoverContents::Markup(args_contents) = args_hover.contents else {
            panic!("args hover should use markdown");
        };
        let tower_lsp::lsp_types::HoverContents::Markup(command_contents) = command_hover.contents
        else {
            panic!("command hover should use markdown");
        };

        assert!(
            args_contents
                .value
                .contains("fn std::env::args() -> std::env::Arguments"),
            "{}",
            args_contents.value
        );
        assert!(
            args_contents
                .value
                .contains("lightweight view over all command-line arguments")
        );
        assert!(
            command_contents.value.contains(
                "fn std::process::Command::new(program: str) -> \
                     Result<std::process::Command, std::process::ProcessError>"
            ),
            "{}",
            command_contents.value
        );
        assert!(
            command_contents
                .value
                .contains("invokes one executable directly")
        );
        assert!(!command_contents.value.contains("__module_"));

        let completions = document.completions();
        assert!(completions.iter().any(|item| {
            item.label == "Command" && item.detail.as_deref() == Some("std::process")
        }));
        assert!(
            completions
                .iter()
                .any(|item| { item.label == "args" && item.detail.as_deref() == Some("std::env") })
        );
    }

    #[test]
    fn document_should_show_safe_filesystem_documentation() {
        let source = "from std::fs import FileError, open;
fn inspect() -> Result<(), FileError> {
    let file = open(\"missing.txt\")?;
    file.deinit();
    Ok(())
}
fn main() -> i32 { 0 }
";
        let fixture = Fixture::new();
        let main = fixture.write("main.reim", source);
        let lines = LineIndex::new(Arc::from(source));
        let document = Document::new(
            Url::from_file_path(main).expect("file URL should be created"),
            source.to_owned(),
        );
        let call_position =
            lines.position(source.find("open(\"").expect("file open call should exist"));
        let binding_position =
            lines.position(source.find("file =").expect("file binding should exist"));

        let call_hover = document
            .hover(call_position)
            .expect("file open call should have hover information");
        let binding_hover = document
            .hover(binding_position)
            .expect("file binding should have hover information");
        let tower_lsp::lsp_types::HoverContents::Markup(call_contents) = call_hover.contents else {
            panic!("file call hover should use markdown");
        };
        let tower_lsp::lsp_types::HoverContents::Markup(binding_contents) = binding_hover.contents
        else {
            panic!("file binding hover should use markdown");
        };

        assert!(call_contents.value.contains("fn std::fs::open"));
        assert!(
            call_contents
                .value
                .contains("Opens an existing UTF-8 path for reading.")
        );
        assert!(binding_contents.value.contains("File"));
        assert!(!binding_contents.value.contains("__module_"));
    }

    #[test]
    fn document_should_show_scalar_and_vector_math_documentation() {
        let source = "from std::math import Vec3, square_root;
fn main() -> i32 {
    let direction = Vec3::new(3.0, 4.0, 0.0);
    let length = square_root(81.0);
    if direction.length() + length == 14.0 { 42 } else { 0 }
}
";
        let fixture = Fixture::new();
        let main = fixture.write("main.reim", source);
        let lines = LineIndex::new(Arc::from(source));
        let document = Document::new(
            Url::from_file_path(main).expect("file URL should be created"),
            source.to_owned(),
        );
        let call_position = lines.position(
            source
                .find("square_root(81")
                .expect("math call should exist"),
        );
        let binding_position = lines.position(
            source
                .find("direction =")
                .expect("vector binding should exist"),
        );

        let call_hover = document
            .hover(call_position)
            .expect("math call should have hover information");
        let binding_hover = document
            .hover(binding_position)
            .expect("vector binding should have hover information");
        let tower_lsp::lsp_types::HoverContents::Markup(call_contents) = call_hover.contents else {
            panic!("math call hover should use markdown");
        };
        let tower_lsp::lsp_types::HoverContents::Markup(binding_contents) = binding_hover.contents
        else {
            panic!("vector binding hover should use markdown");
        };

        assert!(call_contents.value.contains("fn std::math::square_root"));
        assert!(
            call_contents
                .value
                .contains("Returns the principal square root")
        );
        assert!(binding_contents.value.contains("Vec3"));
        assert!(
            binding_contents
                .value
                .contains("Three-dimensional single-precision vector.")
        );
        assert!(!binding_contents.value.contains("__module_"));
    }

    #[test]
    fn document_should_show_text_documentation_and_allocator_estimates() {
        let source = "from std::alloc import AllocError, general_allocator;
from std::string import String, concat;
fn build() -> Result<String, AllocError> {
    let allocator = general_allocator();
    concat(&allocator, \"hello\", \" world\")
}
fn main() -> i32 { 0 }
";
        let fixture = Fixture::new();
        let main = fixture.write("main.reim", source);
        let lines = LineIndex::new(Arc::from(source));
        let document = Document::new(
            Url::from_file_path(main).expect("file URL should be created"),
            source.to_owned(),
        );
        let call_position = lines.position(
            source
                .find("concat(&allocator")
                .expect("concatenation call should exist"),
        );

        let hover = document
            .hover(call_position)
            .expect("concatenation call should have hover information");
        let tower_lsp::lsp_types::HoverContents::Markup(contents) = hover.contents else {
            panic!("text call hover should use markdown");
        };

        assert!(contents.value.contains("fn std::string::concat"));
        assert!(
            contents
                .value
                .contains("Allocates one string containing `left` followed by `right`.")
        );
        assert!(contents.value.contains("11 B reserved"));
    }

    #[test]
    fn document_should_understand_interpolated_values_and_destination_growth() {
        let source = "from std::alloc import AllocError, general_allocator;
from std::string import String;
fn build() -> Result<i32, AllocError> {
    let allocator = general_allocator();
    let score = 42;
    let mut message = String::with_capacity(&allocator, 32)?;
    message.push_format(f\"score={score:?}\")?;
    message.deinit();
    Ok(score)
}
fn main() -> i32 { 0 }
";
        let fixture = Fixture::new();
        let main = fixture.write("main.reim", source);
        let lines = LineIndex::new(Arc::from(source));
        let document = Document::new(
            Url::from_file_path(main).expect("file URL should be created"),
            source.to_owned(),
        );
        let value_position = lines.position(
            source
                .find("{score:?}")
                .expect("formatted value should exist")
                .saturating_add(1),
        );
        let call_position = lines.position(
            source
                .find("push_format")
                .expect("formatting call should exist"),
        );

        let value_hover = document
            .hover(value_position)
            .expect("formatted value should have hover information");
        let call_hover = document
            .hover(call_position)
            .expect("formatting call should have hover information");
        let tower_lsp::lsp_types::HoverContents::Markup(value_contents) = value_hover.contents
        else {
            panic!("formatted value hover should use markdown");
        };
        let tower_lsp::lsp_types::HoverContents::Markup(call_contents) = call_hover.contents else {
            panic!("formatting call hover should use markdown");
        };
        let value_byte = lines
            .byte(value_position)
            .expect("formatted value position should map to a byte");
        let value_candidates = document
            .analysis
            .type_hints
            .iter()
            .filter(|hint| value_byte >= hint.span.start && value_byte <= hint.span.end)
            .collect::<Vec<_>>();

        assert!(
            value_contents.value.contains("i32"),
            "unexpected hover: {}; candidates: {value_candidates:?}",
            value_contents.value,
        );
        assert!(value_contents.value.contains("32-bit signed integer"));
        assert!(call_contents.value.contains("String::push_format"));
        assert!(call_contents.value.contains("runtime-sized reservation"));
        assert!(
            call_contents
                .value
                .contains("allocator retained by String `message`")
        );
    }

    #[test]
    fn document_should_show_target_correct_c_helper_documentation() {
        let source = "from std::c import Int, int_from_bool;
fn main() -> i32 {
    let code: Int = int_from_bool(true);
    code
}
";
        let fixture = Fixture::new();
        let main = fixture.write("main.reim", source);
        let lines = LineIndex::new(Arc::from(source));
        let document = Document::new(
            Url::from_file_path(main).expect("file URL should be created"),
            source.to_owned(),
        );
        let call_position = lines.position(
            source
                .find("int_from_bool(true")
                .expect("C helper call should exist"),
        );
        let binding_position =
            lines.position(source.find("code:").expect("C alias binding should exist"));

        let call_hover = document
            .hover(call_position)
            .expect("C helper call should have hover information");
        let binding_hover = document
            .hover(binding_position)
            .expect("C alias binding should have hover information");
        let tower_lsp::lsp_types::HoverContents::Markup(call_contents) = call_hover.contents else {
            panic!("C helper call hover should use markdown");
        };
        let tower_lsp::lsp_types::HoverContents::Markup(binding_contents) = binding_hover.contents
        else {
            panic!("C alias binding hover should use markdown");
        };

        assert!(call_contents.value.contains("fn std::c::int_from_bool"));
        assert!(
            call_contents
                .value
                .contains("conventional C integer representation")
        );
        assert!(binding_contents.value.contains("i32"));
        assert!(!binding_contents.value.contains("__module_"));
        assert!(
            document
                .completions()
                .iter()
                .any(|completion| completion.label == "Int")
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
