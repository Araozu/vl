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
use vl_common::{Diagnostic, GenericBound, Span, VlType};
use vl_hir::{HirBinOp, HirExpr, HirItem, HirProgram, HirStmt, HirUnOp};

mod mono;

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

    /// Fully concrete (no `Param` inside, no unresolved `Int`)? Only
    /// concrete types reach LIR. `Int` is an untyped literal awaiting context
    /// and must be defaulted (`u64`) before lowering; `Param` is an opaque
    /// generic use resolved per instance; `Error` (including nested
    /// `Array[Error]`) is poison already reported upstream.
    pub fn is_concrete(&self) -> bool {
        match self {
            Ty::Array(elem) => elem.is_concrete(),
            Ty::Param(_) | Ty::Error | Ty::Int => false,
            _ => true,
        }
    }

    /// Normalized for emission: concrete and containing no `Error` at any
    /// depth. Poisoned types (`Error` / `Array[Error]`) are never emitted
    /// (LIR skips them); every other recorded type in monomorphic code and
    /// every instance signature must satisfy this.
    pub fn is_normalized(&self) -> bool {
        self.is_concrete()
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
    /// Bound per type parameter (`T -> Numeric`); absent means unconstrained
    /// (fully opaque, no operators).
    pub bounds: HashMap<String, GenericBound>,
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

    /// Validate the normalized-type invariant at the typecheck-to-LIR
    /// boundary: every type in monomorphic code, every instance signature,
    /// and every instance body type after argument substitution must be
    /// normalized (no `Int`, no `Param`, no nested `Error`). Generic template
    /// bodies legitimately contain `Param` and are checked per instance
    /// instead of skipped. When `prior` already holds errors, lowering is
    /// blocked anyway, so validation stands down entirely (error subtrees
    /// may still hold undefaulted `int`s, and exact-error-count tests must
    /// not see a second diagnostic). Otherwise poison at the boundary is
    /// itself an E500. Returns E500 (compiler bug) diagnostics so invalid
    /// types cannot silently disappear in LIR.
    pub fn validate_normalized(&self, prog: &HirProgram, prior: &[Diagnostic]) -> Vec<Diagnostic> {
        validate_normalized(self, prog, prior)
    }
}

/// See [`TypedProgram::validate_normalized`].
pub fn validate_normalized(
    typed: &TypedProgram,
    prog: &HirProgram,
    prior: &[Diagnostic],
) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    // Earlier errors already block lowering: boundary validation is moot.
    if prior.iter().any(|d| d.is_error()) {
        return diags;
    }
    let check_ty = |what: String, ty: &Ty, diags: &mut Vec<Diagnostic>| {
        if ty_has_error(ty) {
            diags.push(
                Diagnostic::error(format!(
                    "{what} is poisoned without a prior error (compiler bug)"
                ))
                .with_code("E500"),
            );
            return;
        }
        if !ty.is_normalized() {
            diags.push(
                Diagnostic::error(format!(
                    "{what} has non-normalized type `{ty}` (compiler bug)"
                ))
                .with_note(
                    "unresolved `int`/`Param` must be defaulted or monomorphized before lowering",
                )
                .with_code("E500"),
            );
        }
    };
    let generic_ids = generic_template_ids(prog);
    for (id, ty) in &typed.types {
        if generic_ids.contains(id) {
            continue;
        }
        check_ty(format!("node `{id}`"), ty, &mut diags);
    }
    for (name, inst) in &typed.instances {
        for ty in inst
            .sig
            .param_tys
            .iter()
            .chain(std::iter::once(&inst.sig.ret))
        {
            check_ty(format!("instance `{name}` signature"), ty, &mut diags);
        }
        for arg in &inst.args {
            check_ty(format!("instance `{name}` argument"), arg, &mut diags);
        }
        // Instance bodies: substitute this instance's arguments into the
        // template's recorded types, then validate the result. A `Param`
        // surviving substitution is unbound; an `Int` was never defaulted.
        let env: HashMap<String, Ty> = typed
            .func_sigs
            .get(&inst.orig)
            .map(|sig| {
                sig.type_params
                    .iter()
                    .cloned()
                    .zip(inst.args.iter().cloned())
                    .collect()
            })
            .unwrap_or_default();
        for id in template_ids_for(prog, inst.orig) {
            if let Some(ty) = typed.types.get(&id) {
                let substed = subst_ty(ty, &env);
                check_ty(
                    format!("instance `{name}` node `{id}`"),
                    &substed,
                    &mut diags,
                );
            }
        }
    }
    diags
}

fn template_expr_ids(e: &HirExpr, out: &mut HashSet<u32>) {
    out.insert(e.id().0);
    match e {
        HirExpr::ArrayLiteral { elems, .. } => {
            for el in elems {
                template_expr_ids(el, out);
            }
        }
        HirExpr::Index { base, index, .. } => {
            template_expr_ids(base, out);
            template_expr_ids(index, out);
        }
        HirExpr::Call { args, .. } => {
            for a in args {
                template_expr_ids(a, out);
            }
        }
        HirExpr::Binary { lhs, rhs, .. } => {
            template_expr_ids(lhs, out);
            template_expr_ids(rhs, out);
        }
        HirExpr::Unary { inner, .. } => template_expr_ids(inner, out),
        HirExpr::Cast { inner, .. } => template_expr_ids(inner, out),
        HirExpr::Literal { .. } | HirExpr::String { .. } | HirExpr::Var { .. } => {}
    }
}

fn template_stmt_ids(s: &HirStmt, out: &mut HashSet<u32>) {
    match s {
        HirStmt::Let { id, value, .. } | HirStmt::Assign { id, value, .. } => {
            out.insert(id.0);
            template_expr_ids(value, out);
        }
        HirStmt::IndexAssign {
            id,
            array,
            index,
            value,
            ..
        } => {
            out.insert(id.0);
            template_expr_ids(array, out);
            template_expr_ids(index, out);
            template_expr_ids(value, out);
        }
        HirStmt::Expr(e) => template_expr_ids(e, out),
        HirStmt::Return { value, .. } => {
            if let Some(e) = value {
                template_expr_ids(e, out);
            }
        }
        HirStmt::If {
            condition,
            then_body,
            else_body,
            ..
        } => {
            template_expr_ids(condition, out);
            for st in then_body {
                template_stmt_ids(st, out);
            }
            if let Some(body) = else_body {
                for st in body {
                    template_stmt_ids(st, out);
                }
            }
        }
        HirStmt::While {
            condition, body, ..
        } => {
            template_expr_ids(condition, out);
            for st in body {
                template_stmt_ids(st, out);
            }
        }
        HirStmt::Break { .. } | HirStmt::Continue { .. } => {}
    }
}

/// All `HirId.0` values of one function template (the item id plus every
/// node in its body), used to validate instance bodies after substitution.
fn template_ids_for(prog: &HirProgram, def: u32) -> HashSet<u32> {
    let mut out = HashSet::new();
    for item in &prog.items {
        if let HirItem::Fn {
            id,
            def: Some(d),
            body,
            ..
        } = item
        {
            if d.0 != def {
                continue;
            }
            out.insert(id.0);
            for st in body {
                template_stmt_ids(st, &mut out);
            }
        }
    }
    out
}

/// All `HirId.0` values inside generic function templates (bodies, params,
/// and the item id itself). Their recorded types may contain `Param`, so the
/// monomorphic validation loop skips them (instances are validated after
/// substitution instead).
fn generic_template_ids(prog: &HirProgram) -> HashSet<u32> {
    let mut out = HashSet::new();
    for item in &prog.items {
        if let HirItem::Fn {
            id,
            type_params,
            body,
            ..
        } = item
        {
            if type_params.is_empty() {
                continue;
            }
            out.insert(id.0);
            for st in body {
                template_stmt_ids(st, &mut out);
            }
        }
    }
    out
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
        type_env: HashMap::new(),
        type_bounds: HashMap::new(),
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
                .map(|p| (p.name.clone(), Ty::Param(p.name.clone())))
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
                    type_params: type_params.iter().map(|p| p.name.clone()).collect(),
                    bounds: type_params
                        .iter()
                        .filter_map(|p| p.bound.map(|b| (p.name.clone(), b)))
                        .collect(),
                },
            );
        }
    }
    for item in &prog.items {
        cx.check_item(item);
    }
    // Separate monomorphization pass: instance expansion owns caching,
    // poison suppression, expanding-recursion diagnostics, and budgets.
    let pending = std::mem::take(&mut cx.pending_instances);
    let mut typed = std::mem::take(&mut cx.typed);
    let mut diags = std::mem::take(&mut cx.diags);
    drop(cx);
    mono::expand(prog, &mut typed, pending, &mut diags);
    (typed, diags)
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
    /// Current function's type parameters mapped to opaque `Param` types
    /// (empty while checking monomorphic code).
    type_env: HashMap<String, Ty>,
    /// Bounds of the current function's type parameters (`T -> Numeric`).
    type_bounds: HashMap<String, GenericBound>,
    /// Concrete `(template DefId.0, args)` pairs awaiting the separate
    /// monomorphization pass ([`mono::expand`]).
    pending_instances: Vec<(u32, Vec<Ty>)>,
}

impl Checker {
    fn record(&mut self, id: vl_hir::HirId, ty: Ty) -> Ty {
        self.typed.types.insert(id.0, ty.clone());
        ty
    }

    fn check_item(&mut self, item: &HirItem) {
        match item {
            HirItem::Let {
                id,
                def,
                ty,
                ty_span,
                value,
                ..
            } => {
                let ty = self.let_type(ty, ty_span, value);
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
                // substitute their own arguments per site. Bounds (`extends`)
                // unlock operators on otherwise-opaque `T`.
                self.type_env = type_params
                    .iter()
                    .map(|p| (p.name.clone(), Ty::Param(p.name.clone())))
                    .collect();
                self.type_bounds = type_params
                    .iter()
                    .filter_map(|p| p.bound.map(|b| (p.name.clone(), b)))
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
                    ty_has_error(&ret_ty) || params.iter().any(|(_, _, t, _)| t.is_none());
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
                // Every reachable path must return (definite-return analysis
                // below); a bare tail value never satisfies the return type.
                self.fn_ret = ret_ty.clone();
                self.fn_name = name.clone();
                self.fn_ret_span = *ret_span;
                self.fn_span = *span;
                for stmt in body {
                    self.check_stmt(stmt);
                }
                self.type_env.clear();
                self.type_bounds.clear();
                if poisoned_sig {
                    return;
                }
                if ret_ty == Ty::Void {
                    // `return;` is optional; bare expression values are
                    // discarded. Any body is accepted, but still warn on
                    // unreachable code.
                    check_unreachable(body, &mut self.diags);
                    return;
                }
                check_unreachable(body, &mut self.diags);
                if block_flow(body) != Flow::Returns {
                    // Any `return` (even a mistyped or bare one, already
                    // reported) counts as diverging, so this fires only when
                    // some reachable path falls through — one root cause.
                    let anchor = ret_span.unwrap_or(*span);
                    let mut diag = Diagnostic::error(format!(
                        "function `{name}` declares return `{ret_ty}` but not all paths return a value"
                    ))
                    .with_label(anchor, "declared here");
                    if let Some(fallthrough) = fallthrough_span(body) {
                        diag = diag
                            .with_label(fallthrough, "this path can fall through without `return`");
                    }
                    self.diags.push(
                        diag.with_note("add `return <expr>;` of the return type (`return;` is only for `void`)")
                            .with_code("E307"),
                    );
                }
                let _ = def;
            }
        }
    }

    /// Check a `let` initializer against its optional annotation. Returns
    /// the binding type (`Error` when poisoned). An `Array[T]` annotation on
    /// a bare `Array.new(n)` supplies `T` contextually; every other shape
    /// infers first (integer literals coerced by the annotation) and then
    /// must be compatible with it.
    fn let_type(&mut self, ty: &Option<VlType>, ty_span: &Option<Span>, value: &HirExpr) -> Ty {
        // Failed annotation (parser-reported): infer inner errors only.
        if ty.is_none() && ty_span.is_some() {
            let _ = self.infer_expr(value);
            return Ty::Error;
        }
        let ann = ty.as_ref().map(|v| Ty::from_vl_in(v, &self.type_env));
        if let (
            Some(Ty::Array(elem)),
            HirExpr::Call {
                name,
                type_args,
                args,
                span,
                id: call_id,
                ..
            },
        ) = (ann.as_ref(), value)
        {
            if name == "Array.new" && type_args.is_empty() {
                let mut arg_tys = Vec::with_capacity(args.len());
                let mut poisoned = false;
                for arg in args {
                    let t = self.infer_expr(arg);
                    if ty_has_error(&t) {
                        poisoned = true;
                    }
                    arg_tys.push(t);
                }
                if poisoned {
                    return Ty::Error;
                }
                return self.check_array_new_elem(
                    name,
                    *span,
                    (**elem).clone(),
                    args,
                    &arg_tys,
                    *call_id,
                );
            }
        }
        let inferred = match &ann {
            Some(a) => {
                if ty_has_error(a) {
                    let _ = self.infer_expr(value);
                    return Ty::Error;
                }
                self.infer_expr_expected(value, a)
            }
            None => self.infer_expr(value),
        };
        if ty_has_error(&inferred) {
            return Ty::Error;
        }
        if inferred == Ty::Void {
            self.diags.push(
                Diagnostic::error("cannot bind a `void` value")
                    .with_label(value.span(), "`void` is not a value")
                    .with_note("`void` calls may only appear as bare statements")
                    .with_code("E308"),
            );
            return Ty::Error;
        }
        if let Some(a) = &ann {
            if !types_compatible(&inferred, a) {
                self.diags.push(
                    Diagnostic::error(format!(
                        "cannot initialize `{a}` binding with `{inferred}` value"
                    ))
                    .with_label(value.span(), format!("expected `{a}` here"))
                    .with_code("E309"),
                );
                return Ty::Error;
            }
            return inferred;
        }
        // Unannotated bindings holding an unresolved `int` (top level or
        // nested, e.g. `Array[Int]`) resolve through the target's default
        // unsigned lane (`u64`): an `int` variable must not stay compatible
        // with every integer type, or `let v = 300; take_u8(v);` would pass.
        if ty_contains_int(&inferred) {
            let defaulted = default_inferred_ty(inferred.clone());
            self.coerce_expr_literals(value, &defaulted);
            let resolved = self.infer_expr(value);
            if ty_has_error(&resolved) {
                return Ty::Error;
            }
            if ty_contains_int(&resolved) {
                // Non-coercible shape (unreachable for literals, which the
                // arms above handle): record the default so no `Int` lingers
                // for the LIR boundary.
                return self.record(value.id(), defaulted);
            }
            return resolved;
        }
        inferred
    }

    /// Check a statement. There are no implicit returns: `let` initializers
    /// and bare expression values are discarded and never satisfy a declared
    /// return type — only an explicit `return expr;` does.
    fn check_stmt(&mut self, stmt: &HirStmt) {
        match stmt {
            HirStmt::Let {
                id,
                def,
                ty,
                ty_span,
                value,
                ..
            } => {
                let ty = self.let_type(ty, ty_span, value);
                self.record(*id, ty.clone());
                if let Some(def) = def {
                    self.bindings.insert(def.0, ty);
                }
            }
            HirStmt::Expr(e) => {
                // Value discarded; still infer for inner errors. A result
                // holding `int` (top level or nested, e.g. `[1];`) defaults
                // through the `u64` lane so no unresolved `Int` reaches LIR.
                let t = self.infer_expr(e);
                if !ty_has_error(&t) && ty_contains_int(&t) {
                    let defaulted = default_inferred_ty(t);
                    self.coerce_expr_literals(e, &defaulted);
                    let t2 = self.infer_expr(e);
                    if ty_contains_int(&t2) && !ty_has_error(&t2) {
                        self.record(e.id(), defaulted);
                    }
                }
            }
            HirStmt::Return { value, span } => {
                match value {
                    None => {
                        // Bare `return;`: only valid for `void` (or poisoned).
                        // Any `return` diverges, so the missing-return check
                        // (definite-return analysis) stays quiet — one error.
                        if ty_has_error(&self.fn_ret) || self.fn_ret == Ty::Void {
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
                        // Poisoned values already reported; the statement still
                        // diverges, so the missing-`return` check stays quiet.
                        if ty_has_error(&got) || ty_has_error(&self.fn_ret) {
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
                        }
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
                if let Some(ty) = expected.as_ref() {
                    if ty_has_error(ty) {
                        let _ = self.infer_expr(value);
                        self.record(*id, Ty::Error);
                        return;
                    }
                }
                let got = if let Some(ty) = expected.as_ref() {
                    self.infer_expr_expected(value, ty)
                } else {
                    self.infer_expr(value)
                };
                if ty_has_error(&got) {
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
                    Some(want) if ty_has_error(&want) => {
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
                if ty_has_error(&at) || ty_has_error(&it) {
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
                if ty_has_error(&vt) || ty_has_error(&elem) {
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
                if !ty_has_error(&condition_ty) && condition_ty != Ty::Bool {
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
                if !ty_has_error(&condition_ty) && condition_ty != Ty::Bool {
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
            .any(|p| ty_has_error(&Ty::from_vl_in(&p.ty, &self.type_env)))
            || ty_has_error(&ret)
        {
            return self.record(id, Ty::Error);
        }
        for (i, (arg, got)) in args.iter().zip(arg_tys.iter()).enumerate() {
            if ty_has_error(got) {
                continue;
            }
            let want = Ty::from_vl_in(&params[i].ty, &self.type_env);
            if ty_has_error(&want) {
                continue;
            }
            self.coerce_expr_literals(arg, &want);
            let got = self.infer_expr(arg);
            if ty_has_error(&got) {
                continue;
            }
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
        if ty_has_error(&ty) {
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
                if ty_has_error(&t) {
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
            if !self.check_bounds(name, span, sig, &out) {
                return None;
            }
            return Some(out);
        }
        self.infer_type_args(name, span, sig, arg_tys)
    }

    /// Check inferred or explicit type arguments against the callee's bounds.
    /// Concrete types must satisfy the bound directly; an outer `Param`
    /// satisfies by implication (`Numeric` implies `Comparable`). Reports one
    /// E303 per violation.
    fn check_bounds(&mut self, name: &str, span: Span, sig: &FuncSigTy, args: &[Ty]) -> bool {
        for (param, arg) in sig.type_params.iter().zip(args.iter()) {
            let Some(bound) = sig.bounds.get(param) else {
                continue;
            };
            if ty_satisfies_bound(arg, *bound, &self.type_bounds) {
                continue;
            }
            // Friendlier message when an unconstrained outer `T` flows into a
            // bounded callee inside a generic body.
            if matches!(arg, Ty::Param(_)) {
                self.diags.push(
                    Diagnostic::error(format!(
                        "`{name}` requires `{param}` to satisfy `{bound}`, but it is passed as `{arg}`"
                    ))
                    .with_label(span, format!("`{arg}` does not satisfy `{bound}` here"))
                    .with_note(format!(
                        "add `extends {bound}` to `{arg}` or pass a concrete `{bound}` type"
                    ))
                    .with_code("E303"),
                );
            } else {
                self.diags.push(
                    Diagnostic::error(format!(
                        "`{name}` type argument `{arg}` does not satisfy `{bound}`"
                    ))
                    .with_label(span, format!("expected a `{bound}` type here"))
                    .with_code("E303"),
                );
            }
            return false;
        }
        true
    }

    /// Infer omitted type arguments constraint-based: collect every
    /// `Param` constraint first, then solve. Collecting before solving makes
    /// inference independent of argument order (`same(1u64, 2)` and
    /// `same(1, 2u64)` both infer `T = u64`) and prepares for nested
    /// constraints (`Array[Array[T]]`) and contextual result inference
    /// (an expected return type contributes one more constraint).
    fn infer_type_args(
        &mut self,
        name: &str,
        span: Span,
        sig: &FuncSigTy,
        arg_tys: &[Ty],
    ) -> Option<Vec<Ty>> {
        let mut set = ConstraintSet::default();
        for (formal, actual) in sig.param_tys.iter().zip(arg_tys.iter()) {
            if ty_has_error(actual) || ty_has_error(formal) {
                continue;
            }
            if !set.collect(formal, actual, name, span, &mut self.diags) {
                return None;
            }
        }
        let out = set.solve(name, span, sig, &mut self.diags)?;
        if !self.check_bounds(name, span, sig, &out) {
            return None;
        }
        Some(out)
    }

    /// `Array.new::[T](count)`: one `u64` argument, returns `Array[T]`.
    /// A bare `Array.new(count)` only typechecks under an annotated `let`
    /// (handled in [`Checker::let_type`](Self::let_type)); everywhere else
    /// it is E303 here. (Arity above one is enforced by `vl-semantic`.)
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
            self.diags.push(
                Diagnostic::error("`Array.new` needs an element type")
                    .with_label(
                        span,
                        "write `Array.new::[T](count)`, or annotate the `let`: `let a: Array[T] = Array.new(count)`",
                    )
                    .with_code("E303"),
            );
            return self.record(id, Ty::Error);
        };
        let elem = self.vl_to_ty_reported(elem_vl, span);
        if ty_has_error(&elem) {
            return self.record(id, Ty::Error);
        }
        self.check_array_new_elem(name, span, elem, args, arg_tys, id)
    }

    /// Core constructor check once the element type is known (explicitly or
    /// from a `let` annotation). The count coerces integer literals to `u64`.
    fn check_array_new_elem(
        &mut self,
        name: &str,
        span: Span,
        elem: Ty,
        args: &[HirExpr],
        arg_tys: &[Ty],
        id: vl_hir::HirId,
    ) -> Ty {
        // Poisoned arguments stay quiet (root cause already reported).
        if arg_tys.iter().any(ty_has_error) || ty_has_error(&elem) {
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
        if arg_tys.iter().any(ty_has_error) {
            return self.record(id, Ty::Error);
        }
        self.coerce_expr_literals(&args[0], &Ty::U64);
        let ct = self.typed.type_of_id(args[0].id()).unwrap_or(Ty::Error);
        if ty_has_error(&ct) {
            return self.record(id, Ty::Error);
        }
        if !types_compatible(&ct, &Ty::U64) {
            self.diags.push(
                Diagnostic::error(format!(
                    "`{name}` parameter `count` expects `u64`, got `{ct}`"
                ))
                .with_label(args[0].span(), "expected `u64` here")
                .with_code("E306"),
            );
            return self.record(id, Ty::Error);
        }
        self.record(id, Ty::Array(Box::new(elem)))
    }

    fn infer_expr_expected(&mut self, expr: &HirExpr, expected: &Ty) -> Ty {
        self.coerce_expr_literals(expr, expected);
        self.infer_expr(expr)
    }

    /// Give untyped integer literals the concrete type required by a use site.
    /// This is the central coercion entry: literal defaulting + range checks
    /// live here (see [`coerce_int_literal`]). Updating the recorded types
    /// here also lets LIR select the right integer register class without
    /// adding conversion instructions.
    fn coerce_expr_literals(&mut self, expr: &HirExpr, expected: &Ty) {
        match expr {
            HirExpr::Literal { id, value, span }
                if matches!(value, Scalar::Int(_)) && is_integer(expected) =>
            {
                if let Scalar::Int(v) = value {
                    if coerce_int_literal(*v, expected, *span, &mut self.diags).is_none() {
                        self.record(*id, Ty::Error);
                        return;
                    }
                }
                // Don't overwrite a previous range error with a good type.
                if !matches!(self.typed.type_of_id(*id), Some(Ty::Error)) {
                    self.record(*id, expected.clone());
                }
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

    /// Numeric in the current generic scope: concrete numerics plus `Param`
    /// with a `Numeric` bound. Unconstrained `T` stays opaque (no operators).
    fn is_numeric_in_scope(&self, ty: &Ty) -> bool {
        match ty {
            Ty::Param(name) => self.type_bounds.get(name) == Some(&GenericBound::Numeric),
            _ => is_numeric(ty),
        }
    }

    /// Comparable in scope: concrete comparables plus `Param` with `Numeric`
    /// (numbers compare) or `Comparable` bounds.
    fn is_comparable_in_scope(&self, ty: &Ty) -> bool {
        match ty {
            Ty::Param(name) => matches!(
                self.type_bounds.get(name),
                Some(GenericBound::Numeric) | Some(GenericBound::Comparable)
            ),
            _ => is_comparable(ty),
        }
    }

    fn infer_expr(&mut self, expr: &HirExpr) -> Ty {
        match expr {
            HirExpr::Literal { id, value, .. } => {
                // A range error recorded by coercion stays poisoned.
                if matches!(self.typed.type_of_id(*id), Some(Ty::Error)) {
                    return self.record(*id, Ty::Error);
                }
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
                    // Contextual empty: coercion already recorded the
                    // annotation (`let e: Array[u64] = [];`); honor it.
                    // A repeat visit after an error stays quiet.
                    match self.typed.type_of_id(*id) {
                        Some(t) if ty_has_error(&t) => return self.record(*id, Ty::Error),
                        Some(t @ Ty::Array(_)) => return self.record(*id, t),
                        _ => {}
                    }
                    // No element to infer from: point at the typed
                    // constructor instead of guessing.
                    self.diags.push(
                        Diagnostic::error("cannot infer the element type of `[]`")
                            .with_label(
                                expr.span(),
                                "empty array literal needs a type: `let e: Array[T] = [];` or `Array.new::[T](n)`",
                            )
                            .with_code("E302"),
                    );
                    return self.record(*id, Ty::Error);
                }
                let mut first = self.infer_expr(&elems[0]);
                let mut poisoned = ty_has_error(&first);
                for elem in &elems[1..] {
                    let t = self.infer_expr(elem);
                    if ty_has_error(&t) {
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
                // An all-`int` literal keeps `Array[Int]` here on purpose:
                // defaulting eagerly to `u64` would erase the literal's
                // deference before generic constraint solving sees it
                // (`same([1], [2u8])` must solve `T = Array[u8]`, not
                // conflict `u64` vs `u8`). The `Int` is defaulted at use
                // sites instead: `let`/discarded-statement defaulting,
                // `infer_expr_expected` coercion, and per-argument coercion
                // after generic solving all coerce the elements then.
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
                if ty_has_error(&bt) || ty_has_error(&it) {
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
                    if ty_has_error(&t) {
                        poisoned = true;
                    }
                    arg_tys.push(t);
                }
                let Some(d) = def else {
                    // Unresolved callee already reported; stay quiet.
                    return self.record(*id, Ty::Error);
                };
                if *external {
                    // `Array.new` carries its element type in the call (or
                    // the surrounding `let`), not the catalog: resolve it
                    // here, before the signature check (a bare `Array.new`
                    // has no prebuilt signature by design).
                    if name == "Array.new" {
                        return self.check_array_new(name, *span, type_args, args, &arg_tys, *id);
                    }
                    // Externs are never generic: `print::[u64]` is an error.
                    if !type_args.is_empty() {
                        self.diags.push(
                            Diagnostic::error(format!(
                                "`{name}` is not generic but got {} type argument(s)",
                                type_args.len()
                            ))
                            .with_label(*span, "remove the `::[...]`")
                            .with_code("E303"),
                        );
                        return self.record(*id, Ty::Error);
                    }
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
                if ty_has_error(&sig.ret) || sig.param_tys.iter().any(ty_has_error) {
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
                    if ty_has_error(original_got) {
                        continue;
                    }
                    let want = &param_tys[i];
                    if ty_has_error(want) {
                        continue;
                    }
                    self.coerce_expr_literals(&args[i], want);
                    let got = self.infer_expr(&args[i]);
                    if ty_has_error(&got) {
                        continue;
                    }
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
                if ty_has_error(&inner_ty) {
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
                if ty_has_error(&lt) || ty_has_error(&rt) {
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
                        // Central coercion: an `Int` literal defers to a
                        // concrete integer lane; two bare `Int`s default to
                        // `u64` so no unresolved `Int` reaches LIR.
                        if lt == Ty::Int && rt == Ty::Int {
                            self.coerce_expr_literals(lhs, &Ty::U64);
                            self.coerce_expr_literals(rhs, &Ty::U64);
                        } else if lt == Ty::Int && is_integer(&rt) {
                            self.coerce_expr_literals(lhs, &rt);
                        } else if rt == Ty::Int && is_integer(&lt) {
                            self.coerce_expr_literals(rhs, &lt);
                        }
                        let lt = self.infer_expr(lhs);
                        let rt = self.infer_expr(rhs);
                        if ty_has_error(&lt) || ty_has_error(&rt) {
                            return self.record(*id, Ty::Error);
                        }
                        if !types_compatible(&lt, &rt) || !self.is_numeric_in_scope(&lt) {
                            // Opaque `T` without a `Numeric` bound cannot use
                            // arithmetic; point at the bound instead of a bare
                            // mismatch.
                            if matches!((&lt, &rt), (Ty::Param(_), _) | (_, Ty::Param(_)))
                                && types_compatible(&lt, &rt)
                            {
                                self.diags.push(
                                    Diagnostic::error(format!(
                                        "cannot use arithmetic on generic `{lt}` without a `Numeric` bound"
                                    ))
                                    .with_label(*span, "add `extends Numeric` to the type parameter")
                                    .with_code("E302"),
                                );
                            } else {
                                self.diags.push(mismatch(*span, &lt, &rt));
                            }
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
                        if lt == Ty::Int && rt == Ty::Int {
                            self.coerce_expr_literals(lhs, &Ty::U64);
                            self.coerce_expr_literals(rhs, &Ty::U64);
                        } else if lt == Ty::Int && is_integer(&rt) {
                            self.coerce_expr_literals(lhs, &rt);
                        } else if rt == Ty::Int && is_integer(&lt) {
                            self.coerce_expr_literals(rhs, &lt);
                        }
                        let lt = self.infer_expr(lhs);
                        let rt = self.infer_expr(rhs);
                        if ty_has_error(&lt) || ty_has_error(&rt) {
                            return self.record(*id, Ty::Error);
                        }
                        if !types_compatible(&lt, &rt) || !self.is_comparable_in_scope(&lt) {
                            if matches!((&lt, &rt), (Ty::Param(_), _) | (_, Ty::Param(_)))
                                && types_compatible(&lt, &rt)
                            {
                                self.diags.push(
                                    Diagnostic::error(format!(
                                        "cannot compare generic `{lt}` without a `Comparable` bound"
                                    ))
                                    .with_label(
                                        *span,
                                        "add `extends Comparable` (or `extends Numeric`)",
                                    )
                                    .with_code("E302"),
                                );
                            } else {
                                self.diags.push(mismatch(*span, &lt, &rt));
                            }
                            return self.record(*id, Ty::Error);
                        }
                        self.record(*id, Ty::Bool)
                    }
                    HirBinOp::Lt | HirBinOp::Le | HirBinOp::Gt | HirBinOp::Ge => {
                        if lt == Ty::Int && rt == Ty::Int {
                            self.coerce_expr_literals(lhs, &Ty::U64);
                            self.coerce_expr_literals(rhs, &Ty::U64);
                        } else if lt == Ty::Int && is_integer(&rt) {
                            self.coerce_expr_literals(lhs, &rt);
                        } else if rt == Ty::Int && is_integer(&lt) {
                            self.coerce_expr_literals(rhs, &lt);
                        }
                        let lt = self.infer_expr(lhs);
                        let rt = self.infer_expr(rhs);
                        if ty_has_error(&lt) || ty_has_error(&rt) {
                            return self.record(*id, Ty::Error);
                        }
                        if !types_compatible(&lt, &rt) || !self.is_numeric_in_scope(&lt) {
                            if matches!((&lt, &rt), (Ty::Param(_), _) | (_, Ty::Param(_)))
                                && types_compatible(&lt, &rt)
                            {
                                self.diags.push(
                                    Diagnostic::error(format!(
                                        "cannot order generic `{lt}` without a `Numeric` bound"
                                    ))
                                    .with_label(
                                        *span,
                                        "add `extends Numeric` to the type parameter",
                                    )
                                    .with_code("E302"),
                                );
                            } else {
                                self.diags.push(mismatch(*span, &lt, &rt));
                            }
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
            HirExpr::Cast {
                id,
                inner,
                target,
                target_span,
                span,
            } => self.check_cast(*id, inner, target, *target_span, *span),
        }
    }

    /// Explicit numeric conversion (`value as u8`, TypeScript-like).
    ///
    /// Semantics (v0, integers only):
    /// - Target must be `u64`, `i64`, or `u8` (float/string/array casts are
    ///   rejected with E302; they need ISA conversions not yet specified).
    /// - Source must be a concrete integer (`u64/i64/u8/int` literal).
    ///   Generic parameters are rejected even with a `Numeric` bound: `T`
    ///   may instantiate to `f64`, whose IEEE-754 bits must never be copied
    ///   into an integer lane by the backend's value copy.
    /// - Literals are range-checked at compile time (`300 as u8` is E302).
    /// - Variable conversions are unchecked reinterpretations with no runtime
    ///   cost (no trap, no wrap instruction): the 64-bit payload is kept and
    ///   reinterpreted per the target lane (backends treat `u8` like `u64`
    ///   for arithmetic). Out-of-range variables are logic errors.
    /// - Implicit cross-integer conversions stay narrow: only literals coerce
    ///   implicitly; variables require `as`.
    fn check_cast(
        &mut self,
        id: vl_hir::HirId,
        inner: &HirExpr,
        target_vl: &VlType,
        target_span: Span,
        _span: Span,
    ) -> Ty {
        let target = Ty::from_vl_in(target_vl, &self.type_env);
        if ty_has_error(&target) {
            // Unknown target (`as Bogus` parses, but `T` params are rejected
            // at parse scope): report once here since the parser could not.
            self.diags.push(
                Diagnostic::error(format!("unknown cast target `{target_vl}`"))
                    .with_label(target_span, "no type with this name is in scope")
                    .with_code("E105"),
            );
            let _ = self.infer_expr(inner);
            return self.record(id, Ty::Error);
        }
        // v0 targets: integers only.
        if !matches!(target, Ty::U64 | Ty::I64 | Ty::U8) {
            self.diags.push(
                Diagnostic::error(format!("cannot cast to `{target}` (only `u64`, `i64`, `u8` casts are supported)"))
                    .with_label(target_span, "unsupported cast target")
                    .with_note("float, string, and array conversions need target conversions not yet specified")
                    .with_code("E302"),
            );
            let _ = self.infer_expr(inner);
            return self.record(id, Ty::Error);
        }
        // Identity: still infer inner for its errors.
        let inner_ty = self.infer_expr(inner);
        if ty_has_error(&inner_ty) || ty_has_error(&target) {
            return self.record(id, Ty::Error);
        }
        if inner_ty == Ty::Void {
            self.diags.push(
                Diagnostic::error("cannot cast a `void` value")
                    .with_label(inner.span(), "`void` is not a value")
                    .with_code("E308"),
            );
            return self.record(id, Ty::Error);
        }
        // Source must be a concrete integer. Generic parameters are rejected
        // even with a `Numeric` bound: `T` may instantiate to `f64`, and the
        // backend lowers casts to a value copy, which would reinterpret IEEE
        // bits as an integer.
        if matches!(&inner_ty, Ty::Param(_)) {
            self.diags.push(
                Diagnostic::error(format!("cannot cast generic `{inner_ty}` to `{target}`"))
                    .with_label(
                        inner.span(),
                        "casts need a concrete integer source (`u64`, `i64`, `u8`)",
                    )
                    .with_note("monomorphize first (call with a concrete type), then cast")
                    .with_code("E302"),
            );
            return self.record(id, Ty::Error);
        }
        if !matches!(&inner_ty, Ty::Int | Ty::U64 | Ty::I64 | Ty::U8) {
            self.diags.push(
                Diagnostic::error(format!("cannot cast `{inner_ty}` to `{target}`"))
                    .with_label(
                        inner.span(),
                        format!("expected an integer here, got `{inner_ty}`"),
                    )
                    .with_code("E302"),
            );
            return self.record(id, Ty::Error);
        }
        // Literals range-check against the target; variables are unchecked.
        if let HirExpr::Literal {
            value: Scalar::Int(v),
            span: lit_span,
            ..
        } = inner
        {
            if coerce_int_literal(*v, &target, *lit_span, &mut self.diags).is_none() {
                return self.record(id, Ty::Error);
            }
            // Record the literal under its target lane so LIR emits the right
            // constant class without a conversion instruction.
            self.record(inner.id(), target.clone());
        }
        self.record(id, target)
    }
}

fn mismatch(span: Span, lt: &Ty, rt: &Ty) -> Diagnostic {
    Diagnostic::error(format!("type mismatch: {lt} vs {rt}"))
        .with_label(span, format!("expected {lt} on both sides"))
        .with_code("E302")
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

pub(crate) fn is_integer(ty: &Ty) -> bool {
    matches!(ty, Ty::Int | Ty::U64 | Ty::I64 | Ty::U8)
}

/// Recursive poison check: `Array[Error]` is just as poisoned as `Error.
/// Direct `== Ty::Error` comparisons miss nested poison and cascade.
pub(crate) fn ty_has_error(ty: &Ty) -> bool {
    match ty {
        Ty::Error => true,
        Ty::Array(elem) => ty_has_error(elem),
        _ => false,
    }
}

/// Does a type still hold an unresolved `int` literal (top level or nested
/// in `Array`)? Such types must be defaulted (`u64` lane) before lowering.
fn ty_contains_int(ty: &Ty) -> bool {
    match ty {
        Ty::Int => true,
        Ty::Array(elem) => ty_contains_int(elem),
        _ => false,
    }
}

/// Structurally unify two solved constraint types, letting `int` defer to a
/// concrete integer lane at any depth (`Array[Int]` + `Array[u8]` gives
/// `Array[u8]`). Returns `None` on genuine conflict.
pub(crate) fn unify_solved(a: &Ty, b: &Ty) -> Option<Ty> {
    if a == b {
        return Some(a.clone());
    }
    if *a == Ty::Int && is_integer(b) {
        return Some(b.clone());
    }
    if *b == Ty::Int && is_integer(a) {
        return Some(a.clone());
    }
    match (a, b) {
        (Ty::Array(x), Ty::Array(y)) => unify_solved(x, y).map(|e| Ty::Array(Box::new(e))),
        _ => None,
    }
}

/// Central integer coercion: the single operation for literal defaulting,
/// range checks, and compatibility.
///
/// - Literal defaulting: an unconstrained `int` literal defaults to the
///   target's unsigned lane (`u64`) via [`default_inferred_ty`].
/// - Range checks: [`int_fits_in`] + [`coerce_int_literal`] reject
///   out-of-range literals (`let x: u8 = 300;`) with E302 instead of
///   silently truncating.
/// - Compatibility: after [`Checker::coerce_expr_literals`] has run,
///   ordinary compatibility is exact equality ([`types_compatible`]); the old
///   general `Int`-vs-integer exception lives only inside coercion, so an
///   `int` variable cannot escape to a narrower parameter.
///
/// All use sites funnel through `infer_expr_expected` (coerce then infer)
/// plus exact `types_compatible`, keeping implicit cross-integer conversions
/// narrow (literals only).
pub fn coerce_int_literal(
    value: i64,
    want: &Ty,
    span: Span,
    diags: &mut Vec<Diagnostic>,
) -> Option<Ty> {
    if !int_fits_in(value, want) {
        diags.push(
            Diagnostic::error(format!(
                "integer literal `{value}` out of range for `{want}`"
            ))
            .with_label(span, format!("expected `{want}` here"))
            .with_code("E302"),
        );
        return None;
    }
    Some(want.clone())
}

/// Does an untyped `int` literal value fit in the target integer type?
fn int_fits_in(v: i64, expected: &Ty) -> bool {
    match expected {
        Ty::Int | Ty::I64 => true,
        Ty::U64 => v >= 0,
        Ty::U8 => (0..=255).contains(&v),
        _ => true,
    }
}

/// Default for inferred type arguments left as untyped `int`: the target's
/// unsigned lane (`u64`). Keeps non-literal `Int` expressions (generic call
/// results) from staying compatible with every integer type.
pub(crate) fn default_inferred_ty(ty: Ty) -> Ty {
    match ty {
        Ty::Int => Ty::U64,
        Ty::Array(elem) => Ty::Array(Box::new(default_inferred_ty(*elem))),
        _ => ty,
    }
}

/// Constraint-based generic inference: one entry per type parameter holding
/// every actual type that constrains it. Collecting first and solving after
/// makes inference independent of argument order and supports nested
/// constraints (`Array[Array[T]]` contributes `T` through two layers).
/// A future contextual pass can push one more constraint from the expected
/// return type before solving.
#[derive(Debug, Default)]
struct ConstraintSet {
    per_param: HashMap<String, Vec<Ty>>,
}

impl ConstraintSet {
    /// Collect constraints from one formal-vs-actual pair. `Param` uses push
    /// a constraint; concrete formals must match exactly (an `Int` actual
    /// against an integer formal is coercible and contributes nothing — the
    /// later per-argument coercion pass handles it). Reports one E306 on
    /// concrete mismatch and returns false.
    fn collect(
        &mut self,
        formal: &Ty,
        actual: &Ty,
        name: &str,
        span: Span,
        diags: &mut Vec<Diagnostic>,
    ) -> bool {
        match (formal, actual) {
            (Ty::Param(p), t) => {
                self.per_param.entry(p.clone()).or_default().push(t.clone());
                true
            }
            (Ty::Array(f), Ty::Array(a)) => self.collect(f, a, name, span, diags),
            (f, a) if f == a => true,
            // An untyped literal against a concrete integer lane coerces
            // later; it is not an inference conflict.
            (f, Ty::Int) if is_integer(f) => true,
            (f, a) => {
                diags.push(
                    Diagnostic::error(format!("`{name}` expects `{f}`, got `{a}`"))
                        .with_label(span, format!("expected `{f}` here"))
                        .with_code("E306"),
                );
                false
            }
        }
    }

    /// Solve collected constraints into concrete type arguments, defaulting
    /// leftover `int` to `u64`. Integer literals defer to concrete
    /// constraints structurally at any depth, so `same(1u64, 2)` and
    /// `same(1, 2u64)` both solve `T = u64`, and `same([1], [2u8])` solves
    /// `T = Array[u8]` (elements are coerced after solving). Reports one
    /// diagnostic per failure.
    fn solve(
        mut self,
        name: &str,
        span: Span,
        sig: &FuncSigTy,
        diags: &mut Vec<Diagnostic>,
    ) -> Option<Vec<Ty>> {
        let mut out = Vec::with_capacity(sig.type_params.len());
        for p in &sig.type_params {
            let constraints = self.per_param.remove(p).unwrap_or_default();
            if constraints.is_empty() {
                diags.push(
                    Diagnostic::error(format!("cannot infer type argument `{p}` for `{name}`"))
                        .with_label(span, "pass it explicitly: `::[...]`")
                        .with_note(format!("write `{name}::[{p}](...)` with a concrete type"))
                        .with_code("E303"),
                );
                return None;
            }
            // Fold constraints structurally: `Int` defers to a concrete
            // integer lane even when nested (`Array[Int]` vs `Array[u8]`).
            let mut acc: Option<Ty> = None;
            for t in &constraints {
                match &acc {
                    None => acc = Some(t.clone()),
                    Some(c) => match unify_solved(c, t) {
                        Some(u) => acc = Some(u),
                        None => {
                            diags.push(
                                Diagnostic::error(format!(
                                    "`{name}` infers conflicting types for `{p}`: `{c}` vs `{t}`"
                                ))
                                .with_label(span, "conflicting arguments here")
                                .with_code("E306"),
                            );
                            return None;
                        }
                    },
                }
            }
            // Anything still holding `int` (all-literal constraints) defaults
            // through the `u64` lane, so a generic result crossing a concrete
            // boundary (`return id(300);` in a `u8` function) mismatches
            // instead of silently truncating.
            out.push(default_inferred_ty(acc.expect("non-empty constraints")));
        }
        Some(out)
    }
}

/// General control-flow analysis: how does a statement complete?
/// `FallsThrough` means execution may continue past it; the other three
/// diverge (exit the current block via `return`/`break`/`continue`).
/// `while` always falls through (the body may not run; `break` exits to
/// after the loop). An `if` without `else` always falls through (the missing
/// branch does). This grounds the definite-return check (`Returns`) and
/// unreachable-code warnings, and prepares for future `match`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    FallsThrough,
    Returns,
    Breaks,
    Continues,
}

/// Flow of one statement.
pub fn stmt_flow(stmt: &HirStmt) -> Flow {
    match stmt {
        HirStmt::Return { .. } => Flow::Returns,
        HirStmt::Break { .. } => Flow::Breaks,
        HirStmt::Continue { .. } => Flow::Continues,
        HirStmt::Let { .. }
        | HirStmt::Assign { .. }
        | HirStmt::IndexAssign { .. }
        | HirStmt::Expr(_) => Flow::FallsThrough,
        HirStmt::While { .. } => Flow::FallsThrough,
        HirStmt::If {
            then_body,
            else_body,
            ..
        } => {
            let then_flow = block_flow(then_body);
            let Some(else_body) = else_body else {
                return Flow::FallsThrough;
            };
            let else_flow = block_flow(else_body);
            if then_flow == else_flow {
                return then_flow;
            }
            if then_flow == Flow::FallsThrough || else_flow == Flow::FallsThrough {
                return Flow::FallsThrough;
            }
            // Both branches diverge but differently (`return` vs `break`):
            // no path falls through, but not every path returns either.
            // Report a diverging non-`Returns` flow so the missing-return
            // check still fires while unreachable-code analysis still sees
            // the construct as diverging.
            Flow::Breaks
        }
    }
}

/// Flow of a block: the first diverging statement wins (later statements
/// are unreachable); a block with no diverging statement falls through.
pub fn block_flow(stmts: &[HirStmt]) -> Flow {
    for stmt in stmts {
        let flow = stmt_flow(stmt);
        if flow != Flow::FallsThrough {
            return flow;
        }
    }
    Flow::FallsThrough
}

/// Warn on statements unreachable after a diverging statement, recursing
/// into branches and loop bodies. Warnings never fail the build.
fn check_unreachable(stmts: &[HirStmt], diags: &mut Vec<Diagnostic>) {
    let mut diverged = false;
    for stmt in stmts {
        if diverged {
            diags.push(
                Diagnostic::warning("unreachable code")
                    .with_label(stmt_span(stmt), "this statement can never run")
                    .with_note("it follows a `return`, `break`, or `continue`")
                    .with_code("W001"),
            );
        }
        match stmt {
            HirStmt::If {
                then_body,
                else_body,
                ..
            } => {
                check_unreachable(then_body, diags);
                if let Some(body) = else_body {
                    check_unreachable(body, diags);
                }
            }
            HirStmt::While { body, .. } => check_unreachable(body, diags),
            _ => {}
        }
        if stmt_flow(stmt) != Flow::FallsThrough {
            diverged = true;
        }
    }
}

/// Span of one statement (for labelling the fallthrough path).
fn stmt_span(stmt: &HirStmt) -> Span {
    match stmt {
        HirStmt::Let { span, .. }
        | HirStmt::Assign { span, .. }
        | HirStmt::IndexAssign { span, .. }
        | HirStmt::If { span, .. }
        | HirStmt::While { span, .. }
        | HirStmt::Break { span }
        | HirStmt::Continue { span }
        | HirStmt::Return { span, .. } => *span,
        HirStmt::Expr(e) => e.span(),
    }
}

/// Span of the construct that lets execution fall through: an `if` whose
/// taken branch returns while the other path falls through, else the last
/// statement (`None` for an empty body). Implemented over [`block_flow`].
fn fallthrough_span(body: &[HirStmt]) -> Option<Span> {
    for stmt in body {
        if let HirStmt::If {
            then_body,
            else_body,
            span,
            ..
        } = stmt
        {
            let then_returns = block_flow(then_body) == Flow::Returns;
            let else_returns = else_body
                .as_ref()
                .map(|body| block_flow(body) == Flow::Returns)
                .unwrap_or(false);
            if then_returns != else_returns {
                return Some(*span);
            }
        }
    }
    body.last().map(stmt_span)
}

/// Ordinary compatibility after coercion: exact type equality (plus
/// structural `Array[T]` recursion). The former general `Int`-vs-integer
/// exception is gone on purpose — integer literals are coerced to their
/// context type by [`Checker::coerce_expr_literals`] before this runs, so a
/// lingering `Int` here means "no context supplied one" and must not match
/// every integer lane.
fn types_compatible(got: &Ty, want: &Ty) -> bool {
    got == want || matches!((got, want), (Ty::Array(g), Ty::Array(w)) if types_compatible(g, w))
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

/// Does a concrete type satisfy a generic bound? `Numeric` covers the
/// arithmetic lanes (`u64,i64,f64,u8`, plus undefaulted `int` defensively);
/// `Comparable` covers those plus `bool` and `string` (equality).
pub(crate) fn bound_satisfied(bound: GenericBound, ty: &Ty) -> bool {
    match bound {
        GenericBound::Numeric => matches!(ty, Ty::Int | Ty::U64 | Ty::I64 | Ty::F64 | Ty::U8),
        GenericBound::Comparable => matches!(
            ty,
            Ty::Int | Ty::U64 | Ty::I64 | Ty::F64 | Ty::Bool | Ty::U8 | Ty::String
        ),
    }
}

/// Does an outer bound imply an inner requirement? `Numeric` is stronger:
/// it satisfies both `Numeric` and `Comparable`. `Comparable` satisfies
/// only `Comparable`. Unconstrained satisfies neither.
fn bound_implies(outer: Option<GenericBound>, inner: GenericBound) -> bool {
    matches!(
        (outer, inner),
        (Some(GenericBound::Numeric), _)
            | (Some(GenericBound::Comparable), GenericBound::Comparable)
    )
}

/// Does `ty` (concrete or outer `Param`) satisfy `bound` given the current
/// function's bounds? Concrete types check directly; an outer parameter
/// checks by implication (so `wrap[T]` cannot forward an unconstrained `T`
/// to a `Numeric` callee).
fn ty_satisfies_bound(
    ty: &Ty,
    bound: GenericBound,
    outer_bounds: &HashMap<String, GenericBound>,
) -> bool {
    match ty {
        Ty::Param(name) => bound_implies(outer_bounds.get(name).copied(), bound),
        Ty::Array(_) => false,
        _ => bound_satisfied(bound, ty),
    }
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
    fn bare_array_new_without_annotation_is_one_error() {
        let (_, diags) = check_src("function main() { let a = Array.new(3); a; }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("needs an element type")),
            "{diags:?}"
        );
    }

    #[test]
    fn annotated_let_supplies_array_new_element() {
        let (typed, diags) =
            check_src("function main() { let scores: Array[u64] = Array.new(3); scores; }");
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed
            .types
            .values()
            .any(|t| *t == Ty::Array(Box::new(Ty::U64))));
    }

    #[test]
    fn annotated_let_accepts_empty_literal() {
        let (_, diags) = check_src("function main() { let e: Array[u64] = []; e[0] = 1; e; }");
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn annotated_let_rejects_mismatch() {
        let (_, diags) = check_src("function main() { let x: u64 = \"s\"; x; }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("E309")),
            "{diags:?}"
        );
    }

    #[test]
    fn annotated_let_coerces_int_literals() {
        let (_, diags) = check_src("function main() { let x: u64 = 3; x; }");
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn annotated_let_with_explicit_turbofish_checks() {
        let (_, diags) =
            check_src("function main() { let a: Array[u64] = Array.new::[u64](3); a; }");
        assert!(diags.is_empty(), "{diags:?}");
        let (_, diags) =
            check_src("function main() { let a: Array[string] = Array.new::[u64](3); a; }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
    }

    #[test]
    fn annotated_let_in_generic_body() {
        let (_, diags) = check_src(
            "function f[T](x: T): T { let y: T = x; let a: Array[T] = Array.new(1); return y; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn failed_annotation_poisons_quietly() {
        // Parser reports E105; typecheck must not cascade.
        let (toks, _) = vl_lex::lex("function main() { let x: Bogus = 1; x; }");
        let (prog, pdiags) = vl_syntax::parse(&toks, "");
        assert!(pdiags.iter().any(|d| d.is_error()));
        let (res, _) = vl_semantic::resolve(&prog);
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
            diags
                .iter()
                .any(|d| d.message.contains("not all paths return")),
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

    #[test]
    fn partial_return_path_is_an_error() {
        // Every reachable path must return: a single `if` branch is not
        // enough, even though a value `return` is present.
        let (_, diags) = check_src(
            "function f(x: bool): u64 { if (x) { return 1u64; } } function main() { f(true); }",
        );
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("not all paths return")),
            "{diags:?}"
        );
        assert!(
            diags.iter().any(|d| d.labels.iter().any(|l| l
                .message
                .as_deref()
                .is_some_and(|m| m.contains("fall through")))),
            "{diags:?}"
        );
    }

    #[test]
    fn while_body_return_does_not_satisfy() {
        // A `while` body may never run, so its `return` never counts.
        let (_, diags) = check_src(
            "function f(x: bool): u64 { while (x) { return 1u64; } } function main() { f(true); }",
        );
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("E307")),
            "{diags:?}"
        );
    }

    #[test]
    fn bare_return_in_value_function_is_one_error() {
        // The invalid bare `return` is the single error: no second
        // missing-return diagnostic follows it.
        let (_, diags) = check_src("function f(): i64 { return; } function main() { }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("returns nothing"), "{diags:?}");
    }

    #[test]
    fn expanding_recursion_hits_the_instance_budget() {
        // Original void-function repro: type-expanding recursion must
        // terminate with exactly one E303 diagnostic, not hang the compiler.
        // The void body keeps the return check quiet so the budget error is
        // the one root cause (single-root-error rule).
        let (_, diags) =
            check_src("function grow[T](x: T) { grow([x]); } function main() { grow(1u64); }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert_eq!(diags[0].code.as_deref(), Some("E303"), "{diags:?}");
        assert!(
            diags[0].message.contains("instantiation limit exceeded"),
            "{diags:?}"
        );
    }

    #[test]
    fn nested_unknown_type_argument_is_an_error() {
        // `Array(Error)` is poisoned: one `unknown type` error, no cascade.
        let (_, diags) = check_src("function main() { let a = Array.new::[Array[Bogus]](1u64); }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("unknown type"), "{diags:?}");
    }

    #[test]
    fn u8_literal_range_is_checked() {
        let (_, diags) = check_src("function main() { let x: u8 = 300; x; }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("out of range"), "{diags:?}");
        let (_, diags) = check_src("function main() { let x: u8 = 255; x; }");
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn int_binding_does_not_escape_to_u8_param() {
        // `let v = 300` resolves to `u64`, which must not pass a `u8`
        // parameter even though the literal would fit neither.
        let (_, diags) = check_src(
            "function take(x: u8): u8 { return x; } function main() { let v = 300; take(v); }",
        );
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("expects `u8`"), "{diags:?}");
    }

    #[test]
    fn generic_result_does_not_escape_to_u8() {
        // All-literal inference defaults to `u64`, so a generic result
        // crossing a narrower boundary mismatches instead of truncating.
        let (typed, diags) = check_src(
            "function id[T](x: T): T { return x; } function f(): u64 { return id(300); } function main() { f(); }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.instances.contains_key("id$u64"));
        let (_, diags) = check_src(
            "function id[T](x: T): T { return x; } function f(): u8 { return id(300); } function main() { f(); }",
        );
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("declares return"), "{diags:?}");
    }

    #[test]
    fn generic_inference_ignores_argument_order() {
        // A literal constraint defers to a concrete one regardless of
        // position: both orders infer `T = u64`.
        for call in ["same(1u64, 2)", "same(1, 2u64)"] {
            let (typed, diags) = check_src(&format!(
                "function same[T](a: T, b: T): T {{ return a; }} function main() {{ {call}; }}"
            ));
            assert!(diags.is_empty(), "{call}: {diags:?}");
            assert!(typed.instances.contains_key("same$u64"), "{call}");
        }
    }

    #[test]
    fn turbofish_on_extern_is_an_error() {
        let (_, diags) = check_src("use std.print; function main() { print::[u64](\"hi\"); }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("not generic"), "{diags:?}");
    }

    /// Build a `main` calling `id` once per nesting depth `0..count`
    /// (`1u64`, `[1u64]`, `[[1u64]]`, ...), each a distinct instance.
    fn nested_id_calls(count: usize) -> String {
        let mut src = String::from("function id[T](x: T): T { return x; } function main() { ");
        for depth in 0..count {
            src.push_str("id(");
            for _ in 0..depth {
                src.push('[');
            }
            src.push_str("1u64");
            for _ in 0..depth {
                src.push(']');
            }
            src.push_str("); ");
        }
        src.push('}');
        src
    }

    #[test]
    fn instance_limit_allows_repeated_call_at_boundary() {
        // 64 distinct instances fill the budget; a repeated call after that
        // is a duplicate and must not trip the limit.
        let mut src = nested_id_calls(64);
        src.pop();
        src.push_str(" id(1u64); }");
        let (typed, diags) = check_src(&src);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(typed.instances.len(), 64);
    }

    #[test]
    fn instance_limit_rejects_the_65th_distinct_instance() {
        let (_, diags) = check_src(&nested_id_calls(65));
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("instantiation limit exceeded")),
            "{diags:?}"
        );
    }

    // -- Item 2: normalized-type invariant -------------------------------

    fn check_src_with_hir(src: &str) -> (vl_hir::HirProgram, TypedProgram, Vec<Diagnostic>) {
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        let (typed, diags) = check(&hir);
        (hir, typed, diags)
    }

    #[test]
    fn normalized_validation_accepts_clean_programs() {
        for src in [
            "function main() { let x = 1; x; }",
            "function id[T](x: T): T { return x; } function main() { id(1u64); }",
            "function main() { let a = [1, 2]; a; }",
            "function main() { 1 + 2; }",
        ] {
            let (hir, typed, diags) = check_src_with_hir(src);
            assert!(diags.iter().all(|d| !d.is_error()), "{src}: {diags:?}");
            assert!(typed.validate_normalized(&hir, &diags).is_empty(), "{src}");
        }
    }

    #[test]
    fn normalized_validation_rejects_lingering_int() {
        let (hir, mut typed, _) = check_src_with_hir("function main() { let x = 1u64; x; }");
        // Inject a non-normalized `Int` where a concrete type belongs.
        typed.types.insert(0, Ty::Int);
        let errs = typed.validate_normalized(&hir, &[]);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].code.as_deref(), Some("E500"));
    }

    #[test]
    fn normalized_validation_skips_generic_templates() {
        // `Param` inside a generic body is expected and must not trip the
        // boundary check; instances themselves are concrete.
        let (hir, typed, diags) = check_src_with_hir(
            "function id[T](x: T): T { return x; } function main() { id(1u64); }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.validate_normalized(&hir, &diags).is_empty());
        assert!(typed.instances.contains_key("id$u64"));
    }

    #[test]
    fn untyped_arithmetic_defaults_to_u64() {
        let (typed, diags) = check_src("function main() { 1 + 2; }");
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.types.values().any(|t| *t == Ty::U64));
        assert!(!typed.types.values().any(|t| *t == Ty::Int));
    }

    // -- Item 3: centralized coercion ------------------------------------

    #[test]
    fn implicit_cross_integer_stays_narrow() {
        // Concrete `i64` does not coerce to `u64` even though both are
        // integers; only literals coerce.
        let (_, diags) =
            check_src("function take(x: u64): u64 { return x; } function main() { let v: i64 = 1i64; take(v); }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("expects `u64`"), "{diags:?}");
    }

    #[test]
    fn coerce_int_literal_range_checks() {
        let mut diags = Vec::new();
        assert!(coerce_int_literal(255, &Ty::U8, Span::empty(0), &mut diags).is_some());
        assert!(diags.is_empty());
        assert!(coerce_int_literal(300, &Ty::U8, Span::empty(0), &mut diags).is_none());
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code.as_deref(), Some("E302"));
    }

    // -- Item 4: constraint-based inference --------------------------------

    #[test]
    fn nested_array_constraints_solve() {
        let (typed, diags) = check_src(
            "function first2[T](a: Array[Array[T]]): T { return a[0u64][0u64]; } function main() { first2([[1u64]]); }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.instances.contains_key("first2$u64"));
    }

    #[test]
    fn all_int_constraints_default_to_u64() {
        let (typed, diags) = check_src(
            "function same[T](a: T, b: T): T { return a; } function main() { same(1, 2); }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.instances.contains_key("same$u64"));
    }

    // -- Item 6: flow -------------------------------------------------------

    #[test]
    fn flow_breaks_do_not_satisfy_returns() {
        // `break` inside `while` diverges the loop body but the function
        // still falls through.
        let (_, diags) =
            check_src("function f(): u64 { while (true) { break; } } function main() { f(); }");
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("E307")),
            "{diags:?}"
        );
    }

    #[test]
    fn unreachable_code_warns_without_failing() {
        let (_, diags) = check_src("function main() { return; let x = 1; x; }");
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("W001")),
            "{diags:?}"
        );
    }

    #[test]
    fn if_both_branches_return_satisfies() {
        let (_, diags) = check_src(
            "function f(x: bool): u64 { if (x) { return 1u64; } else { return 2u64; } } function main() { f(true); }",
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    // -- Item 7: `as` casts --------------------------------------------------

    #[test]
    fn as_cast_same_type_checks() {
        let (_, diags) = check_src("function main() { let x = 1u64 as u64; x; }");
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn as_cast_variable_between_integers_checks() {
        let (_, diags) = check_src(
            "function take(x: u8): u8 { return x; } function main() { let v = 200u64; take(v as u8); }",
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn as_cast_literal_out_of_range_is_one_error() {
        let (_, diags) = check_src("function main() { let x = 300 as u8; x; }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("out of range"), "{diags:?}");
    }

    #[test]
    fn as_cast_rejects_non_integer_target() {
        let (_, diags) = check_src("function main() { let x = 1u64 as string; x; }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("cannot cast"), "{diags:?}");
    }

    #[test]
    fn as_cast_rejects_non_integer_source() {
        let (_, diags) = check_src("function main() { let s = \"hi\"; let x = s as u8; x; }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("cannot cast"), "{diags:?}");
    }

    #[test]
    fn implicit_variable_conversion_still_rejected() {
        // Without `as`, a `u64` variable must not flow into `u8`.
        let (_, diags) = check_src(
            "function take(x: u8): u8 { return x; } function main() { let v = 1u64; take(v); }",
        );
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
    }

    // -- Item 8: constrained generics ----------------------------------------

    #[test]
    fn unconstrained_param_rejects_arithmetic() {
        let (_, diags) = check_src(
            "function add[T](a: T, b: T): T { return a + b; } function main() { add(1u64, 2u64); }",
        );
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("Numeric"), "{diags:?}");
    }

    #[test]
    fn numeric_bound_allows_arithmetic() {
        let (typed, diags) = check_src(
            "function add[T extends Numeric](a: T, b: T): T { return a + b; } function main() { add(1u64, 2u64); add(1i64, 2i64); }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.instances.contains_key("add$u64"));
        assert!(typed.instances.contains_key("add$i64"));
    }

    #[test]
    fn comparable_bound_allows_equality() {
        let (_, diags) = check_src(
            "function eq[T extends Comparable](a: T, b: T): bool { return a == b; } function main() { eq(1u64, 2u64); eq(\"a\", \"b\"); }",
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn bound_violation_is_one_error() {
        let (_, diags) = check_src(
            "function add[T extends Numeric](a: T, b: T): T { return a + b; } function main() { add(\"a\", \"b\"); }",
        );
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("does not satisfy"), "{diags:?}");
        assert_eq!(diags[0].code.as_deref(), Some("E303"));
    }

    #[test]
    fn unconstrained_forwarding_to_bounded_is_an_error() {
        let (_, diags) = check_src(
            "function add[T extends Numeric](a: T, b: T): T { return a + b; } function wrap[T](x: T): T { return add(x, x); } function main() { wrap(1u64); }",
        );
        assert!(
            diags.iter().any(|d| d.message.contains("satisfy")),
            "{diags:?}"
        );
    }

    #[test]
    fn numeric_implies_comparable_forwarding() {
        let (_, diags) = check_src(
            "function eq[T extends Comparable](a: T, b: T): bool { return a == b; } function wrap[T extends Numeric](x: T): bool { return eq(x, x); } function main() { wrap(1u64); }",
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn as_cast_on_generic_param_is_rejected() {
        // `Numeric` includes `f64`: allowing `x as u8` for `x: T` would copy
        // IEEE-754 bits into an integer lane once `T = f64`.
        let (_, diags) = check_src(
            "function get[T extends Numeric](x: T): u8 { return x as u8; } function main() { get(1.5f64); }",
        );
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("generic"), "{diags:?}");
        assert_eq!(diags[0].code.as_deref(), Some("E302"));
    }

    #[test]
    fn as_cast_from_f64_is_rejected() {
        let (_, diags) = check_src("function main() { let x = 1.0f64 as u8; x; }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("cannot cast"), "{diags:?}");
    }

    #[test]
    fn nested_int_literal_defers_to_concrete_element() {
        // Array literals keep `Array[Int]` until solving: `T` solves to
        // `Array[u8]` (not a `u64`-vs-`u8` conflict) and the `1` coerces.
        let (typed, diags) = check_src(
            "function same[T](a: T, b: T): T { return a; } function main() { same([1], [2u8]); }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.instances.contains_key("same$Array_u8"));
    }

    #[test]
    fn nested_int_conflict_still_conflicts() {
        let (_, diags) = check_src(
            "function same[T](a: T, b: T): T { return a; } function main() { same([1u64], [2u8]); }",
        );
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("conflicting types"), "{diags:?}");
    }

    #[test]
    fn unannotated_all_int_array_defaults_to_u64() {
        let (typed, diags) = check_src("function main() { let a = [1, 2]; a; }");
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed
            .types
            .values()
            .any(|t| *t == Ty::Array(Box::new(Ty::U64))));
        assert!(!typed.types.values().any(ty_contains_int));
    }

    #[test]
    fn instance_bodies_validate_after_substitution() {
        let (hir, typed, diags) = check_src_with_hir(
            "function add[T extends Numeric](a: T, b: T): T { return a + b; } function main() { add(1u64, 2u64); }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.validate_normalized(&hir, &diags).is_empty());
    }

    #[test]
    fn validation_stands_down_with_prior_errors() {
        let (hir, mut typed, _) = check_src_with_hir("function main() { let x = 1u64; x; }");
        typed.types.insert(0, Ty::Error);
        // No prior errors: poison at the boundary is itself reported.
        let errs = typed.validate_normalized(&hir, &[]);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].code.as_deref(), Some("E500"));
        // With a prior error (lowering already blocked): validation is moot.
        let prior = vec![Diagnostic::error("prior failure").with_code("E999")];
        assert!(typed.validate_normalized(&hir, &prior).is_empty());
    }
}
