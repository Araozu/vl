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
//! - helpers are monomorphic with no globals and no `main`.

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
    bodies: HashMap<(String, String), Function>,
    /// Owning module -> that module's imports (extern calls its helpers
    /// need). Merged into the user program alongside copied bodies so the
    /// backend still sees every native import.
    module_imports: HashMap<String, Vec<FunctionImport>>,
    /// Helper-only catalog specs, one per embedded module.
    specs: Vec<ModuleSpec>,
}

/// Assert no errors; embedded sources are deterministic and tested.
fn expect_clean(diags: &[vl_common::Diagnostic], what: &str) {
    assert!(
        diags.iter().all(|d| !d.is_error()),
        "embedded stdlib {what} has diagnostics (impossible: covered by tests): {diags:?}"
    );
}

/// Lex, parse, resolve, check, and lower every embedded module. Panics on
/// any diagnostic or authoring-rule violation: the inputs are deterministic
/// and covered by tests, so failure here is a compiler bug, not user error.
pub fn load() -> Stdlib {
    let externs = vl_codegen::modules();
    let mut bodies = HashMap::new();
    let mut specs = Vec::new();
    // (importer module, imported symbol): checked once all modules load,
    // since a helper may call a helper from a later file.
    let mut module_imports: HashMap<String, Vec<FunctionImport>> = HashMap::new();
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
            interface.generic_functions.is_empty()
                && interface.poisoned_exports.is_empty()
                && interface.global_dependent_exports.is_empty(),
            "embedded stdlib {} must export only clean monomorphic helpers",
            module.path
        );
        let (res, rdiags) = vl_semantic::resolve_with_modules(&prog, &externs);
        expect_clean(&rdiags, &format!("{} resolve", module.path));
        let hir = vl_hir::lower(&prog, &res);
        let (typed, mut tdiags) = vl_typecheck::check(&hir);
        tdiags.append(&mut typed.validate_normalized(&hir, &tdiags));
        expect_clean(&tdiags, &format!("{} typecheck", module.path));
        let lir = vl_lir::lower(&hir, &typed);
        assert!(
            !lir.entrypoint && lir.globals.is_empty(),
            "embedded stdlib {} must be a pure function module",
            module.path
        );
        module_imports
            .entry(module.path.to_string())
            .or_default()
            .extend(lir.imports.iter().cloned());
        for f in &lir.functions {
            assert!(
                !f.name.contains('$'),
                "embedded stdlib {} must not instantiate generics",
                module.path
            );
            bodies.insert((module.path.to_string(), f.name.clone()), f.clone());
        }
        specs.push(interface.as_spec());
    }
    let stdlib = Stdlib {
        bodies,
        module_imports,
        specs,
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
    }

    /// Helper names exported by one embedded module, sorted.
    pub fn helper_names(&self, module: &str) -> Vec<String> {
        let mut names: Vec<String> = self
            .bodies
            .keys()
            .filter(|(m, _)| m == module)
            .map(|(_, f)| f.clone())
            .collect();
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
                        if self.is_helper(&key.0, &key.1)
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
        prog.imports
            .retain(|i| !self.is_helper(&i.symbol.module, &i.symbol.function));
    }
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
        let mut lir = vl_lir::lower(&hir, &typed);
        stdlib.link(&mut lir);
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
}
