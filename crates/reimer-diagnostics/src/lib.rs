//! Source spans and text diagnostics shared by the Reimer frontend.

/// A half-open UTF-8 byte range in a source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    /// Inclusive start byte.
    pub start: usize,
    /// Exclusive end byte.
    pub end: usize,
}

impl Span {
    /// Creates a span and normalizes an inverted end position.
    #[must_use]
    pub const fn new(start: usize, end: usize) -> Self {
        Self {
            start,
            end: if end < start { start } else { end },
        }
    }

    /// Creates an empty span at a byte position.
    #[must_use]
    pub const fn empty(position: usize) -> Self {
        Self::new(position, position)
    }
}

/// A compiler diagnostic with one primary source location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Stable diagnostic identifier.
    pub code: &'static str,
    /// Human-readable summary.
    pub message: String,
    /// Primary source range.
    pub span: Span,
    /// Optional actionable suggestion.
    pub help: Option<String>,
}

impl Diagnostic {
    /// Creates an error diagnostic.
    #[must_use]
    pub fn error(code: &'static str, message: impl Into<String>, span: Span) -> Self {
        Self {
            code,
            message: message.into(),
            span,
            help: None,
        }
    }

    /// Attaches an actionable suggestion.
    #[must_use]
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    /// Renders the diagnostic using one-based line and column numbers.
    #[must_use]
    pub fn render(&self, source_name: &str, source: &str) -> String {
        let bounded_start = self.span.start.min(source.len());
        let (line_index, line_start) = line_index_and_start(source, bounded_start);
        let line_end = source[line_start..]
            .find('\n')
            .map_or(source.len(), |offset| line_start + offset);
        let line = source[line_start..line_end].trim_end_matches('\r');
        let column = source[line_start..bounded_start].chars().count();
        let span_len = self
            .span
            .end
            .min(line_end)
            .saturating_sub(bounded_start)
            .max(1);
        let marker_len = source[bounded_start..bounded_start + span_len]
            .chars()
            .count()
            .max(1);
        let gutter_width = (line_index + 1).to_string().len();
        let mut rendered = format!(
            "error[{}]: {}\n --> {source_name}:{}:{}\n{:>gutter_width$} |\n{:>gutter_width$} | {line}\n{:>gutter_width$} | {}{}\n",
            self.code,
            self.message,
            line_index + 1,
            column + 1,
            "",
            line_index + 1,
            "",
            " ".repeat(column),
            "^".repeat(marker_len)
        );

        if let Some(help) = &self.help {
            rendered.push_str(" help: ");
            rendered.push_str(help);
            rendered.push('\n');
        }

        rendered
    }
}

fn line_index_and_start(source: &str, byte_index: usize) -> (usize, usize) {
    let prefix = &source[..byte_index];
    let line_index = prefix.bytes().filter(|byte| *byte == b'\n').count();
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    (line_index, line_start)
}

#[cfg(test)]
mod tests {
    use super::{Diagnostic, Span};

    #[test]
    fn render_should_include_source_location_and_help() {
        let diagnostic = Diagnostic::error("E0001", "unexpected character", Span::new(14, 15))
            .with_help("remove it");

        let rendered = diagnostic.render("main.reim", "fn main() {\n  @\n}\n");

        assert!(rendered.contains("main.reim:2:3\n"));
    }

    #[test]
    fn render_should_mark_unicode_by_character_column() {
        let source = "let café = @;\n";
        let at = source.find('@').expect("fixture contains @");
        let diagnostic = Diagnostic::error("E0001", "unexpected character", Span::new(at, at + 1));

        let rendered = diagnostic.render("main.reim", source);

        assert!(rendered.contains("main.reim:1:12\n"));
    }
}
