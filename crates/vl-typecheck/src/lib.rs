//! vl-typecheck: type checking over HIR.
//!
//! Value types are the compiler-owned [`Ty`] (`u64`, `i64`, `f64`, `bool`,
//! `u8`, `string`, `File`, `Array[T]`, `void`) converted from [`vl_common::VlType`].
//! These are VL language types enforced here — deliberately distinct from any
//! VM representation, which backends map to separately.
//!
//! Generics are purely a frontend concern: `Array[T]` checks element types,
//! generic functions (`function first[T](a: Array[T]): T`) check once with
//! their parameters opaque (`Ty::Param`) and monomorphize per concrete
//! call (`first$u64`, ...). LIR and backends only ever see concrete types.
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ty {
    /// Integer literal whose concrete integer type is supplied by context.
    Int,
    U64,
    I64,
    F64,
    Bool,
    U8,
    String,
    File,
    /// Fixed-length heap array of `T` (reference type, like `String`).
    Array(Box<Ty>),
    /// Opaque use of an enclosing generic function's type parameter.
    Param(String),
    Void,
    /// Poison: an earlier error made this node's type unknowable.
    /// Poisoned nodes don't produce follow-on errors.
    Error,
}

impl std::fmt::Display for Ty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Ty::U64 => write!(f, "u64"),
            Ty::Int => write!(f, "int"),
            Ty::I64 => write!(f, "i64"),
            Ty::F64 => write!(f, "f64"),
            Ty::Bool => write!(f, "bool"),
            Ty::U8 => write!(f, "u8"),
            Ty::String => write!(f, "string"),
            Ty::File => write!(f, "File"),
            Ty::Array(elem) => write!(f, "Array[{elem}]"),
            Ty::Param(name) => write!(f, "{name}"),
            Ty::Void => write!(f, "void"),
            Ty::Error => write!(f, "<error>"),
        }
    }
}

impl Ty {
    pub fn from_vl(v: &VlType) -> Self {
        Self::from_vl_in(v, &HashMap::new())
    }

    /// Convert with a type-parameter environment: `Param(name)` looks up
    /// `env`, and an unbound name becomes [`Ty::Error`] (the caller reports;
    /// declaration positions are already validated by the parser).
    pub fn from_vl_in(v: &VlType, env: &HashMap<String, Ty>) -> Self {
        match v {
            VlType::U64 => Ty::U64,
            VlType::I64 => Ty::I64,
            VlType::F64 => Ty::F64,
            VlType::Bool => Ty::Bool,
            VlType::U8 => Ty::U8,
            VlType::String => Ty::String,
            VlType::File => Ty::File,
            VlType::Array(elem) => Ty::Array(Box::new(Self::from_vl_in(elem, env))),
            VlType::Param(name) => env.get(name).cloned().unwrap_or(Ty::Error),
            VlType::Void => Ty::Void,
        }
    }

    /// Fully concrete (no `Param` inside)? Only concrete types reach LIR.
    pub fn is_concrete(&self) -> bool {
        match self {
            Ty::Array(elem) => elem.is_concrete(),
            Ty::Param(_) | Ty::Error => false,
            _ => true,
        }
    }

    /// Element type for `Array[T]`; `None` for everything else.
    pub fn array_elem(&self) -> Option<&Ty> {
        match self {
            Ty::Array(elem) => Some(elem),
            _ => None,
        }
    }
}

/// Substitute type parameters via `env` (`Param(name)` -> mapped type).
/// Names with no entry survive untouched (outer-scope parameters while
/// checking a generic body).
pub fn subst_ty(ty: &Ty, env: &HashMap<String, Ty>) -> Ty {
    match ty {
        Ty::Array(elem) => Ty::Array(Box::new(subst_ty(elem, env))),
        Ty::Param(name) => env.get(name).cloned().unwrap_or(Ty::Param(name.clone())),
        _ => ty.clone(),
    }
}

/// Mangled instance name: `first$u64`, `get$Array_string`. `$` is not lexable
/// in VL source, so instances can never collide with user-written names.
pub fn mangle(name: &str, args: &[Ty]) -> String {
    let parts: Vec<String> = args.iter().map(mangle_ty).collect();
    format!("{name}${}", parts.join("_"))
}

fn mangle_ty(ty: &Ty) -> String {
    match ty {
        Ty::Int => "int".into(),
        Ty::U64 => "u64".into(),
        Ty::I64 => "i64".into(),
        Ty::F64 => "f64".into(),
        Ty::Bool => "bool".into(),
        Ty::U8 => "u8".into(),
        Ty::String => "string".into(),
        Ty::File => "File".into(),
        Ty::Array(elem) => format!("Array_{}", mangle_ty(elem)),
        Ty::Param(name) => name.clone(),
        Ty::Void => "void".into(),
        Ty::Error => "error".into(),
    }
}

/// Compiler-owned signature of one user function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuncSigTy {
    pub param_names: Vec<String>,
    pub param_tys: Vec<Ty>,
    pub ret: Ty,
    /// Declared type parameters (`[]` when monomorphic).
    pub type_params: Vec<String>,
}

impl FuncSigTy {
    /// Substitute concrete type arguments for the declared parameters.
    pub fn instantiate(&self, args: &[Ty]) -> (Vec<Ty>, Ty) {
        let env: HashMap<String, Ty> = self
            .type_params
            .iter()
            .cloned()
            .zip(args.iter().cloned())
            .collect();
        (
            self.param_tys.iter().map(|t| subst_ty(t, &env)).collect(),
            subst_ty(&self.ret, &env),
        )
    }
}

/// One monomorphized instance of a generic function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    /// `DefId.0` of the generic template.
    pub orig: u32,
    /// Concrete type arguments.
    pub args: Vec<Ty>,
    /// Specialized signature (no `Param` inside).
    pub sig: FuncSigTy,
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
    /// Monomorphic call site (`HirId.0`) -> mangled instance name.
    /// Only calls in non-generic code land here; calls inside generic
    /// templates resolve per-instance in [`TypedProgram::inst_calls`].
    pub root_calls: HashMap<u32, String>,
    /// `(outer instance, call site)` -> mangled callee. Discovered by the
    /// monomorphization worklist, which substitutes each outer instance's
    /// arguments before resolving inner calls.
    pub inst_calls: HashMap<(String, u32), String>,
    /// Mangled name -> concrete instance (signature + template link).
    pub instances: HashMap<String, Instance>,
}

impl TypedProgram {
    pub fn type_of_id(&self, id: vl_hir::HirId) -> Option<Ty> {
        self.types.get(&id.0).cloned()
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
        type_env: HashMap::new(),
        prog,
        pending_instances: Vec::new(),
    };
    // Pass 1: collect function signatures so calls resolve arity + types
    // regardless of definition order (matches the resolver pre-pass).
    for item in &prog.items {
        if let HirItem::Fn {
            def: Some(d),
            params,
            type_params,
            ret,
            ..
        } = item
        {
            let param_names = params
                .iter()
                .map(|(n, _, _, _)| n.clone())
                .collect::<Vec<_>>();
            // Signatures are stored generic (`Param` inside): each call site
            // substitutes its own type arguments.
            let env: HashMap<String, Ty> = type_params
                .iter()
                .map(|n| (n.clone(), Ty::Param(n.clone())))
                .collect();
            let param_tys = params
                .iter()
                .map(|(_, _, t, _)| {
                    t.as_ref()
                        .map(|v| Ty::from_vl_in(v, &env))
                        .unwrap_or(Ty::Error)
                })
                .collect::<Vec<_>>();
            let ret_ty = ret
                .as_ref()
                .map(|v| Ty::from_vl_in(v, &env))
                .unwrap_or(Ty::Error);
            cx.typed.func_defs.insert(d.0);
            cx.typed.func_arity.insert(d.0, params.len());
            cx.typed.func_sigs.insert(
                d.0,
                FuncSigTy {
                    param_names,
                    param_tys,
                    ret: ret_ty,
                    type_params: type_params.clone(),
                },
            );
        }
    }
    for item in &prog.items {
        cx.check_item(item);
    }
    cx.monomorphize();
    (cx.typed, cx.diags)
}

struct Checker<'a> {
    typed: TypedProgram,
    diags: Vec<Diagnostic>,
    bindings: HashMap<u32, Ty>,
    reported_unknown: HashSet<u32>,
    fn_ret: Ty,
    fn_name: String,
    fn_ret_span: Option<Span>,
    fn_span: Span,
    saw_value_return: bool,
    /// Current function's type parameters mapped to opaque `Param` types
    /// (empty while checking monomorphic code).
    type_env: HashMap<String, Ty>,
    prog: &'a HirProgram,
    /// Concrete `(template DefId.0, args)` pairs awaiting worklist expansion.
    pending_instances: Vec<(u32, Vec<Ty>)>,
}

impl<'a> Checker<'a> {
    fn record(&mut self, id: vl_hir::HirId, ty: Ty) -> Ty {
        self.typed.types.insert(id.0, ty.clone());
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
                self.record(*id, ty.clone());
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
                type_params,
                params,
                ret,
                ret_span,
                body,
                span,
            } => {
                if name == "main" && !type_params.is_empty() {
                    // The entrypoint is concrete by definition; LIR never
                    // emits uninstantiated templates, so a generic `main`
                    // would silently drop the program entrypoint.
                    self.diags.push(
                        Diagnostic::error("`main` must not declare type parameters")
                            .with_label(*span, "entrypoint declared here")
                            .with_code("E401"),
                    );
                }
                // Each type parameter checks opaquely as `Param(name)`; calls
                // substitute their own arguments per site.
                self.type_env = type_params
                    .iter()
                    .map(|n| (n.clone(), Ty::Param(n.clone())))
                    .collect();
                let ret_ty = ret
                    .as_ref()
                    .map(|v| Ty::from_vl_in(v, &self.type_env))
                    .unwrap_or(Ty::Error);
                self.record(*id, ret_ty.clone());
                // Bad annotations were already reported by the parser
                // (E104/E105); poison the scope quietly so no second error
                // cascades. (An omitted return parses as `void`, never `None`.)
                let poisoned_sig =
                    ret_ty == Ty::Error || params.iter().any(|(_, _, t, _)| t.is_none());
                for (_, def, ty, _) in params {
                    if let Some(def) = def {
                        let t = ty
                            .as_ref()
                            .map(|v| Ty::from_vl_in(v, &self.type_env))
                            .unwrap_or(Ty::Error);
                        self.bindings.insert(def.0, t);
                    }
                }
                // Explicit returns only: the body's tail value is discarded.
                // Track `return expr;` statements (including inside `if` /
                // `while`) to enforce the declared return type.
                self.fn_ret = ret_ty.clone();
                self.fn_name = name.clone();
                self.fn_ret_span = *ret_span;
                self.fn_span = *span;
                self.saw_value_return = false;
                for stmt in body {
                    self.check_stmt(stmt);
                }
                self.type_env.clear();
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
                self.record(*id, ty.clone());
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
                        let got = self.infer_expr_expected(e, &self.fn_ret.clone());
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
                        if !types_compatible(&got, &self.fn_ret) {
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
                let Some(def) = def else {
                    // Unresolved target already reported (E201); stay quiet.
                    self.record(*id, Ty::Error);
                    return;
                };
                let expected = self.bindings.get(&def.0).cloned();
                let got = if let Some(ty) = expected.as_ref() {
                    self.infer_expr_expected(value, ty)
                } else {
                    self.infer_expr(value)
                };
                if got == Ty::Error {
                    self.record(*id, Ty::Error);
                    return;
                }
                match self.bindings.get(&def.0).cloned() {
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
                        if !types_compatible(&got, &want) {
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
            HirStmt::IndexAssign {
                id,
                array,
                index,
                value,
                ..
            } => {
                let at = self.infer_expr(array);
                let it = self.infer_expr_expected(index, &Ty::U64);
                if at == Ty::Error || it == Ty::Error {
                    self.record(*id, Ty::Error);
                    return;
                }
                let Some(elem) = at.array_elem().cloned() else {
                    self.diags.push(
                        Diagnostic::error(format!("cannot index `{at}`"))
                            .with_label(array.span(), "only `Array[T]` supports indexing")
                            .with_code("E302"),
                    );
                    self.record(*id, Ty::Error);
                    return;
                };
                let vt = self.infer_expr_expected(value, &elem);
                if vt == Ty::Error {
                    self.record(*id, Ty::Error);
                    return;
                }
                if !types_compatible(&it, &Ty::U64) {
                    self.diags.push(
                        Diagnostic::error(format!("array index must be `u64`, got `{it}`"))
                            .with_label(index.span(), "expected `u64` here")
                            .with_code("E302"),
                    );
                    self.record(*id, Ty::Error);
                    return;
                }
                if !types_compatible(&vt, &elem) {
                    self.diags.push(
                        Diagnostic::error(format!(
                            "cannot store `{vt}` in `{at}` (elements are `{elem}`)"
                        ))
                        .with_label(value.span(), format!("expected `{elem}` here"))
                        .with_code("E302"),
                    );
                    self.record(*id, Ty::Error);
                    return;
                }
                self.record(*id, elem);
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
        ret_vl: &VlType,
        id: vl_hir::HirId,
    ) -> Ty {
        let ret = Ty::from_vl_in(ret_vl, &self.type_env);
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
        if params
            .iter()
            .any(|p| Ty::from_vl_in(&p.ty, &self.type_env) == Ty::Error)
            || ret == Ty::Error
        {
            return self.record(id, Ty::Error);
        }
        for (i, (arg, got)) in args.iter().zip(arg_tys.iter()).enumerate() {
            if *got == Ty::Error {
                continue;
            }
            let want = Ty::from_vl_in(&params[i].ty, &self.type_env);
            self.coerce_expr_literals(arg, &want);
            let got = self.infer_expr(arg);
            if got == Ty::Void || want == Ty::Void {
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
            if !types_compatible(&got, &want) {
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

    /// Convert one explicit type argument with unbound-name reporting.
    /// (Declaration positions are parser-validated; turbofish arguments in
    /// expression position parse permissively and land here.)
    fn vl_to_ty_reported(&mut self, v: &VlType, span: Span) -> Ty {
        let ty = Ty::from_vl_in(v, &self.type_env);
        if ty == Ty::Error {
            // from_vl_in only fails on unbound `Param` (void-in-Array is a
            // parser error; everything else converts).
            if let VlType::Param(name) = v {
                self.diags.push(
                    Diagnostic::error(format!("unknown type `{name}`"))
                        .with_label(span, "no type parameter with this name is in scope")
                        .with_note(
                            "declare it on the function (`function f[T]`) or use a concrete type",
                        )
                        .with_code("E105"),
                );
            } else if let VlType::Array(_) = v {
                // An unbound parameter nested inside `Array[...]`.
                self.diags.push(
                    Diagnostic::error(format!("unknown type `{v}`"))
                        .with_label(span, "no type parameter with this name is in scope")
                        .with_note(
                            "declare it on the function (`function f[T]`) or use a concrete type",
                        )
                        .with_code("E105"),
                );
            }
        }
        ty
    }

    /// Resolve a call's type arguments to `Ty`s. Explicit (`::[T]`) converts
    /// directly; omitted ones infer from the value arguments. Returns `None`
    /// (after reporting) when resolution fails.
    fn resolve_type_args(
        &mut self,
        name: &str,
        span: Span,
        sig: &FuncSigTy,
        type_args: &[VlType],
        arg_tys: &[Ty],
    ) -> Option<Vec<Ty>> {
        if sig.type_params.is_empty() {
            if !type_args.is_empty() {
                self.diags.push(
                    Diagnostic::error(format!(
                        "`{name}` is not generic but got {} type argument(s)",
                        type_args.len()
                    ))
                    .with_label(span, "remove the `::[...]`")
                    .with_code("E303"),
                );
                return None;
            }
            return Some(Vec::new());
        }
        if !type_args.is_empty() {
            if type_args.len() != sig.type_params.len() {
                self.diags.push(
                    Diagnostic::error(format!(
                        "`{name}` expects {} type argument(s), got {}",
                        sig.type_params.len(),
                        type_args.len()
                    ))
                    .with_label(span, "wrong number of type arguments")
                    .with_code("E303"),
                );
                return None;
            }
            let mut out = Vec::with_capacity(type_args.len());
            for v in type_args {
                let t = self.vl_to_ty_reported(v, span);
                if t == Ty::Error {
                    return None;
                }
                if t == Ty::Void {
                    self.diags.push(
                        Diagnostic::error("type argument cannot be `void`")
                            .with_label(span, "`void` is not a value type")
                            .with_code("E308"),
                    );
                    return None;
                }
                out.push(t);
            }
            return Some(out);
        }
        self.infer_type_args(name, span, sig, arg_tys)
    }

    /// Infer omitted type arguments by unifying formal parameter types
    /// against actual argument types.
    fn infer_type_args(
        &mut self,
        name: &str,
        span: Span,
        sig: &FuncSigTy,
        arg_tys: &[Ty],
    ) -> Option<Vec<Ty>> {
        let mut binds: HashMap<String, Ty> = HashMap::new();
        for (formal, actual) in sig.param_tys.iter().zip(arg_tys.iter()) {
            if *actual == Ty::Error {
                continue;
            }
            if !Self::unify(formal, actual, &mut binds, name, span, &mut self.diags) {
                return None;
            }
        }
        let mut out = Vec::with_capacity(sig.type_params.len());
        for p in &sig.type_params {
            match binds.remove(p) {
                Some(t) => out.push(t),
                None => {
                    self.diags.push(
                        Diagnostic::error(format!("cannot infer type argument `{p}` for `{name}`"))
                            .with_label(span, "pass it explicitly: `::[...]`")
                            .with_note(format!("write `{name}::[{p}](...)` with a concrete type"))
                            .with_code("E303"),
                    );
                    return None;
                }
            }
        }
        Some(out)
    }

    /// Unify one formal parameter type against an actual argument type,
    /// recording `Param` bindings. Reports exactly one diagnostic on
    /// conflict and returns false.
    fn unify(
        formal: &Ty,
        actual: &Ty,
        binds: &mut HashMap<String, Ty>,
        name: &str,
        span: Span,
        diags: &mut Vec<Diagnostic>,
    ) -> bool {
        match (formal, actual) {
            (Ty::Param(p), t) => {
                if let Some(bound) = binds.get(p) {
                    if !types_compatible(t, bound) {
                        diags.push(
                            Diagnostic::error(format!(
                                "`{name}` infers conflicting types for `{p}`: `{bound}` vs `{t}`"
                            ))
                            .with_label(span, "conflicting arguments here")
                            .with_code("E306"),
                        );
                        return false;
                    }
                    true
                } else {
                    binds.insert(p.clone(), t.clone());
                    true
                }
            }
            (Ty::Array(f), Ty::Array(a)) => Self::unify(f, a, binds, name, span, diags),
            (f, a) => {
                if f != a {
                    diags.push(
                        Diagnostic::error(format!("`{name}` expects `{f}`, got `{a}`"))
                            .with_label(span, format!("expected `{f}` here"))
                            .with_code("E306"),
                    );
                    false
                } else {
                    true
                }
            }
        }
    }

    /// `Array.new::[T](count)`: one `u64` argument, returns `Array[T]`.
    /// (Arity of the type argument itself is enforced by `vl-semantic`.)
    fn check_array_new(
        &mut self,
        name: &str,
        span: Span,
        type_args: &[VlType],
        args: &[HirExpr],
        arg_tys: &[Ty],
        id: vl_hir::HirId,
    ) -> Ty {
        let [elem_vl] = type_args else {
            // E303 already reported by vl-semantic; stay quiet.
            return self.record(id, Ty::Error);
        };
        let elem = self.vl_to_ty_reported(elem_vl, span);
        if elem == Ty::Error {
            return self.record(id, Ty::Error);
        }
        if elem == Ty::Void {
            self.diags.push(
                Diagnostic::error("type argument cannot be `void`")
                    .with_label(span, "`void` is not a value type")
                    .with_code("E308"),
            );
            return self.record(id, Ty::Error);
        }
        if args.len() != 1 {
            self.diags.push(
                Diagnostic::error(format!(
                    "`{name}` expects 1 argument(s), got {}",
                    args.len()
                ))
                .with_label(span, "wrong number of arguments")
                .with_code("E303"),
            );
            return self.record(id, Ty::Error);
        }
        if arg_tys[0] != Ty::Error && !types_compatible(&arg_tys[0], &Ty::U64) {
            self.diags.push(
                Diagnostic::error(format!(
                    "`{name}` parameter `count` expects `u64`, got `{}`",
                    arg_tys[0]
                ))
                .with_label(args[0].span(), "expected `u64` here")
                .with_code("E306"),
            );
            return self.record(id, Ty::Error);
        }
        self.record(id, Ty::Array(Box::new(elem)))
    }

    /// Drain [`Checker::pending_instances`](Self::pending_instances):
    /// for every concrete call discovered while checking, substitute the
    /// template's type arguments and resolve the calls inside its body, so
    /// generic-to-generic forwarding (`wrap[T]` calling `id::[T]`) lands on
    /// concrete instances too. LIR emits one function per entry of
    /// [`TypedProgram::instances`].
    fn monomorphize(&mut self) {
        let mut visited: HashSet<String> = HashSet::new();
        while let Some((def, args)) = self.pending_instances.pop() {
            let Some(sig) = self.typed.func_sigs.get(&def).cloned() else {
                continue;
            };
            let Some(name) = self.instance_name(def) else {
                continue;
            };
            let mangled = mangle(&name, &args);
            if !visited.insert(mangled.clone()) {
                continue;
            }
            let (param_tys, ret_ty) = sig.instantiate(&args);
            self.typed.instances.insert(
                mangled.clone(),
                Instance {
                    orig: def,
                    args: args.clone(),
                    sig: FuncSigTy {
                        param_names: sig.param_names.clone(),
                        param_tys,
                        ret: ret_ty,
                        type_params: Vec::new(),
                    },
                },
            );
            // Resolve the calls inside this instance's body under its
            // substitution environment.
            let env: HashMap<String, Ty> = sig
                .type_params
                .iter()
                .cloned()
                .zip(args.iter().cloned())
                .collect();
            let calls = calls_in_item(self.prog, &self.typed, def);
            for (call_id, callee_def, type_args, actual_tys) in calls {
                let Some(inner) = self.typed.func_sigs.get(&callee_def).cloned() else {
                    continue;
                };
                if inner.type_params.is_empty() {
                    continue;
                }
                // Substitute first: formals, explicit args, and the recorded
                // (generic) actual types all live in template space.
                let actuals: Vec<Ty> = actual_tys.iter().map(|t| subst_ty(t, &env)).collect();
                if actuals.contains(&Ty::Error) {
                    continue;
                }
                let resolved: Option<Vec<Ty>> = if type_args.is_empty() {
                    Self::infer_quiet(&inner, &actuals)
                } else {
                    let mut out = Vec::with_capacity(type_args.len());
                    let mut ok = true;
                    for v in &type_args {
                        // Explicit arguments name outer parameters (`T`
                        // means the caller's `T`): substitute, and anything
                        // still a `Param` afterwards is unbound (already
                        // reported while checking the template).
                        let t = Self::vl_in_instance(v, &env);
                        if !t.is_concrete() {
                            ok = false;
                            break;
                        }
                        out.push(t);
                    }
                    ok.then_some(out)
                };
                let Some(resolved) = resolved else {
                    continue;
                };
                if resolved.len() != inner.type_params.len()
                    || !resolved.iter().all(|t| t.is_concrete())
                {
                    continue;
                }
                let Some(inner_name) = self.instance_name(callee_def) else {
                    continue;
                };
                let inner_mangled = mangle(&inner_name, &resolved);
                self.typed
                    .inst_calls
                    .insert((mangled.clone(), call_id), inner_mangled.clone());
                if !self.typed.instances.contains_key(&inner_mangled)
                    && !self
                        .pending_instances
                        .iter()
                        .any(|(d, a)| *d == callee_def && *a == resolved)
                {
                    self.pending_instances.push((callee_def, resolved));
                }
            }
        }
    }

    /// Convert an explicit type argument under an instance environment:
    /// outer parameter names substitute, concrete types convert directly.
    fn vl_in_instance(v: &VlType, env: &HashMap<String, Ty>) -> Ty {
        match v {
            VlType::Param(name) => env.get(name).cloned().unwrap_or(Ty::Param(name.clone())),
            VlType::Array(elem) => Ty::Array(Box::new(Self::vl_in_instance(elem, env))),
            _ => Ty::from_vl(v),
        }
    }

    /// Quiet inference for worklist expansion (errors were already reported
    /// while checking the template generically).
    fn infer_quiet(sig: &FuncSigTy, actuals: &[Ty]) -> Option<Vec<Ty>> {
        let mut binds: HashMap<String, Ty> = HashMap::new();
        for (formal, actual) in sig.param_tys.iter().zip(actuals.iter()) {
            if !Self::unify_quiet(formal, actual, &mut binds) {
                return None;
            }
        }
        sig.type_params
            .iter()
            .map(|p| binds.remove(p))
            .collect::<Option<Vec<_>>>()
    }

    fn unify_quiet(formal: &Ty, actual: &Ty, binds: &mut HashMap<String, Ty>) -> bool {
        match (formal, actual) {
            (Ty::Param(p), t) => {
                if let Some(bound) = binds.get(p) {
                    bound == t
                } else {
                    binds.insert(p.clone(), t.clone());
                    true
                }
            }
            (Ty::Array(f), Ty::Array(a)) => Self::unify_quiet(f, a, binds),
            (f, a) => f == a,
        }
    }

    fn infer_expr_expected(&mut self, expr: &HirExpr, expected: &Ty) -> Ty {
        self.coerce_expr_literals(expr, expected);
        self.infer_expr(expr)
    }

    /// Give untyped integer literals the concrete type required by a use site.
    /// Updating the recorded types here also lets LIR select the right integer
    /// register class without adding conversion instructions.
    fn coerce_expr_literals(&mut self, expr: &HirExpr, expected: &Ty) {
        match expr {
            HirExpr::Literal { id, value, .. }
                if matches!(value, Scalar::Int(_)) && is_integer(expected) =>
            {
                self.record(*id, expected.clone());
            }
            HirExpr::ArrayLiteral { id, elems, .. } => {
                if let Ty::Array(elem) = expected {
                    for e in elems {
                        self.coerce_expr_literals(e, elem);
                    }
                    self.record(*id, expected.clone());
                }
            }
            HirExpr::Binary { lhs, rhs, .. } if is_integer(expected) => {
                self.coerce_expr_literals(lhs, expected);
                self.coerce_expr_literals(rhs, expected);
            }
            _ => {}
        }
    }

    /// Template function name for a `DefId.0` (worklist helper).
    fn instance_name(&self, def: u32) -> Option<String> {
        self.prog.items.iter().find_map(|item| match item {
            HirItem::Fn {
                def: Some(d), name, ..
            } if d.0 == def => Some(name.clone()),
            _ => None,
        })
    }

    fn infer_expr(&mut self, expr: &HirExpr) -> Ty {
        match expr {
            HirExpr::Literal { id, value, .. } => {
                let ty = if matches!(value, Scalar::Int(_)) {
                    self.typed
                        .type_of_id(*id)
                        .filter(is_integer)
                        .unwrap_or_else(|| scalar_ty(*value))
                } else {
                    scalar_ty(*value)
                };
                self.record(*id, ty)
            }
            HirExpr::String { id, .. } => self.record(*id, Ty::String),
            HirExpr::ArrayLiteral { id, elems, .. } => {
                if elems.is_empty() {
                    // No element to infer from: point at the typed
                    // constructor instead of guessing.
                    self.diags.push(
                        Diagnostic::error("cannot infer the element type of `[]`")
                            .with_label(
                                expr.span(),
                                "empty array literal needs `Array.new::[T](n)`",
                            )
                            .with_code("E302"),
                    );
                    return self.record(*id, Ty::Error);
                }
                let mut first = self.infer_expr(&elems[0]);
                let mut poisoned = first == Ty::Error;
                for elem in &elems[1..] {
                    let t = self.infer_expr(elem);
                    if t == Ty::Error {
                        poisoned = true;
                    } else if !poisoned && first == Ty::Int && is_integer(&t) {
                        self.coerce_expr_literals(&elems[0], &t);
                        first = t;
                    } else if !poisoned && t == Ty::Int && is_integer(&first) {
                        self.coerce_expr_literals(elem, &first);
                    } else if !poisoned && t != first {
                        self.diags.push(
                            Diagnostic::error(format!(
                                "array literal expects `{first}` elements, got `{t}`"
                            ))
                            .with_label(elem.span(), format!("expected `{first}` here"))
                            .with_code("E302"),
                        );
                        poisoned = true;
                    }
                }
                // An array literal with no concrete integer context still
                // needs a runtime element type. Use the target's default
                // unsigned lane, while contextual uses above can select i64
                // or u8 before this point.
                if !poisoned && first == Ty::Int {
                    for elem in elems {
                        self.coerce_expr_literals(elem, &Ty::U64);
                    }
                    first = Ty::U64;
                }
                // One bad element poisons the whole literal (single root
                // cause, no cascade).
                if poisoned {
                    return self.record(*id, Ty::Error);
                }
                self.record(*id, Ty::Array(Box::new(first)))
            }
            HirExpr::Index {
                id, base, index, ..
            } => {
                let bt = self.infer_expr(base);
                let it = self.infer_expr_expected(index, &Ty::U64);
                if bt == Ty::Error || it == Ty::Error {
                    return self.record(*id, Ty::Error);
                }
                let Some(elem) = bt.array_elem().cloned() else {
                    self.diags.push(
                        Diagnostic::error(format!("cannot index `{bt}`"))
                            .with_label(base.span(), "only `Array[T]` supports indexing")
                            .with_code("E302"),
                    );
                    return self.record(*id, Ty::Error);
                };
                if !types_compatible(&it, &Ty::U64) {
                    self.diags.push(
                        Diagnostic::error(format!("array index must be `u64`, got `{it}`"))
                            .with_label(index.span(), "expected `u64` here")
                            .with_code("E302"),
                    );
                    return self.record(*id, Ty::Error);
                }
                self.record(*id, elem)
            }
            HirExpr::Var { id, def, span, .. } => {
                // Unresolved names were already reported by `vl-semantic`;
                // poison quietly instead of cascading a second error.
                if def.is_none() {
                    self.record(*id, Ty::Error)
                } else {
                    let def_id = def.as_ref().map(|d| d.0).expect("checked above");
                    let ty = def
                        .as_ref()
                        .and_then(|def| self.bindings.get(&def.0).cloned())
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
                type_args,
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
                    // `Array.new::[T]` carries its element type in the call,
                    // not the catalog: resolve it here (reporting unbound
                    // names) instead of trusting the prebuilt signature.
                    if name == "Array.new" {
                        return self.check_array_new(name, *span, type_args, args, &arg_tys, *id);
                    }
                    return self.check_call_args(
                        name,
                        *span,
                        args,
                        &arg_tys,
                        &sig.params,
                        &sig.ret,
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
                // Resolve type arguments: explicit (`::[T]`) converts with
                // unbound-name reporting, omitted ones infer from the values.
                let Some(resolved) = self.resolve_type_args(name, *span, &sig, type_args, &arg_tys)
                else {
                    return self.record(*id, Ty::Error);
                };
                let (param_tys, ret_ty) = sig.instantiate(&resolved);
                if args.len() != param_tys.len() {
                    self.diags.push(
                        Diagnostic::error(format!(
                            "`{name}` expects {} argument(s), got {}",
                            param_tys.len(),
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
                for (i, original_got) in arg_tys.iter().enumerate() {
                    if *original_got == Ty::Error {
                        continue;
                    }
                    let want = &param_tys[i];
                    self.coerce_expr_literals(&args[i], want);
                    let got = self.infer_expr(&args[i]);
                    if got == Ty::Void || *want == Ty::Void {
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
                    if !types_compatible(&got, want) {
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
                // Concrete calls to generic functions monomorphize: record
                // the instance for LIR (deferred calls inside generic bodies
                // resolve per-instance in the worklist instead).
                if !sig.type_params.is_empty()
                    && resolved.iter().all(|t| t.is_concrete())
                    && self.type_env.is_empty()
                {
                    let mangled = mangle(name, &resolved);
                    self.typed.root_calls.insert(id.0, mangled.clone());
                    if !self.typed.instances.contains_key(&mangled) {
                        self.pending_instances.push((d.0, resolved.clone()));
                    }
                }
                self.record(*id, ret_ty)
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
                        if lt == Ty::Int && is_integer(&rt) {
                            self.coerce_expr_literals(lhs, &rt);
                        } else if rt == Ty::Int && is_integer(&lt) {
                            self.coerce_expr_literals(rhs, &lt);
                        }
                        let lt = self.infer_expr(lhs);
                        let rt = self.infer_expr(rhs);
                        if !types_compatible(&lt, &rt) || !is_numeric(&lt) {
                            self.diags.push(mismatch(*span, &lt, &rt));
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
                        if lt == Ty::Int && is_integer(&rt) {
                            self.coerce_expr_literals(lhs, &rt);
                        } else if rt == Ty::Int && is_integer(&lt) {
                            self.coerce_expr_literals(rhs, &lt);
                        }
                        let lt = self.infer_expr(lhs);
                        let rt = self.infer_expr(rhs);
                        if !types_compatible(&lt, &rt) || !is_comparable(&lt) {
                            self.diags.push(mismatch(*span, &lt, &rt));
                            return self.record(*id, Ty::Error);
                        }
                        self.record(*id, Ty::Bool)
                    }
                    HirBinOp::Lt | HirBinOp::Le | HirBinOp::Gt | HirBinOp::Ge => {
                        if lt == Ty::Int && is_integer(&rt) {
                            self.coerce_expr_literals(lhs, &rt);
                        } else if rt == Ty::Int && is_integer(&lt) {
                            self.coerce_expr_literals(rhs, &lt);
                        }
                        let lt = self.infer_expr(lhs);
                        let rt = self.infer_expr(rhs);
                        if !types_compatible(&lt, &rt) || !is_numeric(&lt) {
                            self.diags.push(mismatch(*span, &lt, &rt));
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

fn mismatch(span: Span, lt: &Ty, rt: &Ty) -> Diagnostic {
    Diagnostic::error(format!("type mismatch: {lt} vs {rt}"))
        .with_label(span, format!("expected {lt} on both sides"))
        .with_code("E302")
}

/// Every `Call` inside one template function: `(call HirId.0, callee
/// DefId.0, explicit type args, recorded generic actual types)`. Used by the
/// monomorphization worklist to resolve inner calls per outer instance.
fn calls_in_item(
    prog: &HirProgram,
    typed: &TypedProgram,
    def: u32,
) -> Vec<(u32, u32, Vec<VlType>, Vec<Ty>)> {
    fn expr_ty(typed: &TypedProgram, e: &HirExpr) -> Ty {
        typed.type_of_id(e.id()).unwrap_or(Ty::Error)
    }
    fn walk_expr(
        typed: &TypedProgram,
        e: &HirExpr,
        out: &mut Vec<(u32, u32, Vec<VlType>, Vec<Ty>)>,
    ) {
        match e {
            HirExpr::Call {
                id,
                def,
                type_args,
                args,
                ..
            } => {
                for arg in args {
                    walk_expr(typed, arg, out);
                }
                if let Some(d) = def {
                    let actuals = args.iter().map(|a| expr_ty(typed, a)).collect();
                    out.push((id.0, d.0, type_args.clone(), actuals));
                }
            }
            HirExpr::ArrayLiteral { elems, .. } => {
                for elem in elems {
                    walk_expr(typed, elem, out);
                }
            }
            HirExpr::Index { base, index, .. } => {
                walk_expr(typed, base, out);
                walk_expr(typed, index, out);
            }
            HirExpr::Binary { lhs, rhs, .. } => {
                walk_expr(typed, lhs, out);
                walk_expr(typed, rhs, out);
            }
            HirExpr::Unary { inner, .. } => walk_expr(typed, inner, out),
            HirExpr::Literal { .. } | HirExpr::String { .. } | HirExpr::Var { .. } => {}
        }
    }
    fn walk_stmt(
        typed: &TypedProgram,
        s: &HirStmt,
        out: &mut Vec<(u32, u32, Vec<VlType>, Vec<Ty>)>,
    ) {
        match s {
            HirStmt::Let { value, .. } | HirStmt::Assign { value, .. } => {
                walk_expr(typed, value, out)
            }
            HirStmt::Expr(e) => walk_expr(typed, e, out),
            HirStmt::Return { value, .. } => {
                if let Some(e) = value {
                    walk_expr(typed, e, out);
                }
            }
            HirStmt::IndexAssign {
                array,
                index,
                value,
                ..
            } => {
                walk_expr(typed, array, out);
                walk_expr(typed, index, out);
                walk_expr(typed, value, out);
            }
            HirStmt::If {
                condition,
                then_body,
                else_body,
                ..
            } => {
                walk_expr(typed, condition, out);
                for s in then_body {
                    walk_stmt(typed, s, out);
                }
                if let Some(body) = else_body {
                    for s in body {
                        walk_stmt(typed, s, out);
                    }
                }
            }
            HirStmt::While {
                condition, body, ..
            } => {
                walk_expr(typed, condition, out);
                for s in body {
                    walk_stmt(typed, s, out);
                }
            }
            HirStmt::Break { .. } | HirStmt::Continue { .. } => {}
        }
    }
    let mut out = Vec::new();
    for item in &prog.items {
        let HirItem::Fn {
            def: Some(d), body, ..
        } = item
        else {
            continue;
        };
        if d.0 != def {
            continue;
        }
        for s in body {
            walk_stmt(typed, s, &mut out);
        }
    }
    out
}

fn is_zero_literal(expr: &HirExpr) -> bool {
    matches!(expr, HirExpr::Literal { value, .. } if scalar_is_zero(*value))
}

fn scalar_is_zero(value: Scalar) -> bool {
    match value {
        Scalar::Int(v) => v == 0,
        Scalar::I64(v) => v == 0,
        Scalar::U64(v) => v == 0,
        Scalar::U8(v) => v == 0,
        Scalar::F64(v) => f64::from_bits(v) == 0.0,
        Scalar::Bool(_) => false,
    }
}

fn scalar_ty(value: Scalar) -> Ty {
    match value {
        Scalar::Int(_) => Ty::Int,
        Scalar::U64(_) => Ty::U64,
        Scalar::I64(_) => Ty::I64,
        Scalar::F64(_) => Ty::F64,
        Scalar::Bool(_) => Ty::Bool,
        Scalar::U8(_) => Ty::U8,
    }
}

fn is_integer(ty: &Ty) -> bool {
    matches!(ty, Ty::Int | Ty::U64 | Ty::I64 | Ty::U8)
}

fn types_compatible(got: &Ty, want: &Ty) -> bool {
    got == want
        || (*got == Ty::Int && is_integer(want))
        || matches!((got, want), (Ty::Array(g), Ty::Array(w)) if types_compatible(g, w))
}

fn is_numeric(ty: &Ty) -> bool {
    matches!(ty, Ty::Int | Ty::U64 | Ty::I64 | Ty::F64 | Ty::U8)
}

fn is_comparable(ty: &Ty) -> bool {
    matches!(
        ty,
        Ty::Int | Ty::U64 | Ty::I64 | Ty::F64 | Ty::Bool | Ty::U8 | Ty::String
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
    fn arrays_check_clean() {
        let (_, diags) = check_src(
            "function sum(a: Array[u64]): u64 { return a[0u64]; } function main() { let a = Array.new::[u64](3u64); a[0u64] = 1u64; let b = [1u64, 2u64]; sum(a); sum(b); }",
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn integer_literals_coerce_in_array_context() {
        let (_, diags) = check_src("function main() { let a = [1, 2u64]; a; }");
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn integer_literals_coerce_at_concrete_boundaries() {
        let (_, diags) = check_src(
            "function take(a: u64, b: i64, c: u8): u64 { return a; } function main(): u64 { take(1, 2, 3); return 4; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn empty_literal_needs_the_typed_constructor() {
        let (_, diags) = check_src("function main() { let e = []; e; }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("cannot infer the element type")),
            "{diags:?}"
        );
    }

    #[test]
    fn array_new_without_type_arg_is_quietly_poisoned() {
        // vl-semantic reports E303; typecheck must not cascade.
        let (toks, _) = vl_lex::lex("function main() { let a = Array.new(1u64); a; }");
        let (prog, _) = vl_syntax::parse(&toks, "");
        let (res, rdiags) = vl_semantic::resolve(&prog);
        assert!(rdiags.iter().any(|d| d.is_error()));
        let hir = vl_hir::lower(&prog, &res);
        let (_, tdiags) = check(&hir);
        assert!(tdiags.is_empty(), "{tdiags:?}");
    }

    #[test]
    fn array_new_arg_is_checked() {
        let (_, diags) = check_src("function main() { let a = Array.new::[u64](1.0f64); a; }");
        assert!(
            diags.iter().any(|d| d.message.contains("expects `u64`")),
            "{diags:?}"
        );
    }

    #[test]
    fn array_new_rejects_void_element() {
        let (_, diags) = check_src("function main() { let a = Array.new::[void](1u64); a; }");
        assert!(
            diags.iter().any(|d| d.message.contains("cannot be `void`")),
            "{diags:?}"
        );
    }

    #[test]
    fn string_arrays_check_clean() {
        let (_, diags) = check_src(
            "function main() { let a = Array.new::[string](2u64); let b = [\"x\", \"y\"]; b[0u64]; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn index_requires_array_and_u64() {
        let (_, diags) = check_src(r#"function main() { let s = "hi"; let x = s[0u64]; x; }"#);
        assert!(
            diags.iter().any(|d| d.message.contains("cannot index")),
            "{diags:?}"
        );
        let (_, diags) = check_src("function main() { let a = [1u64]; let x = a[true]; x; }");
        assert!(
            diags.iter().any(|d| d.message.contains("must be `u64`")),
            "{diags:?}"
        );
    }

    #[test]
    fn index_assign_checks_shapes() {
        let (_, diags) = check_src(r#"function main() { let a = [1u64]; a[0u64] = "s"; }"#);
        assert!(
            diags.iter().any(|d| d.message.contains("cannot store")),
            "{diags:?}"
        );
    }

    #[test]
    fn arrays_are_not_numeric() {
        let (_, diags) =
            check_src("function main() { let a = [1u64]; let b = [2u64]; let c = a + b; c; }");
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("E302")),
            "{diags:?}"
        );
    }

    #[test]
    fn comparisons_and_logic_yield_bool() {
        let (typed, diags) =
            check_src("function main() { let a = 1; let ok = a < 2 && a == 1 || !false; ok; }");
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.types.values().any(|t| *t == Ty::Bool));
    }

    #[test]
    fn untyped_integer_comparison_uses_concrete_context() {
        let (_, diags) = check_src("function main() { let x = 1 < 2u64; x; }");
        assert!(diags.is_empty(), "{diags:?}");
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

    #[test]
    fn generic_identity_infers_and_specializes() {
        let (typed, diags) = check_src(
            "function id[T](x: T): T { return x; } function main() { let a = id(1u64); let b = id::[string](\"s\"); a; b; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.instances.contains_key("id$u64"));
        assert!(typed.instances.contains_key("id$string"));
        assert_eq!(typed.root_calls.len(), 2);
        let inst = &typed.instances["id$u64"];
        assert_eq!(inst.sig.ret, Ty::U64);
    }

    #[test]
    fn generic_array_first_checks() {
        let (typed, diags) = check_src(
            "function first[T](a: Array[T]): T { return a[0u64]; } function main() { let x = first([1u64, 2u64]); x; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.instances.contains_key("first$u64"));
    }

    #[test]
    fn inference_failure_asks_for_turbofish() {
        let (_, diags) = check_src(
            "function never[T](): T { let a = Array.new::[T](1u64); return a[0u64]; } function main() { never(); }",
        );
        assert!(
            diags.iter().any(|d| d.message.contains("cannot infer")),
            "{diags:?}"
        );
    }

    #[test]
    fn conflicting_inference_is_one_error() {
        let (_, diags) = check_src(
            "function same[T](a: T, b: T): T { return a; } function main() { same(1u64, 2i64); }",
        );
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("conflicting types"), "{diags:?}");
    }

    #[test]
    fn explicit_arity_mismatch_is_one_error() {
        let (_, diags) = check_src(
            "function id[T](x: T): T { return x; } function main() { id::[u64, i64](1u64); }",
        );
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("type argument"), "{diags:?}");
    }

    #[test]
    fn turbofish_on_monomorphic_fn_is_an_error() {
        let (_, diags) = check_src(
            "function add(a: i64, b: i64): i64 { return a + b; } function main() { add::[u64](1, 2); }",
        );
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("not generic"), "{diags:?}");
    }

    #[test]
    fn unbound_type_argument_is_an_error() {
        let (_, diags) = check_src(
            "function id[T](x: T): T { return x; } function main() { id::[Bogus](1u64); }",
        );
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("unknown type"), "{diags:?}");
    }

    #[test]
    fn generic_to_generic_forwarding_monomorphizes() {
        let (typed, diags) = check_src(
            "function id[T](x: T): T { return x; } function wrap[T](x: T): T { return id::[T](x); } function main() { wrap(1u64); }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.instances.contains_key("wrap$u64"));
        assert!(typed.instances.contains_key("id$u64"));
        // The inner call inside `wrap` resolves per outer instance.
        assert_eq!(typed.inst_calls.len(), 1);
        let ((outer, _), inner) = typed.inst_calls.iter().next().unwrap();
        assert_eq!(outer, "wrap$u64");
        assert_eq!(inner, "id$u64");
    }

    #[test]
    fn generic_recursion_terminates() {
        let (typed, diags) = check_src(
            "function count[T](a: Array[T], n: u64): u64 { if (n == 0u64) { return 0u64; } return count(a, n - 1u64); } function main() { count([1u64], 2u64); }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.instances.contains_key("count$u64"));
    }

    #[test]
    fn wrong_value_arg_in_instance_is_an_error() {
        let (_, diags) = check_src(
            "function first[T](a: Array[T]): T { return a[0u64]; } function main() { first::[u64]([\"s\"]); }",
        );
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(
            diags[0].message.contains("expects `Array[u64]`"),
            "{diags:?}"
        );
    }

    #[test]
    fn uninstantiated_generic_emits_no_instance() {
        let (typed, diags) =
            check_src("function dead[T](x: T): T { return x; } function main() { 1u64; }");
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.instances.is_empty());
    }
}
