//! vl-typecheck: type checking over HIR.
//!
//! Value types are the compiler-owned [`Ty`] (`u64`, `i64`, `f64`, `bool`,
//! `u8`, `string`, `File`, `void`) converted from [`vl_common::VlType`].
//! These are VL language types enforced here — deliberately distinct from any
//! VM representation, which backends map to separately.
//!
//! [`check`] walks the HIR, annotates each node, enforces function boundaries
//! (param types, arity, return types, `void` misuse), checks extern calls
//! against their catalog signatures (carried in HIR from `vl-semantic`), and
//! quietly poisons nodes whose names failed resolution (already reported
//! upstream, so no cascading second error).

use std::collections::{HashMap, HashSet};

use vl_common::Scalar;
use vl_common::{Diagnostic, Span, VlType};
use vl_hir::{HirBinOp, HirExpr, HirItem, HirProgram, HirStmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ty {
    U64,
    I64,
    F64,
    Bool,
    U8,
    String,
    File,
    Void,
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
            Ty::File => write!(f, "File"),
            Ty::Void => write!(f, "void"),
            Ty::Error => write!(f, "<error>"),
        }
    }
}

impl Ty {
    pub fn from_vl(v: VlType) -> Self {
        match v {
            VlType::U64 => Ty::U64,
            VlType::I64 => Ty::I64,
            VlType::F64 => Ty::F64,
            VlType::Bool => Ty::Bool,
            VlType::U8 => Ty::U8,
            VlType::String => Ty::String,
            VlType::File => Ty::File,
            VlType::Void => Ty::Void,
        }
    }
}

/// Compiler-owned signature of one user function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuncSigTy {
    pub param_names: Vec<String>,
    pub param_tys: Vec<Ty>,
    pub ret: Ty,
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
    /// Function `DefId.0` -> declared signature (param + return types).
    pub func_sigs: HashMap<u32, FuncSigTy>,
}

impl TypedProgram {
    pub fn type_of_id(&self, id: vl_hir::HirId) -> Option<Ty> {
        self.types.get(&id.0).copied()
    }

    pub fn sig_of(&self, def: u32) -> Option<&FuncSigTy> {
        self.func_sigs.get(&def)
    }
}

pub fn check(prog: &HirProgram) -> (TypedProgram, Vec<Diagnostic>) {
    let mut cx = Checker {
        typed: TypedProgram::default(),
        diags: vec![],
        bindings: HashMap::new(),
        reported_unknown: HashSet::new(),
    };
    // Pass 1: collect function signatures so calls resolve arity + types
    // regardless of definition order (matches the resolver pre-pass).
    for item in &prog.items {
        if let HirItem::Fn {
            def: Some(d),
            params,
            ret,
            ..
        } = item
        {
            let param_names = params
                .iter()
                .map(|(n, _, _, _)| n.clone())
                .collect::<Vec<_>>();
            let param_tys = params
                .iter()
                .map(|(_, _, t, _)| t.map(Ty::from_vl).unwrap_or(Ty::Error))
                .collect::<Vec<_>>();
            let ret_ty = ret.map(Ty::from_vl).unwrap_or(Ty::Error);
            cx.typed.func_defs.insert(d.0);
            cx.typed.func_arity.insert(d.0, params.len());
            cx.typed.func_sigs.insert(
                d.0,
                FuncSigTy {
                    param_names,
                    param_tys,
                    ret: ret_ty,
                },
            );
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
    reported_unknown: HashSet<u32>,
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
                if ty == Ty::Void {
                    self.diags.push(
                        Diagnostic::error("cannot bind a `void` value")
                            .with_label(value.span(), "`void` is not a value")
                            .with_note("`void` calls may only appear as bare statements")
                            .with_code("E308"),
                    );
                    self.record(*id, Ty::Error);
                    if let Some(def) = def {
                        self.bindings.insert(def.0, Ty::Error);
                        self.typed.globals.push(format!("let#{}", id.0));
                    }
                    return;
                }
                self.record(*id, ty);
                if let Some(def) = def {
                    self.bindings.insert(def.0, ty);
                    self.typed.globals.push(format!("let#{}", id.0));
                } else {
                    // Name resolution already reported this; stay quiet.
                }
            }
            HirItem::Fn {
                id,
                def,
                name,
                params,
                ret,
                ret_span,
                body,
                span,
            } => {
                let ret_ty = ret.map(Ty::from_vl).unwrap_or(Ty::Error);
                self.record(*id, ret_ty);
                // Bad annotations were already reported by the parser
                // (E104/E105); poison the scope quietly so no second error
                // cascades. (An omitted return parses as `void`, never `None`.)
                let poisoned_sig =
                    ret_ty == Ty::Error || params.iter().any(|(_, _, t, _)| t.is_none());
                for (_, def, ty, _) in params {
                    if let Some(def) = def {
                        let t = ty.map(Ty::from_vl).unwrap_or(Ty::Error);
                        self.bindings.insert(def.0, t);
                    }
                }
                let mut last_ty: Option<Ty> = None;
                for stmt in body {
                    if let Some(t) = self.check_stmt(stmt) {
                        last_ty = Some(t);
                    }
                }
                if poisoned_sig {
                    return;
                }
                if ret_ty == Ty::Void {
                    // Value discarded; any body is accepted.
                    return;
                }
                match last_ty {
                    Some(t) if t == ret_ty => {}
                    Some(Ty::Error) => {}
                    Some(t) => {
                        self.diags.push(
                            Diagnostic::error(format!(
                                "function `{name}` declares return `{ret_ty}` but body yields `{t}`"
                            ))
                            .with_label(*span, "mismatched return")
                            .with_code("E307"),
                        );
                    }
                    None => {
                        // Empty body or body ending in `if` with no tail value.
                        // LIR defaults such bodies to `0`; require explicit `void`.
                        let anchor = ret_span.unwrap_or(*span);
                        self.diags.push(
                            Diagnostic::error(format!(
                                "function `{name}` declares return `{ret_ty}` but has no tail value"
                            ))
                            .with_label(anchor, "declared here")
                            .with_note("end the body with an expression of the return type, or declare `: void`")
                            .with_code("E307"),
                        );
                    }
                }
                let _ = def;
            }
        }
    }

    /// Check a statement. Returns the tail-value type when the statement
    /// produces one (`let` initializer / expression value); `if` preserves
    /// the incoming tail (matches LIR, which leaves `last` untouched).
    fn check_stmt(&mut self, stmt: &HirStmt) -> Option<Ty> {
        match stmt {
            HirStmt::Let { id, def, value, .. } => {
                let ty = self.infer_expr(value);
                if ty == Ty::Void {
                    self.diags.push(
                        Diagnostic::error("cannot bind a `void` value")
                            .with_label(value.span(), "`void` is not a value")
                            .with_note("`void` calls may only appear as bare statements")
                            .with_code("E308"),
                    );
                    self.record(*id, Ty::Error);
                    if let Some(def) = def {
                        self.bindings.insert(def.0, Ty::Error);
                    }
                    return Some(Ty::Error);
                }
                self.record(*id, ty);
                if let Some(def) = def {
                    self.bindings.insert(def.0, ty);
                }
                Some(ty)
            }
            HirStmt::Expr(e) => Some(self.infer_expr(e)),
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
                None
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn check_call_args(
        &mut self,
        name: &str,
        span: Span,
        args: &[HirExpr],
        arg_tys: &[Ty],
        params: &[vl_common::ParamSig],
        ret_vl: VlType,
        id: vl_hir::HirId,
    ) -> Ty {
        let ret = Ty::from_vl(ret_vl);
        if args.len() != params.len() {
            self.diags.push(
                Diagnostic::error(format!(
                    "`{name}` expects {} argument(s), got {}",
                    params.len(),
                    args.len()
                ))
                .with_label(span, "wrong number of arguments")
                .with_code("E303"),
            );
            return self.record(id, Ty::Error);
        }
        // Poisoned annotations stay quiet (root cause already reported).
        if params.iter().any(|p| Ty::from_vl(p.ty) == Ty::Error) || ret == Ty::Error {
            return self.record(id, Ty::Error);
        }
        for (i, (arg, got)) in args.iter().zip(arg_tys.iter()).enumerate() {
            if *got == Ty::Error {
                continue;
            }
            let want = Ty::from_vl(params[i].ty);
            if *got == Ty::Void || want == Ty::Void {
                self.diags.push(
                    Diagnostic::error(format!(
                        "`{name}` parameter `{}` cannot be `void`",
                        params[i].name
                    ))
                    .with_label(arg.span(), "unexpected `void` here")
                    .with_code("E308"),
                );
                return self.record(id, Ty::Error);
            }
            if *got != want {
                self.diags.push(
                    Diagnostic::error(format!(
                        "`{name}` parameter `{}` expects `{}`, got `{got}`",
                        params[i].name, want
                    ))
                    .with_label(arg.span(), format!("expected `{want}` here"))
                    .with_code("E306"),
                );
                return self.record(id, Ty::Error);
            }
        }
        self.record(id, ret)
    }

    fn infer_expr(&mut self, expr: &HirExpr) -> Ty {
        match expr {
            HirExpr::Literal { id, value, .. } => self.record(*id, scalar_ty(*value)),
            HirExpr::String { id, .. } => self.record(*id, Ty::String),
            HirExpr::Var { id, def, span, .. } => {
                // Unresolved names were already reported by `vl-semantic`;
                // poison quietly instead of cascading a second error.
                if def.is_none() {
                    self.record(*id, Ty::Error)
                } else {
                    let def_id = def.as_ref().map(|d| d.0).expect("checked above");
                    let ty = def
                        .as_ref()
                        .and_then(|def| self.bindings.get(&def.0).copied())
                        .unwrap_or_else(|| {
                            if self.reported_unknown.insert(def_id) {
                                self.diags.push(
                                    Diagnostic::error(
                                        "cannot infer the type of this forward global reference",
                                    )
                                    .with_label(*span, "type is not known yet")
                                    .with_note("define the global before using it")
                                    .with_code("E305"),
                                );
                            }
                            Ty::Error
                        });
                    self.record(*id, ty)
                }
            }
            HirExpr::Call {
                id,
                def,
                external,
                extern_sig,
                name,
                args,
                span,
            } => {
                let mut arg_tys = Vec::with_capacity(args.len());
                let mut poisoned = false;
                for arg in args {
                    let t = self.infer_expr(arg);
                    if t == Ty::Error {
                        poisoned = true;
                    }
                    arg_tys.push(t);
                }
                let Some(d) = def else {
                    // Unresolved callee already reported; stay quiet.
                    return self.record(*id, Ty::Error);
                };
                if *external {
                    let Some(sig) = extern_sig else {
                        // Poisoned import (E202/E203 already reported).
                        return self.record(*id, Ty::Error);
                    };
                    if poisoned {
                        return self.record(*id, Ty::Error);
                    }
                    return self.check_call_args(
                        name,
                        *span,
                        args,
                        &arg_tys,
                        &sig.params,
                        sig.ret,
                        *id,
                    );
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
                let Some(sig) = self.typed.func_sigs.get(&d.0).cloned() else {
                    return self.record(*id, Ty::Error);
                };
                if sig.ret == Ty::Error || sig.param_tys.contains(&Ty::Error) {
                    // Definition already poisoned (missing annotations); quiet.
                    return self.record(*id, Ty::Error);
                }
                if args.len() != sig.param_tys.len() {
                    self.diags.push(
                        Diagnostic::error(format!(
                            "`{name}` expects {} argument(s), got {}",
                            sig.param_tys.len(),
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
                for (i, got) in arg_tys.iter().enumerate() {
                    if *got == Ty::Error {
                        continue;
                    }
                    let want = sig.param_tys[i];
                    if *got == Ty::Void || want == Ty::Void {
                        self.diags.push(
                            Diagnostic::error(format!(
                                "`{name}` parameter `{}` cannot be `void`",
                                sig.param_names[i]
                            ))
                            .with_label(args[i].span(), "unexpected `void` here")
                            .with_code("E308"),
                        );
                        return self.record(*id, Ty::Error);
                    }
                    if *got != want {
                        self.diags.push(
                            Diagnostic::error(format!(
                                "`{name}` parameter `{}` expects `{}`, got `{got}`",
                                sig.param_names[i], want
                            ))
                            .with_label(args[i].span(), format!("expected `{want}` here"))
                            .with_code("E306"),
                        );
                        return self.record(*id, Ty::Error);
                    }
                }
                self.record(*id, sig.ret)
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
                if lt == Ty::Void || rt == Ty::Void {
                    self.diags.push(
                        Diagnostic::error("cannot use a `void` value in an operation")
                            .with_label(*span, "`void` is not a value")
                            .with_code("E308"),
                    );
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
    matches!(expr, HirExpr::Literal { value, .. } if scalar_is_zero(*value))
}

fn scalar_is_zero(value: Scalar) -> bool {
    match value {
        Scalar::I64(v) => v == 0,
        Scalar::U64(v) => v == 0,
        Scalar::U8(v) => v == 0,
        Scalar::F64(v) => f64::from_bits(v) == 0.0,
        Scalar::Bool(_) => false,
    }
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
    fn call_with_correct_types_checks_clean() {
        let (_, diags) = check_src(
            "function add(a: i64, b: i64): i64 { a + b; } function main() { add(1, 2); }",
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn call_with_wrong_arity_errors_once() {
        let (_, diags) =
            check_src("function add(a: i64, b: i64): i64 { a + b; } function main() { add(1); }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("expects 2"));
    }

    #[test]
    fn call_with_wrong_param_type_errors() {
        let (_, diags) = check_src(
            r#"function add(a: i64, b: i64): i64 { a + b; } function main() { add(1, "s"); }"#,
        );
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("expects `i64`"), "{diags:?}");
    }

    #[test]
    fn return_mismatch_errors() {
        let (_, diags) = check_src(r#"function f(): i64 { "s"; }"#);
        assert!(
            diags.iter().any(|d| d.message.contains("declares return")),
            "{diags:?}"
        );
    }

    #[test]
    fn void_function_accepts_any_tail() {
        let (_, diags) = check_src("function main() { 1 + 2; }");
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn binding_void_errors() {
        let (_, diags) = check_src("use std.print; function main() { let x = print(\"hi\"); }");
        assert!(
            diags.iter().any(|d| d.message.contains("void")),
            "{diags:?}"
        );
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

    #[test]
    fn forward_global_type_is_not_invented() {
        let (_, diags) = check_src("let x = later + 1; let later = \"s\";");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("forward global"));
    }

    #[test]
    fn every_numeric_zero_divisor_is_rejected() {
        for source in [
            "let x = 1u64 / 0u64;",
            "let x = 1u8 / 0u8;",
            "let x = 1.0f64 / 0.0f64;",
        ] {
            let (_, diags) = check_src(source);
            assert!(
                diags.iter().any(|d| d.message.contains("division by zero")),
                "{source}: {diags:?}"
            );
        }
    }
}
