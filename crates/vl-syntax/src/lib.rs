//! vl-syntax: recursive-descent parser, tokens -> AST.
//!
//! Grammar (v0):
//! ```text
//! program := item*
//! item    := `let` ident `=` expr `;` | `fn` ident `(` params? `)` block
//! block   := `{` stmt* expr? `}`
//! stmt    := `let` ident `=` expr `;`
//! expr    := term ((`+`|`-`) term)*
//! term    := factor ((`*`|`/`) factor)*
//! factor  := int | ident | `(` expr `)` | `-` factor
//! ```
//!
//! The parser recovers per-item: one bad item doesn't kill the rest.

use vl_common::{Diagnostic, Span};
use vl_lex::{Token, TokenKind};

// ---------------------------------------------------------------- AST ---

#[derive(Debug, Clone)]
pub struct Program {
    pub items: Vec<Item>,
}

#[derive(Debug, Clone)]
pub enum Item {
    Let {
        name: String,
        name_span: Span,
        value: Expr,
        span: Span,
    },
    Fn {
        name: String,
        name_span: Span,
        params: Vec<(String, Span)>,
        body: Vec<Stmt>,
        span: Span,
    },
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Let {
        name: String,
        name_span: Span,
        value: Expr,
        span: Span,
    },
    Expr(Expr),
}

#[derive(Debug, Clone)]
pub enum Expr {
    Int(i64, Span),
    Var(String, Span),
    Unary {
        op: UnOp,
        rhs: Box<Expr>,
        span: Span,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        span: Span,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::Int(_, s) => *s,
            Expr::Var(_, s) => *s,
            Expr::Unary { span, .. } | Expr::Binary { span, .. } => *span,
        }
    }
}

// -------------------------------------------------------------- Parser ---

struct Parser<'a> {
    toks: &'a [Token],
    pos: usize,
    diags: Vec<Diagnostic>,
}

pub fn parse(toks: &[Token], _src: &str) -> (Program, Vec<Diagnostic>) {
    let mut p = Parser {
        toks,
        pos: 0,
        diags: vec![],
    };
    let mut items = Vec::new();
    while !p.at_eof() {
        match p.parse_item() {
            Some(item) => items.push(item),
            None => p.recover_to_item_boundary(),
        }
    }
    (Program { items }, p.diags)
}

impl<'a> Parser<'a> {
    fn peek(&self) -> &Token {
        &self.toks[self.pos.min(self.toks.len() - 1)]
    }

    fn at_eof(&self) -> bool {
        matches!(self.peek().kind, TokenKind::Eof)
    }

    fn bump(&mut self) -> Token {
        let t = self.peek().clone();
        if !self.at_eof() {
            self.pos += 1;
        }
        t
    }

    fn expect(&mut self, want: &TokenKind, what: &str) -> Option<Token> {
        let t = self.peek().clone();
        if discriminant(&t.kind) == discriminant(want) {
            Some(self.bump())
        } else {
            self.diags.push(
                Diagnostic::error(format!("expected {what}, found {}", describe(&t.kind)))
                    .with_label(t.span, "unexpected token here")
                    .with_code("E100"),
            );
            None
        }
    }

    /// Skip to the next plausible item start so one error hides no others.
    fn recover_to_item_boundary(&mut self) {
        while !self.at_eof() {
            match &self.peek().kind {
                TokenKind::Semi | TokenKind::RBrace => {
                    self.bump();
                    return;
                }
                TokenKind::Let | TokenKind::Fn => return,
                _ => {
                    self.bump();
                }
            }
        }
    }

    fn parse_item(&mut self) -> Option<Item> {
        match &self.peek().kind {
            TokenKind::Let => self.parse_let_item(),
            TokenKind::Fn => self.parse_fn_item(),
            TokenKind::Eof => None,
            _ => {
                let t = self.peek().clone();
                self.diags.push(
                    Diagnostic::error(format!(
                        "expected an item (`let` or `fn`), found {}",
                        describe(&t.kind)
                    ))
                    .with_label(t.span, "items start with `let` or `fn`")
                    .with_code("E101"),
                );
                None
            }
        }
    }

    fn parse_let_item(&mut self) -> Option<Item> {
        let let_tok = self.bump(); // `let`
        let (name, name_span) = self.parse_ident()?;
        self.expect(&TokenKind::Eq, "`=`")?;
        let value = self.parse_expr()?;
        let semi = self.expect(&TokenKind::Semi, "`;`")?;
        Some(Item::Let {
            name,
            name_span,
            value,
            span: Span::new(let_tok.span.start, semi.span.end),
        })
    }

    fn parse_fn_item(&mut self) -> Option<Item> {
        let fn_tok = self.bump(); // `fn`
        let (name, name_span) = self.parse_ident()?;
        self.expect(&TokenKind::LParen, "`(`")?;
        let mut params = Vec::new();
        if !matches!(self.peek().kind, TokenKind::RParen) {
            loop {
                if let Some((n, s)) = self.parse_ident_opt() {
                    params.push((n, s));
                } else {
                    return None;
                }
                match &self.peek().kind {
                    TokenKind::Comma => {
                        self.bump();
                    }
                    _ => break,
                }
            }
        }
        self.expect(&TokenKind::RParen, "`)`")?;
        self.expect(&TokenKind::LBrace, "`{` for the function body")?;
        let mut body = Vec::new();
        while !self.at_eof() && !matches!(self.peek().kind, TokenKind::RBrace) {
            match self.parse_stmt() {
                Some(s) => body.push(s),
                None => self.recover_to_stmt_boundary(),
            }
        }
        let close = self.expect(&TokenKind::RBrace, "`}`")?;
        Some(Item::Fn {
            name,
            name_span,
            params,
            body,
            span: Span::new(fn_tok.span.start, close.span.end),
        })
    }

    fn parse_stmt(&mut self) -> Option<Stmt> {
        if matches!(self.peek().kind, TokenKind::Let) {
            let let_tok = self.bump();
            let (name, name_span) = self.parse_ident()?;
            self.expect(&TokenKind::Eq, "`=`")?;
            let value = self.parse_expr()?;
            let semi = self.expect(&TokenKind::Semi, "`;`")?;
            Some(Stmt::Let {
                name,
                name_span,
                value,
                span: Span::new(let_tok.span.start, semi.span.end),
            })
        } else {
            self.parse_expr().map(Stmt::Expr)
        }
    }

    fn recover_to_stmt_boundary(&mut self) {
        while !self.at_eof() {
            match &self.peek().kind {
                TokenKind::Semi => {
                    self.bump();
                    return;
                }
                TokenKind::RBrace | TokenKind::Let | TokenKind::Fn => return,
                _ => {
                    self.bump();
                }
            }
        }
    }

    fn parse_ident(&mut self) -> Option<(String, Span)> {
        self.parse_ident_opt().or_else(|| {
            let t = self.peek().clone();
            self.diags.push(
                Diagnostic::error(format!("expected a name, found {}", describe(&t.kind)))
                    .with_label(t.span, "expected identifier here")
                    .with_code("E102"),
            );
            None
        })
    }

    fn parse_ident_opt(&mut self) -> Option<(String, Span)> {
        match &self.peek().kind {
            TokenKind::Ident(_) => {
                let t = self.bump();
                match t.kind {
                    TokenKind::Ident(name) => Some((name, t.span)),
                    _ => unreachable!(),
                }
            }
            _ => None,
        }
    }

    fn parse_expr(&mut self) -> Option<Expr> {
        let mut lhs = self.parse_term()?;
        loop {
            let op = match &self.peek().kind {
                TokenKind::Plus => BinOp::Add,
                TokenKind::Minus => BinOp::Sub,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_term()?;
            let span = lhs.span().merge(rhs.span());
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        Some(lhs)
    }

    fn parse_term(&mut self) -> Option<Expr> {
        let mut lhs = self.parse_factor()?;
        loop {
            let op = match &self.peek().kind {
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_factor()?;
            let span = lhs.span().merge(rhs.span());
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        Some(lhs)
    }

    fn parse_factor(&mut self) -> Option<Expr> {
        let t = self.peek().clone();
        match t.kind {
            TokenKind::Int(v) => {
                self.bump();
                Some(Expr::Int(v, t.span))
            }
            TokenKind::Ident(_) => {
                self.bump();
                match t.kind {
                    TokenKind::Ident(name) => Some(Expr::Var(name, t.span)),
                    _ => unreachable!(),
                }
            }
            TokenKind::LParen => {
                self.bump();
                let inner = self.parse_expr()?;
                self.expect(&TokenKind::RParen, "`)`")?;
                Some(inner)
            }
            TokenKind::Minus => {
                self.bump();
                let rhs = self.parse_factor()?;
                let span = Span::new(t.span.start, rhs.span().end);
                Some(Expr::Unary {
                    op: UnOp::Neg,
                    rhs: Box::new(rhs),
                    span,
                })
            }
            _ => {
                self.diags.push(
                    Diagnostic::error(format!(
                        "expected an expression, found {}",
                        describe(&t.kind)
                    ))
                    .with_label(t.span, "expected value here")
                    .with_code("E103"),
                );
                None
            }
        }
    }
}

fn discriminant(k: &TokenKind) -> std::mem::Discriminant<TokenKind> {
    std::mem::discriminant(k)
}

fn describe(k: &TokenKind) -> String {
    match k {
        TokenKind::Ident(n) => format!("identifier `{n}`"),
        TokenKind::Int(v) => format!("integer `{v}`"),
        TokenKind::Let => "`let`".into(),
        TokenKind::Fn => "`fn`".into(),
        TokenKind::Plus => "`+`".into(),
        TokenKind::Minus => "`-`".into(),
        TokenKind::Star => "`*`".into(),
        TokenKind::Slash => "`/`".into(),
        TokenKind::Eq => "`=`".into(),
        TokenKind::Semi => "`;`".into(),
        TokenKind::LParen => "`(`".into(),
        TokenKind::RParen => "`)`".into(),
        TokenKind::LBrace => "`{`".into(),
        TokenKind::RBrace => "`}`".into(),
        TokenKind::Comma => "`,`".into(),
        TokenKind::Eof => "end of file".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_src(src: &str) -> (Program, Vec<Diagnostic>) {
        let (toks, lex_diags) = vl_lex::lex(src);
        assert!(lex_diags.is_empty());
        parse(&toks, src)
    }

    #[test]
    fn parses_arithmetic_with_precedence() {
        let (prog, diags) = parse_src("let x = 1 + 2 * 3;");
        assert!(diags.is_empty());
        assert_eq!(prog.items.len(), 1);
    }

    #[test]
    fn missing_semi_is_an_error() {
        let (_prog, diags) = parse_src("let x = 1");
        assert!(!diags.is_empty());
    }
}
