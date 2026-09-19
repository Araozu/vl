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
                    ty: *t,
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
                    sig: FuncSig::new(params, *ret),
                })
                .collect(),
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
