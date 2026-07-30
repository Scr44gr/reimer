use reimer_ast::{ImportDeclaration, ImportKind, Item, Program};
use reimer_diagnostics::Span;

use crate::Fix;

/// Builds a safe whole-import-section replacement in canonical order.
///
/// Imports from `std` are grouped first. Remaining module paths follow in
/// lexical order, and selective imported names are sorted within each
/// declaration. The edit is withheld when comments occur inside the import
/// section so organizing cannot silently detach or delete them.
#[must_use]
pub fn organize_imports(source: &str, program: &Program) -> Option<Fix> {
    let imports: Vec<_> = program
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Import(import) => Some(import),
            _ => None,
        })
        .collect();
    let first = imports.first()?;
    let last = imports.last()?;
    let span = line_bounded_span(source, first.span.start, last.span.end);
    let original = source.get(span.start..span.end)?;
    if original.contains("//") || original.contains("/*") {
        return None;
    }

    let mut normalized: Vec<_> = imports.iter().map(|import| render(import)).collect();
    normalized.sort_by(|left, right| {
        let left_std = left.path.starts_with("std::") || left.path == "std";
        let right_std = right.path.starts_with("std::") || right.path == "std";
        right_std
            .cmp(&left_std)
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.text.cmp(&right.text))
    });
    normalized.dedup_by(|left, right| left.text == right.text);

    let mut replacement = String::new();
    let mut previous_std = None;
    for entry in normalized {
        let is_std = entry.path.starts_with("std::") || entry.path == "std";
        if previous_std == Some(true) && !is_std {
            replacement.push('\n');
        }
        replacement.push_str(&entry.text);
        replacement.push('\n');
        previous_std = Some(is_std);
    }
    if !original.ends_with('\n') && !original.ends_with("\r\n") {
        replacement.pop();
    }
    if normalize_newlines(original) == normalize_newlines(&replacement) {
        return None;
    }

    Some(Fix {
        title: "Organize imports".to_owned(),
        span,
        replacement,
    })
}

struct RenderedImport {
    path: String,
    text: String,
}

fn render(import: &ImportDeclaration) -> RenderedImport {
    let visibility = if import.is_public { "pub " } else { "" };
    match &import.kind {
        ImportKind::Module { path, alias } => {
            let path_text = path.display();
            let alias = alias
                .as_ref()
                .map_or_else(String::new, |alias| format!(" as {}", alias.name));
            RenderedImport {
                path: path_text.clone(),
                text: format!("{visibility}import {path_text}{alias};"),
            }
        }
        ImportKind::Symbols { module, names } => {
            let path = module.display();
            let mut names: Vec<_> = names
                .iter()
                .map(|name| {
                    name.alias.as_ref().map_or_else(
                        || name.name.name.clone(),
                        |alias| format!("{} as {}", name.name.name, alias.name),
                    )
                })
                .collect();
            names.sort();
            names.dedup();
            RenderedImport {
                path: path.clone(),
                text: format!("{visibility}from {path} import {};", names.join(", ")),
            }
        }
    }
}

fn line_bounded_span(source: &str, start: usize, end: usize) -> Span {
    let line_start = source
        .get(..start)
        .and_then(|prefix| prefix.rfind('\n').map(|index| index + 1))
        .unwrap_or(0);
    let line_end = source
        .get(end..)
        .and_then(|suffix| suffix.find('\n').map(|offset| end + offset + 1))
        .unwrap_or(source.len());
    Span::new(line_start, line_end)
}

fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n")
}

#[cfg(test)]
mod tests {
    use reimer_lexer::lex;
    use reimer_parser::parse;

    use super::organize_imports;

    #[test]
    fn organize_imports_should_sort_std_first_and_selective_names() {
        let source = "from game::world import z, a;\nimport std::io;\nfn main() {}\n";
        let tokens = lex(source).expect("fixture should lex");
        let syntax = parse(&tokens).expect("fixture should parse");

        let edit = organize_imports(source, &syntax).expect("imports should need organizing");

        assert_eq!(
            edit.replacement,
            "import std::io;\n\nfrom game::world import a, z;\n"
        );
    }

    #[test]
    fn organize_imports_should_preserve_commented_sections_by_declining_an_edit() {
        let source = "import b; // keep this context\nimport a;\nfn main() {}\n";
        let tokens = lex(source).expect("fixture should lex");
        let syntax = parse(&tokens).expect("fixture should parse");

        let edit = organize_imports(source, &syntax);

        assert!(edit.is_none());
    }
}
