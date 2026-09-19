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
    Plus,
    Minus,
    Star,
    Slash,
    Eq,
    Semi,
    LParen,
    RParen,
    LBrace,
    RBrace,
    Comma,
    Dot,
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
        let c = bytes[i] as char;
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
                tokens.push(Token::new(TokenKind::Eq, Span::new(i, i + 1)));
                i += 1;
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
                    Err(message) => diags.push(
                        Diagnostic::error(message)
                            .with_label(Span::new(start, i), "invalid numeric literal")
                            .with_code("E001"),
                    ),
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
                            } else {
                                valid = false;
                                diags.push(
                                    Diagnostic::error("unknown string escape")
                                        .with_label(Span::new(i - 1, i + 1), "unknown escape")
                                        .with_note("supported escapes are `\\0`, `\\n`, `\\r`, `\\t`, `\\\\`, and `\\\"`")
                                        .with_code("E003"),
                                );
                            }
                            i += 1;
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
                    // Leave the newline for the normal whitespace path.
                } else if valid {
                    tokens.push(Token::new(TokenKind::String(value), Span::new(start, i)));
                }
            }
            'a'..='z' | 'A'..='Z' | '_' => {
                let start = i;
                while i < bytes.len() && ((bytes[i] as char).is_alphanumeric() || bytes[i] == b'_')
                {
                    i += 1;
                }
                let word = &src[start..i];
                let kind = match word {
                    "let" => TokenKind::Let,
                    "function" => TokenKind::Function,
                    "if" => TokenKind::If,
                    "else" => TokenKind::Else,
                    "true" => TokenKind::Bool(true),
                    "false" => TokenKind::Bool(false),
                    _ => TokenKind::Ident(word.to_string()),
                };
                tokens.push(Token::new(kind, Span::new(start, i)));
            }
            _ => {
                // Non-ASCII or punctuation we don't know: one error, keep going.
                let end = (i + 1).min(bytes.len());
                diags.push(
                    Diagnostic::error(format!("unexpected character `{c}`"))
                        .with_label(Span::new(i, end), "unexpected here")
                        .with_note("identifiers use letters, digits and `_`; see `let`, `function`")
                        .with_code("E000"),
                );
                i += 1;
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
