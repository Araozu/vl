//! vl-typecheck: type checking over HIR.
//!
//! Value types are the compiler-owned [`Ty`] (`u64`, `i64`, `f64`, `bool`,
//! `u8`, `String`, `File`, named reference-semantic objects, `Array[T]`,
//! `void`) converted from [`vl_common::VlType`].
//! These are VL language types enforced here — deliberately distinct from any
//! VM representation, which backends map to separately.
//!
//! Generics are purely a frontend concern: `Array[T]` checks element types,
//! generic functions (`fun first[T](a: Array[T]): T`) check once with
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
use vl_hir::{BindingKind, HirBinOp, HirExpr, HirItem, HirProgram, HirStmt, HirUnOp};

mod mono;
pub mod world;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
    /// User-defined nominal object (reference semantics).
    Object(String),
    /// Nominal union instantiation (`Option`, `Option[u64]`). Reference
    /// semantics (heap tag + payload); constructed via variants, read via
    /// `match`. Boxed to keep [`Ty`] small on the stack: generic-instance
    /// checking nests types dozens deep.
    Union(Box<UnionTy>),
    /// Fixed-length heap array of `T` (reference type, like `String`).
    Array(Box<Ty>),
    /// Fixed-arity heterogeneous tuple (`#(u64, String)`). Value semantics
    /// (copy on bind/assign); each element is `(name, type)` with `None`
    /// for unnamed positions. Uniformly named or unnamed.
    Tuple(Vec<(Option<String>, Ty)>),
    /// Opaque use of an enclosing generic function's type parameter.
    Param(String),
    Void,
    /// Poison: an earlier error made this node's type unknowable.
    /// Poisoned nodes don't produce follow-on errors.
    Error,
    /// Mutable view (`*T`) of a GC-managed reference. Capability-only:
    /// same runtime representation as the read-only view.
    Mutable(Box<Ty>),
}

/// Nominal union instantiation: `name` plus one argument per declared type
/// parameter (`args` aligns positionally). Boxed inside [`Ty::Union`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UnionTy {
    pub name: String,
    pub args: Vec<Ty>,
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
            Ty::String => write!(f, "String"),
            Ty::File => write!(f, "File"),
            Ty::Object(name) => write!(f, "{name}"),
            Ty::Union(u) => {
                // The builtin nullable (`?T` sugar over `Option[T]`) keeps
                // its surface spelling in diagnostics and dumps; every other
                // union prints nominally. Only the single-argument `Option`
                // is the nullable (a multi-arg union also named `Option`
                // stays nominal).
                if u.name == "Option" && u.args.len() == 1 {
                    return write!(f, "?{}", u.args[0]);
                }
                write!(f, "{}", u.name)?;
                if !u.args.is_empty() {
                    write!(f, "[")?;
                    for (i, arg) in u.args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{arg}")?;
                    }
                    write!(f, "]")?;
                }
                Ok(())
            }
            Ty::Array(elem) => write!(f, "Array[{elem}]"),
            Ty::Tuple(fields) => {
                write!(f, "#(")?;
                for (i, (name, ty)) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    match name {
                        Some(name) => write!(f, "{name}: {ty}")?,
                        None => write!(f, "{ty}")?,
                    }
                }
                write!(f, ")")
            }
            Ty::Param(name) => write!(f, "{name}"),
            Ty::Void => write!(f, "void"),
            Ty::Error => write!(f, "<error>"),
            Ty::Mutable(inner) => write!(f, "*{inner}"),
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
            VlType::Object(name) => Ty::Object(name.clone()),
            VlType::Union { name, args } => Ty::Union(Box::new(UnionTy {
                name: name.clone(),
                args: args.iter().map(|a| Self::from_vl_in(a, env)).collect(),
            })),
            // `?T` desugars here to the builtin `Option` union so every
            // later stage (LIR, backends) only sees unions ("sugar all
            // the way"). `??T` nests as `Option[Option[T]]`.
            VlType::Nullable(inner) => Ty::Union(Box::new(UnionTy {
                name: "Option".to_string(),
                args: vec![Self::from_vl_in(inner, env)],
            })),
            VlType::Array(elem) => Ty::Array(Box::new(Self::from_vl_in(elem, env))),
            VlType::Tuple(fields) => Ty::Tuple(
                fields
                    .iter()
                    .map(|f| (f.name.clone(), Self::from_vl_in(&f.ty, env)))
                    .collect(),
            ),
            VlType::Param(name) => env.get(name).cloned().unwrap_or(Ty::Error),
            VlType::Void => Ty::Void,
            VlType::Mutable(inner) => Ty::Mutable(Box::new(Self::from_vl_in(inner, env))),
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
            Ty::Tuple(fields) => fields.iter().all(|(_, ty)| ty.is_concrete()),
            Ty::Union(u) => u.args.iter().all(|a| a.is_concrete()),
            Ty::Mutable(inner) => inner.is_concrete(),
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
    /// Looks through `*` so `*Array[T]` still yields `T`.
    pub fn array_elem(&self) -> Option<&Ty> {
        match self {
            Ty::Array(elem) => Some(elem),
            Ty::Mutable(inner) => inner.array_elem(),
            _ => None,
        }
    }

    /// Tuple element types in order; `None` for non-tuples.
    /// Looks through `*` so `*#(...)` still yields its elements.
    pub fn tuple_elems(&self) -> Option<Vec<(Option<String>, Ty)>> {
        match self {
            Ty::Tuple(fields) => Some(fields.clone()),
            Ty::Mutable(inner) => inner.tuple_elems(),
            _ => None,
        }
    }

    /// True for `*T`.
    pub fn is_mutable_view(&self) -> bool {
        matches!(self, Ty::Mutable(_))
    }

    /// Remove one outer `*` (`*Foo` -> `Foo`).
    pub fn readonly_view(&self) -> Ty {
        match self {
            Ty::Mutable(inner) => (**inner).clone(),
            _ => self.clone(),
        }
    }

    /// Recursively erase capabilities (`*Array[*Foo]` -> `Array[Foo]`).
    pub fn erase_capability(&self) -> Ty {
        match self {
            Ty::Mutable(inner) => inner.erase_capability(),
            Ty::Array(elem) => Ty::Array(Box::new(elem.erase_capability())),
            Ty::Union(u) => Ty::Union(Box::new(UnionTy {
                name: u.name.clone(),
                args: u.args.iter().map(|a| a.erase_capability()).collect(),
            })),
            Ty::Tuple(fields) => Ty::Tuple(
                fields
                    .iter()
                    .map(|(name, ty)| (name.clone(), ty.erase_capability()))
                    .collect(),
            ),
            _ => self.clone(),
        }
    }

    /// Alias for [`Ty::erase_capability`].
    pub fn runtime_type(&self) -> Ty {
        self.erase_capability()
    }

    /// GC-managed reference (including mutable views of one).
    /// Tuples are heap containers (hence `is_ref` on the target) with
    /// value copy semantics at the language level.
    pub fn is_reference_type(&self) -> bool {
        match self {
            Ty::String | Ty::File => true,
            Ty::Object(_) => true,
            Ty::Union { .. } => true,
            Ty::Array(_) => true,
            Ty::Tuple(_) => true,
            Ty::Mutable(inner) => inner.is_reference_type(),
            _ => false,
        }
    }

    /// `void` through an optional outer `*` (`void` or `*void`).
    /// `*void` is invalid (E106) but still counts as void for recovery.
    /// A tuple containing `void` also counts as void.
    pub fn is_void(&self) -> bool {
        match self {
            Ty::Void => true,
            Ty::Mutable(inner) => inner.is_void(),
            Ty::Array(elem) => elem.is_void(),
            Ty::Union(u) => u.args.iter().any(|a| a.is_void()),
            Ty::Tuple(fields) => fields.iter().any(|(_, ty)| ty.is_void()),
            _ => false,
        }
    }
}

/// Substitute type parameters via `env` (`Param(name)` -> mapped type).
/// Names with no entry survive untouched (outer-scope parameters while
/// checking a generic body).
pub fn subst_ty(ty: &Ty, env: &HashMap<String, Ty>) -> Ty {
    match ty {
        Ty::Array(elem) => Ty::Array(Box::new(subst_ty(elem, env))),
        Ty::Union(u) => Ty::Union(Box::new(UnionTy {
            name: u.name.clone(),
            args: u.args.iter().map(|a| subst_ty(a, env)).collect(),
        })),
        Ty::Tuple(fields) => Ty::Tuple(
            fields
                .iter()
                .map(|(name, ty)| (name.clone(), subst_ty(ty, env)))
                .collect(),
        ),
        Ty::Mutable(inner) => Ty::Mutable(Box::new(subst_ty(inner, env))),
        Ty::Param(name) => env.get(name).cloned().unwrap_or(Ty::Param(name.clone())),
        _ => ty.clone(),
    }
}

/// Mangled instance name: `first$u64`, `get$Array$String`. `$` is not lexable
/// in VL source, so instances can never collide with user-written names.
///
/// Encoding is collision-free and deterministic: object names are fully
/// qualified and escaped (`_` -> `__`, `.` -> `_D`, `$` -> `_S`), arrays and
/// capabilities use `Array_`/`Mut_` prefixes, and multiple arguments join
/// with `$` (which never appears inside an encoded argument). Single-argument
/// names (`id$u64`, `same$Array_u8`) are unchanged.
pub fn mangle(name: &str, args: &[Ty]) -> String {
    let parts: Vec<String> = args.iter().map(mangle_ty).collect();
    format!("{name}${}", parts.join("$"))
}

fn sanitize_object_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        match ch {
            '_' => out.push_str("__"),
            '.' => out.push_str("_D"),
            '$' => out.push_str("_S"),
            _ => out.push(ch),
        }
    }
    out
}

fn mangle_ty(ty: &Ty) -> String {
    match ty {
        Ty::Int => "int".into(),
        Ty::U64 => "u64".into(),
        Ty::I64 => "i64".into(),
        Ty::F64 => "f64".into(),
        Ty::Bool => "bool".into(),
        Ty::U8 => "u8".into(),
        Ty::String => "String".into(),
        Ty::File => "File".into(),
        Ty::Object(name) => format!("Object_{}", sanitize_object_name(name)),
        Ty::Union(u) => {
            // `Option[u64]` -> `Union_Option_u64`; bare `Option` ->
            // `Union_Option`. `$` never appears inside an encoded argument,
            // so multi-argument joins stay collision-free.
            let mut out = format!("Union_{}", sanitize_object_name(&u.name));
            for arg in &u.args {
                out.push('_');
                out.push_str(&mangle_ty(arg));
            }
            out
        }
        Ty::Array(elem) => format!("Array_{}", mangle_ty(elem)),
        Ty::Tuple(fields) => {
            let parts: Vec<String> = fields
                .iter()
                .map(|(name, ty)| match name {
                    Some(name) => format!("{name}_{}", mangle_ty(ty)),
                    None => mangle_ty(ty),
                })
                .collect();
            format!("Tuple_{}", parts.join("_"))
        }
        Ty::Mutable(inner) => format!("Mut_{}", mangle_ty(inner)),
        Ty::Param(name) => sanitize_object_name(name),
        Ty::Void => "void".into(),
        Ty::Error => "error".into(),
    }
}

/// Statically known layout of one user-defined object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectSigTy {
    pub fields: Vec<(String, Ty)>,
}

/// Declaration-only metadata for one nominal union. It is intentionally
/// separate from [`ObjectSigTy`]: unions have no fields, methods, or runtime
/// layout in this milestone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnionSigTy {
    pub type_params: Vec<String>,
    pub variants: Vec<UnionVariantSigTy>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnionVariantSigTy {
    pub name: String,
    pub payload: Vec<Ty>,
}

/// Builtin nullable union backing `?T` / `null`: `Option[T]` with
/// `None` (tag 0, no payload) and `Some(T)` (tag 1). Available without a
/// declaration; a local `type Option` shadows it in `typed.unions`.
pub fn builtin_option_sig() -> UnionSigTy {
    UnionSigTy {
        type_params: vec!["T".to_string()],
        variants: vec![
            UnionVariantSigTy {
                name: "None".to_string(),
                payload: Vec::new(),
            },
            UnionVariantSigTy {
                name: "Some".to_string(),
                payload: vec![Ty::Param("T".to_string())],
            },
        ],
    }
}

/// True when `ty` is a nullable (`Option[T]`) instantiation, returning its
/// single argument. Looks through an outer `*` so `*?T` counts.
pub fn nullable_inner_ty(ty: &Ty) -> Option<&Ty> {
    match ty {
        Ty::Union(u) if u.name == "Option" && u.args.len() == 1 => Some(&u.args[0]),
        Ty::Mutable(inner) => nullable_inner_ty(inner),
        _ => None,
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

    /// Convert a shared catalog signature (possibly generic) into a callable
    /// signature. Type parameters become opaque `Param` types; bounds are
    /// preserved for bound checking.
    pub fn from_shared(sig: &vl_common::FuncSig) -> Self {
        let env: HashMap<String, Ty> = sig
            .type_params
            .iter()
            .map(|p| (p.name.clone(), Ty::Param(p.name.clone())))
            .collect();
        Self {
            param_names: sig.params.iter().map(|p| p.name.clone()).collect(),
            param_tys: sig
                .params
                .iter()
                .map(|p| Ty::from_vl_in(&p.ty, &env))
                .collect(),
            ret: Ty::from_vl_in(&sig.ret, &env),
            type_params: sig.type_params.iter().map(|p| p.name.clone()).collect(),
            bounds: sig
                .type_params
                .iter()
                .filter_map(|p| p.bound.map(|b| (p.name.clone(), b)))
                .collect(),
        }
    }
}

pub use vl_common::TemplateKey;

/// One instance-sugar call site, bundled so checking stays under the
/// argument-count lint.
struct MethodCallParts<'a> {
    id: vl_hir::HirId,
    receiver: &'a HirExpr,
    method: &'a str,
    method_span: Span,
    type_args: &'a [VlType],
    args: &'a [HirExpr],
    span: Span,
}
/// One foreign associated function from the module catalog: its owner module
/// plus its shared (possibly generic) signature.
#[derive(Debug, Clone)]
struct ForeignMethod {
    owner_module: String,
    /// Short `Type.method` spelling for messages and cross-module identity.
    dotted: String,
    sig: FuncSigTy,
    global_dependent: bool,
}

/// Result of locating an associated function for a receiver object type.
#[derive(Debug, Clone)]
enum AssocLookup {
    /// Same-module method: template def plus HIR `Fn` name and signature.
    Local {
        def: u32,
        fn_name: String,
        sig: FuncSigTy,
    },
    Foreign(ForeignMethod),
    /// Known to exist but unavailable across the boundary (provider root
    /// cause already reported): poison quietly.
    Poisoned,
    /// No such method on the object. `has_field` steers toward field access.
    Missing {
        has_field: bool,
    },
}

/// Identity of one concrete instantiation: its template plus structural type
/// arguments. The emitted symbol is derived from this key only at the LIR
/// boundary; never use a pre-mangled string as semantic identity upstream.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InstanceKey {
    pub template: TemplateKey,
    pub args: Vec<Ty>,
}

impl InstanceKey {
    pub fn new(template: TemplateKey, args: Vec<Ty>) -> Self {
        Self { template, args }
    }

    pub fn mangled(&self) -> String {
        mangle(&self.template.function, &self.args)
    }
}

impl PartialOrd for InstanceKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for InstanceKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Deterministic order for the worklist: owner, function, then the
        // collision-free mangling (injective in `args`, so consistent with
        // `Eq`).
        (
            self.template.module.clone(),
            self.template.function.clone(),
            self.mangled(),
        )
            .cmp(&(
                other.template.module.clone(),
                other.template.function.clone(),
                other.mangled(),
            ))
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
    /// User-defined object declarations and their field types.
    pub objects: HashMap<String, ObjectSigTy>,
    /// Nominal union declarations, kept out of object layouts and methods.
    pub unions: HashMap<String, UnionSigTy>,
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
    /// Imported concrete call site (`HirId.0`) -> requested owner instance.
    /// Only calls in non-generic code land here; calls inside generic
    /// templates resolve per-instance via the world fixed point.
    pub imported_root_calls: HashMap<u32, InstanceKey>,
    /// `(outer local instance, call site)` -> requested owner instance for
    /// imported callees discovered by the local worklist. The world fixed
    /// point generalizes this to `(outer InstanceKey, call site)`.
    pub imported_inst_calls: HashMap<(String, u32), InstanceKey>,
    /// Concrete imported requests discovered in monomorphic code and global
    /// initializers, awaiting the project-wide fixed point.
    pub pending_imported: Vec<InstanceKey>,
    /// Resolved instance-sugar call site (`HirId.0`) -> call target. Every
    /// successfully checked `MethodCall` lands here; generic sugar calls
    /// additionally land in `root_calls` / `imported_root_calls` so
    /// monomorphization reuses the ordinary generic paths.
    pub method_targets: HashMap<u32, MethodTarget>,
    /// Sugar call site (`HirId.0`) -> local method template `DefId.0`.
    /// Feeds the monomorphization worklist for method calls inside generic
    /// templates (mirrors `Call.def` for ordinary calls).
    pub method_defs: HashMap<u32, u32>,
    /// Sugar call site (`HirId.0`) -> foreign provider identity.
    /// Feeds import collection and the world fixed point for foreign method
    /// calls (mirrors `Call.symbol` for ordinary calls).
    pub method_symbols: HashMap<u32, vl_common::SymbolRef>,
    /// HIR value nodes implicitly wrapped as `Option.Some` for a `?T`
    /// expectation (`val x: ?u64 = 5;`). LIR emits a `NewVariant Some`
    /// around the lowered inner value; the recorded type is the nullable.
    pub nullable_wraps: std::collections::HashSet<u32>,
}

/// Resolved target of one instance-sugar call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MethodTarget {
    /// Same-module method: HIR `Fn` name (`Owner.method`).
    Local(String),
    /// Foreign associated function: provider identity (`Type.method` in the
    /// owner module).
    Foreign(vl_common::SymbolRef),
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
        HirExpr::ObjectLiteral { fields, .. } => {
            for (_, value) in fields {
                template_expr_ids(value, out);
            }
        }
        HirExpr::Variant { args, .. } => {
            for a in args {
                template_expr_ids(a, out);
            }
        }
        HirExpr::Index { base, index, .. } => {
            template_expr_ids(base, out);
            template_expr_ids(index, out);
        }
        HirExpr::TupleLiteral { elems, .. } => {
            for (_, value) in elems {
                template_expr_ids(value, out);
            }
        }
        HirExpr::TupleIndex { base, .. } => template_expr_ids(base, out),
        HirExpr::Field { base, .. } => template_expr_ids(base, out),
        HirExpr::Call { args, .. } => {
            for a in args {
                template_expr_ids(a, out);
            }
        }
        HirExpr::MethodCall { receiver, args, .. } => {
            template_expr_ids(receiver, out);
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
        HirExpr::Literal { .. }
        | HirExpr::String { .. }
        | HirExpr::Null { .. }
        | HirExpr::Var { .. } => {}
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
        HirStmt::FieldAssign {
            id, base, value, ..
        } => {
            out.insert(id.0);
            template_expr_ids(base, out);
            template_expr_ids(value, out);
        }
        HirStmt::TupleAssign {
            id, base, value, ..
        } => {
            out.insert(id.0);
            template_expr_ids(base, out);
            template_expr_ids(value, out);
        }
        HirStmt::Destructure { id, value, .. } => {
            out.insert(id.0);
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
        HirStmt::Match {
            scrutinee,
            arms,
            else_body,
            ..
        } => {
            template_expr_ids(scrutinee, out);
            for arm in arms {
                for st in &arm.body {
                    template_stmt_ids(st, out);
                }
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

/// All `HirId.0` values of one fun template (the item id plus every
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

/// All `HirId.0` values inside generic fun templates (bodies, params,
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

/// True when a converted type names a qualified object (`a.b.C`) with no
/// layout in scope. Callers poison quietly: [`validate_qualified_types`]
/// already reported the E302, so any follow-on mismatch would cascade.
fn ty_has_unknown_qualified(
    ty: &Ty,
    objects: &HashMap<String, ObjectSigTy>,
    unions: &HashMap<String, UnionSigTy>,
) -> bool {
    match ty {
        Ty::Object(name) => {
            name.contains('.') && !objects.contains_key(name) && !unions.contains_key(name)
        }
        Ty::Array(elem) => ty_has_unknown_qualified(elem, objects, unions),
        Ty::Union(u) => u
            .args
            .iter()
            .any(|a| ty_has_unknown_qualified(a, objects, unions)),
        Ty::Tuple(fields) => fields
            .iter()
            .any(|(_, ty)| ty_has_unknown_qualified(ty, objects, unions)),
        Ty::Mutable(inner) => ty_has_unknown_qualified(inner, objects, unions),
        _ => false,
    }
}

/// Resolve union spellings in a converted annotation (see
/// [`Checker::vl_to_ty`](Checker::vl_to_ty)). Free function so the module
/// pre-pass (which owns no `Checker` yet) shares the exact rules.
fn normalize_union_ty(
    diags: &mut Vec<Diagnostic>,
    unions: &HashMap<String, UnionSigTy>,
    objects: &HashMap<String, ObjectSigTy>,
    ty: Ty,
    span: Span,
) -> Ty {
    match ty {
        Ty::Union(u) => {
            let name = u.name.clone();
            let args = u
                .args
                .into_iter()
                .map(|a| normalize_union_ty(diags, unions, objects, a, span))
                .collect::<Vec<_>>();
            if args.iter().any(ty_has_error) {
                return Ty::Error;
            }
            // Builtin `Option` backs `?T` / `null` without a declaration;
            // a local `type Option` shadows it (checked first).
            let builtin =
                (name == "Option" && !unions.contains_key(&name)).then(builtin_option_sig);
            let sig: &UnionSigTy = match unions.get(&name) {
                Some(sig) => sig,
                None => match &builtin {
                    Some(b) => b,
                    None => {
                        if objects.contains_key(&name) {
                            diags.push(
                                Diagnostic::error(format!(
                                    "unknown type `{name}` with type arguments"
                                ))
                                .with_label(
                                    span,
                                    format!("`{name}` is an object and takes no `[...]` arguments"),
                                )
                                .with_note("only `union` types take `[...]` type arguments")
                                .with_code("E105"),
                            );
                        } else {
                            let mut diag = Diagnostic::error(format!(
                                "unknown type `{name}`{}",
                                if args.is_empty() {
                                    String::new()
                                } else {
                                    format!(
                                        "[{}]",
                                        args.iter()
                                            .map(|a| a.to_string())
                                            .collect::<Vec<_>>()
                                            .join(", ")
                                    )
                                }
                            ))
                            .with_label(span, "no union with this name is in scope")
                            .with_code("E105");
                            // Foreign unions need their qualified spelling
                            // (`m.Option[u64]`); suggest it when unambiguous.
                            if !name.contains('.') {
                                let mut qualified: Vec<&String> = unions
                                    .keys()
                                    .filter(|k| {
                                        k.rsplit('.').next().is_some_and(|short| short == name)
                                    })
                                    .collect();
                                qualified.sort();
                                qualified.dedup();
                                if qualified.len() == 1 {
                                    diag = diag.with_note(format!(
                                        "did you mean `{}`? (unions from other modules need their qualified spelling)",
                                        qualified[0]
                                    ));
                                }
                            }
                            diags.push(diag);
                        }
                        return Ty::Error;
                    }
                },
            };
            if args.len() != sig.type_params.len() {
                diags.push(
                    Diagnostic::error(format!(
                        "union `{name}` expects {} type argument(s), got {}",
                        sig.type_params.len(),
                        args.len()
                    ))
                    .with_label(
                        span,
                        format!(
                            "write `{name}[...]` with {} argument(s)",
                            sig.type_params.len()
                        ),
                    )
                    .with_code("E302"),
                );
                return Ty::Error;
            }
            Ty::Union(Box::new(UnionTy { name, args }))
        }
        Ty::Object(name) => {
            // A bare object spelling naming a union: the parser resolves
            // file-local bare unions itself, so only qualified spellings
            // (`m.Option`), the builtin `Option` (bare `Option` without
            // arguments), and odd orders land here.
            let builtin =
                (name == "Option" && !unions.contains_key(&name)).then(builtin_option_sig);
            let sig_opt: Option<&UnionSigTy> = unions.get(&name).or(builtin.as_ref());
            if let Some(sig) = sig_opt {
                if sig.type_params.is_empty() {
                    return Ty::Union(Box::new(UnionTy {
                        name,
                        args: Vec::new(),
                    }));
                }
                diags.push(
                    Diagnostic::error(format!(
                        "union `{name}` expects {} type argument(s), got 0",
                        sig.type_params.len()
                    ))
                    .with_label(
                        span,
                        format!(
                            "write `{name}[...]` with {} argument(s)",
                            sig.type_params.len()
                        ),
                    )
                    .with_code("E302"),
                );
                return Ty::Error;
            }
            Ty::Object(name)
        }
        Ty::Array(elem) => {
            let elem = normalize_union_ty(diags, unions, objects, *elem, span);
            if ty_has_error(&elem) {
                return Ty::Error;
            }
            Ty::Array(Box::new(elem))
        }
        Ty::Tuple(fields) => {
            let mut out = Vec::with_capacity(fields.len());
            for (fname, fty) in fields {
                let fty = normalize_union_ty(diags, unions, objects, fty, span);
                if ty_has_error(&fty) {
                    return Ty::Error;
                }
                out.push((fname, fty));
            }
            Ty::Tuple(out)
        }
        Ty::Mutable(inner) => {
            let inner = normalize_union_ty(diags, unions, objects, *inner, span);
            if ty_has_error(&inner) {
                return Ty::Error;
            }
            Ty::Mutable(Box::new(inner))
        }
        _ => ty,
    }
}

/// Report qualified object references (`vl.person.Person`) with no layout in
/// scope. Bare names were already validated by the parser against the file's
/// own `type` items, so only dotted spellings are checked here: each gets one
/// E302 pointing at its annotation. Cast targets are skipped (`check_cast`
/// owns that position); other uses (literals, field access) resolve through
/// the same map and report themselves during inference.
fn validate_qualified_types(
    prog: &HirProgram,
    objects: &HashMap<String, ObjectSigTy>,
    unions: &HashMap<String, UnionSigTy>,
) -> Vec<Diagnostic> {
    fn check_ty(
        ty: &VlType,
        span: Span,
        objects: &HashMap<String, ObjectSigTy>,
        unions: &HashMap<String, UnionSigTy>,
        diags: &mut Vec<Diagnostic>,
    ) {
        match ty {
            VlType::Object(name)
                if name.contains('.')
                    && !objects.contains_key(name)
                    && !unions.contains_key(name) =>
            {
                diags.push(
                    Diagnostic::error(format!("cannot find object type `{name}`"))
                        .with_label(span, "unknown object type")
                        .with_code("E302"),
                );
            }
            VlType::Array(elem) => check_ty(elem, span, objects, unions, diags),
            VlType::Nullable(inner) => check_ty(inner, span, objects, unions, diags),
            VlType::Union { args, .. } => {
                // Union heads are validated during conversion
                // (`normalize_union_ty` owns E105/E302); only dotted names
                // nested in the arguments still need the walk. Bare argument
                // names were parser-validated.
                for arg in args {
                    check_ty(arg, span, objects, unions, diags);
                }
            }
            VlType::Tuple(fields) => {
                for f in fields {
                    check_ty(&f.ty, span, objects, unions, diags);
                }
            }
            VlType::Mutable(inner) => check_ty(inner, span, objects, unions, diags),
            _ => {}
        }
    }
    fn check_expr(
        expr: &HirExpr,
        objects: &HashMap<String, ObjectSigTy>,
        unions: &HashMap<String, UnionSigTy>,
        diags: &mut Vec<Diagnostic>,
    ) {
        match expr {
            HirExpr::Cast { inner, .. } => {
                // Target owned by `check_cast`; it reports once itself.
                check_expr(inner, objects, unions, diags);
            }
            HirExpr::Call {
                type_args,
                args,
                span,
                ..
            } => {
                for arg in type_args {
                    check_ty(arg, *span, objects, unions, diags);
                }
                for arg in args {
                    check_expr(arg, objects, unions, diags);
                }
            }
            HirExpr::MethodCall {
                type_args,
                receiver,
                args,
                span,
                ..
            } => {
                for arg in type_args {
                    check_ty(arg, *span, objects, unions, diags);
                }
                check_expr(receiver, objects, unions, diags);
                for arg in args {
                    check_expr(arg, objects, unions, diags);
                }
            }
            HirExpr::ArrayLiteral { elems, .. } => {
                for elem in elems {
                    check_expr(elem, objects, unions, diags);
                }
            }
            HirExpr::ObjectLiteral { fields, .. } => {
                for (_, value) in fields {
                    check_expr(value, objects, unions, diags);
                }
            }
            HirExpr::Variant {
                type_args,
                args,
                span,
                ..
            } => {
                for arg in type_args {
                    check_ty(arg, *span, objects, unions, diags);
                }
                for arg in args {
                    check_expr(arg, objects, unions, diags);
                }
            }
            HirExpr::Index { base, index, .. } => {
                check_expr(base, objects, unions, diags);
                check_expr(index, objects, unions, diags);
            }
            HirExpr::TupleLiteral { elems, .. } => {
                for (_, value) in elems {
                    check_expr(value, objects, unions, diags);
                }
            }
            HirExpr::TupleIndex { base, .. } => check_expr(base, objects, unions, diags),
            HirExpr::Field { base, .. } => check_expr(base, objects, unions, diags),
            HirExpr::Binary { lhs, rhs, .. } => {
                check_expr(lhs, objects, unions, diags);
                check_expr(rhs, objects, unions, diags);
            }
            HirExpr::Unary { inner, .. } => check_expr(inner, objects, unions, diags),
            HirExpr::Literal { .. }
            | HirExpr::String { .. }
            | HirExpr::Null { .. }
            | HirExpr::Var { .. } => {}
        }
    }
    fn check_stmts(
        stmts: &[HirStmt],
        objects: &HashMap<String, ObjectSigTy>,
        unions: &HashMap<String, UnionSigTy>,
        diags: &mut Vec<Diagnostic>,
    ) {
        for stmt in stmts {
            match stmt {
                HirStmt::Let {
                    ty, ty_span, value, ..
                } => {
                    if let (Some(ty), Some(span)) = (ty, ty_span) {
                        check_ty(ty, *span, objects, unions, diags);
                    }
                    check_expr(value, objects, unions, diags);
                }
                HirStmt::Assign { value, .. } => check_expr(value, objects, unions, diags),
                HirStmt::Expr(value) => check_expr(value, objects, unions, diags),
                HirStmt::IndexAssign {
                    array,
                    index,
                    value,
                    ..
                } => {
                    check_expr(array, objects, unions, diags);
                    check_expr(index, objects, unions, diags);
                    check_expr(value, objects, unions, diags);
                }
                HirStmt::FieldAssign { base, value, .. } => {
                    check_expr(base, objects, unions, diags);
                    check_expr(value, objects, unions, diags);
                }
                HirStmt::TupleAssign { base, value, .. } => {
                    check_expr(base, objects, unions, diags);
                    check_expr(value, objects, unions, diags);
                }
                HirStmt::Destructure {
                    ty, ty_span, value, ..
                } => {
                    if let (Some(ty), Some(span)) = (ty, ty_span) {
                        check_ty(ty, *span, objects, unions, diags);
                    }
                    check_expr(value, objects, unions, diags);
                }
                HirStmt::If {
                    condition,
                    then_body,
                    else_body,
                    ..
                } => {
                    check_expr(condition, objects, unions, diags);
                    check_stmts(then_body, objects, unions, diags);
                    if let Some(else_body) = else_body {
                        check_stmts(else_body, objects, unions, diags);
                    }
                }
                HirStmt::Match {
                    scrutinee,
                    arms,
                    else_body,
                    ..
                } => {
                    check_expr(scrutinee, objects, unions, diags);
                    for arm in arms {
                        check_stmts(&arm.body, objects, unions, diags);
                    }
                    if let Some(else_body) = else_body {
                        check_stmts(else_body, objects, unions, diags);
                    }
                }
                HirStmt::While {
                    condition, body, ..
                } => {
                    check_expr(condition, objects, unions, diags);
                    check_stmts(body, objects, unions, diags);
                }
                HirStmt::Return { value, .. } => {
                    if let Some(value) = value {
                        check_expr(value, objects, unions, diags);
                    }
                }
                HirStmt::Break { .. } | HirStmt::Continue { .. } => {}
            }
        }
    }
    let mut diags = Vec::new();
    for item in &prog.items {
        match item {
            HirItem::Object { fields, .. } => {
                for (_, ty, span) in fields {
                    if let Some(ty) = ty {
                        check_ty(ty, *span, objects, unions, &mut diags);
                    }
                }
            }
            HirItem::Union { variants, .. } => {
                for variant in variants {
                    for (ty, span) in &variant.payload {
                        check_ty(ty, *span, objects, unions, &mut diags);
                    }
                }
            }
            HirItem::Let {
                ty, ty_span, value, ..
            } => {
                if let (Some(ty), Some(span)) = (ty, ty_span) {
                    check_ty(ty, *span, objects, unions, &mut diags);
                }
                check_expr(value, objects, unions, &mut diags);
            }
            HirItem::Destructure {
                ty, ty_span, value, ..
            } => {
                if let (Some(ty), Some(span)) = (ty, ty_span) {
                    check_ty(ty, *span, objects, unions, &mut diags);
                }
                check_expr(value, objects, unions, &mut diags);
            }
            HirItem::Fn {
                params,
                ret,
                ret_span,
                body,
                ..
            } => {
                for (_, _, ty, span) in params {
                    if let Some(ty) = ty {
                        check_ty(ty, *span, objects, unions, &mut diags);
                    }
                }
                if let (Some(ret), Some(span)) = (ret, ret_span) {
                    check_ty(ret, *span, objects, unions, &mut diags);
                }
                check_stmts(body, objects, unions, &mut diags);
            }
        }
    }
    diags
}

pub fn check(prog: &HirProgram) -> (TypedProgram, Vec<Diagnostic>) {
    check_with_modules(prog, &[])
}

/// Check one module with every project module's exported object layouts in
/// scope. Local objects are keyed both bare (`Person`, for code written in
/// the defining module) and qualified (`vl.person.Person`); foreign objects
/// are keyed qualified only, so nominal identity never collides across
/// modules. Unknown qualified references are reported once here (E302);
/// field and literal uses stay quiet downstream when the layout is missing.
pub fn check_with_modules(
    prog: &HirProgram,
    modules: &[vl_common::ModuleSpec],
) -> (TypedProgram, Vec<Diagnostic>) {
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
        module: prog.module.clone(),
        pending_instances: Vec::new(),
        pending_imported: Vec::new(),
        assoc_local: HashMap::new(),
        assoc_foreign: HashMap::new(),
        assoc_poisoned: HashSet::new(),
        fixed_defs: HashSet::new(),
    };
    // Merge foreign layouts first so local declarations can reference
    // them (a local field may hold a `vl.other.Person`). The driver's own
    // interface is included in `modules`; it is skipped because the local
    // definitions below are the canonical entry.
    for spec in modules {
        if spec.path.as_string() == prog.module {
            continue;
        }
        for export in &spec.objects {
            if cx.typed.objects.contains_key(&export.qualified) {
                continue;
            }
            let mut out_fields = Vec::with_capacity(export.fields.len());
            for field in &export.fields {
                out_fields.push((
                    field.name.clone(),
                    Ty::from_vl_in(&field.ty, &HashMap::new()),
                ));
            }
            cx.typed
                .objects
                .insert(export.qualified.clone(), ObjectSigTy { fields: out_fields });
            // Associated functions join the foreign method table under their
            // qualified owner so instance sugar (`p.method()`) resolves
            // without an import, like object layouts. Poisoned and
            // global-dependent marks mirror the free-function boundary.
            for method in &export.methods {
                cx.assoc_foreign.insert(
                    (export.qualified.clone(), method.name.clone()),
                    ForeignMethod {
                        owner_module: spec.path.as_string(),
                        dotted: format!("{}.{}", export.name, method.name),
                        sig: FuncSigTy::from_shared(&method.sig),
                        global_dependent: spec
                            .global_dependent_exports
                            .iter()
                            .any(|e| e == &format!("{}.{}", export.name, method.name)),
                    },
                );
            }
            for poisoned in &spec.poisoned_exports {
                if let Some((owner, method)) = poisoned.split_once('.') {
                    if owner == export.name {
                        cx.assoc_poisoned
                            .insert((export.qualified.clone(), method.to_string()));
                    }
                }
            }
        }
        for export in &spec.unions {
            if cx.typed.unions.contains_key(&export.qualified) {
                continue;
            }
            let env: HashMap<String, Ty> = export
                .type_params
                .iter()
                .map(|p| (p.name.clone(), Ty::Param(p.name.clone())))
                .collect();
            cx.typed.unions.insert(
                export.qualified.clone(),
                UnionSigTy {
                    type_params: export.type_params.iter().map(|p| p.name.clone()).collect(),
                    variants: export
                        .variants
                        .iter()
                        .map(|variant| UnionVariantSigTy {
                            name: variant.name.clone(),
                            payload: variant
                                .payload
                                .iter()
                                .map(|ty| Ty::from_vl_in(ty, &env))
                                .collect(),
                        })
                        .collect(),
                },
            );
        }
    }
    // Predeclare every local nominal name before validating any declaration so
    // qualified forward/self references are known without manufacturing union
    // object layouts.
    for item in &prog.items {
        match item {
            HirItem::Union {
                name, type_params, ..
            } => {
                let sig = UnionSigTy {
                    type_params: type_params.iter().map(|p| p.name.clone()).collect(),
                    variants: Vec::new(),
                };
                cx.typed.unions.entry(name.clone()).or_insert(sig.clone());
                cx.typed
                    .unions
                    .entry(format!("{}.{}", prog.module, name))
                    .or_insert(sig);
            }
            HirItem::Object { name, .. } => {
                cx.typed
                    .objects
                    .entry(name.clone())
                    .or_insert_with(|| ObjectSigTy { fields: Vec::new() });
                cx.typed
                    .objects
                    .entry(format!("{}.{}", prog.module, name))
                    .or_insert_with(|| ObjectSigTy { fields: Vec::new() });
            }
            _ => {}
        }
    }
    // Pass 0: collect nominal union metadata separately from object layouts.
    for item in &prog.items {
        let HirItem::Union {
            name,
            type_params,
            variants,
            ..
        } = item
        else {
            continue;
        };
        let env: HashMap<String, Ty> = type_params
            .iter()
            .map(|p| (p.name.clone(), Ty::Param(p.name.clone())))
            .collect();
        let out_variants = variants
            .iter()
            .map(|variant| UnionVariantSigTy {
                name: variant.name.clone(),
                payload: variant
                    .payload
                    .iter()
                    .map(|(ty, span)| {
                        let mut payload_ty = Checker::vl_to_ty_in(
                            &mut cx.diags,
                            &cx.typed.unions,
                            &cx.typed.objects,
                            ty,
                            &env,
                            *span,
                        );
                        if ty_has_error(&payload_ty)
                            || payload_ty.is_void()
                            || ty_has_unknown_qualified(
                                &payload_ty,
                                &cx.typed.objects,
                                &cx.typed.unions,
                            )
                            || !validate_capability(&payload_ty, *span, &mut cx.diags)
                        {
                            payload_ty = Ty::Error;
                        }
                        payload_ty
                    })
                    .collect(),
            })
            .collect();
        let sig = UnionSigTy {
            type_params: type_params.iter().map(|p| p.name.clone()).collect(),
            variants: out_variants,
        };
        cx.typed.unions.insert(name.clone(), sig.clone());
        cx.typed
            .unions
            .insert(format!("{}.{}", prog.module, name), sig);
    }
    // Pass 1: collect object layouts so field types and object literals can
    // refer to declarations in either order.
    for item in &prog.items {
        if let HirItem::Object { name, fields, .. } = item {
            let mut out_fields = Vec::with_capacity(fields.len());
            for (field, ty, span) in fields {
                let mut field_ty = ty
                    .as_ref()
                    .map(|v| {
                        Checker::vl_to_ty_in(
                            &mut cx.diags,
                            &cx.typed.unions,
                            &cx.typed.objects,
                            v,
                            &HashMap::new(),
                            *span,
                        )
                    })
                    .unwrap_or(Ty::Error);
                if ty_has_error(&field_ty) {
                    // Parser already reported (unknown type); stay quiet.
                } else if ty_has_unknown_qualified(&field_ty, &cx.typed.objects, &cx.typed.unions) {
                    // Qualified reference with no layout in scope: the
                    // validation walk below owns the E302, so poison quietly.
                    field_ty = Ty::Error;
                } else if !validate_capability(&field_ty, *span, &mut cx.diags) {
                    field_ty = Ty::Error;
                } else if field_ty.is_void() {
                    cx.diags.push(
                        Diagnostic::error("an object field cannot be `void`")
                            .with_label(*span, "`void` is not a value type")
                            .with_code("E104"),
                    );
                    field_ty = Ty::Error;
                }
                out_fields.push((field.clone(), field_ty));
            }
            let sig = ObjectSigTy { fields: out_fields };
            cx.typed.objects.insert(name.clone(), sig.clone());
            // Qualified identity for cross-module references and
            // self-references spelled `vl.person.Person`.
            cx.typed
                .objects
                .insert(format!("{}.{}", prog.module, name), sig);
        }
    }
    cx.diags.append(&mut validate_qualified_types(
        prog,
        &cx.typed.objects,
        &cx.typed.unions,
    ));
    // Pass 1: collect function signatures so calls resolve arity + types
    // regardless of definition order (matches the resolver pre-pass).
    for item in &prog.items {
        if let HirItem::Fn {
            def: Some(d),
            params,
            type_params,
            ret,
            ret_span,
            span,
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
            let mut param_tys = Vec::with_capacity(params.len());
            for (pname, _, t, pspan) in params {
                let mut pt = t
                    .as_ref()
                    .map(|v| {
                        Checker::vl_to_ty_in(
                            &mut cx.diags,
                            &cx.typed.unions,
                            &cx.typed.objects,
                            v,
                            &env,
                            *pspan,
                        )
                    })
                    .unwrap_or(Ty::Error);
                if !ty_has_error(&pt)
                    && ty_has_unknown_qualified(&pt, &cx.typed.objects, &cx.typed.unions)
                {
                    // Qualified reference with no layout: the validation walk
                    // owns the E302, so poison quietly instead of cascading
                    // arity-independent E303s at every call site.
                    pt = Ty::Error;
                }
                if !ty_has_error(&pt) && !validate_capability(&pt, *pspan, &mut cx.diags) {
                    pt = Ty::Error;
                } else if !ty_has_error(&pt) && pt.is_void() {
                    cx.diags.push(
                        Diagnostic::error(format!("parameter `{pname}` cannot be `void`"))
                            .with_label(*pspan, "`void` is not a value type")
                            .with_code("E104"),
                    );
                    pt = Ty::Error;
                }
                // Note: HIR params carry the name span (type span is erased at
                // lowering); capability diagnostics anchor there.
                param_tys.push(pt);
            }
            let mut ret_ty = ret
                .as_ref()
                .map(|v| {
                    Checker::vl_to_ty_in(
                        &mut cx.diags,
                        &cx.typed.unions,
                        &cx.typed.objects,
                        v,
                        &env,
                        ret_span.unwrap_or(*span),
                    )
                })
                .unwrap_or(Ty::Error);
            if !ty_has_error(&ret_ty)
                && ty_has_unknown_qualified(&ret_ty, &cx.typed.objects, &cx.typed.unions)
            {
                ret_ty = Ty::Error;
            }
            if !ty_has_error(&ret_ty) {
                let rsp = ret_span.unwrap_or(*span);
                if !validate_capability(&ret_ty, rsp, &mut cx.diags) {
                    ret_ty = Ty::Error;
                }
            }
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
    // Associated methods are `Fn` items named `Owner.method`: index their
    // template defs for instance-sugar lookup (first declaration wins, like
    // the resolver's table, so duplicates agree on the surviving target).
    for item in &prog.items {
        if let HirItem::Fn {
            def: Some(d), name, ..
        } = item
        {
            if let Some((owner, method)) = name.split_once('.') {
                cx.assoc_local
                    .entry((owner.to_string(), method.to_string()))
                    .or_insert(d.0);
            }
        }
    }
    for item in &prog.items {
        cx.check_item(item);
    }
    // Separate monomorphization pass: instance expansion owns caching,
    // poison suppression, expanding-recursion diagnostics, and budgets.
    let pending = std::mem::take(&mut cx.pending_instances);
    let pending_imported = std::mem::take(&mut cx.pending_imported);
    let mut typed = std::mem::take(&mut cx.typed);
    let mut diags = std::mem::take(&mut cx.diags);
    drop(cx);
    typed.pending_imported = pending_imported;
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
    /// Owning module for object canonicalization in instance keys.
    module: String,
    /// Concrete `(template DefId.0, args)` pairs awaiting the separate
    /// monomorphization pass ([`mono::expand`]).
    pending_instances: Vec<(u32, Vec<Ty>)>,
    /// Concrete imported requests awaiting the project-wide fixed point.
    pending_imported: Vec<InstanceKey>,
    /// `(Owner, method)` -> method template `DefId.0` for this module's own
    /// associated functions (owner spelled as in the HIR `Fn` name).
    assoc_local: HashMap<(String, String), u32>,
    /// `(qualified Owner, method)` -> foreign associated function.
    assoc_foreign: HashMap<(String, String), ForeignMethod>,
    /// `(qualified Owner, method)` known-but-unavailable across the boundary.
    assoc_poisoned: HashSet<(String, String)>,
    /// Fixed `val` bindings share the assignment-poisoning path with params.
    fixed_defs: HashSet<u32>,
}

pub(crate) fn canonicalize_for_key(
    ty: &Ty,
    caller: &str,
    objects: &HashMap<String, ObjectSigTy>,
) -> Ty {
    match ty {
        Ty::Object(name) if !name.contains('.') => {
            let qualified = format!("{caller}.{name}");
            if objects.contains_key(&qualified) {
                Ty::Object(qualified)
            } else {
                ty.clone()
            }
        }
        Ty::Array(elem) => Ty::Array(Box::new(canonicalize_for_key(elem, caller, objects))),
        Ty::Mutable(inner) => Ty::Mutable(Box::new(canonicalize_for_key(inner, caller, objects))),
        _ => ty.clone(),
    }
}

/// Union arguments carried by an expected type (`Union` or `*Union`) when it
/// names `union` with `arity` parameters. `None` means "no usable context"
/// (a different type, poisoned, or an arity mismatch — inference proceeds and
/// the boundary reports any real mismatch).
fn expected_union_args(expected: Option<&Ty>, union: &str, arity: usize) -> Option<Vec<Ty>> {
    let want = expected?;
    let inner = match want {
        Ty::Union(u) if u.name == union => &u.args,
        Ty::Mutable(inner) => match &**inner {
            Ty::Union(u) if u.name == union => &u.args,
            _ => return None,
        },
        _ => return None,
    };
    if inner.len() != arity || inner.iter().any(ty_has_error) {
        return None;
    }
    Some(inner.clone())
}

impl Checker {
    fn record(&mut self, id: vl_hir::HirId, ty: Ty) -> Ty {
        self.typed.types.insert(id.0, ty.clone());
        ty
    }

    /// Convert a user annotation to [`Ty`], resolving union spellings against
    /// the declared (local + imported) unions.
    ///
    /// - `VlType::Union` validates the union exists and the arity matches.
    /// - Bare `VlType::Object` naming a monomorphic union becomes
    ///   `Union{name, []}` (covers qualified `m.U`, which the parser cannot
    ///   resolve); naming a generic union is one E302 (write `U[...]`).
    ///
    /// Recurses through `Array`/`Tuple`/`Union` arguments/`*` so
    /// `Array[Option[u64]]` works. Reports once per call; callers poison via
    /// the returned [`Ty::Error`].
    fn vl_to_ty(&mut self, v: &VlType, span: Span) -> Ty {
        let ty = Ty::from_vl_in(v, &self.type_env);
        self.normalize_union_ty(ty, span)
    }

    /// Same as [`vl_to_ty`](Self::vl_to_ty) with an explicit type-parameter
    /// environment (union declarations and the module pre-pass, which build
    /// their own `env` instead of using the current function scope).
    fn vl_to_ty_in(
        diags: &mut Vec<Diagnostic>,
        unions: &HashMap<String, UnionSigTy>,
        objects: &HashMap<String, ObjectSigTy>,
        v: &VlType,
        env: &HashMap<String, Ty>,
        span: Span,
    ) -> Ty {
        let ty = Ty::from_vl_in(v, env);
        normalize_union_ty(diags, unions, objects, ty, span)
    }

    fn normalize_union_ty(&mut self, ty: Ty, span: Span) -> Ty {
        normalize_union_ty(
            &mut self.diags,
            &self.typed.unions,
            &self.typed.objects,
            ty,
            span,
        )
    }

    /// Look up a union by spelling (bare `Option` or qualified `m.Option`).
    /// Falls back to the builtin `Option` backing `?T` / `null` when no
    /// local declaration shadows it.
    fn union_sig(&self, name: &str) -> Option<UnionSigTy> {
        if let Some(sig) = self.typed.unions.get(name) {
            return Some(sig.clone());
        }
        if name == "Option" {
            return Some(builtin_option_sig());
        }
        None
    }

    fn check_item(&mut self, item: &HirItem) {
        match item {
            HirItem::Object { .. } => {}
            HirItem::Union { .. } => {}
            HirItem::Let {
                id,
                def,
                kind,
                ty,
                ty_span,
                value,
                ..
            } => {
                let ty = self.binding_type(*kind, ty, ty_span, value);
                self.record(*id, ty.clone());
                if let Some(def) = def {
                    self.bindings.insert(def.0, ty);
                    if *kind == BindingKind::Val {
                        self.fixed_defs.insert(def.0);
                    }
                    self.typed.globals.push(format!("{kind:?}#{}", id.0));
                } else {
                    // Name resolution already reported this; stay quiet.
                }
            }
            HirItem::Destructure {
                id,
                kind,
                bindings,
                ty,
                ty_span,
                value,
                span,
            } => {
                let base = self.check_destructure(*kind, bindings, ty, ty_span, value, *span);
                self.record(*id, base.clone());
                for b in bindings {
                    if let Some(def) = &b.def {
                        self.bindings
                            .insert(def.0, self.binding_element_ty(&base, b));
                        if *kind == BindingKind::Val {
                            self.fixed_defs.insert(def.0);
                        }
                        self.typed
                            .globals
                            .push(format!("{kind:?}#{}#{}", id.0, b.index));
                    }
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
                // Signatures were validated once in the pre-pass (annotations
                // converted, union spellings normalized, E104/E106/E302 owned
                // there); reuse them so no second error cascades here.
                let (mut ret_ty, sig_params) = match def {
                    Some(d) => match self.typed.func_sigs.get(&d.0) {
                        Some(sig) => (sig.ret.clone(), sig.param_tys.clone()),
                        None => (Ty::Error, vec![Ty::Error; params.len()]),
                    },
                    None => (Ty::Error, vec![Ty::Error; params.len()]),
                };
                self.record(*id, ret_ty.clone());
                // Bad annotations were already reported by the parser
                // (E104/E105) or by Pass 1 (E106/E302); poison the scope
                // quietly so no second error cascades. (An omitted return
                // parses as `void`, never `None`.) Capability-invalid shapes
                // (`*T`, `*u64` surviving as `Error` excluded) poison quietly
                // here without re-reporting: Pass 1 already owns E106.
                let mut poisoned_sig =
                    ty_has_error(&ret_ty) || params.iter().any(|(_, _, t, _)| t.is_none());
                if !ty_has_error(&ret_ty) && !is_capability_valid(&ret_ty) {
                    ret_ty = Ty::Error;
                    poisoned_sig = true;
                }
                for ((_, def, _, _), t) in params.iter().zip(sig_params.iter()) {
                    if let Some(def) = def {
                        let mut t = t.clone();
                        if !ty_has_error(&t) && !is_capability_valid(&t) {
                            t = Ty::Error;
                            poisoned_sig = true;
                        }
                        self.bindings.insert(def.0, t);
                        self.fixed_defs.insert(def.0);
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
                let _ = ret;
            }
        }
    }

    /// Element type for one destructure binding given the checked base
    /// tuple type (or `Error` when the base was poisoned).
    fn binding_element_ty(&self, base: &Ty, b: &vl_hir::HirDestructureBinding) -> Ty {
        if ty_has_error(base) {
            return Ty::Error;
        }
        let Some(elems) = base.tuple_elems() else {
            return Ty::Error;
        };
        if elems.iter().all(|(n, _)| n.is_none()) {
            return elems
                .get(b.index)
                .map(|(_, t)| project_capability(base, t))
                .unwrap_or(Ty::Error);
        }
        // Named: explicit `field:` wins, else the binding name is the field.
        let want = b.field.as_deref().unwrap_or(b.binding.as_str());
        if let Some((_, t)) = elems.iter().find(|(n, _)| n.as_deref() == Some(want)) {
            return project_capability(base, t);
        }
        Ty::Error
    }

    /// Check `val #(pats) = value;` (items and statements share this).
    /// Returns the base tuple type (or `Error`). On success each binding is
    /// entered in `bindings`/`fixed_defs`; on any shape error all bindings
    /// become `Error` with exactly one diagnostic.
    fn check_destructure(
        &mut self,
        kind: BindingKind,
        bindings: &[vl_hir::HirDestructureBinding],
        ty: &Option<VlType>,
        ty_span: &Option<Span>,
        value: &HirExpr,
        span: Span,
    ) -> Ty {
        // Poison every binding quietly (the one root cause is reported by the caller).
        let poison = |checker: &mut Self| {
            for b in bindings {
                if let Some(def) = &b.def {
                    checker.bindings.insert(def.0, Ty::Error);
                    if kind == BindingKind::Val {
                        checker.fixed_defs.insert(def.0);
                    }
                }
            }
        };
        // Failed annotation (parser-reported): infer inner errors only.
        if ty.is_none() && ty_span.is_some() {
            let _ = self.infer_expr(value);
            poison(self);
            return Ty::Error;
        }
        let ann = ty.as_ref().map(|v| {
            let asp = ty_span.unwrap_or(value.span());
            self.vl_to_ty(v, asp)
        });
        if let Some(a) = &ann {
            if ty_has_error(a) {
                let _ = self.infer_expr(value);
                poison(self);
                return Ty::Error;
            }
            if ty_has_unknown_qualified(a, &self.typed.objects, &self.typed.unions) {
                let _ = self.infer_expr(value);
                poison(self);
                return Ty::Error;
            }
            let asp = ty_span.unwrap_or(value.span());
            if !validate_capability(a, asp, &mut self.diags) {
                let _ = self.infer_expr(value);
                poison(self);
                return Ty::Error;
            }
            if a.is_void() {
                self.diags.push(
                    Diagnostic::error("a binding cannot be `void`")
                        .with_label(asp, "`void` is not a value")
                        .with_code("E104"),
                );
                let _ = self.infer_expr(value);
                poison(self);
                return Ty::Error;
            }
        }
        let base = match &ann {
            Some(a) => self.infer_expr_expected(value, a),
            None => self.infer_expr(value),
        };
        if ty_has_error(&base) {
            poison(self);
            return Ty::Error;
        }
        if base == Ty::Void || base.is_void() {
            self.diags.push(
                Diagnostic::error("cannot destructure a `void` value")
                    .with_label(value.span(), "`void` is not a value")
                    .with_code("E308"),
            );
            poison(self);
            return Ty::Error;
        }
        let Some(elems) = base.tuple_elems() else {
            self.diags.push(
                Diagnostic::error(format!("cannot destructure `{base}`"))
                    .with_label(value.span(), "only tuples support destructuring")
                    .with_code("E309"),
            );
            poison(self);
            return Ty::Error;
        };
        if elems.len() != bindings.len() {
            self.diags.push(
                Diagnostic::error(format!(
                    "tuple `{base}` has {} element(s), pattern binds {}",
                    elems.len(),
                    bindings.len()
                ))
                .with_label(span, "arity mismatch in destructure pattern")
                .with_code("E309"),
            );
            // Still infer element types for inner uses? Poison to stay quiet.
            poison(self);
            return Ty::Error;
        }
        if let Some(a) = &ann {
            if !can_coerce(&base, a) {
                self.diags.push(
                    Diagnostic::error(format!("cannot destructure `{base}` as `{a}`"))
                        .with_label(value.span(), format!("expected `{a}` here"))
                        .with_code("E309"),
                );
                poison(self);
                return Ty::Error;
            }
        }
        let unnamed = elems.iter().all(|(n, _)| n.is_none());
        // Validate pattern shape against the tuple kind.
        for (i, b) in bindings.iter().enumerate() {
            if unnamed {
                if b.field.is_some() {
                    self.diags.push(
                        Diagnostic::error(format!("tuple `{base}` is unnamed"))
                            .with_label(
                                span,
                                "remove `field:` from the pattern; write `#(a, b, ...)`",
                            )
                            .with_code("E309"),
                    );
                    poison(self);
                    return Ty::Error;
                }
                let _ = i;
            } else {
                // Named tuple: explicit `field:` or shorthand binding name.
                // Shorthand validation needs the source name, which lives in
                // the resolver def; check existence positionally here and let
                // LIR resolve the exact slot by field-or-index.
                if let Some(field) = &b.field {
                    if !elems
                        .iter()
                        .any(|(n, _)| n.as_deref() == Some(field.as_str()))
                    {
                        self.diags.push(
                            Diagnostic::error(format!("tuple `{base}` has no field `{field}`"))
                                .with_label(span, "unknown tuple field in pattern")
                                .with_code("E309"),
                        );
                        poison(self);
                        return Ty::Error;
                    }
                } else {
                    // Shorthand: the binding name must be an existing field.
                    if !elems
                        .iter()
                        .any(|(n, _)| n.as_deref() == Some(b.binding.as_str()))
                    {
                        self.diags.push(
                            Diagnostic::error(format!(
                                "tuple `{base}` has no field `{}`",
                                b.binding
                            ))
                            .with_label(b.binding_span, "unknown tuple field in pattern")
                            .with_code("E309"),
                        );
                        poison(self);
                        return Ty::Error;
                    }
                }
            }
        }
        // Success: enter each binding with its projected element type.
        // Unnamed tuples bind positionally; named tuples resolve by explicit
        // `field:` or by the binding name (shorthand) so reordered patterns
        // like `#(y, x)` still bind the right element types.
        for b in bindings {
            let ty = if unnamed {
                elems
                    .get(b.index)
                    .map(|(_, t)| project_capability(&base, t))
                    .unwrap_or(Ty::Error)
            } else {
                let want = b.field.as_deref().unwrap_or(b.binding.as_str());
                elems
                    .iter()
                    .find(|(n, _)| n.as_deref() == Some(want))
                    .map(|(_, t)| project_capability(&base, t))
                    .unwrap_or(Ty::Error)
            };
            if ty_has_error(&ty) {
                poison(self);
                return Ty::Error;
            }
            if let Some(def) = &b.def {
                // Default untyped int elements through the u64 lane.
                let ty = if ty == Ty::Int { Ty::U64 } else { ty };
                self.bindings.insert(def.0, ty);
                if kind == BindingKind::Val {
                    self.fixed_defs.insert(def.0);
                }
            }
        }
        // Default any lingering `int` elements is handled per binding above;
        // the base itself may still hold `Int` for literal bases, which the
        // binding-type defaulting path already coerced during inference.
        base
    }

    /// Check a binding initializer against its optional annotation. Returns
    /// the binding type (`Error` when poisoned). An `Array[T]`/`*Array[T]`
    /// annotation on a bare `Array.new(n)` supplies `T` contextually; fresh
    /// object/array literals adopt an expected mutable capability; every
    /// other shape infers first and then must coerce directionally.
    fn binding_type(
        &mut self,
        kind: BindingKind,
        ty: &Option<VlType>,
        ty_span: &Option<Span>,
        value: &HirExpr,
    ) -> Ty {
        // Failed annotation (parser-reported): infer inner errors only.
        if ty.is_none() && ty_span.is_some() {
            let _ = self.infer_expr(value);
            return Ty::Error;
        }
        let ann = ty.as_ref().map(|v| {
            let asp = ty_span.unwrap_or(value.span());
            self.vl_to_ty(v, asp)
        });
        if let Some(a) = &ann {
            if ty_has_error(a) {
                let _ = self.infer_expr(value);
                return Ty::Error;
            }
            if ty_has_unknown_qualified(a, &self.typed.objects, &self.typed.unions) {
                // Qualified annotation with no layout: the validation walk
                // owns the E302, so poison quietly instead of cascading E309.
                let _ = self.infer_expr(value);
                return Ty::Error;
            }
            let asp = ty_span.unwrap_or(value.span());
            if !validate_capability(a, asp, &mut self.diags) {
                let _ = self.infer_expr(value);
                return Ty::Error;
            }
            if a.is_void() {
                self.diags.push(
                    Diagnostic::error("a binding cannot be `void`")
                        .with_label(asp, "`void` is not a value")
                        .with_code("E104"),
                );
                let _ = self.infer_expr(value);
                return Ty::Error;
            }
        }
        // Contextual bare `Array.new(n)`: `Array[T]` or `*Array[T]` annotation
        // supplies the element type. Returns the (possibly mutable) array.
        if let (
            Some(a),
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
            let elem_opt: Option<&Ty> = match a {
                Ty::Array(elem) => Some(elem),
                Ty::Mutable(inner) => match &**inner {
                    Ty::Array(elem) => Some(elem),
                    _ => None,
                },
                _ => None,
            };
            if let Some(elem) = elem_opt {
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
                    let arr = self.check_array_new_elem(
                        name,
                        *span,
                        (*elem).clone(),
                        args,
                        &arg_tys,
                        *call_id,
                    );
                    if ty_has_error(&arr) {
                        return Ty::Error;
                    }
                    // Contextual capability: `*Array[T]` annotation makes the
                    // fresh allocation mutable.
                    if a.is_mutable_view() {
                        return self.record(*call_id, a.clone());
                    }
                    return arr;
                }
            }
        }
        let inferred = match &ann {
            Some(a) => self.infer_expr_expected(value, a),
            None => self.infer_expr(value),
        };
        if ty_has_error(&inferred) {
            return Ty::Error;
        }
        if inferred == Ty::Void || inferred.is_void() {
            self.diags.push(
                Diagnostic::error("cannot bind a `void` value")
                    .with_label(value.span(), "`void` is not a value")
                    .with_note("`void` calls may only appear as bare statements")
                    .with_code("E308"),
            );
            return Ty::Error;
        }
        if let Some(a) = &ann {
            if !can_coerce(&inferred, a) {
                // Focused capability diagnostics where the shapes match except
                // for authority; generic shape mismatches stay E309.
                if inferred.readonly_view() == a.readonly_view()
                    && inferred.is_mutable_view() != a.is_mutable_view()
                {
                    self.diags.push(
                        Diagnostic::error(format!(
                            "cannot initialize `{a}` binding with read-only `{inferred}` value"
                        ))
                        .with_label(value.span(), "mutation authority is required here")
                        .with_note(format!("a read-only view cannot be upgraded to `{a}`"))
                        .with_code("E309"),
                    );
                } else {
                    self.diags.push(
                        Diagnostic::error(format!(
                            "cannot initialize `{a}` binding with `{inferred}` value"
                        ))
                        .with_label(value.span(), format!("expected `{a}` here"))
                        .with_code("E309"),
                    );
                }
                return Ty::Error;
            }
            // Binding type is the declared type; the value keeps its actual
            // capability (already recorded by inference).
            return a.clone();
        }
        // Unannotated bindings holding an unresolved `int` (top level or
        // nested, e.g. `Array[Int]`) resolve through the target's default
        // unsigned lane (`u64`): an `int` variable must not stay compatible
        // with every integer type, or `var v = 300; take_u8(v);` would pass.
        if ty_contains_int(&inferred) {
            let defaulted = default_inferred_ty(inferred.clone());
            self.coerce_expr_literals(value, &defaulted);
            let resolved = self.infer_expr(value);
            if ty_has_error(&resolved) {
                return Ty::Error;
            }
            let inferred = if ty_contains_int(&resolved) {
                // Non-coercible shape (unreachable for literals, which the
                // arms above handle): record the default so no `Int` lingers
                // for the LIR boundary.
                self.record(value.id(), defaulted)
            } else {
                resolved
            };
            return self.finish_inferred_binding(kind, inferred, value);
        }
        self.finish_inferred_binding(kind, inferred, value)
    }

    /// Apply the default outer capability for an unannotated binding. Fresh
    /// allocations can adopt mutable context; calls and existing values keep
    /// their declared capability and therefore cannot be upgraded.
    fn finish_inferred_binding(&mut self, kind: BindingKind, inferred: Ty, value: &HirExpr) -> Ty {
        if !inferred.is_reference_type() {
            return inferred;
        }
        let expected = match kind {
            BindingKind::Var if !inferred.is_mutable_view() => {
                Ty::Mutable(Box::new(inferred.clone()))
            }
            BindingKind::Val => inferred.readonly_view(),
            BindingKind::Var => inferred.clone(),
        };
        if same_type(&inferred, &expected) {
            return expected;
        }
        let contextual = self.infer_expr_expected(value, &expected);
        if ty_has_error(&contextual) {
            return Ty::Error;
        }
        if can_coerce(&contextual, &expected) {
            return expected;
        }
        if contextual.readonly_view() == expected.readonly_view()
            && contextual.is_mutable_view() != expected.is_mutable_view()
        {
            self.diags.push(
                Diagnostic::error(format!(
                    "cannot initialize inferred `{expected}` binding with read-only `{contextual}` value"
                ))
                .with_label(value.span(), "mutation authority is required here")
                .with_note(format!("a read-only view cannot be upgraded to `{expected}`"))
                .with_code("E309"),
            );
        } else {
            self.diags.push(
                Diagnostic::error(format!(
                    "cannot initialize inferred `{expected}` binding with `{contextual}` value"
                ))
                .with_label(value.span(), format!("expected `{expected}` here"))
                .with_code("E309"),
            );
        }
        Ty::Error
    }

    /// Check a statement. There are no implicit returns: binding initializers
    /// and bare expression values are discarded and never satisfy a declared
    /// return type — only an explicit `return expr;` does.
    fn check_stmt(&mut self, stmt: &HirStmt) {
        match stmt {
            HirStmt::Let {
                id,
                def,
                kind,
                ty,
                ty_span,
                value,
                ..
            } => {
                let ty = self.binding_type(*kind, ty, ty_span, value);
                self.record(*id, ty.clone());
                if let Some(def) = def {
                    self.bindings.insert(def.0, ty);
                    if *kind == BindingKind::Val {
                        self.fixed_defs.insert(def.0);
                    }
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
                        if !can_coerce(&got, &self.fn_ret) {
                            // Capability upgrade through a return must never
                            // launder a read-only view into a mutable result.
                            if got.readonly_view() == self.fn_ret.readonly_view()
                                && got.is_mutable_view() != self.fn_ret.is_mutable_view()
                            {
                                self.diags.push(
                                    Diagnostic::error(format!(
                                        "function `{}` declares return `{}` but returns read-only `{got}`",
                                        self.fn_name, self.fn_ret
                                    ))
                                    .with_label(*span, "mutation authority is required here")
                                    .with_note(format!(
                                        "a read-only view cannot be upgraded to `{}`",
                                        self.fn_ret
                                    ))
                                    .with_code("E307"),
                                );
                            } else {
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
            }
            HirStmt::Assign { id, def, value, .. } => {
                let Some(def) = def else {
                    // Unresolved target already reported (E201); stay quiet.
                    self.record(*id, Ty::Error);
                    return;
                };
                // Direct parameter rebinding already has its root cause (E205
                // from resolution). Infer the RHS for inner errors, then poison
                // quietly without a second boundary diagnostic.
                if self.fixed_defs.contains(&def.0) {
                    let _ = self.infer_expr(value);
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
                        if !can_coerce(&got, &want) {
                            if got.readonly_view() == want.readonly_view()
                                && got.is_mutable_view() != want.is_mutable_view()
                            {
                                self.diags.push(
                                    Diagnostic::error(format!(
                                        "cannot assign read-only `{got}` to `{want}` binding"
                                    ))
                                    .with_label(value.span(), "mutation authority is required here")
                                    .with_note(format!(
                                        "a read-only view cannot be upgraded to `{want}`"
                                    ))
                                    .with_code("E309"),
                                );
                            } else {
                                self.diags.push(
                                    Diagnostic::error(format!(
                                        "cannot assign `{got}` to `{want}` binding"
                                    ))
                                    .with_label(value.span(), format!("expected `{want}` here"))
                                    .with_code("E309"),
                                );
                            }
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
                // Element writes need a mutable array view.
                if !at.is_mutable_view() {
                    self.diags.push(
                        Diagnostic::error(format!(
                            "cannot assign element through read-only view `{at}`"
                        ))
                        .with_label(
                            array.span(),
                            format!("this expression has read-only type `{at}`"),
                        )
                        .with_note(format!(
                            "use a `*{at}` binding when this code must mutate it",
                        ))
                        .with_code("E310"),
                    );
                    let _ = self.infer_expr(value);
                    self.record(*id, Ty::Error);
                    return;
                }
                let vt = self.infer_expr_expected(value, &elem);
                if ty_has_error(&vt) || ty_has_error(&elem) {
                    self.record(*id, Ty::Error);
                    return;
                }
                if !same_type(&it, &Ty::U64) {
                    self.diags.push(
                        Diagnostic::error(format!("array index must be `u64`, got `{it}`"))
                            .with_label(index.span(), "expected `u64` here")
                            .with_code("E302"),
                    );
                    self.record(*id, Ty::Error);
                    return;
                }
                if !can_coerce(&vt, &elem) {
                    if vt.readonly_view() == elem.readonly_view()
                        && vt.is_mutable_view() != elem.is_mutable_view()
                    {
                        self.diags.push(
                            Diagnostic::error(format!(
                                "cannot store read-only `{vt}` in mutable element `{elem}`"
                            ))
                            .with_label(value.span(), "mutation authority is required here")
                            .with_note(format!("a read-only view cannot be upgraded to `{elem}`"))
                            .with_code("E302"),
                        );
                    } else {
                        self.diags.push(
                            Diagnostic::error(format!(
                                "cannot store `{vt}` in `{at}` (elements are `{elem}`)"
                            ))
                            .with_label(value.span(), format!("expected `{elem}` here"))
                            .with_code("E302"),
                        );
                    }
                    self.record(*id, Ty::Error);
                    return;
                }
                self.record(*id, elem);
            }
            HirStmt::FieldAssign {
                id,
                base,
                field,
                value,
                span,
            } => {
                let bt = self.infer_expr(base);
                // Named-tuple field write (`t.x = v;`); unnamed writes use TupleAssign.
                if let Some(elems) = bt.tuple_elems() {
                    if ty_has_error(&bt) {
                        let _ = self.infer_expr(value);
                        self.record(*id, Ty::Error);
                        return;
                    }
                    if elems.iter().all(|(n, _)| n.is_none()) {
                        self.diags.push(
                            Diagnostic::error(format!("tuple `{bt}` is unnamed; assign by index"))
                                .with_label(*span, "write ``t.`i` = v;`` for unnamed tuples")
                                .with_code("E302"),
                        );
                        let _ = self.infer_expr(value);
                        self.record(*id, Ty::Error);
                        return;
                    }
                    if !bt.is_mutable_view() {
                        self.diags.push(
                            Diagnostic::error(format!(
                                "cannot assign field `{field}` through read-only view `{bt}`"
                            ))
                            .with_label(
                                base.span(),
                                format!("this expression has read-only type `{bt}`"),
                            )
                            .with_note(format!(
                                "use a `*{bt}` parameter or binding when this function must mutate it"
                            ))
                            .with_code("E310"),
                        );
                        let _ = self.infer_expr(value);
                        self.record(*id, Ty::Error);
                        return;
                    }
                    let Some((_, want)) = elems
                        .iter()
                        .find(|(n, _)| n.as_deref() == Some(field.as_str()))
                    else {
                        self.diags.push(
                            Diagnostic::error(format!("tuple `{bt}` has no field `{field}`"))
                                .with_label(*span, "unknown tuple field")
                                .with_code("E302"),
                        );
                        let _ = self.infer_expr(value);
                        self.record(*id, Ty::Error);
                        return;
                    };
                    let want = want.clone();
                    if ty_has_error(&want) {
                        let _ = self.infer_expr(value);
                        self.record(*id, Ty::Error);
                        return;
                    }
                    let got = self.infer_expr_expected(value, &want);
                    if ty_has_error(&got) || !can_coerce(&got, &want) {
                        if !ty_has_error(&got) {
                            self.diags.push(
                                Diagnostic::error(format!(
                                    "tuple field `{field}` expects `{want}`, got `{got}`"
                                ))
                                .with_label(value.span(), format!("expected `{want}` here"))
                                .with_code("E302"),
                            );
                        }
                        self.record(*id, Ty::Error);
                    } else {
                        self.record(*id, want);
                    }
                    return;
                }
                let Some(object_name) = object_base(&bt) else {
                    if !ty_has_error(&bt) {
                        self.diags.push(
                            Diagnostic::error(format!("cannot assign field `{field}` on `{bt}`"))
                                .with_label(*span, "expected an object value here")
                                .with_code("E302"),
                        );
                    }
                    let _ = self.infer_expr(value);
                    self.record(*id, Ty::Error);
                    return;
                };
                // Field writes need a mutable object view.
                if !bt.is_mutable_view() {
                    self.diags.push(
                        Diagnostic::error(format!(
                            "cannot assign field `{field}` through read-only view `{bt}`"
                        ))
                        .with_label(
                            base.span(),
                            format!("this expression has read-only type `{bt}`"),
                        )
                        .with_note(format!(
                            "use a `*{bt}` parameter or binding when this function must mutate it"
                        ))
                        .with_code("E310"),
                    );
                    let _ = self.infer_expr(value);
                    self.record(*id, Ty::Error);
                    return;
                }
                let Some(sig) = self.typed.objects.get(&object_name).cloned() else {
                    self.record(*id, Ty::Error);
                    return;
                };
                let Some((_, want)) = sig.fields.iter().find(|(name, _)| name == field) else {
                    self.diags.push(
                        Diagnostic::error(format!("object `{object_name}` has no field `{field}`"))
                            .with_label(*span, "unknown object field")
                            .with_code("E302"),
                    );
                    let _ = self.infer_expr(value);
                    self.record(*id, Ty::Error);
                    return;
                };
                // Poisoned field declarations (E106 already reported) stay
                // quiet here to keep one root cause.
                if ty_has_error(want) {
                    let _ = self.infer_expr(value);
                    self.record(*id, Ty::Error);
                    return;
                }
                // Check the stored declaration type, not the projected read
                // type: a mutable receiver does not upgrade a read-only field.
                let got = self.infer_expr_expected(value, want);
                if ty_has_error(&got) || !can_coerce(&got, want) {
                    if !ty_has_error(&got) {
                        // Focused capability message when shapes match except
                        // authority; generic mismatch stays E302.
                        if got.readonly_view() == want.readonly_view()
                            && got.is_mutable_view() != want.is_mutable_view()
                        {
                            self.diags.push(
                                Diagnostic::error(format!(
                                    "cannot assign read-only `{got}` to mutable field `{field}: {want}`"
                                ))
                                .with_label(
                                    value.span(),
                                    "mutation authority is required here",
                                )
                                .with_note(format!(
                                    "a read-only view cannot be upgraded to `{want}`"
                                ))
                                .with_code("E302"),
                            );
                        } else {
                            self.diags.push(
                                Diagnostic::error(format!(
                                    "object field `{field}` expects `{want}`, got `{got}`"
                                ))
                                .with_label(value.span(), format!("expected `{want}` here"))
                                .with_code("E302"),
                            );
                        }
                    }
                    self.record(*id, Ty::Error);
                } else {
                    self.record(*id, want.clone());
                }
            }
            HirStmt::TupleAssign {
                id,
                base,
                index,
                value,
                span,
            } => {
                let bt = self.infer_expr(base);
                if ty_has_error(&bt) {
                    let _ = self.infer_expr(value);
                    self.record(*id, Ty::Error);
                    return;
                }
                let Some(elems) = bt.tuple_elems() else {
                    self.diags.push(
                        Diagnostic::error(format!("cannot assign tuple element on `{bt}`"))
                            .with_label(*span, "only tuples support backtick element writes")
                            .with_code("E302"),
                    );
                    let _ = self.infer_expr(value);
                    self.record(*id, Ty::Error);
                    return;
                };
                if elems.iter().any(|(n, _)| n.is_some()) {
                    self.diags.push(
                        Diagnostic::error(format!("tuple `{bt}` is named; assign by field"))
                            .with_label(*span, "write `t.field = v;` for named tuples")
                            .with_code("E302"),
                    );
                    let _ = self.infer_expr(value);
                    self.record(*id, Ty::Error);
                    return;
                }
                let Some((_, want)) = elems.get(*index).cloned() else {
                    self.diags.push(
                        Diagnostic::error(format!(
                            "tuple index ``.`{index}`` out of range for `{bt}`"
                        ))
                        .with_label(*span, format!("this tuple has {} element(s)", elems.len()))
                        .with_code("E302"),
                    );
                    let _ = self.infer_expr(value);
                    self.record(*id, Ty::Error);
                    return;
                };
                if !bt.is_mutable_view() {
                    self.diags.push(
                        Diagnostic::error(format!(
                            "cannot assign element through read-only view `{bt}`"
                        ))
                        .with_label(
                            base.span(),
                            format!("this expression has read-only type `{bt}`"),
                        )
                        .with_note(format!(
                            "use a `*{bt}` binding when this code must mutate it",
                        ))
                        .with_code("E310"),
                    );
                    let _ = self.infer_expr(value);
                    self.record(*id, Ty::Error);
                    return;
                }
                let got = self.infer_expr_expected(value, &want);
                if ty_has_error(&got) || !can_coerce(&got, &want) {
                    if !ty_has_error(&got) {
                        self.diags.push(
                            Diagnostic::error(format!(
                                "tuple element ``.`{index}`` expects `{want}`, got `{got}`"
                            ))
                            .with_label(value.span(), format!("expected `{want}` here"))
                            .with_code("E302"),
                        );
                    }
                    self.record(*id, Ty::Error);
                    return;
                }
                self.record(*id, want);
            }
            HirStmt::Destructure {
                id,
                kind,
                bindings,
                ty,
                ty_span,
                value,
                span,
            } => {
                let base = self.check_destructure(*kind, bindings, ty, ty_span, value, *span);
                self.record(*id, base);
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
            HirStmt::Match {
                scrutinee,
                arms,
                else_body,
                span,
            } => {
                self.check_match(scrutinee, arms, else_body.as_deref(), *span);
            }
        }
    }

    /// Check `match (scrut) { Union.Variant(binds) { ... } ... else { ... } }`.
    /// The scrutinee must be a `Union` (or `*Union`); each arm must name a
    /// variant of that union with exactly the payload arity, binding each
    /// payload position to a fresh `val` (capability-projected like field
    /// reads). Duplicate arms are one E200; a missing `else` with uncovered
    /// variants is one E309. Bodies always check (poisoned bindings stay
    /// quiet) so one root cause never hides inner errors.
    fn check_match(
        &mut self,
        scrutinee: &HirExpr,
        arms: &[vl_hir::HirMatchArm],
        else_body: Option<&[HirStmt]>,
        span: Span,
    ) {
        let scrut_ty = self.infer_expr(scrutinee);
        // Poison every arm binding quietly (the one root cause is reported
        // by the caller path below).
        let poison_arm = |checker: &mut Self, arm: &vl_hir::HirMatchArm| {
            for b in &arm.bindings {
                if let Some(def) = &b.def {
                    checker.bindings.insert(def.0, Ty::Error);
                    checker.fixed_defs.insert(def.0);
                }
            }
        };
        if ty_has_error(&scrut_ty) {
            for arm in arms {
                poison_arm(self, arm);
                for stmt in &arm.body {
                    self.check_stmt(stmt);
                }
            }
            if let Some(body) = else_body {
                for stmt in body {
                    self.check_stmt(stmt);
                }
            }
            return;
        }
        let scrut_core: Option<(&String, &Vec<Ty>)> = match &scrut_ty {
            Ty::Union(u) => Some((&u.name, &u.args)),
            Ty::Mutable(inner) => match &**inner {
                Ty::Union(u) => Some((&u.name, &u.args)),
                _ => None,
            },
            _ => None,
        };
        let Some((scrut_name, scrut_args)) = scrut_core else {
            self.diags.push(
                Diagnostic::error(format!("match scrutinee must be a union, got `{scrut_ty}`"))
                    .with_label(scrutinee.span(), "expected a union value here")
                    .with_code("E302"),
            );
            for arm in arms {
                poison_arm(self, arm);
                for stmt in &arm.body {
                    self.check_stmt(stmt);
                }
            }
            if let Some(body) = else_body {
                for stmt in body {
                    self.check_stmt(stmt);
                }
            }
            return;
        };
        let scrut_name = scrut_name.clone();
        let scrut_args = scrut_args.clone();
        let Some(sig) = self.union_sig(&scrut_name) else {
            self.diags.push(
                Diagnostic::error(format!("cannot find union type `{scrut_name}`"))
                    .with_label(span, "unknown union type")
                    .with_code("E302"),
            );
            for arm in arms {
                poison_arm(self, arm);
                for stmt in &arm.body {
                    self.check_stmt(stmt);
                }
            }
            if let Some(body) = else_body {
                for stmt in body {
                    self.check_stmt(stmt);
                }
            }
            return;
        };
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut covered: std::collections::HashSet<String> = std::collections::HashSet::new();
        for arm in arms {
            if arm.union != scrut_name {
                self.diags.push(
                    Diagnostic::error(format!(
                        "pattern `{}.{}` does not match scrutinee type `{scrut_name}`",
                        arm.union, arm.variant
                    ))
                    .with_label(arm.span, "this arm matches a different union")
                    .with_code("E302"),
                );
                poison_arm(self, arm);
                for stmt in &arm.body {
                    self.check_stmt(stmt);
                }
                continue;
            }
            let Some(tag) = sig.variants.iter().position(|v| v.name == arm.variant) else {
                self.diags.push(
                    Diagnostic::error(format!(
                        "union `{scrut_name}` has no variant `{}`",
                        arm.variant
                    ))
                    .with_label(arm.span, "unknown union variant")
                    .with_code("E302"),
                );
                poison_arm(self, arm);
                for stmt in &arm.body {
                    self.check_stmt(stmt);
                }
                continue;
            };
            if !seen.insert(arm.variant.clone()) {
                self.diags.push(
                    Diagnostic::error(format!(
                        "duplicate arm for variant `{scrut_name}.{}`",
                        arm.variant
                    ))
                    .with_label(arm.span, "this variant is already matched above")
                    .with_code("E200"),
                );
                poison_arm(self, arm);
                for stmt in &arm.body {
                    self.check_stmt(stmt);
                }
                continue;
            }
            covered.insert(arm.variant.clone());
            let payload = &sig.variants[tag].payload;
            if arm.bindings.len() != payload.len() {
                self.diags.push(
                    Diagnostic::error(format!(
                        "variant `{scrut_name}.{}` has {} payload(s), pattern binds {}",
                        arm.variant,
                        payload.len(),
                        arm.bindings.len()
                    ))
                    .with_label(arm.span, "arity mismatch in match pattern")
                    .with_code("E303"),
                );
                poison_arm(self, arm);
                for stmt in &arm.body {
                    self.check_stmt(stmt);
                }
                continue;
            }
            let env: HashMap<String, Ty> = sig
                .type_params
                .iter()
                .cloned()
                .zip(scrut_args.iter().cloned())
                .collect();
            for (binding, formal) in arm.bindings.iter().zip(payload.iter()) {
                let bound = project_capability(&scrut_ty, &subst_ty(formal, &env));
                if let Some(def) = &binding.def {
                    if ty_has_error(&bound) {
                        self.bindings.insert(def.0, Ty::Error);
                    } else {
                        self.bindings.insert(def.0, bound);
                    }
                    self.fixed_defs.insert(def.0);
                }
            }
            for stmt in &arm.body {
                self.check_stmt(stmt);
            }
        }
        if let Some(body) = else_body {
            for stmt in body {
                self.check_stmt(stmt);
            }
        } else {
            let mut missing: Vec<&str> = sig
                .variants
                .iter()
                .filter(|v| !covered.contains(&v.name))
                .map(|v| v.name.as_str())
                .collect();
            missing.sort();
            if !missing.is_empty() {
                self.diags.push(
                    Diagnostic::error(format!(
                        "match is not exhaustive: missing variant(s) {}",
                        missing
                            .iter()
                            .map(|v| format!("`{scrut_name}.{v}`"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                    .with_label(span, "add an `else` arm for the remaining variants")
                    .with_code("E309"),
                );
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
            // Bare `Array.new(n)` without an element type defers its error
            // until the formal is known (contextual `Array`/` *Array`
            // supplies it); empty `[]` and `null` defer the same way. All
            // other poisoned args stay quiet.
            let is_bare_new = matches!(arg, HirExpr::Call { name, type_args, .. } if name == "Array.new" && type_args.is_empty());
            let is_empty_array =
                matches!(arg, HirExpr::ArrayLiteral { elems, .. } if elems.is_empty());
            let is_null = matches!(arg, HirExpr::Null { .. });
            if ty_has_error(got) && !(is_bare_new || is_empty_array || is_null) {
                continue;
            }
            let want = Ty::from_vl_in(&params[i].ty, &self.type_env);
            if ty_has_error(&want) {
                continue;
            }
            // Fresh literals adopt an expected mutable capability
            // (`Foo {}` for `*Foo` params); existing values never upgrade.
            let got = self.infer_expr_expected(arg, &want);
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
            if !can_coerce(&got, &want) {
                if got.readonly_view() == want.readonly_view()
                    && got.is_mutable_view() != want.is_mutable_view()
                {
                    self.diags.push(
                        Diagnostic::error(format!(
                            "cannot pass read-only `{got}` to mutable parameter `{}: {want}`",
                            params[i].name
                        ))
                        .with_label(arg.span(), "mutation authority is required here")
                        .with_note(format!("a read-only view cannot be upgraded to `{want}`"))
                        .with_code("E306"),
                    );
                } else {
                    self.diags.push(
                        Diagnostic::error(format!(
                            "`{name}` parameter `{}` expects `{}`, got `{got}`",
                            params[i].name, want
                        ))
                        .with_label(arg.span(), format!("expected `{want}` here"))
                        .with_code("E306"),
                    );
                }
                return self.record(id, Ty::Error);
            }
        }
        self.record(id, ret)
    }

    /// Locate an associated function for a receiver object type.
    ///
    /// Qualified receivers never fall back to a same-named local: an owner
    /// spelled `other.Counter` consults only the foreign table, and a bare
    /// `Counter` consults only the local table (foreign values are always
    /// qualified, so nominal identity cannot cross modules by accident).
    fn find_assoc_method(&self, obj: &str, method: &str) -> AssocLookup {
        if let Some(short) = obj.strip_prefix(&format!("{}.", self.module)) {
            // Own-module qualification (`my.mod.Counter`): the local table,
            // keyed by the bare HIR spelling.
            if let Some(def) = self
                .assoc_local
                .get(&(short.to_string(), method.to_string()))
            {
                if let Some(sig) = self.typed.func_sigs.get(def).cloned() {
                    return AssocLookup::Local {
                        def: *def,
                        fn_name: format!("{short}.{method}"),
                        sig,
                    };
                }
            }
            return AssocLookup::Missing {
                has_field: self
                    .typed
                    .objects
                    .get(obj)
                    .or_else(|| self.typed.objects.get(short))
                    .is_some_and(|sig| sig.fields.iter().any(|(f, _)| f == method)),
            };
        }
        if obj.contains('.') {
            // Foreign qualification: the foreign table only, never a
            // same-named local.
            if let Some(foreign) = self
                .assoc_foreign
                .get(&(obj.to_string(), method.to_string()))
            {
                return AssocLookup::Foreign(foreign.clone());
            }
            if self
                .assoc_poisoned
                .contains(&(obj.to_string(), method.to_string()))
            {
                return AssocLookup::Poisoned;
            }
            return AssocLookup::Missing {
                has_field: self
                    .typed
                    .objects
                    .get(obj)
                    .is_some_and(|sig| sig.fields.iter().any(|(f, _)| f == method)),
            };
        }
        // Bare receivers are always local.
        if let Some(def) = self.assoc_local.get(&(obj.to_string(), method.to_string())) {
            if let Some(sig) = self.typed.func_sigs.get(def).cloned() {
                return AssocLookup::Local {
                    def: *def,
                    fn_name: format!("{obj}.{method}"),
                    sig,
                };
            }
        }
        AssocLookup::Missing {
            has_field: self
                .typed
                .objects
                .get(obj)
                .is_some_and(|sig| sig.fields.iter().any(|(f, _)| f == method)),
        }
    }

    /// Check one instance-sugar call `receiver.method(args)`: the receiver
    /// counts as the first argument. The sugar gate requires the method's
    /// first parameter to take the receiver's object type (capability-aware);
    /// everything else checks exactly like an explicit `Owner.method` call,
    /// including generic inference, monomorphization, and E208.
    fn check_method_call(&mut self, call: MethodCallParts<'_>) -> Ty {
        let MethodCallParts {
            id,
            receiver,
            method,
            method_span,
            type_args,
            args,
            span,
        } = call;
        let r_ty = self.infer_expr(receiver);
        // Argument types first, with the same bare-`Array.new` / empty-`[]`
        // / `null` deferral as ordinary calls (context comes from the formal
        // below).
        let mut arg_tys = Vec::with_capacity(args.len());
        let mut poisoned = false;
        for arg in args {
            if let HirExpr::Call {
                name: inner,
                type_args: inner_args,
                args: inner_call_args,
                ..
            } = arg
            {
                if inner == "Array.new" && inner_args.is_empty() {
                    for a in inner_call_args {
                        let _ = self.infer_expr(a);
                    }
                    arg_tys.push(Ty::Error);
                    continue;
                }
            }
            if let HirExpr::ArrayLiteral { elems, .. } = arg {
                if elems.is_empty() {
                    arg_tys.push(Ty::Error);
                    continue;
                }
            }
            if matches!(arg, HirExpr::Null { .. }) {
                arg_tys.push(Ty::Error);
                continue;
            }
            let t = self.infer_expr(arg);
            if ty_has_error(&t) {
                poisoned = true;
            }
            arg_tys.push(t);
        }
        if ty_has_error(&r_ty) {
            return self.record(id, Ty::Error);
        }
        let Some(obj_name) = object_base(&r_ty) else {
            if !ty_has_error(&r_ty) {
                self.diags.push(
                    Diagnostic::error(format!("cannot call method `{method}` on `{r_ty}`"))
                        .with_label(method_span, "expected an object value here")
                        .with_code("E302"),
                );
            }
            return self.record(id, Ty::Error);
        };
        // (display name, template def or foreign owner, callable signature).
        enum Target {
            Local {
                def: u32,
                display: String,
            },
            Foreign {
                owner: String,
                display: String,
                sym: vl_common::SymbolRef,
            },
        }
        let (target, sig) = match self.find_assoc_method(&obj_name, method) {
            AssocLookup::Local { def, fn_name, sig } => (
                Target::Local {
                    def,
                    display: fn_name,
                },
                sig,
            ),
            AssocLookup::Foreign(foreign) => {
                if foreign.owner_module != self.module && foreign.global_dependent {
                    self.diags.push(
                        Diagnostic::error(format!(
                            "imported function `{}.{}` depends on module globals",
                            foreign.owner_module, foreign.dotted
                        ))
                        .with_label(span, "unsupported cross-module boundary")
                        .with_code("E208"),
                    );
                    return self.record(id, Ty::Error);
                }
                let display = format!("{}.{}", foreign.owner_module, foreign.dotted);
                let sym = vl_common::SymbolRef {
                    module: vl_common::ModulePath::from_dotted(&foreign.owner_module),
                    name: foreign.dotted.clone(),
                };
                (
                    Target::Foreign {
                        owner: foreign.owner_module.clone(),
                        display,
                        sym,
                    },
                    foreign.sig,
                )
            }
            AssocLookup::Poisoned => {
                return self.record(id, Ty::Error);
            }
            AssocLookup::Missing { has_field } => {
                let mut diag = Diagnostic::error(format!(
                    "object `{obj_name}` has no associated function `{method}`"
                ))
                .with_label(method_span, "unknown associated function")
                .with_code("E302");
                if has_field {
                    diag = diag.with_note(format!(
                        "`{method}` is a field of `{obj_name}`; read it without `(...)`"
                    ));
                }
                self.diags.push(diag);
                return self.record(id, Ty::Error);
            }
        };
        if ty_has_error(&sig.ret) || sig.param_tys.iter().any(ty_has_error) {
            // Definition already poisoned (missing annotations); quiet.
            return self.record(id, Ty::Error);
        }
        // Generic inference sees the receiver as argument zero.
        let mut full_tys = Vec::with_capacity(arg_tys.len() + 1);
        full_tys.push(r_ty.clone());
        full_tys.extend(arg_tys.iter().cloned());
        let display = match &target {
            Target::Local { display, .. } => display.clone(),
            Target::Foreign { display, .. } => display.clone(),
        };
        let Some(resolved) = self.resolve_type_args(&display, span, &sig, type_args, &full_tys)
        else {
            return self.record(id, Ty::Error);
        };
        let (param_tys, ret_ty) = sig.instantiate(&resolved);
        if param_tys.len() != args.len() + 1 {
            self.diags.push(
                Diagnostic::error(format!(
                    "`{display}` expects {} argument(s), got {}",
                    param_tys.len(),
                    args.len() + 1
                ))
                .with_label(span, "wrong number of arguments")
                .with_code("E303"),
            );
            return self.record(id, Ty::Error);
        }
        if poisoned {
            return self.record(id, Ty::Error);
        }
        // The sugar gate: the first parameter must take the receiver's
        // object. A different object is E303 (sugar does not apply); the same
        // object with an unmet `*` capability is E306 like any other argument.
        let p0 = &param_tys[0];
        match object_base(p0) {
            Some(p0obj) if same_object_name(&p0obj, &obj_name, &self.module) => {
                if !can_coerce(&r_ty, p0) {
                    if r_ty.readonly_view() == p0.readonly_view()
                        && r_ty.is_mutable_view() != p0.is_mutable_view()
                    {
                        self.diags.push(
                            Diagnostic::error(format!(
                                "cannot pass read-only `{r_ty}` to mutable parameter `{}: {p0}`",
                                sig.param_names[0]
                            ))
                            .with_label(receiver.span(), "mutation authority is required here")
                            .with_note(format!("a read-only view cannot be upgraded to `{p0}`"))
                            .with_code("E306"),
                        );
                    } else {
                        self.diags.push(
                            Diagnostic::error(format!(
                                "`{display}` parameter `{}` expects `{p0}`, got `{r_ty}`",
                                sig.param_names[0]
                            ))
                            .with_label(receiver.span(), format!("expected `{p0}` here"))
                            .with_code("E306"),
                        );
                    }
                    return self.record(id, Ty::Error);
                }
            }
            _ => {
                let want = p0.clone();
                self.diags.push(
                    Diagnostic::error(format!(
                        "`{display}` first parameter expects `{want}`, got receiver `{r_ty}`"
                    ))
                    .with_label(
                        receiver.span(),
                        "instance sugar passes the receiver as the first argument",
                    )
                    .with_note(format!(
                        "call `{display}(...)` explicitly, or declare the first parameter as `{obj_name}`"
                    ))
                    .with_code("E303"),
                );
                return self.record(id, Ty::Error);
            }
        }
        // Remaining arguments check exactly like an explicit call, with the
        // receiver prepended so positions and names line up.
        let mut full_args: Vec<&HirExpr> = Vec::with_capacity(args.len() + 1);
        full_args.push(receiver);
        full_args.extend(args.iter());
        let mut full_got: Vec<Ty> = Vec::with_capacity(arg_tys.len() + 1);
        full_got.push(r_ty.clone());
        full_got.extend(arg_tys.iter().cloned());
        for (i, (arg, original_got)) in full_args.iter().zip(full_got.iter()).enumerate() {
            let is_bare_new = matches!(arg, HirExpr::Call { name, type_args, .. } if name == "Array.new" && type_args.is_empty());
            let is_empty_array =
                matches!(arg, HirExpr::ArrayLiteral { elems, .. } if elems.is_empty());
            let is_null = matches!(arg, HirExpr::Null { .. });
            if ty_has_error(original_got) && !(is_bare_new || is_empty_array || is_null) {
                continue;
            }
            let want = &param_tys[i];
            if ty_has_error(want) {
                continue;
            }
            let got = self.infer_expr_expected(arg, want);
            if ty_has_error(&got) {
                continue;
            }
            if got == Ty::Void || *want == Ty::Void {
                self.diags.push(
                    Diagnostic::error(format!(
                        "`{display}` parameter `{}` cannot be `void`",
                        sig.param_names[i]
                    ))
                    .with_label(arg.span(), "unexpected `void` here")
                    .with_code("E308"),
                );
                return self.record(id, Ty::Error);
            }
            if !can_coerce(&got, want) {
                if got.readonly_view() == want.readonly_view()
                    && got.is_mutable_view() != want.is_mutable_view()
                {
                    self.diags.push(
                        Diagnostic::error(format!(
                            "cannot pass read-only `{got}` to mutable parameter `{}: {want}`",
                            sig.param_names[i]
                        ))
                        .with_label(arg.span(), "mutation authority is required here")
                        .with_note(format!("a read-only view cannot be upgraded to `{want}`"))
                        .with_code("E306"),
                    );
                } else {
                    self.diags.push(
                        Diagnostic::error(format!(
                            "`{display}` parameter `{}` expects `{want}`, got `{got}`",
                            sig.param_names[i]
                        ))
                        .with_label(arg.span(), format!("expected `{want}` here"))
                        .with_code("E306"),
                    );
                }
                return self.record(id, Ty::Error);
            }
        }
        // Monomorphization mirrors ordinary calls: concrete generic targets
        // outside generic bodies record instances; nested ones resolve per
        // outer instance in the worklist / world fixed point.
        if !sig.type_params.is_empty()
            && resolved.iter().all(|t| t.is_concrete())
            && self.type_env.is_empty()
        {
            match &target {
                Target::Local { def, display } => {
                    let mangled = mangle(display, &resolved);
                    self.typed.root_calls.insert(id.0, mangled.clone());
                    if !self.typed.instances.contains_key(&mangled) {
                        self.pending_instances.push((*def, resolved.clone()));
                    }
                }
                Target::Foreign { owner, sym, .. } => {
                    let canonical: Vec<Ty> = resolved
                        .iter()
                        .map(|t| canonicalize_for_key(t, &self.module, &self.typed.objects))
                        .collect();
                    let key = InstanceKey::new(
                        TemplateKey::new(owner.clone(), sym.name.clone()),
                        canonical,
                    );
                    if !self.typed.imported_root_calls.values().any(|k| k == &key)
                        && !self.pending_imported.iter().any(|k| k == &key)
                    {
                        self.pending_imported.push(key.clone());
                    }
                    self.typed.imported_root_calls.insert(id.0, key);
                }
            }
        }
        // LIR targets: locals by HIR `Fn` name, foreign by provider symbol.
        // Generic templates skip the direct target (no unmangled emission);
        // their concrete instances resolve through the maps above. Local
        // template defs and foreign symbols are always recorded so the
        // monomorphization worklist and the world fixed point see sugar
        // calls nested inside generic bodies (mirroring `Call.def` /
        // `Call.symbol` for ordinary calls).
        match target {
            Target::Local { def, display } => {
                self.typed.method_defs.insert(id.0, def);
                if sig.type_params.is_empty() {
                    self.typed
                        .method_targets
                        .insert(id.0, MethodTarget::Local(display));
                }
            }
            Target::Foreign { sym, .. } => {
                self.typed.method_symbols.insert(id.0, sym.clone());
                if sig.type_params.is_empty() {
                    self.typed
                        .method_targets
                        .insert(id.0, MethodTarget::Foreign(sym));
                }
            }
        }
        self.record(id, ret_ty)
    }

    /// Convert one explicit type argument with unbound-name reporting.
    /// (Declaration positions are parser-validated; turbofish arguments in
    /// expression position parse permissively and land here.) Union spellings
    /// normalize like annotations (`Option[u64]`, qualified `m.Option`).
    fn vl_to_ty_reported(&mut self, v: &VlType, span: Span) -> Ty {
        let ty = self.vl_to_ty(v, span);
        if ty_has_error(&ty) {
            // Conversion only fails on unknown names/`void`/arity (parser
            // errors surface as `None` annotations elsewhere); unbound
            // `Param`s report E105 here instead of leaking to an E500.
            // Recurse through containers so `Array[Missing]` reports E105
            // here instead of leaking downstream.
            if Self::vl_has_unbound_param(v) {
                self.diags.push(
                    Diagnostic::error(format!("unknown type `{v}`"))
                        .with_label(span, "no type parameter with this name is in scope")
                        .with_note("declare it on the function (`fun f[T]`) or use a concrete type")
                        .with_code("E105"),
                );
            }
        }
        ty
    }

    /// True when a type argument mentions an unbound `Param` at any depth
    /// (including through `Array[T]`, union arguments, and `*T`).
    fn vl_has_unbound_param(v: &VlType) -> bool {
        match v {
            VlType::Param(_) => true,
            VlType::Array(elem) => Self::vl_has_unbound_param(elem),
            VlType::Nullable(inner) => Self::vl_has_unbound_param(inner),
            VlType::Tuple(fields) => fields.iter().any(|f| Self::vl_has_unbound_param(&f.ty)),
            VlType::Union { args, .. } => args.iter().any(Self::vl_has_unbound_param),
            VlType::Mutable(inner) => Self::vl_has_unbound_param(inner),
            _ => false,
        }
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
                if !is_capability_valid(&t) {
                    // Explicit `*T`, `*u64`, `**Foo` never valid, even as
                    // generic arguments. Substitution could otherwise launder
                    // `*T` with `T = u64` into `*u64`.
                    validate_capability(&t, span, &mut self.diags);
                    return None;
                }
                if t == Ty::Void || t.is_void() {
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

    /// Declared payload of one union variant: index (tag order) plus arity.
    fn lookup_variant(&self, union: &str, variant: &str) -> Option<(UnionSigTy, usize)> {
        let sig = self.union_sig(union)?;
        let tag = sig.variants.iter().position(|v| v.name == variant)?;
        Some((sig, tag))
    }

    /// Instantiate a variant's payload formals under concrete union arguments
    /// (declaration parameters substituted). `None` when unknown (reported by
    /// the caller).
    fn variant_payload_tys(&self, sig: &UnionSigTy, tag: usize, args: &[Ty]) -> Option<Vec<Ty>> {
        let payload = &sig.variants.get(tag)?.payload;
        let env: HashMap<String, Ty> = sig
            .type_params
            .iter()
            .cloned()
            .zip(args.iter().cloned())
            .collect();
        Some(payload.iter().map(|t| subst_ty(t, &env)).collect())
    }

    /// Check `Union.Variant(args)` construction. `expected` (a `Union` or
    /// `*Union` from an annotation, parameter, or return position) supplies
    /// the union arguments when present and name-matching; an explicit
    /// turbofish wins over it; otherwise arguments infer from the payloads
    /// (missing constraints are one E303, like generic calls).
    #[allow(clippy::too_many_arguments)]
    fn check_variant(
        &mut self,
        id: vl_hir::HirId,
        union: &str,
        variant: &str,
        type_args: &[VlType],
        args: &[HirExpr],
        span: Span,
        expected: Option<&Ty>,
    ) -> Ty {
        let display = format!("{union}.{variant}");
        let Some((sig, tag)) = self.lookup_variant(union, variant) else {
            // Unreachable through the driver (semantic owns unknown unions
            // and variants with E201/E302 and records no site), but poison
            // loudly rather than silently for hand-built HIR.
            self.diags.push(
                Diagnostic::error(format!("cannot find union variant `{display}`"))
                    .with_label(span, "unknown union variant")
                    .with_code("E302"),
            );
            for arg in args {
                let _ = self.infer_expr(arg);
            }
            return self.record(id, Ty::Error);
        };
        // Infer payload argument types first (inner errors surface here, like
        // calls). Bare `Array.new(n)`, empty `[]`, and `null` defer to the
        // formal, exactly like call arguments.
        let mut arg_tys = Vec::with_capacity(args.len());
        let mut poisoned = false;
        for arg in args {
            if let HirExpr::Call {
                name: inner,
                type_args: inner_args,
                args: inner_call_args,
                ..
            } = arg
            {
                if inner == "Array.new" && inner_args.is_empty() {
                    for a in inner_call_args {
                        let _ = self.infer_expr(a);
                    }
                    arg_tys.push(Ty::Error);
                    continue;
                }
            }
            if let HirExpr::ArrayLiteral { elems, .. } = arg {
                if elems.is_empty() {
                    arg_tys.push(Ty::Error);
                    continue;
                }
            }
            // `null` carries no type to infer now; the per-payload expected
            // check below resolves it against the formal (or reports one
            // E303 when the formal is not nullable).
            if matches!(arg, HirExpr::Null { .. }) {
                arg_tys.push(Ty::Error);
                continue;
            }
            let t = self.infer_expr(arg);
            if ty_has_error(&t) {
                poisoned = true;
            }
            arg_tys.push(t);
        }
        let formals = sig.variants[tag].payload.clone();
        if args.len() != formals.len() {
            self.diags.push(
                Diagnostic::error(format!(
                    "variant `{display}` expects {} argument(s), got {}",
                    formals.len(),
                    args.len()
                ))
                .with_label(span, "wrong number of variant arguments")
                .with_code("E303"),
            );
            return self.record(id, Ty::Error);
        }
        if poisoned {
            return self.record(id, Ty::Error);
        }
        // Resolve the union arguments: explicit turbofish, then the expected
        // union (same name, arity-checked), then payload inference.
        let resolved: Option<Vec<Ty>> = if !type_args.is_empty() {
            if type_args.len() != sig.type_params.len() {
                self.diags.push(
                    Diagnostic::error(format!(
                        "`{display}` expects {} type argument(s), got {}",
                        sig.type_params.len(),
                        type_args.len()
                    ))
                    .with_label(span, "wrong number of type arguments")
                    .with_code("E303"),
                );
                return self.record(id, Ty::Error);
            }
            let mut out = Vec::with_capacity(type_args.len());
            for v in type_args {
                let t = self.vl_to_ty_reported(v, span);
                if ty_has_error(&t) {
                    return self.record(id, Ty::Error);
                }
                if !is_capability_valid(&t) {
                    validate_capability(&t, span, &mut self.diags);
                    return self.record(id, Ty::Error);
                }
                if t == Ty::Void || t.is_void() {
                    self.diags.push(
                        Diagnostic::error("type argument cannot be `void`")
                            .with_label(span, "`void` is not a value type")
                            .with_code("E308"),
                    );
                    return self.record(id, Ty::Error);
                }
                out.push(t);
            }
            Some(out)
        } else if let Some(want) = expected_union_args(expected, union, sig.type_params.len()) {
            Some(want)
        } else if args.is_empty() && type_args.is_empty() && !sig.type_params.is_empty() {
            // A nullary variant of a generic union (`Option.None`) carries
            // no constraints and takes no turbofish: the only source is an
            // annotation. Point there instead of the generic `::[...]` hint.
            self.diags.push(
                Diagnostic::error(format!(
                    "cannot infer type argument(s) `{}` for `{display}`",
                    sig.type_params.join(", ")
                ))
                .with_label(span, "annotate the binding with concrete arguments")
                .with_note(format!("write e.g. `val x: {union}[u64] = {display};`"))
                .with_code("E303"),
            );
            return self.record(id, Ty::Error);
        } else {
            // Inference: one synthetic signature over the declaration
            // parameters, solved exactly like a generic call.
            let decl_env: HashMap<String, Ty> = sig
                .type_params
                .iter()
                .map(|p| (p.clone(), Ty::Param(p.clone())))
                .collect();
            let formal_tys: Vec<Ty> = formals.iter().map(|t| subst_ty(t, &decl_env)).collect();
            let synthetic = FuncSigTy {
                param_names: (0..formal_tys.len())
                    .map(|i| format!("payload{i}"))
                    .collect(),
                param_tys: formal_tys,
                ret: Ty::Void,
                type_params: sig.type_params.clone(),
                bounds: HashMap::new(),
            };
            self.infer_type_args(&display, span, &synthetic, &arg_tys)
        };
        let Some(resolved) = resolved else {
            return self.record(id, Ty::Error);
        };
        let Some(wants) = self.variant_payload_tys(&sig, tag, &resolved) else {
            return self.record(id, Ty::Error);
        };
        for (i, (arg, original_got)) in args.iter().zip(arg_tys.iter()).enumerate() {
            let is_bare_new = matches!(arg, HirExpr::Call { name, type_args, .. } if name == "Array.new" && type_args.is_empty());
            let is_empty_array =
                matches!(arg, HirExpr::ArrayLiteral { elems, .. } if elems.is_empty());
            let is_null = matches!(arg, HirExpr::Null { .. });
            if ty_has_error(original_got) && !(is_bare_new || is_empty_array || is_null) {
                continue;
            }
            let want = &wants[i];
            if ty_has_error(want) {
                continue;
            }
            let got = self.infer_expr_expected(arg, want);
            if ty_has_error(&got) {
                continue;
            }
            if got == Ty::Void || *want == Ty::Void {
                self.diags.push(
                    Diagnostic::error(format!(
                        "variant `{display}` payload `{i}` cannot be `void`"
                    ))
                    .with_label(arg.span(), "unexpected `void` here")
                    .with_code("E308"),
                );
                return self.record(id, Ty::Error);
            }
            if !can_coerce(&got, want) {
                if got.readonly_view() == want.readonly_view()
                    && got.is_mutable_view() != want.is_mutable_view()
                {
                    self.diags.push(
                        Diagnostic::error(format!(
                            "cannot pass read-only `{got}` to mutable variant payload `{i}: {want}`"
                        ))
                        .with_label(arg.span(), "mutation authority is required here")
                        .with_note(format!("a read-only view cannot be upgraded to `{want}`"))
                        .with_code("E306"),
                    );
                } else {
                    self.diags.push(
                        Diagnostic::error(format!(
                            "variant `{display}` payload `{i}` expects `{want}`, got `{got}`"
                        ))
                        .with_label(arg.span(), format!("expected `{want}` here"))
                        .with_code("E306"),
                    );
                }
                return self.record(id, Ty::Error);
            }
        }
        self.record(
            id,
            Ty::Union(Box::new(UnionTy {
                name: union.to_string(),
                args: resolved,
            })),
        )
    }

    /// `Array.new::[T](count)`: one `u64` argument, returns `Array[T]`.
    /// A bare `Array.new(count)` only typechecks under an annotated binding
    /// (handled in [`Checker::binding_type`](Self::binding_type)); everywhere else
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
                        "write `Array.new::[T](count)`, or annotate the binding: `val a: Array[T] = Array.new(count)`",
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
    /// from a binding annotation). The count coerces integer literals to `u64`.
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
        if !validate_capability(&elem, span, &mut self.diags) {
            return self.record(id, Ty::Error);
        }
        if elem == Ty::Void || elem.is_void() {
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
        if !same_type(&ct, &Ty::U64) {
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

    /// Bare `null` without a `?T` expectation: no `T` to infer (one E303).
    /// A contextual `null` already recorded its nullable via
    /// `infer_expr_expected`; honor it. Out-of-line so the hot `infer_expr`
    /// frame stays small for deeply nested generics.
    fn check_bare_null(&mut self, id: vl_hir::HirId, span: Span) -> Ty {
        match self.typed.type_of_id(id) {
            Some(t) if ty_has_error(&t) => return self.record(id, Ty::Error),
            Some(t) if nullable_inner_ty(&t).is_some() => return self.record(id, t),
            _ => {}
        }
        self.diags.push(
            Diagnostic::error("cannot infer the type of `null`")
                .with_label(span, "null needs a type: `val x: ?T = null;`")
                .with_code("E303"),
        );
        self.record(id, Ty::Error)
    }

    /// `x == null` / `x != null` (one side is the `null` literal, handled
    /// before general operand inference so `null` records its nullable
    /// type). Always returns `Some` (either `Bool` or poisoned `Error`);
    /// out-of-line so the hot `infer_expr` frame stays small for deeply
    /// nested generics.
    fn check_null_comparison(
        &mut self,
        id: vl_hir::HirId,
        _op: vl_hir::HirBinOp,
        lhs: &vl_hir::HirExpr,
        rhs: &vl_hir::HirExpr,
        span: Span,
    ) -> Option<Ty> {
        let (value_side, null_side) = if matches!(lhs, vl_hir::HirExpr::Null { .. }) {
            (rhs, lhs)
        } else {
            (lhs, rhs)
        };
        if matches!(value_side, vl_hir::HirExpr::Null { .. }) {
            // `null == null` carries no `T`: one E303 from the left side,
            // the right poisons quietly (no cascade).
            let _ = self.infer_expr(lhs);
            self.record(rhs.id(), Ty::Error);
            return Some(self.record(id, Ty::Error));
        }
        let value_ty = self.infer_expr(value_side);
        if ty_has_error(&value_ty) {
            // Root cause already reported; poison the `null` quietly so a
            // bare-`null` E303 does not cascade beside it.
            self.record(null_side.id(), Ty::Error);
            return Some(self.record(id, Ty::Error));
        }
        if value_ty == Ty::Void {
            self.diags.push(
                Diagnostic::error("cannot use a `void` value in an operation")
                    .with_label(span, "`void` is not a value")
                    .with_code("E308"),
            );
            self.record(null_side.id(), Ty::Error);
            return Some(self.record(id, Ty::Error));
        }
        if nullable_inner_ty(&value_ty).is_none() {
            self.diags.push(
                Diagnostic::error(format!("cannot compare `{value_ty}` with `null`"))
                    .with_label(span, "only a nullable (`?T`) compares with `null`")
                    .with_code("E302"),
            );
            self.record(null_side.id(), Ty::Error);
            return Some(self.record(id, Ty::Error));
        }
        let _ = self.infer_expr_expected(null_side, &value_ty);
        Some(self.record(id, Ty::Bool))
    }

    /// Nullable prefix of `infer_expr_expected`: `null` and auto-`Some`.
    /// Returns `Some(ty)` when the nullable sugar handled `expr` fully
    /// (either the `null` literal or an implicit `T` -> `?T` wrap, or a
    /// poisoned re-visit); `None` to fall through to the general union,
    /// array, and literal paths below. Split out so the hot
    /// `infer_expr_expected` frame stays small for deeply nested generics.
    fn infer_nullable_prefix(&mut self, expr: &HirExpr, expected: &Ty) -> Option<Ty> {
        if ty_has_error(expected) {
            return None;
        }
        if let HirExpr::Null { id, .. } = expr {
            // `null` under a `?T` (or `*?T`) expectation: the empty nullable.
            if nullable_inner_ty(expected).is_some() {
                return Some(self.record(*id, expected.clone()));
            }
            // Bare `null` without a nullable expectation: fall through to
            // `infer_expr` for the single E303 below.
            return None;
        }
        if matches!(expr, HirExpr::Variant { .. }) {
            return None;
        }
        let inner = nullable_inner_ty(expected)?.clone();
        // Exact match first: a `?T` value where `?T` is expected needs no
        // wrap (avoids a spurious inner probe that would mismatch `?T`
        // vs `T`).
        let current = self.infer_expr(expr);
        if ty_has_error(&current) {
            return Some(self.record(expr.id(), Ty::Error));
        }
        if can_coerce(&current, expected) {
            return None;
        }
        let probe = self.infer_expr_expected(expr, &inner);
        if !ty_has_error(&probe) && !ty_has_error(&inner) && can_coerce(&probe, &inner) {
            // Keep the node's recorded type as the inner `T` (so literals
            // lower in the right lane); the wrap set tells LIR to emit
            // `Option.Some` around it.
            self.typed.nullable_wraps.insert(expr.id().0);
            return Some(expected.clone());
        }
        if ty_has_error(&probe) {
            return Some(self.record(expr.id(), Ty::Error));
        }
        // Inner rejects the value: fall through to the general mismatch
        // below (one error at the boundary).
        None
    }

    fn infer_expr_expected(&mut self, expr: &HirExpr, expected: &Ty) -> Ty {
        // Fast path guard: only enter the nullable helper when the
        // expectation could be a `?T` (`Option` union, possibly under `*`)
        // and the expression is not already a variant construction (which
        // has its own contextual path below). Keeps the hot generic-nesting
        // frames free of an extra call.
        let maybe_nullable = matches!(expected, Ty::Union(_) | Ty::Mutable(_))
            && !matches!(expr, HirExpr::Variant { .. });
        if maybe_nullable {
            if let Some(ty) = self.infer_nullable_prefix(expr, expected) {
                return ty;
            }
        }
        // Contextual variant construction: an expected `Union` (or `*Union`)
        // supplies the union arguments — a nullary `Option.None` under
        // `val x: Option[u64]`, or payload `int` literals.
        if let HirExpr::Variant {
            id,
            union,
            variant,
            type_args,
            args,
            span,
        } = expr
        {
            let union_expected = matches!(expected, Ty::Union { .. })
                || matches!(
                    expected,
                    Ty::Mutable(inner) if matches!(&**inner, Ty::Union { .. })
                );
            if union_expected && !ty_has_error(expected) {
                let got =
                    self.check_variant(*id, union, variant, type_args, args, *span, Some(expected));
                if !ty_has_error(&got)
                    && expected.is_mutable_view()
                    && same_type(&got, &expected.readonly_view())
                {
                    return self.record(*id, expected.clone());
                }
                return got;
            }
        }
        // Contextual bare `Array.new(n)`: `Array[T]` or `*Array[T]` expected
        // supplies the element type (returns/args as well as bindings).
        if let HirExpr::Call {
            name,
            type_args,
            args,
            span,
            id: call_id,
            ..
        } = expr
        {
            if name == "Array.new" && type_args.is_empty() {
                let elem_opt: Option<&Ty> = match expected {
                    Ty::Array(elem) => Some(elem),
                    Ty::Mutable(inner) => match &**inner {
                        Ty::Array(elem) => Some(elem),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(elem) = elem_opt {
                    if !ty_has_error(expected) && !ty_has_error(elem) {
                        let mut arg_tys = Vec::with_capacity(args.len());
                        let mut poisoned = false;
                        for arg in args {
                            let t = self.infer_expr(arg);
                            if ty_has_error(&t) {
                                poisoned = true;
                            }
                            arg_tys.push(t);
                        }
                        if !poisoned {
                            let arr = self.check_array_new_elem(
                                name,
                                *span,
                                (*elem).clone(),
                                args,
                                &arg_tys,
                                *call_id,
                            );
                            if !ty_has_error(&arr) && expected.is_mutable_view() {
                                return self.record(*call_id, expected.clone());
                            }
                            return arr;
                        }
                    }
                }
            }
        }
        // Contextual array elements: `Array[*Foo]` or `*Array[*Foo]` expected
        // infers each fresh element with its expected element type, so
        // `[Foo {}]` becomes `Array[*Foo]` instead of failing. Variables and
        // calls still never upgrade (fresh-only in the element check).
        if let HirExpr::ArrayLiteral { id, elems, .. } = expr {
            let expected_elem_opt: Option<&Ty> = match expected {
                Ty::Array(elem) => Some(elem),
                Ty::Mutable(inner) => match &**inner {
                    Ty::Array(elem) => Some(elem),
                    _ => None,
                },
                _ => None,
            };
            if let Some(expected_elem) = expected_elem_opt {
                if !ty_has_error(expected) && !ty_has_error(expected_elem) && !elems.is_empty() {
                    // Only take this path when every element can coerce to the
                    // expected element (otherwise fall through to the general
                    // literal mismatch, which reports one E302).
                    let mut elem_tys = Vec::with_capacity(elems.len());
                    let mut ok = true;
                    for e in elems {
                        let t = self.infer_expr_expected(e, expected_elem);
                        if ty_has_error(&t) {
                            ok = false;
                            break;
                        }
                        elem_tys.push(t);
                    }
                    if !ok {
                        // Element errors already reported; stay quiet.
                        return self.record(*id, Ty::Error);
                    }
                    {
                        // Merge element types (handles `int` + mixed `*`).
                        let mut acc = elem_tys[0].clone();
                        let mut conflict = false;
                        for t in &elem_tys[1..] {
                            match common_type(&acc, t) {
                                Some(c) => acc = c,
                                None => {
                                    conflict = true;
                                    break;
                                }
                            }
                        }
                        if !conflict && can_coerce(&acc, expected_elem) {
                            // Record elements already via recursion; record the
                            // literal itself as the expected shape when fresh
                            // (or as readonly Array when expected readonly).
                            if expected.is_mutable_view() {
                                // Fresh literal adopts mutable when element
                                // shapes match (checked via can_coerce above).
                                return self.record(*id, expected.clone());
                            } else {
                                return self.record(*id, Ty::Array(Box::new(acc)));
                            }
                        }
                    }
                    // Fall through to general handling on conflict (one E302).
                }
            }
        }
        // Contextual tuple elements: `#(T, U)` / `#(x: T)` expected
        // infers each fresh element with its expected element type, so
        // `#(1, "a")` against `#(u64, String)` coerces the `int` literal.
        if let HirExpr::TupleLiteral { id, elems, .. } = expr {
            let expected_elems_opt: Option<&Vec<(Option<String>, Ty)>> = match expected {
                Ty::Tuple(fields) => Some(fields),
                Ty::Mutable(inner) => match &**inner {
                    Ty::Tuple(fields) => Some(fields),
                    _ => None,
                },
                _ => None,
            };
            if let Some(expected_elems) = expected_elems_opt {
                if !ty_has_error(expected)
                    && expected_elems.len() == elems.len()
                    && !elems.is_empty()
                {
                    let mut ok = true;
                    let mut elem_tys = Vec::with_capacity(elems.len());
                    for ((_, value), (_, want)) in elems.iter().zip(expected_elems.iter()) {
                        if ty_has_error(want) {
                            ok = false;
                            break;
                        }
                        let t = self.infer_expr_expected(value, want);
                        if ty_has_error(&t) {
                            ok = false;
                            break;
                        }
                        elem_tys.push(t);
                    }
                    if !ok {
                        return self.record(*id, Ty::Error);
                    }
                    // Field names must match positionally as well as types:
                    // `#(a = 1u64)` against `#(x: u64, ...)` is a shape
                    // mismatch, not a silent rename. On conflict fall through
                    // to general handling (one error at the boundary).
                    let mut conflict = elems
                        .iter()
                        .zip(expected_elems.iter())
                        .any(|((got_name, _), (want_name, _))| got_name != want_name);
                    for ((name, _), (got, want)) in elems
                        .iter()
                        .zip(elem_tys.iter().zip(expected_elems.iter().map(|(_, t)| t)))
                    {
                        let _ = name;
                        if !can_coerce(got, want) {
                            conflict = true;
                            break;
                        }
                    }
                    if !conflict {
                        if expected.is_mutable_view() {
                            return self.record(*id, expected.clone());
                        } else {
                            let built: Vec<(Option<String>, Ty)> = elems
                                .iter()
                                .zip(elem_tys)
                                .map(|((name, _), ty)| (name.clone(), ty))
                                .collect();
                            return self.record(*id, Ty::Tuple(built));
                        }
                    }
                    // Fall through to general handling on conflict (one E309/E302 at the boundary).
                }
            }
        }
        self.coerce_expr_literals(expr, expected);
        let inferred = self.infer_expr(expr);
        // Fresh allocations adopt an expected mutable capability
        // (`Foo {}` with expected `*Foo` becomes `*Foo`). Existing
        // expressions never upgrade: variables, fields, index results, and
        // calls keep their capability.
        if !ty_has_error(&inferred)
            && !ty_has_error(expected)
            && expected.is_mutable_view()
            && is_fresh_allocation(expr)
            && same_type(&inferred, &expected.readonly_view())
        {
            return self.record(expr.id(), expected.clone());
        }
        inferred
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
                // Fresh literals adopt expected capability: `*Array[T]`
                // coerces elements to `T` like `Array[T]` does. The mutable
                // wrapper is restored by `infer_expr_expected` after inference.
                let elem_opt: Option<&Ty> = match expected {
                    Ty::Array(elem) => Some(elem),
                    Ty::Mutable(inner) => match &**inner {
                        Ty::Array(elem) => Some(elem),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(elem) = elem_opt {
                    for e in elems {
                        self.coerce_expr_literals(e, elem);
                    }
                    // Record the expected shape so contextual empty `[]`
                    // honors it (`*Array[T]` included). Non-empty literals
                    // re-infer below; the mutable wrapper is restored by the
                    // fresh-allocation upgrade when the shapes match.
                    self.record(*id, expected.clone());
                }
            }
            HirExpr::ObjectLiteral { id, fields, .. } => {
                // `*Foo` in a fresh context coerces fields like `Foo`.
                let obj_name: Option<&String> = match expected {
                    Ty::Object(name) => Some(name),
                    Ty::Mutable(inner) => match &**inner {
                        Ty::Object(name) => Some(name),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(name) = obj_name {
                    if let Some(sig) = self.typed.objects.get(name).cloned() {
                        for (field, value) in fields {
                            if let Some((_, field_ty)) = sig.fields.iter().find(|(n, _)| n == field)
                            {
                                self.coerce_expr_literals(value, field_ty);
                            }
                        }
                        if matches!(expected, Ty::Object(_)) {
                            self.record(*id, expected.clone());
                        }
                    }
                }
            }
            HirExpr::TupleLiteral { id, elems, .. } => {
                let expected_elems_opt: Option<&Vec<(Option<String>, Ty)>> = match expected {
                    Ty::Tuple(fields) => Some(fields),
                    Ty::Mutable(inner) => match &**inner {
                        Ty::Tuple(fields) => Some(fields),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(expected_elems) = expected_elems_opt {
                    // Only coerce when names agree positionally; a renamed
                    // shape falls through to general inference so the
                    // boundary reports one mismatch instead of renaming.
                    let names_match = elems
                        .iter()
                        .zip(expected_elems.iter())
                        .all(|((got_name, _), (want_name, _))| got_name == want_name);
                    if names_match && expected_elems.len() == elems.len() {
                        for ((_, value), (_, want)) in elems.iter().zip(expected_elems.iter()) {
                            self.coerce_expr_literals(value, want);
                        }
                        self.record(*id, expected.clone());
                    }
                }
            }
            HirExpr::Variant {
                id,
                union,
                variant,
                args,
                ..
            } => {
                // Coerce payload literals under an expected union of the same
                // name: instantiate the formals and recurse per argument,
                // then record the expected shape (re-inference rebuilds it).
                let want_union: Option<(&String, &Vec<Ty>)> = match expected {
                    Ty::Union(u) => Some((&u.name, &u.args)),
                    Ty::Mutable(inner) => match &**inner {
                        Ty::Union(u) => Some((&u.name, &u.args)),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some((want_name, want_args)) = want_union {
                    if want_name == union {
                        if let Some((sig, tag)) = self.lookup_variant(union, variant) {
                            if let Some(wants) = self.variant_payload_tys(&sig, tag, want_args) {
                                if wants.len() == args.len() {
                                    for (arg, want) in args.iter().zip(wants.iter()) {
                                        self.coerce_expr_literals(arg, want);
                                    }
                                    self.record(*id, expected.clone());
                                }
                            }
                        }
                    }
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
    /// Mutable views inspect their read-only base (though `*u64` itself is
    /// invalid and never reaches here valid).
    fn is_numeric_in_scope(&self, ty: &Ty) -> bool {
        match ty {
            Ty::Param(name) => self.type_bounds.get(name) == Some(&GenericBound::Numeric),
            Ty::Mutable(inner) => self.is_numeric_in_scope(inner),
            _ => is_numeric(ty),
        }
    }

    /// Comparable in scope: concrete comparables plus `Param` with `Numeric`
    /// (numbers compare) or `Comparable` bounds. Mutable views read as their
    /// base (`*String` compares as `String`).
    fn is_comparable_in_scope(&self, ty: &Ty) -> bool {
        match ty {
            Ty::Param(name) => matches!(
                self.type_bounds.get(name),
                Some(GenericBound::Numeric) | Some(GenericBound::Comparable)
            ),
            Ty::Mutable(inner) => self.is_comparable_in_scope(inner),
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
            HirExpr::Null { id, .. } => {
                // Out-of-line so this hot frame stays small for deeply
                // nested generics (debug builds keep all locals alive).
                self.check_bare_null(*id, expr.span())
            }
            HirExpr::ArrayLiteral { id, elems, .. } => {
                if elems.is_empty() {
                    // Contextual empty: coercion already recorded the
                    // annotation (`val e: Array[u64] = [];`, `*Array[T]`
                    // likewise); honor it. A repeat visit after an error
                    // stays quiet.
                    match self.typed.type_of_id(*id) {
                        Some(t) if ty_has_error(&t) => return self.record(*id, Ty::Error),
                        Some(t @ Ty::Array(_)) => return self.record(*id, t),
                        Some(t @ Ty::Mutable(_)) if t.array_elem().is_some() => {
                            return self.record(*id, t)
                        }
                        _ => {}
                    }
                    // No element to infer from: point at the typed
                    // constructor instead of guessing.
                    self.diags.push(
                        Diagnostic::error("cannot infer the element type of `[]`")
                            .with_label(
                                expr.span(),
                                "empty array literal needs a type: `val e: Array[T] = [];` or `Array.new::[T](n)`",
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
                    } else if poisoned {
                        // Already poisoned; stay quiet.
                    } else if let Some(common) = common_type(&first, &t) {
                        // `int` deferral and `*Foo`/`Foo` mixing both fold
                        // here; coerce literals to the common lane.
                        if first == Ty::Int && is_integer(&t) {
                            self.coerce_expr_literals(&elems[0], &t);
                        } else if t == Ty::Int && is_integer(&first) {
                            self.coerce_expr_literals(elem, &first);
                        }
                        first = common;
                    } else {
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
                // sites instead: binding/discarded-statement defaulting,
                // `infer_expr_expected` coercion, and per-argument coercion
                // after generic solving all coerce the elements then.
                // One bad element poisons the whole literal (single root
                // cause, no cascade).
                if poisoned {
                    return self.record(*id, Ty::Error);
                }
                self.record(*id, Ty::Array(Box::new(first)))
            }
            HirExpr::ObjectLiteral {
                id,
                name,
                fields,
                span,
            } => {
                if self.typed.unions.contains_key(name) {
                    self.diags.push(
                        Diagnostic::error(format!(
                            "cannot construct union `{name}` with an object literal"
                        ))
                        .with_label(*span, "unions construct through variants")
                        .with_note(format!(
                            "write `{name}.Variant(...)` for a payload variant or `{name}.Variant` for a nullary one"
                        ))
                        .with_code("E302"),
                    );
                    for (_, value) in fields {
                        self.infer_expr(value);
                    }
                    return self.record(*id, Ty::Error);
                }
                let Some(sig) = self.typed.objects.get(name).cloned() else {
                    self.diags.push(
                        Diagnostic::error(format!("cannot find object type `{name}`"))
                            .with_label(*span, "unknown object type")
                            .with_code("E302"),
                    );
                    for (_, value) in fields {
                        self.infer_expr(value);
                    }
                    return self.record(*id, Ty::Error);
                };
                let wrong_count = fields.len() != sig.fields.len();
                if wrong_count {
                    self.diags.push(
                        Diagnostic::error(format!(
                            "object `{name}` expects {} field(s), got {}",
                            sig.fields.len(),
                            fields.len()
                        ))
                        .with_label(*span, "wrong number of object fields")
                        .with_code("E302"),
                    );
                    for (_, value) in fields {
                        self.infer_expr(value);
                    }
                    return self.record(*id, Ty::Error);
                }
                let mut poisoned = false;
                let mut seen = HashSet::new();
                for (field, value) in fields {
                    if !seen.insert(field.clone()) {
                        self.diags.push(
                            Diagnostic::error(format!("duplicate object field `{field}`"))
                                .with_label(value.span(), "field repeated here")
                                .with_code("E302"),
                        );
                        poisoned = true;
                        continue;
                    }
                    let Some((_, want)) = sig.fields.iter().find(|(name, _)| name == field) else {
                        self.diags.push(
                            Diagnostic::error(format!("object `{name}` has no field `{field}`"))
                                .with_label(value.span(), "unknown object field")
                                .with_code("E302"),
                        );
                        self.infer_expr(value);
                        poisoned = true;
                        continue;
                    };
                    // Poisoned field declarations (E106 already reported) stay
                    // quiet here to keep one root cause.
                    if ty_has_error(want) {
                        self.infer_expr(value);
                        poisoned = true;
                        continue;
                    }
                    let got = self.infer_expr_expected(value, want);
                    if ty_has_error(&got) {
                        poisoned = true;
                    } else if !can_coerce(&got, want) {
                        if got.readonly_view() == want.readonly_view()
                            && got.is_mutable_view() != want.is_mutable_view()
                        {
                            self.diags.push(
                                Diagnostic::error(format!(
                                    "cannot initialize mutable field `{field}: {want}` with read-only `{got}`"
                                ))
                                .with_label(
                                    value.span(),
                                    "mutation authority is required here",
                                )
                                .with_note(format!(
                                    "a read-only view cannot be upgraded to `{want}`"
                                ))
                                .with_code("E302"),
                            );
                        } else {
                            self.diags.push(
                                Diagnostic::error(format!(
                                    "object field `{field}` expects `{want}`, got `{got}`"
                                ))
                                .with_label(value.span(), format!("expected `{want}` here"))
                                .with_code("E302"),
                            );
                        }
                        poisoned = true;
                    }
                }
                if poisoned {
                    self.record(*id, Ty::Error)
                } else {
                    self.record(*id, Ty::Object(name.clone()))
                }
            }
            HirExpr::Variant {
                id,
                union,
                variant,
                type_args,
                args,
                span,
            } => self.check_variant(*id, union, variant, type_args, args, *span, None),
            HirExpr::TupleLiteral { id, elems, span } => {
                if elems.len() < 2 {
                    self.diags.push(
                        Diagnostic::error("tuple literal needs at least two elements")
                            .with_label(*span, "write `#(a, b, ...)` with two or more values")
                            .with_code("E302"),
                    );
                    for (_, value) in elems {
                        self.infer_expr(value);
                    }
                    return self.record(*id, Ty::Error);
                }
                let named_count = elems.iter().filter(|(n, _)| n.is_some()).count();
                if named_count > 0 && named_count != elems.len() {
                    self.diags.push(
                        Diagnostic::error("cannot mix named and unnamed tuple elements")
                            .with_label(*span, "write all `#(a, b)` or all `#(x = a, y = b)`")
                            .with_code("E302"),
                    );
                    for (_, value) in elems {
                        self.infer_expr(value);
                    }
                    return self.record(*id, Ty::Error);
                }
                let mut out = Vec::with_capacity(elems.len());
                let mut poisoned = false;
                let mut seen = HashSet::new();
                for (name, value) in elems {
                    if let Some(name) = name {
                        if !seen.insert(name.clone()) {
                            self.diags.push(
                                Diagnostic::error(format!("duplicate tuple field `{name}`"))
                                    .with_label(value.span(), "field repeated here")
                                    .with_code("E302"),
                            );
                            poisoned = true;
                        }
                    }
                    let t = self.infer_expr(value);
                    if ty_has_error(&t) {
                        poisoned = true;
                    } else if t == Ty::Void || t.is_void() {
                        self.diags.push(
                            Diagnostic::error("a tuple element cannot be `void`")
                                .with_label(value.span(), "`void` is not a value type")
                                .with_code("E302"),
                        );
                        poisoned = true;
                    }
                    out.push((name.clone(), t));
                }
                if poisoned {
                    return self.record(*id, Ty::Error);
                }
                self.record(*id, Ty::Tuple(out))
            }
            HirExpr::TupleIndex {
                id,
                base,
                index,
                span,
            } => {
                let bt = self.infer_expr(base);
                if ty_has_error(&bt) {
                    return self.record(*id, Ty::Error);
                }
                let Some(elems) = bt.tuple_elems() else {
                    self.diags.push(
                        Diagnostic::error(format!("cannot index `{bt}` with ``.`{index}``"))
                            .with_label(*span, "only tuples support backtick indexing")
                            .with_code("E302"),
                    );
                    return self.record(*id, Ty::Error);
                };
                // Named tuples expose fields (`.name`), not positions.
                if elems.iter().any(|(n, _)| n.is_some()) {
                    self.diags.push(
                        Diagnostic::error(format!("tuple `{bt}` is named; use `.field` access"))
                            .with_label(
                                *span,
                                "unnamed ``.`i``` indexing is only for unnamed tuples",
                            )
                            .with_code("E302"),
                    );
                    return self.record(*id, Ty::Error);
                }
                let Some((_, elem)) = elems.get(*index) else {
                    self.diags.push(
                        Diagnostic::error(format!(
                            "tuple index ``.`{index}`` out of range for `{bt}`"
                        ))
                        .with_label(*span, format!("this tuple has {} element(s)", elems.len()))
                        .with_code("E302"),
                    );
                    return self.record(*id, Ty::Error);
                };
                self.record(*id, project_capability(&bt, elem))
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
                if !same_type(&it, &Ty::U64) {
                    self.diags.push(
                        Diagnostic::error(format!("array index must be `u64`, got `{it}`"))
                            .with_label(index.span(), "expected `u64` here")
                            .with_code("E302"),
                    );
                    return self.record(*id, Ty::Error);
                }
                // Reading works through either capability; reference elements
                // project transitively (`Array[*Foo][i]` -> `Foo`).
                self.record(*id, project_capability(&bt, &elem))
            }
            HirExpr::Field {
                id,
                base,
                name,
                span,
            } => {
                let bt = self.infer_expr(base);
                // Named-tuple field access (`u.x`) resolves positionally.
                if let Some(elems) = bt.tuple_elems() {
                    if ty_has_error(&bt) {
                        return self.record(*id, Ty::Error);
                    }
                    if elems.iter().all(|(n, _)| n.is_none()) {
                        self.diags.push(
                            Diagnostic::error(format!(
                                "tuple `{bt}` is unnamed; use backtick indexing"
                            ))
                            .with_label(*span, "write ``.`0``, ``.`1``, ... for unnamed tuples")
                            .with_code("E302"),
                        );
                        return self.record(*id, Ty::Error);
                    }
                    let Some((_, elem)) = elems
                        .iter()
                        .find(|(n, _)| n.as_deref() == Some(name.as_str()))
                    else {
                        self.diags.push(
                            Diagnostic::error(format!("tuple `{bt}` has no field `{name}`"))
                                .with_label(*span, "unknown tuple field")
                                .with_code("E302"),
                        );
                        return self.record(*id, Ty::Error);
                    };
                    return self.record(*id, project_capability(&bt, elem));
                }
                let Some(object_name) = object_base(&bt) else {
                    if !ty_has_error(&bt) {
                        self.diags.push(
                            Diagnostic::error(format!("cannot access field `{name}` on `{bt}`"))
                                .with_label(*span, "expected an object value here")
                                .with_code("E302"),
                        );
                    }
                    return self.record(*id, Ty::Error);
                };
                let Some(sig) = self.typed.objects.get(&object_name) else {
                    return self.record(*id, Ty::Error);
                };
                let Some((_, ty)) = sig.fields.iter().find(|(field, _)| field == name) else {
                    self.diags.push(
                        Diagnostic::error(format!("object `{object_name}` has no field `{name}`"))
                            .with_label(*span, "unknown object field")
                            .with_code("E302"),
                    );
                    return self.record(*id, Ty::Error);
                };
                // Transitive projection: `Parent.child` -> `Child`,
                // `*Parent.child` -> `*Child`.
                self.record(*id, project_capability(&bt, ty))
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
                extern_kind,
                symbol,
                name,
                type_args,
                args,
                span,
                ..
            } => {
                let mut arg_tys = Vec::with_capacity(args.len());
                let mut poisoned = false;
                for arg in args {
                    // Bare `Array.new(n)` defers its element-type error until
                    // the expected formal is known (per-argument contextual
                    // handling supplies it for `Array`/`*Array` formals).
                    // Infer its count argument for inner errors, then push a
                    // quiet `Error` here; the later expected check either
                    // succeeds contextually or reports one E303.
                    if let HirExpr::Call {
                        name: inner,
                        type_args: inner_args,
                        args: inner_call_args,
                        ..
                    } = arg
                    {
                        if inner == "Array.new" && inner_args.is_empty() {
                            for a in inner_call_args {
                                let _ = self.infer_expr(a);
                            }
                            arg_tys.push(Ty::Error);
                            continue;
                        }
                    }
                    // Empty `[]` likewise defers to the expected formal:
                    // inferring now would emit E302 before the explicit
                    // `::[T]` (or a concrete `Array[T]` formal) is known.
                    // Push a quiet `Error`; the later expected check either
                    // adopts the formal contextually or reports one error.
                    if let HirExpr::ArrayLiteral { elems, .. } = arg {
                        if elems.is_empty() {
                            arg_tys.push(Ty::Error);
                            continue;
                        }
                    }
                    // `null` likewise defers: only the formal tells whether
                    // it is the empty `?T` (contextual) or one E303.
                    if matches!(arg, HirExpr::Null { .. }) {
                        arg_tys.push(Ty::Error);
                        continue;
                    }
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
                    let is_source = matches!(extern_kind, Some(vl_common::ExportKind::Source));
                    if is_source {
                        // Imported source function (monomorphic or generic):
                        // same signature algorithm as local calls.
                        let Some(shared) = extern_sig else {
                            // Poisoned import (E202/E203/E208 already reported).
                            return self.record(*id, Ty::Error);
                        };
                        let sig = FuncSigTy::from_shared(shared);
                        if ty_has_error(&sig.ret) || sig.param_tys.iter().any(ty_has_error) {
                            return self.record(*id, Ty::Error);
                        }
                        let Some(resolved) =
                            self.resolve_type_args(name, *span, &sig, type_args, &arg_tys)
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
                            let is_bare_new = matches!(&args[i], HirExpr::Call { name, type_args, .. } if name == "Array.new" && type_args.is_empty());
                            let is_empty_array = matches!(&args[i], HirExpr::ArrayLiteral { elems, .. } if elems.is_empty());
                            let is_null = matches!(&args[i], HirExpr::Null { .. });
                            if ty_has_error(original_got)
                                && !(is_bare_new || is_empty_array || is_null)
                            {
                                continue;
                            }
                            let want = &param_tys[i];
                            if ty_has_error(want) {
                                continue;
                            }
                            let got = self.infer_expr_expected(&args[i], want);
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
                            if !can_coerce(&got, want) {
                                if got.readonly_view() == want.readonly_view()
                                    && got.is_mutable_view() != want.is_mutable_view()
                                {
                                    self.diags.push(
                                        Diagnostic::error(format!(
                                            "cannot pass read-only `{got}` to mutable parameter `{}: {want}`",
                                            sig.param_names[i]
                                        ))
                                        .with_label(
                                            args[i].span(),
                                            "mutation authority is required here",
                                        )
                                        .with_note(format!(
                                            "a read-only view cannot be upgraded to `{want}`"
                                        ))
                                        .with_code("E306"),
                                    );
                                } else {
                                    self.diags.push(
                                        Diagnostic::error(format!(
                                            "`{name}` parameter `{}` expects `{}`, got `{got}`",
                                            sig.param_names[i], want
                                        ))
                                        .with_label(
                                            args[i].span(),
                                            format!("expected `{want}` here"),
                                        )
                                        .with_code("E306"),
                                    );
                                }
                                return self.record(*id, Ty::Error);
                            }
                        }
                        // Concrete imported source calls outside a generic body
                        // request an owner-module instance. Calls inside a
                        // generic body resolve per outer instance in the world
                        // fixed point (via `symbol` template identity).
                        if self.type_env.is_empty() && resolved.iter().all(|t| t.is_concrete()) {
                            if let Some(sym) = symbol {
                                // Only generic source calls need cross-module
                                // instantiation; monomorphic source calls use
                                // the existing extern import path.
                                if !sig.type_params.is_empty() {
                                    // Canonicalize nominal object identity for
                                    // cross-module dedup (`Person` in the
                                    // caller becomes `<caller>.Person`).
                                    let canonical: Vec<Ty> = resolved
                                        .iter()
                                        .map(|t| {
                                            canonicalize_for_key(
                                                t,
                                                &self.module,
                                                &self.typed.objects,
                                            )
                                        })
                                        .collect();
                                    let key = InstanceKey::new(
                                        TemplateKey::new(sym.module.as_string(), sym.name.clone()),
                                        canonical,
                                    );
                                    // Deduplicate requests; budget is enforced
                                    // by the world fixed point.
                                    if !self.typed.imported_root_calls.values().any(|k| k == &key)
                                        && !self.pending_imported.iter().any(|k| k == &key)
                                    {
                                        self.pending_imported.push(key.clone());
                                    }
                                    self.typed.imported_root_calls.insert(id.0, key);
                                }
                            } else if !sig.type_params.is_empty() {
                                // Defensive: generic source import without a
                                // symbol should not happen (resolver always
                                // attaches one); stay quiet rather than
                                // emitting an unresolved call.
                            }
                        }
                        return self.record(*id, ret_ty);
                    }
                    // Target-native exports are never generic:
                    // `print::[u64]` is an error.
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
                    // Bare `Array.new` and empty `[]` defer to expected-formal
                    // contextual handling below; other poisoned args stay quiet.
                    let is_bare_new = matches!(&args[i], HirExpr::Call { name, type_args, .. } if name == "Array.new" && type_args.is_empty());
                    let is_empty_array =
                        matches!(&args[i], HirExpr::ArrayLiteral { elems, .. } if elems.is_empty());
                    let is_null = matches!(&args[i], HirExpr::Null { .. });
                    if ty_has_error(original_got) && !(is_bare_new || is_empty_array || is_null) {
                        continue;
                    }
                    let want = &param_tys[i];
                    if ty_has_error(want) {
                        continue;
                    }
                    let got = self.infer_expr_expected(&args[i], want);
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
                    if !can_coerce(&got, want) {
                        if got.readonly_view() == want.readonly_view()
                            && got.is_mutable_view() != want.is_mutable_view()
                        {
                            self.diags.push(
                                Diagnostic::error(format!(
                                    "cannot pass read-only `{got}` to mutable parameter `{}: {want}`",
                                    sig.param_names[i]
                                ))
                                .with_label(
                                    args[i].span(),
                                    "mutation authority is required here",
                                )
                                .with_note(format!(
                                    "a read-only view cannot be upgraded to `{want}`"
                                ))
                                .with_code("E306"),
                            );
                        } else {
                            self.diags.push(
                                Diagnostic::error(format!(
                                    "`{name}` parameter `{}` expects `{}`, got `{got}`",
                                    sig.param_names[i], want
                                ))
                                .with_label(args[i].span(), format!("expected `{want}` here"))
                                .with_code("E306"),
                            );
                        }
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
            HirExpr::MethodCall {
                id,
                receiver,
                method,
                method_span,
                type_args,
                args,
                span,
                ..
            } => self.check_method_call(MethodCallParts {
                id: *id,
                receiver,
                method,
                method_span: *method_span,
                type_args,
                args,
                span: *span,
            }),
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
                // `x == null` helper lives out-of-line so this hot frame
                // stays small for deeply nested generics (debug builds keep
                // all locals alive; `Ty` values are large).
                if matches!(op, HirBinOp::Eq | HirBinOp::Ne)
                    && (matches!(&**lhs, HirExpr::Null { .. })
                        || matches!(&**rhs, HirExpr::Null { .. }))
                {
                    if let Some(ty) = self.check_null_comparison(*id, *op, lhs, rhs, *span) {
                        return ty;
                    }
                    // `None` means already poisoned (diagnostic emitted);
                    // fall through to avoid a second error? The helper
                    // always returns `Some` (either Bool or Error), so this
                    // is unreachable; keep the general path as a fallback.
                }
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
                        // `x == null` / `x != null` returned early above
                        // (before operand inference) so `null` records its
                        // nullable type; reaching here means neither side is
                        // `null`.
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
                        // Read-only comparison: mutable views read as their
                        // base (`*String` compares as `String`).
                        let comparable = same_type(&lt, &rt)
                            || same_type(&lt.readonly_view(), &rt.readonly_view());
                        if !comparable || !self.is_comparable_in_scope(&lt) {
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

    /// Explicit numeric conversion (`value as u8`).
    ///
    /// Semantics (v0, integers only):
    /// - Target must be `u64`, `i64`, or `u8` (float/String/array casts are
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
                    .with_note("float, String, and array conversions need target conversions not yet specified")
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
        Ty::Union(u) => u.args.iter().any(ty_has_error),
        Ty::Tuple(fields) => fields.iter().any(|(_, ty)| ty_has_error(ty)),
        Ty::Mutable(inner) => ty_has_error(inner),
        _ => false,
    }
}

/// Does a type still hold an unresolved `int` literal (top level or nested
/// in `Array`/`Tuple`/`Union`)? Such types must be defaulted (`u64` lane) before lowering.
fn ty_contains_int(ty: &Ty) -> bool {
    match ty {
        Ty::Int => true,
        Ty::Array(elem) => ty_contains_int(elem),
        Ty::Union(u) => u.args.iter().any(ty_contains_int),
        Ty::Tuple(fields) => fields.iter().any(|(_, ty)| ty_contains_int(ty)),
        Ty::Mutable(inner) => ty_contains_int(inner),
        _ => false,
    }
}

/// Structurally unify two solved constraint types, letting `int` defer to a
/// concrete integer lane at any depth (`Array[Int]` + `Array[u8]` gives
/// `Array[u8]`), and picking the safe read-only side when mixing `*R` and
/// `R` (`*Foo` + `Foo` gives `Foo`). Returns `None` on genuine conflict.
pub(crate) fn unify_solved(a: &Ty, b: &Ty) -> Option<Ty> {
    // Shared with array literals: safe common type covers `int` deferral,
    // capability downgrade, and structural recursion.
    common_type(a, b)
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
        Ty::Union(u) => Ty::Union(Box::new(UnionTy {
            name: u.name.clone(),
            args: u.args.into_iter().map(default_inferred_ty).collect(),
        })),
        Ty::Tuple(fields) => Ty::Tuple(
            fields
                .into_iter()
                .map(|(n, t)| (n, default_inferred_ty(t)))
                .collect(),
        ),
        Ty::Mutable(inner) => Ty::Mutable(Box::new(default_inferred_ty(*inner))),
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
    /// later per-argument coercion pass handles it; a mutable actual against
    /// a read-only formal downgrades likewise). Reports one E306 on
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
            (Ty::Union(fu), Ty::Union(au))
                if fu.name == au.name && fu.args.len() == au.args.len() =>
            {
                let mut ok = true;
                for (f, a) in fu.args.iter().zip(au.args.iter()) {
                    if !self.collect(f, a, name, span, diags) {
                        ok = false;
                        break;
                    }
                }
                ok
            }
            (Ty::Tuple(fs), Ty::Tuple(as_)) => {
                if fs.len() != as_.len() {
                    diags.push(
                        Diagnostic::error(format!("`{name}` expects `{formal}`, got `{actual}`"))
                            .with_label(span, format!("expected `{formal}` here"))
                            .with_code("E306"),
                    );
                    return false;
                }
                for ((nf, tf), (na, ta)) in fs.iter().zip(as_.iter()) {
                    if nf != na {
                        diags.push(
                            Diagnostic::error(format!(
                                "`{name}` expects `{formal}`, got `{actual}`"
                            ))
                            .with_label(span, format!("expected `{formal}` here"))
                            .with_code("E306"),
                        );
                        return false;
                    }
                    if !self.collect(tf, ta, name, span, diags) {
                        return false;
                    }
                }
                true
            }
            (Ty::Mutable(f), Ty::Mutable(a)) => self.collect(f, a, name, span, diags),
            // Read-only array formal accepts a mutable array actual via
            // downgrade for inference (`Array[T]` with `*Array[u64]` infers
            // `T = u64`); the later per-argument check enforces downgrade.
            (Ty::Array(_), Ty::Mutable(inner)) => match &**inner {
                Ty::Array(_) => self.collect(formal, inner, name, span, diags),
                _ => {
                    diags.push(
                        Diagnostic::error(format!("`{name}` expects `{formal}`, got `{actual}`"))
                            .with_label(span, format!("expected `{formal}` here"))
                            .with_code("E306"),
                    );
                    false
                }
            },
            // Mutable formal infers from a readonly actual via its readonly
            // view (`*Array[T]` with `[1u64]` infers `T = u64`); the later
            // per-argument fresh upgrade makes the literal mutable.
            (Ty::Mutable(f), _) => self.collect(f, actual, name, span, diags),
            (f, a) if f == a => true,
            // An untyped literal against a concrete integer lane coerces
            // later; it is not an inference conflict.
            (f, Ty::Int) if is_integer(f) => true,
            // Read-only formal accepts a mutable actual via downgrade
            // (`Foo` accepts `*Foo`); the later per-argument check enforces it.
            (f, a) if can_coerce(a, f) => true,
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
        | HirStmt::FieldAssign { .. }
        | HirStmt::TupleAssign { .. }
        | HirStmt::Destructure { .. }
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
        HirStmt::Match {
            arms, else_body, ..
        } => {
            // Like `if`, but arms stand in for branches: every arm plus
            // `else` (when present) must agree. A missing `else` with full
            // coverage behaves the same way (exhaustiveness is enforced by
            // checking; when it fails, that E309 is the single root cause
            // and this must not add a spurious missing-return error).
            let mut flows: Vec<Flow> = Vec::with_capacity(arms.len() + 1);
            for arm in arms {
                flows.push(block_flow(&arm.body));
            }
            if let Some(else_body) = else_body {
                flows.push(block_flow(else_body));
            }
            if flows.is_empty() {
                return Flow::FallsThrough;
            }
            let first = flows[0];
            if flows.iter().all(|f| *f == first) {
                return first;
            }
            if flows.contains(&Flow::FallsThrough) {
                return Flow::FallsThrough;
            }
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
            HirStmt::Match {
                arms, else_body, ..
            } => {
                for arm in arms {
                    check_unreachable(&arm.body, diags);
                }
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
        | HirStmt::FieldAssign { span, .. }
        | HirStmt::TupleAssign { span, .. }
        | HirStmt::Destructure { span, .. }
        | HirStmt::If { span, .. }
        | HirStmt::Match { span, .. }
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
        if let HirStmt::Match {
            arms,
            else_body,
            span,
            ..
        } = stmt
        {
            // Any arm/else disagreeing on `return` vs fallthrough lets
            // execution fall through on some path: anchor there.
            let mut returns = else_body
                .as_ref()
                .map(|body| block_flow(body) == Flow::Returns)
                .unwrap_or(false);
            let mut falls = else_body.is_none();
            for arm in arms {
                if block_flow(&arm.body) == Flow::Returns {
                    returns = true;
                } else {
                    falls = true;
                }
            }
            if returns && falls {
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
///
/// Kept for symmetric operand checks (arithmetic, index). Boundary checks
/// (initializer, assignment, argument, return, field, element) use
/// [`can_coerce`] so `*R -> R` downgrades while `R -> *R` fails.
fn types_compatible(got: &Ty, want: &Ty) -> bool {
    same_type(got, want)
}

/// Invariant/equality check: exact structural equality, including capability.
/// `Foo` vs `*Foo` are different; `*Foo` vs `*Foo` are the same.
fn same_type(a: &Ty, b: &Ty) -> bool {
    a == b
}

/// Directional capability coercion for typed boundaries (initializer,
/// assignment, argument, return, object-field, array-element).
/// Allows discarding mutation authority (`*R -> R` for the same reference
/// shape) and forbids inventing it (`R -> *R`). Nested container arguments
/// stay invariant: `Array[*Foo]` does not coerce to `Array[Foo]` as a whole
/// (element boundaries coerce per element, and reads project).
pub(crate) fn can_coerce(got: &Ty, want: &Ty) -> bool {
    if same_type(got, want) {
        return true;
    }
    // One implicit downgrade: `*R` supplies a read-only `R`.
    if let Ty::Mutable(inner) = got {
        return same_type(inner, want);
    }
    false
}

/// Safe common type for array literals and generic constraint merging.
/// Picks the read-only side when mixing `*R` and `R`, defers `int` to a
/// concrete integer lane, and recurses through `Array` (combining both, so
/// `*Array[u64]` + `Array[int]` gives `Array[u64]`). Returns `None` on
/// genuine conflict.
fn common_type(a: &Ty, b: &Ty) -> Option<Ty> {
    if same_type(a, b) {
        return Some(a.clone());
    }
    if *a == Ty::Int && is_integer(b) {
        return Some(b.clone());
    }
    if *b == Ty::Int && is_integer(a) {
        return Some(a.clone());
    }
    // Mixed capability at the top level: the safe common type is read-only.
    // `*Foo` + `Foo` -> `Foo`; `*Array[T]` + `Array[T]` -> `Array[T]`.
    if let Ty::Mutable(ai) = a {
        if same_type(ai, b) {
            return Some(b.clone());
        }
        // Combined downgrade + structural (e.g. `*Array[u64]` vs
        // `Array[int]`): try the readonly view first.
        if let Some(c) = common_type(ai, b) {
            // Only accept when the result is readonly-safe (no invented
            // authority): `c` must be coercible to itself and not introduce
            // a new `*` beyond `b`'s shape. Since `ai` is readonly, `c`
            // derived from it is safe.
            return Some(c);
        }
    }
    if let Ty::Mutable(bi) = b {
        if same_type(a, bi) {
            return Some(a.clone());
        }
        if let Some(c) = common_type(a, bi) {
            return Some(c);
        }
    }
    match (a, b) {
        // Nested container arguments stay invariant: no capability downgrade
        // inside `Array` (only top-level `*R`/`R` mixing above, plus `int`).
        // `Array[*Foo]` vs `Array[Foo]` therefore has no common type.
        (Ty::Array(x), Ty::Array(y)) => invariant_common(x, y).map(|e| Ty::Array(Box::new(e))),
        (Ty::Mutable(x), Ty::Mutable(y)) => common_type(x, y).map(|e| Ty::Mutable(Box::new(e))),
        // Same-named unions merge argument-wise (arity already enforced by
        // checking; a length mismatch here is a genuine conflict).
        (Ty::Union(au), Ty::Union(bu)) if au.name == bu.name && au.args.len() == bu.args.len() => {
            let mut out = Vec::with_capacity(au.args.len());
            for (x, y) in au.args.iter().zip(bu.args.iter()) {
                out.push(common_type(x, y)?);
            }
            Some(Ty::Union(Box::new(UnionTy {
                name: au.name.clone(),
                args: out,
            })))
        }
        (Ty::Tuple(x), Ty::Tuple(y)) if x.len() == y.len() => {
            let mut out = Vec::with_capacity(x.len());
            for ((nx, tx), (ny, ty)) in x.iter().zip(y.iter()) {
                if nx != ny {
                    return None;
                }
                out.push((nx.clone(), common_type(tx, ty)?));
            }
            Some(Ty::Tuple(out))
        }
        _ => None,
    }
}

/// Capability-invariant common type for nested positions: exact, `int`
/// deferral, and structural `Array` recursion only (no `*R`/`R` downgrade).
fn invariant_common(a: &Ty, b: &Ty) -> Option<Ty> {
    if same_type(a, b) {
        return Some(a.clone());
    }
    if *a == Ty::Int && is_integer(b) {
        return Some(b.clone());
    }
    if *b == Ty::Int && is_integer(a) {
        return Some(a.clone());
    }
    match (a, b) {
        (Ty::Array(x), Ty::Array(y)) => invariant_common(x, y).map(|e| Ty::Array(Box::new(e))),
        (Ty::Union(au), Ty::Union(bu)) if au.name == bu.name && au.args.len() == bu.args.len() => {
            let mut out = Vec::with_capacity(au.args.len());
            for (x, y) in au.args.iter().zip(bu.args.iter()) {
                out.push(invariant_common(x, y)?);
            }
            Some(Ty::Union(Box::new(UnionTy {
                name: au.name.clone(),
                args: out,
            })))
        }
        (Ty::Tuple(x), Ty::Tuple(y)) if x.len() == y.len() => {
            let mut out = Vec::with_capacity(x.len());
            for ((nx, tx), (ny, ty)) in x.iter().zip(y.iter()) {
                if nx != ny {
                    return None;
                }
                out.push((nx.clone(), invariant_common(tx, ty)?));
            }
            Some(Ty::Tuple(out))
        }
        _ => None,
    }
}

/// Transitive read-only projection for field and element reads.
/// A mutable receiver preserves the declared member capability;
/// a read-only receiver downgrades a mutable member (`*Child` -> `Child`).
fn project_capability(receiver: &Ty, member: &Ty) -> Ty {
    if receiver.is_mutable_view() {
        member.clone()
    } else {
        member.readonly_view()
    }
}

/// Object name through an optional outer `*` (`Foo` or `*Foo` -> `Foo`).
fn object_base(ty: &Ty) -> Option<String> {
    match ty {
        Ty::Object(name) => Some(name.clone()),
        Ty::Mutable(inner) => match &**inner {
            Ty::Object(name) => Some(name.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Nominal object identity tolerating bare/qualified spelling: `Counter` in
/// `my.mod` is `my.mod.Counter`, and a qualified name matches its own short
/// tail. Used by the instance-sugar self gate.
fn same_object_name(a: &str, b: &str, current_module: &str) -> bool {
    if a == b {
        return true;
    }
    let qualified = |n: &str| {
        if n.contains('.') {
            n.to_string()
        } else {
            format!("{current_module}.{n}")
        }
    };
    qualified(a) == qualified(b)
}

/// Compiler-known fresh allocations whose initial capability may be selected
/// by an expected type: object literals, array literals (including `[]`),
/// `Array.new` constructions, variant constructions, and string literals.
/// Variables, fields, index results, and calls never upgrade from a
/// read-only view.
fn is_fresh_allocation(expr: &vl_hir::HirExpr) -> bool {
    match expr {
        vl_hir::HirExpr::ObjectLiteral { .. } => true,
        vl_hir::HirExpr::Variant { .. } => true,
        vl_hir::HirExpr::Null { .. } => true,
        vl_hir::HirExpr::TupleLiteral { .. } => true,
        vl_hir::HirExpr::ArrayLiteral { .. } => true,
        vl_hir::HirExpr::Call { name, .. } if name == "Array.new" => true,
        vl_hir::HirExpr::String { .. } => true,
        _ => false,
    }
}

/// Validate a declared type's capability placement (E106). Rejects `*` over
/// scalars/`void`, nested `**`, and `*T` over an unconstrained parameter.
/// `Array` contents recurse. Returns true when valid; on failure reports one
/// diagnostic and the caller poisons.
fn validate_capability(ty: &Ty, span: Span, diags: &mut Vec<Diagnostic>) -> bool {
    match ty {
        Ty::Mutable(inner) => {
            // Nested `**T` never valid.
            if inner.is_mutable_view() {
                diags.push(
                    Diagnostic::error(format!("repeated capability qualifier `*{inner}`"))
                        .with_label(span, "only one `*` is allowed here")
                        .with_code("E106"),
                );
                return false;
            }
            match &**inner {
                Ty::Param(name) => {
                    diags.push(
                        Diagnostic::error(format!("`*{name}` needs a reference-kind bound"))
                            .with_label(span, "unconstrained `T` cannot grant mutation authority")
                            .with_note("use `T` itself and supply `*Foo` as the argument")
                            .with_code("E106"),
                    );
                    return false;
                }
                Ty::Void => {
                    diags.push(
                        Diagnostic::error("`*void` is not a valid type")
                            .with_label(span, "`void` is not a value type")
                            .with_code("E106"),
                    );
                    return false;
                }
                Ty::U64 | Ty::I64 | Ty::F64 | Ty::Bool | Ty::U8 | Ty::Int => {
                    diags.push(
                        Diagnostic::error(format!("`*{inner}` is not a reference type"))
                            .with_label(span, "only reference types take `*`")
                            .with_note("write `*Foo`, `*String`, `*File`, or `*Array[T]`")
                            .with_code("E106"),
                    );
                    return false;
                }
                Ty::String
                | Ty::File
                | Ty::Object(_)
                | Ty::Union { .. }
                | Ty::Array(_)
                | Ty::Tuple(_)
                | Ty::Error => {
                    // Payload may still be malformed (`*Array[*u64]`).
                    return validate_capability(inner, span, diags);
                }
                Ty::Mutable(_) => {
                    diags.push(
                        Diagnostic::error(format!("repeated capability qualifier `*{inner}`"))
                            .with_label(span, "only one `*` is allowed here")
                            .with_code("E106"),
                    );
                    return false;
                }
            }
        }
        Ty::Array(elem) => return validate_capability(elem, span, diags),
        Ty::Union(u) => {
            for arg in &u.args {
                if !validate_capability(arg, span, diags) {
                    return false;
                }
            }
            return true;
        }
        Ty::Tuple(fields) => {
            for (_, ty) in fields {
                if !validate_capability(ty, span, diags) {
                    return false;
                }
            }
            return true;
        }
        _ => {}
    }
    true
}

/// Quiet validity check for capability placement (no diagnostics).
/// False for `*` over scalars/`void`, nested `**`, `*T`, and any `Array`
/// containing such.
pub(crate) fn is_capability_valid(ty: &Ty) -> bool {
    match ty {
        Ty::Mutable(inner) => {
            if inner.is_mutable_view() {
                return false;
            }
            match &**inner {
                Ty::Param(_)
                | Ty::Void
                | Ty::U64
                | Ty::I64
                | Ty::F64
                | Ty::Bool
                | Ty::U8
                | Ty::Int
                | Ty::Mutable(_) => return false,
                Ty::String | Ty::File | Ty::Object(_) | Ty::Error => {
                    return is_capability_valid(inner);
                }
                Ty::Array(_) | Ty::Tuple(_) | Ty::Union { .. } => {
                    return is_capability_valid(inner);
                }
            }
        }
        Ty::Array(elem) => return is_capability_valid(elem),
        Ty::Tuple(fields) => return fields.iter().all(|(_, ty)| is_capability_valid(ty)),
        Ty::Union(u) => return u.args.iter().all(is_capability_valid),
        _ => {}
    }
    true
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
/// `Comparable` covers those plus `bool` and `String` (equality).
/// Mutable views read as their base (`*String` satisfies `Comparable` as
/// `String`; `*Foo` does not become comparable).
pub(crate) fn bound_satisfied(bound: GenericBound, ty: &Ty) -> bool {
    match ty {
        Ty::Mutable(inner) => bound_satisfied(bound, inner),
        _ => match bound {
            GenericBound::Numeric => matches!(ty, Ty::Int | Ty::U64 | Ty::I64 | Ty::F64 | Ty::U8),
            GenericBound::Comparable => matches!(
                ty,
                Ty::Int | Ty::U64 | Ty::I64 | Ty::F64 | Ty::Bool | Ty::U8 | Ty::String
            ),
        },
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
        Ty::Array(_) | Ty::Tuple(_) => false,
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

    fn check_src_module(src: &str, module: &str) -> (TypedProgram, Vec<Diagnostic>) {
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse_with_module(&toks, src, module);
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        check(&hir)
    }

    #[test]
    fn union_payload_qualified_names_and_capabilities_are_checked_once() {
        let (_, unknown) = check_src("type U = union { A(no.such.Type), };");
        assert_eq!(
            unknown.iter().filter(|d| d.is_error()).count(),
            1,
            "{unknown:?}"
        );
        assert_eq!(unknown[0].code.as_deref(), Some("E302"), "{unknown:?}");

        let (_, valid_forward) = check_src_module(
            "type U = union { A(demo.V), }; type V = object { value: u64, };",
            "demo",
        );
        assert!(valid_forward.is_empty(), "{valid_forward:?}");

        let (_, valid_self) = check_src_module("type U = union { A(demo.U), };", "demo");
        assert!(valid_self.is_empty(), "{valid_self:?}");

        let (toks, _) = vl_lex::lex("type U = union { A(demo.lib.ForeignU), };");
        let (prog, _) = vl_syntax::parse_with_module(&toks, "", "demo.main");
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        let mut foreign = vl_common::ModuleSpec::new(&["demo", "lib"], &[]);
        foreign.unions.push(vl_common::UnionExport {
            name: "ForeignU".into(),
            qualified: "demo.lib.ForeignU".into(),
            type_params: Vec::new(),
            variants: vec![vl_common::UnionVariantSig {
                name: "A".into(),
                payload: Vec::new(),
            }],
        });
        let (_, valid_foreign) = check_with_modules(&hir, &[foreign]);
        assert!(valid_foreign.is_empty(), "{valid_foreign:?}");

        let (_, invalid_mutable) = check_src("type U[T] = union { A(*T), };");
        assert_eq!(invalid_mutable.iter().filter(|d| d.is_error()).count(), 1);
        assert_eq!(invalid_mutable[0].code.as_deref(), Some("E106"));
    }

    #[test]
    fn union_values_check_clean_through_signatures() {
        // Object-literal construction of a union stays an error pointing at
        // variant syntax.
        let (_, construction) = check_src("type U = union { A, }; fun main() { val u = U {}; u; }");
        assert_eq!(
            construction.iter().filter(|d| d.is_error()).count(),
            1,
            "{construction:?}"
        );
        assert!(construction[0].message.contains("object literal"));

        // Union values now flow through signatures, bindings, and returns.
        let (typed, diags) =
            check_src("type U = union { A, B(u64), }; fun passthrough(x: U): U { return x; } fun main() { val u = passthrough(U.B(1u64)); u; }");
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
        let ret = typed.func_sigs.values().find(|s| {
            s.ret
                == Ty::Union(Box::new(UnionTy {
                    name: "U".into(),
                    args: Vec::new(),
                }))
        });
        assert!(ret.is_some(), "{typed:?}");
    }

    #[test]
    fn union_generic_construction_infers_and_checks() {
        let (typed, diags) = check_src(
            "type Option[T] = union { None, Some(T), }; fun main() { val a = Option.Some(1u64); val n: Option[u64] = Option.None; a; n; }",
        );
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
        let tys: Vec<&Ty> = typed.types.values().collect();
        assert!(
            tys.iter().any(|t| **t
                == Ty::Union(Box::new(UnionTy {
                    name: "Option".into(),
                    args: vec![Ty::U64],
                }))),
            "{tys:?}"
        );
    }

    #[test]
    fn nullable_desugars_to_the_builtin_option_union() {
        // `?u64` needs no `type Option` declaration: it is the builtin
        // `Option[u64]` (printed with the surface spelling).
        let (typed, diags) =
            check_src("fun f(x: ?u64): ?u64 { return x; } fun main() { val a: ?u64 = null; a; }");
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
        assert_eq!(
            typed
                .func_sigs
                .values()
                .find(|s| s.param_names == vec!["x".to_string()])
                .expect("sig")
                .ret
                .to_string(),
            "?u64"
        );
        // `Option.Some` / `Option.None` spell the same union without a
        // declaration.
        let (_, builtin) = check_src(
            "fun main() { val a = Option.Some(1u64); val n: Option[u64] = Option.None; a; n; }",
        );
        assert!(builtin.iter().all(|d| !d.is_error()), "{builtin:?}");
    }

    #[test]
    fn plain_values_wrap_as_some_for_nullable_formals() {
        // `T` where `?T` is expected wraps as `Option.Some` (recorded for
        // LIR); the node's own type stays the inner lane.
        let (typed, diags) = check_src("fun main() { val x: ?u64 = 5u64; x; }");
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
        assert_eq!(typed.nullable_wraps.len(), 1, "{typed:?}");
        // Int literals coerce through the inner type too.
        let (typed, diags) = check_src("fun main() { val x: ?u64 = 5; x; }");
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
        assert_eq!(typed.nullable_wraps.len(), 1, "{typed:?}");
        // A nullable value where a nullable is expected needs no wrap.
        let (typed, diags) = check_src("fun main() { val x: ?u64 = null; val y: ?u64 = x; y; }");
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
        assert!(typed.nullable_wraps.is_empty(), "{typed:?}");
    }

    #[test]
    fn bare_null_without_context_is_one_e303() {
        let (_, diags) = check_src("fun main() { val x = null; x; }");
        let errors = diags.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "{diags:?}");
        assert_eq!(errors[0].code.as_deref(), Some("E303"));
    }

    #[test]
    fn null_comparison_needs_a_nullable_operand() {
        let (typed, diags) = check_src(
            "fun main() { val x: ?u64 = null; if (x == null) { x; } if (x != null) { x; } }",
        );
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
        assert!(typed.types.values().any(|t| *t == Ty::Bool));
        // One root cause, one error: no cascading bare-`null` E303.
        let (_, bad) = check_src("fun main() { val x = 1u64; if (x == null) { x; } }");
        let errors = bad.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "{bad:?}");
        assert_eq!(errors[0].code.as_deref(), Some("E302"));
        let (_, both) = check_src("fun main() { if (null == null) { 1u64; } }");
        let errors = both.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "{both:?}");
        assert_eq!(errors[0].code.as_deref(), Some("E303"));
    }

    #[test]
    fn null_call_arguments_resolve_against_the_formal() {
        let (_, diags) = check_src(
            "fun f(x: ?u64): u64 { match (x) { Option.Some(v) { return v; } null { return 0u64; } } } fun main() { val a = f(null); a; }",
        );
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
    }

    #[test]
    fn null_match_arm_covers_none() {
        // `null` + `Some` is exhaustive without `else`; `null` + `None` is
        // one duplicate-arm E200.
        let (_, full) = check_src(
            "fun f(x: ?u64): u64 { match (x) { Option.Some(v) { return v; } null { return 0u64; } } } fun main() { val r = f(null); r; }",
        );
        assert!(full.iter().all(|d| !d.is_error()), "{full:?}");
        let (_, dup) = check_src(
            "fun main() { val x: ?u64 = null; match (x) { Option.None { x; } null { x; } else { x; } } }",
        );
        let errors = dup.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "{dup:?}");
        assert_eq!(errors[0].code.as_deref(), Some("E200"));
    }

    #[test]
    fn union_match_binds_payloads_and_needs_coverage() {
        let (_, diags) = check_src(
            "type U = union { A, B(u64), }; fun f(o: U): u64 { match (o) { U.A { return 0u64; } U.B(v) { return v; } } } fun main() { f(U.A); }",
        );
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");

        let (_, missing) = check_src(
            "type U = union { A, B, }; fun main() { val u = U.A; match (u) { U.A { 1u64; } } }",
        );
        assert_eq!(missing.iter().filter(|d| d.is_error()).count(), 1);
        assert_eq!(missing[0].code.as_deref(), Some("E309"));
    }

    #[test]
    fn associated_calls_check_clean_with_sugar() {
        let (typed, diags) = check_src(
            "type C = object { value: u64, fun init(v: u64): *C { return C { value = v }; }, fun bump(self: *C): *C { self.value = self.value + 1; return self; }, fun get(self: C): u64 { return self.value; }, }; fun main() { var c = C.init(1); c.bump(); C.bump(c); val g = c.get(); g; }",
        );
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
        // Sugar targets resolve to the namespaced method names.
        assert!(typed
            .method_targets
            .values()
            .any(|t| *t == MethodTarget::Local("C.bump".into())));
        assert!(typed
            .method_targets
            .values()
            .any(|t| *t == MethodTarget::Local("C.get".into())));
    }

    #[test]
    fn associated_sugar_gate_reports_once() {
        for (src, code) in [
            (
                "type C = object { value: u64, fun bump(self: *C): *C { return self; }, }; fun main() { val c: C = C { value = 1 }; c.bump(); }",
                "E306",
            ),
            (
                "type C = object { value: u64, }; fun main() { var c: *C = C { value = 1 }; c.nope(); }",
                "E302",
            ),
            (
                "type O = object { v: u64, }; type A = object { x: u64, fun f(o: O): u64 { return o.v; }, }; fun main() { var a: *A = A { x = 1 }; a.f(); }",
                "E303",
            ),
            (
                "type C = object { value: u64, fun get(self: C): u64 { return self.value; }, }; fun main() { var c: *C = C { value = 1 }; c.get(1u64); }",
                "E303",
            ),
        ] {
            let (_, diags) = check_src(src);
            let errors: Vec<_> = diags.iter().filter(|d| d.is_error()).collect();
            assert_eq!(errors.len(), 1, "{src}: {diags:?}");
            assert_eq!(errors[0].code.as_deref(), Some(code), "{src}: {diags:?}");
        }
    }

    #[test]
    fn associated_generic_method_records_instances() {
        let (typed, diags) = check_src(
            "type B = object { tag: u64, fun wrap[T](self: B, v: T): T { return v; }, }; fun main() { var b: *B = B { tag = 1 }; b.wrap(7u64); B.wrap(b, \"s\"); }",
        );
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
        assert!(typed.instances.contains_key("B.wrap$u64"));
        assert!(typed.instances.contains_key("B.wrap$String"));
    }

    #[test]
    fn tuples_check_clean_with_access_assign_and_destructure() {
        let (_, diags) = check_src(
            "fun main() { val t: #(u64, String) = #(1u64, \"a\"); val a = t.`0; val u: #(x: u64, y: String) = #(x = 1u64, y = \"b\"); val x = u.x; val #(p, q) = t; val #(x: x2, y: y2) = u; var m: *#(u64, u64) = #(1u64, 2u64); m.`0 = 3u64; var n: *#(x: u64, y: u64) = #(x = 1u64, y = 2u64); n.x = 9u64; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn tuple_shape_mismatches_are_single_errors() {
        for (src, code) in [
            (
                "fun main() { val t: #(u64, String) = #(1u64, \"a\", 2u64); }",
                "E309",
            ),
            (
                "fun main() { val t = #(1u64, \"a\"); val x = t.`5; }",
                "E302",
            ),
            (
                "fun main() { val t = #(1u64, \"a\"); t.`0 = 2u64; }",
                "E310",
            ),
            (
                "fun main() { val t = #(1u64, \"a\"); val #(a, b, c) = t; }",
                "E309",
            ),
            (
                "fun main() { val u = #(x = 1u64, y = 2u64); val z = u.zzz; }",
                "E302",
            ),
            (
                "fun main() { val u = #(x = 1u64, y = 2u64); val v = u.`0; }",
                "E302",
            ),
        ] {
            let (_, diags) = check_src(src);
            let errors: Vec<_> = diags.iter().filter(|d| d.is_error()).collect();
            assert_eq!(errors.len(), 1, "{src}: {diags:?}");
            assert_eq!(errors[0].code.as_deref(), Some(code), "{src}: {diags:?}");
        }
    }

    #[test]
    fn arrays_check_clean() {
        let (_, diags) = check_src(
            "fun sum(a: Array[u64]): u64 { return a[0u64]; } fun main() { val a: *Array[u64] = Array.new::[u64](3u64); a[0u64] = 1u64; val b = [1u64, 2u64]; sum(a); sum(b); }",
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    fn check_importer(provider_src: &str, importer_src: &str) -> (TypedProgram, Vec<Diagnostic>) {
        let (toks, _) = vl_lex::lex(provider_src);
        let (provider, _) = vl_syntax::parse_with_module(&toks, provider_src, "vl.person");
        let (interface, _) = vl_semantic::collect_interface_quiet(&provider);
        let spec = interface.as_spec();
        let (toks, _) = vl_lex::lex(importer_src);
        let (prog, mut diags) = vl_syntax::parse_with_module(&toks, importer_src, "vl.main");
        let (res, mut d) = vl_semantic::resolve_with_modules(&prog, std::slice::from_ref(&spec));
        diags.append(&mut d);
        let hir = vl_hir::lower(&prog, &res);
        let (typed, mut d) = check_with_modules(&hir, std::slice::from_ref(&spec));
        diags.append(&mut d);
        (typed, diags)
    }

    const PERSON_PROVIDER: &str = "type Person = object { name: String, age: u64, }; fun new(name: String, age: u64): *Person { return Person { name = name, age = age, }; } fun name_of(p: Person): String { return p.name; }";

    #[test]
    fn imported_object_types_check_clean() {
        let (_, diags) = check_importer(
            PERSON_PROVIDER,
            "use vl.person; fun main() { var rose = person.new(\"R\", 1); rose.age = 2; val n: String = person.name_of(rose); val lit: vl.person.Person = vl.person.Person { name = \"L\", age = 3 }; person.name_of(lit); }",
        );
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
    }

    #[test]
    fn unknown_qualified_object_is_one_diagnostic() {
        let (_, diags) = check_importer(
            PERSON_PROVIDER,
            "use vl.person; fun main() { val x: vl.person.Nope = person.new(\"R\", 1); }",
        );
        let errors = diags.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "{diags:?}");
        assert_eq!(errors[0].code.as_deref(), Some("E302"), "{diags:?}");
    }

    #[test]
    fn objects_check_field_types_and_reference_operations() {
        let (_, diags) = check_src(
            "type Counter = object { value: u64, }; fun bump(c: *Counter): *Counter { c.value = c.value + 1u64; return c; } fun main() { val c: *Counter = Counter { value = 1u64 }; var d = bump(c); d.value = 3u64; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn object_literal_field_count_is_one_diagnostic() {
        let (_, diags) = check_src(
            "type Point = object { x: u64, }; fun main() { val p = Point { y = 1, z = 2 }; }",
        );
        let errors = diags.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "{diags:?}");
        assert!(errors[0].message.contains("expects 1 field"), "{diags:?}");
    }

    #[test]
    fn unknown_object_literal_is_one_diagnostic() {
        let (_, diags) = check_src("fun main() { val x = Missing {}; }");
        let errors = diags.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "{diags:?}");
        assert!(
            errors[0].message.contains("cannot find object type"),
            "{diags:?}"
        );
    }

    #[test]
    fn integer_literals_coerce_in_array_context() {
        let (_, diags) = check_src("fun main() { val a = [1, 2u64]; a; }");
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn integer_literals_coerce_at_concrete_boundaries() {
        let (_, diags) = check_src(
            "fun take(a: u64, b: i64, c: u8): u64 { return a; } fun main(): u64 { take(1, 2, 3); return 4; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn empty_literal_needs_the_typed_constructor() {
        let (_, diags) = check_src("fun main() { val e = []; e; }");
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
        let (_, diags) = check_src("fun main() { val a = Array.new(3); a; }");
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
            check_src("fun main() { val scores: Array[u64] = Array.new(3); scores; }");
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed
            .types
            .values()
            .any(|t| *t == Ty::Array(Box::new(Ty::U64))));
    }

    #[test]
    fn annotated_let_accepts_empty_literal() {
        let (_, diags) = check_src("fun main() { val e: *Array[u64] = []; e[0u64] = 1u64; e; }");
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn annotated_let_rejects_mismatch() {
        let (_, diags) = check_src("fun main() { val x: u64 = \"s\"; x; }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("E309")),
            "{diags:?}"
        );
    }

    #[test]
    fn annotated_let_coerces_int_literals() {
        let (_, diags) = check_src("fun main() { val x: u64 = 3; x; }");
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn annotated_let_with_explicit_turbofish_checks() {
        let (_, diags) = check_src("fun main() { val a: Array[u64] = Array.new::[u64](3); a; }");
        assert!(diags.is_empty(), "{diags:?}");
        let (_, diags) = check_src("fun main() { val a: Array[String] = Array.new::[u64](3); a; }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
    }

    #[test]
    fn annotated_let_in_generic_body() {
        let (_, diags) = check_src(
            "fun f[T](x: T): T { val y: T = x; val a: Array[T] = Array.new(1); return y; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn failed_annotation_poisons_quietly() {
        // Parser reports E105; typecheck must not cascade.
        let (toks, _) = vl_lex::lex("fun main() { val x: Bogus = 1; x; }");
        let (prog, pdiags) = vl_syntax::parse(&toks, "");
        assert!(pdiags.iter().any(|d| d.is_error()));
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        let (_, tdiags) = check(&hir);
        assert!(tdiags.is_empty(), "{tdiags:?}");
    }

    #[test]
    fn array_new_arg_is_checked() {
        let (_, diags) = check_src("fun main() { val a = Array.new::[u64](1.0f64); a; }");
        assert!(
            diags.iter().any(|d| d.message.contains("expects `u64`")),
            "{diags:?}"
        );
    }

    #[test]
    fn array_new_rejects_void_element() {
        let (_, diags) = check_src("fun main() { val a = Array.new::[void](1u64); a; }");
        assert!(
            diags.iter().any(|d| d.message.contains("cannot be `void`")),
            "{diags:?}"
        );
    }

    #[test]
    fn string_arrays_check_clean() {
        let (_, diags) = check_src(
            "fun main() { val a = Array.new::[String](2u64); val b = [\"x\", \"y\"]; b[0u64]; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn index_requires_array_and_u64() {
        let (_, diags) = check_src(r#"fun main() { val s = "hi"; val x = s[0u64]; x; }"#);
        assert!(
            diags.iter().any(|d| d.message.contains("cannot index")),
            "{diags:?}"
        );
        let (_, diags) = check_src("fun main() { val a = [1u64]; val x = a[true]; x; }");
        assert!(
            diags.iter().any(|d| d.message.contains("must be `u64`")),
            "{diags:?}"
        );
    }

    #[test]
    fn index_assign_checks_shapes() {
        let (_, diags) = check_src(r#"fun main() { val a: *Array[u64] = [1u64]; a[0u64] = "s"; }"#);
        assert!(
            diags.iter().any(|d| d.message.contains("cannot store")),
            "{diags:?}"
        );
    }

    #[test]
    fn arrays_are_not_numeric() {
        let (_, diags) =
            check_src("fun main() { val a = [1u64]; val b = [2u64]; val c = a + b; c; }");
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("E302")),
            "{diags:?}"
        );
    }

    #[test]
    fn comparisons_and_logic_yield_bool() {
        let (typed, diags) =
            check_src("fun main() { val a = 1; val ok = a < 2 && a == 1 || !false; ok; }");
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.types.values().any(|t| *t == Ty::Bool));
    }

    #[test]
    fn untyped_integer_comparison_uses_concrete_context() {
        let (_, diags) = check_src("fun main() { val x = 1 < 2u64; x; }");
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn logical_operators_require_bool() {
        let (_, diags) = check_src("fun main() { val x = 1 && true; x; }");
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("E304")),
            "{diags:?}"
        );
    }

    #[test]
    fn not_requires_bool() {
        let (_, diags) = check_src("fun main() { !1; }");
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("E304")),
            "{diags:?}"
        );
    }

    #[test]
    fn while_condition_must_be_bool() {
        let (_, diags) = check_src("fun main() { while (1) { 2; } }");
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("while condition must be bool")),
            "{diags:?}"
        );
    }

    #[test]
    fn assignment_type_mismatch_errors() {
        let (_, diags) = check_src(r#"fun main() { var x = 1; x = "s"; }"#);
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("E309")),
            "{diags:?}"
        );
    }

    #[test]
    fn assignment_with_matching_type_checks() {
        let (_, diags) = check_src("fun main() { var x = 1; x = 2; }");
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn ints_check_clean() {
        let (_, diags) = check_src("val x = 1 + 2 * 3;");
        assert!(diags.is_empty());
    }

    #[test]
    fn strings_check_as_string_type() {
        let (typed, diags) = check_src(r#"val greeting = "hello";"#);
        assert!(diags.is_empty());
        assert_eq!(typed.types.values().next(), Some(&Ty::String));
    }

    #[test]
    fn string_bindings_keep_their_type() {
        let (typed, diags) = check_src(r#"val greeting = "hello"; val copy = greeting;"#);
        assert!(diags.is_empty());
        assert_eq!(typed.types.get(&3), Some(&Ty::String));
    }

    #[test]
    fn var_string_literal_gets_mutable_view() {
        let (typed, diags) = check_src(r#"var greeting = "hello";"#);
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed
            .types
            .values()
            .any(|t| *t == Ty::Mutable(Box::new(Ty::String))));
    }

    #[test]
    fn const_div_by_zero_errors() {
        let (_, diags) = check_src("val x = 1 / 0;");
        assert!(diags.iter().any(|d| d.message.contains("division by zero")));
    }

    #[test]
    fn call_with_correct_types_checks_clean() {
        let (_, diags) =
            check_src("fun add(a: i64, b: i64): i64 { return a + b; } fun main() { add(1, 2); }");
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn call_with_wrong_arity_errors_once() {
        let (_, diags) =
            check_src("fun add(a: i64, b: i64): i64 { return a + b; } fun main() { add(1); }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("expects 2"));
    }

    #[test]
    fn call_with_wrong_param_type_errors() {
        let (_, diags) = check_src(
            r#"fun add(a: i64, b: i64): i64 { return a + b; } fun main() { add(1, "s"); }"#,
        );
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("expects `i64`"), "{diags:?}");
    }

    #[test]
    fn return_mismatch_errors() {
        let (_, diags) = check_src(r#"fun f(): i64 { return "s"; }"#);
        assert!(
            diags.iter().any(|d| d.message.contains("declares return")),
            "{diags:?}"
        );
    }

    #[test]
    fn trailing_expr_is_not_a_return() {
        // No implicit returns: a bare tail value does not satisfy `: i64`.
        let (_, diags) = check_src(r#"fun f(): i64 { "s"; }"#);
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("not all paths return")),
            "{diags:?}"
        );
    }

    #[test]
    fn explicit_return_satisfies_declared_type() {
        let (_, diags) = check_src(r#"fun f(): i64 { return 1; }"#);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn missing_return_is_an_error() {
        let (_, diags) = check_src(r#"fun f(): i64 { val x = 1; }"#);
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("E307")),
            "{diags:?}"
        );
    }

    #[test]
    fn bare_return_in_value_function_errors() {
        let (_, diags) = check_src(r#"fun f(): i64 { return; }"#);
        assert!(
            diags.iter().any(|d| d.message.contains("returns nothing")),
            "{diags:?}"
        );
    }

    #[test]
    fn value_return_in_void_function_errors() {
        let (_, diags) = check_src(r#"fun main() { return 1; }"#);
        assert!(
            diags.iter().any(|d| d.message.contains("returns `void`")),
            "{diags:?}"
        );
    }

    #[test]
    fn bare_return_in_void_function_checks() {
        let (_, diags) = check_src(r#"fun main() { return; }"#);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn return_inside_branch_satisfies_declared_type() {
        let (_, diags) =
            check_src(r#"fun f(x: bool): i64 { if (x) { return 1; } else { return 2; } }"#);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn void_function_accepts_any_tail() {
        let (_, diags) = check_src("fun main() { 1 + 2; }");
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn binding_void_errors() {
        let (_, diags) = check_src("use std.print; fun main() { val x = print(\"hi\"); }");
        assert!(
            diags.iter().any(|d| d.message.contains("void")),
            "{diags:?}"
        );
    }

    #[test]
    fn calling_a_let_binding_errors() {
        let (_, diags) = check_src("val x = 1; fun main() { x(); }");
        assert!(diags.iter().any(|d| d.message.contains("not a function")));
    }

    #[test]
    fn unresolved_callee_poisoned_quietly() {
        // E201 comes from resolve; typecheck must not add a second error.
        let (toks, _) = vl_lex::lex("fun main() { nope(1); }");
        let (prog, _) = vl_syntax::parse(&toks, "");
        let (res, rdiags) = vl_semantic::resolve(&prog);
        assert!(rdiags.iter().any(|d| d.is_error()));
        let hir = vl_hir::lower(&prog, &res);
        let (_, tdiags) = check(&hir);
        assert!(tdiags.is_empty());
    }

    #[test]
    fn forward_global_type_is_not_invented() {
        let (_, diags) = check_src("val x = later + 1; val later = \"s\";");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("forward global"));
    }

    #[test]
    fn every_numeric_zero_divisor_is_rejected() {
        for source in [
            "val x = 1u64 / 0u64;",
            "val x = 1u8 / 0u8;",
            "val x = 1.0f64 / 0.0f64;",
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
            "fun id[T](x: T): T { return x; } fun main() { val a = id(1u64); val b = id::[String](\"s\"); a; b; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.instances.contains_key("id$u64"));
        assert!(typed.instances.contains_key("id$String"));
        assert_eq!(typed.root_calls.len(), 2);
        let inst = &typed.instances["id$u64"];
        assert_eq!(inst.sig.ret, Ty::U64);
    }

    #[test]
    fn generic_array_first_checks() {
        let (typed, diags) = check_src(
            "fun first[T](a: Array[T]): T { return a[0u64]; } fun main() { val x = first([1u64, 2u64]); x; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.instances.contains_key("first$u64"));
    }

    #[test]
    fn inference_failure_asks_for_turbofish() {
        let (_, diags) = check_src(
            "fun never[T](): T { val a = Array.new::[T](1u64); return a[0u64]; } fun main() { never(); }",
        );
        assert!(
            diags.iter().any(|d| d.message.contains("cannot infer")),
            "{diags:?}"
        );
    }

    #[test]
    fn conflicting_inference_is_one_error() {
        let (_, diags) =
            check_src("fun same[T](a: T, b: T): T { return a; } fun main() { same(1u64, 2i64); }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("conflicting types"), "{diags:?}");
    }

    #[test]
    fn explicit_arity_mismatch_is_one_error() {
        let (_, diags) =
            check_src("fun id[T](x: T): T { return x; } fun main() { id::[u64, i64](1u64); }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("type argument"), "{diags:?}");
    }

    #[test]
    fn turbofish_on_monomorphic_fn_is_an_error() {
        let (_, diags) = check_src(
            "fun add(a: i64, b: i64): i64 { return a + b; } fun main() { add::[u64](1, 2); }",
        );
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("not generic"), "{diags:?}");
    }

    #[test]
    fn unbound_type_argument_is_an_error() {
        let (_, diags) =
            check_src("fun id[T](x: T): T { return x; } fun main() { id::[Bogus](1u64); }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("unknown type"), "{diags:?}");
    }

    #[test]
    fn generic_to_generic_forwarding_monomorphizes() {
        let (typed, diags) = check_src(
            "fun id[T](x: T): T { return x; } fun wrap[T](x: T): T { return id::[T](x); } fun main() { wrap(1u64); }",
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
            "fun count[T](a: Array[T], n: u64): u64 { if (n == 0u64) { return 0u64; } return count(a, n - 1u64); } fun main() { count([1u64], 2u64); }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.instances.contains_key("count$u64"));
    }

    #[test]
    fn wrong_value_arg_in_instance_is_an_error() {
        let (_, diags) = check_src(
            "fun first[T](a: Array[T]): T { return a[0u64]; } fun main() { first::[u64]([\"s\"]); }",
        );
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(
            diags[0].message.contains("expects `Array[u64]`"),
            "{diags:?}"
        );
    }

    #[test]
    fn uninstantiated_generic_emits_no_instance() {
        let (typed, diags) = check_src("fun dead[T](x: T): T { return x; } fun main() { 1u64; }");
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.instances.is_empty());
    }

    #[test]
    fn partial_return_path_is_an_error() {
        // Every reachable path must return: a single `if` branch is not
        // enough, even though a value `return` is present.
        let (_, diags) =
            check_src("fun f(x: bool): u64 { if (x) { return 1u64; } } fun main() { f(true); }");
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
        let (_, diags) =
            check_src("fun f(x: bool): u64 { while (x) { return 1u64; } } fun main() { f(true); }");
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
        let (_, diags) = check_src("fun f(): i64 { return; } fun main() { }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("returns nothing"), "{diags:?}");
    }

    #[test]
    fn expanding_recursion_hits_the_instance_budget() {
        // Original void-function repro: type-expanding recursion must
        // terminate with exactly one E303 diagnostic, not hang the compiler.
        // The void body keeps the return check quiet so the budget error is
        // the one root cause (single-root-error rule).
        let (_, diags) = check_src("fun grow[T](x: T) { grow([x]); } fun main() { grow(1u64); }");
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
        let (_, diags) = check_src("fun main() { val a = Array.new::[Array[Bogus]](1u64); }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("unknown type"), "{diags:?}");
    }

    #[test]
    fn u8_literal_range_is_checked() {
        let (_, diags) = check_src("fun main() { val x: u8 = 300; x; }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("out of range"), "{diags:?}");
        let (_, diags) = check_src("fun main() { val x: u8 = 255; x; }");
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn int_binding_does_not_escape_to_u8_param() {
        // `let v = 300` resolves to `u64`, which must not pass a `u8`
        // parameter even though the literal would fit neither.
        let (_, diags) =
            check_src("fun take(x: u8): u8 { return x; } fun main() { val v = 300; take(v); }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("expects `u8`"), "{diags:?}");
    }

    #[test]
    fn generic_result_does_not_escape_to_u8() {
        // All-literal inference defaults to `u64`, so a generic result
        // crossing a narrower boundary mismatches instead of truncating.
        let (typed, diags) = check_src(
            "fun id[T](x: T): T { return x; } fun f(): u64 { return id(300); } fun main() { f(); }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.instances.contains_key("id$u64"));
        let (_, diags) = check_src(
            "fun id[T](x: T): T { return x; } fun f(): u8 { return id(300); } fun main() { f(); }",
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
                "fun same[T](a: T, b: T): T {{ return a; }} fun main() {{ {call}; }}"
            ));
            assert!(diags.is_empty(), "{call}: {diags:?}");
            assert!(typed.instances.contains_key("same$u64"), "{call}");
        }
    }

    #[test]
    fn explicit_generic_call_contextualizes_empty_array_argument() {
        let (_, diags) = check_src("fun take[T](a: *Array[T]) {} fun main() { take::[u64]([]); }");
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn turbofish_on_extern_is_an_error() {
        let (_, diags) = check_src("use std.print; fun main() { print::[u64](\"hi\"); }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("not generic"), "{diags:?}");
    }

    /// Build a `main` calling `id` once per nesting depth `0..count`
    /// (`1u64`, `[1u64]`, `[[1u64]]`, ...), each a distinct instance.
    fn nested_id_calls(count: usize) -> String {
        let mut src = String::from("fun id[T](x: T): T { return x; } fun main() { ");
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
            "fun main() { val x = 1; x; }",
            "fun id[T](x: T): T { return x; } fun main() { id(1u64); }",
            "fun main() { val a = [1, 2]; a; }",
            "fun main() { 1 + 2; }",
        ] {
            let (hir, typed, diags) = check_src_with_hir(src);
            assert!(diags.iter().all(|d| !d.is_error()), "{src}: {diags:?}");
            assert!(typed.validate_normalized(&hir, &diags).is_empty(), "{src}");
        }
    }

    #[test]
    fn normalized_validation_rejects_lingering_int() {
        let (hir, mut typed, _) = check_src_with_hir("fun main() { val x = 1u64; x; }");
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
        let (hir, typed, diags) =
            check_src_with_hir("fun id[T](x: T): T { return x; } fun main() { id(1u64); }");
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.validate_normalized(&hir, &diags).is_empty());
        assert!(typed.instances.contains_key("id$u64"));
    }

    #[test]
    fn untyped_arithmetic_defaults_to_u64() {
        let (typed, diags) = check_src("fun main() { 1 + 2; }");
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.types.values().any(|t| *t == Ty::U64));
        assert!(!typed.types.values().any(|t| *t == Ty::Int));
    }

    // -- Item 3: centralized coercion ------------------------------------

    #[test]
    fn implicit_cross_integer_stays_narrow() {
        // Concrete `i64` does not coerce to `u64` even though both are
        // integers; only literals coerce.
        let (_, diags) = check_src(
            "fun take(x: u64): u64 { return x; } fun main() { val v: i64 = 1i64; take(v); }",
        );
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
            "fun first2[T](a: Array[Array[T]]): T { return a[0u64][0u64]; } fun main() { first2([[1u64]]); }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.instances.contains_key("first2$u64"));
    }

    #[test]
    fn all_int_constraints_default_to_u64() {
        let (typed, diags) =
            check_src("fun same[T](a: T, b: T): T { return a; } fun main() { same(1, 2); }");
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.instances.contains_key("same$u64"));
    }

    // -- Item 6: flow -------------------------------------------------------

    #[test]
    fn flow_breaks_do_not_satisfy_returns() {
        // `break` inside `while` diverges the loop body but the function
        // still falls through.
        let (_, diags) = check_src("fun f(): u64 { while (true) { break; } } fun main() { f(); }");
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("E307")),
            "{diags:?}"
        );
    }

    #[test]
    fn unreachable_code_warns_without_failing() {
        let (_, diags) = check_src("fun main() { return; val x = 1; x; }");
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("W001")),
            "{diags:?}"
        );
    }

    #[test]
    fn if_both_branches_return_satisfies() {
        let (_, diags) = check_src(
            "fun f(x: bool): u64 { if (x) { return 1u64; } else { return 2u64; } } fun main() { f(true); }",
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    // -- Item 7: `as` casts --------------------------------------------------

    #[test]
    fn as_cast_same_type_checks() {
        let (_, diags) = check_src("fun main() { val x = 1u64 as u64; x; }");
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn as_cast_variable_between_integers_checks() {
        let (_, diags) = check_src(
            "fun take(x: u8): u8 { return x; } fun main() { val v = 200u64; take(v as u8); }",
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn as_cast_literal_out_of_range_is_one_error() {
        let (_, diags) = check_src("fun main() { val x = 300 as u8; x; }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("out of range"), "{diags:?}");
    }

    #[test]
    fn as_cast_rejects_non_integer_target() {
        let (_, diags) = check_src("fun main() { val x = 1u64 as String; x; }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("cannot cast"), "{diags:?}");
    }

    #[test]
    fn as_cast_rejects_non_integer_source() {
        let (_, diags) = check_src("fun main() { val s = \"hi\"; val x = s as u8; x; }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("cannot cast"), "{diags:?}");
    }

    #[test]
    fn implicit_variable_conversion_still_rejected() {
        // Without `as`, a `u64` variable must not flow into `u8`.
        let (_, diags) =
            check_src("fun take(x: u8): u8 { return x; } fun main() { val v = 1u64; take(v); }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
    }

    // -- Item 8: constrained generics ----------------------------------------

    #[test]
    fn unconstrained_param_rejects_arithmetic() {
        let (_, diags) = check_src(
            "fun add[T](a: T, b: T): T { return a + b; } fun main() { add(1u64, 2u64); }",
        );
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("Numeric"), "{diags:?}");
    }

    #[test]
    fn numeric_bound_allows_arithmetic() {
        let (typed, diags) = check_src(
            "fun add[T extends Numeric](a: T, b: T): T { return a + b; } fun main() { add(1u64, 2u64); add(1i64, 2i64); }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.instances.contains_key("add$u64"));
        assert!(typed.instances.contains_key("add$i64"));
    }

    #[test]
    fn comparable_bound_allows_equality() {
        let (_, diags) = check_src(
            "fun eq[T extends Comparable](a: T, b: T): bool { return a == b; } fun main() { eq(1u64, 2u64); eq(\"a\", \"b\"); }",
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn bound_violation_is_one_error() {
        let (_, diags) = check_src(
            "fun add[T extends Numeric](a: T, b: T): T { return a + b; } fun main() { add(\"a\", \"b\"); }",
        );
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("does not satisfy"), "{diags:?}");
        assert_eq!(diags[0].code.as_deref(), Some("E303"));
    }

    #[test]
    fn unconstrained_forwarding_to_bounded_is_an_error() {
        let (_, diags) = check_src(
            "fun add[T extends Numeric](a: T, b: T): T { return a + b; } fun wrap[T](x: T): T { return add(x, x); } fun main() { wrap(1u64); }",
        );
        assert!(
            diags.iter().any(|d| d.message.contains("satisfy")),
            "{diags:?}"
        );
    }

    #[test]
    fn numeric_implies_comparable_forwarding() {
        let (_, diags) = check_src(
            "fun eq[T extends Comparable](a: T, b: T): bool { return a == b; } fun wrap[T extends Numeric](x: T): bool { return eq(x, x); } fun main() { wrap(1u64); }",
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn as_cast_on_generic_param_is_rejected() {
        // `Numeric` includes `f64`: allowing `x as u8` for `x: T` would copy
        // IEEE-754 bits into an integer lane once `T = f64`.
        let (_, diags) = check_src(
            "fun get[T extends Numeric](x: T): u8 { return x as u8; } fun main() { get(1.5f64); }",
        );
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("generic"), "{diags:?}");
        assert_eq!(diags[0].code.as_deref(), Some("E302"));
    }

    #[test]
    fn as_cast_from_f64_is_rejected() {
        let (_, diags) = check_src("fun main() { val x = 1.0f64 as u8; x; }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("cannot cast"), "{diags:?}");
    }

    #[test]
    fn nested_int_literal_defers_to_concrete_element() {
        // Array literals keep `Array[Int]` until solving: `T` solves to
        // `Array[u8]` (not a `u64`-vs-`u8` conflict) and the `1` coerces.
        let (typed, diags) =
            check_src("fun same[T](a: T, b: T): T { return a; } fun main() { same([1], [2u8]); }");
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.instances.contains_key("same$Array_u8"));
    }

    #[test]
    fn nested_int_conflict_still_conflicts() {
        let (_, diags) = check_src(
            "fun same[T](a: T, b: T): T { return a; } fun main() { same([1u64], [2u8]); }",
        );
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("conflicting types"), "{diags:?}");
    }

    #[test]
    fn unannotated_all_int_array_defaults_to_u64() {
        let (typed, diags) = check_src("fun main() { val a = [1, 2]; a; }");
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
            "fun add[T extends Numeric](a: T, b: T): T { return a + b; } fun main() { add(1u64, 2u64); }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.validate_normalized(&hir, &diags).is_empty());
    }

    #[test]
    fn validation_stands_down_with_prior_errors() {
        let (hir, mut typed, _) = check_src_with_hir("fun main() { val x = 1u64; x; }");
        typed.types.insert(0, Ty::Error);
        // No prior errors: poison at the boundary is itself reported.
        let errs = typed.validate_normalized(&hir, &[]);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].code.as_deref(), Some("E500"));
        // With a prior error (lowering already blocked): validation is moot.
        let prior = vec![Diagnostic::error("prior failure").with_code("E999")];
        assert!(typed.validate_normalized(&hir, &prior).is_empty());
    }

    #[test]
    fn mutable_ty_structural_ops() {
        use std::collections::HashMap;
        let foo = Ty::Object("Foo".into());
        let mfoo = Ty::Mutable(Box::new(foo.clone()));
        // Display round-trips the `*` spelling.
        assert_eq!(mfoo.to_string(), "*Foo");
        assert_eq!(
            Ty::Mutable(Box::new(Ty::Array(Box::new(mfoo.clone())))).to_string(),
            "*Array[*Foo]"
        );
        // from_vl preserves capability recursively.
        let v = VlType::Mutable(Box::new(VlType::Array(Box::new(VlType::Mutable(
            Box::new(VlType::Object("Foo".into())),
        )))));
        assert_eq!(
            Ty::from_vl(&v),
            Ty::Mutable(Box::new(Ty::Array(Box::new(mfoo.clone()))))
        );
        // Predicates and erasure.
        assert!(mfoo.is_mutable_view());
        assert!(!foo.is_mutable_view());
        assert!(mfoo.is_reference_type());
        assert_eq!(mfoo.readonly_view(), foo);
        assert_eq!(
            Ty::Mutable(Box::new(Ty::Array(Box::new(foo.clone())))).erase_capability(),
            Ty::Array(Box::new(foo.clone()))
        );
        assert_eq!(
            Ty::Mutable(Box::new(Ty::Array(Box::new(mfoo.clone())))).runtime_type(),
            Ty::Array(Box::new(foo.clone()))
        );
        // Substitution descends through `*`.
        let mut env = HashMap::new();
        env.insert("T".to_string(), foo.clone());
        assert_eq!(
            subst_ty(
                &Ty::Mutable(Box::new(Ty::Array(Box::new(Ty::Param("T".into()))))),
                &env
            ),
            Ty::Mutable(Box::new(Ty::Array(Box::new(foo.clone()))))
        );
        // Mangling distinguishes `Foo` from `*Foo` with identical runtime layout.
        assert_ne!(mangle_ty(&foo), mangle_ty(&mfoo));
        assert!(mangle_ty(&mfoo).contains("Mut"));
        // Poison and int traversal see through `*`.
        assert!(ty_has_error(&Ty::Mutable(Box::new(Ty::Array(Box::new(
            Ty::Error
        ))))));
        assert!(ty_contains_int(&Ty::Mutable(Box::new(Ty::Array(
            Box::new(Ty::Int)
        )))));
        assert_eq!(
            default_inferred_ty(Ty::Mutable(Box::new(Ty::Int))),
            Ty::Mutable(Box::new(Ty::U64))
        );
        assert_eq!(
            unify_solved(&mfoo, &mfoo),
            Some(mfoo.clone()),
            "identical mutable views unify"
        );
        // Mixed `*Foo` vs `Foo` picks the safe read-only side.
        assert_eq!(unify_solved(&mfoo, &foo), Some(foo.clone()));
        assert_eq!(unify_solved(&foo, &mfoo), Some(foo.clone()));
        assert!(can_coerce(&mfoo, &foo));
        assert!(!can_coerce(&foo, &mfoo));
        assert!(same_type(&mfoo, &mfoo));
        assert!(!same_type(&mfoo, &foo));
        assert_eq!(common_type(&mfoo, &foo), Some(foo.clone()));
        assert_eq!(
            project_capability(&foo, &mfoo),
            foo,
            "read-only receiver downgrades"
        );
        assert_eq!(
            project_capability(&mfoo, &mfoo),
            mfoo,
            "mutable receiver preserves"
        );
    }

    #[test]
    fn mutable_downgrade_allowed_at_all_boundaries() {
        // `*Foo -> Foo` succeeds everywhere; `Foo -> *Foo` fails everywhere.
        let (_, diags) = check_src(
            "type Foo = object { value: u64, }; fun read(foo: Foo) {} fun change(foo: *Foo) {} fun main() { val e: *Foo = Foo { value = 1u64 }; val v: Foo = e; read(e); read(v); change(e); }",
        );
        assert!(diags.is_empty(), "{diags:?}");

        for (src, code) in [
            (
                "type Foo = object { value: u64, }; fun change(foo: *Foo) {} fun main() { val v: Foo = Foo { value = 1u64 }; change(v); }",
                "E306",
            ),
            (
                "type Foo = object { value: u64, }; fun main() { val v: Foo = Foo { value = 1u64 }; val bad: *Foo = v; }",
                "E309",
            ),
            (
                "type Foo = object { value: u64, }; fun get(): Foo { val v: Foo = Foo { value = 1u64 }; return v; } fun bad(): *Foo { val v: Foo = Foo { value = 1u64 }; return v; }",
                "E307",
            ),
        ] {
            let (_, diags) = check_src(src);
            assert_eq!(
                diags.iter().filter(|d| d.is_error()).count(),
                1,
                "{src}: {diags:?}"
            );
            assert_eq!(
                diags[0].code.as_deref(),
                Some(code),
                "{src}: {diags:?}"
            );
        }
    }

    #[test]
    fn local_rebinding_uses_directional_coercion() {
        let (_, diags) = check_src(
            "type Foo = object { value: u64, }; fun main() { var c: Foo = Foo { value = 1u64 }; c = Foo { value = 2u64 }; var m: *Foo = Foo { value = 1u64 }; m = Foo { value = 2u64 }; var p = 1u64; p = 2u64; var a: *Array[u64] = [1u64]; a = [2u64]; }",
        );
        // `m = Foo{}` upgrades a fresh readonly literal? No: fresh adopts
        // `*Foo` via context, so all rebindings are downgrades or exact.
        assert!(diags.is_empty(), "{diags:?}");

        let (_, diags) = check_src(
            "type Foo = object { value: u64, }; fun main() { var m: *Foo = Foo { value = 1u64 }; val v: Foo = Foo { value = 1u64 }; m = v; }",
        );
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert_eq!(diags[0].code.as_deref(), Some("E309"));
    }

    #[test]
    fn readonly_writes_fail_mutable_writes_pass() {
        let (_, diags) = check_src(
            "type Foo = object { value: u64, }; fun edit(m: *Foo) { m.value = 1u64; } fun read(v: Foo) { v.value; }",
        );
        assert!(diags.is_empty(), "{diags:?}");

        let (_, diags) =
            check_src("type Foo = object { value: u64, }; fun bad(v: Foo) { v.value = 1u64; }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert_eq!(diags[0].code.as_deref(), Some("E310"));

        let (_, diags) = check_src("fun bad(a: Array[u64]) { a[0u64] = 1u64; }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert_eq!(diags[0].code.as_deref(), Some("E310"));

        let (_, diags) = check_src("fun good(a: *Array[u64]) { a[0u64] = 1u64; }");
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn deep_projection_does_not_leak_mutability() {
        let (_, diags) = check_src(
            "type Child = object { value: u64, }; type Parent = object { child: *Child, children: *Array[*Child], }; fun bad(p: Parent) { p.child.value = 1u64; }",
        );
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert_eq!(diags[0].code.as_deref(), Some("E310"));

        let (_, diags) = check_src(
            "type Child = object { value: u64, }; type Parent = object { child: *Child, children: *Array[*Child], }; fun good(p: *Parent) { p.child.value = 1u64; p.children[0u64].value = 1u64; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn mutable_returns_preserve_and_cannot_launder() {
        let (_, diags) = check_src(
            "type Foo = object { value: u64, }; fun create(): *Foo { return Foo { value = 1u64 }; } fun main() { val e = create(); val v: Foo = create(); e; v; }",
        );
        assert!(diags.is_empty(), "{diags:?}");

        let (_, diags) = check_src(
            "type Foo = object { value: u64, }; fun inspect(v: Foo): Foo { return v; } fun main() { val v: Foo = Foo { value = 1u64 }; val bad: *Foo = inspect(v); }",
        );
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
    }

    #[test]
    fn fresh_allocations_default_readonly_and_adopt_mutable() {
        let (typed, diags) = check_src(
            "type Foo = object { value: u64, }; fun main() { val v = Foo { value = 1u64 }; v; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.types.values().any(|t| *t == Ty::Object("Foo".into())));

        let (typed, diags) = check_src(
            "type Foo = object { value: u64, }; fun main() { val e: *Foo = Foo { value = 1u64 }; e; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed
            .types
            .values()
            .any(|t| *t == Ty::Mutable(Box::new(Ty::Object("Foo".into())))));

        // Existing values never upgrade from context.
        let (_, diags) = check_src(
            "type Foo = object { value: u64, }; fun get(): Foo { val v: Foo = Foo { value = 1u64 }; return v; } fun main() { val bad: *Foo = get(); }",
        );
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
    }

    #[test]
    fn binding_keyword_controls_fresh_reference_capability() {
        let (typed, diags) = check_src(
            "type Foo = object { value: u64, }; fun main() { var fresh = Foo { value = 1u64 }; fresh.value = 2u64; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed
            .types
            .values()
            .any(|t| *t == Ty::Mutable(Box::new(Ty::Object("Foo".into())))));

        let (typed, diags) = check_src(
            "type Foo = object { value: u64, }; fun main() { val view = Foo { value = 1u64 }; view; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.types.values().any(|t| *t == Ty::Object("Foo".into())));

        let (typed, diags) = check_src(
            "type Foo = object { value: u64, }; fun main() { var view: Foo = Foo { value = 1u64 }; view; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.types.values().any(|t| *t == Ty::Object("Foo".into())));

        let (_, diags) = check_src(
            "type Foo = object { value: u64, }; fun read(): Foo { return Foo { value = 1u64 }; } fun main() { var bad = read(); }",
        );
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("E309")),
            "{diags:?}"
        );
    }

    #[test]
    fn invalid_mutable_shapes_are_one_e106() {
        // `*T` parses (needs substitution) and fails here with one E106 per
        // invalid annotation.
        let (_, diags) = check_src("fun f[T](x: *T): T { return x; }");
        assert_eq!(
            diags.iter().filter(|d| d.is_error()).count(),
            1,
            "{diags:?}"
        );
        assert_eq!(diags[0].code.as_deref(), Some("E106"), "{diags:?}");

        // `*u64` is rejected by the parser; type checking stays quiet (no
        // cascade) when fed the poisoned HIR.
        for src in [
            "fun f(x: *u64) { x; }",
            "type Foo = object { value: *u64, }; fun main() { val x = 1u64; x; }",
        ] {
            let (toks, _) = vl_lex::lex(src);
            let (prog, pdiags) = vl_syntax::parse(&toks, src);
            assert!(
                pdiags.iter().any(|d| d.code.as_deref() == Some("E106")),
                "{src}: {pdiags:?}"
            );
            let (res, _) = vl_semantic::resolve(&prog);
            let hir = vl_hir::lower(&prog, &res);
            let (_, tdiags) = check(&hir);
            assert!(tdiags.iter().all(|d| !d.is_error()), "{src}: {tdiags:?}");
        }
    }

    #[test]
    fn as_cast_never_upgrades_capability() {
        let (_, diags) = check_src(
            "type Foo = object { value: u64, }; fun main() { val v: Foo = Foo { value = 1u64 }; val x = v as u64; x; }",
        );
        // `Foo as u64` is an unsupported cast (E302), not a capability upgrade.
        assert!(diags.iter().any(|d| d.code.as_deref() == Some("E302")));
    }

    #[test]
    fn generic_mutable_inference_preserves_and_merges() {
        // Unconstrained `T` preserves `*Foo`.
        let (typed, diags) = check_src(
            "type Foo = object { value: u64, }; fun identity[T](x: T): T { return x; } fun main() { val e: *Foo = Foo { value = 1u64 }; val same = identity(e); same; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.instances.contains_key("identity$Mut_Object_Foo"));

        // Explicit turbofish accepts `*Foo`.
        let (_, diags) = check_src(
            "type Foo = object { value: u64, }; fun identity[T](x: T): T { return x; } fun main() { val e: *Foo = Foo { value = 1u64 }; val same = identity::[*Foo](e); same; }",
        );
        assert!(diags.is_empty(), "{diags:?}");

        // Mixed `*Foo` + `Foo` constraints choose read-only `Foo`.
        let (typed, diags) = check_src(
            "type Foo = object { value: u64, }; fun same[T](a: T, b: T): T { return a; } fun main() { val e: *Foo = Foo { value = 1u64 }; val v: Foo = Foo { value = 2u64 }; val r = same(e, v); r; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.instances.contains_key("same$Object_Foo"));

        // Mangling distinguishes `Foo` from `*Foo`.
        let (typed, diags) = check_src(
            "type Foo = object { value: u64, }; fun identity[T](x: T): T { return x; } fun main() { val e: *Foo = Foo { value = 1u64 }; val v: Foo = Foo { value = 2u64 }; val a = identity(e); val b = identity(v); a; b; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.instances.contains_key("identity$Mut_Object_Foo"));
        assert!(typed.instances.contains_key("identity$Object_Foo"));
    }

    #[test]
    fn generic_mutable_forwarding_and_array_context() {
        // Forwarding preserves `*Foo` through `wrap[T]` -> `id[T]`.
        let (_, diags) = check_src(
            "type Foo = object { value: u64, }; fun id[T](x: T): T { return x; } fun wrap[T](x: T): T { return id(x); } fun main() { val e: *Foo = Foo { value = 1u64 }; var r = wrap(e); r.value = 1u64; }",
        );
        assert!(diags.is_empty(), "{diags:?}");

        // `*Array[T]` is valid with a mutable formal.
        let (_, diags) = check_src(
            "fun get[T](a: *Array[T]): T { return a[0u64]; } fun main() { val a: *Array[u64] = [1u64]; val x = get(a); x; }",
        );
        assert!(diags.is_empty(), "{diags:?}");

        // Read-only `Array[T]` formal accepts `*Array[u64]` via downgrade.
        let (_, diags) = check_src(
            "fun first[T](a: Array[T]): T { return a[0u64]; } fun main() { val a: *Array[u64] = [1u64]; val x = first(a); x; }",
        );
        assert!(diags.is_empty(), "{diags:?}");

        // Fresh array infers through a mutable generic formal.
        let (_, diags) = check_src(
            "fun take[T](a: *Array[T]): u64 { return 1u64; } fun main() { val x = take([1u64]); x; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn generic_bounds_rechecked_after_substitution() {
        // `*Foo` does not satisfy `Numeric`.
        let (_, diags) = check_src(
            "type Foo = object { value: u64, }; fun add[T extends Numeric](a: T, b: T): T { return a + b; } fun main() { val e: *Foo = Foo { value = 1u64 }; val x = add(e, e); x; }",
        );
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("E303")),
            "{diags:?}"
        );

        // `*String` satisfies `Comparable` via its base.
        let (_, diags) = check_src(
            "fun eq[T extends Comparable](a: T, b: T): bool { return a == b; } fun f(s: *String): bool { return eq(s, s); } fun main() { val x = 1u64; x; }",
        );
        assert!(diags.is_empty(), "{diags:?}");

        // Every monomorphized instance is normalized (no `Param`/`*T` leaks).
        let (hir, typed, diags) = check_src_with_hir(
            "type Foo = object { value: u64, }; fun identity[T](x: T): T { return x; } fun main() { val e: *Foo = Foo { value = 1u64 }; val r = identity(e); r; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.validate_normalized(&hir, &diags).is_empty());
    }

    fn check_importer_with_provider(
        provider_src: &str,
        provider_module: &str,
        importer_src: &str,
    ) -> (TypedProgram, Vec<Diagnostic>) {
        let (toks, _) = vl_lex::lex(provider_src);
        let (provider, _) = vl_syntax::parse_with_module(&toks, provider_src, provider_module);
        let (interface, _) = vl_semantic::collect_interface_quiet(&provider);
        let spec = interface.as_spec();
        let (toks, _) = vl_lex::lex(importer_src);
        let (prog, mut diags) = vl_syntax::parse(&toks, importer_src);
        let (res, mut d) = vl_semantic::resolve_with_modules(&prog, std::slice::from_ref(&spec));
        diags.append(&mut d);
        let hir = vl_hir::lower(&prog, &res);
        let (typed, mut d) = check_with_modules(&hir, std::slice::from_ref(&spec));
        diags.append(&mut d);
        (typed, diags)
    }

    #[test]
    fn canonical_keys_and_collision_free_mangling() {
        // Nested arrays, mutable views, and qualified objects mangle distinctly.
        let array_u64 = Ty::Array(Box::new(Ty::U64));
        let mut_array = Ty::Mutable(Box::new(array_u64.clone()));
        assert_ne!(
            mangle("f", std::slice::from_ref(&array_u64)),
            mangle("f", std::slice::from_ref(&mut_array))
        );
        let a_person = Ty::Object("vl.a.Person".into());
        let b_person = Ty::Object("vl.b.Person".into());
        assert_ne!(mangle("f", &[a_person]), mangle("f", &[b_person]));
        assert!(mangle("f", &[Ty::String]).contains("String"));
        let key = InstanceKey::new(TemplateKey::new("demo.lib", "id"), vec![Ty::U64]);
        assert_eq!(key.mangled(), "id$u64");
    }

    #[test]
    fn imported_id_infers_and_accepts_turbofish() {
        let (typed, diags) = check_importer_with_provider(
            "fun id[T](value: T): T { return value; }",
            "demo.lib",
            "use demo.lib.id; fun main() { val a = id(1u64); val b = id::[String](\"s\"); a; b; }",
        );
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
        assert_eq!(typed.pending_imported.len(), 2);
        assert!(typed.imported_root_calls.len() == 2);
    }

    #[test]
    fn imported_call_diagnostics_at_caller() {
        for (src, code) in [
            (
                "use demo.lib.id; fun main() { id::[u64, u64](1u64); }",
                "E303",
            ),
            ("use demo.lib.id; fun main() { id(); }", "E303"),
            (
                "use demo.lib.add; fun main() { add(\"a\", \"b\"); }",
                "E303",
            ),
        ] {
            let provider = if src.contains("add") {
                "fun add[T extends Numeric](a: T, b: T): T { return a + b; }"
            } else {
                "fun id[T](value: T): T { return value; }"
            };
            let (_, diags) = check_importer_with_provider(provider, "demo.lib", src);
            assert_eq!(
                diags.iter().filter(|d| d.is_error()).count(),
                1,
                "{src}: {diags:?}"
            );
            assert_eq!(diags[0].code.as_deref(), Some(code), "{src}: {diags:?}");
        }
    }

    #[test]
    fn imported_forwarding_records_no_root_inside_generic() {
        // `wrap[T]` forwarding to imported `id[T]` stays generic (no root);
        // the world resolves it after the outer becomes concrete.
        let (typed, diags) = check_importer_with_provider(
            "fun id[T](value: T): T { return value; }",
            "demo.lib",
            "use demo.lib; fun wrap[T](x: T): T { return lib.id(x); } fun main() { wrap(1u64); }",
        );
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
        // One local root (`wrap$u64`), no imported root yet (forwarding is nested).
        assert!(typed.root_calls.len() == 1);
        assert!(typed.pending_imported.is_empty());
    }

    #[test]
    fn imported_instances_deduplicate_across_callers() {
        let (typed, diags) = check_importer_with_provider(
            "fun id[T](value: T): T { return value; }",
            "demo.lib",
            "use demo.lib.id; fun main() { val a = id(1u64); val b = id(2u64); a; b; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        // Same `id[u64]` twice: one pending key (deduplicated).
        assert_eq!(typed.pending_imported.len(), 1);
    }
}
