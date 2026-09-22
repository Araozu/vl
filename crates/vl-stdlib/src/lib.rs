//! vl-stdlib: the embedded VL standard library.
//!
//! Real modules, not a prelude: `src/std/string.vl` is module `std.string`,
//! `src/std/math.vl` is `std.math`, `src/std/fmt.vl` is `std.fmt`. Nothing
//! is available without an explicit `use`; there are no implicit globals.
//!
//! Each module view is a *merge* of two owners:
//!
//! - thin externs for the VM natives (`string.concat`, `math.mod_u64`, …),
//!   declared in `vl_codegen::modules` and emitted as native `calli`;
//! - VL helpers (`math.max_u64`, `string.is_empty`, …) from the embedded
//!   sources, compiled like user code.
//!
//! [`Stdlib::extend_catalog`] unions the helper exports into a module
//! catalog so `use std.math;` resolves both owners at once. [`Stdlib::link`]
//! then splices the pre-lowered bodies of referenced helpers into the user
//! `LirProgram` as locals (rewriting their calls), so one artifact carries
//! everything — no provider artifacts, no cross-artifact calls. Unreferenced
//! helpers stay out, keeping programs that use nothing byte-identical.
//!
//! Special-case rules, enforced at [`load`]:
//!
//! - a helper name colliding with an extern export in the same module is a
//!   stdlib bug (rejected, not shadowed);
//! - helpers may only call other helpers or naravm-emittable externs, so a
//!   linked program can never smuggle an unemittable call past resolution;
//! - helpers have no globals and no `main` (they may be generic; concrete
//!   instances lower per compilation with the instance plan).

use std::collections::{HashMap, HashSet};

use vl_common::ModuleSpec;
use vl_lir::{Function, FunctionImport, Instr, LirProgram};

/// One embedded module: its canonical path plus its VL source.
struct EmbeddedModule {
    path: &'static str,
    src: &'static str,
}

const EMBEDDED: &[EmbeddedModule] = &[
    EmbeddedModule {
        path: "std.string",
        src: include_str!("std/string.vl"),
    },
    EmbeddedModule {
        path: "std.math",
        src: include_str!("std/math.vl"),
    },
    EmbeddedModule {
        path: "std.fmt",
        src: include_str!("std/fmt.vl"),
    },
];

/// Mangle a qualified helper into a local function name. `$` is unlexable
/// in VL source (like the generic-instance mangling), so this can never
/// collide with a user definition.
pub fn mangle(module: &str, function: &str) -> String {
    format!("{}${}", module.replace('.', "$"), function)
}

/// Loaded standard library: pre-lowered helper bodies plus their catalog
/// specs. Build once with [`load`] and reuse.
pub struct Stdlib {
    /// (module, function) -> lowered body, exactly as the frontend produced.
    /// Only monomorphic helpers without plan-sensitive generic calls are
    /// stored here; generic templates live in `checked` and lower per
    /// compilation with the instance plan.
    bodies: HashMap<(String, String), Function>,
    /// Owning module -> that module's imports (extern calls its helpers
    /// need). Merged into the user program alongside copied bodies so the
    /// backend still sees every native import.
    module_imports: HashMap<String, Vec<FunctionImport>>,
    /// Helper-only catalog specs, one per embedded module.
    specs: Vec<ModuleSpec>,
    /// Immutable checked modules as the source of truth for specialization:
    /// one `(HirProgram, TypedProgram)` per embedded module, in source order.
    /// Each compilation receives its own instance set and budget; this cache
    /// is never mutated with one project's instances.
    checked: Vec<(vl_hir::HirProgram, vl_typecheck::TypedProgram)>,
}

/// Assert no errors; embedded sources are deterministic and tested.
fn expect_clean(diags: &[vl_common::Diagnostic], what: &str) {
    assert!(
        diags.iter().all(|d| !d.is_error()),
        "embedded stdlib {what} has diagnostics (impossible: covered by tests): {diags:?}"
    );
}

fn hir_expr_has_generic_call(expr: &vl_hir::HirExpr, typed: &vl_typecheck::TypedProgram) -> bool {
    match expr {
        vl_hir::HirExpr::Call {
            extern_sig, def, ..
        } => {
            if let Some(sig) = extern_sig {
                if !sig.type_params.is_empty() {
                    return true;
                }
            }
            if let Some(d) = def {
                if let Some(sig) = typed.func_sigs.get(&d.0) {
                    if !sig.type_params.is_empty() {
                        return true;
                    }
                }
            }
            // Recurse into arguments (a generic call may hide there).
            match expr {
                vl_hir::HirExpr::Call { args, .. } => {
                    args.iter().any(|a| hir_expr_has_generic_call(a, typed))
                }
                _ => false,
            }
        }
        vl_hir::HirExpr::ArrayLiteral { elems, .. } => {
            elems.iter().any(|e| hir_expr_has_generic_call(e, typed))
        }
        vl_hir::HirExpr::ObjectLiteral { fields, .. } => fields
            .iter()
            .any(|(_, v)| hir_expr_has_generic_call(v, typed)),
        vl_hir::HirExpr::Variant { args, .. } => {
            args.iter().any(|a| hir_expr_has_generic_call(a, typed))
        }
        vl_hir::HirExpr::TupleLiteral { elems, .. } => elems
            .iter()
            .any(|(_, v)| hir_expr_has_generic_call(v, typed)),
        vl_hir::HirExpr::TupleIndex { base, .. } => hir_expr_has_generic_call(base, typed),
        vl_hir::HirExpr::Index { base, index, .. } => {
            hir_expr_has_generic_call(base, typed) || hir_expr_has_generic_call(index, typed)
        }
        vl_hir::HirExpr::Field { base, .. }
        | vl_hir::HirExpr::Unary { inner: base, .. }
        | vl_hir::HirExpr::Cast { inner: base, .. } => hir_expr_has_generic_call(base, typed),
        vl_hir::HirExpr::Binary { lhs, rhs, .. } => {
            hir_expr_has_generic_call(lhs, typed) || hir_expr_has_generic_call(rhs, typed)
        }
        vl_hir::HirExpr::MethodCall {
            id, receiver, args, ..
        } => {
            // Suspended until stdlib sources declare associated functions;
            // structurally identical to `Call` when they do.
            if let Some(d) = typed.method_defs.get(&id.0) {
                if let Some(sig) = typed.func_sigs.get(d) {
                    if !sig.type_params.is_empty() {
                        return true;
                    }
                }
            }
            hir_expr_has_generic_call(receiver, typed)
                || args.iter().any(|a| hir_expr_has_generic_call(a, typed))
        }
        vl_hir::HirExpr::Literal { .. }
        | vl_hir::HirExpr::String { .. }
        | vl_hir::HirExpr::Null { .. }
        | vl_hir::HirExpr::Var { .. } => false,
    }
}

fn hir_stmt_has_generic_call(stmt: &vl_hir::HirStmt, typed: &vl_typecheck::TypedProgram) -> bool {
    match stmt {
        vl_hir::HirStmt::Let { value, .. }
        | vl_hir::HirStmt::Assign { value, .. }
        | vl_hir::HirStmt::Expr(value) => hir_expr_has_generic_call(value, typed),
        vl_hir::HirStmt::Return { value, .. } => value
            .as_ref()
            .is_some_and(|v| hir_expr_has_generic_call(v, typed)),
        vl_hir::HirStmt::IndexAssign {
            array,
            index,
            value,
            ..
        } => {
            hir_expr_has_generic_call(array, typed)
                || hir_expr_has_generic_call(index, typed)
                || hir_expr_has_generic_call(value, typed)
        }
        vl_hir::HirStmt::FieldAssign { base, value, .. } => {
            hir_expr_has_generic_call(base, typed) || hir_expr_has_generic_call(value, typed)
        }
        vl_hir::HirStmt::TupleAssign { base, value, .. } => {
            hir_expr_has_generic_call(base, typed) || hir_expr_has_generic_call(value, typed)
        }
        vl_hir::HirStmt::Destructure { value, .. } => hir_expr_has_generic_call(value, typed),
        vl_hir::HirStmt::Match {
            scrutinee,
            arms,
            else_body,
            ..
        } => {
            hir_expr_has_generic_call(scrutinee, typed)
                || arms
                    .iter()
                    .any(|arm| arm.body.iter().any(|s| hir_stmt_has_generic_call(s, typed)))
                || else_body
                    .as_ref()
                    .is_some_and(|b| b.iter().any(|s| hir_stmt_has_generic_call(s, typed)))
        }
        vl_hir::HirStmt::If {
            condition,
            then_body,
            else_body,
            ..
        } => {
            hir_expr_has_generic_call(condition, typed)
                || then_body
                    .iter()
                    .any(|s| hir_stmt_has_generic_call(s, typed))
                || else_body
                    .as_ref()
                    .is_some_and(|b| b.iter().any(|s| hir_stmt_has_generic_call(s, typed)))
        }
        vl_hir::HirStmt::While {
            condition, body, ..
        } => {
            hir_expr_has_generic_call(condition, typed)
                || body.iter().any(|s| hir_stmt_has_generic_call(s, typed))
        }
        vl_hir::HirStmt::Break { .. } | vl_hir::HirStmt::Continue { .. } => false,
    }
}

/// Lex, parse, resolve, check, and lower every embedded module. Panics on
/// any diagnostic or authoring-rule violation: the inputs are deterministic
/// and covered by tests, so failure here is a compiler bug, not user error.
///
/// Two-pass embedded project build: lex/parse all modules, collect all helper
/// interfaces before resolving any body (so cross-stdlib calls are order
/// independent), then resolve/lower/check every helper against the merged
/// catalog of target natives plus helper interfaces.
pub fn load() -> Stdlib {
    let externs = vl_codegen::modules();
    // Pass 1: lex, parse, and collect interfaces for every embedded module.
    let mut parsed: Vec<vl_syntax::Program> = Vec::with_capacity(EMBEDDED.len());
    let mut interfaces: Vec<vl_common::ModuleInterface> = Vec::with_capacity(EMBEDDED.len());
    for module in EMBEDDED {
        let (toks, ldiags) = vl_lex::lex(module.src);
        expect_clean(&ldiags, &format!("{} lex", module.path));
        let (prog, pdiags) = vl_syntax::parse_with_module(&toks, module.src, module.path);
        expect_clean(&pdiags, &format!("{} parse", module.path));
        assert!(
            !prog.items.iter().any(|item| matches!(
                item,
                vl_syntax::Item::Function { name, .. } if name == "main"
            )),
            "embedded stdlib {} must not define `main`",
            module.path
        );
        let (interface, idiags) = vl_semantic::collect_interface_quiet(&prog);
        expect_clean(&idiags, &format!("{} interface", module.path));
        assert!(
            interface.poisoned_exports.is_empty() && interface.global_dependent_exports.is_empty(),
            "embedded stdlib {} must export only clean helpers",
            module.path
        );
        parsed.push(prog);
        interfaces.push(interface);
    }
    // Merged catalog for helper bodies: target natives plus all helper
    // interfaces (so a helper may call a helper from a later file).
    let mut merged = externs.clone();
    for interface in &interfaces {
        let spec = interface.as_spec();
        match merged.iter_mut().find(|m| m.path == spec.path) {
            Some(existing) => {
                for export in &spec.exports {
                    assert!(
                        existing.lookup(&export.name).is_none(),
                        "stdlib helper `{}` collides with a `{}` export",
                        export.name,
                        spec.path.as_string(),
                    );
                    existing.exports.push(export.clone());
                }
                for obj in &spec.objects {
                    if !existing
                        .objects
                        .iter()
                        .any(|o| o.qualified == obj.qualified)
                    {
                        existing.objects.push(obj.clone());
                    }
                }
                for union in &spec.unions {
                    if !existing
                        .unions
                        .iter()
                        .any(|u| u.qualified == union.qualified)
                    {
                        existing.unions.push(union.clone());
                    }
                }
            }
            None => merged.push(spec),
        }
    }
    let mut bodies = HashMap::new();
    let mut specs = Vec::new();
    let mut checked = Vec::new();
    let mut module_imports: HashMap<String, Vec<FunctionImport>> = HashMap::new();
    for (prog, interface) in parsed.iter().zip(interfaces.iter()) {
        let (res, rdiags) = vl_semantic::resolve_with_modules(prog, &merged);
        expect_clean(&rdiags, &format!("{} resolve", prog.module));
        let hir = vl_hir::lower(prog, &res);
        let (typed, mut tdiags) = vl_typecheck::check_with_modules(&hir, &merged);
        tdiags.append(&mut typed.validate_normalized(&hir, &tdiags));
        expect_clean(&tdiags, &format!("{} typecheck", prog.module));
        let lir = vl_lir::lower(&hir, &typed);
        assert!(
            !lir.entrypoint && lir.globals.is_empty(),
            "embedded stdlib {} must be a pure function module",
            prog.module
        );
        module_imports
            .entry(prog.module.clone())
            .or_default()
            .extend(lir.imports.iter().cloned());
        // Pre-lowered monomorphic bodies are retained per function: a helper
        // whose own body contains no generic calls is cached; one calling a
        // generic (even concretely) lowers per compilation with the plan.
        // Generic templates never pre-lower (LIR skips them) and concrete
        // `$` instances are never cached process-wide.
        for f in &lir.functions {
            if f.name.contains('$') {
                continue;
            }
            let own_has_generic = hir.items.iter().any(|item| match item {
                vl_hir::HirItem::Fn { name, body, .. } if name == &f.name => {
                    body.iter().any(|s| hir_stmt_has_generic_call(s, &typed))
                }
                _ => false,
            });
            if own_has_generic {
                continue;
            }
            bodies.insert((prog.module.clone(), f.name.clone()), f.clone());
        }
        // Generic templates are never pre-lowered (LIR skips them); their
        // checked HIR/typed is the source of truth for per-compilation
        // specialization.
        checked.push((hir, typed));
        specs.push(interface.as_spec());
    }
    let stdlib = Stdlib {
        bodies,
        module_imports,
        specs,
        checked,
    };
    // Every helper import must be another helper or a naravm-emittable
    // extern. Otherwise linking could plant a call the backend cannot emit
    // (e.g. `std.fs`) without any user-visible import.
    let emittable: HashSet<(String, String)> = vl_codegen::modules_for_target("naravm")
        .iter()
        .flat_map(|m| {
            m.exports
                .iter()
                .map(|e| (m.path.as_string(), e.name.clone()))
        })
        .collect();
    for (importer, imports) in &stdlib.module_imports {
        for import in imports {
            let symbol = &import.symbol;
            assert!(
                stdlib.is_helper(&symbol.module, &symbol.function)
                    || emittable.contains(&(symbol.module.clone(), symbol.function.clone())),
                "embedded stdlib {importer} imports {symbol}, which is neither a helper nor a naravm-emittable extern",
            );
        }
    }
    stdlib
}

impl Stdlib {
    /// Union helper exports into `catalog`, merging with same-path specs
    /// (the VM externs). A helper colliding with an existing export is a
    /// stdlib bug.
    pub fn extend_catalog(&self, catalog: &mut Vec<ModuleSpec>) {
        for spec in &self.specs {
            match catalog.iter_mut().find(|m| m.path == spec.path) {
                Some(existing) => {
                    for export in &spec.exports {
                        assert!(
                            existing.lookup(&export.name).is_none(),
                            "stdlib helper `{}` collides with a `{}` export (merge them or rename)",
                            export.name,
                            spec.path.as_string(),
                        );
                        existing.exports.push(export.clone());
                    }
                    for union in &spec.unions {
                        if !existing
                            .unions
                            .iter()
                            .any(|candidate| candidate.qualified == union.qualified)
                        {
                            existing.unions.push(union.clone());
                        }
                    }
                }
                None => catalog.push(spec.clone()),
            }
        }
    }

    /// Whether `(module, function)` is a VL helper (as opposed to a VM
    /// extern, user function, or cross-module source call).
    pub fn is_helper(&self, module: &str, function: &str) -> bool {
        self.bodies
            .contains_key(&(module.to_string(), function.to_string()))
            || self.is_generic_template(module, function)
            || self.is_concrete_generic_instance(module, function)
    }

    /// Whether `(module, function)` names a generic template (unmangled).
    fn is_generic_template(&self, module: &str, function: &str) -> bool {
        // `function` without `$` (mangled instances contain `$`).
        if function.contains('$') {
            return false;
        }
        self.checked.iter().any(|(hir, typed)| {
            hir.module == module
                && typed.func_sigs.iter().any(|(def, sig)| {
                    !sig.type_params.is_empty()
                        && hir.items.iter().any(|item| match item {
                            vl_hir::HirItem::Fn {
                                def: Some(d), name, ..
                            } => d.0 == *def && name == function,
                            _ => false,
                        })
                })
        })
    }

    /// Whether `(module, function)` is a concrete generic instance
    /// (`max$u64`): its base (before `$`) is a generic template in this module.
    fn is_concrete_generic_instance(&self, module: &str, function: &str) -> bool {
        let Some((base, _)) = function.split_once('$') else {
            return false;
        };
        self.is_generic_template(module, base)
    }

    /// Whether `(module, function)` is a plan-sensitive monomorphic helper:
    /// a monomorphic `Fn` exists in checked HIR but was not cached (its own
    /// body calls a generic, so it lowers per compilation with the plan).
    fn is_plan_mono(&self, module: &str, function: &str) -> bool {
        if function.contains('$') {
            return false;
        }
        if self
            .bodies
            .contains_key(&(module.to_string(), function.to_string()))
        {
            return false;
        }
        self.checked.iter().any(|(hir, typed)| {
            hir.module == module
                && hir.items.iter().any(|item| match item {
                    vl_hir::HirItem::Fn {
                        def: Some(d),
                        name,
                        type_params,
                        ..
                    } => {
                        name == function
                            && type_params.is_empty()
                            && typed.func_sigs.contains_key(&d.0)
                    }
                    _ => false,
                })
        })
    }

    /// Immutable checked modules for the per-compilation fixed point.
    pub fn checked_modules(&self) -> &[(vl_hir::HirProgram, vl_typecheck::TypedProgram)] {
        &self.checked
    }

    /// Helper names exported by one embedded module, sorted.
    /// Includes both monomorphic helpers and generic templates.
    pub fn helper_names(&self, module: &str) -> Vec<String> {
        let mut names: Vec<String> = self
            .bodies
            .keys()
            .filter(|(m, _)| m == module)
            .map(|(_, f)| f.clone())
            .collect();
        for (hir, typed) in &self.checked {
            if hir.module != module {
                continue;
            }
            for item in &hir.items {
                if let vl_hir::HirItem::Fn {
                    def: Some(d), name, ..
                } = item
                {
                    if let Some(sig) = typed.func_sigs.get(&d.0) {
                        if !sig.type_params.is_empty() && !names.contains(name) {
                            names.push(name.clone());
                        }
                    }
                }
            }
        }
        names.sort();
        names
    }

    /// Splice referenced helper bodies into `prog` as locals, rewriting
    /// their calls (including transitive helper-to-helper calls and calls
    /// in global initializers). Extern imports pass through untouched for
    /// the backend's native path.
    pub fn link(&self, prog: &mut LirProgram) {
        let mut copied: HashMap<(String, String), String> = HashMap::new();
        loop {
            let mut needed: Vec<(String, String)> = Vec::new();
            let mut scan = |instrs: &[Instr]| {
                for ins in instrs {
                    if let Instr::Call { callee, .. } = ins {
                        let key = (callee.module.clone(), callee.function.clone());
                        // Monomorphic cached bodies only; concrete generic
                        // instances (`max$u64`) are handled by `link_with_plan`.
                        if self.bodies.contains_key(&key)
                            && !copied.contains_key(&key)
                            && !needed.contains(&key)
                        {
                            needed.push(key);
                        }
                    }
                }
            };
            for f in &prog.functions {
                scan(&f.instrs);
            }
            for g in &prog.globals {
                scan(&g.init);
            }
            if needed.is_empty() {
                break;
            }
            needed.sort();
            for (module, function) in needed {
                let mangled = mangle(&module, &function);
                let body = self
                    .bodies
                    .get(&(module.clone(), function.clone()))
                    .unwrap_or_else(|| {
                        panic!(
                            "stdlib helper {module}::{function} referenced but not lowered (impossible: built from the same sources)"
                        )
                    });
                let mut copy = body.clone();
                copy.name = mangled.clone();
                prog.functions.push(copy);
                copied.insert((module.clone(), function), mangled);
                // The helper's extern imports travel with its body so the
                // backend still sees every native import.
                if let Some(imports) = self.module_imports.get(&module) {
                    for import in imports {
                        if !prog.imports.iter().any(|i| i.symbol == import.symbol) {
                            prog.imports.push(import.clone());
                        }
                    }
                }
            }
            let owner = prog.module.clone();
            let rewrite = |instrs: &mut [Instr]| {
                for ins in instrs {
                    if let Instr::Call { callee, .. } = ins {
                        let key = (callee.module.clone(), callee.function.clone());
                        if let Some(mangled) = copied.get(&key) {
                            callee.module = owner.clone();
                            callee.function = mangled.clone();
                        }
                    }
                }
            };
            for f in &mut prog.functions {
                rewrite(&mut f.instrs);
            }
            for g in &mut prog.globals {
                rewrite(&mut g.init);
            }
        }
        prog.imports.retain(|i| {
            !self
                .bodies
                .contains_key(&(i.symbol.module.clone(), i.symbol.function.clone()))
        });
        // No pruning here: monomorphic-only programs must remain byte-for-byte
        // stable (module-wide native imports are long-standing behavior).
    }

    /// Splice referenced helpers into `prog` using the world plan for generic
    /// instances. Monomorphic helpers reuse the cached bodies via [`link`];
    /// concrete generic instances (`std.math::max$u64`) lower per compilation
    /// from the checked templates with their substitution and nested targets,
    /// then copy as collision-proof locals. Only bodies reachable from the
    /// consumer are copied; unused instances stay out. Instance caches and
    /// budgets are compilation-local (never mutates the process-wide cache).
    pub fn link_with_plan(
        &self,
        prog: &mut LirProgram,
        plan: &vl_typecheck::world::MonomorphizationPlan,
    ) {
        // Unified per-compilation reachability queue handling monomorphic and
        // generic helpers together, so generic -> monomorphic calls added
        // later still link (and vice versa for mono -> generic, which lower
        // per compilation when plan-sensitive and bypass the cache).
        let mut copied_mono: HashMap<(String, String), String> = HashMap::new();
        let mut copied_generic: HashMap<(String, String), String> = HashMap::new();
        // Plan-sensitive monomorphic helpers (calling a generic) lowered per
        // compilation in this run; tracked separately to avoid re-lowering.
        let mut copied_mono_plan: HashMap<(String, String), String> = HashMap::new();
        loop {
            let mut needed_mono: Vec<(String, String)> = Vec::new();
            let mut needed_mono_plan: Vec<(String, String)> = Vec::new();
            let mut needed_generic: Vec<(String, String)> = Vec::new();
            let mut scan = |instrs: &[Instr]| {
                for ins in instrs {
                    if let Instr::Call { callee, .. } = ins {
                        let key = (callee.module.clone(), callee.function.clone());
                        if self.bodies.contains_key(&key)
                            && !copied_mono.contains_key(&key)
                            && !needed_mono.contains(&key)
                        {
                            needed_mono.push(key);
                        } else if self.is_concrete_generic_instance(&key.0, &key.1)
                            && !copied_generic.contains_key(&key)
                            && !needed_generic.contains(&key)
                        {
                            needed_generic.push(key);
                        } else if !key.1.contains('$')
                            && self.is_plan_mono(&key.0, &key.1)
                            && !copied_mono.contains_key(&key)
                            && !copied_mono_plan.contains_key(&key)
                            && !needed_mono.contains(&key)
                            && !needed_mono_plan.contains(&key)
                        {
                            needed_mono_plan.push(key);
                        }
                    }
                }
            };
            for f in &prog.functions {
                scan(&f.instrs);
            }
            for g in &prog.globals {
                scan(&g.init);
            }
            if needed_mono.is_empty() && needed_mono_plan.is_empty() && needed_generic.is_empty() {
                break;
            }
            needed_mono.sort();
            for (module, function) in needed_mono {
                let mangled = mangle(&module, &function);
                let body = self
                    .bodies
                    .get(&(module.clone(), function.clone()))
                    .expect("stdlib monomorphic helper referenced but not cached (impossible)");
                let mut copy = body.clone();
                copy.name = mangled.clone();
                prog.functions.push(copy);
                copied_mono.insert((module.clone(), function), mangled);
                if let Some(imports) = self.module_imports.get(&module) {
                    for import in imports {
                        if !prog.imports.iter().any(|i| i.symbol == import.symbol) {
                            prog.imports.push(import.clone());
                        }
                    }
                }
            }
            needed_mono_plan.sort();
            for (module, function) in needed_mono_plan {
                // Per-compilation lowering for plan-sensitive monos (calling a
                // generic). No substitution (monomorphic), but nested generic
                // calls resolve via the plan as roots of this stdlib module.
                let Some((hir, typed)) = self.checked.iter().find(|(h, _)| h.module == module)
                else {
                    continue;
                };
                let template = hir.items.iter().find_map(|item| match item {
                    vl_hir::HirItem::Fn {
                        def,
                        params,
                        body,
                        name,
                        type_params,
                        ..
                    } if name == &function && type_params.is_empty() => {
                        Some((def.clone(), params.clone(), body.clone()))
                    }
                    _ => None,
                });
                let Some((def, params, body)) = template else {
                    continue;
                };
                let lowered = vl_lir::lower_stdlib_mono(hir, typed, plan, &params, &body);
                let Some(mut lowered) = lowered else {
                    continue;
                };
                let local = mangle(&module, &function);
                lowered.name = local.clone();
                // Fill signature from checked FuncSigTy (monomorphic, erased).
                if let Some(def) = def {
                    if let Some(sig) = typed.func_sigs.get(&def.0) {
                        lowered.param_tys =
                            sig.param_tys.iter().map(|t| t.erase_capability()).collect();
                        lowered.ret = sig.ret.erase_capability();
                    }
                }
                prog.functions.push(lowered);
                copied_mono_plan.insert((module.clone(), function.clone()), local);
                if let Some(imports) = self.module_imports.get(&module) {
                    for import in imports {
                        if !prog.imports.iter().any(|i| i.symbol == import.symbol) {
                            prog.imports.push(import.clone());
                        }
                    }
                }
            }
            needed_generic.sort();
            for (module, function) in needed_generic {
                let Some((base, _)) = function.split_once('$') else {
                    continue;
                };
                // Find the instance in the plan by structural key (derive the
                // mangled symbol only for comparison at this boundary).
                let Some(instances) = plan.instances_by_owner.get(&module) else {
                    continue;
                };
                let Some((key, inst)) = instances.iter().find(|(k, _)| k.mangled() == function)
                else {
                    continue;
                };
                let key = key.clone();
                // Find the template HIR in checked stdlib modules.
                let Some((hir, typed)) = self.checked.iter().find(|(h, _)| h.module == module)
                else {
                    continue;
                };
                let template = hir.items.iter().find_map(|item| match item {
                    vl_hir::HirItem::Fn {
                        def: Some(d),
                        type_params,
                        params,
                        body,
                        ..
                    } if d.0 == inst.orig => Some((type_params, params, body)),
                    _ => None,
                });
                let Some((type_params, params, body)) = template else {
                    continue;
                };
                let env: std::collections::HashMap<String, vl_typecheck::Ty> = type_params
                    .iter()
                    .map(|p| p.name.clone())
                    .zip(inst.args.iter().cloned())
                    .collect();
                let _ = base;
                // Lower the instance body with the world plan (nested generic
                // calls inside helpers resolve transitively). `key` is the
                // structural outer identity; mangling happens only for names.
                let lowered =
                    lower_stdlib_instance(hir, typed, plan, &key, &env, params, body, inst);
                let Some(mut lowered) = lowered else {
                    continue;
                };
                let local = mangle(&module, &function);
                lowered.name = local.clone();
                prog.functions.push(lowered);
                copied_generic.insert((module.clone(), function.clone()), local);
                if let Some(imports) = self.module_imports.get(&module) {
                    for import in imports {
                        if !prog.imports.iter().any(|i| i.symbol == import.symbol) {
                            prog.imports.push(import.clone());
                        }
                    }
                }
            }
            let owner = prog.module.clone();
            let rewrite = |instrs: &mut [Instr]| {
                for ins in instrs {
                    if let Instr::Call { callee, .. } = ins {
                        let key = (callee.module.clone(), callee.function.clone());
                        if let Some(local) = copied_mono
                            .get(&key)
                            .or_else(|| copied_mono_plan.get(&key))
                            .or_else(|| copied_generic.get(&key))
                        {
                            callee.module = owner.clone();
                            callee.function = local.clone();
                        }
                    }
                }
            };
            for f in &mut prog.functions {
                rewrite(&mut f.instrs);
            }
            for g in &mut prog.globals {
                rewrite(&mut g.init);
            }
        }
        prog.imports.retain(|i| {
            !(self.is_helper(&i.symbol.module, &i.symbol.function)
                || self.is_concrete_generic_instance(&i.symbol.module, &i.symbol.function))
        });
        // Prune unused natives only when generics were involved (new programs
        // have no prior bytes to preserve). Monomorphic-only programs skip
        // pruning to remain byte-for-byte stable.
        if !copied_generic.is_empty() || !copied_mono_plan.is_empty() {
            prune_unused_native_imports(prog);
        }
    }
}

/// Retain only native imports actually referenced by the final program.
/// Helpers merged per module previously over-approximated (e.g. linking only
/// `max` would still carry `mod_u64` used by `is_even`); scanning the emitted
/// calls drops unused natives while keeping every required one. Only applied
/// when generics were linked (see above) to preserve byte stability for
/// monomorphic-only programs.
fn prune_unused_native_imports(prog: &mut LirProgram) {
    use std::collections::HashSet;
    let mut referenced: HashSet<(String, String)> = HashSet::new();
    for f in &prog.functions {
        for ins in &f.instrs {
            if let Instr::Call { callee, .. } = ins {
                referenced.insert((callee.module.clone(), callee.function.clone()));
            }
        }
    }
    for g in &prog.globals {
        for ins in &g.init {
            if let Instr::Call { callee, .. } = ins {
                referenced.insert((callee.module.clone(), callee.function.clone()));
            }
        }
    }
    prog.imports
        .retain(|i| referenced.contains(&(i.symbol.module.clone(), i.symbol.function.clone())));
}

/// Lower one stdlib generic instance body with its substitution and the world
/// plan for nested calls. Returns `None` when poisoned (already reported).
#[allow(clippy::too_many_arguments)]
fn lower_stdlib_instance(
    hir: &vl_hir::HirProgram,
    typed: &vl_typecheck::TypedProgram,
    plan: &vl_typecheck::world::MonomorphizationPlan,
    outer_key: &vl_typecheck::InstanceKey,
    env: &std::collections::HashMap<String, vl_typecheck::Ty>,
    params: &[(
        String,
        Option<vl_semantic::DefId>,
        Option<vl_common::VlType>,
        vl_common::Span,
    )],
    body: &[vl_hir::HirStmt],
    inst: &vl_typecheck::Instance,
) -> Option<vl_lir::Function> {
    // Reuse the project lowering machinery by building a synthetic owner
    // program? Simpler: lower here with a minimal Lowerer-like flow via
    // `vl_lir::lower_project` on a synthetic single-instance program is
    // overkill; instead lower statements through a temporary LIR program
    // built from the template. To avoid duplicating the Lowerer, we lower via
    // a temporary HirProgram containing only this template as a monomorphic
    // function? That would lose generic substitution.
    //
    // Instead, duplicate the small lowering steps: create a fresh LIR function
    // by invoking `vl_lir` internals through the public `lower_project` on a
    // filtered view is not possible (it lowers whole modules). For the first
    // generic helper (`max` with only operators, no calls), lowering is
    // straightforward, but to support transitive helpers we delegate to a
    // generic lowering helper exposed by `vl-lir`.
    //
    // To keep crates decoupled (`vl-typecheck` must not depend on `vl-stdlib`),
    // the actual instruction lowering lives in `vl-lir`; expose a helper there.
    vl_lir::lower_stdlib_instance(hir, typed, plan, outer_key, env, params, body, inst)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_lir(stdlib: &Stdlib, src: &str) -> LirProgram {
        let (toks, mut diags) = vl_lex::lex(src);
        let (prog, mut d) = vl_syntax::parse(&toks, src);
        diags.append(&mut d);
        let mut catalog = vl_codegen::modules();
        stdlib.extend_catalog(&mut catalog);
        let (res, mut d) = vl_semantic::resolve_with_modules(&prog, &catalog);
        diags.append(&mut d);
        let hir = vl_hir::lower(&prog, &res);
        let (typed, mut d) = vl_typecheck::check(&hir);
        diags.append(&mut d);
        diags.append(&mut typed.validate_normalized(&hir, &diags));
        assert!(diags.is_empty(), "{diags:?}");
        let mut world_refs = vec![(&hir, &typed)];
        for (shir, styped) in stdlib.checked_modules() {
            world_refs.push((shir, styped));
        }
        let (plan, world_diags) = vl_typecheck::world::plan_world(&world_refs);
        for (_, diag) in world_diags {
            diags.push(diag);
        }
        assert!(diags.is_empty(), "{diags:?}");
        let mut lir = vl_lir::lower_project(&hir, &typed, &plan);
        stdlib.link_with_plan(&mut lir, &plan);
        lir
    }

    #[test]
    fn embedded_modules_export_exactly_the_helpers() {
        let stdlib = load();
        assert_eq!(
            stdlib.helper_names("std.string"),
            vec!["is_empty", "strings_equal"]
        );
        assert_eq!(
            stdlib.helper_names("std.math"),
            vec![
                "abs_i64",
                "clamp_u64",
                "is_even",
                "max",
                "max_i64",
                "max_u64",
                "min_i64",
                "min_u64"
            ]
        );
        assert_eq!(stdlib.helper_names("std.fmt"), vec!["u64_to_string"]);
    }

    #[test]
    fn no_helper_collides_with_an_extern_export() {
        let stdlib = load();
        for spec in vl_codegen::modules() {
            for export in &spec.exports {
                assert!(
                    !stdlib.is_helper(&spec.path.as_string(), &export.name),
                    "helper `{}` collides with an extern export",
                    export.name
                );
            }
        }
    }

    #[test]
    fn extend_catalog_preserves_union_metadata() {
        let mut helper = ModuleSpec::new(&["demo"], &[]);
        helper.unions.push(vl_common::UnionExport {
            name: "U".into(),
            qualified: "demo.U".into(),
            type_params: Vec::new(),
            variants: vec![vl_common::UnionVariantSig {
                name: "A".into(),
                payload: Vec::new(),
            }],
        });
        let stdlib = Stdlib {
            bodies: HashMap::new(),
            module_imports: HashMap::new(),
            specs: vec![helper],
            checked: Vec::new(),
        };
        let mut catalog = vec![ModuleSpec::new(&["demo"], &[])];
        stdlib.extend_catalog(&mut catalog);
        assert_eq!(catalog[0].unions.len(), 1);
        assert_eq!(catalog[0].unions[0].qualified, "demo.U");
    }

    #[test]
    fn qualified_helper_calls_resolve_to_their_module() {
        let stdlib = load();
        let (toks, _) = vl_lex::lex("use std.math; fun main() { math.max_u64(1u64, 2u64); }");
        let (prog, _) = vl_syntax::parse(&toks, "");
        let mut catalog = vl_codegen::modules();
        stdlib.extend_catalog(&mut catalog);
        let (res, diags) = vl_semantic::resolve_with_modules(&prog, &catalog);
        assert!(diags.is_empty(), "{diags:?}");
        let def = res
            .defs
            .iter()
            .find(|d| d.name == "math.max_u64")
            .expect("def");
        let symbol = def.symbol.as_ref().expect("symbol");
        assert_eq!(symbol.module.as_string(), "std.math");
        assert_eq!(symbol.name, "max_u64");
    }

    #[test]
    fn unknown_helper_export_is_a_single_error() {
        // Same shape as a misspelled thin extern: the module resolves, the
        // export does not, and exactly one root diagnostic stays quiet
        // downstream.
        let stdlib = load();
        let (toks, _) = vl_lex::lex("use std.math; fun main() { math.bogus(1u64); }");
        let (prog, _) = vl_syntax::parse(&toks, "");
        let mut catalog = vl_codegen::modules();
        stdlib.extend_catalog(&mut catalog);
        let (_, diags) = vl_semantic::resolve_with_modules(&prog, &catalog);
        assert_eq!(
            diags.iter().filter(|d| d.is_error()).count(),
            1,
            "{diags:?}"
        );
    }

    #[test]
    fn link_with_explicit_use_inlines_helpers_as_locals() {
        use vl_codegen::Target;
        let stdlib = load();
        let lir = user_lir(
            &stdlib,
            "use std.math; use std.string; fun main() { val c = math.clamp_u64(100u64, 0u64, 10u64); val e = string.is_empty(\"s\"); c; e; }",
        );
        let dump = lir.dump();
        // clamp pulls min+max transitively; is_even stays out.
        for name in [
            "fn std$math$clamp_u64:",
            "fn std$math$min_u64:",
            "fn std$math$max_u64:",
            "fn std$string$is_empty:",
        ] {
            assert!(dump.contains(name), "{name} missing in {dump}");
        }
        assert!(!dump.contains("fn std$math$is_even:"), "{dump}");
        assert!(!dump.contains("std.math::"), "{dump}");
        let (artifact, diags) = vl_codegen::NaraVmTarget.emit(&lir);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(&artifact.unwrap().bytes.unwrap()[..4], b"nara");
    }

    #[test]
    fn link_leaves_extern_calls_on_the_native_path() {
        use vl_codegen::Target;
        let stdlib = load();
        let lir = user_lir(
            &stdlib,
            "use std.string; fun main() { val s = string.concat(\"a\", \"b\"); s; }",
        );
        let dump = lir.dump();
        assert!(dump.contains("call std.string::concat"), "{dump}");
        assert!(
            !dump.contains("std$string$"),
            "extern must not be inlined: {dump}"
        );
        let (artifact, diags) = vl_codegen::NaraVmTarget.emit(&lir);
        assert!(diags.is_empty(), "{diags:?}");
        let bytes = artifact.unwrap().bytes.unwrap();
        assert!(
            bytes
                .windows(b"std::string".len())
                .any(|w| w == b"std::string"),
            "native module spelling missing"
        );
    }

    #[test]
    fn user_bare_name_does_not_capture_qualified_helper() {
        use vl_codegen::Target;
        let stdlib = load();
        let lir = user_lir(
            &stdlib,
            "use std.math; fun max_u64(a: u64, b: u64): u64 { return a; } fun main() { val a = max_u64(1u64, 2u64); val b = math.max_u64(1u64, 2u64); a; b; }",
        );
        let dump = lir.dump();
        assert!(dump.contains("fn max_u64:"), "{dump}");
        assert!(dump.contains("fn std$math$max_u64:"), "{dump}");
        let (artifact, diags) = vl_codegen::NaraVmTarget.emit(&lir);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(&artifact.unwrap().bytes.unwrap()[..4], b"nara");
    }

    #[test]
    fn generic_helper_infers_and_selects_explicitly() {
        use vl_codegen::Target;
        let stdlib = load();
        let lir = user_lir(
            &stdlib,
            "use std.math; fun main() { val a = math.max(3u64, 9u64); val b = math.max::[i64](-1i64, 5i64); a; b; }",
        );
        let dump = lir.dump();
        assert!(dump.contains("fn std$math$max$u64:"), "{dump}");
        assert!(dump.contains("fn std$math$max$i64:"), "{dump}");
        assert!(!dump.contains("std.math::max("), "{dump}");
        let (artifact, diags) = vl_codegen::NaraVmTarget.emit(&lir);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(&artifact.unwrap().bytes.unwrap()[..4], b"nara");
    }

    #[test]
    fn generic_helper_links_only_requested_and_reuses() {
        let stdlib = load();
        let lir = user_lir(
            &stdlib,
            "use std.math; fun main() { val a = math.max(1u64, 2u64); val b = math.max(3u64, 4u64); val c = math.max_u64(5u64, 6u64); a; b; c; }",
        );
        let dump = lir.dump();
        assert_eq!(dump.matches("fn std$math$max$u64:").count(), 1, "{dump}");
        assert!(dump.contains("fn std$math$max_u64:"), "{dump}");
        assert!(!dump.contains("fn std$math$max$i64:"), "{dump}");
        assert!(!dump.contains("fn std$math$is_even:"), "{dump}");
    }

    #[test]
    fn generic_helper_carries_no_unemittable_imports() {
        use vl_codegen::Target;
        let stdlib = load();
        let lir = user_lir(
            &stdlib,
            "use std.math; fun main() { val m = math.max(1u64, 2u64); m; }",
        );
        let (artifact, diags) = vl_codegen::NaraVmTarget.emit(&lir);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(&artifact.unwrap().bytes.unwrap()[..4], b"nara");
    }
}
