//! Project-wide monomorphization fixed point.
//!
//! The local [`crate::mono::expand`] worklist only sees one `HirProgram`.
//! Cross-module generics need a module-aware operation over immutable checked
//! modules: every source template is indexed by [`TemplateKey`], concrete
//! requests are seeded from all callers, and the queue discovers transitive
//! generic-to-generic calls across any mix of modules until it converges or
//! hits the instantiation budget.
//!
//! The result is a [`MonomorphizationPlan`] shared by `check` and `build`:
//! instances grouped by owner module, root targets by caller, and nested
//! targets by outer instance. LIR emits each instance in its owner and rewrites
//! callers to concrete imports. No `Ty::Param`, template, or unmangled generic
//! import reaches lowering.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use vl_common::{Diagnostic, Span, VlType};
use vl_hir::{HirExpr, HirItem, HirProgram, HirStmt};

use super::{
    bound_satisfied, canonicalize_for_key, is_capability_valid, subst_ty, ty_has_error, FuncSigTy,
    Instance, InstanceKey, TemplateKey, Ty, TypedProgram,
};
use crate::mono::MAX_INSTANCES;

/// Concrete call target: the requested instance key. The emitted symbol
/// (`owner::mangled`) is derived from this key only at the LIR boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConcreteTarget {
    pub key: InstanceKey,
}

impl ConcreteTarget {
    pub fn new(key: InstanceKey) -> Self {
        Self { key }
    }

    pub fn module(&self) -> &str {
        &self.key.template.module
    }

    pub fn mangled(&self) -> String {
        self.key.mangled()
    }
}

/// Module-aware monomorphization result.
#[derive(Debug, Default, Clone)]
pub struct MonomorphizationPlan {
    /// Owner module -> structural instance key -> concrete instance.
    /// The emitted symbol is derived via `key.mangled()` at lowering.
    pub instances_by_owner: BTreeMap<String, BTreeMap<InstanceKey, Instance>>,
    /// (caller module, call `HirId.0`) -> concrete target for calls in
    /// monomorphic code and global initializers.
    pub root_targets: BTreeMap<(String, u32), ConcreteTarget>,
    /// (outer instance, call `HirId.0`) -> concrete target for calls inside
    /// generic templates, resolved under the outer substitution.
    pub nested_targets: HashMap<(InstanceKey, u32), ConcreteTarget>,
}

struct TemplateInfo {
    owner_idx: usize,
    def: u32,
    sig: FuncSigTy,
    span: Span,
}

fn instance_name(prog: &HirProgram, def: u32) -> Option<String> {
    prog.items.iter().find_map(|item| match item {
        HirItem::Fn {
            def: Some(d), name, ..
        } if d.0 == def => Some(name.clone()),
        _ => None,
    })
}

pub(super) fn vl_in_instance(v: &VlType, env: &HashMap<String, Ty>) -> Ty {
    match v {
        VlType::Param(n) => env.get(n).cloned().unwrap_or(Ty::Param(n.clone())),
        VlType::Array(t) => Ty::Array(Box::new(vl_in_instance(t, env))),
        VlType::Mutable(t) => Ty::Mutable(Box::new(vl_in_instance(t, env))),
        VlType::Tuple(fs) => Ty::Tuple(
            fs.iter()
                .map(|f| (f.name.clone(), vl_in_instance(&f.ty, env)))
                .collect(),
        ),
        VlType::Object { name, args } => Ty::Object(Box::new(super::ObjectTy {
            name: name.clone(),
            args: args.iter().map(|t| vl_in_instance(t, env)).collect(),
        })),
        VlType::Union { name, args } => Ty::Union(Box::new(super::UnionTy {
            name: name.clone(),
            args: args.iter().map(|t| vl_in_instance(t, env)).collect(),
        })),
        VlType::Nullable(t) => Ty::Union(Box::new(super::UnionTy {
            name: "Option".into(),
            args: vec![vl_in_instance(t, env)],
        })),
        VlType::Fallible { err, ok } => Ty::Fallible(Box::new(super::FallibleTy {
            err: err.clone(),
            ok: vl_in_instance(ok, env),
        })),
        _ => Ty::from_vl(v),
    }
}

/// Check invariants that only become visible once a generic function is
/// instantiated. This is shared by local and project-wide expansion so local
/// and imported templates enforce the same capability rules.
pub(super) fn specialization_diagnostic(
    prog: &HirProgram,
    typed: &TypedProgram,
    def: u32,
    sig: &FuncSigTy,
    args: &[Ty],
) -> Option<Diagnostic> {
    let span = function_span(prog, def);
    let name = instance_name(prog, def).unwrap_or_else(|| format!("def#{def}"));

    if let Some(readonly) = typed.readonly_return_params.get(&def) {
        if sig
            .type_params
            .iter()
            .zip(args)
            .any(|(param, arg)| readonly.contains(param) && arg.is_mutable_view())
        {
            return Some(
                Diagnostic::error(format!(
                    "generic `{name}` returns a read-only projection as `{}`",
                    sig.ret
                ))
                .with_label(span, "this return loses mutable capability")
                .with_code("E302"),
            );
        }
    }

    if let Some(required) = typed.readonly_mutable_call_params.get(&def) {
        if let Some((param, arg)) = sig
            .type_params
            .iter()
            .zip(args)
            .find(|(param, arg)| required.contains(*param) && arg.is_mutable_view())
        {
            return Some(
                Diagnostic::error(format!(
                    "generic `{name}` passes read-only `{param}` to a mutable parameter, but `{param}` specializes to `{arg}`"
                ))
                .with_label(span, "this specialization would restore mutation authority")
                .with_code("E106"),
            );
        }
    }

    let env: HashMap<String, Ty> = sig
        .type_params
        .iter()
        .cloned()
        .zip(args.iter().cloned())
        .collect();
    let (params, ret) = sig.instantiate(args);
    let invalid = params
        .iter()
        .chain(std::iter::once(&ret))
        .find(|ty| has_mutable_nominal_argument(ty))
        .cloned()
        .or_else(|| {
            super::template_ids_for(prog, def)
                .into_iter()
                .find_map(|id| {
                    typed
                        .types
                        .get(&id)
                        .map(|ty| subst_ty(ty, &env))
                        .filter(has_mutable_nominal_argument)
                })
        });
    invalid.map(|ty| {
        Diagnostic::error(format!(
            "generic `{name}` specializes to nominal type `{ty}` with mutable type arguments"
        ))
        .with_label(span, "nominal type arguments cannot contain mutable views")
        .with_code("E106")
    })
}

fn has_mutable_nominal_argument(ty: &Ty) -> bool {
    match ty {
        Ty::Object(o) => {
            o.args.iter().any(super::ty_contains_mutable)
                || o.args.iter().any(has_mutable_nominal_argument)
        }
        Ty::Union(u) => {
            u.args.iter().any(super::ty_contains_mutable)
                || u.args.iter().any(has_mutable_nominal_argument)
        }
        Ty::Array(inner) | Ty::Mutable(inner) => has_mutable_nominal_argument(inner),
        Ty::Tuple(fields) => fields
            .iter()
            .any(|(_, field)| has_mutable_nominal_argument(field)),
        Ty::Fallible(f) => has_mutable_nominal_argument(&f.ok),
        _ => false,
    }
}

fn function_span(prog: &HirProgram, def: u32) -> Span {
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
        (Ty::Union(fu), Ty::Union(au)) if fu.name == au.name && fu.args.len() == au.args.len() => {
            for (f, a) in fu.args.iter().zip(au.args.iter()) {
                if !collect_quiet(f, a, per_param) {
                    return false;
                }
            }
            true
        }
        (Ty::Object(fo), Ty::Object(ao))
            if fo.name == ao.name && fo.args.len() == ao.args.len() =>
        {
            for (f, a) in fo.args.iter().zip(ao.args.iter()) {
                if !collect_quiet(f, a, per_param) {
                    return false;
                }
            }
            true
        }
        (Ty::Tuple(fs), Ty::Tuple(as_)) if fs.len() == as_.len() => {
            for ((_, f), (_, a)) in fs.iter().zip(as_.iter()) {
                if !collect_quiet(f, a, per_param) {
                    return false;
                }
            }
            true
        }
        (Ty::Mutable(f), Ty::Mutable(a)) => collect_quiet(f, a, per_param),
        (Ty::Array(_), Ty::Mutable(inner)) => match &**inner {
            Ty::Array(_) => collect_quiet(formal, inner, per_param),
            _ => false,
        },
        (Ty::Object(fo), Ty::Mutable(inner)) => match &**inner {
            Ty::Object(ao) if fo.name == ao.name && fo.args.len() == ao.args.len() => {
                collect_quiet(formal, inner, per_param)
            }
            _ => false,
        },
        (Ty::Union(fu), Ty::Mutable(inner)) => match &**inner {
            Ty::Union(au) if fu.name == au.name && fu.args.len() == au.args.len() => {
                collect_quiet(formal, inner, per_param)
            }
            _ => false,
        },
        (Ty::Mutable(f), _) => collect_quiet(f, actual, per_param),
        (f, a) if f == a => true,
        (f, Ty::Int) if is_integer(f) => true,
        (f, a) if can_coerce(a, f) => true,
        _ => false,
    }
}

struct NestedCall {
    call_id: u32,
    callee: TemplateKey,
    type_args: Vec<VlType>,
    actuals: Vec<Ty>,
}

fn calls_in_template(prog: &HirProgram, typed: &TypedProgram, def: u32) -> Vec<NestedCall> {
    fn expr_ty(typed: &TypedProgram, e: &HirExpr) -> Ty {
        typed.type_of_id(e.id()).unwrap_or(Ty::Error)
    }
    fn walk_expr(prog: &HirProgram, typed: &TypedProgram, e: &HirExpr, out: &mut Vec<NestedCall>) {
        match e {
            HirExpr::Call {
                id,
                def,
                symbol,
                type_args,
                args,
                ..
            } => {
                for arg in args {
                    walk_expr(prog, typed, arg, out);
                }
                // Callee identity: imported calls carry a stable `SymbolRef`
                // (the cross-module template identity); local calls resolve
                // via `DefId` to a function name in this module.
                if let Some(sym) = symbol {
                    let key = TemplateKey::new(sym.module.as_string(), sym.name.clone());
                    let actuals = args.iter().map(|a| expr_ty(typed, a)).collect();
                    out.push(NestedCall {
                        call_id: id.0,
                        callee: key,
                        type_args: type_args.clone(),
                        actuals,
                    });
                } else if let Some(d) = def {
                    if let Some(name) = instance_name(prog, d.0) {
                        let key = TemplateKey::new(prog.module.clone(), name);
                        let actuals = args.iter().map(|a| expr_ty(typed, a)).collect();
                        out.push(NestedCall {
                            call_id: id.0,
                            callee: key,
                            type_args: type_args.clone(),
                            actuals,
                        });
                    }
                }
            }
            HirExpr::MethodCall {
                id,
                receiver,
                type_args,
                args,
                ..
            } => {
                walk_expr(prog, typed, receiver, out);
                for arg in args {
                    walk_expr(prog, typed, arg, out);
                }
                // Sugar callee identity mirrors `Call`: local methods resolve
                // via the recorded template def, foreign ones via the
                // recorded provider symbol. The receiver counts as the first
                // actual, mirroring the checker's combined arity.
                let mut actuals = vec![expr_ty(typed, receiver)];
                actuals.extend(args.iter().map(|a| expr_ty(typed, a)));
                if let Some(d) = typed.method_defs.get(&id.0) {
                    if let Some(name) = instance_name(prog, *d) {
                        out.push(NestedCall {
                            call_id: id.0,
                            callee: TemplateKey::new(prog.module.clone(), name),
                            type_args: type_args.clone(),
                            actuals,
                        });
                    }
                } else if let Some(sym) = typed.method_symbols.get(&id.0) {
                    out.push(NestedCall {
                        call_id: id.0,
                        callee: TemplateKey::new(sym.module.as_string(), sym.name.clone()),
                        type_args: type_args.clone(),
                        actuals,
                    });
                }
            }
            HirExpr::ArrayLiteral { elems, .. } => {
                for elem in elems {
                    walk_expr(prog, typed, elem, out);
                }
            }
            HirExpr::ObjectLiteral { fields, .. } => {
                for (_, value) in fields {
                    walk_expr(prog, typed, value, out);
                }
            }
            HirExpr::Variant { args, .. } => {
                for arg in args {
                    walk_expr(prog, typed, arg, out);
                }
            }
            HirExpr::TupleLiteral { elems, .. } => {
                for (_, value) in elems {
                    walk_expr(prog, typed, value, out);
                }
            }
            HirExpr::TupleIndex { base, .. } => walk_expr(prog, typed, base, out),
            HirExpr::Index { base, index, .. } => {
                walk_expr(prog, typed, base, out);
                walk_expr(prog, typed, index, out);
            }
            HirExpr::Field { base, .. } => walk_expr(prog, typed, base, out),
            HirExpr::Binary { lhs, rhs, .. } => {
                walk_expr(prog, typed, lhs, out);
                walk_expr(prog, typed, rhs, out);
            }
            HirExpr::Unary { inner, .. } => walk_expr(prog, typed, inner, out),
            HirExpr::Try { inner, .. } => walk_expr(prog, typed, inner, out),
            HirExpr::Catch { lhs, fallback, .. } => {
                walk_expr(prog, typed, lhs, out);
                walk_expr(prog, typed, fallback, out);
            }
            HirExpr::Cast { inner, .. } => walk_expr(prog, typed, inner, out),
            HirExpr::ErrorValue { args, .. } => {
                for a in args {
                    walk_expr(prog, typed, a, out);
                }
            }
            HirExpr::Literal { .. }
            | HirExpr::String { .. }
            | HirExpr::Null { .. }
            | HirExpr::Var { .. } => {}
        }
    }
    fn walk_stmt(prog: &HirProgram, typed: &TypedProgram, s: &HirStmt, out: &mut Vec<NestedCall>) {
        match s {
            HirStmt::Let { value, .. } | HirStmt::Assign { value, .. } => {
                walk_expr(prog, typed, value, out)
            }
            HirStmt::Expr(e) => walk_expr(prog, typed, e, out),
            HirStmt::Return { value, .. } => {
                if let Some(e) = value {
                    walk_expr(prog, typed, e, out);
                }
            }
            HirStmt::IndexAssign {
                array,
                index,
                value,
                ..
            } => {
                walk_expr(prog, typed, array, out);
                walk_expr(prog, typed, index, out);
                walk_expr(prog, typed, value, out);
            }
            HirStmt::FieldAssign { base, value, .. } => {
                walk_expr(prog, typed, base, out);
                walk_expr(prog, typed, value, out);
            }
            HirStmt::TupleAssign { base, value, .. } => {
                walk_expr(prog, typed, base, out);
                walk_expr(prog, typed, value, out);
            }
            HirStmt::Destructure { value, .. } => walk_expr(prog, typed, value, out),
            HirStmt::If {
                condition,
                then_body,
                else_body,
                ..
            } => {
                walk_expr(prog, typed, condition, out);
                for s in then_body {
                    walk_stmt(prog, typed, s, out);
                }
                if let Some(body) = else_body {
                    for s in body {
                        walk_stmt(prog, typed, s, out);
                    }
                }
            }
            HirStmt::Match {
                scrutinee,
                arms,
                else_body,
                ..
            } => {
                walk_expr(prog, typed, scrutinee, out);
                for arm in arms {
                    for s in &arm.body {
                        walk_stmt(prog, typed, s, out);
                    }
                }
                if let Some(body) = else_body {
                    for s in body {
                        walk_stmt(prog, typed, s, out);
                    }
                }
            }
            HirStmt::While {
                condition, body, ..
            } => {
                walk_expr(prog, typed, condition, out);
                for s in body {
                    walk_stmt(prog, typed, s, out);
                }
            }
            HirStmt::Defer { inner, .. } | HirStmt::ErrDefer { inner, .. } => {
                walk_stmt(prog, typed, inner, out);
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
            walk_stmt(prog, typed, s, &mut out);
        }
        // Global initializers cannot contain generic template bodies, but
        // calls inside them are roots (handled via pending lists), not nested.
    }
    out
}

/// Run the project-wide fixed point over immutable checked modules.
///
/// `modules` holds one `(HirProgram, TypedProgram)` per project source module
/// (each already checked definitionally, with local `mono::expand` instances
/// plus `pending_imported`/`imported_root_calls` for cross-module requests).
/// Returns the shared plan plus diagnostics grouped by owner module name
/// (bare `Span`s are insufficient to choose a project source file).
pub fn plan_world(
    modules: &[(&HirProgram, &TypedProgram)],
) -> (MonomorphizationPlan, Vec<(String, Diagnostic)>) {
    let mut plan = MonomorphizationPlan::default();
    let mut diags: Vec<(String, Diagnostic)> = Vec::new();

    // Index every source template by canonical key.
    let mut index: BTreeMap<TemplateKey, TemplateInfo> = BTreeMap::new();
    for (idx, (hir, typed)) in modules.iter().enumerate() {
        for item in &hir.items {
            if let HirItem::Fn {
                def: Some(d),
                name,
                type_params,
                span,
                ..
            } = item
            {
                if type_params.is_empty() {
                    continue;
                }
                let Some(sig) = typed.func_sigs.get(&d.0).cloned() else {
                    continue;
                };
                // Poisoned templates never expand (definition already reported).
                if ty_has_error(&sig.ret) || sig.param_tys.iter().any(ty_has_error) {
                    continue;
                }
                let key = TemplateKey::new(hir.module.clone(), name.clone());
                index.entry(key).or_insert(TemplateInfo {
                    owner_idx: idx,
                    def: d.0,
                    sig,
                    span: *span,
                });
            }
        }
    }

    // Seed the queue with concrete root requests from all modules, in sorted
    // order so LIR and diagnostics do not depend on filesystem order.
    let mut seeds: BTreeSet<InstanceKey> = BTreeSet::new();
    for (hir, typed) in modules {
        // Local roots: reconstruct InstanceKey from recorded instances.
        // Bare local objects canonicalize to owner-qualified identity so the
        // same instance requested locally and remotely deduplicates.
        for (call_id, mangled) in &typed.root_calls {
            if let Some(inst) = typed.instances.get(mangled) {
                if let Some(name) = instance_name(hir, inst.orig) {
                    let canonical: Vec<Ty> = inst
                        .args
                        .iter()
                        .map(|t| canonicalize_for_key(t, &hir.module, &typed.objects))
                        .collect();
                    let key =
                        InstanceKey::new(TemplateKey::new(hir.module.clone(), name), canonical);
                    seeds.insert(key.clone());
                    plan.root_targets
                        .insert((hir.module.clone(), *call_id), ConcreteTarget::new(key));
                }
            }
        }
        // Imported roots: already canonicalized at the call site.
        for (call_id, key) in &typed.imported_root_calls {
            seeds.insert(key.clone());
            plan.root_targets.insert(
                (hir.module.clone(), *call_id),
                ConcreteTarget::new(key.clone()),
            );
        }
    }

    let mut visited: HashSet<InstanceKey> = HashSet::new();
    let mut queue: BTreeSet<InstanceKey> = seeds;

    while let Some(key) = queue.iter().next().cloned() {
        queue.remove(&key);
        if visited.contains(&key) {
            continue;
        }
        if visited.len() >= MAX_INSTANCES {
            // Expanding recursion: report once at the owner template, grouped
            // by owner module for the driver to emit with the right source.
            // Includes the budget, current count, and canonical template key.
            let message = format!(
                "generic instantiation limit exceeded ({} of {} instances, at `{}`; possible polymorphic recursion)",
                visited.len(),
                MAX_INSTANCES,
                key.template,
            );
            if let Some(info) = index.get(&key.template) {
                let owner = modules[info.owner_idx].0.module.clone();
                diags.push((
                    owner,
                    Diagnostic::error(message)
                        .with_label(info.span, "recursive instantiations keep growing here")
                        .with_note(
                            "avoid calls that wrap a type parameter in a larger type (`grow([x])`)",
                        )
                        .with_code("E303"),
                ));
            } else {
                diags.push((
                    key.template.module.clone(),
                    Diagnostic::error(message).with_code("E303"),
                ));
            }
            break;
        }
        let Some(info) = index.get(&key.template) else {
            // Unknown template (e.g. stdlib, handled separately): skip quietly.
            // Call-site checking already validated the call.
            continue;
        };
        if key.args.iter().any(ty_has_error) {
            continue;
        }
        if key.args.len() != info.sig.type_params.len() || !key.args.iter().all(|t| t.is_concrete())
        {
            continue;
        }
        // Bounds were enforced with diagnostics while checking (both for
        // concrete calls and for `Param` forwarding by implication); a
        // concrete violation here means the outer template was already
        // rejected, so stay quiet.
        {
            let mut ok = true;
            for (param, arg) in info.sig.type_params.iter().zip(key.args.iter()) {
                if let Some(bound) = info.sig.bounds.get(param) {
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
        visited.insert(key.clone());
        if let Some(diagnostic) = specialization_diagnostic(
            modules[info.owner_idx].0,
            modules[info.owner_idx].1,
            info.def,
            &info.sig,
            &key.args,
        ) {
            diags.push((key.template.module.clone(), diagnostic));
            continue;
        }
        let (param_tys, ret_ty) = info.sig.instantiate(&key.args);
        if param_tys.iter().any(ty_has_error) || ty_has_error(&ret_ty) {
            continue;
        }
        plan.instances_by_owner
            .entry(key.template.module.clone())
            .or_default()
            .entry(key.clone())
            .or_insert(Instance {
                orig: info.def,
                args: key.args.clone(),
                sig: FuncSigTy {
                    param_names: info.sig.param_names.clone(),
                    param_tys,
                    ret: ret_ty,
                    type_params: Vec::new(),
                    bounds: HashMap::new(),
                },
            });

        // Walk calls in this instance's body under its substitution.
        let (owner_hir, owner_typed) = modules[info.owner_idx];
        let env: HashMap<String, Ty> = info
            .sig
            .type_params
            .iter()
            .cloned()
            .zip(key.args.iter().cloned())
            .collect();
        for nested in calls_in_template(owner_hir, owner_typed, info.def) {
            let Some(callee_info) = index.get(&nested.callee) else {
                continue;
            };
            if ty_has_error(&callee_info.sig.ret)
                || callee_info.sig.param_tys.iter().any(ty_has_error)
            {
                continue;
            }
            let actuals: Vec<Ty> = nested.actuals.iter().map(|t| subst_ty(t, &env)).collect();
            if actuals.iter().any(ty_has_error) {
                continue;
            }
            let resolved: Option<Vec<Ty>> = if nested.type_args.is_empty() {
                infer_quiet(&callee_info.sig, &actuals)
            } else {
                let mut out = Vec::with_capacity(nested.type_args.len());
                let mut ok = true;
                for v in &nested.type_args {
                    let t = vl_in_instance(v, &env);
                    if !t.is_concrete() || !is_capability_valid(&t) {
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
            if resolved.len() != callee_info.sig.type_params.len()
                || !resolved.iter().all(|t| t.is_concrete())
            {
                continue;
            }
            {
                let mut ok = true;
                for (param, arg) in callee_info.sig.type_params.iter().zip(resolved.iter()) {
                    if let Some(bound) = callee_info.sig.bounds.get(param) {
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
            // Canonicalize nominal objects with the outer (caller) module so
            // bare locals match qualified remotes for the same logical type.
            let outer_objects = modules
                .iter()
                .find(|(h, _)| h.module == key.template.module)
                .map(|(_, t)| &t.objects);
            let canonical: Vec<Ty> = match outer_objects {
                Some(objects) => resolved
                    .iter()
                    .map(|t| canonicalize_for_key(t, &key.template.module, objects))
                    .collect(),
                None => resolved.clone(),
            };
            let nested_key = InstanceKey::new(nested.callee.clone(), canonical);
            let target = ConcreteTarget::new(nested_key.clone());
            plan.nested_targets
                .entry((key.clone(), nested.call_id))
                .or_insert(target);
            if !visited.contains(&nested_key) && !queue.contains(&nested_key) {
                // Only queue if the instance isn't already emitted (deduplicate
                // across callers: the owner emits once).
                let already_emitted = plan
                    .instances_by_owner
                    .get(&nested.callee.module)
                    .is_some_and(|m| m.contains_key(&nested_key));
                if !already_emitted {
                    queue.insert(nested_key);
                }
            }
        }
    }

    (plan, diags)
}

/// Validate every planned instance: signatures, arguments, and substituted
/// body types must be normalized (no `Int`, `Param`, or nested `Error`).
/// Generic templates legitimately contain `Param`; instances must not after
/// substitution. When `prior` already holds errors, lowering is blocked anyway
/// so validation stands down (mirrors [`TypedProgram::validate_normalized`]).
/// Returns diagnostics grouped by owner module.
pub fn validate_plan(
    plan: &MonomorphizationPlan,
    modules: &[(&HirProgram, &TypedProgram)],
    prior: &[Diagnostic],
) -> Vec<(String, Diagnostic)> {
    if prior.iter().any(|d| d.is_error()) {
        return Vec::new();
    }
    let mut out = Vec::new();
    // Owner HIR/typed lookup for body validation.
    let find_owner = |owner: &str| modules.iter().find(|(h, _)| h.module == owner);
    for (owner, instances) in &plan.instances_by_owner {
        let Some((hir, typed)) = find_owner(owner) else {
            continue;
        };
        for (key, inst) in instances {
            let mangled = key.mangled();
            for ty in inst
                .sig
                .param_tys
                .iter()
                .chain(std::iter::once(&inst.sig.ret))
            {
                if ty_has_error(ty) {
                    out.push((
                        owner.clone(),
                        Diagnostic::error(format!(
                            "instance `{mangled}` is poisoned without a prior error (compiler bug)"
                        ))
                        .with_code("E500"),
                    ));
                } else if !ty.is_concrete() {
                    out.push((
                        owner.clone(),
                        Diagnostic::error(format!(
                            "instance `{mangled}` has non-normalized type `{ty}` (compiler bug)"
                        ))
                        .with_note(
                            "unresolved `int`/`Param` must be defaulted or monomorphized before lowering",
                        )
                        .with_code("E500"),
                    ));
                }
            }
            for arg in &inst.args {
                if ty_has_error(arg) {
                    out.push((
                        owner.clone(),
                        Diagnostic::error(format!(
                            "instance `{mangled}` argument is poisoned without a prior error (compiler bug)"
                        ))
                        .with_code("E500"),
                    ));
                } else if !arg.is_concrete() {
                    out.push((
                        owner.clone(),
                        Diagnostic::error(format!(
                            "instance `{mangled}` argument has non-normalized type `{arg}` (compiler bug)"
                        ))
                        .with_code("E500"),
                    ));
                }
            }
            // Substituted body types must also be normalized.
            let env: HashMap<String, Ty> = hir
                .items
                .iter()
                .find_map(|item| match item {
                    HirItem::Fn {
                        def: Some(d),
                        type_params,
                        ..
                    } if d.0 == inst.orig => Some(
                        type_params
                            .iter()
                            .map(|p| p.name.clone())
                            .zip(inst.args.iter().cloned())
                            .collect(),
                    ),
                    _ => None,
                })
                .unwrap_or_default();
            for id in super::template_ids_for(hir, inst.orig) {
                if let Some(ty) = typed.types.get(&id) {
                    let substed = subst_ty(ty, &env);
                    if ty_has_error(&substed) {
                        out.push((
                            owner.clone(),
                            Diagnostic::error(format!(
                                "instance `{mangled}` node `{id}` is poisoned without a prior error (compiler bug)"
                            ))
                            .with_code("E500"),
                        ));
                    } else if !substed.is_concrete() {
                        out.push((
                            owner.clone(),
                            Diagnostic::error(format!(
                                "instance `{mangled}` node `{id}` has non-normalized type `{substed}` (compiler bug)"
                            ))
                            .with_code("E500"),
                        ));
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod specialization_tests {
    use super::*;
    use crate::{ObjectTy, UnionTy};

    fn mutable_foo() -> Ty {
        Ty::Mutable(Box::new(Ty::Object(Box::new(ObjectTy {
            name: "Foo".into(),
            args: Vec::new(),
        }))))
    }

    #[test]
    fn deferred_readonly_mutable_call_is_rejected_at_concrete_instance() {
        let prog = HirProgram {
            module: "test".into(),
            items: Vec::new(),
        };
        let mut typed = TypedProgram::default();
        typed
            .readonly_mutable_call_params
            .insert(7, ["T".to_string()].into_iter().collect());
        let sig = FuncSigTy {
            param_names: vec!["x".into()],
            param_tys: vec![Ty::Param("T".into())],
            ret: Ty::Param("T".into()),
            type_params: vec!["T".into()],
            bounds: HashMap::new(),
        };

        let diagnostic = specialization_diagnostic(&prog, &typed, 7, &sig, &[mutable_foo()])
            .expect("mutable specialization must not restore authority");
        assert_eq!(diagnostic.code.as_deref(), Some("E106"));
        assert!(specialization_diagnostic(&prog, &typed, 7, &sig, &[Ty::U64]).is_none());
    }

    #[test]
    fn nominal_argument_validation_recurses_through_composites() {
        let invalid = Ty::Array(Box::new(Ty::Tuple(vec![(
            None,
            Ty::Fallible(Box::new(crate::FallibleTy {
                err: None,
                ok: Ty::Union(Box::new(UnionTy {
                    name: "Box".into(),
                    args: vec![Ty::Array(Box::new(mutable_foo()))],
                })),
            })),
        )])));
        assert!(has_mutable_nominal_argument(&invalid));
        assert!(!has_mutable_nominal_argument(&Ty::Array(Box::new(
            mutable_foo()
        ))));
    }
}
