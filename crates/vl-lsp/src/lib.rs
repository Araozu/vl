//! vl-lsp: Language Server Protocol support for VL (tooling layer).
//!
//! A stdio LSP server over the exact same pipeline the driver runs:
//! in-memory [`vl_frontend::check_text`] (lex → parse → resolve → lower →
//! check → world plan) for diagnostics, plus the [`vl_semantic::Resolution`]
//! for hover and goto-definition, [`vl_syntax`] for document symbols,
//! [`vl_fmt`] for formatting, and a keyword + scope-name completion.
//!
//! Layering: the pure analysis helpers ([`analyze`], [`hover_at`],
//! [`definition_at`], [`document_symbols`], [`completion_at`],
//! [`format_document`], position conversion) take an already-merged module
//! catalog, exactly like [`vl_frontend::check_text`]. Only [`run_stdio`]
//! builds the default catalog (target natives via [`vl_codegen`] plus the
//! embedded stdlib, mirroring the driver) and owns process I/O.

use std::collections::HashMap;
use std::error::Error;

use crossbeam_channel::Sender;
use lsp_server::{Connection, Message, Notification, Request};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument,
    Notification as _, PublishDiagnostics,
};
use lsp_types::request::{Request as _, Shutdown};
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionList, CompletionParams, CompletionResponse,
    DiagnosticSeverity, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DidSaveTextDocumentParams, DocumentFormattingParams,
    DocumentSymbolParams, DocumentSymbolResponse, GotoDefinitionParams, GotoDefinitionResponse,
    Hover, HoverContents, HoverParams, InitializeParams, Location, MarkupContent, MarkupKind,
    Position, PublishDiagnosticsParams, Range, ServerCapabilities, SymbolKind,
    TextDocumentSyncKind, Url,
};
use vl_common::{Diagnostic, FuncSig, ModuleSpec, Severity, Span};
use vl_frontend::FrontendOk;
use vl_semantic::{Def, DefKind, Resolution};
use vl_syntax::{Item, Program};
use vl_typecheck::FuncSigTy;

// ---------------------------------------------------------------------------
// Catalog (tooling default, mirrors the driver).
// ---------------------------------------------------------------------------

/// Embedded standard library, loaded once (same arrangement as the driver).
fn stdlib() -> &'static vl_stdlib::Stdlib {
    static STDLIB: std::sync::OnceLock<vl_stdlib::Stdlib> = std::sync::OnceLock::new();
    STDLIB.get_or_init(vl_stdlib::load)
}

/// Default module catalog: target natives plus embedded stdlib helper
/// exports. Mirrors `vl check` for standalone files.
fn default_catalog() -> Vec<ModuleSpec> {
    let mut catalog = vl_codegen::modules();
    stdlib().extend_catalog(&mut catalog);
    catalog
}

/// Immutable checked stdlib modules as world refs for
/// [`vl_frontend::check_text`]. Mirrors the driver.
fn stdlib_world_refs() -> Vec<(
    &'static vl_hir::HirProgram,
    &'static vl_typecheck::TypedProgram,
)> {
    stdlib()
        .checked_modules()
        .iter()
        .map(|(hir, typed)| (hir, typed))
        .collect()
}

/// Module name for a document URI: the file stem (`/a/b/arith.vl` →
/// `arith`), falling back to `stdin` for unsavable URIs. Mirrors the
/// driver's `source_module`, minus filesystem access.
pub fn module_for_uri(uri: &Url) -> String {
    if uri.scheme() != "file" {
        return "stdin".to_owned();
    }
    let path = uri.path();
    let file = path.rsplit('/').next().unwrap_or("");
    let stem = file.strip_suffix(".vl").unwrap_or(file);
    if stem.is_empty() {
        "stdin".to_owned()
    } else {
        stem.to_owned()
    }
}

// ---------------------------------------------------------------------------
// Analysis.
// ---------------------------------------------------------------------------

/// One open document's checked state: the parsed program (the parser
/// recovers per item, so this is present even when checking fails — for
/// symbols/completion), the full frontend result, and a best-effort
/// resolution re-run over the recovered program so hover/goto-definition
/// keep working mid-edit.
pub struct Analysis {
    pub text: String,
    pub module: String,
    pub catalog: Vec<ModuleSpec>,
    pub program: Program,
    pub frontend: Result<FrontendOk, Vec<Diagnostic>>,
    /// Name resolution for hover/goto-definition. Re-resolved over the
    /// recovered program (cheap, pure), so it exists even when `frontend`
    /// is an error; resolution diagnostics are dropped here because the
    /// frontend already reported them.
    pub resolution: Resolution,
    /// Diagnostics to publish: frontend errors, or lex/parse errors when the
    /// file never reaches checking.
    pub diags: Vec<Diagnostic>,
}

/// Run the full in-memory frontend over one document's text.
pub fn analyze(text: &str, module: &str, catalog: &[ModuleSpec]) -> Analysis {
    let extra = stdlib_world_refs();
    let frontend = vl_frontend::check_text(text, module, catalog, &extra);
    // The parsed program is recovered independently so symbols and
    // completion survive check failures. Parse diagnostics are already
    // inside the frontend error; this copy is only structural.
    let (tokens, _) = vl_lex::lex(text);
    let (program, _) = vl_syntax::parse_with_module(&tokens, text, module);
    // The frontend owns a resolution on success, but it is moved into the
    // `FrontendOk`; re-resolve uniformly instead (cheap, pure) so hover and
    // goto-definition share one path for clean and broken files alike.
    let resolution = best_effort_resolution(&program, catalog);
    let diags = match &frontend {
        Ok(ok) => ok.diags.clone(),
        Err(diags) => diags.clone(),
    };
    Analysis {
        text: text.to_owned(),
        module: module.to_owned(),
        catalog: catalog.to_vec(),
        program,
        frontend: match frontend {
            Ok(ok) => Ok(ok),
            Err(diags) => Err(diags),
        },
        resolution,
        diags,
    }
}

/// Best-effort name resolution over a parsed program, ignoring the returned
/// diagnostics (already reported or about to be).
fn best_effort_resolution(program: &Program, catalog: &[ModuleSpec]) -> Resolution {
    let (resolution, _) = vl_semantic::resolve_with_modules(program, catalog);
    resolution
}

/// Analyze with the default catalog (target natives + stdlib).
pub fn analyze_default(text: &str, module: &str) -> Analysis {
    let catalog = default_catalog();
    analyze(text, module, &catalog)
}

// ---------------------------------------------------------------------------
// Positions: byte offsets/spans (1-based scalar columns) <-> LSP (UTF-16).
// ---------------------------------------------------------------------------

/// Line table over the LSP line endings (`\r\n`, `\r`, `\n`). `starts[i]`
/// is the first byte of line `i`; `ends[i]` is one past its last content
/// byte (line-break bytes excluded, so columns never count a `\r`).
#[derive(Debug)]
struct LineTable {
    starts: Vec<usize>,
    ends: Vec<usize>,
}

impl LineTable {
    fn new(text: &str) -> Self {
        let bytes = text.as_bytes();
        let mut starts = vec![0];
        let mut ends = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'\r' if bytes.get(i + 1) == Some(&b'\n') => {
                    ends.push(i);
                    starts.push(i + 2);
                    i += 2;
                }
                b'\r' | b'\n' => {
                    ends.push(i);
                    starts.push(i + 1);
                    i += 1;
                }
                _ => i += 1,
            }
        }
        ends.push(text.len());
        Self { starts, ends }
    }

    fn line_count(&self) -> usize {
        self.starts.len()
    }
}

/// UTF-16 code units in `s` (LSP columns count these, not bytes or scalars).
fn utf16_len(s: &str) -> usize {
    s.chars().map(|c| c.len_utf16()).sum()
}

/// Clamp `offset` into `text` and snap down to a character boundary.
fn clamp_offset(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// Byte `offset` → LSP position (0-based line, UTF-16 character).
pub fn offset_to_position(text: &str, offset: usize) -> Position {
    let offset = clamp_offset(text, offset);
    let table = LineTable::new(text);
    let line = table.starts.partition_point(|&s| s <= offset).max(1) - 1;
    let character = utf16_len(&text[table.starts[line]..offset]);
    Position {
        line: line as u32,
        character: character as u32,
    }
}

/// LSP position → byte offset. Out-of-range lines clamp to EOF and
/// out-of-range characters clamp to the line end; mid-character positions
/// snap down to the boundary.
pub fn position_to_offset(text: &str, pos: Position) -> usize {
    let table = LineTable::new(text);
    if (pos.line as usize) >= table.line_count() {
        return text.len();
    }
    let line = pos.line as usize;
    let line_start = table.starts[line];
    let line_end = table.ends[line];
    let mut offset = line_start;
    let mut units = 0u32;
    for ch in text[line_start..line_end].chars() {
        let w = ch.len_utf16() as u32;
        if units + w > pos.character {
            break;
        }
        units += w;
        offset += ch.len_utf8();
    }
    offset
}

/// Byte [`Span`] → LSP range (end is exclusive, matching the half-open span).
pub fn span_to_range(text: &str, span: Span) -> Range {
    Range {
        start: offset_to_position(text, span.start),
        end: offset_to_position(text, span.end),
    }
}

// ---------------------------------------------------------------------------
// Diagnostics.
// ---------------------------------------------------------------------------

fn severity(severity: Severity) -> DiagnosticSeverity {
    match severity {
        Severity::Error => DiagnosticSeverity::ERROR,
        Severity::Warning => DiagnosticSeverity::WARNING,
    }
}

/// One VL diagnostic → one LSP diagnostic. The range covers the first label's
/// span; label-less diagnostics (driver-level) land on the document start.
/// The code, note, and remaining labels fold into the message so nothing is
/// lost on the wire.
pub fn diagnostic_to_lsp(text: &str, diag: &Diagnostic) -> lsp_types::Diagnostic {
    let range = diag
        .labels
        .first()
        .map(|label| span_to_range(text, label.span))
        .unwrap_or_else(|| Range {
            start: Position {
                line: 0,
                character: 0,
            },
            end: Position {
                line: 0,
                character: 0,
            },
        });
    let mut message = diag.message.clone();
    if let Some(code) = &diag.code {
        message = format!("[{code}] {message}");
    }
    for label in &diag.labels {
        if let Some(note) = &label.message {
            message.push_str(&format!("\nnote: {note}"));
        }
    }
    // Secondary labels point at related code; keep them visible.
    for label in diag.labels.iter().skip(1) {
        let loc = span_to_range(text, label.span);
        message.push_str(&format!(
            "\nsee {}:{}",
            loc.start.line + 1,
            loc.start.character + 1
        ));
    }
    if let Some(note) = &diag.note {
        message.push_str(&format!("\n{note}"));
    }
    lsp_types::Diagnostic {
        range,
        severity: Some(severity(diag.severity)),
        code: diag.code.clone().map(lsp_types::NumberOrString::String),
        source: Some("vl".to_owned()),
        message,
        ..Default::default()
    }
}

/// All publishable diagnostics for one analyzed document.
pub fn diagnostics_for(analysis: &Analysis) -> Vec<lsp_types::Diagnostic> {
    analysis
        .diags
        .iter()
        .map(|d| diagnostic_to_lsp(&analysis.text, d))
        .collect()
}

// ---------------------------------------------------------------------------
// Hover + goto-definition.
// ---------------------------------------------------------------------------

/// A resolved name at the cursor: its [`Def`] plus the span to highlight
/// (the use site when reached through a use, the def site otherwise).
pub struct CursorDef<'a> {
    pub def: &'a Def,
    pub highlight: Span,
}

fn module_alias_import_span(program: &Program, alias: &str) -> Option<Span> {
    program.items.iter().find_map(|item| match item {
        Item::Use { path, names, span }
            if names
                .as_ref()
                .is_none_or(|names| names.iter().any(|name| name == "self"))
                && path.last().is_some_and(|leaf| leaf == alias) =>
        {
            Some(*span)
        }
        _ => None,
    })
}

/// Find the definition under `offset`: the narrowest use-span containing it,
/// else the def-site span containing it.
pub fn def_at_offset<'a>(resolution: &'a Resolution, offset: usize) -> Option<CursorDef<'a>> {
    let mut best: Option<(Span, &Def)> = None;
    for ((start, end), id) in &resolution.uses {
        let contains = (*start <= offset && offset < *end) || (*start == *end && offset == *start);
        if contains {
            let span = Span::new(*start, *end);
            let smaller = best.as_ref().is_none_or(|(s, _)| span.len() < s.len());
            if smaller {
                if let Some(def) = resolution.defs.iter().find(|d| d.id == *id) {
                    best = Some((span, def));
                }
            }
        }
    }
    if let Some((highlight, def)) = best {
        return Some(CursorDef { def, highlight });
    }
    resolution
        .defs
        .iter()
        .filter(|d| d.span.start <= offset && offset < d.span.end.max(d.span.start + 1))
        .min_by_key(|d| d.span.len())
        .map(|def| CursorDef {
            def,
            highlight: def.span,
        })
}

fn def_kind_label(def: &Def) -> String {
    match def.kind {
        DefKind::Parameter => "parameter".to_owned(),
        DefKind::Local => match def.binding {
            Some(vl_syntax::BindingKind::Var) => "var".to_owned(),
            Some(vl_syntax::BindingKind::Val) => "val".to_owned(),
            None => "function".to_owned(),
        },
        DefKind::External => "external function".to_owned(),
        DefKind::ImportedFunction => "imported function".to_owned(),
        DefKind::ModuleAlias => "module".to_owned(),
    }
}

fn format_shared_sig(name: &str, sig: &FuncSig) -> String {
    let params = sig
        .params
        .iter()
        .map(|p| format!("{}: {}", p.name, p.ty))
        .collect::<Vec<_>>()
        .join(", ");
    let generics = if sig.type_params.is_empty() {
        String::new()
    } else {
        format!(
            "[{}]",
            sig.type_params
                .iter()
                .map(|p| p.name.clone())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    format!("fun {name}{generics}({params}): {}", sig.ret)
}

fn format_typed_sig(name: &str, sig: &FuncSigTy) -> String {
    let params = sig
        .param_names
        .iter()
        .zip(sig.param_tys.iter())
        .map(|(n, t)| format!("{n}: {t}"))
        .collect::<Vec<_>>()
        .join(", ");
    let generics = if sig.type_params.is_empty() {
        String::new()
    } else {
        format!("[{}]", sig.type_params.join(", "))
    };
    format!("fun {name}{generics}({params}): {}", sig.ret)
}

/// Markdown hover for the name under `offset`, or `None` when the cursor is
/// not on a known name. Returns the contents plus the range to highlight.
pub fn hover_at(analysis: &Analysis, offset: usize) -> Option<(String, Range)> {
    let cursor = def_at_offset(&analysis.resolution, offset)?;
    let def = cursor.def;
    let headline = if matches!(def.kind, DefKind::Local)
        && analysis
            .frontend
            .as_ref()
            .ok()
            .and_then(|ok| ok.typed.func_sigs.get(&def.id.0))
            .is_some()
    {
        let ok = analysis.frontend.as_ref().ok()?;
        let sig = ok.typed.func_sigs.get(&def.id.0)?;
        format_typed_sig(&def.name, sig)
    } else if let Some(sig) = &def.sig {
        format_shared_sig(&def.name, sig)
    } else {
        format!("{} {}", def_kind_label(def), def.name)
    };
    let defined_at = match def.kind {
        DefKind::Local | DefKind::Parameter => Some(def.span),
        DefKind::ModuleAlias => module_alias_import_span(&analysis.program, &def.name),
        DefKind::External | DefKind::ImportedFunction => None,
    };
    let location = defined_at
        .map(|span| {
            let line = offset_to_position(&analysis.text, span.start).line + 1;
            format!(" · defined at line {line}")
        })
        .unwrap_or_default();
    let contents = format!(
        "```vl\n{headline}\n```\n\n*{}*{location}",
        def_kind_label(def)
    );
    Some((contents, span_to_range(&analysis.text, cursor.highlight)))
}

/// Goto-definition target for the name under `offset`: the URI stays the
/// same (single-file documents in this milestone).
pub fn definition_at(analysis: &Analysis, uri: &Url, offset: usize) -> Option<Location> {
    let cursor = def_at_offset(&analysis.resolution, offset)?;
    let span = match cursor.def.kind {
        DefKind::Local | DefKind::Parameter => cursor.def.span,
        DefKind::ModuleAlias => module_alias_import_span(&analysis.program, &cursor.def.name)?,
        // Imported symbols and native externals have no provider source URI.
        DefKind::External | DefKind::ImportedFunction => return None,
    };
    Some(Location {
        uri: uri.clone(),
        range: span_to_range(&analysis.text, span),
    })
}

// ---------------------------------------------------------------------------
// Document symbols.
// ---------------------------------------------------------------------------

fn item_symbol(
    text: &str,
    name: String,
    kind: SymbolKind,
    name_span: Span,
    span: Span,
    children: Option<Vec<lsp_types::DocumentSymbol>>,
) -> lsp_types::DocumentSymbol {
    #[allow(deprecated)]
    lsp_types::DocumentSymbol {
        name,
        detail: None,
        kind,
        tags: None,
        deprecated: None,
        range: span_to_range(text, span),
        selection_range: span_to_range(text, name_span),
        children,
    }
}

/// Top-level symbols for the outline: functions, objects (with methods as
/// children), unions, error sets, globals, and imports.
pub fn document_symbols(analysis: &Analysis) -> Vec<lsp_types::DocumentSymbol> {
    let program = &analysis.program;
    let text = &analysis.text;
    let mut symbols = Vec::new();
    for item in &program.items {
        match item {
            Item::Function {
                name,
                name_span,
                span,
                ..
            } => symbols.push(item_symbol(
                text,
                name.clone(),
                SymbolKind::FUNCTION,
                *name_span,
                *span,
                None,
            )),
            Item::Object {
                name,
                name_span,
                methods,
                span,
                ..
            } => {
                let children = methods
                    .iter()
                    .map(|m| {
                        item_symbol(
                            text,
                            m.name.clone(),
                            SymbolKind::METHOD,
                            m.name_span,
                            m.span,
                            None,
                        )
                    })
                    .collect::<Vec<_>>();
                symbols.push(item_symbol(
                    text,
                    name.clone(),
                    SymbolKind::CLASS,
                    *name_span,
                    *span,
                    Some(children),
                ));
            }
            Item::Union {
                name,
                name_span,
                span,
                ..
            } => symbols.push(item_symbol(
                text,
                name.clone(),
                SymbolKind::ENUM,
                *name_span,
                *span,
                None,
            )),
            Item::Error {
                name,
                name_span,
                span,
                ..
            } => symbols.push(item_symbol(
                text,
                name.clone(),
                SymbolKind::ENUM,
                *name_span,
                *span,
                None,
            )),
            Item::Let {
                name,
                name_span,
                span,
                ..
            } => symbols.push(item_symbol(
                text,
                name.clone(),
                SymbolKind::VARIABLE,
                *name_span,
                *span,
                None,
            )),
            Item::Destructure {
                bindings,
                bindings_span,
                span,
                ..
            } => {
                for b in bindings {
                    symbols.push(item_symbol(
                        text,
                        b.binding.clone(),
                        SymbolKind::VARIABLE,
                        b.binding_span,
                        *span,
                        None,
                    ));
                }
                let _ = bindings_span;
            }
            Item::Use { path, span, .. } => symbols.push(item_symbol(
                text,
                path.join("."),
                SymbolKind::MODULE,
                *span,
                *span,
                None,
            )),
        }
    }
    symbols
}

// ---------------------------------------------------------------------------
// Completion.
// ---------------------------------------------------------------------------

/// VL keywords that are valid at statement/item starts.
pub const KEYWORDS: &[&str] = &[
    "fun", "val", "var", "type", "object", "union", "error", "use", "if", "else", "match", "while",
    "break", "continue", "return", "null", "try", "catch", "as", "extends", "self",
];

/// Whether `span` lies inside `outer` (inclusive on both ends, so a cursor
/// at an item boundary still counts as enclosed).
fn contains(outer: Span, span: Span) -> bool {
    outer.start <= span.start && span.end <= outer.end
}

/// Complete at `offset`: keywords plus the names lexically visible there.
/// Imports, functions, and top-level bindings are file-visible (once
/// declared — no forward suggestions); parameters and body locals are only
/// suggested while their actual lexical scope is active.
fn brace_scopes(text: &str) -> Vec<Span> {
    let (tokens, _) = vl_lex::lex(text);
    let mut stack = Vec::new();
    let mut scopes = Vec::new();
    for token in tokens {
        match token.kind {
            vl_lex::TokenKind::LBrace => stack.push(token.span.start),
            vl_lex::TokenKind::RBrace => {
                if let Some(start) = stack.pop() {
                    scopes.push(Span::new(start, token.span.end));
                }
            }
            _ => {}
        }
    }
    for start in stack {
        scopes.push(Span::new(start, text.len()));
    }
    scopes
}

fn point_in_scope(scope: Span, offset: usize) -> bool {
    scope.start <= offset && offset <= scope.end
}

fn stmt_span(stmt: &vl_syntax::Stmt) -> Span {
    use vl_syntax::Stmt;
    match stmt {
        Stmt::Let { span, .. }
        | Stmt::Assign { span, .. }
        | Stmt::IndexAssign { span, .. }
        | Stmt::FieldAssign { span, .. }
        | Stmt::TupleAssign { span, .. }
        | Stmt::Destructure { span, .. }
        | Stmt::If { span, .. }
        | Stmt::Match { span, .. }
        | Stmt::While { span, .. }
        | Stmt::Break { span }
        | Stmt::Continue { span }
        | Stmt::Return { span, .. }
        | Stmt::Defer { span, .. }
        | Stmt::ErrDefer { span, .. } => *span,
        Stmt::Expr(expr) => expr.span(),
    }
}

fn list_scope(stmts: &[vl_syntax::Stmt], parent: Span, blocks: &[Span]) -> Span {
    let first = stmts.first().map(stmt_span);
    let last = stmts.last().map(stmt_span);
    blocks
        .iter()
        .copied()
        .filter(|block| contains(parent, *block))
        .filter(|block| match (first, last) {
            (Some(first), Some(last)) => contains(*block, first) && contains(*block, last),
            _ => true,
        })
        .min_by_key(|block| block.len())
        .unwrap_or_else(|| {
            if stmts.len() == 1 {
                stmt_span(&stmts[0])
            } else {
                parent
            }
        })
}

fn collect_stmt_bindings(
    stmts: &[vl_syntax::Stmt],
    scope: Span,
    offset: usize,
    blocks: &[Span],
    visible: &mut std::collections::HashSet<(usize, usize)>,
) {
    if !point_in_scope(scope, offset) {
        return;
    }
    for stmt in stmts {
        let stmt_range = stmt_span(stmt);
        if stmt_range.start > offset {
            continue;
        }
        match stmt {
            vl_syntax::Stmt::Let {
                name_span, value, ..
            } => {
                if value.span().end <= offset {
                    visible.insert((name_span.start, name_span.end));
                }
            }
            vl_syntax::Stmt::Destructure {
                bindings, value, ..
            } => {
                for binding in bindings {
                    if value.span().end <= offset {
                        visible.insert((binding.binding_span.start, binding.binding_span.end));
                    }
                }
            }
            vl_syntax::Stmt::If {
                then_body,
                else_body,
                span,
                ..
            } => {
                let then_scope = list_scope(then_body, *span, blocks);
                collect_stmt_bindings(then_body, then_scope, offset, blocks, visible);
                if let Some(body) = else_body {
                    let branch_scope = list_scope(body, *span, blocks);
                    collect_stmt_bindings(body, branch_scope, offset, blocks, visible);
                }
            }
            vl_syntax::Stmt::While { body, span, .. } => {
                let body_scope = list_scope(body, *span, blocks);
                collect_stmt_bindings(body, body_scope, offset, blocks, visible);
            }
            vl_syntax::Stmt::Match {
                arms,
                else_body,
                span,
                ..
            } => {
                for arm in arms {
                    let arm_scope = list_scope(&arm.body, arm.span, blocks);
                    if point_in_scope(arm_scope, offset) {
                        for (_, binding_span) in &arm.bindings {
                            if binding_span.start <= offset {
                                visible.insert((binding_span.start, binding_span.end));
                            }
                        }
                    }
                    collect_stmt_bindings(&arm.body, arm_scope, offset, blocks, visible);
                }
                if let Some(body) = else_body {
                    let else_scope = list_scope(body, *span, blocks);
                    collect_stmt_bindings(body, else_scope, offset, blocks, visible);
                }
            }
            vl_syntax::Stmt::Defer { inner, .. } | vl_syntax::Stmt::ErrDefer { inner, .. } => {
                collect_stmt_bindings(
                    std::slice::from_ref(inner),
                    stmt_span(inner),
                    offset,
                    blocks,
                    visible,
                );
            }
            _ => {}
        }
    }
}

fn add_module_export(out: &mut Vec<(String, CompletionItemKind)>, spec: &ModuleSpec, name: &str) {
    if spec.lookup(name).is_some() {
        out.push((name.to_owned(), CompletionItemKind::FUNCTION));
    }
    if spec.lookup_object(name).is_some() {
        out.push((name.to_owned(), CompletionItemKind::CLASS));
    }
    if spec.lookup_union(name).is_some() || spec.lookup_error(name).is_some() {
        out.push((name.to_owned(), CompletionItemKind::ENUM));
    }
}

fn import_completion_items(analysis: &Analysis) -> Vec<(String, CompletionItemKind)> {
    let mut out = Vec::new();
    let module_for_path = |path: &[String]| {
        analysis
            .catalog
            .iter()
            .find(|spec| spec.path.segments() == path)
    };
    for item in &analysis.program.items {
        let Item::Use { path, names, .. } = item else {
            continue;
        };
        if let Some(spec) = module_for_path(path) {
            match names {
                None => {
                    if let Some(leaf) = path.last() {
                        out.push((leaf.clone(), CompletionItemKind::MODULE));
                    }
                }
                Some(names) => {
                    for name in names {
                        if name == "self" {
                            if let Some(leaf) = path.last() {
                                out.push((leaf.clone(), CompletionItemKind::MODULE));
                            }
                        } else {
                            add_module_export(&mut out, spec, name);
                        }
                    }
                }
            }
        } else if names.is_none() && path.len() >= 2 {
            if let Some(spec) = module_for_path(&path[..path.len() - 1]) {
                if let Some(leaf) = path.last() {
                    add_module_export(&mut out, spec, leaf);
                }
            }
        }
    }
    out
}

pub fn completion_at(analysis: &Analysis, offset: usize) -> Vec<CompletionItem> {
    let mut seen = std::collections::HashSet::new();
    let mut items = Vec::new();
    for keyword in KEYWORDS {
        seen.insert(keyword.to_string());
        items.push(CompletionItem {
            label: keyword.to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            ..Default::default()
        });
    }
    // Binding spans of top-level `let`/`destructure` items: file-visible
    // values (as opposed to function-body locals, which share the same
    // `Local` def kind but live inside a `Function` item span).
    let mut visible_spans = std::collections::HashSet::new();
    let blocks = brace_scopes(&analysis.text);
    for item in &analysis.program.items {
        match item {
            Item::Let {
                name_span, value, ..
            } if value.span().end <= offset => {
                visible_spans.insert((name_span.start, name_span.end));
            }
            Item::Destructure {
                bindings, value, ..
            } => {
                for binding in bindings {
                    if value.span().end <= offset {
                        visible_spans
                            .insert((binding.binding_span.start, binding.binding_span.end));
                    }
                }
            }
            Item::Function {
                name_span,
                params,
                body,
                span,
                ..
            } if point_in_scope(*span, offset) => {
                let body_scope = list_scope(body, *span, &blocks);
                if point_in_scope(body_scope, offset) {
                    visible_spans.extend(
                        params
                            .iter()
                            .filter(|p| p.name_span.start <= offset)
                            .map(|p| (p.name_span.start, p.name_span.end)),
                    );
                    collect_stmt_bindings(body, body_scope, offset, &blocks, &mut visible_spans);
                }
                if name_span.start <= offset {
                    visible_spans.insert((name_span.start, name_span.end));
                }
            }
            Item::Function { name_span, .. } if name_span.start <= offset => {
                visible_spans.insert((name_span.start, name_span.end));
            }
            Item::Object { methods, .. } | Item::Union { methods, .. } => {
                for method in methods {
                    if point_in_scope(method.span, offset) {
                        let body_scope = list_scope(&method.body, method.span, &blocks);
                        if !point_in_scope(body_scope, offset) {
                            continue;
                        }
                        visible_spans.extend(
                            method
                                .params
                                .iter()
                                .filter(|p| p.name_span.start <= offset)
                                .map(|p| (p.name_span.start, p.name_span.end)),
                        );
                        collect_stmt_bindings(
                            &method.body,
                            body_scope,
                            offset,
                            &blocks,
                            &mut visible_spans,
                        );
                        visible_spans.insert((method.name_span.start, method.name_span.end));
                    }
                }
            }
            _ => {}
        }
    }
    let mut names: Vec<(String, CompletionItemKind)> = Vec::new();
    for def in &analysis.resolution.defs {
        let kind = match def.kind {
            DefKind::Parameter => CompletionItemKind::VARIABLE,
            DefKind::Local => match def.binding {
                Some(_) => CompletionItemKind::VARIABLE,
                None => CompletionItemKind::FUNCTION,
            },
            DefKind::External | DefKind::ImportedFunction => CompletionItemKind::FUNCTION,
            DefKind::ModuleAlias => CompletionItemKind::MODULE,
        };
        let visible = match def.kind {
            DefKind::External | DefKind::ImportedFunction | DefKind::ModuleAlias => false,
            DefKind::Local if def.binding.is_none() => {
                def.span.start <= offset
                    && (visible_spans.contains(&(def.span.start, def.span.end))
                        || (!def.name.contains('.')
                            && analysis.program.items.iter().any(|item| {
                                matches!(item, Item::Function { name, name_span, .. }
                                if name == &def.name && name_span.start == def.span.start)
                            })))
            }
            _ => {
                def.span.start <= offset && visible_spans.contains(&(def.span.start, def.span.end))
            }
        };
        if visible {
            names.push((def.name.clone(), kind));
        }
    }
    names.extend(import_completion_items(analysis));
    names.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, kind) in names {
        if seen.insert(name.clone()) {
            items.push(CompletionItem {
                label: name,
                kind: Some(kind),
                ..Default::default()
            });
        }
    }
    items
}

// ---------------------------------------------------------------------------
// Formatting.
// ---------------------------------------------------------------------------

/// Format the document with `vl fmt` defaults. Returns `None` when the file
/// has lex/parse errors (diagnostics carry the reason; nothing is applied).
pub fn format_document(text: &str, module: &str) -> Option<String> {
    vl_fmt::format(text, module).ok()
}

// ---------------------------------------------------------------------------
// stdio server.
// ---------------------------------------------------------------------------

struct Documents {
    docs: HashMap<Url, String>,
    analyses: HashMap<Url, Analysis>,
    catalog: Vec<ModuleSpec>,
}

impl Documents {
    fn open(&mut self, uri: Url, text: String) -> Vec<lsp_types::Diagnostic> {
        let module = module_for_uri(&uri);
        let analysis = analyze(&text, &module, &self.catalog);
        let diags = diagnostics_for(&analysis);
        self.docs.insert(uri.clone(), text);
        self.analyses.insert(uri, analysis);
        diags
    }

    fn change(&mut self, uri: Url, text: String) -> Vec<lsp_types::Diagnostic> {
        self.open(uri, text)
    }

    fn close(&mut self, uri: &Url) {
        self.docs.remove(uri);
        self.analyses.remove(uri);
    }
}

fn publish(
    sender: &Sender<Message>,
    uri: Url,
    diagnostics: Vec<lsp_types::Diagnostic>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let params = PublishDiagnosticsParams {
        uri,
        diagnostics,
        version: None,
    };
    sender.send(Message::Notification(Notification::new(
        PublishDiagnostics::METHOD.to_owned(),
        params,
    )))?;
    Ok(())
}

fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(lsp_types::TextDocumentSyncCapability::Kind(
            TextDocumentSyncKind::FULL,
        )),
        hover_provider: Some(lsp_types::HoverProviderCapability::Simple(true)),
        definition_provider: Some(lsp_types::OneOf::Left(true)),
        document_symbol_provider: Some(lsp_types::OneOf::Left(true)),
        document_formatting_provider: Some(lsp_types::OneOf::Left(true)),
        completion_provider: Some(lsp_types::CompletionOptions {
            resolve_provider: Some(false),
            trigger_characters: Some(vec![".".to_owned(), ":".to_owned()]),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn request_params<P: serde::de::DeserializeOwned>(req: &Request) -> Option<P> {
    serde_json::from_value(req.params.clone()).ok()
}

/// One handler result: either a JSON result or a JSON-RPC error.
enum HandlerOutcome {
    Ok(serde_json::Value),
    Err(lsp_server::ErrorCode, String),
}

fn respond(
    sender: &Sender<Message>,
    id: lsp_server::RequestId,
    outcome: HandlerOutcome,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let response = match outcome {
        HandlerOutcome::Ok(result) => lsp_server::Response {
            id,
            result: Some(result),
            error: None,
        },
        HandlerOutcome::Err(code, message) => lsp_server::Response {
            id,
            result: None,
            error: Some(lsp_server::ResponseError {
                code: code as i32,
                message,
                data: None,
            }),
        },
    };
    sender.send(Message::Response(response))?;
    Ok(())
}

fn handle_request(
    req: Request,
    docs: &Documents,
    shutdown: bool,
    sender: &Sender<Message>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    // After `shutdown`, the server answers no further requests.
    if shutdown {
        return respond(
            sender,
            req.id.clone(),
            HandlerOutcome::Err(
                lsp_server::ErrorCode::InvalidRequest,
                "server is shut down".to_owned(),
            ),
        );
    }
    // Malformed params are a client bug: answer `InvalidParams` so the
    // client can diagnose it, and keep serving.
    let outcome = match req.method.as_str() {
        lsp_types::request::HoverRequest::METHOD => match request_params(&req) {
            Some(params) => HandlerOutcome::Ok(serde_json::to_value(hover_request(docs, params))?),
            None => invalid_params(&req.method),
        },
        lsp_types::request::GotoDefinition::METHOD => match request_params(&req) {
            Some(params) => {
                HandlerOutcome::Ok(serde_json::to_value(definition_request(docs, params))?)
            }
            None => invalid_params(&req.method),
        },
        lsp_types::request::DocumentSymbolRequest::METHOD => match request_params(&req) {
            Some(params) => {
                HandlerOutcome::Ok(serde_json::to_value(symbols_request(docs, params))?)
            }
            None => invalid_params(&req.method),
        },
        lsp_types::request::Formatting::METHOD => match request_params(&req) {
            Some(params) => {
                HandlerOutcome::Ok(serde_json::to_value(formatting_request(docs, params))?)
            }
            None => invalid_params(&req.method),
        },
        lsp_types::request::Completion::METHOD => match request_params(&req) {
            Some(params) => {
                HandlerOutcome::Ok(serde_json::to_value(completion_request(docs, params))?)
            }
            None => invalid_params(&req.method),
        },
        _ => HandlerOutcome::Err(
            lsp_server::ErrorCode::MethodNotFound,
            format!("unsupported method `{}`", req.method),
        ),
    };
    respond(sender, req.id.clone(), outcome)
}

fn invalid_params(method: &str) -> HandlerOutcome {
    HandlerOutcome::Err(
        lsp_server::ErrorCode::InvalidParams,
        format!("invalid params for `{method}`"),
    )
}

fn hover_request(docs: &Documents, params: HoverParams) -> Option<Hover> {
    let uri = params.text_document_position_params.text_document.uri;
    let analysis = docs.analyses.get(&uri)?;
    let offset = position_to_offset(
        &analysis.text,
        params.text_document_position_params.position,
    );
    let (contents, range) = hover_at(analysis, offset)?;
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: contents,
        }),
        range: Some(range),
    })
}

fn definition_request(
    docs: &Documents,
    params: GotoDefinitionParams,
) -> Option<GotoDefinitionResponse> {
    let uri = &params.text_document_position_params.text_document.uri;
    let analysis = docs.analyses.get(uri)?;
    let offset = position_to_offset(
        &analysis.text,
        params.text_document_position_params.position,
    );
    let location = definition_at(analysis, uri, offset)?;
    Some(GotoDefinitionResponse::Scalar(location))
}

fn symbols_request(
    docs: &Documents,
    params: DocumentSymbolParams,
) -> Option<DocumentSymbolResponse> {
    let analysis = docs.analyses.get(&params.text_document.uri)?;
    Some(DocumentSymbolResponse::Nested(document_symbols(analysis)))
}

fn formatting_request(
    docs: &Documents,
    params: DocumentFormattingParams,
) -> Option<Vec<lsp_types::TextEdit>> {
    let analysis = docs.analyses.get(&params.text_document.uri)?;
    let formatted = format_document(&analysis.text, &analysis.module)?;
    if formatted == analysis.text {
        return Some(Vec::new());
    }
    let end = offset_to_position(&analysis.text, analysis.text.len());
    Some(vec![lsp_types::TextEdit {
        range: Range {
            start: Position {
                line: 0,
                character: 0,
            },
            end,
        },
        new_text: formatted,
    }])
}

fn completion_request(docs: &Documents, params: CompletionParams) -> Option<CompletionResponse> {
    let uri = &params.text_document_position.text_document.uri;
    let analysis = docs.analyses.get(uri)?;
    let offset = position_to_offset(&analysis.text, params.text_document_position.position);
    Some(CompletionResponse::List(CompletionList {
        is_incomplete: false,
        items: completion_at(analysis, offset),
    }))
}

/// Best-effort notification parameters: a malformed notification is a
/// client bug and must never take the server down, so it is ignored.
fn notification_params<P: serde::de::DeserializeOwned>(notif: &Notification) -> Option<P> {
    serde_json::from_value(notif.params.clone()).ok()
}

fn handle_notification(
    notif: Notification,
    docs: &mut Documents,
    sender: &Sender<Message>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    match notif.method.as_str() {
        DidOpenTextDocument::METHOD => {
            let Some(params): Option<DidOpenTextDocumentParams> = notification_params(&notif)
            else {
                return Ok(());
            };
            let uri = params.text_document.uri.clone();
            let diags = docs.open(uri.clone(), params.text_document.text);
            publish(sender, uri, diags)?;
        }
        DidChangeTextDocument::METHOD => {
            let Some(params): Option<DidChangeTextDocumentParams> = notification_params(&notif)
            else {
                return Ok(());
            };
            if let Some(change) = params.content_changes.into_iter().last() {
                let uri = params.text_document.uri.clone();
                let diags = docs.change(uri.clone(), change.text);
                publish(sender, uri, diags)?;
            }
        }
        DidCloseTextDocument::METHOD => {
            let Some(params): Option<DidCloseTextDocumentParams> = notification_params(&notif)
            else {
                return Ok(());
            };
            let uri = params.text_document.uri.clone();
            docs.close(&uri);
            // The server owns its diagnostics: clear them so the client
            // does not keep showing stale errors for a closed document.
            publish(sender, uri, Vec::new())?;
        }
        DidSaveTextDocument::METHOD => {
            let Some(params): Option<DidSaveTextDocumentParams> = notification_params(&notif)
            else {
                return Ok(());
            };
            // Re-analyze on save in case the client only syncs then.
            if let Some(text) = params
                .text
                .or_else(|| docs.docs.get(&params.text_document.uri).cloned())
            {
                let uri = params.text_document.uri.clone();
                let diags = docs.change(uri.clone(), text);
                publish(sender, uri, diags)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Run the LSP server over stdio. Blocks until the client sends `exit`.
/// Transport/codec failures are returned to the driver, which reports them.
pub fn run_stdio() -> Result<(), Box<dyn Error + Send + Sync>> {
    let (connection, io_threads) = Connection::stdio();
    let caps = serde_json::to_value(server_capabilities())?;
    let init: serde_json::Value = connection.initialize(caps)?;
    // Validate the handshake shape; capabilities are intentionally ignored
    // in this milestone (full sync + UTF-16 positions throughout).
    let _: InitializeParams = serde_json::from_value(init)?;
    // `main_loop` owns the connection so its sender drops on return; the
    // writer thread only terminates once every sender is gone, so joining
    // while the connection is alive would hang.
    main_loop(connection)?;
    io_threads.join()?;
    Ok(())
}

fn main_loop(connection: Connection) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut docs = Documents {
        docs: HashMap::new(),
        analyses: HashMap::new(),
        catalog: default_catalog(),
    };
    let mut shutdown = false;
    for msg in &connection.receiver {
        match msg {
            Message::Request(req) => {
                if req.method == Shutdown::METHOD {
                    shutdown = true;
                    respond(
                        &connection.sender,
                        req.id.clone(),
                        HandlerOutcome::Ok(serde_json::Value::Null),
                    )?;
                } else {
                    handle_request(req, &docs, shutdown, &connection.sender)?;
                }
            }
            Message::Notification(notif) => {
                if notif.method == lsp_types::notification::Exit::METHOD {
                    break;
                }
                handle_notification(notif, &mut docs, &connection.sender)?;
            }
            Message::Response(_) => {}
        }
    }
    // `exit` without a prior `shutdown` is a client protocol violation.
    if !shutdown {
        return Err("client sent `exit` without a prior `shutdown` request".into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_catalog() -> Vec<ModuleSpec> {
        Vec::new()
    }

    fn analyze_test(text: &str) -> Analysis {
        analyze(text, "test", &test_catalog())
    }

    #[test]
    fn module_for_uri_uses_file_stem() {
        let uri: Url = "file:///home/user/arith.vl".parse().unwrap();
        assert_eq!(module_for_uri(&uri), "arith");
        let uri: Url = "untitled:Untitled-1".parse().unwrap();
        assert_eq!(module_for_uri(&uri), "stdin");
    }

    #[test]
    fn positions_round_trip_on_ascii() {
        let text = "fun main() {}\nval x = 1;\n";
        for offset in [0, 5, 14, 15, 20, text.len()] {
            let pos = offset_to_position(text, offset);
            assert_eq!(position_to_offset(text, pos), offset, "at {offset}");
        }
    }

    #[test]
    fn positions_count_utf16_units() {
        // `é` is one scalar and one UTF-16 unit; `𝄞` is one scalar but two.
        let text = "val a = \"é𝄞\";\n";
        let e_off = text.find('é').unwrap();
        let pos = offset_to_position(text, e_off);
        let line_prefix = text[..e_off].split('\n').next_back().unwrap();
        assert_eq!(pos.character, utf16_len(line_prefix) as u32);
        let clef_off = text.find('𝄞').unwrap();
        let clef_pos = offset_to_position(text, clef_off);
        // `é` contributed exactly one UTF-16 unit before the clef.
        assert_eq!(clef_pos.character, pos.character + 1);
        assert_eq!(position_to_offset(text, clef_pos), clef_off);
        // Past-end clamps to the line end.
        let clamped = position_to_offset(
            text,
            Position {
                line: 0,
                character: 999,
            },
        );
        assert_eq!(clamped, text.find('\n').unwrap());
        // Beyond-last-line clamps to the final line start region.
        let end = position_to_offset(
            text,
            Position {
                line: 99,
                character: 0,
            },
        );
        assert_eq!(end, text.len());
    }

    #[test]
    fn span_range_is_exclusive_and_zero_based() {
        let text = "val x = 1;\n";
        let range = span_to_range(text, Span::new(4, 5));
        assert_eq!(
            (
                range.start.line,
                range.start.character,
                range.end.line,
                range.end.character
            ),
            (0, 4, 0, 5)
        );
    }

    #[test]
    fn clean_file_has_no_diagnostics_and_resolves() {
        let analysis = analyze_test("fun main() {}\n");
        assert!(analysis.diags.is_empty());
        assert!(!analysis.resolution.defs.is_empty());
        assert!(diagnostics_for(&analysis).is_empty());
    }

    #[test]
    fn broken_file_still_publishes_an_error() {
        let analysis = analyze_test("val x = 1\n");
        assert!(!analysis.diags.is_empty());
        let lsp_diags = diagnostics_for(&analysis);
        assert_eq!(lsp_diags.len(), analysis.diags.len());
        assert_eq!(lsp_diags[0].severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(lsp_diags[0].source.as_deref(), Some("vl"));
    }

    #[test]
    fn successful_analysis_publishes_frontend_warnings() {
        let analysis = analyze_test("fun main() { val x = 1u64; val x = 2u64; x; }");
        assert!(analysis
            .diags
            .iter()
            .any(|d| d.severity == Severity::Warning));
        assert!(diagnostics_for(&analysis)
            .iter()
            .any(|d| d.severity == Some(DiagnosticSeverity::WARNING)));
    }

    #[test]
    fn hover_shows_function_signature() {
        let text =
            "fun add(a: u64, b: u64): u64 { return a + b; }\nfun main() { add(1u64, 2u64); }\n";
        let analysis = analyze_test(text);
        let offset = text.find("add(1u64").unwrap();
        let (contents, _) = hover_at(&analysis, offset).expect("use must hover");
        assert!(contents.contains("fun add"), "{contents}");
        assert!(contents.contains("u64"), "{contents}");
    }

    #[test]
    fn hover_falls_back_to_best_effort_on_type_errors() {
        // `oops` is undefined: the file fails checking, but the `main` use
        // of `add` must still hover via best-effort resolution.
        let text = "fun add(a: u64): u64 { return a; }\nfun main() { add(oops); }\n";
        let analysis = analyze_test(text);
        assert!(!analysis.diags.is_empty());
        let offset = text.find("add(oops").unwrap();
        let (contents, _) = hover_at(&analysis, offset).expect("best-effort hover");
        assert!(contents.contains("add"), "{contents}");
    }

    #[test]
    fn goto_definition_points_at_the_def_site() {
        let text = "fun add(a: u64): u64 { return a; }\nfun main() { add(1u64); }\n";
        let analysis = analyze_test(text);
        let uri: Url = "file:///test.vl".parse().unwrap();
        let offset = text.find("add(1u64").unwrap();
        let loc = definition_at(&analysis, &uri, offset).expect("use must resolve");
        assert_eq!(loc.uri, uri);
        assert_eq!(
            loc.range.start,
            offset_to_position(text, text.find("fun add").unwrap() + 4)
        );
    }

    #[test]
    fn imported_definitions_have_no_fabricated_provider_location() {
        let exports: &[vl_common::ExportDecl<'_>] = &[(
            "print",
            &[("s", vl_common::VlType::String)],
            vl_common::VlType::Void,
        )];
        let catalog = vec![ModuleSpec::new(&["std"], exports)];
        let text = "use std; fun main() { std.print(\"hello\"); }";
        let analysis = analyze(text, "app", &catalog);
        let print = text.find("print").expect("call") + 1;
        let uri: Url = "file:///app.vl".parse().unwrap();
        assert!(definition_at(&analysis, &uri, print).is_none());
        let (hover, _) = hover_at(&analysis, print).expect("import hover");
        assert!(hover.contains("fun std.print"), "{hover}");
        assert!(!hover.contains("defined at line"), "{hover}");

        let alias_text = "use std; fun main() { std; }";
        let alias_analysis = analyze(alias_text, "app", &catalog);
        let alias = alias_text.rfind("std").expect("alias use");
        let location =
            definition_at(&alias_analysis, &uri, alias).expect("module alias import target");
        assert_eq!(
            location.range.start,
            offset_to_position(alias_text, alias_text.find("use std").unwrap())
        );
    }

    #[test]
    fn document_symbols_lists_items() {
        let text = "fun main() {}\nval x = 1u64;\ntype P = object { f: u64, };\n";
        let analysis = analyze_test(text);
        let symbols = document_symbols(&analysis);
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"main"), "{names:?}");
        assert!(names.contains(&"x"), "{names:?}");
        assert!(names.contains(&"P"), "{names:?}");
    }

    #[test]
    fn completion_offers_keywords_and_scope_names() {
        let text = "fun helper() {}\nfun main() {}\n";
        let analysis = analyze_test(text);
        let items = completion_at(&analysis, text.len());
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        for keyword in ["fun", "return", "match"] {
            assert!(labels.contains(&keyword), "{labels:?}");
        }
        assert!(labels.contains(&"helper"), "{labels:?}");
    }

    #[test]
    fn completion_hides_other_functions_locals() {
        let text = "fun first(hidden: u64): u64 { val private = hidden; return private; }\nfun main() { return 1u64; }\n";
        let analysis = analyze_test(text);
        // Inside `main` (second line): neither `first`'s parameter nor its
        // body local is visible.
        let offset = text.find("return 1u64").unwrap();
        let items = completion_at(&analysis, offset);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(!labels.contains(&"hidden"), "{labels:?}");
        assert!(!labels.contains(&"private"), "{labels:?}");
        assert!(labels.contains(&"first"), "{labels:?}");
        // Inside `first`'s body both are visible.
        let inner = text.find("return private").unwrap();
        let inner_items = completion_at(&analysis, inner);
        let inner_labels: Vec<&str> = inner_items.iter().map(|i| i.label.as_str()).collect();
        assert!(inner_labels.contains(&"hidden"), "{inner_labels:?}");
        assert!(inner_labels.contains(&"private"), "{inner_labels:?}");
    }

    #[test]
    fn completion_hides_not_yet_declared_globals() {
        let text = "fun main() {}\nval later = 1u64;\n";
        let analysis = analyze_test(text);
        let items = completion_at(&analysis, 0);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(!labels.contains(&"later"), "{labels:?}");
        let after = completion_at(&analysis, text.len());
        let after_labels: Vec<&str> = after.iter().map(|i| i.label.as_str()).collect();
        assert!(after_labels.contains(&"later"), "{after_labels:?}");
        let initializer = completion_at(&analysis, text.find("1u64").unwrap());
        assert!(!initializer.iter().any(|item| item.label == "later"));
    }

    #[test]
    fn completion_respects_if_blocks_and_method_scopes() {
        let if_text = "fun main() { if (1u64 == 1u64) { val secret = 1u64; } val outside = 2u64; }";
        let analysis = analyze_test(if_text);
        let at = if_text.find("outside").unwrap();
        let labels = completion_at(&analysis, at)
            .into_iter()
            .map(|item| item.label)
            .collect::<Vec<_>>();
        assert!(!labels.iter().any(|name| name == "secret"), "{labels:?}");

        let unbraced = "fun main() { if (true) val then_name = 1u64; else val else_name = 2u64; }";
        let analysis = analyze_test(unbraced);
        let at = unbraced.find("2u64").unwrap();
        let labels = completion_at(&analysis, at)
            .into_iter()
            .map(|item| item.label)
            .collect::<Vec<_>>();
        assert!(!labels.iter().any(|name| name == "then_name"), "{labels:?}");
        assert!(!labels.iter().any(|name| name == "else_name"), "{labels:?}");

        let later = "fun main() { if (true) { val inside = 1u64; inside; } }";
        let analysis = analyze_test(later);
        let at = later.rfind("inside").unwrap();
        let labels = completion_at(&analysis, at)
            .into_iter()
            .map(|item| item.label)
            .collect::<Vec<_>>();
        assert!(labels.iter().any(|name| name == "inside"), "{labels:?}");

        let method_text = "type A = object { fun first(secret: u64): u64 { return secret; } fun second(other: u64): u64 { return other; } };";
        let analysis = analyze_test(method_text);
        let at = method_text.find("return other").unwrap();
        let labels = completion_at(&analysis, at)
            .into_iter()
            .map(|item| item.label)
            .collect::<Vec<_>>();
        assert!(!labels.iter().any(|name| name == "secret"), "{labels:?}");
        assert!(labels.iter().any(|name| name == "other"), "{labels:?}");
    }

    #[test]
    fn completion_keeps_match_arm_bindings_in_their_arm() {
        let text = "type U = union { A(u64), B(u64), }; fun f(u: U) { match (u) { U.A(secret) {} U.B(other) { other; } } }";
        let analysis = analyze_test(text);
        let at = text.find("other;").expect("second arm");
        let labels = completion_at(&analysis, at)
            .into_iter()
            .map(|item| item.label)
            .collect::<Vec<_>>();
        assert!(!labels.iter().any(|name| name == "secret"), "{labels:?}");
        assert!(labels.iter().any(|name| name == "other"), "{labels:?}");
    }

    #[test]
    fn completion_offers_unused_imports_from_the_catalog() {
        let exports: &[vl_common::ExportDecl<'_>] = &[(
            "print",
            &[("s", vl_common::VlType::String)],
            vl_common::VlType::Void,
        )];
        let catalog = vec![ModuleSpec::new(&["std"], exports)];
        let alias_text = "use std; fun main() { }";
        let alias = analyze(alias_text, "app", &catalog);
        let alias_labels = completion_at(&alias, alias_text.find('}').unwrap())
            .into_iter()
            .map(|item| item.label)
            .collect::<Vec<_>>();
        assert!(
            alias_labels.iter().any(|name| name == "std"),
            "{alias_labels:?}"
        );

        let function_text = "use std.print; fun main() { }";
        let function = analyze(function_text, "app", &catalog);
        let function_labels = completion_at(&function, function_text.find('}').unwrap())
            .into_iter()
            .map(|item| item.label)
            .collect::<Vec<_>>();
        assert!(
            function_labels.iter().any(|name| name == "print"),
            "{function_labels:?}"
        );
    }

    #[test]
    fn positions_handle_crlf_endings() {
        let text = "fun main() {}\r\nval x = 1u64;\r\n";
        // Start of the second line, past the CRLF.
        let off = text.find("val").unwrap();
        let pos = offset_to_position(text, off);
        assert_eq!((pos.line, pos.character), (1, 0));
        assert_eq!(position_to_offset(text, pos), off);
        // Past-end clamps to the line content (no `\r` in columns).
        let end = position_to_offset(
            text,
            Position {
                line: 1,
                character: 999,
            },
        );
        assert_eq!(end, off + "val x = 1u64;".len());
        assert_eq!(&text[end..end + 1], "\r");
    }

    #[test]
    fn unknown_method_answers_method_not_found() {
        let (sender, receiver) = crossbeam_channel::unbounded();
        let docs = Documents {
            docs: HashMap::new(),
            analyses: HashMap::new(),
            catalog: test_catalog(),
        };
        let req = Request {
            id: lsp_server::RequestId::from(1),
            method: "textDocument/nope".to_owned(),
            params: serde_json::Value::Null,
        };
        handle_request(req, &docs, false, &sender).unwrap();
        let Message::Response(resp) = receiver.try_recv().unwrap() else {
            panic!("expected a response");
        };
        let error = resp.error.expect("unknown method must error");
        assert_eq!(error.code, lsp_server::ErrorCode::MethodNotFound as i32);
    }

    #[test]
    fn malformed_notifications_are_ignored_and_close_clears() {
        let (sender, receiver) = crossbeam_channel::unbounded();
        let mut docs = Documents {
            docs: HashMap::new(),
            analyses: HashMap::new(),
            catalog: test_catalog(),
        };
        let uri: Url = "file:///close.vl".parse().unwrap();
        // Malformed `didOpen` (empty params) must not kill the server.
        handle_notification(
            Notification {
                method: DidOpenTextDocument::METHOD.to_owned(),
                params: serde_json::json!({}),
            },
            &mut docs,
            &sender,
        )
        .unwrap();
        assert!(receiver.try_recv().is_err());
        // Open for real, then close: the close must publish empty
        // diagnostics to clear the client's stale errors.
        handle_notification(
            Notification {
                method: DidOpenTextDocument::METHOD.to_owned(),
                params: serde_json::to_value(DidOpenTextDocumentParams {
                    text_document: lsp_types::TextDocumentItem {
                        uri: uri.clone(),
                        language_id: "vl".to_owned(),
                        version: 1,
                        text: "val x = 1\n".to_owned(),
                    },
                })
                .unwrap(),
            },
            &mut docs,
            &sender,
        )
        .unwrap();
        let open_notif = match receiver.try_recv().unwrap() {
            Message::Notification(notif) => notif,
            other => panic!("expected diagnostics, got {other:?}"),
        };
        let open_params: PublishDiagnosticsParams =
            serde_json::from_value(open_notif.params).unwrap();
        assert!(!open_params.diagnostics.is_empty());
        handle_notification(
            Notification {
                method: DidCloseTextDocument::METHOD.to_owned(),
                params: serde_json::to_value(DidCloseTextDocumentParams {
                    text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                })
                .unwrap(),
            },
            &mut docs,
            &sender,
        )
        .unwrap();
        let close_notif = match receiver.try_recv().unwrap() {
            Message::Notification(notif) => notif,
            other => panic!("expected diagnostics, got {other:?}"),
        };
        let close_params: PublishDiagnosticsParams =
            serde_json::from_value(close_notif.params).unwrap();
        assert_eq!(close_params.uri, uri);
        assert!(close_params.diagnostics.is_empty());
    }

    #[test]
    fn formatting_returns_canonical_text() {
        let out = format_document("fun main() {}\n", "test").expect("clean file formats");
        assert_eq!(out, "fun main() {}\n");
        assert!(format_document("val x = 1\n", "test").is_none());
    }
}
