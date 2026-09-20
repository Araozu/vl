//! Shared names and compiler-owned signatures for source and target modules.
//!
//! The catalog is where the language's type surface is *declared*: every
//! export carries its parameter names, parameter types, and return type as
//! [`VlType`]s. Name resolution and typechecking both consume this catalog,
//! so imports and checked calls cannot drift apart from the docs.

use crate::ty::VlType;

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

/// Compiler-owned signature of one exported function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuncSig {
    pub params: Vec<ParamSig>,
    pub ret: VlType,
}

impl FuncSig {
    pub fn new(params: &[(&str, VlType)], ret: VlType) -> Self {
        Self {
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
}

/// One exported function: its name plus its signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    pub name: String,
    pub sig: FuncSig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleSpec {
    pub path: ModulePath,
    pub exports: Vec<Export>,
    pub generic_exports: Vec<String>,
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
    pub generic_functions: Vec<String>,
    pub parse_poisoned: bool,
    pub poisoned_exports: Vec<String>,
    pub global_dependent_exports: Vec<String>,
}

impl ModuleInterface {
    pub fn as_spec(&self) -> ModuleSpec {
        ModuleSpec {
            path: self.path.clone(),
            exports: self.functions.clone(),
            generic_exports: self.generic_functions.clone(),
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
                .map(|(name, params, ret)| Export {
                    name: (*name).into(),
                    sig: FuncSig::new(params, ret.clone()),
                })
                .collect(),
            generic_exports: Vec::new(),
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
