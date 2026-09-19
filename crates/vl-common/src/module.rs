//! Shared names for source and target modules.

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleSpec {
    pub path: ModulePath,
    pub exports: Vec<String>,
}

impl ModuleSpec {
    pub fn new(path: &[&str], exports: &[&str]) -> Self {
        Self {
            path: ModulePath::new(path.iter().map(|s| (*s).into()).collect()),
            exports: exports.iter().map(|s| (*s).into()).collect(),
        }
    }
}
