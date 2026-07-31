//! Tokenization for Reimer source files.

use reimer_diagnostics::{Diagnostic, Span};

/// A lexical token and its byte span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// Token category and payload.
    pub kind: TokenKind,
    /// Location in the original source.
    pub span: Span,
}

/// One literal or expression fragment inside an interpolated string token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormattedStringFragment {
    /// Decoded UTF-8 text between interpolations.
    Text {
        /// Decoded text.
        value: String,
        /// Source range occupied by the raw fragment.
        span: Span,
    },
    /// A complete token stream parsed from one `{ expression }` placeholder.
    Expression {
        /// Tokens with spans shifted into the containing source file.
        tokens: Vec<Token>,
        /// Source range inside the braces.
        span: Span,
    },
}

/// Token categories recognized by the current lexer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    /// `fn`
    Fn,
    /// `return`
    Return,
    /// `from`
    From,
    /// `import`
    Import,
    /// `as`
    As,
    /// `pub`
    Pub,
    /// `let`
    Let,
    /// `mut`
    Mut,
    /// `if`
    If,
    /// `else`
    Else,
    /// `while`
    While,
    /// `struct`
    Struct,
    /// `enum`
    Enum,
    /// `type`
    Type,
    /// `match`
    Match,
    /// `loop`
    Loop,
    /// `for`
    For,
    /// `in`
    In,
    /// `break`
    Break,
    /// `continue`
    Continue,
    /// `defer`
    Defer,
    /// `const`
    Const,
    /// `static`
    Static,
    /// `comptime`
    Comptime,
    /// `unsafe`
    Unsafe,
    /// `extern`
    Extern,
    /// `impl`
    Impl,
    /// `trait`
    Trait,
    /// `where`
    Where,
    /// `true`
    True,
    /// `false`
    False,
    /// User or predefined name.
    Identifier(String),
    /// Decimal integer spelling, including `_` separators.
    Integer(String),
    /// Decimal floating-point spelling.
    Float(String),
    /// One decoded Unicode scalar literal.
    Character(char),
    /// One decoded UTF-8 string literal.
    String(String),
    /// An interpolated UTF-8 string literal beginning with `f"`.
    FormattedString(Vec<FormattedStringFragment>),
    /// One decoded UTF-8 C string literal without its implicit terminator.
    CString(String),
    /// `->`
    Arrow,
    /// `=>`
    FatArrow,
    /// `::`
    ColonColon,
    /// `:`
    Colon,
    /// `(`
    LeftParen,
    /// `)`
    RightParen,
    /// `{`
    LeftBrace,
    /// `}`
    RightBrace,
    /// `[`
    LeftBracket,
    /// `]`
    RightBracket,
    /// `;`
    Semicolon,
    /// `,`
    Comma,
    /// `@`
    At,
    /// `=`
    Equal,
    /// `+`
    Plus,
    /// `-`
    Minus,
    /// `*`
    Star,
    /// `/`
    Slash,
    /// `%`
    Percent,
    /// `&`
    Ampersand,
    /// `|`
    Pipe,
    /// `^`
    Caret,
    /// `<<`
    LeftShift,
    /// `>>`
    RightShift,
    /// `.`
    Dot,
    /// `!`
    Bang,
    /// `?`
    Question,
    /// `==`
    EqualEqual,
    /// `!=`
    BangEqual,
    /// `<`
    Less,
    /// `<=`
    LessEqual,
    /// `>`
    Greater,
    /// `>=`
    GreaterEqual,
    /// `&&`
    AmpAmp,
    /// `||`
    PipePipe,
    /// `+=`
    PlusEqual,
    /// `-=`
    MinusEqual,
    /// `*=`
    StarEqual,
    /// `/=`
    SlashEqual,
    /// `%=`
    PercentEqual,
    /// `&=`
    AmpersandEqual,
    /// `|=`
    PipeEqual,
    /// `^=`
    CaretEqual,
    /// `<<=`
    LeftShiftEqual,
    /// `>>=`
    RightShiftEqual,
    /// Synthetic final token.
    Eof,
}

/// Tokenizes a UTF-8 source file.
///
/// All invalid characters are reported in a single pass when possible.
///
/// # Errors
///
/// Returns diagnostics for unexpected characters and unterminated block
/// comments.
pub fn lex(source: &str) -> Result<Vec<Token>, Vec<Diagnostic>> {
    Lexer::new(source).lex()
}

struct Lexer<'source> {
    source: &'source str,
    cursor: usize,
    tokens: Vec<Token>,
    diagnostics: Vec<Diagnostic>,
}

impl<'source> Lexer<'source> {
    fn new(source: &'source str) -> Self {
        Self {
            source,
            cursor: 0,
            tokens: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    fn lex(mut self) -> Result<Vec<Token>, Vec<Diagnostic>> {
        while self.cursor < self.source.len() {
            let start = self.cursor;
            let Some(character) = self.advance_char() else {
                break;
            };

            match character {
                character if character.is_whitespace() => {}
                '/' if self.take_char('/') => self.skip_line_comment(),
                '/' if self.take_char('*') => self.skip_block_comment(start),
                '/' if self.take_char('=') => self.push(TokenKind::SlashEqual, start),
                '/' => self.push(TokenKind::Slash, start),
                '-' if self.take_char('>') => self.push(TokenKind::Arrow, start),
                '-' if self.take_char('=') => self.push(TokenKind::MinusEqual, start),
                '-' => self.push(TokenKind::Minus, start),
                ':' if self.take_char(':') => self.push(TokenKind::ColonColon, start),
                ':' => self.push(TokenKind::Colon, start),
                '=' if self.take_char('=') => self.push(TokenKind::EqualEqual, start),
                '=' if self.take_char('>') => self.push(TokenKind::FatArrow, start),
                '=' => self.push(TokenKind::Equal, start),
                '!' if self.take_char('=') => self.push(TokenKind::BangEqual, start),
                '!' => self.push(TokenKind::Bang, start),
                '?' => self.push(TokenKind::Question, start),
                '<' if self.take_char('<') => {
                    if self.take_char('=') {
                        self.push(TokenKind::LeftShiftEqual, start);
                    } else {
                        self.push(TokenKind::LeftShift, start);
                    }
                }
                '<' if self.take_char('=') => self.push(TokenKind::LessEqual, start),
                '<' => self.push(TokenKind::Less, start),
                '>' if self.take_char('>') => {
                    if self.take_char('=') {
                        self.push(TokenKind::RightShiftEqual, start);
                    } else {
                        self.push(TokenKind::RightShift, start);
                    }
                }
                '>' if self.take_char('=') => self.push(TokenKind::GreaterEqual, start),
                '>' => self.push(TokenKind::Greater, start),
                '&' if self.take_char('&') => self.push(TokenKind::AmpAmp, start),
                '&' if self.take_char('=') => self.push(TokenKind::AmpersandEqual, start),
                '&' => self.push(TokenKind::Ampersand, start),
                '|' if self.take_char('|') => self.push(TokenKind::PipePipe, start),
                '|' if self.take_char('=') => self.push(TokenKind::PipeEqual, start),
                '|' => self.push(TokenKind::Pipe, start),
                '^' if self.take_char('=') => self.push(TokenKind::CaretEqual, start),
                '^' => self.push(TokenKind::Caret, start),
                '+' if self.take_char('=') => self.push(TokenKind::PlusEqual, start),
                '+' => self.push(TokenKind::Plus, start),
                '*' if self.take_char('=') => self.push(TokenKind::StarEqual, start),
                '*' => self.push(TokenKind::Star, start),
                '%' if self.take_char('=') => self.push(TokenKind::PercentEqual, start),
                '%' => self.push(TokenKind::Percent, start),
                '.' => self.push(TokenKind::Dot, start),
                '(' => self.push(TokenKind::LeftParen, start),
                ')' => self.push(TokenKind::RightParen, start),
                '{' => self.push(TokenKind::LeftBrace, start),
                '}' => self.push(TokenKind::RightBrace, start),
                '[' => self.push(TokenKind::LeftBracket, start),
                ']' => self.push(TokenKind::RightBracket, start),
                ';' => self.push(TokenKind::Semicolon, start),
                ',' => self.push(TokenKind::Comma, start),
                '@' => self.push(TokenKind::At, start),
                '\'' => self.character(start),
                '"' => self.string(start),
                'c' if self.take_char('"') => self.c_string(start),
                'f' if self.take_char('"') => self.formatted_string(start),
                character if is_identifier_start(character) => self.identifier(start),
                character if character.is_ascii_digit() => self.number(start),
                character => self.diagnostics.push(
                    Diagnostic::error(
                        "E0001",
                        format!("unexpected character `{character}`"),
                        Span::new(start, self.cursor),
                    )
                    .with_help("remove the character or replace it with valid Reimer syntax"),
                ),
            }
        }

        self.tokens.push(Token {
            kind: TokenKind::Eof,
            span: Span::empty(self.source.len()),
        });

        if self.diagnostics.is_empty() {
            Ok(self.tokens)
        } else {
            Err(self.diagnostics)
        }
    }

    fn advance_char(&mut self) -> Option<char> {
        let character = self.source.get(self.cursor..)?.chars().next()?;
        self.cursor += character.len_utf8();
        Some(character)
    }

    fn take_char(&mut self, expected: char) -> bool {
        if self.source[self.cursor..].starts_with(expected) {
            self.cursor += expected.len_utf8();
            true
        } else {
            false
        }
    }

    fn skip_line_comment(&mut self) {
        while self.cursor < self.source.len() && !self.source[self.cursor..].starts_with('\n') {
            let _ = self.advance_char();
        }
    }

    fn skip_block_comment(&mut self, start: usize) {
        while self.cursor < self.source.len() {
            if self.source[self.cursor..].starts_with("*/") {
                self.cursor += 2;
                return;
            }
            let _ = self.advance_char();
        }

        self.diagnostics.push(
            Diagnostic::error(
                "E0002",
                "unterminated block comment",
                Span::new(start, self.source.len()),
            )
            .with_help("close the comment with `*/`"),
        );
    }

    fn identifier(&mut self, start: usize) {
        while self.cursor < self.source.len() {
            let Some(character) = self.advance_char() else {
                break;
            };
            if !is_identifier_continue(character) {
                self.cursor -= character.len_utf8();
                break;
            }
        }

        let text = &self.source[start..self.cursor];
        let kind = match text {
            "fn" => TokenKind::Fn,
            "return" => TokenKind::Return,
            "from" => TokenKind::From,
            "import" => TokenKind::Import,
            "as" => TokenKind::As,
            "pub" => TokenKind::Pub,
            "let" => TokenKind::Let,
            "mut" => TokenKind::Mut,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "while" => TokenKind::While,
            "struct" => TokenKind::Struct,
            "enum" => TokenKind::Enum,
            "type" => TokenKind::Type,
            "match" => TokenKind::Match,
            "loop" => TokenKind::Loop,
            "for" => TokenKind::For,
            "in" => TokenKind::In,
            "break" => TokenKind::Break,
            "continue" => TokenKind::Continue,
            "defer" => TokenKind::Defer,
            "const" => TokenKind::Const,
            "static" => TokenKind::Static,
            "comptime" => TokenKind::Comptime,
            "unsafe" => TokenKind::Unsafe,
            "extern" => TokenKind::Extern,
            "impl" => TokenKind::Impl,
            "trait" => TokenKind::Trait,
            "where" => TokenKind::Where,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            identifier => TokenKind::Identifier(identifier.to_owned()),
        };
        self.tokens.push(Token {
            kind,
            span: Span::new(start, self.cursor),
        });
    }

    fn number(&mut self, start: usize) {
        if self.source.as_bytes().get(start) == Some(&b'0')
            && let Some((radix, name)) = self.integer_radix_prefix()
        {
            self.prefixed_integer(start, radix, name);
            return;
        }

        while self.cursor < self.source.len() {
            let byte = self.source.as_bytes()[self.cursor];
            if !byte.is_ascii_digit() && byte != b'_' {
                break;
            }
            self.cursor += 1;
        }
        self.validate_decimal_separators(start, self.cursor);

        let mut is_float = false;
        if self.source.as_bytes().get(self.cursor) == Some(&b'.')
            && self
                .source
                .as_bytes()
                .get(self.cursor + 1)
                .is_some_and(u8::is_ascii_digit)
        {
            is_float = true;
            self.cursor += 1;
            let fraction_start = self.cursor;
            self.scan_decimal_digits();
            self.validate_decimal_separators(fraction_start, self.cursor);
        }
        if matches!(self.source.as_bytes().get(self.cursor), Some(b'e' | b'E')) {
            is_float = true;
            self.cursor += 1;
            if matches!(self.source.as_bytes().get(self.cursor), Some(b'+' | b'-')) {
                self.cursor += 1;
            }
            let exponent_start = self.cursor;
            self.scan_decimal_digits();
            let exponent_has_digit = self.source[exponent_start..self.cursor]
                .bytes()
                .any(|byte| byte.is_ascii_digit());
            if exponent_has_digit {
                self.validate_decimal_separators(exponent_start, self.cursor);
            } else {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E0003",
                        "floating-point exponent requires digits",
                        Span::new(start, self.cursor),
                    )
                    .with_help("add decimal digits after the exponent marker"),
                );
            }
        }

        self.tokens.push(Token {
            kind: if is_float {
                TokenKind::Float(self.source[start..self.cursor].to_owned())
            } else {
                TokenKind::Integer(self.source[start..self.cursor].to_owned())
            },
            span: Span::new(start, self.cursor),
        });
    }

    fn integer_radix_prefix(&mut self) -> Option<(u32, &'static str)> {
        let (radix, name) = match self.source.as_bytes().get(self.cursor).copied()? {
            b'b' | b'B' => (2, "binary"),
            b'o' | b'O' => (8, "octal"),
            b'x' | b'X' => (16, "hexadecimal"),
            _ => return None,
        };
        self.cursor += 1;
        Some((radix, name))
    }

    fn prefixed_integer(&mut self, start: usize, radix: u32, name: &str) {
        let digits_start = self.cursor;
        while self
            .source
            .as_bytes()
            .get(self.cursor)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        {
            self.cursor += 1;
        }

        let digits = &self.source[digits_start..self.cursor];
        if digits.is_empty() {
            self.diagnostics.push(
                Diagnostic::error(
                    "E0011",
                    format!("{name} integer literal requires at least one digit"),
                    Span::new(start, self.cursor),
                )
                .with_help(format!("add a base-{radix} digit after the prefix")),
            );
        } else if let Some((offset, character)) = invalid_radix_digit(digits, radix) {
            let invalid_start = digits_start.saturating_add(offset);
            self.diagnostics.push(
                Diagnostic::error(
                    "E0011",
                    format!("digit `{character}` is not valid in a {name} integer literal"),
                    Span::new(invalid_start, invalid_start.saturating_add(1)),
                )
                .with_help(format!("use only base-{radix} digits and `_` separators")),
            );
        } else if let Some(offset) = invalid_separator(digits, |byte| radix_accepts(radix, byte)) {
            let separator_start = digits_start.saturating_add(offset);
            self.numeric_separator_diagnostic(separator_start);
        }

        self.tokens.push(Token {
            kind: TokenKind::Integer(self.source[start..self.cursor].to_owned()),
            span: Span::new(start, self.cursor),
        });
    }

    fn scan_decimal_digits(&mut self) {
        while let Some(byte) = self.source.as_bytes().get(self.cursor) {
            if !byte.is_ascii_digit() && *byte != b'_' {
                break;
            }
            self.cursor += 1;
        }
    }

    fn validate_decimal_separators(&mut self, start: usize, end: usize) {
        let Some(digits) = self.source.get(start..end) else {
            return;
        };
        if let Some(offset) = invalid_separator(digits, |byte| byte.is_ascii_digit()) {
            self.numeric_separator_diagnostic(start.saturating_add(offset));
        }
    }

    fn numeric_separator_diagnostic(&mut self, start: usize) {
        self.diagnostics.push(
            Diagnostic::error(
                "E0012",
                "numeric separator `_` must appear between two digits",
                Span::new(start, start.saturating_add(1)),
            )
            .with_help("write separators between digits, for example `1_000_000`"),
        );
    }

    fn character(&mut self, start: usize) {
        let value = if self.cursor >= self.source.len() {
            None
        } else {
            match self.advance_char() {
                Some('\\') => self.character_escape(start),
                Some('\'' | '\r' | '\n') | None => None,
                Some(character) => Some(character),
            }
        };

        let closed = self.take_char('\'');
        if let Some(value) = value.filter(|_| closed) {
            self.tokens.push(Token {
                kind: TokenKind::Character(value),
                span: Span::new(start, self.cursor),
            });
        } else {
            self.diagnostics.push(
                Diagnostic::error(
                    "E0004",
                    "invalid or unterminated character literal",
                    Span::new(start, self.cursor),
                )
                .with_help("use one Unicode scalar enclosed in single quotes"),
            );
            self.skip_invalid_character_literal();
        }
    }

    fn string(&mut self, start: usize) {
        self.quoted_string(start, false);
    }

    fn c_string(&mut self, start: usize) {
        self.quoted_string(start, true);
    }

    fn formatted_string(&mut self, start: usize) {
        let mut fragments = Vec::new();
        let mut text = String::new();
        let mut text_start = self.cursor;
        let mut closed = false;

        while self.cursor < self.source.len() {
            let character_start = self.cursor;
            let Some(character) = self.advance_char() else {
                break;
            };
            match character {
                '"' => {
                    Self::flush_formatted_text(
                        &mut fragments,
                        &mut text,
                        text_start,
                        character_start,
                    );
                    closed = true;
                    break;
                }
                '\r' | '\n' => break,
                '\\' => {
                    if let Some(character) = self.character_escape(start) {
                        text.push(character);
                    } else {
                        break;
                    }
                }
                '{' if self.take_char('{') => text.push('{'),
                '}' if self.take_char('}') => text.push('}'),
                '{' => {
                    Self::flush_formatted_text(
                        &mut fragments,
                        &mut text,
                        text_start,
                        character_start,
                    );
                    let expression_start = self.cursor;
                    let Some(expression_end) = self.scan_interpolation_end(start) else {
                        return;
                    };
                    let expression_source = &self.source[expression_start..expression_end];
                    if expression_source.trim().is_empty() {
                        self.diagnostics.push(
                            Diagnostic::error(
                                "E0008",
                                "formatted string placeholder is empty",
                                Span::new(expression_start, expression_end),
                            )
                            .with_help("write an expression between the braces"),
                        );
                    } else {
                        match lex(expression_source) {
                            Ok(tokens) => fragments.push(FormattedStringFragment::Expression {
                                tokens: shift_tokens(tokens, expression_start),
                                span: Span::new(expression_start, expression_end),
                            }),
                            Err(diagnostics) => {
                                self.diagnostics.extend(diagnostics.into_iter().map(
                                    |diagnostic| shift_diagnostic(diagnostic, expression_start),
                                ));
                            }
                        }
                    }
                    text_start = self.cursor;
                }
                '}' => {
                    self.diagnostics.push(
                        Diagnostic::error(
                            "E0009",
                            "unmatched `}` in formatted string",
                            Span::new(character_start, self.cursor),
                        )
                        .with_help("write `}}` to include a literal closing brace"),
                    );
                    return;
                }
                character => text.push(character),
            }
        }

        if closed {
            self.tokens.push(Token {
                kind: TokenKind::FormattedString(fragments),
                span: Span::new(start, self.cursor),
            });
        } else {
            self.diagnostics.push(
                Diagnostic::error(
                    "E0006",
                    "invalid or unterminated formatted string literal",
                    Span::new(start, self.cursor),
                )
                .with_help("close the UTF-8 string with `\"`"),
            );
        }
    }

    fn flush_formatted_text(
        fragments: &mut Vec<FormattedStringFragment>,
        text: &mut String,
        start: usize,
        end: usize,
    ) {
        if text.is_empty() {
            return;
        }
        fragments.push(FormattedStringFragment::Text {
            value: std::mem::take(text),
            span: Span::new(start, end),
        });
    }

    fn scan_interpolation_end(&mut self, formatted_start: usize) -> Option<usize> {
        let mut brace_depth = 0_usize;
        while self.cursor < self.source.len() {
            let character_start = self.cursor;
            let character = self.advance_char()?;
            match character {
                '"' | '\'' => self.skip_interpolation_quote(character),
                '/' if self.take_char('/') => self.skip_line_comment(),
                '/' if self.take_char('*') => self.skip_block_comment(character_start),
                '{' => brace_depth = brace_depth.saturating_add(1),
                '}' if brace_depth == 0 => return Some(character_start),
                '}' => brace_depth = brace_depth.saturating_sub(1),
                _ => {}
            }
        }
        self.diagnostics.push(
            Diagnostic::error(
                "E0010",
                "unterminated formatted string placeholder",
                Span::new(formatted_start, self.cursor),
            )
            .with_help("close the placeholder with `}`"),
        );
        None
    }

    fn skip_interpolation_quote(&mut self, delimiter: char) {
        while self.cursor < self.source.len() {
            let Some(character) = self.advance_char() else {
                return;
            };
            match character {
                '\\' => {
                    let _ = self.advance_char();
                }
                character if character == delimiter => return,
                '\r' | '\n' if delimiter == '"' => return,
                _ => {}
            }
        }
    }

    fn quoted_string(&mut self, start: usize, nul_terminated: bool) {
        let mut value = String::new();
        let mut closed = false;
        while self.cursor < self.source.len() {
            let Some(character) = self.advance_char() else {
                break;
            };
            match character {
                '"' => {
                    closed = true;
                    break;
                }
                '\r' | '\n' => break,
                '\\' => {
                    if let Some(character) = self.character_escape(start) {
                        value.push(character);
                    } else {
                        break;
                    }
                }
                character => value.push(character),
            }
        }
        if closed {
            if nul_terminated && value.contains('\0') {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E0007",
                        "C string literal contains an interior NUL",
                        Span::new(start, self.cursor),
                    )
                    .with_help("remove `\\0`; the compiler appends one final NUL automatically"),
                );
                return;
            }
            self.tokens.push(Token {
                kind: if nul_terminated {
                    TokenKind::CString(value)
                } else {
                    TokenKind::String(value)
                },
                span: Span::new(start, self.cursor),
            });
        } else {
            self.diagnostics.push(
                Diagnostic::error(
                    "E0006",
                    "invalid or unterminated string literal",
                    Span::new(start, self.cursor),
                )
                .with_help("close the UTF-8 string with `\"`"),
            );
        }
    }

    fn character_escape(&mut self, start: usize) -> Option<char> {
        if self.cursor >= self.source.len() {
            return None;
        }
        match self.advance_char()? {
            '0' => Some('\0'),
            'n' => Some('\n'),
            'r' => Some('\r'),
            't' => Some('\t'),
            '\\' => Some('\\'),
            '\'' => Some('\''),
            '"' => Some('"'),
            'u' if self.take_char('{') => self.unicode_escape(start),
            _ => None,
        }
    }

    fn unicode_escape(&mut self, start: usize) -> Option<char> {
        let digits_start = self.cursor;
        while self.cursor < self.source.len()
            && !self.source[self.cursor..].starts_with('}')
            && self.cursor.saturating_sub(digits_start) <= 6
        {
            let character = self.advance_char()?;
            if !character.is_ascii_hexdigit() {
                return None;
            }
        }
        if digits_start == self.cursor || !self.take_char('}') {
            return None;
        }
        u32::from_str_radix(&self.source[digits_start..self.cursor - 1], 16)
            .ok()
            .and_then(char::from_u32)
            .or_else(|| {
                self.diagnostics.push(Diagnostic::error(
                    "E0005",
                    "Unicode escape is not a scalar value",
                    Span::new(start, self.cursor),
                ));
                None
            })
    }

    fn skip_invalid_character_literal(&mut self) {
        while self.cursor < self.source.len() {
            let Some(character) = self.advance_char() else {
                break;
            };
            if matches!(character, '\'' | '\n') {
                break;
            }
        }
    }

    fn push(&mut self, kind: TokenKind, start: usize) {
        self.tokens.push(Token {
            kind,
            span: Span::new(start, self.cursor),
        });
    }
}

fn is_identifier_start(character: char) -> bool {
    character == '_' || character.is_alphabetic()
}

fn is_identifier_continue(character: char) -> bool {
    is_identifier_start(character) || character.is_ascii_digit()
}

fn invalid_radix_digit(digits: &str, radix: u32) -> Option<(usize, char)> {
    digits
        .char_indices()
        .find(|(_, character)| *character != '_' && character.to_digit(radix).is_none())
}

fn invalid_separator(digits: &str, accepts_digit: impl Fn(u8) -> bool) -> Option<usize> {
    let bytes = digits.as_bytes();
    bytes.iter().enumerate().find_map(|(index, byte)| {
        if *byte != b'_' {
            return None;
        }
        let separated = index
            .checked_sub(1)
            .and_then(|previous| bytes.get(previous))
            .copied()
            .is_some_and(&accepts_digit)
            && bytes
                .get(index.saturating_add(1))
                .copied()
                .is_some_and(&accepts_digit);
        (!separated).then_some(index)
    })
}

fn radix_accepts(radix: u32, byte: u8) -> bool {
    char::from(byte).is_digit(radix)
}

fn shift_tokens(tokens: Vec<Token>, offset: usize) -> Vec<Token> {
    tokens
        .into_iter()
        .map(|mut token| {
            token.span = shift_span(token.span, offset);
            if let TokenKind::FormattedString(fragments) = &mut token.kind {
                for fragment in fragments {
                    match fragment {
                        FormattedStringFragment::Text { span, .. } => {
                            *span = shift_span(*span, offset);
                        }
                        FormattedStringFragment::Expression { tokens, span } => {
                            *span = shift_span(*span, offset);
                            *tokens = shift_tokens(std::mem::take(tokens), offset);
                        }
                    }
                }
            }
            token
        })
        .collect()
}

fn shift_diagnostic(mut diagnostic: Diagnostic, offset: usize) -> Diagnostic {
    diagnostic.span = shift_span(diagnostic.span, offset);
    diagnostic
}

const fn shift_span(span: Span, offset: usize) -> Span {
    Span::new(
        span.start.saturating_add(offset),
        span.end.saturating_add(offset),
    )
}

#[cfg(test)]
mod tests {
    use reimer_diagnostics::Span;

    use super::{FormattedStringFragment, TokenKind, lex};

    #[test]
    fn lex_should_tokenize_m0_program() {
        let source = "fn main() -> i32 { return 42; }";

        let tokens = lex(source).expect("fixture should lex");

        assert_eq!(tokens[0].kind, TokenKind::Fn);
    }

    #[test]
    fn lex_should_preserve_utf8_byte_spans() {
        let source = "fn café() -> i32 { return 42; }";

        let tokens = lex(source).expect("fixture should lex");

        assert_eq!(tokens[1].span, Span::new(3, 8));
    }

    #[test]
    fn lex_should_preserve_formatted_text_and_expression_tokens() {
        let source = "let message = f\"hello {{user}} {player.name}\";";

        let tokens = lex(source).expect("fixture should lex");
        let TokenKind::FormattedString(fragments) = &tokens[3].kind else {
            panic!("fourth token should be a formatted string");
        };

        assert!(matches!(
            &fragments[0],
            FormattedStringFragment::Text { value, .. } if value == "hello {user} "
        ));
        let FormattedStringFragment::Expression {
            tokens: expression,
            span,
        } = &fragments[1]
        else {
            panic!("second fragment should be an expression");
        };
        assert_eq!(*span, Span::new(32, 43));
        assert_eq!(
            expression[0].kind,
            TokenKind::Identifier("player".to_owned())
        );
        assert_eq!(expression[0].span, Span::new(32, 38));
    }

    #[test]
    fn lex_should_reject_empty_and_unmatched_formatted_placeholders() {
        let empty = lex("f\"value: {}\"").expect_err("empty placeholder should fail");
        let unmatched = lex("f\"value: }\"").expect_err("unmatched brace should fail");

        assert!(empty.iter().any(|diagnostic| diagnostic.code == "E0008"));
        assert!(
            unmatched
                .iter()
                .any(|diagnostic| diagnostic.code == "E0009")
        );
    }

    #[test]
    fn lex_should_skip_line_and_block_comments() {
        let source = "// head\nfn /* name */ main() -> i32 { return 42; }";

        let tokens = lex(source).expect("fixture should lex");

        assert_eq!(tokens[1].kind, TokenKind::Identifier("main".to_owned()));
    }

    #[test]
    fn lex_should_report_unterminated_block_comment() {
        let source = "fn main() /*";

        let diagnostics = lex(source).expect_err("fixture should fail");

        assert_eq!(diagnostics[0].code, "E0002");
    }

    #[test]
    fn lex_should_tokenize_double_colon_paths() {
        let source = "from game::math import Vec3;";

        let tokens = lex(source).expect("fixture should lex");

        assert_eq!(tokens[2].kind, TokenKind::ColonColon);
    }

    #[test]
    fn lex_should_tokenize_m1_operators_and_keywords() {
        let source = "let mut value = 1; while value <= 4 && true { value += 1; }";

        let tokens = lex(source).expect("fixture should lex");

        assert!(
            tokens
                .iter()
                .any(|token| token.kind == TokenKind::PlusEqual)
        );
    }

    #[test]
    fn lex_should_tokenize_floats_chars_and_bitwise_operators() {
        let source = "let value = 1.5e2; let scalar = 'λ'; value <<= 1; value ^= 2;";

        let tokens = lex(source).expect("fixture should lex");

        assert!(tokens.iter().any(|token| {
            matches!(
                token.kind,
                TokenKind::Character('λ') | TokenKind::LeftShiftEqual
            )
        }));
    }

    #[test]
    fn lex_should_tokenize_integer_bases_and_numeric_separators() {
        let source = "0xDEAD_BEEF 0B1010_0110 0o755 1_000_000 1_024.5_0e2";

        let tokens = lex(source).expect("numeric literals should lex");
        let spellings = tokens
            .iter()
            .filter_map(|token| match &token.kind {
                TokenKind::Integer(spelling) | TokenKind::Float(spelling) => {
                    Some(spelling.as_str())
                }
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(
            spellings,
            [
                "0xDEAD_BEEF",
                "0B1010_0110",
                "0o755",
                "1_000_000",
                "1_024.5_0e2",
            ]
        );
    }

    #[test]
    fn lex_should_reject_invalid_based_digits_and_numeric_separators() {
        for source in ["0x", "0b102", "0o8"] {
            let diagnostics = lex(source).expect_err("invalid based literal should fail");

            assert!(
                diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == "E0011"),
                "expected a based-literal diagnostic for {source}"
            );
        }
        for source in ["1__000", "123_", "0x_FF", "1e_2"] {
            let diagnostics = lex(source).expect_err("invalid separator should fail");

            assert!(
                diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == "E0012"),
                "expected a separator diagnostic for {source}"
            );
        }
    }

    #[test]
    fn lex_should_decode_unicode_character_escape() {
        let tokens = lex("let scalar = '\\u{1F980}';").expect("fixture should lex");

        assert!(
            tokens
                .iter()
                .any(|token| { matches!(token.kind, TokenKind::Character('🦀')) })
        );
    }

    #[test]
    fn lex_should_reject_unterminated_character_literal() {
        let diagnostics = lex("let scalar = 'x;").expect_err("fixture should fail");

        assert_eq!(diagnostics[0].code, "E0004");
    }

    #[test]
    fn lex_should_tokenize_composite_and_pattern_syntax() {
        let source =
            "struct Pair { left: i32, right: i32 } match values[0] { 0 => Pair, _ => Pair }";

        let tokens = lex(source).expect("fixture should lex");

        assert!(tokens.iter().any(|token| token.kind == TokenKind::Struct));
        assert!(
            tokens
                .iter()
                .any(|token| token.kind == TokenKind::LeftBracket)
        );
        assert!(tokens.iter().any(|token| token.kind == TokenKind::FatArrow));
    }

    #[test]
    fn lex_should_tokenize_references_raw_pointers_unsafe_and_strings() {
        let source =
            "fn use_view(values: &mut [i32], raw: *const u8) { unsafe { *raw; } \"Reimer\\n\"; }";

        let tokens = lex(source).expect("fixture should lex");

        assert!(tokens.iter().any(|token| token.kind == TokenKind::Const));
        assert!(tokens.iter().any(|token| token.kind == TokenKind::Unsafe));
        assert!(
            tokens.iter().any(
                |token| matches!(&token.kind, TokenKind::String(value) if value == "Reimer\n")
            )
        );
    }

    #[test]
    fn lex_should_recognize_comptime_as_a_keyword() {
        let tokens =
            lex("comptime fn answer() -> usize { 42 }").expect("compile-time function should lex");

        assert_eq!(tokens[0].kind, TokenKind::Comptime);
    }

    #[test]
    fn lex_should_recognize_static_as_a_keyword() {
        let tokens = lex("static mut COUNTER: i32 = 0;").expect("fixture should lex");

        assert!(tokens.iter().any(|token| token.kind == TokenKind::Static));
        assert!(tokens.iter().any(|token| token.kind == TokenKind::Mut));
    }

    #[test]
    fn lex_should_recognize_type_aliases() {
        let tokens = lex("pub type Index = usize;").expect("fixture should lex");

        assert!(tokens.iter().any(|token| token.kind == TokenKind::Type));
    }

    #[test]
    fn lex_should_tokenize_defer_and_try() {
        let tokens = lex("defer release()?;").expect("fixture should lex");

        assert_eq!(tokens[0].kind, TokenKind::Defer);
        assert!(tokens.iter().any(|token| token.kind == TokenKind::Question));
    }

    #[test]
    fn lex_should_tokenize_an_extern_c_declaration() {
        let tokens =
            lex("@link(\"native\") extern \"C\" { fn native(); }").expect("fixture should lex");

        assert_eq!(tokens[0].kind, TokenKind::At);
        assert!(tokens.iter().any(|token| token.kind == TokenKind::Extern));
    }

    #[test]
    fn lex_should_distinguish_c_string_literals_from_identifiers() {
        let tokens = lex(r#"let title = c"Reimer"; let c = 1;"#).expect("fixture should lex");

        assert!(
            tokens
                .iter()
                .any(|token| matches!(&token.kind, TokenKind::CString(value) if value == "Reimer"))
        );
        assert!(
            tokens
                .iter()
                .any(|token| matches!(&token.kind, TokenKind::Identifier(value) if value == "c"))
        );
    }

    #[test]
    fn lex_should_reject_interior_nul_in_c_string_literals() {
        let diagnostics = lex(r#"let title = c"bad\0title";"#).expect_err("fixture should fail");

        assert_eq!(diagnostics[0].code, "E0007");
    }
}
