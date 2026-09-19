//! vl-typecheck: type checking over HIR.
//!
//! v0 type system: exactly one type, `int`. Every expression must be `int`.
//! That sounds trivial, but the scaffolding is the point — [`check`]
//! walks the HIR, annotates each node with [`Ty`], and quietly poisons
//! nodes whose names failed resolution (already reported upstream, so no
//! cascading second error).
//!
//! When the language grows (strings, bools, functions), only [`Ty`]
//! and `infer_expr` need to change; the driver and later stages keep
//! working because they consume [`TypedProgram`].

use std::collections::HashMap;

use vl_common::{Diagnostic, Span};
use vl_hir::{HirBinOp, HirExpr, HirItem, HirProgram, HirStmt};

/// v0 has one type. Future types get added here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ty {
    Int,
    /// Poison: an earlier error made this node's type unknowable.
    /// Poisoned nodes don't produce follow-on errors.
    Error,
}

impl std::fmt::Display for Ty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Ty::Int => write!(f, "int"),
            Ty::Error => write!(f, "<error>"),
        }
    }
}

/// HIR node id -> inferred type.
#[derive(Debug, Default)]
pub struct TypedProgram {
    pub types: HashMap<u32, Ty>,
    /// Top-level value names in order (for LIR/codegen).
    pub globals: Vec<String>,
}

impl TypedProgram {
    pub fn type_of_id(&self, id: vl_hir::HirId) -> Option<Ty> {
        self.types.get(&id.0).copied()
    }
}

pub fn check(prog: &HirProgram) -> (TypedProgram, Vec<Diagnostic>) {
    let mut cx = Checker {
        typed: TypedProgram::default(),
        diags: vec![],
    };
    for item in &prog.items {
        cx.check_item(item);
    }
    (cx.typed, cx.diags)
}

struct Checker {
    typed: TypedProgram,
    diags: Vec<Diagnostic>,
}

impl Checker {
    fn record(&mut self, id: vl_hir::HirId, ty: Ty) -> Ty {
        self.typed.types.insert(id.0, ty);
        ty
    }

    fn check_item(&mut self, item: &HirItem) {
        match item {
            HirItem::Let { id, def, value, .. } => {
                let ty = self.infer_expr(value);
                self.record(*id, ty);
                if def.is_none() {
                    // Name resolution already reported this; stay quiet.
                } else {
                    self.typed.globals.push(format!("let#{}", id.0));
                }
            }
            HirItem::Fn {
                id, params, body, ..
            } => {
                self.record(*id, Ty::Int);
                for stmt in body {
                    self.check_stmt(stmt);
                }
                let _ = params;
            }
        }
    }

    fn check_stmt(&mut self, stmt: &HirStmt) {
        match stmt {
            HirStmt::Let { id, value, .. } => {
                let ty = self.infer_expr(value);
                self.record(*id, ty);
            }
            HirStmt::Expr(e) => {
                self.infer_expr(e);
            }
        }
    }

    fn infer_expr(&mut self, expr: &HirExpr) -> Ty {
        match expr {
            HirExpr::Int { id, .. } => self.record(*id, Ty::Int),
            HirExpr::Var { id, def, .. } => {
                // Unresolved names were already reported by `vl-semantic`;
                // poison quietly instead of cascading a second error.
                if def.is_none() {
                    self.record(*id, Ty::Error)
                } else {
                    self.record(*id, Ty::Int)
                }
            }
            HirExpr::Binary {
                id,
                op,
                lhs,
                rhs,
                span,
            } => {
                let lt = self.infer_expr(lhs);
                let rt = self.infer_expr(rhs);
                if lt == Ty::Error || rt == Ty::Error {
                    return self.record(*id, Ty::Error);
                }
                // v0: both sides must be int; they always are, but keep the
                // check explicit so future types slot in here.
                if lt != Ty::Int || rt != Ty::Int {
                    self.diags.push(mismatch(*span, lt, rt));
                    return self.record(*id, Ty::Error);
                }
                if matches!(op, HirBinOp::Div) && is_zero_literal(rhs) {
                    self.diags.push(
                        Diagnostic::error("division by zero")
                            .with_label(rhs.span(), "denominator is a constant zero")
                            .with_code("E301"),
                    );
                    return self.record(*id, Ty::Error);
                }
                self.record(*id, Ty::Int)
            }
        }
    }
}

fn mismatch(span: Span, lt: Ty, rt: Ty) -> Diagnostic {
    Diagnostic::error(format!("type mismatch: {lt} vs {rt}"))
        .with_label(span, format!("expected {lt} on both sides"))
        .with_code("E302")
}

fn is_zero_literal(expr: &HirExpr) -> bool {
    matches!(expr, HirExpr::Int { value: 0, .. })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check_src(src: &str) -> (TypedProgram, Vec<Diagnostic>) {
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        check(&hir)
    }

    #[test]
    fn ints_check_clean() {
        let (_, diags) = check_src("let x = 1 + 2 * 3;");
        assert!(diags.is_empty());
    }

    #[test]
    fn const_div_by_zero_errors() {
        let (_, diags) = check_src("let x = 1 / 0;");
        assert!(diags.iter().any(|d| d.message.contains("division by zero")));
    }
}
