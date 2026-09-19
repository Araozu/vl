//! vl-typecheck: type checking over HIR.
//!
//! The value types include `u64`, `i64`, `f64`, `bool`, `u8`, and byte strings.
//! Arithmetic requires matching numeric types; function boundaries remain unchecked in v0. That sounds
//! trivial, but the scaffolding is the point — [`check`] walks the HIR,
//! annotates each node with [`Ty`], enforces call arity/callability, and
//! quietly poisons nodes whose names failed resolution (already reported
//! upstream, so no cascading second error).
//!
//! When the language grows (richer strings and function types), only
//! [`Ty`] and `infer_expr` need to change; the driver and later stages keep
//! working because they consume [`TypedProgram`].

use std::collections::HashMap;

use vl_common::Scalar;
use vl_common::{Diagnostic, Span};
use vl_hir::{HirBinOp, HirExpr, HirItem, HirProgram, HirStmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ty {
    U64,
    I64,
    F64,
    Bool,
    U8,
    String,
    /// Poison: an earlier error made this node's type unknowable.
    /// Poisoned nodes don't produce follow-on errors.
    Error,
}

impl std::fmt::Display for Ty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Ty::U64 => write!(f, "u64"),
            Ty::I64 => write!(f, "i64"),
            Ty::F64 => write!(f, "f64"),
            Ty::Bool => write!(f, "bool"),
            Ty::U8 => write!(f, "u8"),
            Ty::String => write!(f, "string"),
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
    /// Function `DefId.0` -> parameter count (for arity checks + LIR).
    pub func_arity: HashMap<u32, usize>,
    /// `DefId.0` of every `function` item (callability checks).
    pub func_defs: std::collections::HashSet<u32>,
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
        bindings: HashMap::new(),
    };
    // Pass 1: collect function signatures so calls resolve arity
    // regardless of definition order (matches the resolver pre-pass).
    for item in &prog.items {
        if let HirItem::Fn {
            def: Some(d),
            params,
            ..
        } = item
        {
            cx.typed.func_defs.insert(d.0);
            cx.typed.func_arity.insert(d.0, params.len());
        }
    }
    for item in &prog.items {
        cx.check_item(item);
    }
    (cx.typed, cx.diags)
}

struct Checker {
    typed: TypedProgram,
    diags: Vec<Diagnostic>,
    bindings: HashMap<u32, Ty>,
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
                if let Some(def) = def {
                    self.bindings.insert(def.0, ty);
                    self.typed.globals.push(format!("let#{}", id.0));
                } else {
                    // Name resolution already reported this; stay quiet.
                }
            }
            HirItem::Fn {
                id, params, body, ..
            } => {
                self.record(*id, Ty::I64);
                for (_, def, _) in params {
                    if let Some(def) = def {
                        self.bindings.insert(def.0, Ty::I64);
                    }
                }
                for stmt in body {
                    self.check_stmt(stmt);
                }
            }
        }
    }

    fn check_stmt(&mut self, stmt: &HirStmt) {
        match stmt {
            HirStmt::Let { id, def, value, .. } => {
                let ty = self.infer_expr(value);
                self.record(*id, ty);
                if let Some(def) = def {
                    self.bindings.insert(def.0, ty);
                }
            }
            HirStmt::Expr(e) => {
                self.infer_expr(e);
            }
            HirStmt::If {
                condition,
                then_body,
                else_body,
                ..
            } => {
                let condition_ty = self.infer_expr(condition);
                if condition_ty != Ty::Error && condition_ty != Ty::Bool {
                    self.diags.push(
                        Diagnostic::error(format!("if condition must be bool, got {condition_ty}"))
                            .with_label(condition.span(), "expected bool")
                            .with_code("E304"),
                    );
                }
                for stmt in then_body {
                    self.check_stmt(stmt);
                }
                if let Some(body) = else_body {
                    for stmt in body {
                        self.check_stmt(stmt);
                    }
                }
            }
        }
    }

    fn infer_expr(&mut self, expr: &HirExpr) -> Ty {
        match expr {
            HirExpr::Literal { id, value, .. } => self.record(*id, scalar_ty(*value)),
            HirExpr::String { id, .. } => self.record(*id, Ty::String),
            HirExpr::Var { id, def, .. } => {
                // Unresolved names were already reported by `vl-semantic`;
                // poison quietly instead of cascading a second error.
                if def.is_none() {
                    self.record(*id, Ty::Error)
                } else {
                    let ty = def
                        .as_ref()
                        .and_then(|def| self.bindings.get(&def.0).copied())
                        .unwrap_or(Ty::I64);
                    self.record(*id, ty)
                }
            }
            HirExpr::Call {
                id,
                def,
                external,
                name,
                args,
                span,
            } => {
                let mut poisoned = false;
                for arg in args {
                    if self.infer_expr(arg) == Ty::Error {
                        poisoned = true;
                    }
                }
                let Some(d) = def else {
                    // Unresolved callee already reported; stay quiet.
                    return self.record(*id, Ty::Error);
                };
                if *external {
                    return self.record(*id, Ty::I64);
                }
                if !self.typed.func_defs.contains(&d.0) {
                    self.diags.push(
                        Diagnostic::error(format!("`{name}` is not a function"))
                            .with_label(*span, "cannot call a non-function value")
                            .with_note("only `function` items are callable in v0")
                            .with_code("E303"),
                    );
                    return self.record(*id, Ty::Error);
                }
                let arity = self.typed.func_arity.get(&d.0).copied().unwrap_or(0);
                if args.len() != arity {
                    self.diags.push(
                        Diagnostic::error(format!(
                            "`{name}` expects {arity} argument(s), got {}",
                            args.len()
                        ))
                        .with_label(*span, "wrong number of arguments")
                        .with_code("E303"),
                    );
                    return self.record(*id, Ty::Error);
                }
                if poisoned {
                    return self.record(*id, Ty::Error);
                }
                self.record(*id, Ty::I64)
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
                if lt != rt || !is_numeric(lt) {
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
                self.record(*id, lt)
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
    matches!(
        expr,
        HirExpr::Literal {
            value: Scalar::I64(0),
            ..
        }
    )
}

fn scalar_ty(value: Scalar) -> Ty {
    match value {
        Scalar::U64(_) => Ty::U64,
        Scalar::I64(_) => Ty::I64,
        Scalar::F64(_) => Ty::F64,
        Scalar::Bool(_) => Ty::Bool,
        Scalar::U8(_) => Ty::U8,
    }
}

fn is_numeric(ty: Ty) -> bool {
    matches!(ty, Ty::U64 | Ty::I64 | Ty::F64 | Ty::U8)
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
    fn strings_check_as_string_type() {
        let (typed, diags) = check_src(r#"let greeting = "hello";"#);
        assert!(diags.is_empty());
        assert_eq!(typed.types.values().next(), Some(&Ty::String));
    }

    #[test]
    fn string_bindings_keep_their_type() {
        let (typed, diags) = check_src(r#"let greeting = "hello"; let copy = greeting;"#);
        assert!(diags.is_empty());
        assert_eq!(typed.types.get(&3), Some(&Ty::String));
    }

    #[test]
    fn const_div_by_zero_errors() {
        let (_, diags) = check_src("let x = 1 / 0;");
        assert!(diags.iter().any(|d| d.message.contains("division by zero")));
    }

    #[test]
    fn call_with_correct_arity_checks_clean() {
        let (_, diags) = check_src("function add(a, b) { a + b; } function main() { add(1, 2); }");
        assert!(diags.is_empty());
    }

    #[test]
    fn call_with_wrong_arity_errors_once() {
        let (_, diags) = check_src("function add(a, b) { a + b; } function main() { add(1); }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("expects 2"));
    }

    #[test]
    fn calling_a_let_binding_errors() {
        let (_, diags) = check_src("let x = 1; function main() { x(); }");
        assert!(diags.iter().any(|d| d.message.contains("not a function")));
    }

    #[test]
    fn unresolved_callee_poisoned_quietly() {
        // E201 comes from resolve; typecheck must not add a second error.
        let (toks, _) = vl_lex::lex("function main() { nope(1); }");
        let (prog, _) = vl_syntax::parse(&toks, "");
        let (res, rdiags) = vl_semantic::resolve(&prog);
        assert!(rdiags.iter().any(|d| d.is_error()));
        let hir = vl_hir::lower(&prog, &res);
        let (_, tdiags) = check(&hir);
        assert!(tdiags.is_empty());
    }
}
