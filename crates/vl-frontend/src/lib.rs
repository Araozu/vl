//! vl-frontend: reusable in-memory frontend for tooling.
//!
//! The driver (`vl`) owns CLI, file I/O, and exit codes. Everything else —
//! lexing through type checking plus the monomorphization world plan —
//! runs here on `&str` input so editors, LSP servers, and test harnesses
//! can drive the exact same pipeline without shelling out.
//!
//! Two pieces:
//!
//! * [`check_text`]: full single-file frontend (lex → parse → resolve →
//!   lower → check → world plan + validation). Returns [`FrontendOk`]
//!   (resolution, HIR, typed HIR, world plan) for hover/goto-def style
//!   consumers, or the collected diagnostics on failure.
//! * [`LineIndex`] + [`render_json`]: byte [`Span`][vl_common::Span]s to
//!   1-based line/column locations and machine-readable diagnostics
//!   (`vl check --format json`). Columns count Unicode scalar values.

use serde::Serialize;
use vl_common::{Diagnostic, ModuleSpec, Span, VlType};

// ---------------------------------------------------------------------------
// Line index: byte offsets to 1-based (line, column).
// ---------------------------------------------------------------------------

/// Line table for one source text. Columns are 1-based and count Unicode
/// scalar values (not bytes, not UTF-16 units — LSP servers convert).
#[derive(Debug, Clone)]
pub struct LineIndex {
    /// Byte offset of each line's first byte. Always starts with `0`; one
    /// entry is pushed after every `\n`.
    line_starts: Vec<usize>,
}

impl LineIndex {
    pub fn new(text: &str) -> Self {
        let mut line_starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        Self { line_starts }
    }

    /// 1-based `(line, column)` for a byte `offset`. Offsets past the end
    /// clamp to the end of text; mid-character offsets snap down to the
    /// enclosing character boundary (spans produced by the lexer are
    /// always on boundaries; this is defensive for tooling callers).
    pub fn line_col(&self, text: &str, offset: usize) -> (usize, usize) {
        let offset = offset.min(text.len());
        let offset = floor_char_boundary(text, offset);
        // Number of line starts at or before `offset` is the 1-based line.
        let line = self.line_starts.partition_point(|&s| s <= offset).max(1);
        let line_start = self.line_starts[line - 1];
        let column = text[line_start..offset].chars().count() + 1;
        (line, column)
    }

    /// 1-based start/end locations for a [`Span`][vl_common::Span]. The end
    /// is exclusive (one past the last character), matching the half-open
    /// byte range.
    pub fn span_location(&self, text: &str, span: Span) -> SpanLocation {
        let (line_start, column_start) = self.line_col(text, span.start);
        let (line_end, column_end) = self.line_col(text, span.end);
        SpanLocation {
            line_start,
            column_start,
            line_end,
            column_end,
        }
    }
}

fn floor_char_boundary(text: &str, mut offset: usize) -> usize {
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

// ---------------------------------------------------------------------------
// JSON diagnostics.
// ---------------------------------------------------------------------------

/// 1-based line/column rendering of a span. Byte offsets are kept alongside
/// so machine consumers never have to re-derive them.
#[derive(Debug, Clone, Serialize)]
pub struct SpanLocation {
    pub line_start: usize,
    pub column_start: usize,
    pub line_end: usize,
    pub column_end: usize,
}

/// One span in machine-readable form.
#[derive(Debug, Clone, Serialize)]
pub struct JsonSpan {
    /// Byte offsets, half-open `[start, end)`.
    pub start: usize,
    pub end: usize,
    pub line_start: usize,
    pub column_start: usize,
    pub line_end: usize,
    pub column_end: usize,
}

/// One labelled span of a diagnostic.
#[derive(Debug, Clone, Serialize)]
pub struct JsonLabel {
    pub message: Option<String>,
    pub span: JsonSpan,
}

/// Machine-readable diagnostic. Mirrors [`Diagnostic`][vl_common::Diagnostic].
#[derive(Debug, Clone, Serialize)]
pub struct JsonDiagnostic {
    pub file: String,
    pub severity: &'static str,
    pub message: String,
    pub code: Option<String>,
    pub note: Option<String>,
    pub labels: Vec<JsonLabel>,
}

/// Top-level `vl check --format json` document.
#[derive(Debug, Clone, Serialize)]
pub struct JsonReport {
    pub ok: bool,
    pub diagnostics: Vec<JsonDiagnostic>,
}

/// Convert one file's diagnostics to machine-readable form.
pub fn collect_json(file: &str, text: &str, diags: &[Diagnostic]) -> Vec<JsonDiagnostic> {
    let index = LineIndex::new(text);
    diags
        .iter()
        .map(|d| {
            let labels = d
                .labels
                .iter()
                .map(|label| {
                    let loc = index.span_location(text, label.span);
                    JsonLabel {
                        message: label.message.clone(),
                        span: JsonSpan {
                            start: label.span.start,
                            end: label.span.end,
                            line_start: loc.line_start,
                            column_start: loc.column_start,
                            line_end: loc.line_end,
                            column_end: loc.column_end,
                        },
                    }
                })
                .collect();
            JsonDiagnostic {
                file: file.to_owned(),
                severity: match d.severity {
                    vl_common::Severity::Error => "error",
                    vl_common::Severity::Warning => "warning",
                },
                message: d.message.clone(),
                code: d.code.clone(),
                note: d.note.clone(),
                labels,
            }
        })
        .collect()
}

/// Render collected diagnostics as one JSON document. Serialization over
/// owned strings cannot fail; a failure here is a compiler bug.
pub fn render_json(diagnostics: Vec<JsonDiagnostic>) -> String {
    let ok = diagnostics.iter().all(|d| d.severity != "error");
    let report = JsonReport { ok, diagnostics };
    serde_json::to_string(&report).expect("diagnostic JSON serialization must not fail")
}

// ---------------------------------------------------------------------------
// In-memory single-file frontend.
// ---------------------------------------------------------------------------

/// Successful single-file frontend: everything an LSP-style consumer needs
/// (name resolution for goto-def, typed HIR for hover) plus the shared
/// monomorphization plan used for lowering.
pub struct FrontendOk {
    pub resolution: vl_semantic::Resolution,
    pub hir: vl_hir::HirProgram,
    pub typed: vl_typecheck::TypedProgram,
    pub plan: vl_typecheck::world::MonomorphizationPlan,
    /// Warnings produced while checking this otherwise successful source.
    pub diags: Vec<Diagnostic>,
}

/// Run the full single-file frontend on in-memory text.
///
/// * `catalog` is the merged module catalog (target natives plus stdlib
///   helper exports — the caller owns merging, exactly like the driver).
/// * `extra_world` holds already-checked immutable modules (e.g. stdlib)
///   participating in the monomorphization fixed point.
///
/// Behaviour matches `vl check` for standalone files: per-item recovery,
/// poison-don't-cascade, entrypoint signature validation, and no lowering.
/// Returns the checked artifacts or the collected diagnostics.
pub fn check_text(
    text: &str,
    module: &str,
    catalog: &[ModuleSpec],
    extra_world: &[(&vl_hir::HirProgram, &vl_typecheck::TypedProgram)],
) -> Result<FrontendOk, Vec<Diagnostic>> {
    let mut diags = Vec::new();
    let (toks, mut d) = vl_lex::lex(text);
    diags.append(&mut d);
    let (ast, mut d) = vl_syntax::parse_with_module(&toks, text, module);
    diags.append(&mut d);
    if diags.iter().any(|d| d.is_error()) {
        return Err(diags);
    }
    let (res, mut d) = vl_semantic::resolve_with_modules(&ast, catalog);
    diags.append(&mut d);
    let imports_poisoned = res.poisoned_imports;
    if !diags.iter().any(|d| d.is_error()) {
        // `main` is optional, but its signature is checked wherever it is
        // declared (same rule as the driver: zero params, infallible void).
        let mains = ast
            .items
            .iter()
            .filter_map(|item| match item {
                vl_syntax::Item::Function {
                    name,
                    params,
                    ret,
                    span,
                    ..
                } if name == "main" => Some((params.len(), ret.clone(), *span)),
                _ => None,
            })
            .collect::<Vec<_>>();
        if let Some((count, ret, span)) = mains.first() {
            if *count != 0 {
                diags.push(
                    Diagnostic::error("`main` must not take parameters")
                        .with_label(*span, "entrypoint declared here")
                        .with_code("E401"),
                );
            }
            if let Some(diag) = main_ret_error(ret, *span) {
                diags.push(diag);
            }
        }
    }
    let hir = vl_hir::lower(&ast, &res);
    // Same merged catalog as resolution: nominal types imported from
    // target modules validate like local ones.
    let (typed, mut d) = vl_typecheck::check_with_modules(&hir, catalog);
    diags.append(&mut d);
    // Boundary guard: no unresolved `int`/`Param`/nested-`Error` type may
    // reach lowering without a diagnostic. E500s here are compiler bugs.
    if !imports_poisoned {
        diags.append(&mut typed.validate_normalized(&hir, &diags));
    }
    if imports_poisoned && !diags.iter().any(Diagnostic::is_error) {
        diags.extend(blocked_import_diagnostics(&ast, catalog));
    }
    if diags.iter().any(|d| d.is_error()) {
        return Err(diags);
    }
    // Single-file world: one source module plus immutable checked modules,
    // sharing the same fixed-point machinery as projects.
    let mut world_refs: Vec<(&vl_hir::HirProgram, &vl_typecheck::TypedProgram)> =
        Vec::with_capacity(1 + extra_world.len());
    world_refs.push((&hir, &typed));
    world_refs.extend(extra_world.iter().copied());
    let (plan, world_diags) = vl_typecheck::world::plan_world(&world_refs);
    for (owner, diag) in world_diags {
        diags.push(anchor_world_diagnostic(&ast, &res, module, &owner, diag));
    }
    if diags.iter().any(|d| d.is_error()) {
        return Err(diags);
    }
    // Validate every planned instance before lowering; `check` stops
    // successfully after this validation.
    for (owner, diag) in vl_typecheck::world::validate_plan(&plan, &world_refs, &diags) {
        diags.push(anchor_world_diagnostic(&ast, &res, module, &owner, diag));
    }
    if diags.iter().any(|d| d.is_error()) {
        return Err(diags);
    }
    Ok(FrontendOk {
        resolution: res,
        hir,
        typed,
        plan,
        diags,
    })
}

fn blocked_import_diagnostics(ast: &vl_syntax::Program, catalog: &[ModuleSpec]) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for item in &ast.items {
        let vl_syntax::Item::Use { path, names, span } = item else {
            continue;
        };
        let key = path.join(".");
        let full = catalog.iter().find(|m| m.path.as_string() == key);
        let parent = path
            .get(..path.len().saturating_sub(1))
            .map(|prefix| prefix.join("."))
            .and_then(|parent_key| catalog.iter().find(|m| m.path.as_string() == parent_key));
        let poisoned = match full {
            Some(spec) => {
                spec.parse_poisoned
                    || names.as_ref().is_some_and(|names| {
                        names
                            .iter()
                            .any(|name| spec.poisoned_exports.contains(name))
                    })
            }
            None => parent.is_some_and(|spec| {
                spec.parse_poisoned
                    || path
                        .last()
                        .is_some_and(|leaf| spec.poisoned_exports.contains(leaf))
            }),
        };
        if poisoned {
            let module_name = full.or(parent).map(|m| m.path.as_string()).unwrap_or(key);
            out.push(
                Diagnostic::error(format!(
                    "import from `{module_name}` is unavailable because the provider has errors"
                ))
                .with_label(*span, "dependency is blocked here")
                .with_note("fix the provider errors before using this import")
                .with_code("E203"),
            );
        }
    }
    if out.is_empty() {
        out.push(
            Diagnostic::error("one or more imported dependencies are unavailable")
                .with_note("fix the provider errors before using imported values")
                .with_code("E203"),
        );
    }
    out
}

fn anchor_world_diagnostic(
    ast: &vl_syntax::Program,
    resolution: &vl_semantic::Resolution,
    module: &str,
    owner: &str,
    diag: Diagnostic,
) -> Diagnostic {
    if owner == module {
        return diag;
    }
    let import = ast
        .items
        .iter()
        .filter_map(|item| match item {
            vl_syntax::Item::Use { path, span, .. } => {
                let name = path.join(".");
                (owner == name || owner.starts_with(&format!("{name}.")))
                    .then_some((name.len(), *span))
            }
            _ => None,
        })
        .max_by_key(|(len, _)| *len)
        .map(|(_, span)| (span, "instantiation requested through this import"));
    let call = import.or_else(|| {
        resolution
            .defs
            .iter()
            .filter(|def| {
                matches!(
                    def.kind,
                    vl_semantic::DefKind::ImportedFunction | vl_semantic::DefKind::External
                ) && def
                    .symbol
                    .as_ref()
                    .is_some_and(|symbol| symbol.module.as_string() == owner)
            })
            .map(|def| (def.span, "instantiation requested at this call"))
            .next()
    });
    let original_ranges = diag
        .labels
        .iter()
        .map(|label| {
            label.message.as_ref().map_or_else(
                || format!("{}..{}", label.span.start, label.span.end),
                |message| format!("{}..{} ({message})", label.span.start, label.span.end),
            )
        })
        .collect::<Vec<_>>();
    let context = if original_ranges.is_empty() {
        format!("provider module `{owner}`")
    } else {
        format!(
            "provider module `{owner}`, original byte range(s) {}",
            original_ranges.join(", ")
        )
    };
    let mut mapped = match diag.severity {
        vl_common::Severity::Error => Diagnostic::error(diag.message),
        vl_common::Severity::Warning => Diagnostic::warning(diag.message),
    };
    if let Some((span, label)) = call {
        mapped = mapped.with_label(span, label);
    }
    if let Some(code) = diag.code {
        mapped = mapped.with_code(code);
    }
    let note = match diag.note {
        Some(note) => format!("{context}; {note}"),
        None => context,
    };
    mapped.with_note(note)
}

/// Entrypoint return check: `main` is infallible by definition (the VM
/// loader owns startup failure). A fallible `main` gets its own E401;
/// any other non-`void` return gets the classic one. `None` (already
/// reported) stays quiet.
fn main_ret_error(ret: &Option<VlType>, span: Span) -> Option<Diagnostic> {
    match ret {
        None | Some(VlType::Void) => None,
        Some(VlType::Fallible { .. }) => Some(
            Diagnostic::error("`main` cannot be fallible")
                .with_label(span, "entrypoint declared here")
                .with_note("handle errors inside `main` (e.g. `catch`) instead")
                .with_code("E401"),
        ),
        _ => Some(
            Diagnostic::error("`main` must return `void`")
                .with_label(span, "entrypoint declared here")
                .with_note("omit the return type (it defaults to `void`)")
                .with_code("E401"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_catalog() -> Vec<ModuleSpec> {
        Vec::new()
    }

    #[test]
    fn line_index_counts_lines_and_char_columns() {
        let text = "ab\ncéf\n";
        let index = LineIndex::new(text);
        assert_eq!(index.line_col(text, 0), (1, 1));
        assert_eq!(index.line_col(text, 3), (2, 1));
        // `é` is two bytes but one column.
        assert_eq!(index.line_col(text, 4), (2, 2));
        assert_eq!(index.line_col(text, 6), (2, 3));
        assert_eq!(index.line_col(text, 7), (2, 4));
        assert_eq!(index.line_col(text, 999), (3, 1));
    }

    #[test]
    fn span_location_maps_both_ends() {
        let text = "val x = 1;\n";
        let index = LineIndex::new(text);
        let loc = index.span_location(text, Span::new(4, 5));
        assert_eq!(
            (
                loc.line_start,
                loc.column_start,
                loc.line_end,
                loc.column_end
            ),
            (1, 5, 1, 6)
        );
    }

    #[test]
    fn check_text_accepts_clean_snippet() {
        let ok = match check_text("fun main() {}", "test", &empty_catalog(), &[]) {
            Ok(ok) => ok,
            Err(diags) => panic!("clean snippet must check: {diags:?}"),
        };
        assert_eq!(ok.hir.items.len(), 1);
        assert!(ok.diags.is_empty());
    }

    #[test]
    fn successful_result_preserves_warnings() {
        let ok = check_text(
            "fun main() { val x = 1u64; val x = 2u64; x; }",
            "test",
            &empty_catalog(),
            &[],
        )
        .expect("shadow warning must not fail checking");
        assert!(ok
            .diags
            .iter()
            .any(|d| d.severity == vl_common::Severity::Warning));
    }

    #[test]
    fn poisoned_dependency_is_a_blocked_failure_without_cascades() {
        let mut broken = ModuleSpec::new(&["broken"], &[]);
        broken.parse_poisoned = true;
        let result = check_text("use broken.{f}; fun main() { f(); }", "app", &[broken], &[]);
        let Err(diags) = result else {
            panic!("poisoned import must block successful artifacts")
        };
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("E203")),
            "{diags:?}"
        );
        assert!(
            !diags.iter().any(|d| d.code.as_deref() == Some("E500")),
            "{diags:?}"
        );
    }

    #[test]
    fn clean_exact_child_import_is_not_poisoned_by_parent_module() {
        let mut parent = ModuleSpec::new(&["parent"], &[]);
        parent.parse_poisoned = true;
        let child = ModuleSpec::new(&["parent", "child"], &[]);
        let result = check_text(
            "use parent.child; use broken.{f}; fun main() { f(); }",
            "app",
            &[parent, child, {
                let mut broken = ModuleSpec::new(&["broken"], &[]);
                broken.parse_poisoned = true;
                broken
            }],
            &[],
        );
        let Err(diags) = result else {
            panic!("the broken import should block success")
        };
        let blocked = diags
            .iter()
            .filter(|diag| diag.code.as_deref() == Some("E203"))
            .collect::<Vec<_>>();
        assert_eq!(blocked.len(), 1, "{diags:?}");
        assert!(blocked[0].message.contains("`broken`"), "{blocked:?}");
    }

    #[test]
    fn foreign_world_error_is_anchored_to_local_import_with_provider_context() {
        let provider_src = "// provider padding to make foreign spans distinct\n\nfun grow[T](x: T) { grow([x]); }";
        let (tokens, _) = vl_lex::lex(provider_src);
        let (provider_ast, parse_diags) =
            vl_syntax::parse_with_module(&tokens, provider_src, "lib");
        assert!(parse_diags.is_empty(), "{parse_diags:?}");
        let (interface, interface_diags) = vl_semantic::collect_interface(&provider_ast);
        assert!(interface_diags.is_empty(), "{interface_diags:?}");
        let (provider_resolution, resolution_diags) =
            vl_semantic::resolve_with_modules(&provider_ast, &[]);
        assert!(resolution_diags.is_empty(), "{resolution_diags:?}");
        let provider_hir = vl_hir::lower(&provider_ast, &provider_resolution);
        let (provider_typed, type_diags) = vl_typecheck::check_with_modules(&provider_hir, &[]);
        assert!(type_diags.is_empty(), "{type_diags:?}");

        let caller = "use lib.{grow}; fun caller() { grow(1u64); }";
        let catalog = vec![interface.as_spec()];
        let result = check_text(caller, "app", &catalog, &[(&provider_hir, &provider_typed)]);
        let Err(diags) = result else {
            panic!("polymorphic recursion must fail planning")
        };
        let diag = diags
            .iter()
            .find(|d| d.code.as_deref() == Some("E303"))
            .expect("world planner diagnostic");
        let import_start = caller.find("use lib").expect("import");
        assert_eq!(diag.labels[0].span.start, import_start);
        assert!(diag
            .note
            .as_deref()
            .is_some_and(|note| note.contains("provider module `lib`")
                && note.contains("original byte range")));
        let json = collect_json("app.vl", caller, std::slice::from_ref(diag));
        assert_eq!(
            json[0].labels[0].span.end,
            caller.find(';').expect("semicolon") + 1
        );
    }

    #[test]
    fn check_text_reports_parse_errors() {
        let err = match check_text("val x = 1", "test", &empty_catalog(), &[]) {
            Ok(_) => panic!("missing semicolon must fail"),
            Err(diags) => diags,
        };
        assert!(err.iter().any(|d| d.is_error()));
    }

    #[test]
    fn check_text_validates_main_signature() {
        let err = match check_text("fun main(a: u64) { a; }", "test", &empty_catalog(), &[]) {
            Ok(_) => panic!("main with params must fail"),
            Err(diags) => diags,
        };
        assert!(err.iter().any(|d| d.code.as_deref() == Some("E401")));
    }

    #[test]
    fn json_report_marks_errors_and_carries_spans() {
        let diags = match check_text("val x = @;", "test", &empty_catalog(), &[]) {
            Ok(_) => panic!("lexer error must fail"),
            Err(diags) => diags,
        };
        let json = collect_json("test.vl", "val x = @;", &diags);
        let rendered = render_json(json);
        let value: serde_json::Value =
            serde_json::from_str(&rendered).expect("report must be valid JSON");
        assert_eq!(value["ok"], false);
        let first = &value["diagnostics"][0];
        assert_eq!(first["file"], "test.vl");
        assert_eq!(first["severity"], "error");
        assert_eq!(first["labels"][0]["span"]["line_start"], 1);
        assert_eq!(first["labels"][0]["span"]["column_start"], 9);
    }

    #[test]
    fn json_report_ok_when_clean() {
        let rendered = render_json(Vec::new());
        let value: serde_json::Value =
            serde_json::from_str(&rendered).expect("report must be valid JSON");
        assert_eq!(value["ok"], true);
        assert_eq!(value["diagnostics"].as_array().unwrap().len(), 0);
    }
}
