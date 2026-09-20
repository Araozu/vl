//! vl-syntax: recursive-descent parser, tokens -> AST.
//!
//! Grammar (v0, TypeScript-like surface):
//! ```text
//! program := item*
//! item    := `use` ... | `let` ident (`:` type)? `=` expr `;` | `function` ident type-params? `(` params? `)` (`:` type)? block | `type` ident `=` `object` `{` object-fields? `}` `;`
//! type-params := `[` type-param (`,` type-param)* `]`
//! type-param  := ident (`extends` (`Numeric` | `Comparable`))?
//! object-fields := object-field (`,` object-field)* `,`?
//! object-field := ident `:` type
//! params  := param (`,` param)*
//! param   := ident `:` type
//! type    := `u64` | `i64` | `f64` | `bool` | `u8` | `String` | `File` | object-name | `Array` `[` type `]` | type-param | `void` (`void` only as return)
//! block   := `{` stmt* `}`
//! stmt    := `let` ident (`:` type)? `=` expr `;` | ident `=` expr `;` | index `=` expr `;` | field `=` expr `;`
//!          | `if` `(` expr `)` branch (`else` branch)?
//!          | `while` `(` expr `)` branch | `break` `;` | `continue` `;`
//!          | `return` expr? `;` | expr `;`
//! branch  := block | stmt
//! index   := ident (`[` expr `]`)+
//! expr    := or
//! or      := and (`||` and)*
//! and     := equality (`&&` equality)*
//! equality:= cast ((`==`|`!=`) cast)*
//! cast    := comparison (`as` type)*
//! comparison := term ((`<`|`<=`|`>`|`>=`) term)*
//! term    := factor ((`+`|`-`) factor)*
//! factor  := unary ((`*`|`/`) unary)*
//! unary   := (`-`|`!`) unary | postfix
//! postfix := primary (`[` expr `]` | `.` ident)*
//! primary := literal | string | array-literal | object-literal | call | path | `(` expr `)`
//! array-literal := `[` (expr (`,` expr)* `,`?)? `]`
//! object-literal := ident `{` (ident `:` expr (`,` ident `:` expr)* `,`?)? `}`
//! call    := path (`::` `[` type (`,` type)* `]`)? `(` args? `)`
//! path    := ident (`.` ident)*
//! args    := expr (`,` expr)*
//! ```
//!
//! `Array[T]` is a fixed-length heap array of `T`: `Array.new::[u64](n)`
//! creates a zero-filled array of `n` elements, `[1u64, 2u64]` is an array
//! literal, `a[i]` reads element `i`, and `a[i] = v;` writes it. Indices are
//! always `u64`; elements have the array's `T`.
//!
//! Objects are named reference types: `type Name = object { field: type, };`
//! creates a heap object, `Name { field: value }` initializes one, and field
//! assignment mutates the shared object visible through every alias.
//!
//! Generic functions declare type parameters after the name
//! (`function first[T](a: Array[T]): T { ... }`, optionally bounded as
//! `function add[T extends Numeric](a: T, b: T): T`). Calls infer them from
//! the value arguments (`first(a)`) or pass them explicitly with a turbofish
//! (`first::[u64](a)`). `f[T](args)` without `::` is *not* a generic call —
//! it parses as indexing `f[T]` (which is not callable), and the parser says
//! so explicitly.
//!
//! Explicit numeric conversions use TypeScript-like `as` (`value as u8`):
//! integer-to-integer casts with no runtime cost (literals are range-checked
//! at compile time; variable conversions are unchecked reinterpretations).
//! Implicit cross-integer conversions stay narrow (literals only).
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
use vl_common::{Diagnostic, Span};
pub use vl_common::{GenericBound, VlType};
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

/// One declared type parameter: `T` in `function first[T](...)`, optionally
/// bounded (`T extends Numeric`). Bounds enable operators on otherwise-opaque
/// `T` without full subtyping.
#[derive(Debug, Clone)]
pub struct TypeParam {
    pub name: String,
    pub span: Span,
    pub bound: Option<GenericBound>,
}

/// One field in a user-defined object type.
#[derive(Debug, Clone)]
pub struct ObjectField {
    pub name: String,
    pub name_span: Span,
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
    Object {
        name: String,
        name_span: Span,
        fields: Vec<ObjectField>,
        span: Span,
    },
    Let {
        name: String,
        name_span: Span,
        /// Optional annotation (`let x: T = ...`). `None` with `ty_span`
        /// `None` means absent (infer); `None` with `Some` means invalid
        /// (already reported; downstream poisons quietly).
        ty: Option<VlType>,
        ty_span: Option<Span>,
        value: Expr,
        span: Span,
    },
    Function {
        name: String,
        name_span: Span,
        /// Declared type parameters (`[]` when monomorphic). Names are
        /// validated by the parser (distinct, not primitives).
        type_params: Vec<TypeParam>,
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
        /// Optional annotation, same encoding as [`Item::Let`].
        ty: Option<VlType>,
        ty_span: Option<Span>,
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
    FieldAssign {
        base: Box<Expr>,
        field: String,
        field_span: Span,
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
    /// Array literal: `[1u64, 2u64]`. Element types are enforced later
    /// (all elements must share one `T`, giving `Array[T]`). Empty `[]`
    /// has no element to infer from and is rejected by typechecking.
    ArrayLiteral {
        elems: Vec<Expr>,
        span: Span,
    },
    ObjectLiteral {
        name: String,
        name_span: Span,
        fields: Vec<(String, Span, Expr)>,
        span: Span,
    },
    /// Element read: `array[index]`.
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
        span: Span,
    },
    Field {
        base: Box<Expr>,
        name: String,
        span: Span,
    },
    Var {
        path: Vec<String>,
        span: Span,
    },
    Call {
        callee: Vec<String>,
        callee_span: Span,
        /// Explicit type arguments from a turbofish (`f::[u64](...)`).
        /// Empty when inference should fill them in (`f(...)`).
        type_args: Vec<VlType>,
        type_args_span: Option<Span>,
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
    /// Explicit numeric conversion (`value as u8`, TypeScript-like).
    /// Integer-to-integer only in v0; literals are range-checked at compile
    /// time, variable conversions are unchecked (no runtime cost).
    Cast {
        inner: Box<Expr>,
        target: VlType,
        target_span: Span,
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
            Expr::ObjectLiteral { span, .. } => *span,
            Expr::Index { span, .. } => *span,
            Expr::Field { span, .. } => *span,
            Expr::Var { span, .. } => *span,
            Expr::Call { span, .. } => *span,
            Expr::Unary { span, .. } | Expr::Binary { span, .. } | Expr::Cast { span, .. } => *span,
        }
    }
}

// -------------------------------------------------------------- Parser ---

struct Parser<'a> {
    toks: &'a [Token],
    pos: usize,
    diags: Vec<Diagnostic>,
    known_objects: std::collections::HashSet<String>,
}

pub fn parse(toks: &[Token], src: &str) -> (Program, Vec<Diagnostic>) {
    parse_with_module(toks, src, "<anonymous>")
}

pub fn parse_with_module(toks: &[Token], _src: &str, module: &str) -> (Program, Vec<Diagnostic>) {
    let known_objects = toks
        .windows(4)
        .filter_map(|window| {
            match (
                &window[0].kind,
                &window[1].kind,
                &window[2].kind,
                &window[3].kind,
            ) {
                (TokenKind::Type, TokenKind::Ident(name), TokenKind::Eq, TokenKind::Object) => {
                    Some(name.clone())
                }
                _ => None,
            }
        })
        .collect();
    let mut p = Parser {
        toks,
        pos: 0,
        diags: vec![],
        known_objects,
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
                TokenKind::Let | TokenKind::Function | TokenKind::Type => return,
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
            TokenKind::Type => self.parse_object_item(),
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
                        "expected an item (`use`, `let`, `function` or `type`), found {}",
                        describe(&t.kind)
                    ))
                    .with_label(
                        t.span,
                        "items start with `use`, `let`, `function`, or `type`",
                    )
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

    fn parse_object_item(&mut self) -> Option<Item> {
        let type_tok = self.bump();
        let (name, name_span) = self.parse_ident()?;
        let reserved_name = matches!(name.as_str(), "Array" | "U64Array" | "string" | "file")
            || name.parse::<VlType>().is_ok();
        if reserved_name {
            self.diags.push(
                Diagnostic::error(format!("object type name `{name}` is reserved"))
                    .with_label(name_span, "choose a different object type name")
                    .with_note("object names cannot shadow built-in or legacy type names")
                    .with_code("E200"),
            );
        }
        self.expect(&TokenKind::Eq, "`=` after object name")?;
        self.expect(&TokenKind::Object, "`object` after `=`")?;
        self.expect(&TokenKind::LBrace, "`{` after `object`")?;
        let mut fields = Vec::new();
        while !self.at_eof() && !matches!(self.peek().kind, TokenKind::RBrace) {
            let (field_name, field_span) = self.parse_ident()?;
            self.expect(&TokenKind::Colon, "`:` after field name")?;
            let fallback = self.peek().clone();
            let (ty, ty_span) = match self.parse_type(&[], true) {
                Some((ty, span)) if !ty.is_void() => (Some(ty), Some(span)),
                Some((_ty, span)) => {
                    self.diags.push(
                        Diagnostic::error("an object field cannot be `void`")
                            .with_label(span, "`void` is not a value type")
                            .with_code("E104"),
                    );
                    (None, Some(span))
                }
                None => (None, Some(fallback.span)),
            };
            if fields.iter().any(|f: &ObjectField| f.name == field_name) {
                self.diags.push(
                    Diagnostic::error(format!("duplicate object field `{field_name}`"))
                        .with_label(field_span, "redefined here")
                        .with_code("E200"),
                );
            }
            fields.push(ObjectField {
                name: field_name,
                name_span: field_span,
                ty,
                ty_span,
            });
            if matches!(self.peek().kind, TokenKind::Comma) {
                self.bump();
                continue;
            }
            if !matches!(self.peek().kind, TokenKind::RBrace) {
                let t = self.peek().clone();
                self.diags.push(
                    Diagnostic::error(format!("expected `,` or `}}`, found {}", describe(&t.kind)))
                        .with_label(t.span, "separate object fields with commas")
                        .with_code("E100"),
                );
                return None;
            }
        }
        let close = self.expect(&TokenKind::RBrace, "`}` after object fields")?;
        let semi = self.expect(&TokenKind::Semi, "`;` after object declaration")?;
        Some(Item::Object {
            name,
            name_span,
            fields,
            span: Span::new(type_tok.span.start, semi.span.end.max(close.span.end)),
        })
    }

    fn parse_let_item(&mut self) -> Option<Item> {
        let let_tok = self.bump(); // `let`
        let (name, name_span) = self.parse_ident()?;
        let (ty, ty_span) = self.parse_let_ann(&[]);
        self.expect(&TokenKind::Eq, "`=`")?;
        let value = self.parse_expr()?;
        let semi = self.expect(&TokenKind::Semi, "`;`")?;
        Some(Item::Let {
            name,
            name_span,
            ty,
            ty_span,
            value,
            span: Span::new(let_tok.span.start, semi.span.end),
        })
    }

    /// Parse an optional `let` annotation (`: type`). Absent means
    /// `(None, None)` (infer); a failed annotation reports and yields
    /// `(None, Some(span))` so downstream poisons quietly. `void` is
    /// rejected: a `let` always binds a value.
    fn parse_let_ann(&mut self, allowed: &[String]) -> (Option<VlType>, Option<Span>) {
        if !matches!(self.peek().kind, TokenKind::Colon) {
            return (None, None);
        }
        self.bump(); // `:`
        let fallback = self.peek().clone();
        match self.parse_type(allowed, true) {
            Some((ty, span)) => {
                if ty.is_void() {
                    self.diags.push(
                        Diagnostic::error("a `let` binding cannot be `void`")
                            .with_label(span, "`void` is not a value")
                            .with_code("E104"),
                    );
                    (None, Some(span))
                } else {
                    (Some(ty), Some(span))
                }
            }
            None => (None, Some(fallback.span)),
        }
    }

    /// Parse a type. `allowed` holds the enclosing function's type parameter
    /// names: a bare unknown identifier resolves to `Param(name)` only when
    /// listed there, otherwise it is E105. `strict` controls whether an
    /// unlisted unknown identifier is reported here (`true`, for declaration
    /// positions where the scope is known) or silently becomes a `Param`
    /// (`false`, for turbofish type arguments in expression position, where
    /// the scope is not threaded through — typechecking reports it).
    fn parse_type(&mut self, allowed: &[String], strict: bool) -> Option<(VlType, Span)> {
        let t = self.peek().clone();
        match t.kind {
            TokenKind::Ident(name) => {
                // `Array[T]`: the bracket must immediately follow. A bare
                // `Array` without arguments is an error (unless a type
                // parameter literally named `Array` is in scope).
                if name == "Array"
                    && !allowed.iter().any(|a| a == "Array")
                    && matches!(
                        self.toks.get(self.pos + 1).map(|t| &t.kind),
                        Some(TokenKind::LBracket)
                    )
                {
                    return self.parse_array_type(allowed, strict);
                }
                self.bump();
                if name == "Array" && !allowed.iter().any(|a| a == "Array") {
                    self.diags.push(
                        Diagnostic::error("`Array` expects an element type")
                            .with_label(t.span, "write `Array[T]`, e.g. `Array[u64]`")
                            .with_code("E104"),
                    );
                    return None;
                }
                // Removed predecessor type: point at the replacement.
                if name == "U64Array" {
                    self.diags.push(
                        Diagnostic::error("unknown type `U64Array`")
                            .with_label(t.span, "`U64Array` was removed; use `Array[u64]`")
                            .with_code("E105"),
                    );
                    return None;
                }
                // `string` was renamed to `String`: reference types are
                // uppercase, only value-semantics primitives stay lowercase.
                if name == "string" {
                    self.diags.push(
                        Diagnostic::error("unknown type `string`")
                            .with_label(t.span, "`string` was renamed; use `String`")
                            .with_code("E105"),
                    );
                    return None;
                }
                // `file` was never a value type spelling: the reference type
                // is `File`.
                if name == "file" {
                    self.diags.push(
                        Diagnostic::error("unknown type `file`")
                            .with_label(t.span, "reference types are uppercase; use `File`")
                            .with_code("E105"),
                    );
                    return None;
                }
                match name.parse::<VlType>() {
                    Ok(ty) => Some((ty, t.span)),
                    Err(_) if allowed.iter().any(|a| a == &name) => {
                        Some((VlType::Param(name), t.span))
                    }
                    Err(_) if self.known_objects.contains(&name) => {
                        Some((VlType::Object(name), t.span))
                    }
                    Err(_) if !strict => Some((VlType::Param(name), t.span)),
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
                            "expected a built-in type, an object type, Array[T], or void",
                        )
                        .with_code("E104"),
                );
                None
            }
        }
    }

    /// Parse the `[T]` tail of `Array[T]` (the `Array` ident and the lookahead
    /// were already established by [`parse_type`](Self::parse_type)).
    fn parse_array_type(&mut self, allowed: &[String], strict: bool) -> Option<(VlType, Span)> {
        let head = self.bump(); // `Array`
        self.bump(); // `[` (established by lookahead)
        let (elem, _) = self.parse_type(allowed, strict)?;
        if elem.is_void() {
            self.diags.push(
                Diagnostic::error("`Array[void]` is not a value type")
                    .with_label(head.span, "`void` has no values to store")
                    .with_code("E104"),
            );
            return None;
        }
        let close = self.expect(&TokenKind::RBracket, "`]` after the element type")?;
        Some((
            VlType::Array(Box::new(elem)),
            Span::new(head.span.start, close.span.end),
        ))
    }

    fn parse_param(&mut self, allowed: &[String]) -> Option<Param> {
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
        match self.parse_type(allowed, true) {
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

    /// Parse `[T, U extends Numeric]` after a function name. Returns the
    /// declared type parameters (empty when there is no bracket). Reports
    /// duplicates, empty lists, shadowing of primitive type names, and
    /// unknown bounds (only `Numeric` and `Comparable` exist).
    fn parse_type_params(&mut self) -> Option<Vec<TypeParam>> {
        if !matches!(self.peek().kind, TokenKind::LBracket) {
            return Some(Vec::new());
        }
        self.bump(); // `[`
        let mut params = Vec::new();
        if matches!(self.peek().kind, TokenKind::RBracket) {
            let t = self.bump();
            self.diags.push(
                Diagnostic::error("expected at least one type parameter")
                    .with_label(t.span, "empty `[]` here")
                    .with_note("write `function f[T](...)` or drop the brackets")
                    .with_code("E104"),
            );
            return Some(params);
        }
        loop {
            match self.parse_ident() {
                Some((name, span)) => {
                    if name.parse::<VlType>().is_ok() {
                        self.diags.push(
                            Diagnostic::error(format!(
                                "type parameter `{name}` shadows a primitive type"
                            ))
                            .with_label(span, "pick another name, e.g. `T`")
                            .with_code("E104"),
                        );
                    } else if params.iter().any(|p: &TypeParam| p.name == name) {
                        self.diags.push(
                            Diagnostic::error(format!("duplicate type parameter `{name}`"))
                                .with_label(span, "redefined here")
                                .with_code("E200"),
                        );
                    } else {
                        let bound = self.parse_opt_extends()?;
                        params.push(TypeParam { name, span, bound });
                    }
                }
                None => return None,
            }
            match &self.peek().kind {
                TokenKind::Comma => {
                    self.bump();
                }
                _ => break,
            }
        }
        self.expect(&TokenKind::RBracket, "`]` after type parameters")?;
        Some(params)
    }

    /// Parse an optional `extends Bound` tail (`Numeric` or `Comparable`).
    fn parse_opt_extends(&mut self) -> Option<Option<GenericBound>> {
        if !matches!(self.peek().kind, TokenKind::Extends) {
            return Some(None);
        }
        self.bump(); // `extends`
        let t = self.peek().clone();
        match &t.kind {
            TokenKind::Ident(name) => {
                self.bump();
                match name.parse::<GenericBound>() {
                    Ok(bound) => Some(Some(bound)),
                    Err(_) => {
                        self.diags.push(
                            Diagnostic::error(format!("unknown bound `{name}`"))
                                .with_label(t.span, "expected `Numeric` or `Comparable`")
                                .with_note("write `T extends Numeric` or `T extends Comparable`")
                                .with_code("E105"),
                        );
                        None
                    }
                }
            }
            _ => {
                self.diags.push(
                    Diagnostic::error(format!("expected a bound, found {}", describe(&t.kind)))
                        .with_label(t.span, "expected `Numeric` or `Comparable` here")
                        .with_code("E104"),
                );
                None
            }
        }
    }

    fn parse_function_item(&mut self) -> Option<Item> {
        let function_tok = self.bump(); // `function`
        let (name, name_span) = self.parse_ident()?;
        let type_params = self.parse_type_params()?;
        let allowed: Vec<String> = type_params.iter().map(|p| p.name.clone()).collect();
        self.expect(&TokenKind::LParen, "`(`")?;
        let mut params = Vec::new();
        if !matches!(self.peek().kind, TokenKind::RParen) {
            loop {
                {
                    let p = self.parse_param(&allowed)?;
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
            match self.parse_type(&allowed, true) {
                Some((ty, span)) => (Some(ty), Some(span)),
                None => (None, None),
            }
        } else {
            (Some(VlType::Void), None)
        };
        self.expect(&TokenKind::LBrace, "`{` for the function body")?;
        let mut body = Vec::new();
        while !self.at_eof() && !matches!(self.peek().kind, TokenKind::RBrace) {
            match self.parse_stmt(&allowed) {
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
            type_params,
            params,
            ret,
            ret_span,
            body,
            span: Span::new(function_tok.span.start, close.span.end),
        })
    }

    fn parse_stmt(&mut self, allowed: &[String]) -> Option<Stmt> {
        if matches!(self.peek().kind, TokenKind::If) {
            return self.parse_if_stmt(allowed);
        }
        if matches!(self.peek().kind, TokenKind::While) {
            return self.parse_while_stmt(allowed);
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
            let (ty, ty_span) = self.parse_let_ann(allowed);
            self.expect(&TokenKind::Eq, "`=`")?;
            let value = self.parse_expr()?;
            let semi = self.expect(&TokenKind::Semi, "`;`")?;
            Some(Stmt::Let {
                name,
                name_span,
                ty,
                ty_span,
                value,
                span: Span::new(let_tok.span.start, semi.span.end),
            })
        } else if matches!(self.peek().kind, TokenKind::Ident(_)) {
            // Possible assignment to a variable, array element, or object
            // field. Parse only the postfix base; when no `=` follows, rewind
            // and use the normal expression-statement path.
            let save = self.pos;
            let diags_len = self.diags.len();
            let base = self.parse_postfix()?;
            if !matches!(self.peek().kind, TokenKind::Eq) {
                self.pos = save;
                self.diags.truncate(diags_len);
                let value = self.parse_expr()?;
                self.expect(&TokenKind::Semi, "`;`")?;
                Some(Stmt::Expr(value))
            } else {
                self.bump(); // `=`
                let value = self.parse_expr()?;
                let semi = self.expect(&TokenKind::Semi, "`;`")?;
                match base {
                    Expr::Var { path, span } if path.len() == 1 => Some(Stmt::Assign {
                        name: path[0].clone(),
                        name_span: span,
                        value,
                        span: Span::new(span.start, semi.span.end),
                    }),
                    Expr::Index { base, index, span } => Some(Stmt::IndexAssign {
                        array: base,
                        index,
                        value: Box::new(value),
                        span: Span::new(span.start, semi.span.end),
                    }),
                    Expr::Field { base, name, span } => Some(Stmt::FieldAssign {
                        base,
                        field: name,
                        field_span: span,
                        value: Box::new(value),
                        span: Span::new(span.start, semi.span.end),
                    }),
                    other => {
                        self.diags.push(
                            Diagnostic::error("cannot assign to this expression")
                                .with_label(
                                    other.span(),
                                    "only variables, array elements, and object fields are assignable",
                                )
                                .with_code("E103"),
                        );
                        None
                    }
                }
            }
        } else {
            let value = self.parse_expr()?;
            self.expect(&TokenKind::Semi, "`;`")?;
            Some(Stmt::Expr(value))
        }
    }

    fn parse_if_stmt(&mut self, allowed: &[String]) -> Option<Stmt> {
        let start = self.bump().span.start;
        self.expect(&TokenKind::LParen, "`(` after `if`")?;
        let condition = self.parse_expr()?;
        self.expect(&TokenKind::RParen, "`)` after condition")?;
        let then_body = self.parse_branch(allowed)?;
        let else_body = if matches!(self.peek().kind, TokenKind::Else) {
            self.bump();
            Some(self.parse_branch(allowed)?)
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

    fn parse_while_stmt(&mut self, allowed: &[String]) -> Option<Stmt> {
        let start = self.bump().span.start; // `while`
        self.expect(&TokenKind::LParen, "`(` after `while`")?;
        let condition = self.parse_expr()?;
        self.expect(&TokenKind::RParen, "`)` after condition")?;
        let body = self.parse_branch(allowed)?;
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

    fn parse_block(&mut self, allowed: &[String]) -> Option<Vec<Stmt>> {
        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut body = Vec::new();
        while !self.at_eof() && !matches!(self.peek().kind, TokenKind::RBrace) {
            match self.parse_stmt(allowed) {
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

    fn parse_branch(&mut self, allowed: &[String]) -> Option<Vec<Stmt>> {
        if matches!(self.peek().kind, TokenKind::LBrace) {
            self.parse_block(allowed)
        } else {
            Some(vec![self.parse_stmt(allowed)?])
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
                | TokenKind::Return
                | TokenKind::Type => return,
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
        let mut lhs = self.parse_cast()?;
        loop {
            let op = match &self.peek().kind {
                TokenKind::EqEq => BinOp::Eq,
                TokenKind::BangEq => BinOp::Ne,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_cast()?;
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

    /// Explicit conversion (`expr as u8`), tighter than `==` but looser than
    /// `+`: `a + b as u8` is `(a + b) as u8`, while `a == b as u8` is
    /// `a == (b as u8)`. Chains left-associatively (`x as u8 as u64`).
    /// Targets are concrete value types in v0 (`as T` with a type parameter
    /// is rejected: it parses with no scope, so `T` is an unknown type).
    fn parse_cast(&mut self) -> Option<Expr> {
        let mut base = self.parse_comparison()?;
        while matches!(self.peek().kind, TokenKind::As) {
            self.bump(); // `as`
            let Some((target, target_span)) = self.parse_type(&[], true) else {
                // `parse_type` already reported; abort this cast chain so one
                // bad target does not cascade.
                return None;
            };
            let span = Span::new(base.span().start, target_span.end);
            base = Expr::Cast {
                inner: Box::new(base),
                target,
                target_span,
                span,
            };
        }
        Some(base)
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
    ///
    /// `f[T](args)` without `::` is not a generic call: it parses here as
    /// indexing `f[T]` followed by a stray `(`, which gets one targeted
    /// error pointing at the turbofish (`f::[T](args)`). Index results are
    /// never callable, so no valid program is rejected by this rule.
    fn parse_postfix(&mut self) -> Option<Expr> {
        let mut base = self.parse_primary()?;
        let mut indexed = false;
        loop {
            if matches!(self.peek().kind, TokenKind::LBracket) {
                self.bump(); // `[`
                let index = self.parse_expr()?;
                let close = self.expect(&TokenKind::RBracket, "`]`")?;
                let span = Span::new(base.span().start, close.span.end);
                base = Expr::Index {
                    base: Box::new(base),
                    index: Box::new(index),
                    span,
                };
                indexed = true;
            } else if matches!(self.peek().kind, TokenKind::Dot) {
                self.bump();
                let (name, name_span) = self.parse_ident()?;
                let start = base.span().start;
                base = Expr::Field {
                    base: Box::new(base),
                    name,
                    span: Span::new(start, name_span.end),
                };
            } else {
                break;
            }
        }
        if indexed && matches!(self.peek().kind, TokenKind::LParen) {
            let paren = self.peek().clone();
            self.diags.push(
                Diagnostic::error("cannot call an index expression")
                    .with_label(base.span(), "this is `path[index]`, not a generic call")
                    .with_label(
                        paren.span,
                        "explicit type arguments use `::`: write `f::[T](...)`",
                    )
                    .with_code("E103"),
            );
            return None;
        }
        Some(base)
    }

    /// Parse an optional turbofish (`::[T, U]`) after a call path. Type
    /// arguments parse permissively (unknown names become type parameters);
    /// typechecking reports the ones nothing binds.
    fn parse_type_args(&mut self) -> Option<(Vec<VlType>, Option<Span>)> {
        if !matches!(self.peek().kind, TokenKind::ColonColon)
            || !matches!(
                self.toks.get(self.pos + 1).map(|t| &t.kind),
                Some(TokenKind::LBracket)
            )
        {
            return Some((Vec::new(), None));
        }
        self.bump(); // `::`
        let open = self.bump(); // `[`
        let mut args = Vec::new();
        loop {
            let (ty, _) = self.parse_type(&[], false)?;
            args.push(ty);
            match &self.peek().kind {
                TokenKind::Comma => {
                    self.bump();
                }
                _ => break,
            }
        }
        let close = self.expect(&TokenKind::RBracket, "`]` after type arguments")?;
        Some((args, Some(Span::new(open.span.start, close.span.end))))
    }

    fn parse_primary(&mut self) -> Option<Expr> {
        let t = self.peek().clone();
        match t.kind {
            TokenKind::Int(v) => {
                self.bump();
                Some(Expr::Literal(Scalar::Int(v), t.span))
            }
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
                if path.len() == 1 && matches!(self.peek().kind, TokenKind::LBrace) {
                    return self.parse_object_literal(path[0].clone(), t.span);
                }
                let (type_args, type_args_span) = self.parse_type_args()?;
                if !type_args.is_empty() && !matches!(self.peek().kind, TokenKind::LParen) {
                    let t = self.peek().clone();
                    self.diags.push(
                        Diagnostic::error("expected `(...)` after type arguments")
                            .with_label(t.span, "explicit type arguments only apply to calls")
                            .with_note("write `f::[T](args)`; `f::[T]` alone is not a value")
                            .with_code("E103"),
                    );
                    return None;
                }
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
                        type_args,
                        type_args_span,
                        args,
                        span,
                    })
                } else {
                    if path.len() == 1 {
                        Some(Expr::Var {
                            path,
                            span: Span::new(t.span.start, end),
                        })
                    } else {
                        let mut base = Expr::Var {
                            path: vec![path[0].clone()],
                            span: t.span,
                        };
                        for name in path.into_iter().skip(1) {
                            base = Expr::Field {
                                base: Box::new(base),
                                name,
                                span: Span::new(t.span.start, end),
                            };
                        }
                        Some(base)
                    }
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

    fn parse_object_literal(&mut self, name: String, name_span: Span) -> Option<Expr> {
        self.bump(); // `{`
        let mut fields = Vec::new();
        while !self.at_eof() && !matches!(self.peek().kind, TokenKind::RBrace) {
            let (field, field_span) = self.parse_ident()?;
            self.expect(&TokenKind::Colon, "`:` after object field")?;
            let value = self.parse_expr()?;
            fields.push((field, field_span, value));
            if matches!(self.peek().kind, TokenKind::Comma) {
                self.bump();
                continue;
            }
            if !matches!(self.peek().kind, TokenKind::RBrace) {
                let t = self.peek().clone();
                self.diags.push(
                    Diagnostic::error(format!("expected `,` or `}}`, found {}", describe(&t.kind)))
                        .with_label(t.span, "separate object fields with commas")
                        .with_code("E100"),
                );
                return None;
            }
        }
        let close = self.expect(&TokenKind::RBrace, "`}` after object literal")?;
        Some(Expr::ObjectLiteral {
            name,
            name_span,
            fields,
            span: Span::new(name_span.start, close.span.end),
        })
    }
}

fn discriminant(k: &TokenKind) -> std::mem::Discriminant<TokenKind> {
    std::mem::discriminant(k)
}

fn describe(k: &TokenKind) -> String {
    match k {
        TokenKind::Ident(n) => format!("identifier `{n}`"),
        TokenKind::Int(v) => format!("integer `{v}`"),
        TokenKind::I64(v) => format!("i64 literal `{v}`"),
        TokenKind::U64(v) => format!("u64 literal `{v}`"),
        TokenKind::F64(v) => format!("f64 literal `{}`", f64::from_bits(*v)),
        TokenKind::U8(v) => format!("u8 literal `{v}`"),
        TokenKind::Bool(v) => format!("boolean `{v}`"),
        TokenKind::String(_) => "string literal".into(),
        TokenKind::Let => "`let`".into(),
        TokenKind::Function => "`function`".into(),
        TokenKind::Type => "`type`".into(),
        TokenKind::Object => "`object`".into(),
        TokenKind::If => "`if`".into(),
        TokenKind::Else => "`else`".into(),
        TokenKind::While => "`while`".into(),
        TokenKind::Break => "`break`".into(),
        TokenKind::Continue => "`continue`".into(),
        TokenKind::Return => "`return`".into(),
        TokenKind::As => "`as`".into(),
        TokenKind::Extends => "`extends`".into(),
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
        TokenKind::ColonColon => "`::`".into(),
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

    /// Search message, labels, and note for a substring.
    fn mentions(diags: &[Diagnostic], s: &str) -> bool {
        diags.iter().any(|d| {
            d.message.contains(s)
                || d.labels
                    .iter()
                    .any(|l| l.message.as_deref().is_some_and(|m| m.contains(s)))
                || d.note.as_deref().is_some_and(|n| n.contains(s))
        })
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
    fn parses_object_declaration_literal_and_field_assign() {
        let (prog, diags) = parse_src(
            "type Counter = object { value: u64, label: String, }; function main() { let c = Counter { label: \"x\", value: 1 }; c.value = 2; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(matches!(prog.items[0], Item::Object { ref fields, .. } if fields.len() == 2));
        match &prog.items[1] {
            Item::Function { body, .. } => {
                assert!(matches!(
                    &body[0],
                    Stmt::Let {
                        value: Expr::ObjectLiteral { name, fields, .. },
                        ..
                    } if name == "Counter" && fields.len() == 2
                ));
                assert!(matches!(&body[1], Stmt::FieldAssign { field, .. } if field == "value"));
            }
            other => panic!("expected function, got {other:?}"),
        }
    }

    #[test]
    fn generic_type_parameter_shadows_object_name() {
        let (prog, diags) =
            parse_src("type T = object { value: u64, }; function id[T](x: T): T { return x; }");
        assert!(diags.is_empty(), "{diags:?}");
        match &prog.items[1] {
            Item::Function { params, ret, .. } => {
                assert_eq!(params[0].ty, Some(VlType::Param("T".into())));
                assert_eq!(*ret, Some(VlType::Param("T".into())));
            }
            other => panic!("expected function, got {other:?}"),
        }
    }

    #[test]
    fn rejects_object_names_reserved_for_builtins() {
        let (_prog, diags) = parse_src("type String = object { value: u64, };");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("reserved"));
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
            "function get(a: Array[u64]): u64 { a[0u64] = 1u64; return a[0u64]; } function main() { let b = [1u64, 2u64,]; let e = []; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        match &prog.items[0] {
            Item::Function { params, body, .. } => {
                assert_eq!(params[0].ty, Some(VlType::Array(Box::new(VlType::U64))));
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
    fn parses_nested_array_types() {
        let (prog, diags) =
            parse_src("function f(a: Array[Array[u64]]): Array[String] { return [\"s\"]; }");
        assert!(diags.is_empty(), "{diags:?}");
        match &prog.items[0] {
            Item::Function { params, ret, .. } => {
                assert_eq!(
                    params[0].ty,
                    Some(VlType::Array(Box::new(VlType::Array(Box::new(
                        VlType::U64
                    )))))
                );
                assert_eq!(*ret, Some(VlType::Array(Box::new(VlType::String))));
            }
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn bare_array_without_element_is_an_error() {
        let (_prog, diags) = parse_src("function f(a: Array): void { return; }");
        assert!(diags.iter().any(|d| d.code.as_deref() == Some("E104")));
    }

    #[test]
    fn u64array_is_removed_with_a_hint() {
        let (_prog, diags) = parse_src("function f(a: U64Array): void { return; }");
        assert!(mentions(&diags, "Array[u64]"), "{diags:?}");
    }

    #[test]
    fn parses_generic_function_and_turbofish_call() {
        let (prog, diags) = parse_src(
            "function first[T](a: Array[T]): T { return a[0u64]; } function main() { first([1u64]); first::[u64]([1u64]); }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        match &prog.items[0] {
            Item::Function {
                type_params,
                params,
                ret,
                ..
            } => {
                assert_eq!(type_params.len(), 1);
                assert_eq!(type_params[0].name, "T");
                assert_eq!(
                    params[0].ty,
                    Some(VlType::Array(Box::new(VlType::Param("T".into()))))
                );
                assert_eq!(*ret, Some(VlType::Param("T".into())));
            }
            other => panic!("expected fn, got {other:?}"),
        }
        match &prog.items[1] {
            Item::Function { body, .. } => {
                match &body[0] {
                    Stmt::Expr(Expr::Call {
                        type_args, args, ..
                    }) => {
                        assert!(type_args.is_empty());
                        assert_eq!(args.len(), 1);
                    }
                    other => panic!("expected inferred call, got {other:?}"),
                }
                match &body[1] {
                    Stmt::Expr(Expr::Call {
                        type_args, args, ..
                    }) => {
                        assert_eq!(*type_args, vec![VlType::U64]);
                        assert_eq!(args.len(), 1);
                    }
                    other => panic!("expected turbofish call, got {other:?}"),
                }
            }
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_type_params_are_an_error() {
        let (_prog, diags) = parse_src("function f[T, T](x: T): T { return x; }");
        assert!(diags.iter().any(|d| d.code.as_deref() == Some("E200")));
    }

    #[test]
    fn type_param_shadowing_primitive_is_an_error() {
        let (_prog, diags) = parse_src("function f[u64](x: u64): u64 { return x; }");
        assert!(diags
            .iter()
            .any(|d| d.message.contains("shadows a primitive")));
    }

    #[test]
    fn unknown_type_param_in_signature_is_an_error() {
        let (_prog, diags) = parse_src("function f[T](x: U): U { return x; }");
        assert!(diags.iter().any(|d| d.code.as_deref() == Some("E105")));
    }

    #[test]
    fn bracket_call_without_turbofish_is_an_index_error() {
        let (_prog, diags) = parse_src("function main() { f[T](1u64); }");
        assert!(mentions(&diags, "f::[T]"), "{diags:?}");
    }

    #[test]
    fn turbofish_without_call_is_an_error() {
        let (_prog, diags) = parse_src("function main() { f::[u64]; }");
        assert!(diags.iter().any(|d| d.code.as_deref() == Some("E103")));
    }

    #[test]
    fn annotated_lets_parse() {
        let (prog, diags) = parse_src(
            "let scores: Array[u64] = Array.new::[u64](3); function main() { let n: u64 = 1; n; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        match &prog.items[0] {
            Item::Let { ty, ty_span, .. } => {
                assert_eq!(*ty, Some(VlType::Array(Box::new(VlType::U64))));
                assert!(ty_span.is_some());
            }
            other => panic!("expected let, got {other:?}"),
        }
        match &prog.items[1] {
            Item::Function { body, .. } => match &body[0] {
                Stmt::Let { ty, .. } => assert_eq!(*ty, Some(VlType::U64)),
                other => panic!("expected let, got {other:?}"),
            },
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn unannotated_lets_stay_untyped() {
        let (prog, diags) = parse_src("let x = 1; function main() { let y = 2; y; }");
        assert!(diags.is_empty(), "{diags:?}");
        match &prog.items[0] {
            Item::Let { ty, ty_span, .. } => {
                assert_eq!(*ty, None);
                assert_eq!(*ty_span, None);
            }
            other => panic!("expected let, got {other:?}"),
        }
    }

    #[test]
    fn void_let_is_an_error() {
        let (_prog, diags) = parse_src("let x: void = 1;");
        assert!(diags.iter().any(|d| d.code.as_deref() == Some("E104")));
        let (_prog, diags) = parse_src("function main() { let x: void = 1; }");
        assert!(diags.iter().any(|d| d.code.as_deref() == Some("E104")));
    }

    #[test]
    fn unknown_let_type_is_an_error() {
        let (_prog, diags) = parse_src("let x: Bogus = 1;");
        assert!(diags.iter().any(|d| d.code.as_deref() == Some("E105")));
    }

    #[test]
    fn let_annotation_sees_type_params() {
        let (prog, diags) =
            parse_src("function f[T](x: T): T { let y: T = x; let z: Array[T] = [x]; return y; }");
        assert!(diags.is_empty(), "{diags:?}");
        match &prog.items[0] {
            Item::Function { body, .. } => {
                match &body[0] {
                    Stmt::Let { ty, .. } => assert_eq!(*ty, Some(VlType::Param("T".into()))),
                    other => panic!("expected let, got {other:?}"),
                }
                match &body[1] {
                    Stmt::Let { ty, .. } => assert_eq!(
                        *ty,
                        Some(VlType::Array(Box::new(VlType::Param("T".into()))))
                    ),
                    other => panic!("expected let, got {other:?}"),
                }
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

    #[test]
    fn parses_as_cast_with_precedence() {
        // `a + b as u8` is `(a + b) as u8`; `a == b as u8` is `a == (b as u8)`.
        let (prog, diags) = parse_src("function main() { let x = 1u64 + 2u64 as u8; x; }");
        assert!(diags.is_empty(), "{diags:?}");
        match &prog.items[0] {
            Item::Function { body, .. } => match &body[0] {
                Stmt::Let { value, .. } => assert!(matches!(value, Expr::Cast { .. })),
                other => panic!("expected cast let, got {other:?}"),
            },
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn parses_bounded_type_params() {
        let (prog, diags) =
            parse_src("function add[T extends Numeric](a: T, b: T): T { return a + b; }");
        assert!(diags.is_empty(), "{diags:?}");
        match &prog.items[0] {
            Item::Function { type_params, .. } => {
                assert_eq!(type_params.len(), 1);
                assert_eq!(type_params[0].name, "T");
                assert_eq!(type_params[0].bound, Some(GenericBound::Numeric));
            }
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn unknown_bound_is_an_error() {
        let (_prog, diags) = parse_src("function f[T extends Bogus](x: T): T { return x; }");
        assert!(diags.iter().any(|d| d.code.as_deref() == Some("E105")));
    }
}
