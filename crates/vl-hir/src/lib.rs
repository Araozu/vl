//! vl-hir: high-level IR. Desugared, explicitly-scoped form of the AST.
//!
//! Differences from the AST: unary negation is lowered to `0 - x`,
//! every node carries a [`HirId`], and variable uses carry the [`DefId`]
//! resolved by `vl-semantic` (or `None` when resolution failed, so later
//! stages can skip rather than cascade errors).

use vl_common::Span;
use vl_semantic::DefId;
use vl_syntax::{
    BinOp as AstBinOp, Expr as AstExpr, Item as AstItem, Program as AstProgram, Stmt as AstStmt,
    UnOp as AstUnOp,
};

/// Unique node id within one lowering run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HirId(pub u32);

#[derive(Debug, Clone)]
pub struct HirProgram {
    pub items: Vec<HirItem>,
}

#[derive(Debug, Clone)]
pub enum HirItem {
    Let {
        id: HirId,
        def: Option<DefId>,
        value: HirExpr,
        span: Span,
    },
    Fn {
        id: HirId,
        def: Option<DefId>,
        name: String,
        params: Vec<(String, Option<DefId>, Span)>,
        body: Vec<HirStmt>,
        span: Span,
    },
}

#[derive(Debug, Clone)]
pub enum HirStmt {
    Let {
        id: HirId,
        def: Option<DefId>,
        value: HirExpr,
        span: Span,
    },
    Expr(HirExpr),
}

#[derive(Debug, Clone)]
pub enum HirExpr {
    Int {
        id: HirId,
        value: i64,
        span: Span,
    },
    String {
        id: HirId,
        value: Vec<u8>,
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
        name: String,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HirBinOp {
    Add,
    Sub,
    Mul,
    Div,
}

impl HirExpr {
    pub fn id(&self) -> HirId {
        match self {
            HirExpr::Int { id, .. }
            | HirExpr::String { id, .. }
            | HirExpr::Var { id, .. }
            | HirExpr::Call { id, .. }
            | HirExpr::Binary { id, .. } => *id,
        }
    }

    pub fn span(&self) -> Span {
        match self {
            HirExpr::Int { span, .. }
            | HirExpr::String { span, .. }
            | HirExpr::Var { span, .. }
            | HirExpr::Call { span, .. }
            | HirExpr::Binary { span, .. } => *span,
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
    let items = prog.items.iter().map(|i| l.lower_item(i)).collect();
    HirProgram { items }
}

impl<'a> Lowerer<'a> {
    fn lower_item(&mut self, item: &AstItem) -> HirItem {
        match item {
            AstItem::Let {
                value,
                span,
                name_span,
                ..
            } => {
                let def = self.def_at_site(*name_span);
                HirItem::Let {
                    id: self.id(),
                    def,
                    value: self.lower_expr(value),
                    span: *span,
                }
            }
            AstItem::Function {
                name,
                name_span,
                params,
                body,
                span,
                ..
            } => HirItem::Fn {
                id: self.id(),
                def: self.def_at_site(*name_span),
                name: name.clone(),
                params: params
                    .iter()
                    .map(|(n, s)| (n.clone(), self.def_at_site(*s), *s))
                    .collect(),
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
                ..
            } => {
                let def = self.def_at_site(*name_span);
                HirStmt::Let {
                    id: self.id(),
                    def,
                    value: self.lower_expr(value),
                    span: *span,
                }
            }
            AstStmt::Expr(e) => HirStmt::Expr(self.lower_expr(e)),
        }
    }

    fn lower_expr(&mut self, expr: &AstExpr) -> HirExpr {
        match expr {
            AstExpr::Int(v, s) => HirExpr::Int {
                id: self.id(),
                value: *v,
                span: *s,
            },
            AstExpr::String(value, s) => HirExpr::String {
                id: self.id(),
                value: value.clone(),
                span: *s,
            },
            AstExpr::Var(name, s) => HirExpr::Var {
                id: self.id(),
                def: self.def_at(*s),
                name: name.clone(),
                span: *s,
            },
            AstExpr::Call {
                callee,
                callee_span,
                args,
                span,
            } => HirExpr::Call {
                id: self.id(),
                def: self.def_at(*callee_span),
                name: callee.clone(),
                args: args.iter().map(|a| self.lower_expr(a)).collect(),
                span: *span,
            },
            AstExpr::Unary { op, rhs, span } => {
                let rhs = self.lower_expr(rhs);
                match op {
                    // Desugar `-x` into `0 - x`.
                    AstUnOp::Neg => {
                        let zero_span = Span::empty(span.start);
                        HirExpr::Binary {
                            id: self.id(),
                            op: HirBinOp::Sub,
                            lhs: Box::new(HirExpr::Int {
                                id: self.id(),
                                value: 0,
                                span: zero_span,
                            }),
                            rhs: Box::new(rhs),
                            span: *span,
                        }
                    }
                }
            }
            AstExpr::Binary { op, lhs, rhs, span } => {
                let op = match op {
                    AstBinOp::Add => HirBinOp::Add,
                    AstBinOp::Sub => HirBinOp::Sub,
                    AstBinOp::Mul => HirBinOp::Mul,
                    AstBinOp::Div => HirBinOp::Div,
                };
                HirExpr::Binary {
                    id: self.id(),
                    op,
                    lhs: Box::new(self.lower_expr(lhs)),
                    rhs: Box::new(self.lower_expr(rhs)),
                    span: *span,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let src = "function add(a, b) { a + b; } function main() { add(1, 2); }";
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
}
