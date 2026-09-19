//! vl-typecheck: type checking over HIR.
//!
//! Value types are the compiler-owned [`Ty`] (`u64`, `i64`, `f64`, `bool`,
//! `u8`, `string`, `File`, `void`) converted from [`vl_common::VlType`].
//! These are VL language types enforced here — deliberately distinct from any
//! VM representation, which backends map to separately.
//!
//! [`check`] walks the HIR, annotates each node, enforces function boundaries
//! (param types, arity, explicit `return` types, `void` misuse), checks extern
//! calls against their catalog signatures (carried in HIR from `vl-semantic`),
//! and quietly poisons nodes whose names failed resolution (already reported
//! upstream, so no cascading second error). There are no implicit returns:
//! only `return expr;` satisfies a value return; bare trailing expressions
//! are discarded.

use std::collections::{HashMap, HashSet};

use vl_common::Scalar;
use vl_common::{Diagnostic, Span, VlType};
use vl_hir::{HirBinOp, HirExpr, HirItem, HirProgram, HirStmt, HirUnOp};

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
        fn_ret: Ty::Void,
        fn_name: String::new(),
        fn_ret_span: None,
        fn_span: Span::empty(0),
        saw_value_return: false,
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
    fn_ret: Ty,
    fn_name: String,
    fn_ret_span: Option<Span>,
    fn_span: Span,
    saw_value_return: bool,
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
                // Explicit returns only: the body's tail value is discarded.
                // Track `return expr;` statements (including inside `if` /
                // `while`) to enforce the declared return type.
                self.fn_ret = ret_ty;
                self.fn_name = name.clone();
                self.fn_ret_span = *ret_span;
                self.fn_span = *span;
                self.saw_value_return = false;
                for stmt in body {
                    self.check_stmt(stmt);
                }
                if poisoned_sig {
                    return;
                }
                if ret_ty == Ty::Void {
                    // `return;` is optional; bare expression values are
                    // discarded. Any body is accepted.
                    return;
                }
                if !self.saw_value_return {
                    // No `return expr;` anywhere (poisoned returns still set
                    // the flag so a single root cause stays single).
                    let anchor = ret_span.unwrap_or(*span);
                    self.diags.push(
                        Diagnostic::error(format!(
                            "function `{name}` declares return `{ret_ty}` but has no `return` statement"
                        ))
                        .with_label(anchor, "declared here")
                        .with_note("add `return <expr>;` of the return type (`return;` is only for `void`)")
                        .with_code("E307"),
                    );
                }
                let _ = def;
            }
        }
    }

    /// Check a statement. There are no implicit returns: `let` initializers
    /// and bare expression values are discarded and never satisfy a declared
    /// return type — only an explicit `return expr;` does.
    fn check_stmt(&mut self, stmt: &HirStmt) {
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
                    return;
                }
                self.record(*id, ty);
                if let Some(def) = def {
                    self.bindings.insert(def.0, ty);
                }
            }
            HirStmt::Expr(e) => {
                // Value discarded; still infer for inner errors.
                let _ = self.infer_expr(e);
            }
            HirStmt::Return { value, span } => {
                match value {
                    None => {
                        // Bare `return;`: only valid for `void` (or poisoned).
                        if self.fn_ret == Ty::Void || self.fn_ret == Ty::Error {
                            return;
                        }
                        self.diags.push(
                            Diagnostic::error(format!(
                                "function `{}` declares return `{}` but returns nothing",
                                self.fn_name, self.fn_ret
                            ))
                            .with_label(*span, "bare `return` here")
                            .with_note("use `return <expr>;` with a value of the return type")
                            .with_code("E307"),
                        );
                    }
                    Some(e) => {
                        let got = self.infer_expr(e);
                        // Poisoned values already reported; mark seen so the
                        // missing-`return` check does not cascade.
                        if got == Ty::Error || self.fn_ret == Ty::Error {
                            self.saw_value_return = true;
                            return;
                        }
                        if self.fn_ret == Ty::Void {
                            self.diags.push(
                                Diagnostic::error(format!(
                                    "function `{}` returns `void` but returns a value",
                                    self.fn_name
                                ))
                                .with_label(*span, format!("unexpected `{got}` here"))
                                .with_note(
                                    "declare a return type (`: <type>`) or use bare `return;`",
                                )
                                .with_code("E307"),
                            );
                            return;
                        }
                        if got != self.fn_ret {
                            self.diags.push(
                                Diagnostic::error(format!(
                                    "function `{}` declares return `{}` but returns `{got}`",
                                    self.fn_name, self.fn_ret
                                ))
                                .with_label(*span, "mismatched `return`")
                                .with_code("E307"),
                            );
                            // Count as seen: the mismatch is the single error.
                            self.saw_value_return = true;
                            return;
                        }
                        self.saw_value_return = true;
                    }
                }
            }
            HirStmt::Assign { id, def, value, .. } => {
                let got = self.infer_expr(value);
                let Some(def) = def else {
                    // Unresolved target already reported (E201); stay quiet.
                    self.record(*id, Ty::Error);
                    return;
                };
                if got == Ty::Error {
                    self.record(*id, Ty::Error);
                    return;
                }
                match self.bindings.get(&def.0).copied() {
                    None => {
                        self.diags.push(
                            Diagnostic::error("cannot assign before the binding type is known")
                                .with_label(value.span(), "type is not known yet")
                                .with_note("define the binding before assigning to it")
                                .with_code("E305"),
                        );
                        self.record(*id, Ty::Error);
                    }
                    Some(Ty::Error) => {
                        self.record(*id, Ty::Error);
                    }
                    Some(want) => {
                        if got == Ty::Void || want == Ty::Void {
                            self.diags.push(
                                Diagnostic::error("cannot assign a `void` value")
                                    .with_label(value.span(), "`void` is not a value")
                                    .with_code("E308"),
                            );
                            self.record(*id, Ty::Error);
                            return;
                        }
                        if got != want {
                            self.diags.push(
                                Diagnostic::error(format!(
                                    "cannot assign `{got}` to `{want}` binding"
                                ))
                                .with_label(value.span(), format!("expected `{want}` here"))
                                .with_code("E309"),
                            );
                            self.record(*id, Ty::Error);
                            return;
                        }
                        self.record(*id, want);
                    }
                }
            }
            HirStmt::Break { .. } | HirStmt::Continue { .. } => {}
            HirStmt::While {
                condition, body, ..
            } => {
                let condition_ty = self.infer_expr(condition);
                if condition_ty != Ty::Error && condition_ty != Ty::Bool {
                    self.diags.push(
                        Diagnostic::error(format!(
                            "while condition must be bool, got {condition_ty}"
                        ))
                        .with_label(condition.span(), "expected bool")
                        .with_code("E304"),
                    );
                }
                for stmt in body {
                    self.check_stmt(stmt);
                }
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
            HirExpr::Unary {
                id,
                op,
                inner,
                span,
            } => {
                let inner_ty = self.infer_expr(inner);
                if inner_ty == Ty::Error {
                    return self.record(*id, Ty::Error);
                }
                match op {
                    HirUnOp::Not => {
                        if inner_ty != Ty::Bool {
                            self.diags.push(
                                Diagnostic::error(format!(
                                    "not operator requires bool, got {inner_ty}"
                                ))
                                .with_label(*span, "expected bool here")
                                .with_code("E304"),
                            );
                            return self.record(*id, Ty::Error);
                        }
                        self.record(*id, Ty::Bool)
                    }
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
                if lt == Ty::Void || rt == Ty::Void {
                    self.diags.push(
                        Diagnostic::error("cannot use a `void` value in an operation")
                            .with_label(*span, "`void` is not a value")
                            .with_code("E308"),
                    );
                    return self.record(*id, Ty::Error);
                }
                match op {
                    HirBinOp::Add | HirBinOp::Sub | HirBinOp::Mul | HirBinOp::Div => {
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
                    HirBinOp::Eq | HirBinOp::Ne => {
                        if lt != rt || !is_comparable(lt) {
                            self.diags.push(mismatch(*span, lt, rt));
                            return self.record(*id, Ty::Error);
                        }
                        self.record(*id, Ty::Bool)
                    }
                    HirBinOp::Lt | HirBinOp::Le | HirBinOp::Gt | HirBinOp::Ge => {
                        if lt != rt || !is_numeric(lt) {
                            self.diags.push(mismatch(*span, lt, rt));
                            return self.record(*id, Ty::Error);
                        }
                        self.record(*id, Ty::Bool)
                    }
                    HirBinOp::And | HirBinOp::Or => {
                        if lt != Ty::Bool || rt != Ty::Bool {
                            self.diags.push(
                                Diagnostic::error(format!(
                                    "logical operator requires bool on both sides, got {lt} and {rt}"
                                ))
                                .with_label(*span, "expected bool here")
                                .with_code("E304"),
                            );
                            return self.record(*id, Ty::Error);
                        }
                        self.record(*id, Ty::Bool)
                    }
                }
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

fn is_comparable(ty: Ty) -> bool {
    matches!(
        ty,
        Ty::U64 | Ty::I64 | Ty::F64 | Ty::Bool | Ty::U8 | Ty::String
    )
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
    fn comparisons_and_logic_yield_bool() {
        let (typed, diags) =
            check_src("function main() { let a = 1; let ok = a < 2 && a == 1 || !false; ok; }");
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.types.values().any(|t| *t == Ty::Bool));
    }

    #[test]
    fn mixed_numeric_comparison_errors() {
        let (_, diags) = check_src("function main() { let x = 1 < 2u64; x; }");
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("E302")),
            "{diags:?}"
        );
    }

    #[test]
    fn logical_operators_require_bool() {
        let (_, diags) = check_src("function main() { let x = 1 && true; x; }");
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("E304")),
            "{diags:?}"
        );
    }

    #[test]
    fn not_requires_bool() {
        let (_, diags) = check_src("function main() { !1; }");
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("E304")),
            "{diags:?}"
        );
    }

    #[test]
    fn while_condition_must_be_bool() {
        let (_, diags) = check_src("function main() { while (1) { 2; } }");
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("while condition must be bool")),
            "{diags:?}"
        );
    }

    #[test]
    fn assignment_type_mismatch_errors() {
        let (_, diags) = check_src(r#"function main() { let x = 1; x = "s"; }"#);
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("E309")),
            "{diags:?}"
        );
    }

    #[test]
    fn assignment_with_matching_type_checks() {
        let (_, diags) = check_src("function main() { let x = 1; x = 2; }");
        assert!(diags.is_empty(), "{diags:?}");
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
            "function add(a: i64, b: i64): i64 { return a + b; } function main() { add(1, 2); }",
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn call_with_wrong_arity_errors_once() {
        let (_, diags) = check_src(
            "function add(a: i64, b: i64): i64 { return a + b; } function main() { add(1); }",
        );
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("expects 2"));
    }

    #[test]
    fn call_with_wrong_param_type_errors() {
        let (_, diags) = check_src(
            r#"function add(a: i64, b: i64): i64 { return a + b; } function main() { add(1, "s"); }"#,
        );
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("expects `i64`"), "{diags:?}");
    }

    #[test]
    fn return_mismatch_errors() {
        let (_, diags) = check_src(r#"function f(): i64 { return "s"; }"#);
        assert!(
            diags.iter().any(|d| d.message.contains("declares return")),
            "{diags:?}"
        );
    }

    #[test]
    fn trailing_expr_is_not_a_return() {
        // No implicit returns: a bare tail value does not satisfy `: i64`.
        let (_, diags) = check_src(r#"function f(): i64 { "s"; }"#);
        assert!(
            diags.iter().any(|d| d.message.contains("has no `return`")),
            "{diags:?}"
        );
    }

    #[test]
    fn explicit_return_satisfies_declared_type() {
        let (_, diags) = check_src(r#"function f(): i64 { return 1; }"#);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn missing_return_is_an_error() {
        let (_, diags) = check_src(r#"function f(): i64 { let x = 1; }"#);
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("E307")),
            "{diags:?}"
        );
    }

    #[test]
    fn bare_return_in_value_function_errors() {
        let (_, diags) = check_src(r#"function f(): i64 { return; }"#);
        assert!(
            diags.iter().any(|d| d.message.contains("returns nothing")),
            "{diags:?}"
        );
    }

    #[test]
    fn value_return_in_void_function_errors() {
        let (_, diags) = check_src(r#"function main() { return 1; }"#);
        assert!(
            diags.iter().any(|d| d.message.contains("returns `void`")),
            "{diags:?}"
        );
    }

    #[test]
    fn bare_return_in_void_function_checks() {
        let (_, diags) = check_src(r#"function main() { return; }"#);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn return_inside_branch_satisfies_declared_type() {
        let (_, diags) =
            check_src(r#"function f(x: bool): i64 { if (x) { return 1; } else { return 2; } }"#);
        assert!(diags.is_empty(), "{diags:?}");
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
