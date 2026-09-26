//! vl-semantic: name resolution over the AST.
//!
//! Builds lexical scopes for top-level items and function bodies,
//! reports undefined names and duplicate definitions. The resulting
//! [`Resolution`] is consumed by `vl-hir` lowering.

use std::collections::HashMap;

use vl_common::{
    Diagnostic, ModuleInterface, ModuleOrigin, ModulePath, ModuleSpec, Span, SymbolRef,
};
use vl_syntax::{BindingKind, Expr, Item, Program, Stmt};

/// Builtin nullable union: `?T` / `null` sugar desugars to
/// `Option` with `None` (tag 0, no payload) and `Some(T)` (tag 1).
/// Available in every module without a declaration; a local
/// `type Option` shadows it.
pub fn builtin_option_variants() -> Vec<String> {
    vec!["None".to_string(), "Some".to_string()]
}

/// A definition site: which item/scope and which binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefId(pub u32);

/// Where a name was defined.
#[derive(Debug, Clone)]
pub struct Def {
    pub id: DefId,
    pub name: String,
    pub span: Span,
    pub kind: DefKind,
    /// Compiler-owned signature. `Some` for `External`/`ImportedFunction` defs
    /// resolved from the module catalog (possibly generic for source imports);
    /// `None` for locals and poisoned imports.
    pub sig: Option<vl_common::FuncSig>,
    /// Source-versus-target linkage for imported calls. `None` for locals,
    /// poisoned imports, and synthetic builtins such as `Array.new`.
    pub export_kind: Option<vl_common::ExportKind>,
    /// Binding mode for value definitions. Functions and parameters leave
    /// this unset because their fixed-binding rules are independent.
    pub binding: Option<BindingKind>,
    pub symbol: Option<SymbolRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefKind {
    Local,
    Parameter,
    External,
    ImportedFunction,
    ModuleAlias,
}

/// A union-variant use site: `Option.Some(args)` (call) or `Option.None`
/// (nullary field path). The resolver records these so later stages can tell
/// variant construction apart from function calls and field reads, which
/// share the surface syntax. `union` is the canonical name (bare for local
/// unions, module-qualified for imported ones, matching `typed.unions` keys).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantUse {
    pub union: String,
    pub variant: String,
}

/// An error-value use site: `MyError.NotFound` (nullary field path).
/// Recorded so later stages can tell error construction apart from field
/// reads, which share the surface syntax. `set` is the canonical name
/// (bare for local sets, module-qualified for imported ones, matching
/// `typed.errors` keys). Error variants take no arguments, so a call tail
/// (`E.V(args)`) is one E303 here and the site still records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorUse {
    pub set: String,
    pub variant: String,
}

/// Resolution result: every variable *use* span maps to its [`Def`].
#[derive(Debug, Default)]
pub struct Resolution {
    /// Keyed by the use-site span start (spans are unique per node here).
    pub uses: HashMap<(usize, usize), DefId>,
    pub defs: Vec<Def>,
    /// Instance-sugar call sites (`receiver.method(args)` where the head is a
    /// bound value): callee span maps to the receiver head's [`DefId`].
    /// `vl-hir` lowers these to method calls; `vl-typecheck` validates the
    /// self-type gate. No E201 is reported for these sites here.
    pub sugar_receivers: HashMap<(usize, usize), DefId>,
    /// Union-variant construction sites: call callee spans (`Option.Some`)
    /// and nullary field-path spans (`Option.None`) map to the referenced
    /// variant. `vl-hir` lowers these to variant nodes; `vl-typecheck`
    /// validates arity and payload types. No E201 is reported here.
    pub variants: HashMap<(usize, usize), VariantUse>,
    /// Error-value construction sites: nullary field-path spans
    /// (`MyError.NotFound`) map to the referenced error. `vl-hir` lowers
    /// these to error-value nodes; `vl-typecheck` validates set membership.
    /// No E201 is reported here.
    pub error_uses: HashMap<(usize, usize), ErrorUse>,
    /// Canonical union names for `match` arm paths, keyed by the arm's path
    /// span. Import-alias heads (`alias.Union.Variant`) rewrite to the
    /// module-qualified spelling so later stages compare one identity;
    /// bare and fully qualified heads keep their spelling.
    pub match_patterns: HashMap<(usize, usize), String>,
    /// Import names poisoned by an upstream provider diagnostic. Downstream
    /// stages use this to keep the poison quiet without inventing E500s.
    pub poisoned_imports: bool,
    /// Bare type names brought in through `use` (`use vl.dog.{Dog}` then
    /// `Dog`): bare spelling maps to the module-qualified identity
    /// (`vl.dog.Dog`). `vl-hir` canonicalizes every type position to the
    /// qualified spelling, so nominal identity never collides across
    /// modules. Functions live in [`Resolution::uses`]; types never enter
    /// the value scope.
    pub imported_types: HashMap<String, String>,
}

impl Resolution {
    fn intern_param(&mut self, name: String, span: Span) -> DefId {
        self.intern_def_as(name, span, DefKind::Parameter, None)
    }

    fn intern_def_as(
        &mut self,
        name: String,
        span: Span,
        kind: DefKind,
        binding: Option<BindingKind>,
    ) -> DefId {
        let id = DefId(self.defs.len() as u32);
        self.defs.push(Def {
            id: id.clone(),
            name,
            span,
            kind,
            sig: None,
            export_kind: None,
            binding,
            symbol: None,
        });
        id
    }

    pub fn def_of(&self, use_span: Span) -> Option<&Def> {
        self.uses
            .get(&(use_span.start, use_span.end))
            .and_then(|id| self.defs.iter().find(|d| d.id == *id))
    }

    /// Definition-site lookup: which [`Def`] was declared *at* `span`.
    /// (Use-sites go through [`def_of`](Self::def_of); def-sites are not uses.)
    pub fn def_at(&self, def_span: Span) -> Option<&Def> {
        self.defs
            .iter()
            .find(|d| d.span.start == def_span.start && d.span.end == def_span.end)
    }
}

struct Resolver {
    scopes: Vec<HashMap<String, DefId>>,
    out: Resolution,
    diags: Vec<Diagnostic>,
    modules: Vec<ModuleSpec>,
    /// Owning module of the program under resolution (for qualified local
    /// associated calls such as `my.mod.Counter.init(...)`).
    module: String,
    imports: HashMap<String, ModuleSpec>,
    import_symbols: HashMap<String, SymbolRef>,
    /// Bare type imports (`Dog` -> `vl.dog.Dog`), kept out of the value
    /// scope: types and functions occupy separate namespaces, so a type
    /// import never conflicts with a same-named function import.
    imported_types: HashMap<String, String>,
    poisoned_imports: std::collections::HashSet<String>,
    import_spans: HashMap<String, Span>,
    /// Parameter `DefId`s shadowed by a duplicate declaration in the same
    /// function. Assigning to them already has one root cause (E200), so E205
    /// stays quiet.
    poisoned_params: std::collections::HashSet<u32>,
    loop_depth: usize,
    /// Bare object type names declared in this module.
    local_objects: std::collections::HashSet<String>,
    /// Field names per local object (first declaration wins), for E302 hints
    /// when a method name matches no associated function.
    local_fields: HashMap<String, Vec<String>>,
    /// Associated functions declared in this module: `(Type, method)` maps to
    /// the method's [`DefId`] (first declaration wins).
    assoc: HashMap<(String, String), DefId>,
    /// Bare union names declared in this module mapped to their variant names
    /// in declaration order. Variant construction (`Union.Variant(...)`,
    /// `Union.Variant`) resolves through this table without an import.
    local_unions: HashMap<String, Vec<String>>,
    /// Bare error-set names declared in this module mapped to their variant
    /// names in declaration order. Error construction (`E.V`) resolves
    /// through this table without an import.
    local_errors: HashMap<String, Vec<String>>,
}

pub fn resolve(prog: &Program) -> (Resolution, Vec<Diagnostic>) {
    resolve_with_modules(prog, &default_modules())
}

/// Collect the public, monomorphic function surface without resolving bodies.
/// The project driver calls this after every source file has parsed so import
/// resolution is independent of filesystem order.
pub fn collect_interface(prog: &Program) -> (ModuleInterface, Vec<Diagnostic>) {
    collect_interface_impl(prog, true)
}

/// Collect interface metadata without repeating parser diagnostics for a
/// malformed function signature. The project driver uses this after parsing.
pub fn collect_interface_quiet(prog: &Program) -> (ModuleInterface, Vec<Diagnostic>) {
    collect_interface_impl(prog, false)
}

fn collect_interface_impl(
    prog: &Program,
    diagnose_incomplete_signatures: bool,
) -> (ModuleInterface, Vec<Diagnostic>) {
    let mut functions: Vec<vl_common::Export> = Vec::new();
    let mut objects: Vec<vl_common::ObjectExport> = Vec::new();
    let mut unions: Vec<vl_common::UnionExport> = Vec::new();
    let mut errors: Vec<vl_common::ErrorExport> = Vec::new();
    let mut diags = Vec::new();
    let mut poisoned_exports = Vec::new();
    let mut global_dependent_exports = Vec::new();
    let mut seen = HashMap::<String, Span>::new();
    // Local object names, so bare references in signatures and fields can
    // be qualified to `<module>.<name>` on export. Importers only ever see
    // the qualified spelling, which keeps nominal identity collision-free.
    let local_objects = prog
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Object { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect::<std::collections::HashSet<_>>();
    let local_types = prog
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Object { name, .. } | Item::Union { name, .. } | Item::Error { name, .. } => {
                Some(name.clone())
            }
            _ => None,
        })
        .collect::<std::collections::HashSet<_>>();
    let global_names = prog
        .items
        .iter()
        .flat_map(|item| match item {
            Item::Let { name, .. } => vec![name.clone()],
            Item::Destructure { bindings, .. } => {
                bindings.iter().map(|b| b.binding.clone()).collect()
            }
            _ => Vec::new(),
        })
        .collect::<std::collections::HashSet<_>>();
    // Every callable unit (free functions plus associated methods keyed
    // `Type.method`) joins one global-dependence fixed point, so cross-module
    // associated calls meet the same E208 boundary as free functions.
    // `(key, params, body)` borrows keep free and associated units uniform.
    let mut fn_units: Vec<(String, &[vl_syntax::Param], &[Stmt])> = Vec::new();
    let mut direct_dependencies = HashMap::<String, Vec<String>>::new();
    // Error-set declarations export nominal identity and variant names.
    // They never acquire layouts, methods, or type parameters.
    for item in &prog.items {
        let Item::Error { name, variants, .. } = item else {
            continue;
        };
        if errors.iter().any(|export| export.name == *name) {
            continue;
        }
        errors.push(vl_common::ErrorExport {
            name: name.clone(),
            qualified: format!("{}.{}", prog.module, name),
            variants: variants.iter().map(|v| v.name.clone()).collect(),
        });
    }
    // Union declarations export nominal identity and payload metadata, but
    // never acquire an object layout or associated-method namespace.
    for item in &prog.items {
        let Item::Union {
            name,
            type_params,
            variants,
            ..
        } = item
        else {
            continue;
        };
        if unions.iter().any(|export| export.name == *name) {
            continue;
        }
        unions.push(vl_common::UnionExport {
            name: name.clone(),
            qualified: format!("{}.{}", prog.module, name),
            type_params: type_params
                .iter()
                .map(|p| vl_common::TypeParamSig {
                    name: p.name.clone(),
                    bound: p.bound,
                })
                .collect(),
            variants: variants
                .iter()
                .map(|variant| vl_common::UnionVariantSig {
                    name: variant.name.clone(),
                    payload: variant
                        .payload
                        .iter()
                        .map(|(ty, _)| qualify_export_ty(ty, &prog.module, &local_types))
                        .collect(),
                })
                .collect(),
        });
    }
    for item in &prog.items {
        if let Item::Function {
            name, params, body, ..
        } = item
        {
            let mut calls = Vec::new();
            collect_local_calls(body, &mut calls);
            direct_dependencies.entry(name.clone()).or_insert(calls);
            if !fn_units.iter().any(|(key, _, _)| key == name) {
                fn_units.push((name.clone(), params, body));
            }
        }
        if let Item::Object {
            name: owner,
            methods,
            ..
        } = item
        {
            for m in methods {
                let key = format!("{owner}.{}", m.name);
                let mut calls = Vec::new();
                collect_local_calls(&m.body, &mut calls);
                // First declaration wins, like the interface below;
                // duplicates are E200 elsewhere.
                direct_dependencies.entry(key.clone()).or_insert(calls);
                if !fn_units.iter().any(|(k, _, _)| k == &key) {
                    fn_units.push((key, &m.params, &m.body));
                }
            }
        }
    }
    let mut depends_on_global = global_names.clone();
    let mut changed = true;
    while changed {
        changed = false;
        for (key, params, body) in &fn_units {
            let direct = function_depends_on_global(params, body, &global_names, &local_objects);
            let transitive = direct_dependencies
                .get(key)
                .into_iter()
                .flatten()
                .any(|callee| depends_on_global.contains(callee));
            if (direct || transitive) && depends_on_global.insert(key.clone()) {
                changed = true;
            }
        }
    }
    // Object layouts are public by default, like top-level functions.
    // Duplicate object names are reported by resolution (E200); the
    // interface keeps the first so importers see a stable catalog.
    for item in &prog.items {
        let Item::Object {
            name,
            fields,
            methods,
            ..
        } = item
        else {
            continue;
        };
        if objects
            .iter()
            .any(|export: &vl_common::ObjectExport| export.name == *name)
        {
            continue;
        }
        let qualified = format!("{}.{}", prog.module, name);
        let mut out_fields = Vec::with_capacity(fields.len());
        for field in fields {
            let ty = field
                .ty
                .clone()
                .map(|ty| qualify_export_ty(&ty, &prog.module, &local_types));
            out_fields.push(vl_common::ObjectFieldSig {
                name: field.name.clone(),
                // A missing field type was already reported by the parser;
                // `void` stands in so the export keeps its shape while
                // typechecking poisons uses quietly downstream.
                ty: ty.unwrap_or(vl_common::VlType::Void),
            });
        }
        // Associated functions are public by default, like fields. A method
        // with an incomplete signature is poisoned under its `Type.method`
        // key so importers stay quiet on the provider's root cause (mirrors
        // free functions, which use the bare name).
        let mut out_methods = Vec::with_capacity(methods.len());
        for m in methods {
            let key = format!("{name}.{}", m.name);
            if m.signature_poisoned || m.ret.is_none() || m.params.iter().any(|p| p.ty.is_none()) {
                poisoned_exports.push(key.clone());
                if diagnose_incomplete_signatures {
                    diags.push(
                        Diagnostic::error(format!(
                            "exported associated function `{key}` has an incomplete signature"
                        ))
                        .with_label(m.name_span, "unsupported cross-module boundary")
                        .with_code("E208"),
                    );
                }
                continue;
            }
            if depends_on_global.contains(&key) {
                global_dependent_exports.push(key.clone());
            }
            let type_param_sigs = m
                .type_params
                .iter()
                .map(|p| vl_common::TypeParamSig {
                    name: p.name.clone(),
                    bound: p.bound,
                })
                .collect::<Vec<_>>();
            let sig = vl_common::FuncSig::generic(
                type_param_sigs,
                m.params
                    .iter()
                    .filter_map(|p| {
                        p.ty.clone().map(|ty| vl_common::ParamSig {
                            name: p.name.clone(),
                            ty: qualify_export_ty(&ty, &prog.module, &local_types),
                        })
                    })
                    .collect(),
                m.ret
                    .clone()
                    .map(|ty| qualify_export_ty(&ty, &prog.module, &local_types))
                    .unwrap_or(vl_common::VlType::Void),
            );
            if sig.params.len() != m.params.len() {
                poisoned_exports.push(key);
                continue;
            }
            out_methods.push(vl_common::Export::source(m.name.clone(), sig));
        }
        objects.push(vl_common::ObjectExport {
            name: name.clone(),
            qualified,
            fields: out_fields,
            methods: out_methods,
        });
    }
    for item in &prog.items {
        let Item::Function {
            name,
            name_span,
            type_params,
            params,
            ret,
            signature_poisoned,
            span,
            ..
        } = item
        else {
            continue;
        };
        if let Some(previous) = seen.insert(name.clone(), *name_span) {
            diags.push(
                Diagnostic::error(format!("duplicate export `{name}`"))
                    .with_label(*name_span, "redefined here")
                    .with_bare_label(previous)
                    .with_code("E200"),
            );
            functions.retain(|export| export.name != *name);
            if !poisoned_exports.contains(name) {
                poisoned_exports.push(name.clone());
            }
            continue;
        }
        // Global dependence is unsafe metadata for every function, including
        // main. Local entrypoints are valid; import validation rejects it only
        // when another module crosses this boundary.
        if depends_on_global.contains(name) {
            global_dependent_exports.push(name.clone());
        }
        if *signature_poisoned || ret.is_none() || params.iter().any(|p| p.ty.is_none()) {
            // Malformed signatures are poisoned so importers stay quiet on
            // the provider's parser root cause.
            poisoned_exports.push(name.clone());
            if diagnose_incomplete_signatures {
                diags.push(
                    Diagnostic::error(format!(
                        "exported function `{name}` has an incomplete signature"
                    ))
                    .with_label(*span, "unsupported cross-module boundary")
                    .with_code("E208"),
                );
            }
            continue;
        }
        // Exported signatures qualify local object references
        // (`Person` -> `vl.person.Person`) so importers resolve nominal
        // identity without the provider's scope. `Param` passes through
        // unchanged; `Array` and `*` recurse.
        let type_param_sigs = type_params
            .iter()
            .map(|p| vl_common::TypeParamSig {
                name: p.name.clone(),
                bound: p.bound,
            })
            .collect::<Vec<_>>();
        let sig = vl_common::FuncSig::generic(
            type_param_sigs,
            params
                .iter()
                .filter_map(|p| {
                    p.ty.clone().map(|ty| vl_common::ParamSig {
                        name: p.name.clone(),
                        ty: qualify_export_ty(&ty, &prog.module, &local_types),
                    })
                })
                .collect(),
            ret.clone()
                .map(|ty| qualify_export_ty(&ty, &prog.module, &local_types))
                .unwrap_or(vl_common::VlType::Void),
        );
        if sig.params.len() != params.len() {
            poisoned_exports.push(name.clone());
            continue;
        }
        functions.push(vl_common::Export::source(name.clone(), sig));
    }
    (
        ModuleInterface {
            path: ModulePath::from_dotted(&prog.module),
            origin: ModuleOrigin::Source,
            functions,
            objects,
            unions,
            errors,
            parse_poisoned: false,
            poisoned_exports,
            global_dependent_exports,
        },
        diags,
    )
}

/// Rewrite bare references to this module's own object types into their
/// fully qualified identity (`Person` -> `<module>.Person`). Already
/// qualified names, primitives, and type parameters pass through; `Array`
/// and `*` recurse. Unknown bare names are left alone for downstream
/// poisoning (the parser already reported them).
fn qualify_export_ty(
    ty: &vl_common::VlType,
    module: &str,
    local_objects: &std::collections::HashSet<String>,
) -> vl_common::VlType {
    match ty {
        vl_common::VlType::Object(name) if !name.contains('.') => {
            if local_objects.contains(name) {
                vl_common::VlType::Object(format!("{module}.{name}"))
            } else {
                ty.clone()
            }
        }
        vl_common::VlType::Array(elem) => {
            vl_common::VlType::Array(Box::new(qualify_export_ty(elem, module, local_objects)))
        }
        vl_common::VlType::Nullable(inner) => {
            vl_common::VlType::Nullable(Box::new(qualify_export_ty(inner, module, local_objects)))
        }
        vl_common::VlType::Tuple(fields) => vl_common::VlType::Tuple(
            fields
                .iter()
                .map(|f| vl_common::TupleField {
                    name: f.name.clone(),
                    ty: Box::new(qualify_export_ty(&f.ty, module, local_objects)),
                })
                .collect(),
        ),
        vl_common::VlType::Mutable(inner) => {
            vl_common::VlType::Mutable(Box::new(qualify_export_ty(inner, module, local_objects)))
        }
        vl_common::VlType::Union { name, args } if !name.contains('.') => {
            let name = if local_objects.contains(name) {
                format!("{module}.{name}")
            } else {
                name.clone()
            };
            vl_common::VlType::Union {
                name,
                args: args
                    .iter()
                    .map(|a| qualify_export_ty(a, module, local_objects))
                    .collect(),
            }
        }
        vl_common::VlType::ErrorSet(name) if !name.contains('.') => {
            if local_objects.contains(name) {
                vl_common::VlType::ErrorSet(format!("{module}.{name}"))
            } else {
                ty.clone()
            }
        }
        vl_common::VlType::Fallible { err, ok } => vl_common::VlType::Fallible {
            err: err.as_ref().map(|set| {
                if !set.contains('.') && local_objects.contains(set) {
                    format!("{module}.{set}")
                } else {
                    set.clone()
                }
            }),
            ok: Box::new(qualify_export_ty(ok, module, local_objects)),
        },
        _ => ty.clone(),
    }
}

fn collect_local_calls(stmts: &[Stmt], calls: &mut Vec<String>) {
    fn visit_expr(value: &Expr, calls: &mut Vec<String>) {
        match value {
            Expr::Call { callee, args, .. } => {
                if callee.len() == 1 {
                    calls.push(callee[0].clone());
                } else if callee.len() >= 2 {
                    // Dotted edges (`Type.method(...)`) feed the same fixed
                    // point so associated calls share the E208 boundary with
                    // free functions. Value-headed sugar (`r.m(...)`) matches
                    // no callable key and stays inert.
                    calls.push(callee.join("."));
                }
                for arg in args {
                    visit_expr(arg, calls);
                }
            }
            Expr::ArrayLiteral { elems, .. } => elems.iter().for_each(|e| visit_expr(e, calls)),
            Expr::ObjectLiteral { fields, .. } => {
                fields.iter().for_each(|(_, _, e)| visit_expr(e, calls))
            }
            Expr::TupleLiteral { elems, .. } => {
                elems.iter().for_each(|(_, _, e)| visit_expr(e, calls))
            }
            Expr::TupleIndex { base, .. } => visit_expr(base, calls),
            Expr::Index { base, index, .. } => {
                visit_expr(base, calls);
                visit_expr(index, calls);
            }
            Expr::Field { base, .. }
            | Expr::Unary { rhs: base, .. }
            | Expr::Try { inner: base, .. }
            | Expr::Cast { inner: base, .. } => visit_expr(base, calls),
            Expr::Catch { lhs, fallback, .. } => {
                visit_expr(lhs, calls);
                visit_expr(fallback, calls);
            }
            Expr::Binary { lhs, rhs, .. } => {
                visit_expr(lhs, calls);
                visit_expr(rhs, calls);
            }
            Expr::Literal(..) | Expr::String(..) | Expr::Null(_) | Expr::Var { .. } => {}
        }
    }
    for stmt in stmts {
        match stmt {
            Stmt::Let { value, .. } | Stmt::Assign { value, .. } | Stmt::Expr(value) => {
                visit_expr(value, calls)
            }
            Stmt::IndexAssign {
                array,
                index,
                value,
                ..
            } => {
                visit_expr(array, calls);
                visit_expr(index, calls);
                visit_expr(value, calls);
            }
            Stmt::FieldAssign { base, value, .. } => {
                visit_expr(base, calls);
                visit_expr(value, calls);
            }
            Stmt::TupleAssign { base, value, .. } => {
                visit_expr(base, calls);
                visit_expr(value, calls);
            }
            Stmt::Destructure { value, .. } => visit_expr(value, calls),
            Stmt::Match {
                scrutinee,
                arms,
                else_body,
                ..
            } => {
                // Arm paths (`Option.Some`) are patterns, not calls.
                visit_expr(scrutinee, calls);
                for arm in arms {
                    collect_local_calls(&arm.body, calls);
                }
                if let Some(body) = else_body {
                    collect_local_calls(body, calls);
                }
            }
            Stmt::If {
                condition,
                then_body,
                else_body,
                ..
            } => {
                visit_expr(condition, calls);
                collect_local_calls(then_body, calls);
                if let Some(body) = else_body {
                    collect_local_calls(body, calls);
                }
            }
            Stmt::While {
                condition, body, ..
            } => {
                visit_expr(condition, calls);
                collect_local_calls(body, calls);
            }
            Stmt::Return { value, .. } => {
                if let Some(value) = value {
                    visit_expr(value, calls);
                }
            }
            Stmt::Break { .. } | Stmt::Continue { .. } => {}
        }
    }
}

fn function_depends_on_global(
    params: &[vl_syntax::Param],
    body: &[Stmt],
    globals: &std::collections::HashSet<String>,
    types: &std::collections::HashSet<String>,
) -> bool {
    let mut locals = params
        .iter()
        .map(|param| param.name.clone())
        .collect::<std::collections::HashSet<_>>();

    fn expr_depends(
        expr: &Expr,
        locals: &std::collections::HashSet<String>,
        globals: &std::collections::HashSet<String>,
        types: &std::collections::HashSet<String>,
    ) -> bool {
        match expr {
            Expr::Var { path, .. } => {
                path.len() == 1
                    && globals.contains(path[0].as_str())
                    && !locals.contains(path[0].as_str())
            }
            Expr::ArrayLiteral { elems, .. } => elems
                .iter()
                .any(|e| expr_depends(e, locals, globals, types)),
            Expr::ObjectLiteral { fields, .. } => fields
                .iter()
                .any(|(_, _, e)| expr_depends(e, locals, globals, types)),
            Expr::TupleLiteral { elems, .. } => elems
                .iter()
                .any(|(_, _, e)| expr_depends(e, locals, globals, types)),
            Expr::TupleIndex { base, .. } => expr_depends(base, locals, globals, types),
            Expr::Index { base, index, .. } => {
                expr_depends(base, locals, globals, types)
                    || expr_depends(index, locals, globals, types)
            }
            Expr::Field { base, .. }
            | Expr::Unary { rhs: base, .. }
            | Expr::Try { inner: base, .. }
            | Expr::Cast { inner: base, .. } => expr_depends(base, locals, globals, types),
            Expr::Catch { lhs, fallback, .. } => {
                expr_depends(lhs, locals, globals, types)
                    || expr_depends(fallback, locals, globals, types)
            }
            Expr::Call { callee, args, .. } => {
                // Instance sugar reads its receiver: a global head is a
                // global use. A head naming an object type (`Type.method`)
                // is not a value read.
                if callee.len() >= 2
                    && globals.contains(callee[0].as_str())
                    && !locals.contains(callee[0].as_str())
                    && !types.contains(callee[0].as_str())
                {
                    return true;
                }
                args.iter().any(|e| expr_depends(e, locals, globals, types))
            }
            Expr::Binary { lhs, rhs, .. } => {
                expr_depends(lhs, locals, globals, types)
                    || expr_depends(rhs, locals, globals, types)
            }
            Expr::Literal(..) | Expr::String(..) | Expr::Null(_) => false,
        }
    }

    fn stmts_depend(
        stmts: &[Stmt],
        locals: &mut std::collections::HashSet<String>,
        globals: &std::collections::HashSet<String>,
        types: &std::collections::HashSet<String>,
    ) -> bool {
        for stmt in stmts {
            let depends = match stmt {
                Stmt::Let { name, value, .. } => {
                    let depends = expr_depends(value, locals, globals, types);
                    locals.insert(name.clone());
                    depends
                }
                Stmt::Assign { name, value, .. } => {
                    globals.contains(name.as_str()) && !locals.contains(name.as_str())
                        || expr_depends(value, locals, globals, types)
                }
                Stmt::IndexAssign {
                    array,
                    index,
                    value,
                    ..
                } => {
                    expr_depends(array, locals, globals, types)
                        || expr_depends(index, locals, globals, types)
                        || expr_depends(value, locals, globals, types)
                }
                Stmt::FieldAssign { base, value, .. } => {
                    expr_depends(base, locals, globals, types)
                        || expr_depends(value, locals, globals, types)
                }
                Stmt::TupleAssign { base, value, .. } => {
                    expr_depends(base, locals, globals, types)
                        || expr_depends(value, locals, globals, types)
                }
                Stmt::Destructure {
                    value, bindings, ..
                } => {
                    let depends = expr_depends(value, locals, globals, types);
                    for b in bindings {
                        locals.insert(b.binding.clone());
                    }
                    depends
                }
                Stmt::If {
                    condition,
                    then_body,
                    else_body,
                    ..
                } => {
                    expr_depends(condition, locals, globals, types)
                        || stmts_depend(then_body, &mut locals.clone(), globals, types)
                        || else_body.as_ref().is_some_and(|body| {
                            stmts_depend(body, &mut locals.clone(), globals, types)
                        })
                }
                Stmt::Match {
                    scrutinee,
                    arms,
                    else_body,
                    ..
                } => {
                    expr_depends(scrutinee, locals, globals, types)
                        || arms.iter().any(|arm| {
                            let mut arm_locals = locals.clone();
                            for (name, _) in &arm.bindings {
                                arm_locals.insert(name.clone());
                            }
                            stmts_depend(&arm.body, &mut arm_locals, globals, types)
                        })
                        || else_body.as_ref().is_some_and(|body| {
                            stmts_depend(body, &mut locals.clone(), globals, types)
                        })
                }
                Stmt::While {
                    condition, body, ..
                } => {
                    expr_depends(condition, locals, globals, types)
                        || stmts_depend(body, &mut locals.clone(), globals, types)
                }
                Stmt::Return { value, .. } => value
                    .as_ref()
                    .is_some_and(|value| expr_depends(value, locals, globals, types)),
                Stmt::Expr(expr) => expr_depends(expr, locals, globals, types),
                Stmt::Break { .. } | Stmt::Continue { .. } => false,
            };
            if depends {
                return true;
            }
        }
        false
    }

    stmts_depend(body, &mut locals, globals, types)
}

pub fn resolve_with_modules(
    prog: &Program,
    modules: &[ModuleSpec],
) -> (Resolution, Vec<Diagnostic>) {
    let mut r = Resolver {
        scopes: vec![HashMap::new()],
        out: Resolution::default(),
        diags: vec![],
        modules: modules.to_vec(),
        module: prog.module.clone(),
        imports: HashMap::new(),
        import_symbols: HashMap::new(),
        imported_types: HashMap::new(),
        poisoned_imports: std::collections::HashSet::new(),
        import_spans: HashMap::new(),
        poisoned_params: std::collections::HashSet::new(),
        loop_depth: 0,
        local_objects: std::collections::HashSet::new(),
        local_errors: HashMap::new(),
        local_fields: HashMap::new(),
        assoc: HashMap::new(),
        local_unions: HashMap::new(),
    };

    for item in &prog.items {
        if let Item::Use { path, names, span } = item {
            r.resolve_use(path, names.as_deref(), *span);
        }
    }

    // Pass 1: declare top-level names so forward references work.
    // Objects, unions, and error sets share one type namespace: any two
    // same-named declarations are one E200 here.
    let mut object_spans: HashMap<String, Span> = HashMap::new();
    for item in &prog.items {
        let (name, name_span) = match item {
            Item::Object {
                name, name_span, ..
            }
            | Item::Union {
                name, name_span, ..
            }
            | Item::Error {
                name, name_span, ..
            } => (name, name_span),
            _ => continue,
        };
        {
            if let Some(previous) = object_spans.insert(name.clone(), *name_span) {
                r.diags.push(
                    Diagnostic::error(format!("duplicate type `{name}`"))
                        .with_label(*name_span, "redefined here")
                        .with_bare_label(previous)
                        .with_code("E200"),
                );
            }
        }
    }
    // Associated functions are declared under their `Type.method` key (first
    // declaration wins; later duplicates keep their own defs for body
    // resolution so only the E200 above fires). The key never enters the
    // value scope, so a bare `method(...)` never resolves to it.
    for item in &prog.items {
        if let Item::Object {
            name: owner,
            fields,
            methods,
            ..
        } = item
        {
            r.local_objects.insert(owner.clone());
            r.local_fields
                .entry(owner.clone())
                .or_insert_with(|| fields.iter().map(|f| f.name.clone()).collect());
            for m in methods {
                let id = r.out.intern_def_as(
                    format!("{owner}.{}", m.name),
                    m.name_span,
                    DefKind::Local,
                    None,
                );
                r.assoc.entry((owner.clone(), m.name.clone())).or_insert(id);
            }
        }
        // Union variant namespaces: first declaration wins, like associated
        // functions, so later duplicates keep quiet here (the E200 above is
        // the single root cause).
        if let Item::Union { name, variants, .. } = item {
            r.local_unions
                .entry(name.clone())
                .or_insert_with(|| variants.iter().map(|v| v.name.clone()).collect());
        }
        // Error variant namespaces: same first-wins rule as unions.
        if let Item::Error { name, variants, .. } = item {
            r.local_errors
                .entry(name.clone())
                .or_insert_with(|| variants.iter().map(|v| v.name.clone()).collect());
        }
    }
    // A bare type import clashes with a same-named local declaration: one
    // E206, the local type wins, and the import is dropped (no poison) so
    // later stages resolve the bare spelling locally without cascading.
    for bare in r.imported_types.keys().cloned().collect::<Vec<_>>() {
        if let Some(local_span) = object_spans.get(&bare) {
            let import_span = r.import_spans.get(&bare).copied().unwrap_or(*local_span);
            r.diags.push(
                Diagnostic::error(format!(
                    "import alias `{bare}` conflicts with a top-level definition"
                ))
                .with_label(*local_span, "definition declared here")
                .with_bare_label(import_span)
                .with_code("E206"),
            );
            r.imported_types.remove(&bare);
        }
    }
    for item in &prog.items {
        match item {
            Item::Use { .. } => {}
            Item::Object { .. } | Item::Union { .. } | Item::Error { .. } => {}
            Item::Let {
                name,
                name_span,
                kind,
                ..
            } => {
                r.declare_global(name.clone(), *name_span, Some(*kind));
            }
            Item::Destructure { bindings, kind, .. } => {
                for b in bindings {
                    r.declare_global(b.binding.clone(), b.binding_span, Some(*kind));
                }
            }
            Item::Function {
                name, name_span, ..
            } => {
                r.declare_global(name.clone(), *name_span, None);
            }
        }
    }

    // Pass 2: resolve bodies.
    for item in &prog.items {
        match item {
            Item::Use { .. } => {}
            Item::Object { methods, .. } => {
                for m in methods {
                    r.resolve_fn_body(&m.params, &m.body);
                }
            }
            Item::Union { .. } | Item::Error { .. } => {}
            Item::Let { value, .. } => {
                r.resolve_expr(value);
            }
            Item::Destructure { value, .. } => {
                r.resolve_expr(value);
            }
            Item::Function { params, body, .. } => {
                r.resolve_fn_body(params, body);
            }
        }
    }

    r.out.poisoned_imports = !r.poisoned_imports.is_empty();
    r.out.imported_types = r.imported_types.clone();
    (r.out, r.diags)
}

impl Resolver {
    /// Resolve one function-like body: fresh parameter scope, duplicate
    /// parameter check, then every statement. Shared by free functions and
    /// associated methods (`self` is an ordinary parameter here).
    fn resolve_fn_body(&mut self, params: &[vl_syntax::Param], body: &[Stmt]) {
        self.scopes.push(HashMap::new());
        for p in params {
            if self.live_import_span(&p.name).is_some() {
                self.import_shadow_error(&p.name, p.name_span, "a parameter");
            }
            if self
                .scopes
                .last()
                .is_some_and(|scope| scope.contains_key(&p.name))
            {
                let previous = self
                    .scopes
                    .last()
                    .and_then(|scope| scope.get(&p.name))
                    .and_then(|id| self.out.defs.iter().find(|d| d.id == *id))
                    .map(|d| d.span);
                let mut diagnostic = Diagnostic::error(format!("duplicate parameter `{}`", p.name))
                    .with_label(p.name_span, "redefined here")
                    .with_code("E200");
                if let Some(previous) = previous {
                    diagnostic = diagnostic.with_bare_label(previous);
                }
                self.diags.push(diagnostic);
                // The surviving binding is ambiguous; suppress E205
                // for it so the duplicate stays the one root cause.
                if let Some(id) = self.scopes.last().and_then(|s| s.get(&p.name)) {
                    self.poisoned_params.insert(id.0);
                }
                continue;
            }
            let id = self.out.intern_param(p.name.clone(), p.name_span);
            self.scopes.last_mut().unwrap().insert(p.name.clone(), id);
        }
        for stmt in body {
            self.resolve_stmt(stmt);
        }
        self.scopes.pop();
    }

    fn poison_import(&mut self, alias: impl Into<String>, span: Span) {
        let alias = alias.into();
        self.poisoned_imports.insert(alias.clone());
        self.import_spans.entry(alias).or_insert(span);
    }

    fn reserve_import_alias(&mut self, alias: &str, span: Span) -> bool {
        if !self.poisoned_imports.contains(alias)
            && !self.imports.contains_key(alias)
            && !self.import_symbols.contains_key(alias)
        {
            return false;
        }
        let mut diagnostic = Diagnostic::error(format!("duplicate import alias `{alias}`"))
            .with_label(span, "imported again here")
            .with_code("E206");
        if let Some(previous) = self.import_spans.get(alias) {
            diagnostic = diagnostic.with_bare_label(*previous);
        }
        self.diags.push(diagnostic);
        self.poisoned_imports.insert(alias.to_string());
        true
    }

    /// Bring one type name into scope (`use vl.dog.{Dog}` then bare `Dog`
    /// means `vl.dog.Dog`). Types share one namespace across objects and
    /// unions, but never conflict with same-named functions or values.
    /// Importing the same spelling from two modules is one E206; the
    /// first mapping is kept so downstream stays quiet behind the single
    /// root cause. Clashes with a local declaration are settled after
    /// the declaration pre-pass below (the local type wins).
    fn import_type(&mut self, bare: String, qualified: String, span: Span) {
        if let Some(existing) = self.imported_types.get(&bare) {
            if *existing == qualified {
                return;
            }
            let mut diagnostic = Diagnostic::error(format!("duplicate import alias `{bare}`"))
                .with_label(span, "imported again here")
                .with_code("E206");
            if let Some(previous) = self.import_spans.get(&bare) {
                diagnostic = diagnostic.with_bare_label(*previous);
            }
            self.diags.push(diagnostic);
            self.poison_import(bare, span);
            return;
        }
        if self.poisoned_imports.contains(&bare) {
            return;
        }
        self.imported_types.insert(bare.clone(), qualified);
        self.import_spans.insert(bare, span);
    }

    fn declare_global(&mut self, name: String, span: Span, binding: Option<BindingKind>) {
        if self.poisoned_imports.contains(&name) {
            let mut diagnostic = Diagnostic::error(format!(
                "import alias `{name}` conflicts with a top-level definition"
            ))
            .with_label(span, "definition declared here")
            .with_code("E206");
            if let Some(previous) = self.import_spans.get(&name) {
                diagnostic = diagnostic.with_bare_label(*previous);
            }
            self.diags.push(diagnostic);
            return;
        }
        if self.imports.contains_key(&name) || self.import_symbols.contains_key(&name) {
            self.diags.push(
                Diagnostic::error(format!(
                    "import alias `{name}` conflicts with a top-level definition"
                ))
                .with_label(span, "definition declared here")
                .with_code("E206"),
            );
            self.poisoned_imports.insert(name);
            return;
        }
        let global = &mut self.scopes[0];
        if let Some(prev) = global
            .get(&name)
            .and_then(|id| self.out.defs.iter().find(|d| d.id == *id).map(|d| d.span))
        {
            self.diags.push(
                Diagnostic::error(format!("duplicate definition of `{name}`"))
                    .with_label(span, "redefined here")
                    .with_bare_label(prev)
                    .with_code("E200"),
            );
            return;
        }
        let id = self
            .out
            .intern_def_as(name.clone(), span, DefKind::Local, binding);
        global.insert(name, id);
    }

    /// Import span when `name` names a live (unpoisoned) import alias.
    /// Poisoned aliases stay quiet here so the original E202/E203/E206/E208
    /// remains the single root cause.
    fn live_import_span(&self, name: &str) -> Option<Span> {
        if self.poisoned_imports.contains(name) {
            return None;
        }
        if self.imports.contains_key(name) || self.import_symbols.contains_key(name) {
            return self.import_spans.get(name).copied();
        }
        None
    }

    fn import_shadow_error(&mut self, name: &str, span: Span, what: &str) {
        let mut diagnostic =
            Diagnostic::error(format!("import alias `{name}` conflicts with {what}"))
                .with_label(span, "definition declared here")
                .with_code("E206");
        if let Some(previous) = self.import_spans.get(name) {
            diagnostic = diagnostic.with_bare_label(*previous);
        }
        self.diags.push(diagnostic);
    }

    fn declare_local(&mut self, name: String, span: Span, binding: BindingKind) {
        if self.live_import_span(&name).is_some() {
            self.import_shadow_error(&name, span, "a local definition");
        }
        let top = self.scopes.last_mut().unwrap();
        if top.contains_key(&name) {
            self.diags.push(
                Diagnostic::warning(format!("`{name}` shadows a previous binding"))
                    .with_label(span, "shadowing definition"),
            );
        }
        let id = self
            .out
            .intern_def_as(name.clone(), span, DefKind::Local, Some(binding));
        top.insert(name, id);
    }

    fn lookup(&self, name: &str) -> Option<DefId> {
        for scope in self.scopes.iter().rev() {
            if let Some(id) = scope.get(name) {
                return Some(id.clone());
            }
        }
        None
    }

    fn resolve_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Let {
                name,
                name_span,
                kind,
                value,
                ..
            } => {
                self.resolve_expr(value);
                self.declare_local(name.clone(), *name_span, *kind);
            }
            Stmt::Assign {
                name,
                name_span,
                value,
                ..
            } => {
                self.resolve_expr(value);
                match self.lookup(name) {
                    Some(id) => {
                        self.out
                            .uses
                            .insert((name_span.start, name_span.end), id.clone());
                        // Parameters are fixed bindings: direct rebinding is
                        // rejected here (E205). Field/index mutation through a
                        // parameter (`p.field = ...`, `p[i] = ...`) is a
                        // referent mutation decided by type checking, not here.
                        // Duplicated parameters already have one root cause
                        // (E200), so E205 stays quiet for them.
                        if self.poisoned_params.contains(&id.0) {
                            return;
                        }
                        if let Some(def) = self.out.defs.iter().find(|d| d.id == id) {
                            if def.kind == DefKind::Parameter
                                || def.binding == Some(BindingKind::Val)
                            {
                                let message = if def.kind == DefKind::Parameter {
                                    format!("cannot rebind parameter `{name}`")
                                } else {
                                    format!("cannot rebind `val` binding `{name}`")
                                };
                                let label = if def.kind == DefKind::Parameter {
                                    "parameters are fixed bindings"
                                } else {
                                    "`val` bindings are fixed"
                                };
                                let note = if def.kind == DefKind::Parameter {
                                    "parameters cannot be assigned; use a local `var` when rebinding is needed"
                                } else {
                                    "`val` bindings cannot be assigned; use `var` when rebinding is needed"
                                };
                                self.diags.push(
                                    Diagnostic::error(message)
                                        .with_label(*name_span, label)
                                        .with_note(note)
                                        .with_code("E205"),
                                );
                            }
                        }
                    }
                    None => {
                        self.diags.push(
                            Diagnostic::error(format!("cannot find `{name}` in this scope"))
                                .with_label(*name_span, "undefined variable")
                                .with_note("did you mean to `var`/`val`-bind it first?")
                                .with_code("E201"),
                        );
                    }
                }
            }
            Stmt::IndexAssign {
                array,
                index,
                value,
                ..
            } => {
                // Element write: every side is an expression (the array base
                // is usually a variable, resolved through `resolve_expr`).
                self.resolve_expr(array);
                self.resolve_expr(index);
                self.resolve_expr(value);
            }
            Stmt::FieldAssign { base, value, .. } => {
                self.resolve_expr(base);
                self.resolve_expr(value);
            }
            Stmt::TupleAssign { base, value, .. } => {
                self.resolve_expr(base);
                self.resolve_expr(value);
            }
            Stmt::Destructure {
                bindings,
                kind,
                value,
                ..
            } => {
                self.resolve_expr(value);
                for b in bindings {
                    self.declare_local(b.binding.clone(), b.binding_span, *kind);
                }
            }
            Stmt::Expr(e) => self.resolve_expr(e),
            Stmt::Return { value, .. } => {
                if let Some(e) = value {
                    self.resolve_expr(e);
                }
            }
            Stmt::Break { span } => {
                if self.loop_depth == 0 {
                    self.diags.push(
                        Diagnostic::error("`break` outside of a loop")
                            .with_label(*span, "no enclosing `while`")
                            .with_code("E204"),
                    );
                }
            }
            Stmt::Continue { span } => {
                if self.loop_depth == 0 {
                    self.diags.push(
                        Diagnostic::error("`continue` outside of a loop")
                            .with_label(*span, "no enclosing `while`")
                            .with_code("E204"),
                    );
                }
            }
            Stmt::While {
                condition, body, ..
            } => {
                self.resolve_expr(condition);
                self.loop_depth += 1;
                self.scopes.push(HashMap::new());
                for stmt in body {
                    self.resolve_stmt(stmt);
                }
                self.scopes.pop();
                self.loop_depth -= 1;
            }
            Stmt::If {
                condition,
                then_body,
                else_body,
                ..
            } => {
                self.resolve_expr(condition);
                self.scopes.push(HashMap::new());
                for stmt in then_body {
                    self.resolve_stmt(stmt);
                }
                self.scopes.pop();
                if let Some(body) = else_body {
                    self.scopes.push(HashMap::new());
                    for stmt in body {
                        self.resolve_stmt(stmt);
                    }
                    self.scopes.pop();
                }
            }
            Stmt::Match {
                scrutinee,
                arms,
                else_body,
                ..
            } => {
                // The scrutinee lives in the enclosing scope; each arm binds
                // its payload names as implicit `val`s scoped to that arm.
                // Union/variant existence is validated by typechecking (which
                // sees qualified and imported unions); scoping here must
                // still declare the bindings so bodies resolve. Import-alias
                // heads (`alias.Union.Variant`) canonicalize to the
                // module-qualified spelling for later stages.
                self.resolve_expr(scrutinee);
                for arm in arms {
                    if arm.path.len() >= 2 {
                        let (head, _) = arm.path.split_at(arm.path.len() - 1);
                        if let Some((canonical, _)) = self.canonical_union_head(head) {
                            let head_joined = head.join(".");
                            if canonical != head_joined {
                                self.out
                                    .match_patterns
                                    .insert((arm.path_span.start, arm.path_span.end), canonical);
                            }
                        }
                    }
                    self.scopes.push(HashMap::new());
                    let mut seen = std::collections::HashSet::new();
                    for (name, span) in &arm.bindings {
                        if !seen.insert(name.clone()) {
                            self.diags.push(
                                Diagnostic::error(format!(
                                    "duplicate binding `{name}` in match arm"
                                ))
                                .with_label(*span, "redefined here")
                                .with_code("E200"),
                            );
                            continue;
                        }
                        self.declare_local(name.clone(), *span, BindingKind::Val);
                    }
                    for stmt in &arm.body {
                        self.resolve_stmt(stmt);
                    }
                    self.scopes.pop();
                }
                if let Some(body) = else_body {
                    self.scopes.push(HashMap::new());
                    for stmt in body {
                        self.resolve_stmt(stmt);
                    }
                    self.scopes.pop();
                }
            }
        }
    }

    fn resolve_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Literal(_, _) | Expr::String(_, _) | Expr::Null(_) => {}
            Expr::ObjectLiteral { fields, .. } => {
                for (_, _, value) in fields {
                    self.resolve_expr(value);
                }
            }
            Expr::ArrayLiteral { elems, .. } => {
                for elem in elems {
                    self.resolve_expr(elem);
                }
            }
            Expr::TupleLiteral { elems, .. } => {
                for (_, _, value) in elems {
                    self.resolve_expr(value);
                }
            }
            Expr::TupleIndex { base, .. } => self.resolve_expr(base),
            Expr::Index { base, index, .. } => {
                self.resolve_expr(base);
                self.resolve_expr(index);
            }
            Expr::Field { base, name, span } => {
                // A nullary variant use (`Option.None`, `alias.Option.None`,
                // `mod.Option.None`): the spine heads a union, not a value.
                // Record the site for HIR lowering and stay quiet here;
                // typechecking validates arity (a payload-carrying variant
                // without `(...)` is an error there).
                if let Some(spine) = Self::field_spine(base, name) {
                    if spine.len() >= 2 {
                        let (head, variant) = spine.split_at(spine.len() - 1);
                        if let Some((canonical, variants)) = self.canonical_union_head(head) {
                            let display = head.join(".");
                            self.record_variant_use(
                                *span,
                                canonical,
                                &variants,
                                &variant[0],
                                &display,
                            );
                            return;
                        }
                        // An error value (`E.NotFound`): the spine heads an
                        // error set, not a value. Record the site for HIR
                        // lowering; typechecking validates set membership.
                        if let Some((canonical, variants)) = self.canonical_error_head(head) {
                            let display = head.join(".");
                            self.record_error_use(
                                *span,
                                canonical,
                                &variants,
                                &variant[0],
                                &display,
                            );
                            return;
                        }
                    }
                }
                self.resolve_expr(base)
            }
            Expr::Var { path, span } => match self.lookup_path(path, *span) {
                Some(id) => {
                    self.out.uses.insert((span.start, span.end), id);
                }
                None => {
                    self.diags.push(
                        Diagnostic::error(format!(
                            "cannot find `{}` in this scope",
                            path.join(".")
                        ))
                        .with_label(*span, "undefined variable")
                        .with_note("did you mean to `var`/`val`-bind it first?")
                        .with_code("E201"),
                    );
                }
            },
            Expr::Call {
                callee,
                callee_span,
                type_args,
                type_args_span,
                args,
                ..
            } => {
                // `Array.new::[T](count)` is a builtin constructor: it needs
                // no import and resolves to a synthetic external def carrying
                // its signature, so typechecking and HIR reuse the normal
                // extern-call path (LIR desugars it to an allocation).
                // A bare `Array.new(count)` carries no type argument: it
                // resolves the same way with no signature, and typechecking
                // either infers `T` from an annotated binding or reports E303.
                if callee.len() == 2 && callee[0] == "Array" && callee[1] == "new" {
                    let [elem] = type_args.as_slice() else {
                        if type_args.len() > 1 {
                            self.diags.push(
                                Diagnostic::error(format!(
                                    "`Array.new` expects exactly one type argument, got {}",
                                    type_args.len()
                                ))
                                .with_label(
                                    type_args_span.unwrap_or(*callee_span),
                                    "write `Array.new::[T](count)`, e.g. `Array.new::[u64](3)`",
                                )
                                .with_code("E303"),
                            );
                        }
                        let id = self.external_def(
                            callee.join("."),
                            *callee_span,
                            None,
                            DefKind::External,
                            None,
                        );
                        self.out
                            .uses
                            .insert((callee_span.start, callee_span.end), id);
                        for arg in args {
                            self.resolve_expr(arg);
                        }
                        return;
                    };
                    let sig = vl_common::FuncSig::new(
                        &[("count", vl_common::VlType::U64)],
                        vl_common::VlType::Array(Box::new(elem.clone())),
                    );
                    let id = self.external_def(
                        callee.join("."),
                        *callee_span,
                        Some(sig),
                        DefKind::External,
                        None,
                    );
                    self.out
                        .uses
                        .insert((callee_span.start, callee_span.end), id);
                    for arg in args {
                        self.resolve_expr(arg);
                    }
                    return;
                }
                // Removed predecessor: point at the replacement instead of a
                // bare "undefined function".
                if callee.first().is_some_and(|head| head == "U64Array") {
                    self.diags.push(
                        Diagnostic::error(format!(
                            "cannot find `{}` in this scope",
                            callee.join(".")
                        ))
                        .with_label(*callee_span, "`U64Array` was removed")
                        .with_note("use `Array[u64]` and `Array.new::[u64](n)` instead")
                        .with_code("E201"),
                    );
                    for arg in args {
                        self.resolve_expr(arg);
                    }
                    return;
                }
                // Error variants take no arguments: `E.V(args)` is one E303
                // here, and the site still records so HIR lowers the error
                // value (lowering is blocked on this error anyway).
                if callee.len() >= 2 {
                    let (head, variant) = callee.split_at(callee.len() - 1);
                    if let Some((canonical, variants)) = self.canonical_error_head(head) {
                        let display = head.join(".");
                        if self.record_error_use(
                            *callee_span,
                            canonical,
                            &variants,
                            &variant[0],
                            &display,
                        ) {
                            self.diags.push(
                                Diagnostic::error(format!(
                                    "error variant `{display}.{}` takes no arguments",
                                    variant[0]
                                ))
                                .with_label(*callee_span, "remove the `(...)` arguments")
                                .with_note("error variants carry no data in this milestone")
                                .with_code("E303"),
                            );
                        }
                        for arg in args {
                            self.resolve_expr(arg);
                        }
                        return;
                    }
                }
                // Associated functions live in the type namespace: `Type.func`
                // resolves through the object tables (no import needed, like
                // `Array.new`). A `value.method` call whose head is a bound
                // value defers to typechecking as instance sugar.
                if self.resolve_assoc_or_sugar_call(callee, *callee_span, args) {
                    return;
                }
                // Callee is a plain name use so `fun` items resolve
                // (including forward references via the global pre-pass).
                match self.lookup_path(callee, *callee_span) {
                    Some(id) => {
                        self.out
                            .uses
                            .insert((callee_span.start, callee_span.end), id);
                    }
                    None => {
                        self.diags.push(
                            Diagnostic::error(format!(
                                "cannot find `{}` in this scope",
                                callee.join(".")
                            ))
                            .with_label(*callee_span, "undefined function")
                            .with_note("did you mean to `fun`-define it first?")
                            .with_code("E201"),
                        );
                    }
                }
                for arg in args {
                    self.resolve_expr(arg);
                }
            }
            Expr::Unary { rhs, .. } => self.resolve_expr(rhs),
            Expr::Try { inner, .. } => self.resolve_expr(inner),
            Expr::Binary { lhs, rhs, .. } => {
                self.resolve_expr(lhs);
                self.resolve_expr(rhs);
            }
            Expr::Catch { lhs, fallback, .. } => {
                self.resolve_expr(lhs);
                self.resolve_expr(fallback);
            }
            Expr::Cast { inner, .. } => self.resolve_expr(inner),
        }
    }

    fn resolve_use(&mut self, path: &[String], names: Option<&[String]>, span: Span) {
        let key = path.join(".");
        let Some(module) = self
            .modules
            .iter()
            .find(|m| m.path.as_string() == key)
            .cloned()
        else {
            // `use std.print;` is a single-export import: the parent path is
            // a known module and the leaf is one of its exports. It behaves
            // like `use std.{print};` and brings `print` into scope.
            if names.is_none() && path.len() >= 2 {
                let parent_key = path[..path.len() - 1].join(".");
                let leaf = path[path.len() - 1].clone();
                if let Some(parent) = self
                    .modules
                    .iter()
                    .find(|m| m.path.as_string() == parent_key)
                    .cloned()
                {
                    if self.reserve_import_alias(&leaf, span) {
                        return;
                    }
                    if parent.poisoned_exports.iter().any(|export| export == &leaf) {
                        self.poison_import(leaf, span);
                        return;
                    }
                    if parent.lookup(&leaf).is_some() {
                        if parent
                            .global_dependent_exports
                            .iter()
                            .any(|name| name == &leaf)
                        {
                            self.diags.push(
                                Diagnostic::error(format!(
                                    "imported function `{key}` depends on module globals"
                                ))
                                .with_label(span, "unsupported cross-module boundary")
                                .with_code("E208"),
                            );
                            self.poison_import(leaf.clone(), span);
                            return;
                        }
                        self.import_symbols.insert(
                            leaf.clone(),
                            SymbolRef {
                                module: parent.path.clone(),
                                name: leaf.clone(),
                            },
                        );
                        self.import_spans.insert(leaf.clone(), span);
                        self.imports.insert(
                            leaf.clone(),
                            ModuleSpec {
                                path: vl_common::ModulePath::new(vec![parent_key, leaf]),
                                exports: vec![],
                                objects: vec![],
                                unions: vec![],
                                errors: vec![],
                                parse_poisoned: parent.parse_poisoned,
                                poisoned_exports: vec![],
                                global_dependent_exports: vec![],
                            },
                        );
                        return;
                    }
                    // Single-type import (`use vl.dog.Dog;` then bare `Dog`):
                    // types never enter the value scope, so no alias
                    // reservation or synthetic module entry is needed.
                    if let Some(qualified) = parent.lookup_type_qualified(&leaf) {
                        let qualified = qualified.to_owned();
                        self.import_type(leaf, qualified, span);
                        return;
                    }
                    self.poison_import(leaf.clone(), span);
                    self.poison_import(key.clone(), span);
                    if parent.parse_poisoned {
                        return;
                    }
                    self.diags.push(
                        Diagnostic::error(format!("module `{parent_key}` has no export `{leaf}`"))
                            .with_label(span, "unknown module export")
                            .with_code("E203"),
                    );
                    return;
                }
            }
            if let Some(names) = names {
                for name in names {
                    if self.reserve_import_alias(name, span) {
                        continue;
                    }
                    self.poison_import(name.clone(), span);
                }
            } else if let Some(name) = path.last() {
                if self.reserve_import_alias(name, span) {
                    return;
                }
                self.poison_import(name.clone(), span);
            }
            self.poison_import(key.clone(), span);
            self.diags.push(
                Diagnostic::error(format!("cannot find module `{key}`"))
                    .with_label(span, "unknown module")
                    .with_code("E202"),
            );
            return;
        };
        match names {
            None => {
                let alias = path.last().cloned().unwrap_or_default();
                if !self.reserve_import_alias(&alias, span) {
                    self.imports.insert(alias, module);
                    self.import_spans
                        .insert(path.last().cloned().unwrap_or_default(), span);
                }
            }
            Some(names) => {
                for name in names {
                    // `self` in a braced list imports the module itself under
                    // its leaf name (`use std.net.tcp.{self, TcpError}` also
                    // brings `tcp` into scope, like `use std.net.tcp;`).
                    if name == "self" {
                        let alias = path.last().cloned().unwrap_or_default();
                        if !self.reserve_import_alias(&alias, span) {
                            self.imports.insert(alias.clone(), module.clone());
                            self.import_spans.insert(alias, span);
                        }
                        continue;
                    }
                    // Types and functions share the `use m.{name}` spelling
                    // but live in separate namespaces: import whatever the
                    // module exports under this spelling (both, either, or
                    // neither). Only a missing-everywhere name is E203.
                    let qualified_type = module.lookup_type_qualified(name).map(str::to_owned);
                    if let Some(qualified) = qualified_type {
                        self.import_type(name.clone(), qualified, span);
                    }
                    if module.lookup(name).is_none() {
                        if module.lookup_type_qualified(name).is_some() {
                            // Pure type import: no value binding to reserve.
                            continue;
                        }
                        if self.reserve_import_alias(name, span) {
                            continue;
                        }
                        if module.poisoned_exports.iter().any(|export| export == name) {
                            self.poison_import(name.clone(), span);
                            continue;
                        }
                        self.poison_import(name.clone(), span);
                        self.poison_import(format!("{key}.{name}"), span);
                        if module.parse_poisoned {
                            continue;
                        }
                        self.diags.push(
                            Diagnostic::error(format!("module `{key}` has no export `{name}`"))
                                .with_label(span, "unknown module export")
                                .with_code("E203"),
                        );
                    } else {
                        if self.reserve_import_alias(name, span) {
                            continue;
                        }
                        if module
                            .global_dependent_exports
                            .iter()
                            .any(|export| export == name)
                        {
                            self.diags.push(
                                Diagnostic::error(format!(
                                    "imported function `{key}.{name}` depends on module globals"
                                ))
                                .with_label(span, "unsupported cross-module boundary")
                                .with_code("E208"),
                            );
                            self.poison_import(name.clone(), span);
                            continue;
                        }
                        self.import_symbols.insert(
                            name.clone(),
                            SymbolRef {
                                module: module.path.clone(),
                                name: name.clone(),
                            },
                        );
                        self.import_spans.insert(name.clone(), span);
                    }
                }
            }
        }
    }

    /// Resolve the compiler-owned signature for a bare imported name
    /// (`use std.print;` then `print()`). The synthetic import path encodes
    /// the parent module + leaf (`std.print`), which we split to find the
    /// original export.
    fn sig_for_bare_import(
        &self,
        alias: &str,
    ) -> Option<(vl_common::FuncSig, vl_common::ExportKind)> {
        if let Some(symbol) = self.import_symbols.get(alias) {
            return self
                .modules
                .iter()
                .find(|m| m.path == symbol.module)
                .and_then(|m| m.lookup(&symbol.name))
                .map(|e| (e.sig.clone(), e.kind));
        }
        // An exact module import is an alias, not a synthetic import of the
        // parent's same-named export. It is qualified-callable only.
        if self.imports.contains_key(alias) {
            return None;
        }
        let synthetic = self.imports.get(alias)?;
        // Synthetic singletons carry no exports; real module aliases (e.g.
        // `use std.string`) do and are not bare-callable.
        if !synthetic.exports.is_empty() {
            return None;
        }
        let full = synthetic.path.as_string();
        let (parent_key, leaf) = full.rsplit_once('.')?;
        // `ModulePath::new(vec![key, name])` stores the dotted key as one
        // segment, so split the string form rather than the segments.
        let parent = self
            .modules
            .iter()
            .find(|m| m.path.as_string() == parent_key)?;
        parent.lookup(leaf).map(|e| (e.sig.clone(), e.kind))
    }

    fn lookup_path(&mut self, path: &[String], span: Span) -> Option<DefId> {
        if path.len() == 1 {
            if let Some(id) = self.lookup(&path[0]) {
                return Some(id);
            }
            if self.poisoned_imports.contains(&path[0]) {
                return Some(self.external_def(
                    path[0].clone(),
                    span,
                    None,
                    DefKind::ImportedFunction,
                    None,
                ));
            }
            if self.import_symbols.contains_key(&path[0]) {
                let symbol = self.import_symbols.get(&path[0]).cloned();
                let resolved = self.sig_for_bare_import(&path[0]);
                let (sig, export_kind) = resolved
                    .map(|(s, k)| (Some(s), Some(k)))
                    .unwrap_or((None, None));
                return Some(self.external_def_with_kind(
                    path[0].clone(),
                    span,
                    sig,
                    export_kind,
                    DefKind::ImportedFunction,
                    symbol,
                ));
            }
            if self.imports.contains_key(&path[0]) {
                let resolved = self.sig_for_bare_import(&path[0]);
                let (sig, export_kind) = resolved
                    .map(|(s, k)| (Some(s), Some(k)))
                    .unwrap_or((None, None));
                return Some(self.external_def_with_kind(
                    path[0].clone(),
                    span,
                    sig,
                    export_kind,
                    if self.import_symbols.contains_key(&path[0]) {
                        DefKind::ImportedFunction
                    } else {
                        DefKind::ModuleAlias
                    },
                    self.import_symbols.get(&path[0]).cloned(),
                ));
            }
            if self.poisoned_imports.contains(&path[0]) {
                return Some(self.external_def(
                    path[0].clone(),
                    span,
                    None,
                    DefKind::ImportedFunction,
                    None,
                ));
            }
            return None;
        }
        if self.poisoned_imports.contains(&path.join("."))
            || self.poisoned_imports.contains(&path[0])
        {
            return Some(self.external_def(
                path.join("."),
                span,
                None,
                DefKind::ImportedFunction,
                None,
            ));
        }
        // `use missing.module;` poisons the imported alias (`module`), not
        // only the full source path. Treat qualified uses through that alias
        // as poisoned too, so the E202 root cause does not cascade into E201.
        if self.poisoned_imports.contains(&path[0]) {
            return Some(self.external_def(
                path.join("."),
                span,
                None,
                DefKind::ImportedFunction,
                None,
            ));
        }
        let module = self.imports.get(&path[0]).cloned()?;
        if path.len() != 2 {
            return None;
        }
        let Some(export) = module.lookup(&path[1]) else {
            if module.poisoned_exports.iter().any(|name| name == &path[1]) {
                self.poisoned_imports.insert(path.join("."));
                return Some(self.external_def(
                    path.join("."),
                    span,
                    None,
                    DefKind::ImportedFunction,
                    None,
                ));
            }
            if module.parse_poisoned {
                self.poisoned_imports.insert(path.join("."));
                return Some(self.external_def(
                    path.join("."),
                    span,
                    None,
                    DefKind::ImportedFunction,
                    None,
                ));
            }
            return None;
        };
        if module
            .global_dependent_exports
            .iter()
            .any(|name| name == &path[1])
        {
            self.diags.push(
                Diagnostic::error(format!(
                    "imported function `{}.{}` depends on module globals",
                    module.path.as_string(),
                    path[1]
                ))
                .with_label(span, "unsupported cross-module boundary")
                .with_code("E208"),
            );
            self.poisoned_imports.insert(path.join("."));
            return Some(self.external_def(
                path.join("."),
                span,
                None,
                DefKind::ImportedFunction,
                None,
            ));
        }
        let sig = export.sig.clone();
        let export_kind = export.kind;
        Some(self.external_def_with_kind(
            path.join("."),
            span,
            Some(sig),
            Some(export_kind),
            DefKind::ImportedFunction,
            Some(SymbolRef {
                module: module.path.clone(),
                name: path[1].clone(),
            }),
        ))
    }

    /// Canonical union name for a `Union.Variant` head (`parts` = every
    /// segment but the last). Returns the canonical spelling plus the
    /// variant list: bare for local unions, module-qualified for imported
    /// ones (matching `typed.unions` keys downstream). Own-module qualified
    /// spellings (`m.Union` inside `m`) fold to bare so one spelling
    /// identifies local values.
    fn canonical_union_head(&self, parts: &[String]) -> Option<(String, Vec<String>)> {
        if parts.len() == 1 {
            if let Some(variants) = self.local_unions.get(&parts[0]) {
                return Some((parts[0].clone(), variants.clone()));
            }
            // Bare imported union (`use vl.types.{U}` then `U.Variant`):
            // canonicalize to the module-qualified identity. An explicit
            // import shadows the builtin `Option`, like a local
            // `type Option` declaration does.
            if let Some(qualified) = self.imported_types.get(&parts[0]) {
                for spec in &self.modules {
                    if let Some(found) = spec.unions.iter().find(|u| u.qualified == *qualified) {
                        return Some((
                            found.qualified.clone(),
                            found.variants.iter().map(|v| v.name.clone()).collect(),
                        ));
                    }
                }
            }
            // Builtin `Option` (nullable `?T` / `null` sugar): available in
            // every module without a `type Option` declaration. A local
            // `type Option` shadows it (first declaration wins above).
            if parts[0] == "Option" {
                return Some(("Option".to_string(), builtin_option_variants()));
            }
            return None;
        }
        // Alias-qualified `alias.Union` (exactly two segments).
        if parts.len() == 2 && self.imports.contains_key(&parts[0]) {
            let spec = self.imports.get(&parts[0]).cloned()?;
            let found = spec.unions.iter().find(|u| u.name == parts[1])?;
            return Some((
                found.qualified.clone(),
                found.variants.iter().map(|v| v.name.clone()).collect(),
            ));
        }
        let joined = parts.join(".");
        // Own-module qualified: fold to the bare local spelling.
        if let Some(rest) = joined.strip_prefix(&format!("{}.", self.module)) {
            if let Some(variants) = self.local_unions.get(rest) {
                return Some((rest.to_string(), variants.clone()));
            }
        }
        // Fully qualified without an import.
        for spec in &self.modules {
            if let Some(found) = spec.unions.iter().find(|u| u.qualified == joined) {
                return Some((
                    joined.clone(),
                    found.variants.iter().map(|v| v.name.clone()).collect(),
                ));
            }
        }
        None
    }

    /// Flatten a field spine (`a.b.C`) into segments in order when the whole
    /// spine is plain names (no calls or indices); `None` otherwise.
    fn field_spine(base: &Expr, outer: &str) -> Option<Vec<String>> {
        let mut rev = vec![outer.to_string()];
        let mut head = base;
        loop {
            match head {
                Expr::Field {
                    base: inner, name, ..
                } => {
                    rev.push(name.clone());
                    head = inner;
                }
                Expr::Var { path, .. } => {
                    for seg in path.iter().rev() {
                        rev.push(seg.clone());
                    }
                    rev.reverse();
                    return Some(rev);
                }
                _ => return None,
            }
        }
    }

    /// Record one variant construction site (`at` = call callee span or field
    /// span) after checking the variant exists. Always returns true (the
    /// site is fully handled: one E302 at most, then quiet).
    fn record_variant_use(
        &mut self,
        at: Span,
        canonical: String,
        variants: &[String],
        variant: &str,
        union_display: &str,
    ) -> bool {
        if !variants.iter().any(|v| v == variant) {
            self.diags.push(
                Diagnostic::error(format!(
                    "union `{union_display}` has no variant `{variant}`"
                ))
                .with_label(at, "unknown union variant")
                .with_code("E302"),
            );
        } else {
            self.out.variants.insert(
                (at.start, at.end),
                VariantUse {
                    union: canonical,
                    variant: variant.to_string(),
                },
            );
        }
        true
    }

    /// Canonical error-set name for an `E.Variant` head (`parts` = every
    /// segment but the last). Returns the canonical spelling plus the
    /// variant list: bare for local sets, module-qualified for imported
    /// ones (matching `typed.errors` keys downstream). Own-module qualified
    /// spellings (`m.E` inside `m`) fold to bare so one spelling
    /// identifies local values. Mirrors
    /// [`canonical_union_head`](Self::canonical_union_head) minus the
    /// builtin (there is no builtin error set).
    fn canonical_error_head(&self, parts: &[String]) -> Option<(String, Vec<String>)> {
        if parts.len() == 1 {
            if let Some(variants) = self.local_errors.get(&parts[0]) {
                return Some((parts[0].clone(), variants.clone()));
            }
            // Bare imported set (`use vl.io.{Io}` then `Io.Variant`):
            // canonicalize to the module-qualified identity.
            if let Some(qualified) = self.imported_types.get(&parts[0]) {
                for spec in &self.modules {
                    if let Some(found) = spec.errors.iter().find(|e| e.qualified == *qualified) {
                        return Some((found.qualified.clone(), found.variants.clone()));
                    }
                }
            }
            return None;
        }
        // Alias-qualified `alias.Set` (exactly two segments).
        if parts.len() == 2 && self.imports.contains_key(&parts[0]) {
            let spec = self.imports.get(&parts[0]).cloned()?;
            let found = spec.errors.iter().find(|e| e.name == parts[1])?;
            return Some((found.qualified.clone(), found.variants.clone()));
        }
        let joined = parts.join(".");
        // Own-module qualified: fold to the bare local spelling.
        if let Some(rest) = joined.strip_prefix(&format!("{}.", self.module)) {
            if let Some(variants) = self.local_errors.get(rest) {
                return Some((rest.to_string(), variants.clone()));
            }
        }
        // Fully qualified without an import.
        for spec in &self.modules {
            if let Some(found) = spec.errors.iter().find(|e| e.qualified == joined) {
                return Some((joined.clone(), found.variants.clone()));
            }
        }
        None
    }

    /// Record one error-value use site (`at` = call callee span or field
    /// span) after checking the variant exists. Returns whether the variant
    /// is known (the site is fully handled either way: one E302 at most,
    /// then quiet).
    fn record_error_use(
        &mut self,
        at: Span,
        canonical: String,
        variants: &[String],
        variant: &str,
        set_display: &str,
    ) -> bool {
        if !variants.iter().any(|v| v == variant) {
            self.diags.push(
                Diagnostic::error(format!(
                    "error set `{set_display}` has no variant `{variant}`"
                ))
                .with_label(at, "unknown error variant")
                .with_code("E302"),
            );
            return false;
        }
        self.out.error_uses.insert(
            (at.start, at.end),
            ErrorUse {
                set: canonical,
                variant: variant.to_string(),
            },
        );
        true
    }

    /// Resolve `Type.method(args)` and `value.method(args)` call sites.
    /// Returns true when the site was fully handled here: an associated
    /// callee recorded, a sugar receiver recorded, or one root-cause
    /// diagnostic emitted with the arguments still resolved for inner
    /// errors. Returns false to fall through to [`lookup_path`](Self::lookup_path).
    ///
    /// Resolution order at each site: poisoned heads stay quiet, then local
    /// `Type.method`, then bare imported `Type.method` (`use vl.dog.{Dog}`),
    /// then instance sugar when the head is a bound value
    /// (values still win over module aliases here for recovery, like bare
    /// names, even though shadowing an import is E206 at the declaration),
    /// then alias-qualified `alias.Type.method`, then fully qualified
    /// `mod.Type.method` (no import needed, like object types). A head naming
    /// a local or imported object type always wins over a same-named value.
    fn resolve_assoc_or_sugar_call(
        &mut self,
        callee: &[String],
        callee_span: Span,
        args: &[Expr],
    ) -> bool {
        if callee.len() < 2 {
            return false;
        }
        let full = callee.join(".");
        // Union variant construction (`Option.Some(args)`,
        // `alias.Option.Some(args)`, `mod.Option.Some(args)`): the head
        // names a union, not a value or an object with methods. Like object
        // type names, the union head wins over same-named values. Unknown
        // variants are one E302 here; arity and payload types are validated
        // by typechecking from the recorded site.
        if callee.len() == 2
            && (self.local_unions.contains_key(&callee[0]) || callee[0] == "Option")
        {
            let union = callee[0].clone();
            let variants = self
                .local_unions
                .get(&union)
                .cloned()
                .unwrap_or_else(builtin_option_variants);
            for arg in args {
                self.resolve_expr(arg);
            }
            return self.record_variant_use(
                callee_span,
                union.clone(),
                &variants,
                &callee[1],
                &union,
            );
        }
        // Bare imported union (`use vl.types.{U}` then `U.Variant(...)`).
        // Non-union imports (objects) fall through to the method logic below.
        if callee.len() == 2 && !self.local_unions.contains_key(&callee[0]) && callee[0] != "Option"
        {
            if let Some(qualified) = self.imported_types.get(&callee[0]).cloned() {
                let found = self.modules.iter().find_map(|m| {
                    m.unions.iter().find(|u| u.qualified == qualified).map(|u| {
                        u.variants
                            .iter()
                            .map(|v| v.name.clone())
                            .collect::<Vec<_>>()
                    })
                });
                if let Some(variants) = found {
                    let display = callee[0].clone();
                    for arg in args {
                        self.resolve_expr(arg);
                    }
                    return self.record_variant_use(
                        callee_span,
                        qualified,
                        &variants,
                        &callee[1],
                        &display,
                    );
                }
            }
        }
        if callee.len() >= 3 {
            let head = &callee[..callee.len() - 1];
            let variant = callee[callee.len() - 1].clone();
            if let Some((canonical, variants)) = self.canonical_union_head(head) {
                let display = head.join(".");
                for arg in args {
                    self.resolve_expr(arg);
                }
                return self.record_variant_use(
                    callee_span,
                    canonical,
                    &variants,
                    &variant,
                    &display,
                );
            }
        }
        if self.poisoned_imports.contains(&full) || self.poisoned_imports.contains(&callee[0]) {
            let id = self.external_def(full, callee_span, None, DefKind::ImportedFunction, None);
            self.out
                .uses
                .insert((callee_span.start, callee_span.end), id);
            for arg in args {
                self.resolve_expr(arg);
            }
            return true;
        }
        // Local `Type.method`.
        if callee.len() == 2 && self.local_objects.contains(&callee[0]) {
            if let Some(id) = self
                .assoc
                .get(&(callee[0].clone(), callee[1].clone()))
                .cloned()
            {
                self.out
                    .uses
                    .insert((callee_span.start, callee_span.end), id);
            } else {
                self.diags.push(
                    self.missing_method_diag(
                        &callee[0],
                        &callee[1],
                        callee_span,
                        self.local_fields
                            .get(&callee[0])
                            .is_some_and(|fields| fields.contains(&callee[1])),
                    ),
                );
            }
            for arg in args {
                self.resolve_expr(arg);
            }
            return true;
        }
        // Bare imported `Type.method` (`use vl.dog.{Dog}` then
        // `Dog.new(...)`). Like local type names, the imported type wins
        // over a same-named value; union imports were consumed as variant
        // construction above, so a non-object head falls through.
        if callee.len() == 2 && !self.local_objects.contains(&callee[0]) {
            if let Some(qualified) = self.imported_types.get(&callee[0]).cloned() {
                let provider = self.modules.iter().find_map(|m| {
                    m.objects
                        .iter()
                        .find(|o| o.qualified == qualified)
                        .map(|o| (m.clone(), o.clone()))
                });
                if let Some((spec, obj)) = provider {
                    let method = callee[1].clone();
                    let dotted = format!("{}.{}", obj.name, method);
                    if spec.poisoned_exports.iter().any(|e| e == &dotted) {
                        let id = self.external_def(
                            full.clone(),
                            callee_span,
                            None,
                            DefKind::ImportedFunction,
                            None,
                        );
                        self.out
                            .uses
                            .insert((callee_span.start, callee_span.end), id);
                        for arg in args {
                            self.resolve_expr(arg);
                        }
                        return true;
                    }
                    match obj.lookup_method(&method) {
                        None => {
                            self.diags.push(self.missing_method_diag(
                                &obj.qualified,
                                &method,
                                callee_span,
                                obj.fields.iter().any(|f| f.name == method),
                            ));
                        }
                        Some(export) => {
                            if spec.global_dependent_exports.iter().any(|e| e == &dotted) {
                                self.diags.push(
                                    Diagnostic::error(format!(
                                        "imported function `{}.{}` depends on module globals",
                                        spec.path.as_string(),
                                        dotted
                                    ))
                                    .with_label(callee_span, "unsupported cross-module boundary")
                                    .with_code("E208"),
                                );
                                self.poisoned_imports.insert(full.clone());
                                let id = self.external_def(
                                    full,
                                    callee_span,
                                    None,
                                    DefKind::ImportedFunction,
                                    None,
                                );
                                self.out
                                    .uses
                                    .insert((callee_span.start, callee_span.end), id);
                                for arg in args {
                                    self.resolve_expr(arg);
                                }
                                return true;
                            }
                            let id = self.external_def_with_kind(
                                full,
                                callee_span,
                                Some(export.sig.clone()),
                                Some(export.kind),
                                DefKind::ImportedFunction,
                                Some(SymbolRef {
                                    module: spec.path.clone(),
                                    name: dotted,
                                }),
                            );
                            self.out
                                .uses
                                .insert((callee_span.start, callee_span.end), id);
                        }
                    }
                    for arg in args {
                        self.resolve_expr(arg);
                    }
                    return true;
                }
            }
        }
        // Instance sugar `head.rest.method(args)`: the head is a bound value.
        // Shadowing an import is E206 at the declaration, but resolution still
        // prefers the bound value here for recovery (like bare names); the
        // receiver path and the self-type gate are validated by `vl-typecheck`,
        // which reports loudly when sugar does not apply. Only object type
        // names (local or imported) take precedence over values. The site
        // records its receiver head so no E201 fires.
        if !self.local_objects.contains(&callee[0]) && !self.imported_types.contains_key(&callee[0])
        {
            if let Some(head) = self.lookup(&callee[0]) {
                self.out
                    .sugar_receivers
                    .insert((callee_span.start, callee_span.end), head);
                for arg in args {
                    self.resolve_expr(arg);
                }
                return true;
            }
        }
        // Alias-qualified `alias.Type.method` (exactly three segments).
        if callee.len() == 3 && self.imports.contains_key(&callee[0]) {
            let spec = self
                .imports
                .get(&callee[0])
                .cloned()
                .expect("checked above");
            let dotted = format!("{}.{}", callee[1], callee[2]);
            if spec.poisoned_exports.iter().any(|e| e == &dotted) {
                let id = self.external_def(
                    full.clone(),
                    callee_span,
                    None,
                    DefKind::ImportedFunction,
                    None,
                );
                self.out
                    .uses
                    .insert((callee_span.start, callee_span.end), id);
                for arg in args {
                    self.resolve_expr(arg);
                }
                return true;
            }
            match spec.objects.iter().find(|o| o.name == callee[1]) {
                None => {
                    self.diags.push(
                        Diagnostic::error(format!(
                            "module `{}` has no object type `{}`",
                            spec.path.as_string(),
                            callee[1]
                        ))
                        .with_label(callee_span, "unknown object type")
                        .with_code("E302"),
                    );
                }
                Some(obj) => match obj.lookup_method(&callee[2]) {
                    None => {
                        self.diags.push(self.missing_method_diag(
                            &obj.qualified,
                            &callee[2],
                            callee_span,
                            obj.fields.iter().any(|f| f.name == callee[2]),
                        ));
                    }
                    Some(export) => {
                        if spec.global_dependent_exports.iter().any(|e| e == &dotted) {
                            self.diags.push(
                                Diagnostic::error(format!(
                                    "imported function `{}.{}` depends on module globals",
                                    spec.path.as_string(),
                                    dotted
                                ))
                                .with_label(callee_span, "unsupported cross-module boundary")
                                .with_code("E208"),
                            );
                            self.poisoned_imports.insert(full.clone());
                            let id = self.external_def(
                                full,
                                callee_span,
                                None,
                                DefKind::ImportedFunction,
                                None,
                            );
                            self.out
                                .uses
                                .insert((callee_span.start, callee_span.end), id);
                            for arg in args {
                                self.resolve_expr(arg);
                            }
                            return true;
                        }
                        let id = self.external_def_with_kind(
                            full,
                            callee_span,
                            Some(export.sig.clone()),
                            Some(export.kind),
                            DefKind::ImportedFunction,
                            Some(SymbolRef {
                                module: spec.path.clone(),
                                name: dotted,
                            }),
                        );
                        self.out
                            .uses
                            .insert((callee_span.start, callee_span.end), id);
                    }
                },
            }
            for arg in args {
                self.resolve_expr(arg);
            }
            return true;
        }
        // Fully qualified `mod.Type.method` without an import: the prefix
        // names a known object (local `<module>.Type` or catalog-qualified).
        if callee.len() >= 3 {
            let method = callee[callee.len() - 1].clone();
            let prefix = callee[..callee.len() - 1].join(".");
            if let Some(local) = prefix
                .strip_prefix(&format!("{}.", self.module))
                .filter(|rest| self.local_objects.contains(*rest))
            {
                if let Some(id) = self
                    .assoc
                    .get(&(local.to_string(), method.clone()))
                    .cloned()
                {
                    self.out
                        .uses
                        .insert((callee_span.start, callee_span.end), id);
                } else {
                    self.diags.push(
                        self.missing_method_diag(
                            &prefix,
                            &method,
                            callee_span,
                            self.local_fields
                                .get(local)
                                .is_some_and(|fields| fields.contains(&method)),
                        ),
                    );
                }
                for arg in args {
                    self.resolve_expr(arg);
                }
                return true;
            }
            for spec in self.modules.clone() {
                let Some(obj) = spec.objects.iter().find(|o| o.qualified == prefix) else {
                    continue;
                };
                let dotted = format!("{}.{}", obj.name, method);
                if spec.poisoned_exports.iter().any(|e| e == &dotted) {
                    let id = self.external_def(
                        full.clone(),
                        callee_span,
                        None,
                        DefKind::ImportedFunction,
                        None,
                    );
                    self.out
                        .uses
                        .insert((callee_span.start, callee_span.end), id);
                    for arg in args {
                        self.resolve_expr(arg);
                    }
                    return true;
                }
                match obj.lookup_method(&method) {
                    None => {
                        self.diags.push(self.missing_method_diag(
                            &prefix,
                            &method,
                            callee_span,
                            obj.fields.iter().any(|f| f.name == method),
                        ));
                    }
                    Some(export) => {
                        if spec.global_dependent_exports.iter().any(|e| e == &dotted) {
                            self.diags.push(
                                Diagnostic::error(format!(
                                    "imported function `{}.{}` depends on module globals",
                                    spec.path.as_string(),
                                    dotted
                                ))
                                .with_label(callee_span, "unsupported cross-module boundary")
                                .with_code("E208"),
                            );
                            self.poisoned_imports.insert(full.clone());
                            let id = self.external_def(
                                full,
                                callee_span,
                                None,
                                DefKind::ImportedFunction,
                                None,
                            );
                            self.out
                                .uses
                                .insert((callee_span.start, callee_span.end), id);
                            for arg in args {
                                self.resolve_expr(arg);
                            }
                            return true;
                        }
                        let id = self.external_def_with_kind(
                            full,
                            callee_span,
                            Some(export.sig.clone()),
                            Some(export.kind),
                            DefKind::ImportedFunction,
                            Some(SymbolRef {
                                module: spec.path.clone(),
                                name: dotted,
                            }),
                        );
                        self.out
                            .uses
                            .insert((callee_span.start, callee_span.end), id);
                    }
                }
                for arg in args {
                    self.resolve_expr(arg);
                }
                return true;
            }
        }
        false
    }

    /// One E302 for `Type.method` when `Type` names a known object but the
    /// method does not exist. When the name matches a field, the note steers
    /// toward field access instead of a call.
    fn missing_method_diag(
        &self,
        owner: &str,
        method: &str,
        span: Span,
        is_field: bool,
    ) -> Diagnostic {
        let mut diag = Diagnostic::error(format!(
            "object `{owner}` has no associated function `{method}`"
        ))
        .with_label(span, "unknown associated function")
        .with_code("E302");
        if is_field {
            diag = diag.with_note(format!(
                "`{method}` is a field of `{owner}`; read it without `(...)`"
            ));
        } else {
            diag = diag.with_note(format!(
                "declare it inside the object body: `fun {method}(...)` in `type {owner} = object {{ ... }}`"
            ));
        }
        diag
    }

    fn external_def(
        &mut self,
        name: String,
        span: Span,
        sig: Option<vl_common::FuncSig>,
        kind: DefKind,
        symbol: Option<SymbolRef>,
    ) -> DefId {
        self.external_def_with_kind(name, span, sig, None, kind, symbol)
    }
    fn external_def_with_kind(
        &mut self,
        name: String,
        span: Span,
        sig: Option<vl_common::FuncSig>,
        export_kind: Option<vl_common::ExportKind>,
        kind: DefKind,
        symbol: Option<SymbolRef>,
    ) -> DefId {
        let id = DefId(self.out.defs.len() as u32);
        self.out.defs.push(Def {
            id: id.clone(),
            name,
            span,
            kind,
            sig,
            export_kind,
            binding: None,
            symbol,
        });
        id
    }
}

/// Default module catalog used by [`resolve`]. Must mirror
/// `vl_codegen::modules` (which owns the extern signatures); the two cannot
/// share code without an import cycle, so keep them in sync by hand.
pub fn default_modules() -> Vec<ModuleSpec> {
    use vl_common::VlType as T;
    vec![
        ModuleSpec::new(
            &["std"],
            &[
                ("print", &[("value", T::String)], T::Void),
                ("println", &[("value", T::String)], T::Void),
                ("print_u64", &[("value", T::U64)], T::Void),
            ],
        ),
        ModuleSpec::new(
            &["std", "fs"],
            &[
                ("open", &[("path", T::String)], T::File),
                ("read", &[("file", T::File)], T::String),
            ],
        ),
        ModuleSpec::new(
            &["std", "string"],
            &[
                ("len", &[("value", T::String)], T::U64),
                ("concat", &[("a", T::String), ("b", T::String)], T::String),
                ("eq", &[("a", T::String), ("b", T::String)], T::Bool),
                ("to_u64", &[("value", T::String)], T::U64),
                ("hex_to_u64", &[("value", T::String)], T::U64),
            ],
        ),
        ModuleSpec::new(
            &["std", "math"],
            &[("mod_u64", &[("a", T::U64), ("b", T::U64)], T::U64)],
        ),
        ModuleSpec::new(
            &["std", "fmt"],
            &[("u64_to_s", &[("value", T::U64)], T::String)],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve_src(src: &str) -> (Resolution, Vec<Diagnostic>) {
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        resolve(&prog)
    }

    #[test]
    fn error_values_resolve_to_use_sites() {
        let (res, diags) = resolve_src("type E = error { A, B, }; fun main() { val x = E.A; x; }");
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
        assert_eq!(res.error_uses.len(), 1, "{res:?}");
        let use_ = res.error_uses.values().next().expect("use");
        assert_eq!(use_.set, "E");
        assert_eq!(use_.variant, "A");
    }

    #[test]
    fn error_interface_exports_sets_for_importers() {
        let spec = provider_spec("type E = error { A, B, };", "vl.io");
        let err = spec.lookup_error("E").expect("error export");
        assert_eq!(err.qualified, "vl.io.E");
        assert_eq!(err.variants, vec!["A", "B"]);
        assert_eq!(spec.lookup_type_qualified("E"), Some("vl.io.E"));
    }

    #[test]
    fn brace_error_import_registers_type_without_e203() {
        let io = provider_spec("type E = error { A, };", "vl.io");
        let (toks, _) = vl_lex::lex("use vl.io.{E}; fun main() { val x = E.A; x; }");
        let (prog, pdiags) = vl_syntax::parse_with_module(&toks, "", "vl.main");
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, diags) = resolve_with_modules(&prog, &[io]);
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
        assert_eq!(res.error_uses.len(), 1, "{res:?}");
        let use_ = res.error_uses.values().next().expect("use");
        // Bare imported sets canonicalize to the qualified identity.
        assert_eq!(use_.set, "vl.io.E");
    }

    #[test]
    fn unknown_error_variant_is_one_e302() {
        let (_res, diags) = resolve_src("type E = error { A, }; fun main() { val x = E.B; x; }");
        let errors = diags.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "{diags:?}");
        assert_eq!(errors[0].code.as_deref(), Some("E302"));
    }

    #[test]
    fn error_variant_call_tail_is_one_e303() {
        let (res, diags) =
            resolve_src("type E = error { A, }; fun main() { val x = E.A(1u64); x; }");
        let errors = diags.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "{diags:?}");
        assert_eq!(errors[0].code.as_deref(), Some("E303"));
        // The site still records so lowering proceeds behind the error.
        assert_eq!(res.error_uses.len(), 1, "{res:?}");
    }

    #[test]
    fn duplicate_type_across_object_and_error_is_e200() {
        let (_res, diags) = resolve_src("type E = object { v: u64, }; type E = error { A, };");
        let errors = diags.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "{diags:?}");
        assert_eq!(errors[0].code.as_deref(), Some("E200"));
    }

    #[test]
    fn try_and_catch_bodies_resolve() {
        let (res, diags) = resolve_src(
            "type E = error { A, }; fun f(): E!u64 { return 1u64; } fun main() { val x = try f() catch 0u64; x; }",
        );
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
        assert!(res.error_uses.is_empty(), "{res:?}");
    }

    #[test]
    fn destructure_bindings_resolve_as_locals() {
        let (toks, _) = vl_lex::lex("fun main() { val t = #(1u64, 2u64); val #(a, b) = t; a; b; }");
        let (prog, pdiags) = vl_syntax::parse(&toks, "");
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, diags) = resolve(&prog);
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
        let names: Vec<&str> = res.defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"a") && names.contains(&"b"), "{names:?}");
    }

    #[test]
    fn destructure_of_unresolved_base_poisons_quietly() {
        // `missing` is one E201; the pattern bindings still intern so later
        // stages stay quiet downstream (no cascade).
        let (_, diags) = resolve_src("fun main() { val #(a, b) = missing; }");
        let errors: Vec<_> = diags.iter().filter(|d| d.is_error()).collect();
        assert_eq!(errors.len(), 1, "{diags:?}");
        assert_eq!(errors[0].code.as_deref(), Some("E201"));
    }

    #[test]
    fn break_outside_a_loop_is_an_error() {
        let (_, diags) = resolve_src("fun main() { break; }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("outside of a loop"));
    }

    #[test]
    fn break_inside_while_resolves() {
        let (_, diags) = resolve_src("fun main() { while (true) { break; } }");
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
    }

    #[test]
    fn assignment_to_an_undefined_name_errors() {
        let (_, diags) = resolve_src("fun main() { x = 1; }");
        assert!(diags.iter().any(|d| d.code.as_deref() == Some("E201")));
    }

    #[test]
    fn assignment_to_a_bound_local_resolves() {
        let (_, diags) = resolve_src("fun main() { var x = 1; x = 2; }");
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
    }

    #[test]
    fn unknown_object_literal_is_deferred_to_typechecking() {
        let (_, diags) = resolve_src("fun main() { val x = Missing {}; }");
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
    }

    #[test]
    fn undefined_variable_errors() {
        let (_, diags) = resolve_src("val x = y;");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains('y'));
    }

    #[test]
    fn shadowing_is_a_warning_only() {
        let (toks, _) = vl_lex::lex("fun f(x: i64): i64 { val x = 1; x; }");
        let (prog, _) = vl_syntax::parse(&toks, "");
        let (_, diags) = resolve(&prog);
        assert!(diags.iter().all(|d| !d.is_error()));
    }

    #[test]
    fn call_callee_and_args_resolve() {
        let (_, diags) =
            resolve_src("fun add(a: i64, b: i64): i64 { a + b; } fun main() { add(1, 2); }");
        assert!(diags.iter().all(|d| !d.is_error()));
    }

    #[test]
    fn undefined_callee_errors() {
        let (_, diags) = resolve_src("fun main() { nope(1); }");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("nope"));
    }

    #[test]
    fn forward_call_resolves_via_global_prepass() {
        let (_, diags) = resolve_src("fun main() { helper(); } fun helper(): i64 { 1; }");
        assert!(diags.iter().all(|d| !d.is_error()));
    }

    #[test]
    fn duplicate_parameters_are_an_error() {
        let (_, diags) = resolve_src("fun f(x: i64, x: i64): i64 { x; }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("duplicate parameter"));
    }

    #[test]
    fn poisoned_module_alias_suppresses_qualified_use_cascade() {
        let (_, diags) = resolve_src("use missing.module; fun main() { module.foo(); }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("cannot find module"));
    }

    #[test]
    fn poisoned_grouped_import_alias_stays_quiet_and_reserved() {
        let module = ModuleSpec::new(&["other"], &[("f", &[], vl_common::VlType::Void)]);
        let (toks, _) = vl_lex::lex("use missing.{f}; use other.{f}; fun main() { f(); }");
        let (program, parse_diags) = vl_syntax::parse(&toks, "");
        assert!(parse_diags.is_empty(), "{parse_diags:?}");
        let (resolution, diags) = resolve_with_modules(&program, &[module]);
        let errors = diags.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
        assert_eq!(errors.len(), 2, "{diags:?}");
        assert_eq!(errors[0].code.as_deref(), Some("E202"), "{diags:?}");
        assert_eq!(errors[1].code.as_deref(), Some("E206"), "{diags:?}");
        assert_eq!(errors[1].labels.len(), 2, "{diags:?}");
        assert!(resolution.poisoned_imports);
    }

    #[test]
    fn exact_module_alias_wins_over_single_export_fallback() {
        let parent = ModuleSpec::new(&["demo", "a"], &[("b", &[], vl_common::VlType::Void)]);
        let exact = ModuleSpec::new(
            &["demo", "a", "b"],
            &[("helper", &[], vl_common::VlType::Void)],
        );
        let (tokens, _) = vl_lex::lex("use demo.a.b; fun main() { b(); b.helper(); }");
        let (program, parse_diags) = vl_syntax::parse(&tokens, "");
        assert!(parse_diags.is_empty(), "{parse_diags:?}");
        let (resolution, diags) = resolve_with_modules(&program, &[parent, exact]);
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
        let alias = resolution
            .defs
            .iter()
            .find(|def| def.name == "b")
            .expect("module alias");
        assert_eq!(alias.kind, DefKind::ModuleAlias);
        assert!(alias.sig.is_none());
        assert!(resolution
            .defs
            .iter()
            .any(|def| { def.name == "b.helper" && def.kind == DefKind::ImportedFunction }));
    }

    #[test]
    fn empty_exact_module_alias_has_no_parent_export_signature() {
        let parent = ModuleSpec::new(&["demo", "a"], &[("b", &[], vl_common::VlType::Void)]);
        let exact = ModuleSpec::new(&["demo", "a", "b"], &[]);
        let (tokens, _) = vl_lex::lex("use demo.a.b; fun main() { b(); }");
        let (program, parse_diags) = vl_syntax::parse(&tokens, "");
        assert!(parse_diags.is_empty(), "{parse_diags:?}");
        let (resolution, diags) = resolve_with_modules(&program, &[parent, exact]);
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
        let alias = resolution
            .defs
            .iter()
            .find(|def| def.name == "b")
            .expect("module alias");
        assert_eq!(alias.kind, DefKind::ModuleAlias);
        assert!(alias.sig.is_none());
    }

    #[test]
    fn missing_grouped_import_reserves_each_alias() {
        let (tokens, _) =
            vl_lex::lex("use missing.one.{f, g}; use missing.two.{f, g}; fun main() { f(); g(); }");
        let (program, parse_diags) = vl_syntax::parse(&tokens, "");
        assert!(parse_diags.is_empty(), "{parse_diags:?}");
        let (_, diags) = resolve_with_modules(&program, &[]);
        let errors = diags.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
        assert_eq!(errors.len(), 4, "{diags:?}");
        assert_eq!(errors[0].code.as_deref(), Some("E202"), "{diags:?}");
        assert_eq!(errors[1].code.as_deref(), Some("E206"), "{diags:?}");
        assert_eq!(errors[2].code.as_deref(), Some("E206"), "{diags:?}");
        assert_eq!(errors[3].code.as_deref(), Some("E202"), "{diags:?}");
        assert_eq!(errors[1].labels.len(), 2, "{diags:?}");
        assert_eq!(errors[2].labels.len(), 2, "{diags:?}");
    }

    #[test]
    fn poisoned_import_alias_stays_reserved_across_import_forms() {
        let mut poisoned_alias = ModuleSpec::new(&["demo", "bad"], &[]);
        poisoned_alias.parse_poisoned = true;
        let valid_alias = ModuleSpec::new(&["other", "bad"], &[]);

        let mut poisoned_export = ModuleSpec::new(&["demo"], &[]);
        poisoned_export.poisoned_exports.push("bad".into());
        let valid_export = ModuleSpec::new(&["other"], &[("bad", &[], vl_common::VlType::Void)]);

        let cases = [
            (
                "use demo.bad; use other.bad; fun main() { bad.missing(); }",
                vec![poisoned_alias, valid_alias],
            ),
            (
                "use demo.bad; use other.bad; fun main() { bad(); }",
                vec![poisoned_export.clone(), valid_export.clone()],
            ),
            (
                "use demo.{bad}; use other.{bad}; fun main() { bad(); }",
                vec![poisoned_export, valid_export],
            ),
        ];

        for (source, modules) in cases {
            let (tokens, _) = vl_lex::lex(source);
            let (program, parse_diags) = vl_syntax::parse(&tokens, "");
            assert!(parse_diags.is_empty(), "{source}: {parse_diags:?}");
            let (resolution, diags) = resolve_with_modules(&program, &modules);
            let errors = diags.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
            assert_eq!(errors.len(), 1, "{source}: {diags:?}");
            assert_eq!(
                errors[0].code.as_deref(),
                Some("E206"),
                "{source}: {diags:?}"
            );
            assert!(resolution.poisoned_imports);
            assert!(resolution
                .defs
                .iter()
                .filter(|def| def.kind == DefKind::ImportedFunction)
                .all(|def| def.sig.is_none()));
        }
    }

    #[test]
    fn object_types_are_exported_qualified_without_e208() {
        let (toks, _) = vl_lex::lex(
            "type Person = object { name: String, age: u64, }; fun new(name: String, age: u64): *Person { return Person { name = name, age = age, }; } fun print_name(person: Person) { person.name; }",
        );
        let (provider, _) = vl_syntax::parse_with_module(&toks, "", "vl.person");
        let (interface, diags) = collect_interface(&provider);
        assert!(diags.is_empty(), "{diags:?}");
        let person = interface
            .objects
            .iter()
            .find(|o| o.name == "Person")
            .expect("Person export");
        assert_eq!(person.qualified, "vl.person.Person");
        assert_eq!(person.fields.len(), 2);
        let new = interface
            .functions
            .iter()
            .find(|e| e.name == "new")
            .expect("new export");
        assert_eq!(
            new.sig.ret,
            vl_common::VlType::Mutable(Box::new(vl_common::VlType::Object(
                "vl.person.Person".into()
            )))
        );
        let print = interface
            .functions
            .iter()
            .find(|e| e.name == "print_name")
            .expect("print_name export");
        assert_eq!(
            print.sig.params[0].ty,
            vl_common::VlType::Object("vl.person.Person".into())
        );
    }

    #[test]
    fn union_interface_metadata_is_separate_from_object_namespaces() {
        let (toks, _) = vl_lex::lex(
            "type Option[T] = union { None, Some(T), }; type Box = object { value: u64, };",
        );
        let (provider, _) = vl_syntax::parse_with_module(&toks, "", "demo.types");
        let (interface, diags) = collect_interface(&provider);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(interface.objects.len(), 1);
        assert_eq!(interface.unions.len(), 1);
        let option = &interface.unions[0];
        assert_eq!(option.qualified, "demo.types.Option");
        assert_eq!(option.type_params.len(), 1);
        assert_eq!(
            option.variants[1].payload,
            vec![vl_common::VlType::Param("T".into())]
        );
        assert!(interface
            .objects
            .iter()
            .all(|object| object.name != "Option"));

        let (_, resolve_diags) = resolve_src("type U = union { A, }; fun main() { U.f(); }");
        assert_eq!(resolve_diags.iter().filter(|d| d.is_error()).count(), 1);
        assert_eq!(resolve_diags[0].code.as_deref(), Some("E302"));
    }

    #[test]
    fn union_variant_construction_resolves_without_e201() {
        let (resolution, diags) = resolve_src(
            "type Option[T] = union { None, Some(T), }; fun main() { val a = Option.Some(1u64); val n = Option.None; a; n; }",
        );
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
        let mut uses: Vec<(String, String)> = resolution
            .variants
            .values()
            .map(|u| (u.union.clone(), u.variant.clone()))
            .collect();
        uses.sort();
        assert_eq!(
            uses,
            vec![
                ("Option".to_string(), "None".to_string()),
                ("Option".to_string(), "Some".to_string()),
            ]
        );
    }

    #[test]
    fn unknown_union_variant_is_one_e302() {
        let (_, diags) = resolve_src("type U = union { A, }; fun main() { val x = U.B(1u64); x; }");
        let errors = diags.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "{diags:?}");
        assert_eq!(errors[0].code.as_deref(), Some("E302"));
    }

    fn provider_spec(src: &str, module: &str) -> vl_common::ModuleSpec {
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse_with_module(&toks, src, module);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (interface, idiags) = collect_interface_quiet(&prog);
        assert!(idiags.is_empty(), "{idiags:?}");
        interface.as_spec()
    }

    #[test]
    fn brace_object_import_registers_type_without_e203() {
        let dog = provider_spec(
            "type Dog = object { name: String, fun new(name: String): *Dog { return Dog { name = name, }; } };",
            "vl.dog",
        );
        let (toks, _) = vl_lex::lex("use vl.dog.{Dog}; fun main() { val d = Dog.new(\"x\"); d; }");
        let (prog, pdiags) = vl_syntax::parse_with_module(&toks, "", "vl.main");
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, diags) = resolve_with_modules(&prog, &[dog]);
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
        assert_eq!(
            res.imported_types.get("Dog"),
            Some(&"vl.dog.Dog".to_string())
        );
        // The bare call resolves to the provider method, not a sugar receiver.
        let def = res
            .defs
            .iter()
            .find(|d| d.kind == DefKind::ImportedFunction)
            .expect("imported Dog.new def");
        assert_eq!(def.name, "Dog.new");
        assert_eq!(
            def.symbol
                .as_ref()
                .map(|s| (s.module.as_string(), s.name.as_str())),
            Some(("vl.dog".to_string(), "Dog.new"))
        );
        assert!(res.sugar_receivers.is_empty());
    }

    #[test]
    fn brace_union_import_canonicalizes_variant_heads() {
        let shapes = provider_spec("type Shape = union { Circle(u64), Point, };", "vl.shapes");
        let (toks, _) = vl_lex::lex(
            "use vl.shapes.{Shape}; fun main() { val a = Shape.Circle(1u64); match (a) { Shape.Circle(r) { r; } Shape.Point { 0u64; } else { 0u64; } } }",
        );
        let (prog, pdiags) = vl_syntax::parse_with_module(&toks, "", "vl.main");
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, diags) = resolve_with_modules(&prog, &[shapes]);
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
        let mut unions: Vec<String> = res.variants.values().map(|u| u.union.clone()).collect();
        unions.sort();
        unions.dedup();
        assert_eq!(unions, vec!["vl.shapes.Shape".to_string()]);
    }

    #[test]
    fn builtin_option_variants_resolve_without_a_declaration() {
        // `?T` / `null` sugar rests on a builtin `Option`: no `type Option`
        // item needed for `Option.Some` / `Option.None` uses.
        let (resolution, diags) = resolve_src(
            "fun main() { val a = Option.Some(1u64); val n = Option.None; val m: ?u64 = null; a; n; m; }",
        );
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
        let mut uses: Vec<(String, String)> = resolution
            .variants
            .values()
            .map(|u| (u.union.clone(), u.variant.clone()))
            .collect();
        uses.sort();
        assert_eq!(
            uses,
            vec![
                ("Option".to_string(), "None".to_string()),
                ("Option".to_string(), "Some".to_string()),
            ]
        );
    }

    #[test]
    fn local_option_declaration_shadows_the_builtin() {
        // First declaration wins: resolving `Option.Some` still records the
        // bare `Option` spelling either way.
        let (resolution, diags) =
            resolve_src("type Option = union { None, }; fun main() { val n = Option.None; n; }");
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
        assert!(resolution
            .variants
            .values()
            .any(|u| u.union == "Option" && u.variant == "None"));
    }

    #[test]
    fn null_literal_needs_no_resolution() {
        let (_, diags) = resolve_src("fun main() { val x: ?u64 = null; x; }");
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
    }

    #[test]
    fn match_arm_bindings_are_scoped_to_their_arm() {
        let (resolution, diags) = resolve_src(
            "type U = union { A(u64), B, }; fun main() { val u = U.B; match (u) { U.A(v) { v; } else { 0u64; } } }",
        );
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
        // The arm binding `v` is declared exactly once as a `val`.
        let v_defs: Vec<_> = resolution.defs.iter().filter(|d| d.name == "v").collect();
        assert_eq!(v_defs.len(), 1);
        assert_eq!(v_defs[0].binding, Some(BindingKind::Val));
    }

    #[test]
    fn duplicate_match_arm_binding_is_one_e200() {
        let (_, diags) = resolve_src(
            "type U = union { A(u64, u64), }; fun main() { val u = U.A(1u64, 2u64); match (u) { U.A(v, v) { v; } else { 0u64; } } }",
        );
        let errors = diags.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "{diags:?}");
        assert_eq!(errors[0].code.as_deref(), Some("E200"));
    }

    #[test]
    fn duplicate_object_and_union_type_is_one_error() {
        let (_, diags) = resolve_src("type U = object { value: u64, }; type U = union { A, };");
        let errors = diags.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "{diags:?}");
        assert_eq!(errors[0].code.as_deref(), Some("E200"));
    }

    #[test]
    fn associated_functions_are_exported_qualified() {
        let (toks, _) = vl_lex::lex(
            "type Counter = object { value: u64, fun init(v: u64): *Counter { return Counter { value = v }; }, fun get(self: Counter): u64 { return self.value; }, };",
        );
        let (provider, _) = vl_syntax::parse_with_module(&toks, "", "demo.count");
        let (interface, diags) = collect_interface(&provider);
        assert!(diags.is_empty(), "{diags:?}");
        let counter = interface
            .objects
            .iter()
            .find(|o| o.name == "Counter")
            .expect("Counter export");
        assert_eq!(counter.methods.len(), 2);
        let init = counter.lookup_method("init").expect("init export");
        assert_eq!(
            init.sig.ret,
            vl_common::VlType::Mutable(Box::new(vl_common::VlType::Object(
                "demo.count.Counter".into()
            )))
        );
        let get = counter.lookup_method("get").expect("get export");
        assert_eq!(
            get.sig.params[0].ty,
            vl_common::VlType::Object("demo.count.Counter".into())
        );
    }

    #[test]
    fn associated_type_func_resolves_to_its_def() {
        let (res, diags) = resolve_src(
            "type C = object { value: u64, fun f(self: C): u64 { return self.value; }, }; fun main() { var c: *C = C { value = 1 }; C.f(c); }",
        );
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
        let def = res.defs.iter().find(|d| d.name == "C.f").expect("C.f def");
        assert_eq!(def.kind, DefKind::Local);
    }

    #[test]
    fn sugar_receiver_is_recorded_without_e201() {
        let (res, diags) = resolve_src(
            "type C = object { value: u64, fun f(self: C): u64 { return self.value; }, }; fun main() { var c: *C = C { value = 1 }; c.f(); }",
        );
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
        assert_eq!(res.sugar_receivers.len(), 1);
        let head = res.sugar_receivers.values().next().expect("receiver");
        let def = res.defs.iter().find(|d| d.id == *head).expect("head def");
        assert_eq!(def.name, "c");
    }

    #[test]
    fn missing_associated_function_is_one_e302() {
        let (_, diags) = resolve_src(
            "type C = object { value: u64, }; fun main() { var c: *C = C { value = 1 }; C.bogus(c); }",
        );
        let errors = diags.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "{diags:?}");
        assert_eq!(errors[0].code.as_deref(), Some("E302"));
    }

    #[test]
    fn sugar_receiver_shadowing_module_alias_is_e206() {
        // Shadowing an import is E206 at the declaration. Resolution still
        // prefers the bound value for recovery (no silent module call), so the
        // sugar receiver is recorded alongside the error.
        let module = ModuleSpec::new(&["foo"], &[("m", &[], vl_common::VlType::Void)]);
        let (toks, _) = vl_lex::lex(
            "use foo; type L = object { v: u64, fun m(self: L): u64 { return self.v; }, }; fun f(foo: *L) { foo.m(); }",
        );
        let (prog, pdiags) = vl_syntax::parse(&toks, "");
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, diags) = resolve_with_modules(&prog, &[module]);
        let errors = diags.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "{diags:?}");
        assert_eq!(errors[0].code.as_deref(), Some("E206"), "{diags:?}");
        assert_eq!(res.sugar_receivers.len(), 1);
    }

    #[test]
    fn sugar_receiver_on_global_marks_dependence() {
        let (toks, _) = vl_lex::lex(
            "type C = object { v: u64, fun bump(self: *C): *C { return self; }, fun use_it(self: C): u64 { g.bump(); return self.v; }, }; val g: *C = C { v = 1 };",
        );
        let (prog, _) = vl_syntax::parse_with_module(&toks, "", "demo");
        let (interface, diags) = collect_interface(&prog);
        assert!(diags.is_empty(), "{diags:?}");
        assert!(
            interface
                .global_dependent_exports
                .iter()
                .any(|e| e == "C.use_it"),
            "sugar on a global must mark the method: {:?}",
            interface.global_dependent_exports
        );
    }

    #[test]
    fn assoc_call_edge_marks_transitive_dependence() {
        let (toks, _) = vl_lex::lex(
            "val shared: u64 = 1; type C = object { v: u64, fun inner(self: C): u64 { return shared; }, fun outer(self: C): u64 { return C.inner(self); }, };",
        );
        let (prog, _) = vl_syntax::parse_with_module(&toks, "", "demo");
        let (interface, diags) = collect_interface(&prog);
        assert!(diags.is_empty(), "{diags:?}");
        for want in ["C.inner", "C.outer"] {
            assert!(
                interface.global_dependent_exports.iter().any(|e| e == want),
                "{want} missing in {:?}",
                interface.global_dependent_exports
            );
        }
    }

    #[test]
    fn single_export_use_brings_bare_name_into_scope() {
        let (_, diags) = resolve_src("use std.string.len; fun main() { len(\"s\"); }");
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
    }

    #[test]
    fn bare_import_carries_its_signature() {
        let (res, diags) = resolve_src("use std.string.len; fun main() { len(\"s\"); }");
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
        let def = res.defs.iter().find(|d| d.name == "len").expect("len def");
        let sig = def.sig.as_ref().expect("extern sig");
        assert_eq!(sig.ret, vl_common::VlType::U64);
        assert_eq!(sig.params.len(), 1);
    }

    #[test]
    fn single_export_use_of_std_print_resolves() {
        let (toks, _) = vl_lex::lex("use std.print; fun main() { print(\"hi\"); }");
        let (prog, _) = vl_syntax::parse(&toks, "");
        let (_, diags) = resolve_with_modules(&prog, &vl_codegen_modules());
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
    }

    #[test]
    fn single_export_use_with_unknown_export_is_one_error() {
        let (_, diags) = resolve_src("use std.string.bogus; fun main() { bogus(); }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("no export `bogus`"));
    }

    #[test]
    fn braced_self_imports_the_module_alias() {
        // `use m.{self, Foo}` brings both the module (`string.len(...)`)
        // and the listed export (`len(...)`) into scope in one line.
        let (res, diags) = resolve_src(
            "use std.string.{self, len}; fun main() { string.len(\"s\"); len(\"s\"); }",
        );
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
        let imported: Vec<_> = res
            .defs
            .iter()
            .filter(|d| d.kind == DefKind::ImportedFunction)
            .collect();
        assert_eq!(imported.len(), 2, "{res:?}");
    }

    #[test]
    fn braced_self_alone_imports_the_module_alias() {
        let (res, diags) = resolve_src("use std.string.{self}; fun main() { string.len(\"s\"); }");
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
        assert!(
            res.defs
                .iter()
                .any(|d| d.kind == DefKind::ImportedFunction && d.name == "string.len"),
            "{res:?}"
        );
    }

    #[test]
    fn duplicate_self_and_module_import_is_e206() {
        let (_, diags) =
            resolve_src("use std.string; use std.string.{self}; fun main() { string.len(\"s\"); }");
        let errors = diags.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "{diags:?}");
        assert_eq!(errors[0].code.as_deref(), Some("E206"));
    }

    #[test]
    fn array_literal_index_and_index_assign_resolve() {
        let (_, diags) =
            resolve_src("fun main() { val a = [1u64, 2u64]; a[0u64] = 3u64; val x = a[1u64]; }");
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
    }

    #[test]
    fn array_new_needs_no_import_and_carries_its_signature() {
        let (res, diags) = resolve_src("fun main() { val a = Array.new::[u64](3u64); a; }");
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
        let def = res
            .defs
            .iter()
            .find(|d| d.name == "Array.new")
            .expect("Array.new def");
        let sig = def.sig.as_ref().expect("extern sig");
        assert_eq!(
            sig.ret,
            vl_common::VlType::Array(Box::new(vl_common::VlType::U64))
        );
        assert_eq!(sig.params.len(), 1);
        assert_eq!(sig.params[0].ty, vl_common::VlType::U64);
    }

    #[test]
    fn array_new_without_type_arg_defers_to_typechecking() {
        // No turbofish: resolution succeeds with no signature; typechecking
        // either infers `T` from an annotated binding or reports E303.
        let (res, diags) = resolve_src("fun main() { val a = Array.new(3); a; }");
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
        let def = res
            .defs
            .iter()
            .find(|d| d.name == "Array.new")
            .expect("Array.new def");
        assert!(def.sig.is_none());
    }

    #[test]
    fn array_new_with_two_type_args_is_one_error() {
        let (_, diags) = resolve_src("fun main() { val a = Array.new::[u64, u64](3); a; }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("exactly one type argument"));
    }

    #[test]
    fn u64array_callee_points_at_the_replacement() {
        let (_, diags) = resolve_src("fun main() { val a = U64Array.new(3u64); a; }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("U64Array"));
    }

    #[test]
    fn index_into_undefined_array_errors() {
        let (_, diags) = resolve_src("fun main() { val x = missing[0u64]; x; }");
        assert!(diags.iter().any(|d| d.code.as_deref() == Some("E201")));
    }

    #[test]
    fn direct_parameter_rebinding_is_one_e205() {
        for src in [
            "fun f(x: u64) { x = 1u64; }",
            "type Foo = object { value: u64, }; fun f(x: Foo, y: Foo) { x = y; }",
            "type Foo = object { value: u64, }; fun f(x: *Foo, y: *Foo) { x = y; }",
        ] {
            let (_, diags) = resolve_src(src);
            let errors = diags.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
            assert_eq!(errors.len(), 1, "{src}: {diags:?}");
            assert_eq!(errors[0].code.as_deref(), Some("E205"), "{src}: {diags:?}");
            assert!(
                errors[0].message.contains("cannot rebind parameter"),
                "{diags:?}"
            );
        }
    }

    #[test]
    fn val_rebinding_is_one_e205() {
        let (_, diags) = resolve_src("fun main() { val answer = 1u64; answer = 2u64; }");
        let errors = diags.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "{diags:?}");
        assert_eq!(errors[0].code.as_deref(), Some("E205"), "{diags:?}");
        assert!(errors[0].message.contains("cannot rebind `val`"));
    }

    #[test]
    fn parameter_field_and_index_mutation_still_resolve() {
        let (_, diags) = resolve_src(
            "type Foo = object { value: u64, }; fun f(x: *Foo, a: *Array[u64]) { x.value = 1u64; a[0u64] = 1u64; }",
        );
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
    }

    #[test]
    fn local_rebinding_and_shadowing_still_allowed() {
        let (_, diags) = resolve_src(
            "fun f(x: u64) { var x = 1u64; x = 2u64; } fun g() { var y = 1u64; y = 2u64; }",
        );
        // Shadowing is a warning; rebinding the local is fine.
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
    }

    #[test]
    fn local_val_shadowing_bare_import_is_e206() {
        // `use foo.bar` brings `bar`; any `val bar` must fail, even inside a
        // function body. No shadowing, ever.
        let (_, diags) = resolve_src("use std.string.len; fun main() { val len = 1u64; len; }");
        let errors = diags.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "{diags:?}");
        assert_eq!(errors[0].code.as_deref(), Some("E206"), "{diags:?}");
        assert!(errors[0].message.contains("`len`"), "{diags:?}");
    }

    #[test]
    fn local_var_and_module_alias_shadowing_is_e206() {
        for src in [
            "use std.string.len; fun main() { var len = 1u64; len; }",
            "use std.string; fun main() { val string = 1u64; string; }",
            "use std.string.{len, eq}; fun main() { val len = 1u64; len; }",
            "use std.string.len; fun main() { if (true) { val len = 1u64; len; } }",
        ] {
            let (_, diags) = resolve_src(src);
            let errors = diags.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
            assert_eq!(errors.len(), 1, "{src}: {diags:?}");
            assert_eq!(errors[0].code.as_deref(), Some("E206"), "{src}: {diags:?}");
        }
    }

    #[test]
    fn parameter_shadowing_import_is_e206() {
        let (_, diags) = resolve_src("use std.string.len; fun f(len: u64): u64 { return len; }");
        let errors = diags.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "{diags:?}");
        assert_eq!(errors[0].code.as_deref(), Some("E206"), "{diags:?}");
    }

    #[test]
    fn destructure_and_match_bindings_shadowing_imports_are_e206() {
        let (_, diags) = resolve_src(
            "use std.string.len; fun main() { val t = #(1u64, 2u64); val #(len, y) = t; y; }",
        );
        let errors = diags.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "{diags:?}");
        assert_eq!(errors[0].code.as_deref(), Some("E206"), "{diags:?}");

        let (_, diags) = resolve_src(
            "type U = union { A(u64), }; use std.string.len; fun main() { val u = U.A(1u64); match (u) { U.A(len) { len; } else { 0u64; } } }",
        );
        let errors = diags.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "{diags:?}");
        assert_eq!(errors[0].code.as_deref(), Some("E206"), "{diags:?}");
    }

    #[test]
    fn local_shadowing_poisoned_import_stays_quiet() {
        // A failed import is already one root cause (E202); reusing its name
        // locally must not add an E206 cascade.
        let (toks, _) = vl_lex::lex("use missing.{f}; fun main() { val f = 1u64; f; }");
        let (prog, pdiags) = vl_syntax::parse(&toks, "");
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (_, diags) = resolve_with_modules(&prog, &[]);
        let errors = diags.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "{diags:?}");
        assert_eq!(errors[0].code.as_deref(), Some("E202"), "{diags:?}");
    }

    #[test]
    fn unresolved_assign_target_is_one_e201() {
        let (_, diags) = resolve_src("fun main() { missing = 1u64; }");
        let errors = diags.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code.as_deref(), Some("E201"));
    }

    #[test]
    fn malformed_exports_are_poisoned_for_importers() {
        let (toks, _) = vl_lex::lex("fun broken(x: u64): Nope { return; }");
        let (provider, _) = vl_syntax::parse_with_module(&toks, "", "demo");
        let (interface, _) = collect_interface(&provider);
        assert!(interface.poisoned_exports.contains(&"broken".to_string()));

        let (toks, _) = vl_lex::lex("use demo.broken; fun main() { broken(1u64); }");
        let (importer, _) = vl_syntax::parse(&toks, "");
        let (_, diags) = resolve_with_modules(&importer, &[interface.as_spec()]);
        assert!(
            diags.is_empty(),
            "poisoned provider imports must stay silent: {diags:?}"
        );
        assert!(!diags.iter().any(|d| d.code.as_deref() == Some("E203")));
    }

    #[test]
    fn malformed_generic_exports_are_poisoned() {
        for source in [
            "fun broken[T](value:): u64 { return 1u64; }",
            "fun broken[T](value: u64): Nope { return 1u64; }",
        ] {
            let (toks, _) = vl_lex::lex(source);
            let (provider, parse_diags) = vl_syntax::parse_with_module(&toks, "", "demo");
            assert!(
                !parse_diags.is_empty(),
                "source should be malformed: {source}"
            );
            let (interface, _) = collect_interface_quiet(&provider);
            assert!(interface.functions.is_empty(), "{source}");
            assert!(
                interface
                    .poisoned_exports
                    .iter()
                    .any(|name| name == "broken"),
                "{source}"
            );
        }
    }

    #[test]
    fn generic_export_collects_complete_signature() {
        let (toks, _) = vl_lex::lex("fun id[T](value: T): T { return value; }");
        let (provider, _) = vl_syntax::parse_with_module(&toks, "", "demo.lib");
        let (interface, diags) = collect_interface(&provider);
        assert!(diags.is_empty(), "{diags:?}");
        let export = interface
            .functions
            .iter()
            .find(|e| e.name == "id")
            .expect("id export");
        assert_eq!(export.sig.type_params.len(), 1);
        assert_eq!(export.sig.type_params[0].name, "T");
        assert_eq!(export.kind, vl_common::ExportKind::Source);
    }

    #[test]
    fn generic_imports_resolve_through_all_forms() {
        use vl_common::{Export, FuncSig, ParamSig, TypeParamSig};
        let sig = FuncSig::generic(
            vec![TypeParamSig {
                name: "T".into(),
                bound: None,
            }],
            vec![ParamSig {
                name: "value".into(),
                ty: vl_common::VlType::Param("T".into()),
            }],
            vl_common::VlType::Param("T".into()),
        );
        let module =
            ModuleSpec::new_source(&["demo", "lib"], vec![Export::source("id".into(), sig)]);
        for src in [
            "use demo.lib; fun main() { lib.id(1u64); }",
            "use demo.lib.id; fun main() { id(1u64); }",
            "use demo.lib.{id}; fun main() { id(1u64); }",
        ] {
            let (toks, _) = vl_lex::lex(src);
            let (program, parse_diags) = vl_syntax::parse(&toks, "");
            assert!(parse_diags.is_empty(), "{src}: {parse_diags:?}");
            let (resolution, diags) = resolve_with_modules(&program, std::slice::from_ref(&module));
            assert!(diags.iter().all(|d| !d.is_error()), "{src}: {diags:?}");
            let def = resolution
                .defs
                .iter()
                .find(|d| d.kind == DefKind::ImportedFunction)
                .expect("imported def");
            let sig = def.sig.as_ref().expect("generic sig");
            assert_eq!(sig.type_params.len(), 1, "{src}");
            assert!(def.symbol.is_some(), "{src}");
        }
    }

    #[test]
    fn global_dependent_generic_import_is_one_e208() {
        use vl_common::{Export, FuncSig, ParamSig, TypeParamSig};
        let sig = FuncSig::generic(
            vec![TypeParamSig {
                name: "T".into(),
                bound: None,
            }],
            vec![ParamSig {
                name: "value".into(),
                ty: vl_common::VlType::Param("T".into()),
            }],
            vl_common::VlType::Param("T".into()),
        );
        let mut module =
            ModuleSpec::new_source(&["demo", "lib"], vec![Export::source("id".into(), sig)]);
        module.global_dependent_exports.push("id".into());
        let (toks, _) = vl_lex::lex("use demo.lib.id; fun main() { id(1u64); }");
        let (program, _) = vl_syntax::parse(&toks, "");
        let (_, diags) = resolve_with_modules(&program, &[module]);
        let errors = diags.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "{diags:?}");
        assert_eq!(errors[0].code.as_deref(), Some("E208"), "{diags:?}");
    }

    #[test]
    fn only_exports_that_reference_globals_are_poisoned() {
        let (toks, _) = vl_lex::lex(
            "val shared: u64 = 1u64; fun uses(): u64 { return shared; } fun unrelated(): u64 { return 2u64; }",
        );
        let (program, _) = vl_syntax::parse_with_module(&toks, "", "demo");
        let (interface, diags) = collect_interface(&program);
        assert!(diags.is_empty(), "provider must remain valid: {diags:?}");
        assert!(interface.functions.iter().any(|e| e.name == "uses"));
        assert!(interface
            .global_dependent_exports
            .iter()
            .any(|e| e == "uses"));
        assert!(interface.functions.iter().any(|e| e.name == "unrelated"));
    }

    #[test]
    fn global_dependent_export_is_diagnosed_only_when_imported() {
        let (toks, _) = vl_lex::lex("val shared: u64 = 1u64; fun uses(): u64 { return shared; }");
        let (provider, _) = vl_syntax::parse_with_module(&toks, "", "demo.lib");
        let (interface, provider_diags) = collect_interface(&provider);
        assert!(provider_diags.is_empty());

        let (toks, _) = vl_lex::lex("use demo.lib.uses; fun main() { uses(); }");
        let (importer, _) = vl_syntax::parse(&toks, "");
        let (_, diags) = resolve_with_modules(&importer, &[interface.as_spec()]);
        assert!(diags.iter().any(|d| d.code.as_deref() == Some("E208")));
    }

    #[test]
    fn duplicate_exports_are_not_usable_from_importers() {
        let (toks, _) = vl_lex::lex(
            "fun f(value: u64): u64 { return value; } fun f(value: String): String { return value; }",
        );
        let (program, _) = vl_syntax::parse_with_module(&toks, "", "demo");
        let (interface, _) = collect_interface(&program);
        assert!(interface.functions.is_empty());
        assert!(interface.poisoned_exports.iter().any(|name| name == "f"));
    }

    #[test]
    fn qualified_generic_import_resolves() {
        use vl_common::{Export, FuncSig, ParamSig, TypeParamSig};
        let sig = FuncSig::generic(
            vec![TypeParamSig {
                name: "T".into(),
                bound: None,
            }],
            vec![ParamSig {
                name: "value".into(),
                ty: vl_common::VlType::Param("T".into()),
            }],
            vl_common::VlType::Param("T".into()),
        );
        let module =
            ModuleSpec::new_source(&["demo", "lib"], vec![Export::source("id".into(), sig)]);
        let (toks, _) = vl_lex::lex("use demo.lib; fun main() { lib.id(1u64); }");
        let (program, _) = vl_syntax::parse(&toks, "");
        let (_, diags) = resolve_with_modules(&program, &[module]);
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
        assert!(!diags.iter().any(|d| d.code.as_deref() == Some("E201")));
    }

    #[test]
    fn exported_main_can_be_imported() {
        let module = ModuleSpec::new(&["demo", "lib"], &[("main", &[], vl_common::VlType::Void)]);
        let (toks, _) = vl_lex::lex("use demo.lib.main; fun caller() { main(); }");
        let (program, _) = vl_syntax::parse(&toks, "");
        let (_, diags) = resolve_with_modules(&program, &[module]);
        assert!(
            diags.is_empty(),
            "main imports should be ordinary calls: {diags:?}"
        );
    }

    #[test]
    fn parameter_assign_keeps_target_mapping_for_hir() {
        let (res, diags) = resolve_src("fun f(x: u64) { x = 1u64; }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        // Target mapping retained so HIR stays structurally complete.
        let uses = res.uses.len();
        assert!(uses >= 1, "assign target must remain mapped");
        let param_def = res.defs.iter().find(|d| d.name == "x").expect("param def");
        assert_eq!(param_def.kind, DefKind::Parameter);
    }

    #[test]
    fn duplicate_parameter_suppresses_e205() {
        let (_, diags) = resolve_src("fun f(x: u64, x: u64) { x = 1u64; }");
        let errors = diags.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "{diags:?}");
        assert_eq!(errors[0].code.as_deref(), Some("E200"), "{diags:?}");
    }

    fn vl_codegen_modules() -> Vec<ModuleSpec> {
        use vl_common::VlType as T;
        vec![
            ModuleSpec::new(
                &["std"],
                &[
                    ("print", &[("value", T::String)], T::Void),
                    ("println", &[("value", T::String)], T::Void),
                    ("print_u64", &[("value", T::U64)], T::Void),
                ],
            ),
            ModuleSpec::new(
                &["std", "fs"],
                &[
                    ("open", &[("path", T::String)], T::File),
                    ("read", &[("file", T::File)], T::String),
                ],
            ),
            ModuleSpec::new(
                &["std", "string"],
                &[("len", &[("value", T::String)], T::U64)],
            ),
        ]
    }
}
