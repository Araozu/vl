//! vl-common: shared primitives for the whole compiler pipeline.
//!
//! Everything downstream depends on this crate and nothing else:
//! byte [`Span`]s, the [`Sources`] file table, and [`Diagnostic`]s that
//! render through [`ariadne`](https://crates.io/crates/ariadne).
//!
//! Hard rule: all user-facing errors MUST go through [`Diagnostic`]
//! so rendering stays uniform. No `miette`, no hand-rolled caret code.

pub mod diagnostic;
pub mod module;
pub mod source;
pub mod ty;

/// Scalar values shared by the frontend and target-neutral IR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scalar {
    U64(u64),
    I64(i64),
    F64(u64),
    Bool(bool),
    U8(u8),
}
pub mod span;

pub use diagnostic::{Diagnostic, Label, Severity};
pub use module::{Export, ExportDecl, FuncSig, ModulePath, ModuleSpec, ParamSig};
pub use source::{FileId, Source, Sources};
pub use span::Span;
pub use ty::{ParseTyError, VlType};

/// Convenience alias: pipeline stages return values plus diagnostics
/// instead of failing fast, so one run can surface many errors.
pub type Diags = Vec<Diagnostic>;
