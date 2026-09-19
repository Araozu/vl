//! vl-lex: hand-rolled lexer for the VL v0 surface syntax.
//!
//! Input: `&str`. Output: `Vec<Token>` plus `Vec<Diagnostic>`.
//! The lexer never panics on bad input — it emits an error diagnostic
//! per offending character and keeps going.

use vl_common::{Diagnostic, Span};

/// Token kinds of the v0 language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    Ident(String),
    I64(i64),
    U64(u64),
    F64(u64),
    U8(u8),
    Bool(bool),
    String(Vec<u8>),
    Let,
    Function,
    If,
    Else,
    While,
    Break,
    Continue,
    Return,
    Plus,
    Minus,
    Star,
    Slash,
    Eq,
    EqEq,
    Bang,
    BangEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    AmpAmp,
    PipePipe,
    Semi,
    LParen,
    RParen,
    LBrace,
    RBrace,
    Comma,
    Dot,
    Colon,
    /// A token whose source span already has a lexer diagnostic. Parsers
    /// consume it without inventing follow-on syntax errors.
    Invalid,
    Eof,
}

/// One lexical token with its source span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    pub fn new(kind: TokenKind, span: Span) -> Self {
        Self { kind, span }
    }
}

/// Lex `src` into tokens. Diagnostics borrow nothing — spans index `src`.
pub fn lex(src: &str) -> (Vec<Token>, Vec<Diagnostic>) {
    let mut tokens = Vec::new();
    let mut diags = Vec::new();
    let bytes = src.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        let c = src[i..]
            .chars()
            .next()
            .expect("i always stays on a char boundary");
        match c {
            // Whitespace.
            ' ' | '\t' | '\r' | '\n' => {
                i += 1;
            }
            // Line comment.
            '/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            '+' => {
                tokens.push(Token::new(TokenKind::Plus, Span::new(i, i + 1)));
                i += 1;
            }
            '-' => {
                tokens.push(Token::new(TokenKind::Minus, Span::new(i, i + 1)));
                i += 1;
            }
            '*' => {
                tokens.push(Token::new(TokenKind::Star, Span::new(i, i + 1)));
                i += 1;
            }
            '/' => {
                tokens.push(Token::new(TokenKind::Slash, Span::new(i, i + 1)));
                i += 1;
            }
            '=' => {
                if bytes.get(i + 1) == Some(&b'=') {
                    tokens.push(Token::new(TokenKind::EqEq, Span::new(i, i + 2)));
                    i += 2;
                } else {
                    tokens.push(Token::new(TokenKind::Eq, Span::new(i, i + 1)));
                    i += 1;
                }
            }
            '!' => {
                if bytes.get(i + 1) == Some(&b'=') {
                    tokens.push(Token::new(TokenKind::BangEq, Span::new(i, i + 2)));
                    i += 2;
                } else {
                    tokens.push(Token::new(TokenKind::Bang, Span::new(i, i + 1)));
                    i += 1;
                }
            }
            '<' => {
                if bytes.get(i + 1) == Some(&b'=') {
                    tokens.push(Token::new(TokenKind::LtEq, Span::new(i, i + 2)));
                    i += 2;
                } else {
                    tokens.push(Token::new(TokenKind::Lt, Span::new(i, i + 1)));
                    i += 1;
                }
            }
            '>' => {
                if bytes.get(i + 1) == Some(&b'=') {
                    tokens.push(Token::new(TokenKind::GtEq, Span::new(i, i + 2)));
                    i += 2;
                } else {
                    tokens.push(Token::new(TokenKind::Gt, Span::new(i, i + 1)));
                    i += 1;
                }
            }
            '&' => {
                if bytes.get(i + 1) == Some(&b'&') {
                    tokens.push(Token::new(TokenKind::AmpAmp, Span::new(i, i + 2)));
                    i += 2;
                } else {
                    diags.push(
                        Diagnostic::error("unexpected character `&`")
                            .with_label(Span::new(i, i + 1), "did you mean `&&`?")
                            .with_code("E000"),
                    );
                    tokens.push(Token::new(TokenKind::Invalid, Span::new(i, i + 1)));
                    i += 1;
                }
            }
            '|' => {
                if bytes.get(i + 1) == Some(&b'|') {
                    tokens.push(Token::new(TokenKind::PipePipe, Span::new(i, i + 2)));
                    i += 2;
                } else {
                    diags.push(
                        Diagnostic::error("unexpected character `|`")
                            .with_label(Span::new(i, i + 1), "did you mean `||`?")
                            .with_code("E000"),
                    );
                    tokens.push(Token::new(TokenKind::Invalid, Span::new(i, i + 1)));
                    i += 1;
                }
            }
            ';' => {
                tokens.push(Token::new(TokenKind::Semi, Span::new(i, i + 1)));
                i += 1;
            }
            '(' => {
                tokens.push(Token::new(TokenKind::LParen, Span::new(i, i + 1)));
                i += 1;
            }
            ')' => {
                tokens.push(Token::new(TokenKind::RParen, Span::new(i, i + 1)));
                i += 1;
            }
            '{' => {
                tokens.push(Token::new(TokenKind::LBrace, Span::new(i, i + 1)));
                i += 1;
            }
            '}' => {
                tokens.push(Token::new(TokenKind::RBrace, Span::new(i, i + 1)));
                i += 1;
            }
            ',' => {
                tokens.push(Token::new(TokenKind::Comma, Span::new(i, i + 1)));
                i += 1;
            }
            ':' => {
                tokens.push(Token::new(TokenKind::Colon, Span::new(i, i + 1)));
                i += 1;
            }
            '.' => {
                tokens.push(Token::new(TokenKind::Dot, Span::new(i, i + 1)));
                i += 1;
            }
            '0'..='9' => {
                let start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                let is_float = bytes.get(i) == Some(&b'.')
                    && bytes.get(i + 1).is_some_and(|b| b.is_ascii_digit());
                if is_float {
                    i += 1;
                    while i < bytes.len() && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                let suffix_start = i;
                while i < bytes.len() && bytes[i].is_ascii_alphanumeric() {
                    i += 1;
                }
                let number = &src[start..suffix_start];
                let suffix = &src[suffix_start..i];
                let result = if is_float {
                    if suffix != "f64" {
                        Err("floating literals require the `f64` suffix")
                    } else {
                        number
                            .parse::<f64>()
                            .map(|v| TokenKind::F64(v.to_bits()))
                            .map_err(|_| "invalid f64 literal")
                    }
                } else {
                    match suffix {
                        "" => number
                            .parse::<i64>()
                            .map(TokenKind::I64)
                            .map_err(|_| "integer literal out of range"),
                        "i64" => number
                            .parse::<i64>()
                            .map(TokenKind::I64)
                            .map_err(|_| "i64 literal out of range"),
                        "u64" => number
                            .parse::<u64>()
                            .map(TokenKind::U64)
                            .map_err(|_| "u64 literal out of range"),
                        "u8" => number
                            .parse::<u8>()
                            .map(TokenKind::U8)
                            .map_err(|_| "u8 literal out of range"),
                        _ => Err("unknown numeric literal suffix"),
                    }
                };
                match result {
                    Ok(kind) => tokens.push(Token::new(kind, Span::new(start, i))),
                    Err(message) => {
                        diags.push(
                            Diagnostic::error(message)
                                .with_label(Span::new(start, i), "invalid numeric literal")
                                .with_code("E001"),
                        );
                        tokens.push(Token::new(TokenKind::Invalid, Span::new(start, i)));
                    }
                }
            }
            '"' => {
                let start = i;
                i += 1;
                let mut value = Vec::new();
                let mut valid = true;
                let mut closed = false;
                while i < bytes.len() {
                    match bytes[i] {
                        b'"' => {
                            i += 1;
                            closed = true;
                            break;
                        }
                        b'\n' | b'\r' => break,
                        b'\\' => {
                            i += 1;
                            if i >= bytes.len() {
                                break;
                            }
                            if matches!(bytes[i], b'\n' | b'\r') {
                                break;
                            }
                            let escaped = match bytes[i] {
                                b'0' => Some(0),
                                b'n' => Some(b'\n'),
                                b'r' => Some(b'\r'),
                                b't' => Some(b'\t'),
                                b'\\' => Some(b'\\'),
                                b'"' => Some(b'"'),
                                _ => None,
                            };
                            if let Some(byte) = escaped {
                                value.push(byte);
                                i += 1;
                            } else {
                                valid = false;
                                let escape_end = src[i..]
                                    .chars()
                                    .next()
                                    .map_or(i + 1, |ch| i + ch.len_utf8());
                                diags.push(
                                    Diagnostic::error("unknown string escape")
                                        .with_label(Span::new(i - 1, escape_end), "unknown escape")
                                        .with_note("supported escapes are `\\0`, `\\n`, `\\r`, `\\t`, `\\\\`, and `\\\"`")
                                        .with_code("E003"),
                                );
                                i = escape_end;
                            }
                        }
                        byte => {
                            value.push(byte);
                            i += 1;
                        }
                    }
                }
                if !closed {
                    let end = i;
                    diags.push(
                        Diagnostic::error("unterminated string literal")
                            .with_label(
                                Span::new(start, end),
                                "string must close before the line ends",
                            )
                            .with_code("E002"),
                    );
                    tokens.push(Token::new(TokenKind::Invalid, Span::new(start, end)));
                    // Leave the newline for the normal whitespace path.
                } else if valid {
                    tokens.push(Token::new(TokenKind::String(value), Span::new(start, i)));
                } else {
                    tokens.push(Token::new(TokenKind::Invalid, Span::new(start, i)));
                }
            }
            'a'..='z' | 'A'..='Z' | '_' => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                let word = &src[start..i];
                let kind = match word {
                    "let" => TokenKind::Let,
                    "function" => TokenKind::Function,
                    "if" => TokenKind::If,
                    "else" => TokenKind::Else,
                    "while" => TokenKind::While,
                    "break" => TokenKind::Break,
                    "continue" => TokenKind::Continue,
                    "return" => TokenKind::Return,
                    "true" => TokenKind::Bool(true),
                    "false" => TokenKind::Bool(false),
                    _ => TokenKind::Ident(word.to_string()),
                };
                tokens.push(Token::new(kind, Span::new(start, i)));
            }
            _ => {
                // Non-ASCII or punctuation we don't know: one error, keep going.
                // Advance by the UTF-8 scalar width so a single character
                // produces one diagnostic and a valid source span.
                let end = i + c.len_utf8();
                diags.push(
                    Diagnostic::error(format!("unexpected character `{c}`"))
                        .with_label(Span::new(i, end), "unexpected here")
                        .with_note("identifiers use letters, digits and `_`; see `let`, `function`")
                        .with_code("E000"),
                );
                tokens.push(Token::new(TokenKind::Invalid, Span::new(i, end)));
                i = end;
            }
        }
    }

    tokens.push(Token::new(TokenKind::Eof, Span::empty(src.len())));
    (tokens, diags)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexes_let_binding() {
        let (toks, diags) = lex("let x = 1 + 2;");
        assert!(diags.is_empty());
        assert!(matches!(toks[0].kind, TokenKind::Let));
        assert!(matches!(toks[1].kind, TokenKind::Ident(_)));
    }

    #[test]
    fn lexes_function_keyword() {
        let (toks, diags) = lex("function main() {}");
        assert!(diags.is_empty());
        assert!(matches!(toks[0].kind, TokenKind::Function));
    }

    #[test]
    fn bad_char_is_a_diagnostic_not_a_panic() {
        let (toks, diags) = lex("let x = @;");
        assert_eq!(diags.len(), 1);
        assert!(toks.iter().any(|t| matches!(t.kind, TokenKind::Eof)));
    }

    #[test]
    fn lexes_byte_string_and_common_escapes() {
        let (toks, diags) = lex("\"a\\n\\t\\\\\\\"\\0\"");
        assert!(diags.is_empty());
        assert!(matches!(
            &toks[0].kind,
            TokenKind::String(value) if value == b"a\n\t\\\"\0"
        ));
    }

    #[test]
    fn unterminated_string_stops_at_newline() {
        let (toks, diags) = lex("\"not closed\nlet x = 1;");
        assert!(diags.iter().any(|d| d.message.contains("unterminated")));
        assert!(toks.iter().any(|t| matches!(t.kind, TokenKind::Let)));
    }

    #[test]
    fn unknown_escape_is_a_diagnostic() {
        let (toks, diags) = lex(r#""bad\q""#);
        assert!(diags
            .iter()
            .any(|d| d.message.contains("unknown string escape")));
        assert!(!toks.iter().any(|t| matches!(t.kind, TokenKind::String(_))));
    }

    #[test]
    fn non_ascii_input_is_an_error_not_a_panic() {
        let (_toks, diags) = lex("let café = 1;");
        assert!(!diags.is_empty());
    }

    #[test]
    fn non_ascii_character_has_one_scalar_span() {
        let (toks, diags) = lex("é");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].labels[0].span, Span::new(0, 2));
        assert!(matches!(toks[0].kind, TokenKind::Invalid));
    }

    #[test]
    fn invalid_numeric_literal_is_poisoned_for_parser_recovery() {
        let (toks, diags) = lex("let x = 999999999999999999999999; let y = 2;");
        assert_eq!(diags.len(), 1);
        assert!(toks.iter().any(|t| matches!(t.kind, TokenKind::Invalid)));
    }

    #[test]
    fn unknown_unicode_escape_has_a_character_aligned_span() {
        let (_toks, diags) = lex("\"\\é\"");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].labels[0].span, Span::new(1, 4));
    }

    #[test]
    fn lexes_loop_keywords_and_operators() {
        let (toks, diags) =
            lex("while (a == 1 && b != 2 || !c) { a = a + 1; break; continue; return a; }");
        assert!(diags.is_empty(), "{diags:?}");
        let kinds: Vec<&TokenKind> = toks.iter().map(|t| &t.kind).collect();
        assert!(matches!(kinds[0], TokenKind::While));
        assert!(kinds.iter().any(|k| matches!(k, TokenKind::EqEq)));
        assert!(kinds.iter().any(|k| matches!(k, TokenKind::AmpAmp)));
        assert!(kinds.iter().any(|k| matches!(k, TokenKind::BangEq)));
        assert!(kinds.iter().any(|k| matches!(k, TokenKind::PipePipe)));
        assert!(kinds.iter().any(|k| matches!(k, TokenKind::Bang)));
        assert!(kinds.iter().any(|k| matches!(k, TokenKind::Break)));
        assert!(kinds.iter().any(|k| matches!(k, TokenKind::Continue)));
        assert!(kinds.iter().any(|k| matches!(k, TokenKind::Return)));
    }

    #[test]
    fn lexes_ordering_operators() {
        let (toks, diags) = lex("a < b <= c > d >= e");
        assert!(diags.is_empty(), "{diags:?}");
        assert!(toks.iter().any(|t| matches!(t.kind, TokenKind::Lt)));
        assert!(toks.iter().any(|t| matches!(t.kind, TokenKind::LtEq)));
        assert!(toks.iter().any(|t| matches!(t.kind, TokenKind::Gt)));
        assert!(toks.iter().any(|t| matches!(t.kind, TokenKind::GtEq)));
    }

    #[test]
    fn single_ampersand_is_an_error() {
        let (_toks, diags) = lex("a & b");
        assert!(diags.iter().any(|d| d.message.contains('`')));
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn lexes_scalar_suffixes_and_bools() {
        let (toks, diags) = lex("1u64 -1i64 1.5f64 255u8 true false");
        assert!(diags.is_empty(), "{diags:?}");
        assert!(matches!(toks[0].kind, TokenKind::U64(1)));
        assert!(matches!(toks[1].kind, TokenKind::Minus));
        assert!(matches!(toks[2].kind, TokenKind::I64(1)));
        assert!(matches!(toks[3].kind, TokenKind::F64(_)));
        assert!(matches!(toks[4].kind, TokenKind::U8(255)));
        assert!(matches!(toks[5].kind, TokenKind::Bool(true)));
        assert!(matches!(toks[6].kind, TokenKind::Bool(false)));
    }
}
