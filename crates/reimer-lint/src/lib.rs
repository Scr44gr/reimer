//! Static analysis used by command-line and editor tooling.
//!
//! The crate deliberately consumes the compiler's public lexer, parser, and
//! resolver APIs. Editor diagnostics therefore follow the same language rules
//! as a normal build instead of maintaining a second grammar or type checker.

mod imports;
mod memory;
mod semantic;
mod syntax;
mod walk;

use reimer_ast as ast;
use reimer_diagnostics::{Diagnostic, Span};
use reimer_hir as hir;

pub use imports::organize_imports;
pub use memory::{AllocationEstimate, AllocationQuantity, AllocatorSummary};
pub use semantic::{DefinitionLink, TypeHint};

/// Importance assigned to one editor finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Compilation cannot continue.
    Error,
    /// Code compiles, but a likely bug or costly pattern was found.
    Warning,
    /// Additional static information that does not imply a problem.
    Information,
    /// A low-priority style improvement.
    Hint,
}

/// One source replacement offered as a safe automatic correction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fix {
    /// Human-readable action title.
    pub title: String,
    /// Source range to replace.
    pub span: Span,
    /// Replacement text.
    pub replacement: String,
}

/// One compiler error, lint warning, or informational estimate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Stable diagnostic identifier.
    pub code: String,
    /// Diagnostic importance.
    pub severity: Severity,
    /// Human-readable summary.
    pub message: String,
    /// Primary source range.
    pub span: Span,
    /// Optional detailed guidance.
    pub help: Option<String>,
    /// Safe automatic corrections associated with this finding.
    pub fixes: Vec<Fix>,
}

impl Finding {
    /// Converts one compiler diagnostic into an editor finding.
    #[must_use]
    pub fn from_compiler(diagnostic: Diagnostic) -> Self {
        Self {
            code: diagnostic.code.to_owned(),
            severity: Severity::Error,
            message: diagnostic.message,
            span: diagnostic.span,
            help: diagnostic.help,
            fixes: Vec::new(),
        }
    }
}

/// Complete editor analysis for one source snapshot.
#[derive(Debug)]
pub struct Analysis {
    /// Parsed syntax tree when lexing and parsing succeeded.
    pub syntax: Option<ast::Program>,
    /// Typed program when semantic analysis succeeded.
    pub typed: Option<hir::Program>,
    /// Compiler diagnostics and language-specific lints.
    pub findings: Vec<Finding>,
    /// Inferred types associated with the narrowest useful source spans.
    pub type_hints: Vec<TypeHint>,
    /// Local and top-level definition links.
    pub definitions: Vec<DefinitionLink>,
    /// Per-call static allocation estimates.
    pub allocations: Vec<AllocationEstimate>,
    /// Per-function and per-allocator allocation summaries.
    pub allocator_summaries: Vec<AllocatorSummary>,
}

impl Analysis {
    fn failed(findings: Vec<Finding>) -> Self {
        Self {
            syntax: None,
            typed: None,
            findings,
            type_hints: Vec::new(),
            definitions: Vec::new(),
            allocations: Vec::new(),
            allocator_summaries: Vec::new(),
        }
    }
}

/// Analyzes one in-memory source snapshot.
///
/// Lexical, syntactic, and semantic errors use the compiler's canonical
/// diagnostic codes. Lints and estimates are added only after parsing succeeds.
#[must_use]
pub fn analyze(source: &str) -> Analysis {
    let tokens = match reimer_lexer::lex(source) {
        Ok(tokens) => tokens,
        Err(diagnostics) => {
            return Analysis::failed(
                diagnostics
                    .into_iter()
                    .map(Finding::from_compiler)
                    .collect(),
            );
        }
    };
    let syntax = match reimer_parser::parse(&tokens) {
        Ok(syntax) => syntax,
        Err(diagnostics) => {
            return Analysis::failed(
                diagnostics
                    .into_iter()
                    .map(Finding::from_compiler)
                    .collect(),
            );
        }
    };

    let mut findings = syntax::lint(source, &syntax);
    let (allocations, allocator_summaries) = memory::estimate(&syntax);
    let has_imports = syntax
        .items
        .iter()
        .any(|item| matches!(item, ast::Item::Import(_)));
    let typed = if has_imports {
        None
    } else {
        match reimer_resolver::resolve(&syntax) {
            Ok(program) => Some(program),
            Err(diagnostics) => {
                findings.extend(diagnostics.into_iter().map(Finding::from_compiler));
                None
            }
        }
    };
    apply_spelling_fixes(source, &syntax, &mut findings);

    let (type_hints, definitions) = typed.as_ref().map_or_else(
        || (Vec::new(), semantic::syntax_definitions(&syntax)),
        |program| semantic::index(&syntax, program),
    );

    Analysis {
        syntax: Some(syntax),
        typed,
        findings,
        type_hints,
        definitions,
        allocations,
        allocator_summaries,
    }
}

/// Rebuilds hover and definition indexes from a package-resolved typed program.
///
/// This is useful to editor hosts that resolve a saved multi-file package
/// after first analyzing the in-memory entry document.
#[must_use]
pub fn index_typed(
    syntax: &ast::Program,
    typed: &hir::Program,
) -> (Vec<TypeHint>, Vec<DefinitionLink>) {
    semantic::index(syntax, typed)
}

/// Adds close-name corrections to unresolved compiler diagnostics.
pub fn apply_spelling_fixes(source: &str, program: &ast::Program, findings: &mut [Finding]) {
    syntax::attach_spelling_fixes(source, program, findings);
}

#[cfg(test)]
mod tests {
    use super::{AllocationQuantity, Severity, analyze};

    #[test]
    fn analyze_should_report_the_inferred_type_of_an_unannotated_binding() {
        let source = "fn main() -> i32 { let answer = 42; answer }";

        let analysis = analyze(source);

        assert!(
            analysis
                .type_hints
                .iter()
                .any(|hint| hint.label == "i32"
                    && &source[hint.span.start..hint.span.end] == "answer"),
            "available findings: {:?}",
            analysis.findings
        );
    }

    #[test]
    fn analyze_should_suggest_a_close_local_name() {
        let analysis = analyze("fn main() -> i32 { let answer = 42; anser }");

        assert!(analysis.findings.iter().any(|finding| {
            finding.severity == Severity::Error
                && finding.fixes.iter().any(|fix| fix.replacement == "answer")
        }));
    }

    #[test]
    fn analyze_should_estimate_constant_allocator_reservations() {
        let source = "fn main() -> i32 { let bytes = allocate_bytes(&allocator, 64); 0 }";

        let analysis = analyze(source);

        assert!(analysis.allocations.iter().any(|estimate| {
            estimate.quantity == AllocationQuantity::Exact(64)
                && estimate.operation == "allocate_bytes"
        }));
    }
}
