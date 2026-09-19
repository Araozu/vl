//! vl-syntax: recursive-descent parser, tokens -> AST.
//!
//! Grammar (v0, TypeScript-like surface):
//! ```text
//! program := item*
//! item    := `let` ident `=` expr `;` | `function` ident `(` params? `)` (`:` type)? block
//! params  := param (`,` param)*
//! param   := ident `:` type
//! type    := `u64` | `i64` | `f64` | `bool` | `u8` | `string` | `File` | `U64Array` | `void` (`void` only as return)
//! block   := `{` stmt* `}`
//! stmt    := `let` ident `=` expr `;` | ident `=` expr `;` | index `=` expr `;`
//!          | `if` `(` expr `)` branch (`else` branch)?
//!          | `while` `(` expr `)` branch | `break` `;` | `continue` `;`
//!          | `return` expr? `;` | expr `;`
//! branch  := block | stmt
//! index   := ident (`[` expr `]`)+
//! expr    := or
//! or      := and (`||` and)*
//! and     := equality (`&&` equality)*
//! equality:= comparison ((`==`|`!=`) comparison)*
//! comparison := term ((`<`|`<=`|`>`|`>=`) term)*
//! term    := factor ((`+`|`-`) factor)*
//! factor  := unary ((`*`|`/`) unary)*
//! unary   := (`-`|`!`) unary | postfix
//! postfix := primary (`[` expr `]`)*
//! primary := literal | array-literal | call | `(` expr `)`
//! array-literal := `[` (expr (`,` expr)* `,`?)? `]`
//! call    := ident `(` args? `)`
//! args    := expr (`,` expr)*
//! ```
//!
//! `U64Array` is a fixed-length heap array of `u64`: `U64Array.new(n)` creates
//! a zero-filled array of `n` elements, `[1u64, 2u64]` is an array literal
//! (empty `[]` is the length-0 array), `a[i]` reads element `i`, and
//! `a[i] = v;` writes it. Indices and elements are always `u64`.
//!
//!
//! Calls are callee-by-name (`ident(args)`), TypeScript-style. The callee
//! is a plain variable use so forward references to `function` items work.
//! Semicolons are mandatory: every `let`, every `return`, and every
//! expression statement ends with `;` (no bare trailing value like Rust).
//! There are no implicit returns: a function yields a value only through an
//! explicit `return expr;` (`return;` for `void`).
//! Function boundaries are typed: every param needs `: type`; the return
//! type may be omitted and defaults to `void`.
//!
//! The parser recovers per-item: one bad item doesn't kill the rest.

pub use vl_common::Scalar;
pub use vl_common::VlType;
use vl_common::{Diagnostic, Span};
use vl_lex::{Token, TokenKind};

// ---------------------------------------------------------------- AST ---

#[derive(Debug, Clone)]
pub struct Program {
    pub module: String,
    pub items: Vec<Item>,
}

/// One typed function parameter: `name: type`.
#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub name_span: Span,
    /// `None` when the annotation was missing or unknown (already reported;
    /// downstream stays quiet via `Ty::Error` poisoning).
    pub ty: Option<VlType>,
    pub ty_span: Option<Span>,
}

#[derive(Debug, Clone)]
pub enum Item {
    Use {
        path: Vec<String>,
        names: Option<Vec<String>>,
        span: Span,
    },
    Let {
        name: String,
        name_span: Span,
        value: Expr,
        span: Span,
    },
    Function {
        name: String,
        name_span: Span,
        params: Vec<Param>,
        /// `None` when the return annotation was missing (already reported).
        ret: Option<VlType>,
        ret_span: Option<Span>,
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
    Assign {
        name: String,
        name_span: Span,
        value: Expr,
        span: Span,
    },
    /// Element write: `array[index] = value;` (only `ident`-led index chains
    /// parse as statements; anything else is an expression-statement error).
    IndexAssign {
        array: Box<Expr>,
        index: Box<Expr>,
        value: Box<Expr>,
        span: Span,
    },
    If {
        condition: Expr,
        then_body: Vec<Stmt>,
        else_body: Option<Vec<Stmt>>,
        span: Span,
    },
    While {
        condition: Expr,
        body: Vec<Stmt>,
        span: Span,
    },
    Break {
        span: Span,
    },
    Continue {
        span: Span,
    },
    Return {
        value: Option<Expr>,
        span: Span,
    },
    Expr(Expr),
}

#[derive(Debug, Clone)]
pub enum Expr {
    Literal(Scalar, Span),
    String(Vec<u8>, Span),
    /// Array literal: `[1u64, 2u64]` (empty `[]` is the length-0 `U64Array`).
    /// Element types are enforced later (`u64` only for now).
    ArrayLiteral {
        elems: Vec<Expr>,
        span: Span,
    },
    /// Element read: `array[index]`.
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
        span: Span,
    },
    Var {
        path: Vec<String>,
        span: Span,
    },
    Call {
        callee: Vec<String>,
        callee_span: Span,
        args: Vec<Expr>,
        span: Span,
    },
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
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::Literal(_, s) => *s,
            Expr::String(_, s) => *s,
            Expr::ArrayLiteral { span, .. } => *span,
            Expr::Index { span, .. } => *span,
            Expr::Var { span, .. } => *span,
            Expr::Call { span, .. } => *span,
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

pub fn parse(toks: &[Token], src: &str) -> (Program, Vec<Diagnostic>) {
    parse_with_module(toks, src, "<anonymous>")
}

pub fn parse_with_module(toks: &[Token], _src: &str, module: &str) -> (Program, Vec<Diagnostic>) {
    let mut p = Parser {
        toks,
        pos: 0,
        diags: vec![],
    };
    let mut items = Vec::new();
    while !p.at_eof() {
        match p.parse_item() {
            Some(item) => items.push(item),
            None => {
                let before = p.pos;
                p.recover_to_item_boundary();
                // A recovery boundary can itself be the token at which
                // parsing failed (notably a nested `function`). Always make
                // progress so malformed input cannot spin forever.
                if p.pos == before && !p.at_eof() {
                    p.bump();
                }
            }
        }
    }
    (
        Program {
            module: module.into(),
            items,
        },
        p.diags,
    )
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
                TokenKind::Let | TokenKind::Function => return,
                _ => {
                    self.bump();
                }
            }
        }
    }

    fn parse_item(&mut self) -> Option<Item> {
        match &self.peek().kind {
            TokenKind::Let => self.parse_let_item(),
            TokenKind::Function => self.parse_function_item(),
            TokenKind::Ident(name) if name == "use" => self.parse_use_item(),
            TokenKind::Eof => None,
            TokenKind::Invalid => {
                self.bump();
                None
            }
            _ => {
                let t = self.peek().clone();
                self.diags.push(
                    Diagnostic::error(format!(
                        "expected an item (`use`, `let` or `function`), found {}",
                        describe(&t.kind)
                    ))
                    .with_label(t.span, "items start with `use`, `let` or `function`")
                    .with_code("E101"),
                );
                None
            }
        }
    }

    fn parse_use_item(&mut self) -> Option<Item> {
        let start = self.bump().span;
        let path = self.parse_path()?;
        let names = if matches!(self.peek().kind, TokenKind::Dot) {
            self.bump();
            self.expect(&TokenKind::LBrace, "`{` after module path")?;
            let mut names = Vec::new();
            loop {
                names.push(self.parse_ident()?.0);
                if !matches!(self.peek().kind, TokenKind::Comma) {
                    break;
                }
                self.bump();
            }
            self.expect(&TokenKind::RBrace, "`}` after imported names")?;
            Some(names)
        } else {
            None
        };
        let semi = self.expect(&TokenKind::Semi, "`;`")?;
        Some(Item::Use {
            path,
            names,
            span: Span::new(start.start, semi.span.end),
        })
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

    fn parse_type(&mut self) -> Option<(VlType, Span)> {
        let t = self.peek().clone();
        match t.kind {
            TokenKind::Ident(name) => {
                self.bump();
                match name.parse::<VlType>() {
                    Ok(ty) => Some((ty, t.span)),
                    Err(e) => {
                        self.diags.push(
                            Diagnostic::error(e.to_string())
                                .with_label(t.span, "unknown type here")
                                .with_code("E105"),
                        );
                        None
                    }
                }
            }
            _ => {
                self.diags.push(
                    Diagnostic::error(format!("expected a type, found {}", describe(&t.kind)))
                        .with_label(
                            t.span,
                            "expected one of u64, i64, f64, bool, u8, string, File, U64Array, void",
                        )
                        .with_code("E104"),
                );
                None
            }
        }
    }

    fn parse_param(&mut self) -> Option<Param> {
        let (name, name_span) = self.parse_ident()?;
        if !matches!(self.peek().kind, TokenKind::Colon) {
            let t = self.peek().clone();
            self.diags.push(
                Diagnostic::error(format!("parameter `{name}` is missing a type"))
                    .with_label(name_span, "declared here")
                    .with_label(t.span, "expected `: type` here")
                    .with_note("write `name: type`, e.g. `a: i64`")
                    .with_code("E104"),
            );
            return Some(Param {
                name,
                name_span,
                ty: None,
                ty_span: None,
            });
        }
        self.bump(); // `:`
        match self.parse_type() {
            Some((ty, ty_span)) => {
                if ty.is_void() {
                    self.diags.push(
                        Diagnostic::error(format!("parameter `{name}` cannot be `void`"))
                            .with_label(ty_span, "`void` is not a value type")
                            .with_code("E104"),
                    );
                    return Some(Param {
                        name,
                        name_span,
                        ty: None,
                        ty_span: Some(ty_span),
                    });
                }
                Some(Param {
                    name,
                    name_span,
                    ty: Some(ty),
                    ty_span: Some(ty_span),
                })
            }
            None => Some(Param {
                name,
                name_span,
                ty: None,
                ty_span: None,
            }),
        }
    }

    fn parse_function_item(&mut self) -> Option<Item> {
        let function_tok = self.bump(); // `function`
        let (name, name_span) = self.parse_ident()?;
        self.expect(&TokenKind::LParen, "`(`")?;
        let mut params = Vec::new();
        if !matches!(self.peek().kind, TokenKind::RParen) {
            loop {
                {
                    let p = self.parse_param()?;
                    params.push(p)
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
        // An omitted return type means `void`: `function main() { ... }` is
        // `function main() { ... }`. Only a failed `: type` parse
        // leaves `ret` as `None` (already reported; downstream stays quiet).
        let (ret, ret_span) = if matches!(self.peek().kind, TokenKind::Colon) {
            self.bump(); // `:`
            match self.parse_type() {
                Some((ty, span)) => (Some(ty), Some(span)),
                None => (None, None),
            }
        } else {
            (Some(VlType::Void), None)
        };
        self.expect(&TokenKind::LBrace, "`{` for the function body")?;
        let mut body = Vec::new();
        while !self.at_eof() && !matches!(self.peek().kind, TokenKind::RBrace) {
            match self.parse_stmt() {
                Some(s) => body.push(s),
                None => {
                    let before = self.pos;
                    self.recover_to_stmt_boundary();
                    if self.pos == before && !self.at_eof() {
                        self.bump();
                    }
                }
            }
        }
        let close = self.expect(&TokenKind::RBrace, "`}`")?;
        Some(Item::Function {
            name,
            name_span,
            params,
            ret,
            ret_span,
            body,
            span: Span::new(function_tok.span.start, close.span.end),
        })
    }

    fn parse_stmt(&mut self) -> Option<Stmt> {
        if matches!(self.peek().kind, TokenKind::If) {
            return self.parse_if_stmt();
        }
        if matches!(self.peek().kind, TokenKind::While) {
            return self.parse_while_stmt();
        }
        if matches!(self.peek().kind, TokenKind::Break) {
            let t = self.bump();
            let semi = self.expect(&TokenKind::Semi, "`;`")?;
            return Some(Stmt::Break {
                span: Span::new(t.span.start, semi.span.end),
            });
        }
        if matches!(self.peek().kind, TokenKind::Continue) {
            let t = self.bump();
            let semi = self.expect(&TokenKind::Semi, "`;`")?;
            return Some(Stmt::Continue {
                span: Span::new(t.span.start, semi.span.end),
            });
        }
        if matches!(self.peek().kind, TokenKind::Return) {
            let t = self.bump();
            // Bare `return;` yields no value (for `void` functions);
            // `return expr;` yields `expr`. The `;` is mandatory either way.
            let value = if matches!(self.peek().kind, TokenKind::Semi) {
                None
            } else {
                Some(self.parse_expr()?)
            };
            let semi = self.expect(&TokenKind::Semi, "`;`")?;
            return Some(Stmt::Return {
                value,
                span: Span::new(t.span.start, semi.span.end),
            });
        }
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
        } else if matches!(self.peek().kind, TokenKind::Ident(_))
            && matches!(
                self.toks.get(self.pos + 1).map(|t| &t.kind),
                Some(TokenKind::LBracket)
            )
        {
            // Possible element write `a[i] = v;`. Parse only the postfix
            // base (not a full expression, so `a[i] == 1;` still parses as a
            // comparison statement): when no `=` follows, rewind and fall
            // through to the expression-statement path.
            let save = self.pos;
            let base = self.parse_postfix()?;
            if !matches!(self.peek().kind, TokenKind::Eq) {
                self.pos = save;
                let value = self.parse_expr()?;
                self.expect(&TokenKind::Semi, "`;`")?;
                Some(Stmt::Expr(value))
            } else {
                self.bump(); // `=`
                let value = self.parse_expr()?;
                let semi = self.expect(&TokenKind::Semi, "`;`")?;
                match base {
                    Expr::Index { base, index, span } => Some(Stmt::IndexAssign {
                        array: base,
                        index,
                        value: Box::new(value),
                        span: Span::new(span.start, semi.span.end),
                    }),
                    other => {
                        self.diags.push(
                            Diagnostic::error("cannot assign to this expression")
                                .with_label(
                                    other.span(),
                                    "only variables and array elements are assignable",
                                )
                                .with_code("E103"),
                        );
                        None
                    }
                }
            }
        } else if matches!(self.peek().kind, TokenKind::Ident(_))
            && matches!(
                self.toks.get(self.pos + 1).map(|t| &t.kind),
                Some(TokenKind::Eq)
            )
        {
            let (name, name_span) = self.parse_ident_opt().expect("checked above");
            self.bump(); // `=`
            let value = self.parse_expr()?;
            let semi = self.expect(&TokenKind::Semi, "`;`")?;
            Some(Stmt::Assign {
                name,
                name_span,
                value,
                span: Span::new(name_span.start, semi.span.end),
            })
        } else {
            let value = self.parse_expr()?;
            self.expect(&TokenKind::Semi, "`;`")?;
            Some(Stmt::Expr(value))
        }
    }

    fn parse_if_stmt(&mut self) -> Option<Stmt> {
        let start = self.bump().span.start;
        self.expect(&TokenKind::LParen, "`(` after `if`")?;
        let condition = self.parse_expr()?;
        self.expect(&TokenKind::RParen, "`)` after condition")?;
        let then_body = self.parse_branch()?;
        let else_body = if matches!(self.peek().kind, TokenKind::Else) {
            self.bump();
            Some(self.parse_branch()?)
        } else {
            None
        };
        let end = else_body
            .as_ref()
            .and_then(|_| self.toks.get(self.pos.saturating_sub(1)))
            .map_or_else(|| condition.span().end, |t| t.span.end);
        Some(Stmt::If {
            condition,
            then_body,
            else_body,
            span: Span::new(start, end),
        })
    }

    fn parse_while_stmt(&mut self) -> Option<Stmt> {
        let start = self.bump().span.start; // `while`
        self.expect(&TokenKind::LParen, "`(` after `while`")?;
        let condition = self.parse_expr()?;
        self.expect(&TokenKind::RParen, "`)` after condition")?;
        let body = self.parse_branch()?;
        let end = self
            .toks
            .get(self.pos.saturating_sub(1))
            .map_or_else(|| condition.span().end, |t| t.span.end);
        Some(Stmt::While {
            condition,
            body,
            span: Span::new(start, end),
        })
    }

    fn parse_block(&mut self) -> Option<Vec<Stmt>> {
        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut body = Vec::new();
        while !self.at_eof() && !matches!(self.peek().kind, TokenKind::RBrace) {
            match self.parse_stmt() {
                Some(stmt) => body.push(stmt),
                None => {
                    let before = self.pos;
                    self.recover_to_stmt_boundary();
                    if self.pos == before && !self.at_eof() {
                        self.bump();
                    }
                }
            }
        }
        self.expect(&TokenKind::RBrace, "`}`")?;
        Some(body)
    }

    fn parse_branch(&mut self) -> Option<Vec<Stmt>> {
        if matches!(self.peek().kind, TokenKind::LBrace) {
            self.parse_block()
        } else {
            Some(vec![self.parse_stmt()?])
        }
    }

    fn recover_to_stmt_boundary(&mut self) {
        while !self.at_eof() {
            match &self.peek().kind {
                TokenKind::Semi => {
                    self.bump();
                    return;
                }
                TokenKind::RBrace
                | TokenKind::Let
                | TokenKind::Function
                | TokenKind::If
                | TokenKind::While
                | TokenKind::Break
                | TokenKind::Continue
                | TokenKind::Return => return,
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

    fn parse_path(&mut self) -> Option<Vec<String>> {
        let mut path = vec![self.parse_ident()?.0];
        while matches!(self.peek().kind, TokenKind::Dot) {
            if matches!(
                self.toks.get(self.pos + 1).map(|t| &t.kind),
                Some(TokenKind::LBrace)
            ) {
                break;
            }
            self.bump();
            path.push(self.parse_ident()?.0);
        }
        Some(path)
    }

    fn parse_expr(&mut self) -> Option<Expr> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Option<Expr> {
        let mut lhs = self.parse_and()?;
        while matches!(self.peek().kind, TokenKind::PipePipe) {
            self.bump();
            let rhs = self.parse_and()?;
            let span = lhs.span().merge(rhs.span());
            lhs = Expr::Binary {
                op: BinOp::Or,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        Some(lhs)
    }

    fn parse_and(&mut self) -> Option<Expr> {
        let mut lhs = self.parse_equality()?;
        while matches!(self.peek().kind, TokenKind::AmpAmp) {
            self.bump();
            let rhs = self.parse_equality()?;
            let span = lhs.span().merge(rhs.span());
            lhs = Expr::Binary {
                op: BinOp::And,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        Some(lhs)
    }

    fn parse_equality(&mut self) -> Option<Expr> {
        let mut lhs = self.parse_comparison()?;
        loop {
            let op = match &self.peek().kind {
                TokenKind::EqEq => BinOp::Eq,
                TokenKind::BangEq => BinOp::Ne,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_comparison()?;
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

    fn parse_comparison(&mut self) -> Option<Expr> {
        let mut lhs = self.parse_add()?;
        loop {
            let op = match &self.peek().kind {
                TokenKind::Lt => BinOp::Lt,
                TokenKind::LtEq => BinOp::Le,
                TokenKind::Gt => BinOp::Gt,
                TokenKind::GtEq => BinOp::Ge,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_add()?;
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

    fn parse_add(&mut self) -> Option<Expr> {
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
        let mut lhs = self.parse_unary()?;
        loop {
            let op = match &self.peek().kind {
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_unary()?;
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

    fn parse_unary(&mut self) -> Option<Expr> {
        match &self.peek().kind {
            TokenKind::Minus => {
                let t = self.bump();
                let rhs = self.parse_unary()?;
                let span = Span::new(t.span.start, rhs.span().end);
                Some(Expr::Unary {
                    op: UnOp::Neg,
                    rhs: Box::new(rhs),
                    span,
                })
            }
            TokenKind::Bang => {
                let t = self.bump();
                let rhs = self.parse_unary()?;
                let span = Span::new(t.span.start, rhs.span().end);
                Some(Expr::Unary {
                    op: UnOp::Not,
                    rhs: Box::new(rhs),
                    span,
                })
            }
            _ => self.parse_factor(),
        }
    }

    fn parse_factor(&mut self) -> Option<Expr> {
        self.parse_postfix()
    }

    /// Postfix indexing: `primary` followed by any number of `[expr]`.
    /// Array literals consume their own brackets, so `[1u64][0]` reads
    /// element 0 of a one-element literal.
    fn parse_postfix(&mut self) -> Option<Expr> {
        let mut base = self.parse_primary()?;
        while matches!(self.peek().kind, TokenKind::LBracket) {
            self.bump(); // `[`
            let index = self.parse_expr()?;
            let close = self.expect(&TokenKind::RBracket, "`]`")?;
            let span = Span::new(base.span().start, close.span.end);
            base = Expr::Index {
                base: Box::new(base),
                index: Box::new(index),
                span,
            };
        }
        Some(base)
    }

    fn parse_primary(&mut self) -> Option<Expr> {
        let t = self.peek().clone();
        match t.kind {
            TokenKind::I64(v) => {
                self.bump();
                Some(Expr::Literal(Scalar::I64(v), t.span))
            }
            TokenKind::U64(v) => {
                self.bump();
                Some(Expr::Literal(Scalar::U64(v), t.span))
            }
            TokenKind::F64(v) => {
                self.bump();
                Some(Expr::Literal(Scalar::F64(v), t.span))
            }
            TokenKind::U8(v) => {
                self.bump();
                Some(Expr::Literal(Scalar::U8(v), t.span))
            }
            TokenKind::Bool(v) => {
                self.bump();
                Some(Expr::Literal(Scalar::Bool(v), t.span))
            }
            TokenKind::String(value) => {
                self.bump();
                Some(Expr::String(value, t.span))
            }
            TokenKind::Ident(_) => {
                let path = self.parse_path()?;
                let end = self.toks[self.pos.saturating_sub(1)].span.end;
                if matches!(self.peek().kind, TokenKind::LParen) {
                    self.bump(); // `(`
                    let mut args = Vec::new();
                    if !matches!(self.peek().kind, TokenKind::RParen) {
                        loop {
                            args.push(self.parse_expr()?);
                            match &self.peek().kind {
                                TokenKind::Comma => {
                                    self.bump();
                                }
                                _ => break,
                            }
                        }
                    }
                    let close = self.expect(&TokenKind::RParen, "`)`")?;
                    let span = Span::new(t.span.start, close.span.end);
                    Some(Expr::Call {
                        callee: path,
                        callee_span: Span::new(t.span.start, end),
                        args,
                        span,
                    })
                } else {
                    Some(Expr::Var {
                        path,
                        span: Span::new(t.span.start, end),
                    })
                }
            }
            TokenKind::LParen => {
                self.bump();
                let inner = self.parse_expr()?;
                self.expect(&TokenKind::RParen, "`)`")?;
                Some(inner)
            }
            TokenKind::LBracket => {
                let open = self.bump();
                let mut elems = Vec::new();
                if !matches!(self.peek().kind, TokenKind::RBracket) {
                    loop {
                        elems.push(self.parse_expr()?);
                        if !matches!(self.peek().kind, TokenKind::Comma) {
                            break;
                        }
                        self.bump();
                        // Allow one trailing comma: `[1u64,]`.
                        if matches!(self.peek().kind, TokenKind::RBracket) {
                            break;
                        }
                    }
                }
                let close = self.expect(&TokenKind::RBracket, "`]`")?;
                Some(Expr::ArrayLiteral {
                    elems,
                    span: Span::new(open.span.start, close.span.end),
                })
            }
            TokenKind::Invalid => {
                self.bump();
                None
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
        TokenKind::I64(v) => format!("integer `{v}`"),
        TokenKind::U64(v) => format!("u64 literal `{v}`"),
        TokenKind::F64(v) => format!("f64 literal `{}`", f64::from_bits(*v)),
        TokenKind::U8(v) => format!("u8 literal `{v}`"),
        TokenKind::Bool(v) => format!("boolean `{v}`"),
        TokenKind::String(_) => "string literal".into(),
        TokenKind::Let => "`let`".into(),
        TokenKind::Function => "`function`".into(),
        TokenKind::If => "`if`".into(),
        TokenKind::Else => "`else`".into(),
        TokenKind::While => "`while`".into(),
        TokenKind::Break => "`break`".into(),
        TokenKind::Continue => "`continue`".into(),
        TokenKind::Return => "`return`".into(),
        TokenKind::Plus => "`+`".into(),
        TokenKind::Minus => "`-`".into(),
        TokenKind::Star => "`*`".into(),
        TokenKind::Slash => "`/`".into(),
        TokenKind::Eq => "`=`".into(),
        TokenKind::EqEq => "`==`".into(),
        TokenKind::Bang => "`!`".into(),
        TokenKind::BangEq => "`!=`".into(),
        TokenKind::Lt => "`<`".into(),
        TokenKind::LtEq => "`<=`".into(),
        TokenKind::Gt => "`>`".into(),
        TokenKind::GtEq => "`>=`".into(),
        TokenKind::AmpAmp => "`&&`".into(),
        TokenKind::PipePipe => "`||`".into(),
        TokenKind::Semi => "`;`".into(),
        TokenKind::LParen => "`(`".into(),
        TokenKind::RParen => "`)`".into(),
        TokenKind::LBrace => "`{`".into(),
        TokenKind::RBrace => "`}`".into(),
        TokenKind::LBracket => "`[`".into(),
        TokenKind::RBracket => "`]`".into(),
        TokenKind::Comma => "`,`".into(),
        TokenKind::Dot => "`.`".into(),
        TokenKind::Colon => "`:`".into(),
        TokenKind::Invalid => "invalid token".into(),
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

    #[test]
    fn expression_statement_requires_semi() {
        let (_prog, diags) = parse_src("function main() { d }");
        assert!(!diags.is_empty());
    }

    #[test]
    fn function_keyword_parses_with_semi_body() {
        let (prog, diags) = parse_src("function main() { d; }");
        assert!(diags.is_empty());
        assert_eq!(prog.items.len(), 1);
    }

    #[test]
    fn typed_params_and_void_return_parse() {
        let (prog, diags) = parse_src("function add(a: i64, b: i64): i64 { return a + b; }");
        assert!(diags.is_empty(), "{diags:?}");
        match &prog.items[0] {
            Item::Function { params, ret, .. } => {
                assert_eq!(params.len(), 2);
                assert_eq!(params[0].ty, Some(VlType::I64));
                assert_eq!(*ret, Some(VlType::I64));
            }
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn missing_param_type_is_an_error() {
        let (_prog, diags) = parse_src("function add(a): i64 { return a; }");
        assert!(diags.iter().any(|d| d.code.as_deref() == Some("E104")));
    }

    #[test]
    fn omitted_return_type_defaults_to_void() {
        let (prog, diags) = parse_src("function main() { 1; }");
        assert!(diags.is_empty(), "{diags:?}");
        match &prog.items[0] {
            Item::Function { ret, .. } => assert_eq!(*ret, Some(VlType::Void)),
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn void_param_is_an_error() {
        let (_prog, diags) = parse_src("function f(x: void): void { return; }");
        assert!(diags.iter().any(|d| d.message.contains("cannot be `void`")));
    }

    #[test]
    fn unknown_type_is_an_error() {
        let (_prog, diags) = parse_src("function f(x: bogus): void { return; }");
        assert!(diags.iter().any(|d| d.code.as_deref() == Some("E105")));
    }

    #[test]
    fn call_with_no_args_parses() {
        let (prog, diags) = parse_src("function main() { foo(); }");
        assert!(diags.is_empty());
        match &prog.items[0] {
            Item::Function { body, .. } => match &body[0] {
                Stmt::Expr(Expr::Call { callee, args, .. }) => {
                    assert_eq!(callee, &vec!["foo".to_string()]);
                    assert!(args.is_empty());
                }
                other => panic!("expected call, got {other:?}"),
            },
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn call_with_args_and_nesting_parses() {
        let (prog, diags) = parse_src("function main() { add(1, mul(2, 3)); }");
        assert!(diags.is_empty());
        match &prog.items[0] {
            Item::Function { body, .. } => match &body[0] {
                Stmt::Expr(Expr::Call { callee, args, .. }) => {
                    assert_eq!(callee, &vec!["add".to_string()]);
                    assert_eq!(args.len(), 2);
                    assert!(matches!(args[1], Expr::Call { .. }));
                }
                other => panic!("expected call, got {other:?}"),
            },
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn call_binds_tighter_than_add() {
        let (prog, diags) = parse_src("let x = f(1) + 2;");
        assert!(diags.is_empty());
        match &prog.items[0] {
            Item::Let { value, .. } => assert!(matches!(value, Expr::Binary { .. })),
            other => panic!("expected let, got {other:?}"),
        }
    }

    #[test]
    fn parses_string_literal() {
        let (prog, diags) = parse_src(r#"let s = "hello";"#);
        assert!(diags.is_empty());
        assert!(matches!(
            &prog.items[0],
            Item::Let { value: Expr::String(value, _), .. } if value == b"hello"
        ));
    }

    #[test]
    fn parses_module_use_and_qualified_call() {
        let (prog, diags) =
            parse_src("use std.string.{len}; function main() { string.len(\"s\"); }");
        assert!(diags.is_empty(), "{diags:?}");
        assert!(
            matches!(&prog.items[0], Item::Use { path, names: Some(names), .. } if path == &vec![String::from("std"), String::from("string")] && names.len() == 1)
        );
    }

    #[test]
    fn parses_while_break_continue_and_assign() {
        let (prog, diags) = parse_src(
            "function main() { let i = 0; while (i < 10) { i = i + 1; if (i == 2) { continue; } break; } }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        match &prog.items[0] {
            Item::Function { body, .. } => {
                assert!(matches!(body[1], Stmt::While { .. }));
                match &body[1] {
                    Stmt::While { body, .. } => {
                        assert!(matches!(body[0], Stmt::Assign { .. }));
                        match &body[1] {
                            Stmt::If { then_body, .. } => {
                                assert!(matches!(then_body[0], Stmt::Continue { .. }))
                            }
                            other => panic!("expected if, got {other:?}"),
                        }
                        assert!(matches!(body[2], Stmt::Break { .. }));
                    }
                    other => panic!("expected while, got {other:?}"),
                }
            }
            other => panic!("expected function, got {other:?}"),
        }
    }

    #[test]
    fn logical_operators_bind_looser_than_comparison() {
        let (prog, diags) = parse_src("function main() { let x = 1; x + 1 == 2 && !x; }");
        assert!(diags.is_empty(), "{diags:?}");
        match &prog.items[0] {
            Item::Function { body, .. } => match &body[1] {
                Stmt::Expr(Expr::Binary {
                    op: BinOp::And,
                    lhs,
                    rhs,
                    ..
                }) => {
                    assert!(matches!(**lhs, Expr::Binary { op: BinOp::Eq, .. }));
                    assert!(matches!(**rhs, Expr::Unary { op: UnOp::Not, .. }));
                }
                other => panic!("expected &&, got {other:?}"),
            },
            other => panic!("expected function, got {other:?}"),
        }
    }

    #[test]
    fn parses_unbraced_conditional_branches() {
        let (prog, diags) = parse_src("function main() { if (true) 1; else 2; }");
        assert!(diags.is_empty(), "{diags:?}");
        match &prog.items[0] {
            Item::Function { body, .. } => assert!(matches!(body[0], Stmt::If { .. })),
            other => panic!("expected function, got {other:?}"),
        }
    }

    #[test]
    fn nested_function_recovery_makes_progress() {
        let (toks, _) = vl_lex::lex("function main() { function nested() {} } function tail() {}");
        let (prog, diags) = parse(&toks, "");
        assert!(!diags.is_empty());
        assert!(prog
            .items
            .iter()
            .any(|item| matches!(item, Item::Function { name, .. } if name == "tail")));
    }

    #[test]
    fn shadowing_is_a_warning_only_placeholder() {
        // (resolver test covers shadowing; parser just needs a valid body)
        let (prog, diags) = parse_src("function f(x: i64): i64 { return x; }");
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(prog.items.len(), 1);
    }

    #[test]
    fn parses_array_literal_index_and_index_assign() {
        let (prog, diags) = parse_src(
            "function get(a: U64Array): u64 { a[0u64] = 1u64; return a[0u64]; } function main() { let b = [1u64, 2u64,]; let e = []; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        match &prog.items[0] {
            Item::Function { params, body, .. } => {
                assert_eq!(params[0].ty, Some(VlType::U64Array));
                assert!(matches!(body[0], Stmt::IndexAssign { .. }));
                assert!(matches!(
                    body[1],
                    Stmt::Return {
                        value: Some(Expr::Index { .. }),
                        ..
                    }
                ));
            }
            other => panic!("expected fn, got {other:?}"),
        }
        match &prog.items[1] {
            Item::Function { body, .. } => {
                assert!(matches!(
                    &body[0],
                    Stmt::Let {
                        value: Expr::ArrayLiteral { elems, .. },
                        ..
                    } if elems.len() == 2
                ));
                assert!(matches!(
                    &body[1],
                    Stmt::Let {
                        value: Expr::ArrayLiteral { elems, .. },
                        ..
                    } if elems.is_empty()
                ));
            }
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn unclosed_index_is_an_error() {
        let (_prog, diags) = parse_src("function main() { a[0u64; }");
        assert!(!diags.is_empty());
    }

    #[test]
    fn return_with_value_and_bare_return_parse() {
        let (prog, diags) = parse_src("function f(): i64 { return 1; } function g() { return; }");
        assert!(diags.is_empty(), "{diags:?}");
        match &prog.items[0] {
            Item::Function { body, .. } => {
                assert!(matches!(body[0], Stmt::Return { value: Some(_), .. }))
            }
            other => panic!("expected fn, got {other:?}"),
        }
        match &prog.items[1] {
            Item::Function { body, .. } => {
                assert!(matches!(body[0], Stmt::Return { value: None, .. }))
            }
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn return_requires_semi() {
        let (_prog, diags) = parse_src("function f(): i64 { return 1 }");
        assert!(!diags.is_empty());
    }

    #[test]
    fn lexical_poison_does_not_hide_later_function() {
        let (toks, lex_diags) = vl_lex::lex("let broken = @; function tail() {} ");
        assert_eq!(lex_diags.len(), 1);
        let (prog, parse_diags) = parse(&toks, "");
        assert!(parse_diags.is_empty(), "{parse_diags:?}");
        assert!(prog
            .items
            .iter()
            .any(|item| matches!(item, Item::Function { name, .. } if name == "tail")));
    }
}
