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
pub mod span;

pub use diagnostic::{Diagnostic, Label, Severity};
pub use module::{ModulePath, ModuleSpec};
pub use source::{FileId, Source, Sources};
pub use span::Span;

/// Convenience alias: pipeline stages return values plus diagnostics
/// instead of failing fast, so one run can surface many errors.
pub type Diags = Vec<Diagnostic>;
