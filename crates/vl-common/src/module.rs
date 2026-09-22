//! Shared names and compiler-owned signatures for source and target modules.
//!
//! The catalog is where the language's type surface is *declared*: every
//! export carries its parameter names, parameter types, and return type as
//! [`VlType`]s. Name resolution and typechecking both consume this catalog,
//! so imports and checked calls cannot drift apart from the docs.

use crate::ty::{GenericBound, VlType};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModulePath(pub Vec<String>);

impl ModulePath {
    pub fn new(segments: Vec<String>) -> Self {
        Self(segments)
    }

    pub fn as_string(&self) -> String {
        self.0.join(".")
    }

    pub fn from_dotted(path: &str) -> Self {
        Self(path.split('.').map(str::to_owned).collect())
    }

    pub fn segments(&self) -> &[String] {
        &self.0
    }

    pub fn leaf(&self) -> Option<&str> {
        self.0.last().map(String::as_str)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SymbolRef {
    pub module: ModulePath,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleOrigin {
    Source,
    Target,
}

/// One named parameter of an exported function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamSig {
    pub name: String,
    pub ty: VlType,
}

/// One generic type parameter with its optional bound (`T`, `T extends Numeric`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TypeParamSig {
    pub name: String,
    pub bound: Option<GenericBound>,
}

/// Compiler-owned signature of one exported function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuncSig {
    pub type_params: Vec<TypeParamSig>,
    pub params: Vec<ParamSig>,
    pub ret: VlType,
}

impl FuncSig {
    pub fn new(params: &[(&str, VlType)], ret: VlType) -> Self {
        Self {
            type_params: Vec::new(),
            params: params
                .iter()
                .map(|(n, t)| ParamSig {
                    name: (*n).into(),
                    ty: t.clone(),
                })
                .collect(),
            ret,
        }
    }

    pub fn generic(type_params: Vec<TypeParamSig>, params: Vec<ParamSig>, ret: VlType) -> Self {
        Self {
            type_params,
            params,
            ret,
        }
    }

    pub fn is_generic(&self) -> bool {
        !self.type_params.is_empty()
    }
}

/// Whether an export has a source body (monomorphization template) or is a
/// target-native callable. A merged path such as `std.math` contains both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExportKind {
    Source,
    Target,
}

/// One exported function: its name plus its signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    pub name: String,
    pub sig: FuncSig,
    pub kind: ExportKind,
}

impl Export {
    pub fn source(name: String, sig: FuncSig) -> Self {
        Self {
            name,
            sig,
            kind: ExportKind::Source,
        }
    }

    pub fn target(name: String, sig: FuncSig) -> Self {
        Self {
            name,
            sig,
            kind: ExportKind::Target,
        }
    }

    pub fn is_generic(&self) -> bool {
        self.sig.is_generic()
    }

    pub fn is_source(&self) -> bool {
        self.kind == ExportKind::Source
    }
}

/// Identity of one generic template: its owning module plus its function name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TemplateKey {
    pub module: String,
    pub function: String,
}

impl TemplateKey {
    pub fn new(module: impl Into<String>, function: impl Into<String>) -> Self {
        Self {
            module: module.into(),
            function: function.into(),
        }
    }
}

impl std::fmt::Display for TemplateKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}::{}", self.module, self.function)
    }
}

/// One exported object type: its short name, its fully qualified identity
/// (`<module>.<name>`), its field layouts, and its associated functions.
/// Object identity is nominal and qualified so two modules may each define a
/// `Person` without collision;
///
/// importers name the type `vl.person.Person`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectExport {
    pub name: String,
    pub qualified: String,
    pub fields: Vec<ObjectFieldSig>,
    /// Associated functions declared inside the object body, each exported
    /// under its short method name (`init` in `Counter.init`). Signatures
    /// qualify local object references exactly like fields, so importers
    /// resolve nominal identity without the provider's scope. Entries are
    /// `Source` exports (they have bodies); generic methods carry type params.
    pub methods: Vec<Export>,
}

impl ObjectExport {
    /// Look up one associated function by its short method name.
    pub fn lookup_method(&self, name: &str) -> Option<&Export> {
        self.methods.iter().find(|m| m.name == name)
    }
}

/// One exported object field: its name plus its declared type. Inner object
/// references are stored fully qualified (`vl.person.Person`, never bare
/// `Person`) so importers resolve them without the provider's scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectFieldSig {
    pub name: String,
    pub ty: VlType,
}

/// One declaration-only union variant exported through a module interface.
/// Payloads retain their source-level nominal type spellings so importers can
/// validate qualified references without inventing a runtime representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnionVariantSig {
    pub name: String,
    pub payload: Vec<VlType>,
}

/// Exported nominal union metadata. Unlike [`ObjectExport`], this carries no
/// layout or associated-method namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnionExport {
    pub name: String,
    pub qualified: String,
    pub type_params: Vec<TypeParamSig>,
    pub variants: Vec<UnionVariantSig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleSpec {
    pub path: ModulePath,
    pub exports: Vec<Export>,
    /// Exported object layouts, keyed by disambiguation through `qualified`.
    pub objects: Vec<ObjectExport>,
    /// Exported nominal unions. Unions have no object layout or methods.
    pub unions: Vec<UnionExport>,
    /// The provider had lexer/parser errors, so recovered AST omissions are
    /// not reliable evidence that an export does not exist.
    pub parse_poisoned: bool,
    /// Names known to exist but unavailable across the module boundary.
    /// Keeping these names prevents importers from misreporting E203.
    pub poisoned_exports: Vec<String>,
    /// Exports that are valid locally but cannot be called across modules
    /// because their implementation depends on this module's globals.
    pub global_dependent_exports: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleInterface {
    pub path: ModulePath,
    pub origin: ModuleOrigin,
    pub functions: Vec<Export>,
    pub objects: Vec<ObjectExport>,
    pub unions: Vec<UnionExport>,
    pub parse_poisoned: bool,
    pub poisoned_exports: Vec<String>,
    pub global_dependent_exports: Vec<String>,
}

impl ModuleInterface {
    pub fn as_spec(&self) -> ModuleSpec {
        ModuleSpec {
            path: self.path.clone(),
            exports: self.functions.clone(),
            objects: self.objects.clone(),
            unions: self.unions.clone(),
            parse_poisoned: self.parse_poisoned,
            poisoned_exports: self.poisoned_exports.clone(),
            global_dependent_exports: self.global_dependent_exports.clone(),
        }
    }
}

/// `(&str, params, ret)` per export; `params` is `&[(name, type)]`.
pub type ExportDecl<'a> = (&'a str, &'a [(&'a str, VlType)], VlType);

impl ModuleSpec {
    pub fn new(path: &[&str], exports: &[ExportDecl<'_>]) -> Self {
        Self {
            path: ModulePath::new(path.iter().map(|s| (*s).into()).collect()),
            exports: exports
                .iter()
                .map(|(name, params, ret)| {
                    Export::target((*name).into(), FuncSig::new(params, ret.clone()))
                })
                .collect(),
            objects: Vec::new(),
            unions: Vec::new(),
            parse_poisoned: false,
            poisoned_exports: Vec::new(),
            global_dependent_exports: Vec::new(),
        }
    }

    pub fn new_source(path: &[&str], exports: Vec<Export>) -> Self {
        Self {
            path: ModulePath::new(path.iter().map(|s| (*s).into()).collect()),
            exports,
            objects: Vec::new(),
            unions: Vec::new(),
            parse_poisoned: false,
            poisoned_exports: Vec::new(),
            global_dependent_exports: Vec::new(),
        }
    }

    /// Look up one export by name.
    pub fn lookup(&self, name: &str) -> Option<&Export> {
        self.exports.iter().find(|e| e.name == name)
    }

    /// Export names only (resolution fast path / diagnostics).
    pub fn export_names(&self) -> impl Iterator<Item = &str> {
        self.exports.iter().map(|e| e.name.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ty::GenericBound;

    #[test]
    fn monomorphic_and_generic_signatures_with_bounds() {
        let mono = FuncSig::new(&[("a", VlType::U64)], VlType::U64);
        assert!(!mono.is_generic());
        let generic = FuncSig::generic(
            vec![TypeParamSig {
                name: "T".into(),
                bound: Some(GenericBound::Numeric),
            }],
            vec![ParamSig {
                name: "a".into(),
                ty: VlType::Param("T".into()),
            }],
            VlType::Param("T".into()),
        );
        assert!(generic.is_generic());
        assert_eq!(generic.type_params[0].bound, Some(GenericBound::Numeric));
    }

    #[test]
    fn source_and_target_exports_after_merge() {
        let mut catalog = [ModuleSpec::new(
            &["std", "math"],
            &[("mod_u64", &[("a", VlType::U64)], VlType::U64)],
        )];
        assert_eq!(catalog[0].exports[0].kind, ExportKind::Target);
        let helper = Export::source(
            "max".into(),
            FuncSig::generic(
                vec![TypeParamSig {
                    name: "T".into(),
                    bound: Some(GenericBound::Numeric),
                }],
                vec![],
                VlType::Param("T".into()),
            ),
        );
        assert!(helper.is_generic());
        assert!(helper.is_source());
        catalog[0].exports.push(helper);
        let native = catalog[0].lookup("mod_u64").expect("native");
        let generic = catalog[0].lookup("max").expect("helper");
        assert_eq!(native.kind, ExportKind::Target);
        assert_eq!(generic.kind, ExportKind::Source);
    }

    #[test]
    fn interface_spec_round_trip_preserves_union_metadata() {
        let interface = ModuleInterface {
            path: ModulePath::from_dotted("demo.types"),
            origin: ModuleOrigin::Source,
            functions: Vec::new(),
            objects: Vec::new(),
            unions: vec![UnionExport {
                name: "Option".into(),
                qualified: "demo.types.Option".into(),
                type_params: vec![TypeParamSig {
                    name: "T".into(),
                    bound: None,
                }],
                variants: vec![UnionVariantSig {
                    name: "Some".into(),
                    payload: vec![VlType::Param("T".into())],
                }],
            }],
            parse_poisoned: false,
            poisoned_exports: Vec::new(),
            global_dependent_exports: Vec::new(),
        };
        let spec = interface.as_spec();
        assert_eq!(spec.unions, interface.unions);
    }
}
