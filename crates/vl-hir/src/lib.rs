//! vl-hir: high-level IR. Desugared, explicitly-scoped form of the AST.
//!
//! Differences from the AST: unary negation is lowered to `0 - x`,
//! every node carries a [`HirId`], and variable uses carry the [`DefId`]
//! resolved by `vl-semantic` (or `None` when resolution failed, so later
//! stages can skip rather than cascade errors). Returns are explicit:
//! only `return expr;` / `return;` yields a value; trailing expression
//! statements are discarded values, never implicit returns.

use vl_common::{GenericBound, Scalar, Span, VlType};

pub use vl_semantic::DefId;
use vl_syntax::{
    BinOp as AstBinOp, Expr as AstExpr, Item as AstItem, Program as AstProgram, Stmt as AstStmt,
    UnOp as AstUnOp,
};

/// One generic type parameter with its optional bound
/// (`T` vs `T extends Numeric`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirTypeParam {
    pub name: String,
    pub bound: Option<GenericBound>,
}

/// Unique node id within one lowering run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HirId(pub u32);

#[derive(Debug, Clone)]
pub struct HirProgram {
    pub module: String,
    pub items: Vec<HirItem>,
}

#[derive(Debug, Clone)]
pub enum HirItem {
    Object {
        name: String,
        fields: Vec<(String, Option<VlType>, Span)>,
        span: Span,
    },
    Let {
        id: HirId,
        def: Option<DefId>,
        /// Optional annotation (`None` = infer; a failed annotation parses
        /// as `None` with `ty_span` set and poisons quietly downstream).
        ty: Option<VlType>,
        ty_span: Option<Span>,
        value: HirExpr,
        span: Span,
    },
    Fn {
        id: HirId,
        def: Option<DefId>,
        name: String,
        /// Declared type parameters (`[]` when monomorphic).
        type_params: Vec<HirTypeParam>,
        /// `(name, def, type, span)`. `ty` is `None` when the annotation was
        /// missing/unknown (already reported; typecheck poisons quietly).
        params: Vec<(String, Option<DefId>, Option<VlType>, Span)>,
        /// Declared return type. `None` means missing (already reported).
        ret: Option<VlType>,
        ret_span: Option<Span>,
        body: Vec<HirStmt>,
        span: Span,
    },
}

#[derive(Debug, Clone)]
pub enum HirStmt {
    Let {
        id: HirId,
        def: Option<DefId>,
        /// Optional annotation, same encoding as [`HirItem::Let`].
        ty: Option<VlType>,
        ty_span: Option<Span>,
        value: HirExpr,
        span: Span,
    },
    Assign {
        id: HirId,
        def: Option<DefId>,
        value: HirExpr,
        span: Span,
    },
    /// Element write: `array[index] = value;`.
    IndexAssign {
        id: HirId,
        array: Box<HirExpr>,
        index: Box<HirExpr>,
        value: Box<HirExpr>,
        span: Span,
    },
    FieldAssign {
        id: HirId,
        base: Box<HirExpr>,
        field: String,
        value: Box<HirExpr>,
        span: Span,
    },
    If {
        condition: HirExpr,
        then_body: Vec<HirStmt>,
        else_body: Option<Vec<HirStmt>>,
        span: Span,
    },
    While {
        condition: HirExpr,
        body: Vec<HirStmt>,
        span: Span,
    },
    Break {
        span: Span,
    },
    Continue {
        span: Span,
    },
    Return {
        value: Option<HirExpr>,
        span: Span,
    },
    Expr(HirExpr),
}

#[derive(Debug, Clone)]
pub enum HirExpr {
    Literal {
        id: HirId,
        value: Scalar,
        span: Span,
    },
    String {
        id: HirId,
        value: Vec<u8>,
        span: Span,
    },
    /// Array literal: `[1, 2]`; integer element types are contextual.
    ArrayLiteral {
        id: HirId,
        elems: Vec<HirExpr>,
        span: Span,
    },
    ObjectLiteral {
        id: HirId,
        name: String,
        fields: Vec<(String, HirExpr)>,
        span: Span,
    },
    /// Element read: `array[index]`.
    Index {
        id: HirId,
        base: Box<HirExpr>,
        index: Box<HirExpr>,
        span: Span,
    },
    Field {
        id: HirId,
        base: Box<HirExpr>,
        name: String,
        span: Span,
    },
    Var {
        id: HirId,
        def: Option<DefId>,
        name: String,
        span: Span,
    },
    Call {
        id: HirId,
        /// Resolved callee (`None` when resolution failed; quiet downstream).
        def: Option<DefId>,
        external: bool,
        /// Compiler-owned extern signature copied from the resolved `Def`.
        /// `None` for locals, poisoned imports, or unresolved callees.
        extern_sig: Option<vl_common::FuncSig>,
        name: String,
        /// Explicit type arguments (`f::[u64]`); empty means infer.
        type_args: Vec<VlType>,
        args: Vec<HirExpr>,
        span: Span,
    },
    Binary {
        id: HirId,
        op: HirBinOp,
        lhs: Box<HirExpr>,
        rhs: Box<HirExpr>,
        span: Span,
    },
    Unary {
        id: HirId,
        op: HirUnOp,
        inner: Box<HirExpr>,
        span: Span,
    },
    /// Explicit conversion (`value as u8`).
    Cast {
        id: HirId,
        inner: Box<HirExpr>,
        target: VlType,
        target_span: Span,
        span: Span,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HirUnOp {
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HirBinOp {
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

impl HirExpr {
    pub fn id(&self) -> HirId {
        match self {
            HirExpr::Literal { id, .. }
            | HirExpr::String { id, .. }
            | HirExpr::ArrayLiteral { id, .. }
            | HirExpr::ObjectLiteral { id, .. }
            | HirExpr::Index { id, .. }
            | HirExpr::Field { id, .. }
            | HirExpr::Var { id, .. }
            | HirExpr::Call { id, .. }
            | HirExpr::Binary { id, .. }
            | HirExpr::Unary { id, .. }
            | HirExpr::Cast { id, .. } => *id,
        }
    }

    pub fn span(&self) -> Span {
        match self {
            HirExpr::Literal { span, .. }
            | HirExpr::String { span, .. }
            | HirExpr::ArrayLiteral { span, .. }
            | HirExpr::ObjectLiteral { span, .. }
            | HirExpr::Index { span, .. }
            | HirExpr::Field { span, .. }
            | HirExpr::Var { span, .. }
            | HirExpr::Call { span, .. }
            | HirExpr::Binary { span, .. }
            | HirExpr::Unary { span, .. }
            | HirExpr::Cast { span, .. } => *span,
        }
    }
}

struct Lowerer<'a> {
    next: u32,
    res: &'a vl_semantic::Resolution,
}

impl<'a> Lowerer<'a> {
    fn id(&mut self) -> HirId {
        let id = HirId(self.next);
        self.next += 1;
        id
    }

    /// Use-site lookup (variable reads, callees).
    fn def_at(&self, span: Span) -> Option<DefId> {
        self.res.def_of(span).map(|d| d.id.clone())
    }

    /// Definition-site lookup (`let` names, `function` names, params).
    fn def_at_site(&self, span: Span) -> Option<DefId> {
        self.res.def_at(span).map(|d| d.id.clone())
    }
}

/// Lower a parsed program with its resolution map. Infallible by design:
/// unresolved names become `def: None` and are reported by earlier stages.
pub fn lower(prog: &AstProgram, res: &vl_semantic::Resolution) -> HirProgram {
    let mut l = Lowerer { next: 0, res };
    let items = prog
        .items
        .iter()
        .filter_map(|i| match i {
            AstItem::Use { .. } => None,
            _ => Some(l.lower_item(i)),
        })
        .collect();
    HirProgram {
        module: prog.module.clone(),
        items,
    }
}

impl<'a> Lowerer<'a> {
    fn lower_item(&mut self, item: &AstItem) -> HirItem {
        match item {
            AstItem::Use { .. } => unreachable!("use items are filtered before lowering"),
            AstItem::Object {
                name, fields, span, ..
            } => HirItem::Object {
                name: name.clone(),
                fields: fields
                    .iter()
                    .map(|f| (f.name.clone(), f.ty.clone(), f.name_span))
                    .collect(),
                span: *span,
            },
            AstItem::Let {
                value,
                span,
                name_span,
                ty,
                ty_span,
                ..
            } => {
                let def = self.def_at_site(*name_span);
                HirItem::Let {
                    id: self.id(),
                    def,
                    ty: ty.clone(),
                    ty_span: *ty_span,
                    value: self.lower_expr(value),
                    span: *span,
                }
            }
            AstItem::Function {
                name,
                name_span,
                type_params,
                params,
                ret,
                ret_span,
                body,
                span,
                ..
            } => HirItem::Fn {
                id: self.id(),
                def: self.def_at_site(*name_span),
                name: name.clone(),
                type_params: type_params
                    .iter()
                    .map(|p| HirTypeParam {
                        name: p.name.clone(),
                        bound: p.bound,
                    })
                    .collect(),
                params: params
                    .iter()
                    .map(|p| {
                        (
                            p.name.clone(),
                            self.def_at_site(p.name_span),
                            p.ty.clone(),
                            p.name_span,
                        )
                    })
                    .collect(),
                ret: ret.clone(),
                ret_span: *ret_span,
                body: body.iter().map(|s| self.lower_stmt(s)).collect(),
                span: *span,
            },
        }
    }

    fn lower_stmt(&mut self, stmt: &AstStmt) -> HirStmt {
        match stmt {
            AstStmt::Let {
                value,
                span,
                name_span,
                ty,
                ty_span,
                ..
            } => {
                let def = self.def_at_site(*name_span);
                HirStmt::Let {
                    id: self.id(),
                    def,
                    ty: ty.clone(),
                    ty_span: *ty_span,
                    value: self.lower_expr(value),
                    span: *span,
                }
            }
            AstStmt::Assign {
                name_span,
                value,
                span,
                ..
            } => {
                // Assignment writes through the resolved binding; the LHS is
                // a use-site (recorded by the resolver), not a new def-site.
                let def = self.def_at(*name_span);
                HirStmt::Assign {
                    id: self.id(),
                    def,
                    value: self.lower_expr(value),
                    span: *span,
                }
            }
            AstStmt::IndexAssign {
                array,
                index,
                value,
                span,
            } => HirStmt::IndexAssign {
                id: self.id(),
                array: Box::new(self.lower_expr(array)),
                index: Box::new(self.lower_expr(index)),
                value: Box::new(self.lower_expr(value)),
                span: *span,
            },
            AstStmt::FieldAssign {
                base,
                field,
                value,
                span,
                ..
            } => HirStmt::FieldAssign {
                id: self.id(),
                base: Box::new(self.lower_expr(base)),
                field: field.clone(),
                value: Box::new(self.lower_expr(value)),
                span: *span,
            },
            AstStmt::Expr(e) => HirStmt::Expr(self.lower_expr(e)),
            AstStmt::Return { value, span } => HirStmt::Return {
                value: value.as_ref().map(|e| self.lower_expr(e)),
                span: *span,
            },
            AstStmt::Break { span } => HirStmt::Break { span: *span },
            AstStmt::Continue { span } => HirStmt::Continue { span: *span },
            AstStmt::While {
                condition,
                body,
                span,
            } => HirStmt::While {
                condition: self.lower_expr(condition),
                body: body.iter().map(|s| self.lower_stmt(s)).collect(),
                span: *span,
            },
            AstStmt::If {
                condition,
                then_body,
                else_body,
                span,
            } => HirStmt::If {
                condition: self.lower_expr(condition),
                then_body: then_body.iter().map(|s| self.lower_stmt(s)).collect(),
                else_body: else_body
                    .as_ref()
                    .map(|body| body.iter().map(|s| self.lower_stmt(s)).collect()),
                span: *span,
            },
        }
    }

    fn lower_expr(&mut self, expr: &AstExpr) -> HirExpr {
        match expr {
            AstExpr::Literal(value, s) => HirExpr::Literal {
                id: self.id(),
                value: *value,
                span: *s,
            },
            AstExpr::String(value, s) => HirExpr::String {
                id: self.id(),
                value: value.clone(),
                span: *s,
            },
            AstExpr::ArrayLiteral { elems, span } => HirExpr::ArrayLiteral {
                id: self.id(),
                elems: elems.iter().map(|e| self.lower_expr(e)).collect(),
                span: *span,
            },
            AstExpr::ObjectLiteral {
                name, fields, span, ..
            } => HirExpr::ObjectLiteral {
                id: self.id(),
                name: name.clone(),
                fields: fields
                    .iter()
                    .map(|(name, _, value)| (name.clone(), self.lower_expr(value)))
                    .collect(),
                span: *span,
            },
            AstExpr::Index { base, index, span } => HirExpr::Index {
                id: self.id(),
                base: Box::new(self.lower_expr(base)),
                index: Box::new(self.lower_expr(index)),
                span: *span,
            },
            AstExpr::Field { base, name, span } => HirExpr::Field {
                id: self.id(),
                base: Box::new(self.lower_expr(base)),
                name: name.clone(),
                span: *span,
            },
            AstExpr::Var { path, span: s } => HirExpr::Var {
                id: self.id(),
                def: self.def_at(*s),
                name: path.join("."),
                span: *s,
            },
            AstExpr::Call {
                callee,
                callee_span,
                type_args,
                args,
                span,
                ..
            } => {
                let def = self.def_at(*callee_span);
                let resolved = def
                    .as_ref()
                    .and_then(|d| self.res.defs.iter().find(|r| r.id == *d));
                let external =
                    resolved.is_some_and(|r| matches!(r.kind, vl_semantic::DefKind::External));
                let extern_sig = if external {
                    resolved.and_then(|r| r.sig.clone())
                } else {
                    None
                };
                HirExpr::Call {
                    id: self.id(),
                    def,
                    external,
                    extern_sig,
                    name: callee.join("."),
                    type_args: type_args.clone(),
                    args: args.iter().map(|a| self.lower_expr(a)).collect(),
                    span: *span,
                }
            }
            AstExpr::Unary { op, rhs, span } => match op {
                // Desugar `-x` into `0 - x`.
                AstUnOp::Neg => {
                    let rhs = self.lower_expr(rhs);
                    let zero = match &rhs {
                        HirExpr::Literal { value, .. } => scalar_zero(*value),
                        _ => Scalar::Int(0),
                    };
                    let zero_span = Span::empty(span.start);
                    HirExpr::Binary {
                        id: self.id(),
                        op: HirBinOp::Sub,
                        lhs: Box::new(HirExpr::Literal {
                            id: self.id(),
                            value: zero,
                            span: zero_span,
                        }),
                        rhs: Box::new(rhs),
                        span: *span,
                    }
                }
                AstUnOp::Not => HirExpr::Unary {
                    id: self.id(),
                    op: HirUnOp::Not,
                    inner: Box::new(self.lower_expr(rhs)),
                    span: *span,
                },
            },
            AstExpr::Binary { op, lhs, rhs, span } => {
                let op = match op {
                    AstBinOp::Add => HirBinOp::Add,
                    AstBinOp::Sub => HirBinOp::Sub,
                    AstBinOp::Mul => HirBinOp::Mul,
                    AstBinOp::Div => HirBinOp::Div,
                    AstBinOp::Eq => HirBinOp::Eq,
                    AstBinOp::Ne => HirBinOp::Ne,
                    AstBinOp::Lt => HirBinOp::Lt,
                    AstBinOp::Le => HirBinOp::Le,
                    AstBinOp::Gt => HirBinOp::Gt,
                    AstBinOp::Ge => HirBinOp::Ge,
                    AstBinOp::And => HirBinOp::And,
                    AstBinOp::Or => HirBinOp::Or,
                };
                HirExpr::Binary {
                    id: self.id(),
                    op,
                    lhs: Box::new(self.lower_expr(lhs)),
                    rhs: Box::new(self.lower_expr(rhs)),
                    span: *span,
                }
            }
            AstExpr::Cast {
                inner,
                target,
                target_span,
                span,
            } => HirExpr::Cast {
                id: self.id(),
                inner: Box::new(self.lower_expr(inner)),
                target: target.clone(),
                target_span: *target_span,
                span: *span,
            },
        }
    }
}

fn scalar_zero(value: Scalar) -> Scalar {
    match value {
        Scalar::Int(_) => Scalar::Int(0),
        Scalar::U64(_) => Scalar::U64(0),
        Scalar::I64(_) => Scalar::I64(0),
        Scalar::F64(_) => Scalar::F64(0.0f64.to_bits()),
        Scalar::Bool(_) => Scalar::Bool(false),
        Scalar::U8(_) => Scalar::U8(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_lowers_to_explicit_unary() {
        let (toks, _) = vl_lex::lex("function main() { !true; }");
        let (prog, _) = vl_syntax::parse(&toks, "");
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = lower(&prog, &res);
        match &hir.items[0] {
            HirItem::Fn { body, .. } => {
                assert!(matches!(&body[0], HirStmt::Expr(HirExpr::Unary { .. })))
            }
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn arrays_lower() {
        let src = "function main() { let a = [1u64, 2u64]; a[0u64] = 3u64; let x = a[1u64]; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, rdiags) = vl_semantic::resolve(&prog);
        assert!(rdiags.iter().all(|d| !d.is_error()), "{rdiags:?}");
        let hir = lower(&prog, &res);
        match &hir.items[0] {
            HirItem::Fn { body, .. } => {
                assert!(matches!(&body[0], HirStmt::Let { .. }));
                assert!(matches!(&body[1], HirStmt::IndexAssign { .. }));
                assert!(matches!(&body[2], HirStmt::Let { .. }));
            }
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn generics_plumb_type_params_and_args() {
        let src = "function first[T](a: Array[T]): T { return a[0u64]; } function main() { first([1u64]); first::[u64]([2u64]); }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, rdiags) = vl_semantic::resolve(&prog);
        assert!(rdiags.iter().all(|d| !d.is_error()), "{rdiags:?}");
        let hir = lower(&prog, &res);
        match &hir.items[0] {
            HirItem::Fn {
                type_params,
                params,
                ret,
                ..
            } => {
                assert_eq!(
                    type_params
                        .iter()
                        .map(|p| p.name.clone())
                        .collect::<Vec<_>>(),
                    vec!["T".to_string()]
                );
                assert!(type_params[0].bound.is_none());
                assert!(matches!(params[0].2, Some(VlType::Array(_))));
                assert!(matches!(ret, Some(VlType::Param(_))));
            }
            other => panic!("expected fn, got {other:?}"),
        }
        match &hir.items[1] {
            HirItem::Fn { body, .. } => {
                match &body[0] {
                    HirStmt::Expr(HirExpr::Call { type_args, .. }) => {
                        assert!(type_args.is_empty());
                    }
                    other => panic!("expected inferred call, got {other:?}"),
                }
                match &body[1] {
                    HirStmt::Expr(HirExpr::Call { type_args, .. }) => {
                        assert_eq!(type_args.len(), 1);
                    }
                    other => panic!("expected turbofish call, got {other:?}"),
                }
            }
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn annotated_lets_carry_their_types() {
        let src =
            "let scores: Array[u64] = Array.new::[u64](3); function main() { let n: u64 = 1; n; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, rdiags) = vl_semantic::resolve(&prog);
        assert!(rdiags.iter().all(|d| !d.is_error()), "{rdiags:?}");
        let hir = lower(&prog, &res);
        match &hir.items[0] {
            HirItem::Let { ty, ty_span, .. } => {
                assert!(matches!(ty, Some(VlType::Array(_))));
                assert!(ty_span.is_some());
            }
            other => panic!("expected let, got {other:?}"),
        }
        match &hir.items[1] {
            HirItem::Fn { body, .. } => match &body[0] {
                HirStmt::Let { ty, .. } => assert_eq!(*ty, Some(VlType::U64)),
                other => panic!("expected let, got {other:?}"),
            },
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn assign_links_the_resolved_binding() {
        let src = "function main() { let x = 1; x = 2; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        let (res, rdiags) = vl_semantic::resolve(&prog);
        assert!(rdiags.iter().all(|d| !d.is_error()), "{rdiags:?}");
        let hir = lower(&prog, &res);
        match &hir.items[0] {
            HirItem::Fn { body, .. } => {
                assert!(matches!(&body[1], HirStmt::Assign { def: Some(_), .. }))
            }
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn while_and_break_lower() {
        let src = "function main() { while (true) { break; } }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        let (res, rdiags) = vl_semantic::resolve(&prog);
        assert!(rdiags.iter().all(|d| !d.is_error()), "{rdiags:?}");
        let hir = lower(&prog, &res);
        match &hir.items[0] {
            HirItem::Fn { body, .. } => match &body[0] {
                HirStmt::While { body, .. } => {
                    assert!(matches!(body[0], HirStmt::Break { .. }))
                }
                other => panic!("expected while, got {other:?}"),
            },
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn neg_desugars_to_sub() {
        let (toks, _) = vl_lex::lex("let x = -1;");
        let (prog, _) = vl_syntax::parse(&toks, "let x = -1;");
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = lower(&prog, &res);
        assert_eq!(hir.items.len(), 1);
        assert!(matches!(hir.items[0], HirItem::Let { .. }));
    }

    #[test]
    fn call_links_callee_def() {
        let src =
            "function add(a: i64, b: i64): i64 { return a + b; } function main() { add(1, 2); }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = lower(&prog, &res);
        assert_eq!(hir.items.len(), 2);
        match &hir.items[1] {
            HirItem::Fn { body, .. } => match &body[0] {
                HirStmt::Expr(HirExpr::Call {
                    name, args, def, ..
                }) => {
                    assert_eq!(name, "add");
                    assert_eq!(args.len(), 2);
                    assert!(def.is_some());
                }
                other => panic!("expected call, got {other:?}"),
            },
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn objects_lower_with_field_reads_and_writes() {
        let src = "type Counter = object { value: u64, }; function main() { let c = Counter { value = 1 }; c.value = c.value + 1; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, rdiags) = vl_semantic::resolve(&prog);
        assert!(rdiags.is_empty(), "{rdiags:?}");
        let hir = lower(&prog, &res);
        assert!(matches!(hir.items[0], HirItem::Object { .. }));
        assert!(matches!(
            hir.items[1],
            HirItem::Fn { ref body, .. }
                if body.iter().any(|stmt| matches!(stmt, HirStmt::FieldAssign { .. }))
        ));
    }

    #[test]
    fn return_lowers_with_value() {
        let src = "function f(): i64 { return 1; } function m() { return; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, rdiags) = vl_semantic::resolve(&prog);
        assert!(rdiags.iter().all(|d| !d.is_error()), "{rdiags:?}");
        let hir = lower(&prog, &res);
        match &hir.items[0] {
            HirItem::Fn { body, .. } => {
                assert!(matches!(&body[0], HirStmt::Return { value: Some(_), .. }))
            }
            other => panic!("expected fn, got {other:?}"),
        }
        match &hir.items[1] {
            HirItem::Fn { body, .. } => {
                assert!(matches!(&body[0], HirStmt::Return { value: None, .. }))
            }
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn unresolved_call_poisoned_not_panic() {
        let src = "function main() { nope(1); }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = lower(&prog, &res);
        match &hir.items[0] {
            HirItem::Fn { body, .. } => assert!(matches!(
                &body[0],
                HirStmt::Expr(HirExpr::Call { def: None, .. })
            )),
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn casts_and_bounds_lower() {
        let src = "function add[T extends Numeric](a: T, b: T): T { let c = a as u64; return a + b; } function main() { add(1u64, 2u64); }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, rdiags) = vl_semantic::resolve(&prog);
        assert!(rdiags.iter().all(|d| !d.is_error()), "{rdiags:?}");
        let hir = lower(&prog, &res);
        match &hir.items[0] {
            HirItem::Fn {
                type_params, body, ..
            } => {
                assert_eq!(type_params.len(), 1);
                assert_eq!(type_params[0].bound, Some(vl_common::GenericBound::Numeric));
                assert!(matches!(
                    &body[0],
                    HirStmt::Let {
                        value: HirExpr::Cast { .. },
                        ..
                    }
                ));
            }
            other => panic!("expected fn, got {other:?}"),
        }
    }
}
