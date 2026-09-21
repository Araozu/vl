//! Monomorphization pass: separate instance expansion from type checking.
//!
//! The checker discovers concrete `(template, args)` pairs while checking
//! calls; this pass drains that worklist, substituting each outer instance's
//! arguments before resolving inner calls, so generic-to-generic forwarding
//! (`wrap[T]` calling `id::[T]`) lands on concrete instances too.
//!
//! This pass owns:
//! - instance caching (`visited` + `TypedProgram::instances`, duplicate calls
//!   reuse the same mangled name without consuming budget),
//! - poisoned-template suppression (poisoned actuals or unbound `Param`
//!   stay quiet — the root cause was already reported while checking),
//! - expanding-recursion diagnostics (type-expanding recursion gets E303
//!   instead of hanging the compiler),
//! - resource budgets (`MAX_INSTANCES` caps distinct instances).

use std::collections::{HashMap, HashSet};

use vl_common::{Diagnostic, Span, VlType};
use vl_hir::{HirExpr, HirItem, HirProgram, HirStmt};

use super::{mangle, subst_ty, ty_has_error, FuncSigTy, Instance, Ty, TypedProgram};

/// Budget for monomorphization: type-expanding recursion creates a fresh
/// instance per nesting level (`T`, `Array[T]`, `Array[Array[T]]`, ...).
pub const MAX_INSTANCES: usize = 64;

/// Drain `pending` into `typed`: one [`Instance`] per concrete generic call.
/// LIR emits one function per entry of [`TypedProgram::instances`].
pub fn expand(
    prog: &HirProgram,
    typed: &mut TypedProgram,
    pending: Vec<(u32, Vec<Ty>)>,
    diags: &mut Vec<Diagnostic>,
) {
    let mut mono = Monomorphizer {
        prog,
        typed,
        diags,
        pending,
        visited: HashSet::new(),
    };
    mono.run();
}

struct Monomorphizer<'a> {
    prog: &'a HirProgram,
    typed: &'a mut TypedProgram,
    diags: &'a mut Vec<Diagnostic>,
    pending: Vec<(u32, Vec<Ty>)>,
    visited: HashSet<String>,
}

impl Monomorphizer<'_> {
    fn run(&mut self) {
        while let Some((def, args)) = self.pending.pop() {
            let Some(sig) = self.typed.func_sigs.get(&def).cloned() else {
                continue;
            };
            // Poisoned templates never expand: the definition was already
            // reported (missing annotations, bad bounds); stay quiet.
            if ty_has_error(&sig.ret) || sig.param_tys.iter().any(ty_has_error) {
                continue;
            }
            if args.iter().any(ty_has_error) {
                continue;
            }
            let Some(name) = instance_name(self.prog, def) else {
                continue;
            };
            let mangled = mangle(&name, &args);
            // Instance caching: skip duplicates before enforcing the budget —
            // a repeated call at the valid boundary must not consume the
            // instance allowance.
            if self.visited.contains(&mangled) {
                continue;
            }
            // Resource budget: type-expanding recursion (`grow[T]` calling
            // `grow[[T]]>`) would otherwise create infinitely many instances.
            // Cap the worklist with a diagnostic instead of hanging.
            if self.visited.len() >= MAX_INSTANCES {
                let span = fn_span_for(self.prog, def);
                let name = instance_name(self.prog, def).unwrap_or_else(|| format!("def#{def}"));
                self.diags.push(
                    Diagnostic::error(format!(
                        "generic instantiation limit exceeded ({} of {} instances, at `{}::{}`; possible polymorphic recursion)",
                        self.visited.len(),
                        MAX_INSTANCES,
                        self.prog.module,
                        name,
                    ))
                    .with_label(span, "recursive instantiations keep growing here")
                    .with_note(
                        "avoid calls that wrap a type parameter in a larger type (`grow([x])`)",
                    )
                    .with_code("E303"),
                );
                break;
            }
            self.visited.insert(mangled.clone());
            let (param_tys, ret_ty) = sig.instantiate(&args);
            // Defensive: only normalized signatures reach LIR; anything else
            // is a checker bug (already guarded above, but stay quiet rather
            // than emitting invalid instances).
            if param_tys.iter().any(ty_has_error) || ty_has_error(&ret_ty) {
                continue;
            }
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
                        bounds: HashMap::new(),
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
            let calls = calls_in_item(self.prog, self.typed, def);
            for (call_id, callee_def, type_args, actual_tys) in calls {
                let Some(inner) = self.typed.func_sigs.get(&callee_def).cloned() else {
                    continue;
                };
                if inner.type_params.is_empty() {
                    continue;
                }
                if ty_has_error(&inner.ret) || inner.param_tys.iter().any(ty_has_error) {
                    continue;
                }
                // Substitute first: formals, explicit args, and the recorded
                // (generic) actual types all live in template space.
                let actuals: Vec<Ty> = actual_tys.iter().map(|t| subst_ty(t, &env)).collect();
                if actuals.iter().any(ty_has_error) {
                    continue;
                }
                let resolved: Option<Vec<Ty>> = if type_args.is_empty() {
                    infer_quiet(&inner, &actuals)
                } else {
                    let mut out = Vec::with_capacity(type_args.len());
                    let mut ok = true;
                    for v in &type_args {
                        // Explicit arguments name outer parameters (`T`
                        // means the caller's `T`): substitute, and anything
                        // still a `Param` afterwards is unbound (already
                        // reported while checking the template). Invalid
                        // capabilities (`*T`, `*u64` via substitution) stay
                        // quiet here: the template already owns the E106.
                        let t = vl_in_instance(v, &env);
                        if !t.is_concrete() || !super::is_capability_valid(&t) {
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
                // Bounds were enforced with diagnostics while checking (both
                // for concrete calls and for `Param` forwarding by
                // implication); a concrete violation here means the outer
                // template was already rejected, so stay quiet.
                {
                    use super::bound_satisfied;
                    let mut ok = true;
                    for (param, arg) in inner.type_params.iter().zip(resolved.iter()) {
                        if let Some(bound) = inner.bounds.get(param) {
                            if !bound_satisfied(*bound, arg) {
                                ok = false;
                                break;
                            }
                        }
                    }
                    if !ok {
                        continue;
                    }
                }
                let Some(inner_name) = instance_name(self.prog, callee_def) else {
                    continue;
                };
                let inner_mangled = mangle(&inner_name, &resolved);
                self.typed
                    .inst_calls
                    .insert((mangled.clone(), call_id), inner_mangled.clone());
                if !self.typed.instances.contains_key(&inner_mangled)
                    && !self
                        .pending
                        .iter()
                        .any(|(d, a)| *d == callee_def && *a == resolved)
                {
                    self.pending.push((callee_def, resolved));
                }
            }
        }
    }
}

/// Template function name for a `DefId.0`.
fn instance_name(prog: &HirProgram, def: u32) -> Option<String> {
    prog.items.iter().find_map(|item| match item {
        HirItem::Fn {
            def: Some(d), name, ..
        } if d.0 == def => Some(name.clone()),
        _ => None,
    })
}

/// Template function span for diagnostics (falls back to empty).
fn fn_span_for(prog: &HirProgram, def: u32) -> Span {
    prog.items
        .iter()
        .find_map(|item| match item {
            HirItem::Fn {
                def: Some(d), span, ..
            } if d.0 == def => Some(*span),
            _ => None,
        })
        .unwrap_or(Span::empty(0))
}

/// Convert an explicit type argument under an instance environment:
/// outer parameter names substitute, concrete types convert directly.
fn vl_in_instance(v: &VlType, env: &HashMap<String, Ty>) -> Ty {
    match v {
        VlType::Param(name) => env.get(name).cloned().unwrap_or(Ty::Param(name.clone())),
        VlType::Array(elem) => Ty::Array(Box::new(vl_in_instance(elem, env))),
        VlType::Mutable(inner) => Ty::Mutable(Box::new(vl_in_instance(inner, env))),
        _ => Ty::from_vl(v),
    }
}

/// Quiet inference for worklist expansion (errors were already reported
/// while checking the template generically). Constraint-based: collect
/// first, solve quietly after, with the same structural `int` deferral as
/// the reporting path.
fn infer_quiet(sig: &FuncSigTy, actuals: &[Ty]) -> Option<Vec<Ty>> {
    use super::{default_inferred_ty, unify_solved};
    let mut per_param: HashMap<String, Vec<Ty>> = HashMap::new();
    for (formal, actual) in sig.param_tys.iter().zip(actuals.iter()) {
        if !collect_quiet(formal, actual, &mut per_param) {
            return None;
        }
    }
    let mut out = Vec::with_capacity(sig.type_params.len());
    for p in &sig.type_params {
        let constraints = per_param.remove(p).unwrap_or_default();
        if constraints.is_empty() {
            return None;
        }
        let mut acc: Option<Ty> = None;
        for t in &constraints {
            match &acc {
                None => acc = Some(t.clone()),
                Some(c) => {
                    let u = unify_solved(c, t)?;
                    acc = Some(u)
                }
            }
        }
        out.push(default_inferred_ty(acc.expect("non-empty constraints")));
    }
    Some(out)
}

fn collect_quiet(formal: &Ty, actual: &Ty, per_param: &mut HashMap<String, Vec<Ty>>) -> bool {
    use super::{can_coerce, is_integer};
    match (formal, actual) {
        (Ty::Param(p), t) => {
            per_param.entry(p.clone()).or_default().push(t.clone());
            true
        }
        (Ty::Array(f), Ty::Array(a)) => collect_quiet(f, a, per_param),
        (Ty::Mutable(f), Ty::Mutable(a)) => collect_quiet(f, a, per_param),
        (Ty::Array(_), Ty::Mutable(inner)) => match &**inner {
            Ty::Array(_) => collect_quiet(formal, inner, per_param),
            _ => false,
        },
        (Ty::Mutable(f), _) => collect_quiet(f, actual, per_param),
        (f, a) if f == a => true,
        (f, Ty::Int) if is_integer(f) => true,
        (f, a) if can_coerce(a, f) => true,
        _ => false,
    }
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
            HirExpr::MethodCall {
                id,
                receiver,
                type_args,
                args,
                ..
            } => {
                walk_expr(typed, receiver, out);
                for arg in args {
                    walk_expr(typed, arg, out);
                }
                // Local method target (foreign sugar resolves through the
                // world fixed point instead); the receiver counts as the
                // first actual, mirroring the checker's combined arity.
                if let Some(d) = typed.method_defs.get(&id.0) {
                    let mut actuals = vec![expr_ty(typed, receiver)];
                    actuals.extend(args.iter().map(|a| expr_ty(typed, a)));
                    out.push((id.0, *d, type_args.clone(), actuals));
                }
            }
            HirExpr::ArrayLiteral { elems, .. } => {
                for elem in elems {
                    walk_expr(typed, elem, out);
                }
            }
            HirExpr::ObjectLiteral { fields, .. } => {
                for (_, value) in fields {
                    walk_expr(typed, value, out);
                }
            }
            HirExpr::Index { base, index, .. } => {
                walk_expr(typed, base, out);
                walk_expr(typed, index, out);
            }
            HirExpr::TupleLiteral { elems, .. } => {
                for (_, value) in elems {
                    walk_expr(typed, value, out);
                }
            }
            HirExpr::TupleIndex { base, .. } => walk_expr(typed, base, out),
            HirExpr::Field { base, .. } => walk_expr(typed, base, out),
            HirExpr::Binary { lhs, rhs, .. } => {
                walk_expr(typed, lhs, out);
                walk_expr(typed, rhs, out);
            }
            HirExpr::Unary { inner, .. } => walk_expr(typed, inner, out),
            HirExpr::Cast { inner, .. } => walk_expr(typed, inner, out),
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
            HirStmt::FieldAssign { base, value, .. } => {
                walk_expr(typed, base, out);
                walk_expr(typed, value, out);
            }
            HirStmt::TupleAssign { base, value, .. } => {
                walk_expr(typed, base, out);
                walk_expr(typed, value, out);
            }
            HirStmt::Destructure { value, .. } => walk_expr(typed, value, out),
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
