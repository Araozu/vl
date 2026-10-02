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

/// One open document's checked state: the parsed program (present even when
/// checking fails, for symbols/completion), the full frontend result, and a
/// best-effort resolution kept across errors so hover/goto-definition keep
/// working mid-edit.
pub struct Analysis {
    pub text: String,
    pub module: String,
    pub program: Option<Program>,
    pub frontend: Result<FrontendOk, Vec<Diagnostic>>,
    /// Name resolution for hover/goto-definition. `Some` whenever the file
    /// parses (even with type errors); `None` only for lex/parse failures.
    pub resolution: Option<Resolution>,
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
    let mut analysis = Analysis {
        text: text.to_owned(),
        module: module.to_owned(),
        program: Some(program),
        frontend: match frontend {
            Ok(ok) => Ok(ok),
            Err(diags) => Err(diags),
        },
        resolution,
        diags: Vec::new(),
    };
    if let Err(diags) = &analysis.frontend {
        analysis.diags = diags.clone();
    }
    analysis
}

/// Best-effort name resolution over a parsed program, ignoring the returned
/// diagnostics (already reported or about to be). Returns `None` when even
/// resolution cannot run — currently never, but the option keeps the
/// mid-edit path explicit.
fn best_effort_resolution(program: &Program, catalog: &[ModuleSpec]) -> Option<Resolution> {
    let (resolution, _) = vl_semantic::resolve_with_modules(program, catalog);
    Some(resolution)
}

/// Analyze with the default catalog (target natives + stdlib).
pub fn analyze_default(text: &str, module: &str) -> Analysis {
    let catalog = default_catalog();
    analyze(text, module, &catalog)
}

// ---------------------------------------------------------------------------
// Positions: byte offsets/spans (1-based scalar columns) <-> LSP (UTF-16).
// ---------------------------------------------------------------------------

/// Byte offset of each line's first byte. Always starts with `0`; one entry
/// is pushed after every `\n`.
fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
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
    let starts = line_starts(text);
    let line = starts.partition_point(|&s| s <= offset).max(1) - 1;
    let character = utf16_len(&text[starts[line]..offset]);
    Position {
        line: line as u32,
        character: character as u32,
    }
}

/// LSP position → byte offset. Out-of-range lines/characters clamp to the
/// line end; mid-character positions snap down to the boundary.
pub fn position_to_offset(text: &str, pos: Position) -> usize {
    let starts = line_starts(text);
    let line = (pos.line as usize).min(starts.len() - 1);
    let line_start = starts[line];
    let line_end = if line + 1 < starts.len() {
        // Exclude the `\n` itself so clamping lands on line content.
        starts[line + 1] - 1
    } else {
        text.len()
    };
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
    let resolution = analysis.resolution.as_ref()?;
    let cursor = def_at_offset(resolution, offset)?;
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
    let defined_line = offset_to_position(&analysis.text, def.span.start).line + 1;
    let contents = format!(
        "```vl\n{headline}\n```\n\n*{}* · defined at line {defined_line}",
        def_kind_label(def)
    );
    Some((contents, span_to_range(&analysis.text, cursor.highlight)))
}

/// Goto-definition target for the name under `offset`: the URI stays the
/// same (single-file documents in this milestone).
pub fn definition_at(analysis: &Analysis, uri: &Url, offset: usize) -> Option<Location> {
    let resolution = analysis.resolution.as_ref()?;
    let cursor = def_at_offset(resolution, offset)?;
    // A use of a module alias points at the alias itself; everything else
    // points at the definition site.
    Some(Location {
        uri: uri.clone(),
        range: span_to_range(&analysis.text, cursor.def.span),
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
    let Some(program) = &analysis.program else {
        return Vec::new();
    };
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

/// Complete at `offset`: keywords plus every name in scope (definitions and
/// imports). Single-file scope in this milestone.
pub fn completion_at(analysis: &Analysis, _offset: usize) -> Vec<CompletionItem> {
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
    if let Some(resolution) = &analysis.resolution {
        let mut names: Vec<(&String, CompletionItemKind)> = Vec::new();
        for def in &resolution.defs {
            let kind = match def.kind {
                DefKind::Parameter => CompletionItemKind::VARIABLE,
                DefKind::Local => match def.binding {
                    Some(_) => CompletionItemKind::VARIABLE,
                    None => CompletionItemKind::FUNCTION,
                },
                DefKind::External | DefKind::ImportedFunction => CompletionItemKind::FUNCTION,
                DefKind::ModuleAlias => CompletionItemKind::MODULE,
            };
            names.push((&def.name, kind));
        }
        names.sort_by(|a, b| a.0.cmp(b.0));
        for (name, kind) in names {
            if seen.insert(name.clone()) {
                items.push(CompletionItem {
                    label: name.clone(),
                    kind: Some(kind),
                    ..Default::default()
                });
            }
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

fn handle_request(
    req: Request,
    docs: &Documents,
    sender: &Sender<Message>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    // Unknown methods and malformed params answer null so the client moves on.
    let result = match req.method.as_str() {
        lsp_types::request::HoverRequest::METHOD => request_params(&req)
            .map(|params| hover_request(docs, params))
            .and_then(|result| serde_json::to_value(result).ok())
            .unwrap_or(serde_json::Value::Null),
        lsp_types::request::GotoDefinition::METHOD => request_params(&req)
            .map(|params| definition_request(docs, params))
            .and_then(|result| serde_json::to_value(result).ok())
            .unwrap_or(serde_json::Value::Null),
        lsp_types::request::DocumentSymbolRequest::METHOD => request_params(&req)
            .map(|params| symbols_request(docs, params))
            .and_then(|result| serde_json::to_value(result).ok())
            .unwrap_or(serde_json::Value::Null),
        lsp_types::request::Formatting::METHOD => request_params(&req)
            .map(|params| formatting_request(docs, params))
            .and_then(|result| serde_json::to_value(result).ok())
            .unwrap_or(serde_json::Value::Null),
        lsp_types::request::Completion::METHOD => request_params(&req)
            .map(|params| completion_request(docs, params))
            .and_then(|result| serde_json::to_value(result).ok())
            .unwrap_or(serde_json::Value::Null),
        _ => serde_json::Value::Null,
    };
    sender.send(Message::Response(lsp_server::Response {
        id: req.id.clone(),
        result: Some(result),
        error: None,
    }))?;
    Ok(())
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

fn handle_notification(
    notif: Notification,
    docs: &mut Documents,
    sender: &Sender<Message>,
) -> Result<bool, Box<dyn Error + Send + Sync>> {
    match notif.method.as_str() {
        DidOpenTextDocument::METHOD => {
            let params: DidOpenTextDocumentParams = serde_json::from_value(notif.params)?;
            let uri = params.text_document.uri.clone();
            let diags = docs.open(uri.clone(), params.text_document.text);
            publish(sender, uri, diags)?;
        }
        DidChangeTextDocument::METHOD => {
            let params: DidChangeTextDocumentParams = serde_json::from_value(notif.params)?;
            if let Some(change) = params.content_changes.into_iter().last() {
                let uri = params.text_document.uri.clone();
                let diags = docs.change(uri.clone(), change.text);
                publish(sender, uri, diags)?;
            }
        }
        DidCloseTextDocument::METHOD => {
            let params: DidCloseTextDocumentParams = serde_json::from_value(notif.params)?;
            docs.close(&params.text_document.uri);
        }
        DidSaveTextDocument::METHOD => {
            let params: DidSaveTextDocumentParams = serde_json::from_value(notif.params)?;
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
    Ok(false)
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
    for msg in &connection.receiver {
        match msg {
            Message::Request(req) => {
                if req.method == Shutdown::METHOD {
                    let resp = lsp_server::Response {
                        id: req.id.clone(),
                        result: Some(serde_json::Value::Null),
                        error: None,
                    };
                    connection.sender.send(Message::Response(resp))?;
                } else {
                    handle_request(req, &docs, &connection.sender)?;
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
        assert!(analysis.resolution.is_some());
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
        let items = completion_at(&analysis, 0);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        for keyword in ["fun", "return", "match"] {
            assert!(labels.contains(&keyword), "{labels:?}");
        }
        assert!(labels.contains(&"helper"), "{labels:?}");
    }

    #[test]
    fn formatting_returns_canonical_text() {
        let out = format_document("fun main() {}\n", "test").expect("clean file formats");
        assert_eq!(out, "fun main() {}\n");
        assert!(format_document("val x = 1\n", "test").is_none());
    }
}
