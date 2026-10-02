//! vl driver: CLI that wires the whole pipeline together.
//!
//! Pipeline: text --lex--> tokens --parse--> AST --resolve--> scopes
//! --lower--> HIR --check--> typed HIR --lower--> LIR --emit--> target.
//!
//! Every stage appends to one `Vec<Diagnostic>`; all printing goes through
//! Ariadne (`vl_common::diagnostic::emit_all`). Exit code 1 iff any error.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use clap::{Parser, ValueEnum};
use serde::Deserialize;
use vl_common::diagnostic::emit_all;

#[derive(Debug, Parser)]
#[command(name = "vl", version, about = "VL — vibecoded language driver")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, clap::Subcommand)]
enum Cmd {
    /// Create a project configuration in the current directory.
    Init { module: Option<String> },
    /// Lex a file and print tokens.
    Lex { file: PathBuf },
    /// Parse a file and print the AST.
    Parse { file: PathBuf },
    /// Run the full frontend (lex..typecheck) and report errors.
    Check {
        /// Source file (`-` reads stdin as module `stdin`, no project lookup).
        file: PathBuf,
        /// Render diagnostics as human text or machine-readable JSON.
        #[arg(long, value_enum, default_value_t = Format::Human)]
        format: Format,
    },
    /// Compile a file or the current project; print or write target output.
    Build {
        /// Source file. Omit this to build the current directory's project.
        /// (`-` reads stdin as module `stdin`, no project lookup.)
        file: Option<PathBuf>,
        /// Which backend to use.
        #[arg(long, default_value = "naravm")]
        target: String,
        /// Dump an intermediate instead of compiling.
        #[arg(long, value_enum)]
        emit: Option<Emit>,
        /// Write a single-file artifact here, or override a project's output folder.
        /// Required with `--format json` so stdout stays pure JSON.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Render diagnostics as human text or machine-readable JSON.
        #[arg(long, value_enum, default_value_t = Format::Human)]
        format: Format,
    },
    /// Run a project script; omit the name to run the `run` script.
    Run { name: Option<String> },
    /// List available codegen backends.
    Targets,
    /// Export the merged standard-library API as JSON for documentation tooling.
    Stdlib,
    /// Format `.vl` files in place (zig fmt style, non-overridable defaults).
    Fmt {
        /// Files or directories to format (directories recurse for `.vl`).
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Report files that would change without writing them.
        #[arg(long)]
        check: bool,
    },
    /// Start the VL language server over stdio (for editor integrations).
    Lsp,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectConfig {
    module: String,
    #[serde(default = "default_source_dir")]
    source: PathBuf,
    #[serde(default = "default_out_dir")]
    out: PathBuf,
    #[serde(default)]
    scripts: BTreeMap<String, String>,
}

#[derive(Debug)]
struct Project {
    root: PathBuf,
    config: ProjectConfig,
}

fn default_source_dir() -> PathBuf {
    PathBuf::from("src")
}

fn default_out_dir() -> PathBuf {
    PathBuf::from("out")
}

#[derive(Debug)]
enum ProjectOutput {
    Text(String),
    Bytes(Vec<u8>),
}

struct ProjectFileBuild {
    output: Option<ProjectOutput>,
    diags: Vec<vl_common::Diagnostic>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Emit {
    Tokens,
    Ast,
    Lir,
    Asm,
}

/// Diagnostic rendering for `check` and `build`.
#[derive(Debug, Clone, Copy, Default, ValueEnum, PartialEq, Eq)]
enum Format {
    /// Ariadne pretty errors on stderr (default).
    #[default]
    Human,
    /// One JSON document on stdout: `{"ok":bool,"diagnostics":[...]}`.
    /// Each diagnostic carries its file, severity, message, optional code
    /// and note, plus labelled spans with byte offsets and 1-based
    /// line/column (columns count Unicode scalar values).
    Json,
}

fn read_input(file: &Path) -> Result<(String, String), String> {
    match fs::read_to_string(file) {
        Ok(text) => Ok((file.display().to_string(), text)),
        Err(e) => Err(format!("cannot read {}: {e}", file.display())),
    }
}

/// `-` means stdin (module `stdin`): for editors and tooling that already
/// hold the buffer. Never does project discovery.
fn is_stdin(file: &Path) -> bool {
    file.as_os_str() == "-"
}

fn read_stdin() -> Result<(String, String), String> {
    use std::io::Read;
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .map_err(|e| format!("cannot read stdin: {e}"))?;
    Ok(("<stdin>".to_string(), text))
}

/// Print one JSON diagnostics document to stdout (machine-readable mode).
/// Returns true if any diagnostic is an error.
fn emit_json(entries: &[(&str, &str, &[vl_common::Diagnostic])]) -> bool {
    let mut collected = Vec::new();
    for (file, text, diags) in entries {
        collected.append(&mut vl_frontend::collect_json(file, text, diags));
    }
    let failed = collected.iter().any(|d| d.severity == "error");
    write_out(&None, &vl_frontend::render_json(collected));
    failed
}

/// One driver-level diagnostic for JSON mode (aggregated under `<driver>`).
fn driver_diagnostic(message: &str, code: &str) -> vl_common::Diagnostic {
    vl_common::Diagnostic::error(message).with_code(code)
}

/// Immediate driver failure: render per `fmt` and exit 2.
fn fail_driver(fmt: Format, message: &str, code: &str) -> ExitCode {
    if fmt == Format::Json {
        let diag = driver_diagnostic(message, code);
        emit_json(&[("<driver>", "", std::slice::from_ref(&diag))]);
    } else {
        emit_driver_error(message, code);
    }
    ExitCode::from(2)
}

/// Entrypoint return check: `main` is infallible by definition (the VM
/// loader owns startup failure). A fallible `main` gets its own E401;
/// any other non-`void` return gets the classic one. `None` (already
/// reported) stays quiet.
fn main_ret_error(
    ret: &Option<vl_common::VlType>,
    span: vl_common::Span,
) -> Option<vl_common::Diagnostic> {
    match ret {
        None | Some(vl_common::VlType::Void) => None,
        Some(vl_common::VlType::Fallible { .. }) => Some(
            vl_common::Diagnostic::error("`main` cannot be fallible")
                .with_label(span, "entrypoint declared here")
                .with_note("handle errors inside `main` (e.g. `catch`) instead")
                .with_code("E401"),
        ),
        _ => Some(
            vl_common::Diagnostic::error("`main` must return `void`")
                .with_label(span, "entrypoint declared here")
                .with_note("omit the return type (it defaults to `void`)")
                .with_code("E401"),
        ),
    }
}

fn source_module(file: &Path) -> String {
    file.file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| file.display().to_string())
}

fn validate_module_name(module: &str) -> Result<(), String> {
    if module.is_empty() {
        return Err("project module cannot be empty".into());
    }
    if module.split('.').any(|segment| {
        let mut chars = segment.chars();
        let Some(first) = chars.next() else {
            return true;
        };
        !(first == '_' || first.is_ascii_alphabetic())
            || chars.any(|ch| !(ch == '_' || ch.is_ascii_alphanumeric()))
    }) {
        return Err(format!(
            "invalid project module `{module}`; use dot-separated identifiers"
        ));
    }
    Ok(())
}

fn validate_project_config(config: &ProjectConfig) -> Result<(), String> {
    validate_module_name(&config.module)?;
    if config.source.as_os_str().is_empty() {
        return Err("project source folder cannot be empty".into());
    }
    if config.out.as_os_str().is_empty() {
        return Err("project output folder cannot be empty".into());
    }
    for (name, command) in &config.scripts {
        if name.is_empty() {
            return Err("project script name cannot be empty".into());
        }
        if command.trim().is_empty() {
            return Err(format!("project script `{name}` cannot be empty"));
        }
    }
    Ok(())
}

fn load_project(root: &Path) -> Result<Project, String> {
    let config_path = root.join("vl.toml");
    let text = fs::read_to_string(&config_path).map_err(|e| {
        format!(
            "cannot read project configuration {}: {e}",
            config_path.display()
        )
    })?;
    let config = toml::from_str::<ProjectConfig>(&text).map_err(|e| {
        format!(
            "cannot parse project configuration {}: {e}",
            config_path.display()
        )
    })?;
    validate_project_config(&config).map_err(|e| format!("{}: {e}", config_path.display()))?;
    Ok(Project {
        root: root.to_path_buf(),
        config,
    })
}

fn default_init_module(root: &Path) -> Result<String, String> {
    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "cannot derive a module name from the current directory".to_string())?;
    let mut module = String::new();
    for (index, ch) in name.chars().enumerate() {
        let valid = ch == '_' || ch.is_ascii_alphanumeric();
        if index == 0 && ch.is_ascii_digit() {
            module.push('_');
        }
        module.push(if valid { ch } else { '_' });
    }
    if module.is_empty() {
        return Err("cannot derive a module name from the current directory".into());
    }
    Ok(module)
}

fn init_project(module: Option<String>) -> Result<(), String> {
    let root = std::env::current_dir().map_err(|e| format!("cannot get current directory: {e}"))?;
    let config_path = root.join("vl.toml");
    if config_path.exists() {
        return Err(format!("{} already exists", config_path.display()));
    }
    let module = match module {
        Some(module) => module,
        None => default_init_module(&root)?,
    };
    validate_module_name(&module)?;
    fs::create_dir_all(root.join("src"))
        .map_err(|e| format!("cannot create source folder: {e}"))?;
    let contents = format!("module = \"{module}\"\nsource = \"src\"\nout = \"out\"\n");
    fs::write(&config_path, contents)
        .map_err(|e| format!("cannot write {}: {e}", config_path.display()))?;
    println!("created {}", config_path.display());
    Ok(())
}

fn run_project_script(name: Option<&str>) -> ExitCode {
    let project = match load_project(Path::new(".")) {
        Ok(project) => project,
        Err(message) => {
            emit_driver_error(&message, "E602");
            return ExitCode::from(2);
        }
    };
    let name = name.unwrap_or("run");
    let Some(command) = project.config.scripts.get(name) else {
        emit_driver_error(&format!("project script `{name}` is not defined"), "E604");
        return ExitCode::from(2);
    };

    #[cfg(windows)]
    let mut process = {
        let mut process = Command::new("cmd");
        process.args(["/C", command]);
        process
    };
    #[cfg(not(windows))]
    let mut process = {
        let mut process = Command::new("sh");
        process.args(["-c", command]);
        process
    };

    let status = match process
        .current_dir(&project.root)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
    {
        Ok(status) => status,
        Err(error) => {
            emit_driver_error(
                &format!("cannot run project script `{name}`: {error}"),
                "E605",
            );
            return ExitCode::from(2);
        }
    };

    match status.code() {
        // `ExitCode::from` only accepts a `u8`, but Windows child statuses
        // may use the full `i32` range. Exit directly so scripts preserve
        // their platform exit status instead of truncating it.
        Some(code) => std::process::exit(code),
        None => {
            emit_driver_error(
                &format!("project script `{name}` terminated without an exit code"),
                "E605",
            );
            ExitCode::from(1)
        }
    }
}

fn resolve_project_path(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn normalized_path(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other.as_os_str()),
        }
    }
    result
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    let left = canonical_or_normalized(left);
    let right = canonical_or_normalized(right);
    left.starts_with(&right) || right.starts_with(&left)
}

fn canonical_or_normalized(path: &Path) -> PathBuf {
    if let Ok(path) = path.canonicalize() {
        return path;
    }
    let mut suffix = Vec::new();
    let mut ancestor = path;
    while let Some(name) = ancestor.file_name() {
        suffix.push(name.to_owned());
        ancestor = ancestor.parent().unwrap_or_else(|| Path::new("."));
        if let Ok(mut resolved) = ancestor.canonicalize() {
            suffix.reverse();
            for component in suffix {
                resolved.push(component);
            }
            return resolved;
        }
    }
    normalized_path(path)
}

fn discover_project(file: &Path) -> Result<Option<Project>, String> {
    let absolute = file
        .canonicalize()
        .map_err(|e| format!("cannot resolve {}: {e}", file.display()))?;
    let mut dir = absolute
        .parent()
        .ok_or_else(|| format!("cannot find parent of {}", file.display()))?
        .to_path_buf();
    loop {
        if dir.join("vl.toml").is_file() {
            let project = load_project(&dir)?;
            let source = resolve_project_path(&dir, &project.config.source)
                .canonicalize()
                .ok();
            if source
                .as_ref()
                .is_some_and(|source| absolute.starts_with(source))
            {
                return Ok(Some(project));
            }
        }
        if !dir.pop() {
            return Ok(None);
        }
    }
}

fn collect_vl_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(dir)
        .map_err(|e| format!("cannot read source folder {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot read source directory entry: {e}"))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| format!("cannot inspect {}: {e}", path.display()))?;
        if file_type.is_dir() {
            collect_vl_files(&path, files)?;
        } else if file_type.is_file() && path.extension().is_some_and(|ext| ext == "vl") {
            files.push(path);
        }
    }
    Ok(())
}

fn project_module_for_file(
    config: &ProjectConfig,
    source_root: &Path,
    file: &Path,
) -> Result<String, String> {
    let relative = file.strip_prefix(source_root).map_err(|_| {
        format!(
            "source file {} is outside source folder {}",
            file.display(),
            source_root.display()
        )
    })?;
    let stem = relative
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .ok_or_else(|| format!("cannot derive a module name from {}", file.display()))?;
    let mut parts = vec![config.module.clone()];
    if let Some(parent) = relative.parent() {
        for component in parent.components() {
            if let Component::Normal(name) = component {
                let name = name
                    .to_str()
                    .ok_or_else(|| format!("non-UTF-8 source path {}", file.display()))?;
                parts.push(name.to_owned());
            }
        }
    }
    parts.push(stem.to_owned());
    let module = parts.join(".");
    validate_module_name(&module).map_err(|message| {
        format!(
            "source file {} produces an invalid module: {message}",
            file.display()
        )
    })?;
    Ok(module)
}

fn source_module_collides(
    module: &str,
    compiler_modules: &[vl_common::ModuleSpec],
    target_modules: &[vl_common::ModuleSpec],
) -> bool {
    module == "std"
        || module.starts_with("std.")
        || target_modules
            .iter()
            .any(|target| target.path.as_string() == module)
        || compiler_modules
            .iter()
            .filter(|catalog| {
                catalog
                    .path
                    .segments()
                    .first()
                    .is_some_and(|root| root != "std")
            })
            .any(|catalog| catalog.path.as_string() == module)
}

fn project_output_path(out_dir: &Path, module: &str, extension: &str) -> PathBuf {
    out_dir.join(format!("{}.{}", module.replace('.', "__"), extension))
}

fn project_output_extension(emit: Option<Emit>, target: &str) -> String {
    match emit {
        Some(Emit::Tokens) => "tokens".into(),
        Some(Emit::Ast) => "ast".into(),
        Some(Emit::Lir) => "lir".into(),
        Some(Emit::Asm) | None => vl_codegen::lookup(target)
            .map(|backend| backend.name().to_owned())
            .unwrap_or_else(|| "out".into()),
    }
}

fn write_project_output(path: &Path, output: &ProjectOutput) -> Result<(), String> {
    match output {
        ProjectOutput::Text(text) => fs::write(path, text),
        ProjectOutput::Bytes(bytes) => fs::write(path, bytes),
    }
    .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Full frontend: returns typed artifacts or the collected diagnostics.
struct Frontend {
    lir: vl_lir::LirProgram,
    diags: Vec<vl_common::Diagnostic>,
}

/// Embedded standard library, loaded once: `std.string` / `std.math` /
/// `std.fmt` VL helpers merged into the module catalog and linked inline.
fn stdlib() -> &'static vl_stdlib::Stdlib {
    static STDLIB: std::sync::OnceLock<vl_stdlib::Stdlib> = std::sync::OnceLock::new();
    STDLIB.get_or_init(vl_stdlib::load)
}

/// Resolve `modules` plus the stdlib helper exports.
fn modules_with_stdlib(modules: &[vl_common::ModuleSpec]) -> Vec<vl_common::ModuleSpec> {
    let mut catalog = modules.to_vec();
    stdlib().extend_catalog(&mut catalog);
    catalog
}

/// Immutable checked stdlib modules as world refs for
/// [`vl_frontend::check_text`].
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

/// Single-file check: validate frontend + world plan, stopping successfully
/// after validation (no lowering). Used by `vl check` for standalone files.
/// Thin wrapper over the shared in-memory [`vl_frontend::check_text`].
fn run_frontend_check(
    text: &str,
    modules: &[vl_common::ModuleSpec],
    module: &str,
) -> Result<Vec<vl_common::Diagnostic>, Vec<vl_common::Diagnostic>> {
    let catalog = modules_with_stdlib(modules);
    let extra = stdlib_world_refs();
    vl_frontend::check_text(text, module, &catalog, &extra).map(|ok| ok.diags)
}

fn run_frontend(
    text: &str,
    modules: &[vl_common::ModuleSpec],
    module: &str,
) -> Result<Frontend, Vec<vl_common::Diagnostic>> {
    // Referenced stdlib helpers resolve through the merged catalog and
    // link inline below; nothing is implicitly in scope. Checked by the
    // shared in-memory frontend; lowering here only runs on success.
    let catalog = modules_with_stdlib(modules);
    let extra = stdlib_world_refs();
    let ok = vl_frontend::check_text(text, module, &catalog, &extra)?;
    let mut lir = vl_lir::lower_project(&ok.hir, &ok.typed, &ok.plan);
    stdlib().link_with_plan(&mut lir, &ok.plan);
    // Standalone builds are programs, whereas project builds set ownership
    // explicitly below. This preserves the existing single-file CLI behavior.
    lir.entrypoint = true;
    lir.entrypoint_module = Some(module.to_owned());
    Ok(Frontend {
        lir,
        diags: ok.diags,
    })
}

/// Use exactly the merged catalog consumed by the frontend, including generic
/// helper templates and target natives. Prose belongs to the docs site.
fn stdlib_catalog_json() -> serde_json::Value {
    let mut catalog = modules_with_stdlib(&vl_codegen::modules());
    catalog.sort_by_key(|module| module.path.as_string());
    serde_json::Value::Array(
        catalog
            .into_iter()
            .map(|module| {
                let path = module.path.as_string();
                let local_type =
                    |ty: &vl_common::VlType| ty.to_string().replace(&format!("{path}."), "");
                let functions: Vec<_> = module
                    .exports
                    .iter()
                    .map(|export| {
                        let params: Vec<_> = export.sig.params.iter().map(|param| {
                        serde_json::json!({ "name": param.name, "type": local_type(&param.ty) })
                    }).collect();
                        let type_params: Vec<_> = export
                            .sig
                            .type_params
                            .iter()
                            .map(|param| match param.bound {
                                Some(bound) => format!("{} extends {bound}", param.name),
                                None => param.name.clone(),
                            })
                            .collect();
                        serde_json::json!({
                            "name": export.name,
                            "type_params": type_params,
                            "params": params,
                            "returns": { "type": local_type(&export.sig.ret) },
                            "implementation": match export.kind {
                                vl_common::ExportKind::Source => "source",
                                vl_common::ExportKind::Target => "native",
                            },
                        })
                    })
                    .collect();
                let errors: Vec<_> = module
                    .errors
                    .iter()
                    .map(|error| {
                        let variants: Vec<_> =
                            error.variants.iter().map(|variant| &variant.name).collect();
                        serde_json::json!({ "name": error.name, "variants": variants })
                    })
                    .collect();
                serde_json::json!({ "module": path, "functions": functions, "errors": errors })
            })
            .collect(),
    )
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Stdlib => {
            let json = serde_json::to_string_pretty(&stdlib_catalog_json())
                .expect("the standard-library catalog contains only JSON values");
            write_out(&None, &format!("{json}\n"));
            ExitCode::SUCCESS
        }
        Cmd::Init { module } => match init_project(module) {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => {
                emit_driver_error(&message, "E602");
                ExitCode::from(2)
            }
        },
        Cmd::Lex { file } => {
            let (name, text) = match read_input(&file) {
                Ok(v) => v,
                Err(e) => {
                    emit_driver_error(&e, "E600");
                    return ExitCode::from(2);
                }
            };
            let (toks, diags) = vl_lex::lex(&text);
            let token_dump = toks
                .iter()
                .map(|t| format!("{:?}\t{:?}\n", t.kind, t.span))
                .collect::<String>();
            write_out(&None, &token_dump);
            if emit_all(&diags, &name, &text) {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
        Cmd::Parse { file } => {
            let (name, text) = match read_input(&file) {
                Ok(v) => v,
                Err(e) => {
                    emit_driver_error(&e, "E600");
                    return ExitCode::from(2);
                }
            };
            let (toks, mut diags) = vl_lex::lex(&text);
            let (ast, mut d) = vl_syntax::parse(&toks, &text);
            diags.append(&mut d);
            write_out(&None, &format!("{ast:#?}\n"));
            if emit_all(&diags, &name, &text) {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
        Cmd::Check { file, format } => {
            let json = format == Format::Json;
            if is_stdin(&file) {
                let (name, text) = match read_stdin() {
                    Ok(v) => v,
                    Err(e) => {
                        if json {
                            let diag = driver_diagnostic(&e, "E600");
                            emit_json(&[("<driver>", "", std::slice::from_ref(&diag))]);
                        } else {
                            emit_driver_error(&e, "E600");
                        }
                        return ExitCode::from(2);
                    }
                };
                return check_single_text(&name, &text, "stdin", format);
            }
            match discover_project(&file) {
                Ok(Some(project)) => return check_project(&project, format),
                Ok(None) => {}
                Err(message) => {
                    if json {
                        let diag = driver_diagnostic(&message, "E602");
                        emit_json(&[("<driver>", "", std::slice::from_ref(&diag))]);
                    } else {
                        emit_driver_error(&message, "E602");
                    }
                    return ExitCode::from(2);
                }
            }
            let (name, text) = match read_input(&file) {
                Ok(v) => v,
                Err(e) => {
                    if json {
                        let diag = driver_diagnostic(&e, "E600");
                        emit_json(&[("<driver>", "", std::slice::from_ref(&diag))]);
                    } else {
                        emit_driver_error(&e, "E600");
                    }
                    return ExitCode::from(2);
                }
            };
            let module = source_module(&file);
            check_single_text(&name, &text, &module, format)
        }
        Cmd::Build {
            file,
            target,
            emit,
            out,
            format,
        } => match file {
            Some(file) => {
                if is_stdin(&file) {
                    return build_single(&file, &target, emit, &out, format);
                }
                match discover_project(&file) {
                    Ok(Some(project)) => {
                        build_project_at(&project, &target, emit, out.as_ref(), format)
                    }
                    Ok(None) => build_single(&file, &target, emit, &out, format),
                    Err(message) => {
                        if format == Format::Json {
                            let diag = driver_diagnostic(&message, "E602");
                            emit_json(&[("<driver>", "", std::slice::from_ref(&diag))]);
                        } else {
                            emit_driver_error(&message, "E602");
                        }
                        ExitCode::from(2)
                    }
                }
            }
            None => build_project(&target, emit, out.as_ref(), format),
        },
        Cmd::Run { name } => run_project_script(name.as_deref()),
        Cmd::Fmt { paths, check } => fmt_paths(&paths, check),
        Cmd::Lsp => match vl_lsp::run_stdio() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                emit_driver_error(&format!("language server failed: {error}"), "E606");
                ExitCode::from(2)
            }
        },
        Cmd::Targets => {
            let mut output = String::new();
            for t in vl_codegen::all_targets() {
                output.push_str(t);
                output.push('\n');
            }
            write_out(&None, &output);
            ExitCode::SUCCESS
        }
    }
}

/// Single-text check shared by the `vl check` file/stdin paths.
fn check_single_text(name: &str, text: &str, module: &str, fmt: Format) -> ExitCode {
    let json = fmt == Format::Json;
    // `check` validates frontend semantics independently of a codegen
    // target; target capability checks belong to `build`. It stops
    // after validation (no lowering).
    let modules = vl_codegen::modules();
    match run_frontend_check(text, &modules, module) {
        Ok(diags) => {
            if json {
                emit_json(&[(name, text, diags.as_slice())]);
            } else {
                emit_all(&diags, name, text);
                write_out(&None, &format!("ok: {name} checks clean\n"));
            }
            ExitCode::SUCCESS
        }
        Err(diags) => {
            let failed = if json {
                emit_json(&[(name, text, diags.as_slice())])
            } else {
                emit_all(&diags, name, text)
            };
            debug_assert!(failed);
            ExitCode::from(1)
        }
    }
}

fn build_single(
    file: &Path,
    target: &str,
    emit: Option<Emit>,
    out: &Option<PathBuf>,
    fmt: Format,
) -> ExitCode {
    let json = fmt == Format::Json;
    // Machine mode keeps stdout pure JSON: dumps and artifacts need `--out`.
    if json && out.is_none() {
        let diag = driver_diagnostic(
            "cannot use `--format json` without `--out` (stdout carries the JSON report)",
            "E601",
        );
        emit_json(&[("<driver>", "", std::slice::from_ref(&diag))]);
        return ExitCode::from(2);
    }
    let (name, text) = if is_stdin(file) {
        match read_stdin() {
            Ok(v) => v,
            Err(e) => {
                if json {
                    let diag = driver_diagnostic(&e, "E600");
                    emit_json(&[("<driver>", "", std::slice::from_ref(&diag))]);
                } else {
                    emit_driver_error(&e, "E600");
                }
                return ExitCode::from(2);
            }
        }
    } else {
        match read_input(file) {
            Ok(v) => v,
            Err(e) => {
                if json {
                    let diag = driver_diagnostic(&e, "E600");
                    emit_json(&[("<driver>", "", std::slice::from_ref(&diag))]);
                } else {
                    emit_driver_error(&e, "E600");
                }
                return ExitCode::from(2);
            }
        }
    };
    // Lex/parse emits are intentionally shallow: they remain useful for
    // broken or incomplete files and do not require frontend/backend support.
    if matches!(emit, Some(Emit::Tokens) | Some(Emit::Ast)) {
        let (toks, mut diags) = vl_lex::lex(&text);
        let dump = if matches!(emit, Some(Emit::Ast)) {
            let (ast, mut parse_diags) = vl_syntax::parse(&toks, &text);
            diags.append(&mut parse_diags);
            format!("{ast:#?}\n")
        } else {
            format!("{toks:#?}\n")
        };
        let write_failed = if let Err(diag) = try_write_out(out, dump.as_bytes()) {
            diags.push(diag);
            true
        } else {
            false
        };
        let failed = if json {
            emit_json(&[(name.as_str(), text.as_str(), diags.as_slice())])
        } else {
            emit_all(&diags, &name, &text)
        };
        return if write_failed {
            ExitCode::from(2)
        } else if failed {
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        };
    }
    // LIR is target-neutral: it can be dumped with an unknown target name and
    // with modules only supported by another backend. Backend lookup applies
    // only to final assembly.
    let target_specific = matches!(emit, Some(Emit::Asm) | None);
    let modules = if target_specific {
        vl_codegen::modules_for_target(target)
    } else {
        vl_codegen::modules()
    };
    let module = if is_stdin(file) {
        "stdin".to_string()
    } else {
        source_module(file)
    };
    let fe = match run_frontend(&text, &modules, &module) {
        Ok(fe) => fe,
        Err(diags) => {
            if json {
                emit_json(&[(name.as_str(), text.as_str(), diags.as_slice())]);
            } else {
                emit_all(&diags, &name, &text);
            }
            return ExitCode::from(1);
        }
    };

    if let Some(dump) = matches!(emit, Some(Emit::Lir)).then(|| fe.lir.dump()) {
        let mut diags = fe.diags;
        let write_failed = if let Err(diag) = try_write_out(out, dump.as_bytes()) {
            diags.push(diag);
            true
        } else {
            false
        };
        if json {
            emit_json(&[(name.as_str(), text.as_str(), diags.as_slice())]);
        } else {
            emit_all(&diags, &name, &text);
        }
        return if write_failed {
            ExitCode::from(2)
        } else {
            ExitCode::SUCCESS
        };
    }

    let Some(backend) = vl_codegen::lookup(target) else {
        let d = vl_common::Diagnostic::error(format!(
            "unknown target `{target}` (have: {})",
            vl_codegen::all_targets().join(", ")
        ))
        .with_code("E501");
        if json {
            emit_json(&[(name.as_str(), text.as_str(), std::slice::from_ref(&d))]);
        } else {
            emit_all(&[d], &name, &text);
        }
        return ExitCode::from(2);
    };

    let (artifact, backend_diags) = backend.emit(&fe.lir);
    let mut diags = fe.diags;
    diags.extend(backend_diags);
    let write_failed = artifact.as_ref().is_some_and(|a| {
        if let Err(diag) = write_artifact(out, a) {
            diags.push(diag);
            true
        } else {
            false
        }
    });
    let failed = if json {
        emit_json(&[(name.as_str(), text.as_str(), diags.as_slice())])
    } else {
        emit_all(&diags, &name, &text)
    };
    if write_failed {
        ExitCode::from(2)
    } else if failed || artifact.is_none() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

#[allow(dead_code)]
fn compile_project_source(
    text: &str,
    module: &str,
    target: &str,
    emit: Option<Emit>,
) -> ProjectFileBuild {
    if matches!(emit, Some(Emit::Tokens) | Some(Emit::Ast)) {
        let (toks, mut diags) = vl_lex::lex(text);
        let output = if matches!(emit, Some(Emit::Ast)) {
            let (ast, mut parse_diags) = vl_syntax::parse_with_module(&toks, text, module);
            diags.append(&mut parse_diags);
            ProjectOutput::Text(format!("{ast:#?}\n"))
        } else {
            ProjectOutput::Text(format!("{toks:#?}\n"))
        };
        return ProjectFileBuild {
            output: Some(output),
            diags,
        };
    }

    let target_specific = matches!(emit, Some(Emit::Asm) | None);
    let modules = if target_specific {
        vl_codegen::modules_for_target(target)
    } else {
        vl_codegen::modules()
    };
    let frontend = match run_frontend(text, &modules, module) {
        Ok(frontend) => frontend,
        Err(diags) => {
            return ProjectFileBuild {
                output: None,
                diags,
            };
        }
    };

    if matches!(emit, Some(Emit::Lir)) {
        return ProjectFileBuild {
            output: Some(ProjectOutput::Text(frontend.lir.dump())),
            diags: frontend.diags,
        };
    }

    let backend = vl_codegen::lookup(target).expect("project target was validated before building");
    let (artifact, mut diags) = backend.emit(&frontend.lir);
    diags.extend(frontend.diags);
    let output = artifact.map(|artifact| match artifact.bytes {
        Some(bytes) => ProjectOutput::Bytes(bytes),
        None => ProjectOutput::Text(artifact.text),
    });
    ProjectFileBuild { output, diags }
}

/// One checked source unit retained for the batch frontend.
struct CheckedUnit {
    hir: vl_hir::HirProgram,
    typed: vl_typecheck::TypedProgram,
}

/// One parsed source unit: filename, text, module, AST, diagnostics, and
/// whether to skip the frontend (duplicate/collision).
type ProjectUnit = (
    String,
    String,
    String,
    vl_syntax::Program,
    Vec<vl_common::Diagnostic>,
    bool,
);

/// Batch frontend for projects: resolve, lower, and typecheck every clean
/// unit while retaining its HIR/typed result, then run the project-wide
/// monomorphization fixed point. Returns the checked units (in `units` order),
/// the shared plan, and per-unit diagnostics appended to each unit's existing
/// `diags`. If any frontend errors exist, monomorphization and artifact
/// generation are skipped (the driver emits per-unit diagnostics and stops).
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn batch_frontend(
    units: &mut [ProjectUnit],
    modules: &[vl_common::ModuleSpec],
    entrypoint_module: Option<&str>,
) -> Option<(
    Vec<Option<CheckedUnit>>,
    vl_typecheck::world::MonomorphizationPlan,
)> {
    // Resolve + lower + check every clean unit, retaining results.
    let catalog = modules_with_stdlib(modules);
    let mut checked: Vec<Option<CheckedUnit>> = Vec::with_capacity(units.len());
    for (_, _, _module, ast, unit_diags, skip_frontend) in units.iter_mut() {
        if *skip_frontend || unit_diags.iter().any(|d| d.is_error()) {
            checked.push(None);
            continue;
        }
        let (res, mut d) = vl_semantic::resolve_with_modules(ast, &catalog);
        // `main` signature is checked wherever it is declared.
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
        if !mains.is_empty() {
            if let Some((count, ret, span)) = mains.first() {
                if *count != 0 {
                    d.push(
                        vl_common::Diagnostic::error("`main` must not take parameters")
                            .with_label(*span, "entrypoint declared here")
                            .with_code("E401"),
                    );
                }
                if let Some(diag) = main_ret_error(ret, *span) {
                    d.push(diag);
                }
            }
        }
        // If resolution already failed, skip lowering/checking but retain the
        // diagnostics (lowering is blocked anyway).
        if d.iter().any(|diag| diag.is_error()) {
            unit_diags.append(&mut d);
            checked.push(None);
            continue;
        }
        let hir = vl_hir::lower(ast, &res);
        let (typed, mut td) = vl_typecheck::check_with_modules(&hir, &catalog);
        d.append(&mut td);
        if !res.poisoned_imports {
            d.append(&mut typed.validate_normalized(&hir, &d));
        }
        let _ = entrypoint_module;
        unit_diags.append(&mut d);
        if unit_diags.iter().any(|diag| diag.is_error()) {
            checked.push(None);
            continue;
        }
        checked.push(Some(CheckedUnit { hir, typed }));
    }
    if units
        .iter()
        .any(|(_, _, _, _, diags, _)| diags.iter().any(|d| d.is_error()))
    {
        return None;
    }
    // Combine checked project modules with immutable checked stdlib modules
    // and run the global fixed point. Ordering is by module name (units are
    // already sorted), so LIR and diagnostics are deterministic. Stdlib
    // templates use the same discovery process but copy into the consumer
    // (they have no separate artifacts).
    let refs: Vec<(&vl_hir::HirProgram, &vl_typecheck::TypedProgram)> = checked
        .iter()
        .filter_map(|c| c.as_ref().map(|u| (&u.hir, &u.typed)))
        .collect();
    let stdlib_checked = stdlib().checked_modules();
    let mut world_refs: Vec<(&vl_hir::HirProgram, &vl_typecheck::TypedProgram)> =
        Vec::with_capacity(refs.len() + stdlib_checked.len());
    world_refs.extend(refs.iter().copied());
    for (hir, typed) in stdlib_checked {
        world_refs.push((hir, typed));
    }
    let (plan, world_diags) = vl_typecheck::world::plan_world(&world_refs);
    // Route world diagnostics to their owner module.
    for (owner, diag) in world_diags {
        if let Some(idx) = units.iter().position(|(_, _, m, _, _, _)| m == &owner) {
            units[idx].4.push(diag);
        } else if let Some(first) = units.first_mut() {
            first.4.push(diag);
        }
    }
    if units
        .iter()
        .any(|(_, _, _, _, diags, _)| diags.iter().any(|d| d.is_error()))
    {
        return None;
    }
    // Validate normalized types for ordinary code (already done per unit) and
    // every planned instance (signatures, arguments, and substituted bodies).
    // A surviving `Int`, `Param`, or nested `Error` remains an `E500`.
    {
        let prior: Vec<vl_common::Diagnostic> = units
            .iter()
            .flat_map(|(_, _, _, _, diags, _)| diags.iter().cloned())
            .collect();
        for (owner, diag) in vl_typecheck::world::validate_plan(&plan, &world_refs, &prior) {
            if let Some(idx) = units.iter().position(|(_, _, m, _, _, _)| m == &owner) {
                units[idx].4.push(diag);
            } else if let Some(first) = units.first_mut() {
                // Stdlib owners have no user unit; attribute defensively.
                first.4.push(diag);
            }
        }
        if units
            .iter()
            .any(|(_, _, _, _, diags, _)| diags.iter().any(|d| d.is_error()))
        {
            return None;
        }
    }
    Some((checked, plan))
}

fn build_project_at(
    project: &Project,
    target: &str,
    emit: Option<Emit>,
    out_override: Option<&PathBuf>,
    fmt: Format,
) -> ExitCode {
    let json = fmt == Format::Json;
    // JSON mode aggregates every diagnostic into one stdout document;
    // driver problems accumulate instead of printing immediately.
    let mut driver_diags: Vec<vl_common::Diagnostic> = Vec::new();
    let mut json_entries: Vec<(String, String, Vec<vl_common::Diagnostic>)> = Vec::new();
    let source_dir =
        match resolve_project_path(&project.root, &project.config.source).canonicalize() {
            Ok(path) => path,
            Err(error) => {
                return fail_driver(
                    fmt,
                    &format!("cannot resolve project source folder: {error}"),
                    "E603",
                );
            }
        };
    if !source_dir.is_dir() {
        return fail_driver(
            fmt,
            &format!(
                "project source folder does not exist: {}",
                source_dir.display()
            ),
            "E603",
        );
    }

    let mut files = Vec::new();
    if let Err(message) = collect_vl_files(&source_dir, &mut files) {
        return fail_driver(fmt, &message, "E603");
    }
    files.sort();
    if files.is_empty() {
        return fail_driver(
            fmt,
            &format!("no `.vl` files found in {}", source_dir.display()),
            "E603",
        );
    }

    let needs_backend = matches!(emit, Some(Emit::Asm) | None);
    if needs_backend && vl_codegen::lookup(target).is_none() {
        let diagnostic = vl_common::Diagnostic::error(format!(
            "unknown target `{target}` (have: {})",
            vl_codegen::all_targets().join(", ")
        ))
        .with_code("E501");
        if json {
            emit_json(&[("<driver>", "", std::slice::from_ref(&diagnostic))]);
        } else {
            emit_all(&[diagnostic], "<driver>", "");
        }
        return ExitCode::from(2);
    }

    let out_dir = out_override
        .map(|path| resolve_project_path(&project.root, path))
        .unwrap_or_else(|| resolve_project_path(&project.root, &project.config.out));
    let out_dir = canonical_or_normalized(&out_dir);
    if paths_overlap(&source_dir, &out_dir) {
        return fail_driver(
            fmt,
            &format!("output path overlaps project source: {}", out_dir.display()),
            "E602",
        );
    }
    if out_dir.exists() && !out_dir.is_dir() {
        return fail_driver(
            fmt,
            &format!("output path is not a directory: {}", out_dir.display()),
            "E601",
        );
    }
    let Some(out_parent) = out_dir.parent() else {
        return fail_driver(fmt, "cannot determine output folder parent", "E601");
    };
    if let Err(e) = fs::create_dir_all(out_parent) {
        return fail_driver(
            fmt,
            &format!("cannot create output parent {}: {e}", out_parent.display()),
            "E601",
        );
    }

    let mut units = Vec::new();
    let mut driver_failed = false;
    for file in files {
        let (filename, text) = match read_input(&file) {
            Ok(input) => input,
            Err(message) => {
                if json {
                    driver_diags.push(driver_diagnostic(&message, "E600"));
                } else {
                    emit_driver_error(&message, "E600");
                }
                driver_failed = true;
                continue;
            }
        };
        let module = match project_module_for_file(&project.config, &source_dir, &file) {
            Ok(module) => module,
            Err(message) => {
                if json {
                    driver_diags.push(driver_diagnostic(&message, "E602"));
                } else {
                    emit_driver_error(&message, "E602");
                }
                driver_failed = true;
                continue;
            }
        };
        let (tokens, mut diags) = vl_lex::lex(&text);
        let (ast, mut parse_diags) = vl_syntax::parse_with_module(&tokens, &text, &module);
        diags.append(&mut parse_diags);
        units.push((filename, text, module, ast, diags, false));
    }
    let target_modules = if matches!(emit, Some(Emit::Asm) | None) {
        vl_codegen::modules_for_target(target)
    } else {
        vl_codegen::modules()
    };
    let compiler_modules = vl_codegen::modules();
    let mut modules = target_modules.clone();
    let mut source_names = HashSet::new();
    let mut entrypoint_module = None;
    let mut catalog_collision_reported = false;
    let shallow_emit = matches!(emit, Some(Emit::Tokens) | Some(Emit::Ast));
    for (_, _, module, ast, unit_diags, _) in &mut units {
        let interface = if shallow_emit {
            None
        } else {
            let (interface, mut interface_diags) = vl_semantic::collect_interface_quiet(ast);
            let mut interface = interface;
            interface.parse_poisoned = !unit_diags.is_empty();
            unit_diags.append(&mut interface_diags);
            Some(interface)
        };
        let new_source = source_names.insert(module.clone());
        if !new_source {
            if json {
                driver_diags.push(driver_diagnostic(
                    &format!("duplicate source module `{module}`"),
                    "E602",
                ));
            } else {
                emit_driver_error(&format!("duplicate source module `{module}`"), "E602");
            }
            driver_failed = true;
        }
        let collides = source_module_collides(module, &compiler_modules, &target_modules);
        if collides {
            if !catalog_collision_reported {
                if json {
                    driver_diags.push(driver_diagnostic(
                        &format!("source module `{module}` is reserved by the compiler module catalog (collides with a target module)"),
                        "E602",
                    ));
                } else {
                    emit_driver_error(
                        &format!("source module `{module}` is reserved by the compiler module catalog (collides with a target module)"),
                        "E602",
                    );
                }
                catalog_collision_reported = true;
            }
            driver_failed = true;
        }
        if !new_source {
            continue;
        }
        if !collides {
            if let Some(interface) = &interface {
                modules.push(interface.as_spec());
            }
        }
        if !shallow_emit && !collides && new_source {
            for item in &ast.items {
                if let vl_syntax::Item::Function { name, span, .. } = item {
                    if name == "main" {
                        if entrypoint_module.is_some() {
                            unit_diags.push(
                                vl_common::Diagnostic::error(
                                    "project defines more than one `main` function",
                                )
                                .with_label(*span, "additional entrypoint declared here")
                                .with_code("E401"),
                            );
                        } else {
                            entrypoint_module = Some(module.clone());
                        }
                    }
                }
            }
        }
    }
    modules.sort_by_key(|module| module.path.as_string());
    units.sort_by(|left, right| left.2.cmp(&right.2));
    let mut failed = false;
    let mut outputs = Vec::new();
    let extension = project_output_extension(emit, target);
    // Shallow emits stay per-unit (no frontend needed).
    if matches!(emit, Some(Emit::Tokens) | Some(Emit::Ast)) {
        for (filename, text, module, ast, unit_diags, _) in units {
            let output = if matches!(emit, Some(Emit::Ast)) {
                ProjectOutput::Text(format!("{ast:#?}\n"))
            } else {
                let (tokens, _) = vl_lex::lex(&text);
                ProjectOutput::Text(format!("{tokens:#?}\n"))
            };
            let built = ProjectFileBuild {
                output: Some(output),
                diags: unit_diags,
            };
            let ProjectFileBuild { output, diags } = built;
            if json {
                failed |= diags.iter().any(|d| d.is_error());
                json_entries.push((filename, text, diags));
            } else {
                failed |= emit_all(&diags, &filename, &text);
            }
            if let Some(output) = output {
                let path = project_output_path(&out_dir, &module, &extension);
                outputs.push((path, output));
            }
        }
    } else {
        // Batch frontend: check all units, run the world fixed point, then
        // lower every module with the complete plan. No provider artifact is
        // emitted before all importers have been checked.
        let batch = batch_frontend(&mut units, &modules, entrypoint_module.as_deref());
        // Emit per-unit diagnostics (frontend + world) in module order.
        for (filename, text, _, _, unit_diags, _) in &units {
            if json {
                failed |= unit_diags.iter().any(|d| d.is_error());
                json_entries.push((filename.clone(), text.clone(), unit_diags.clone()));
            } else {
                failed |= emit_all(unit_diags, filename, text);
            }
        }
        if let Some((checked, plan)) = batch {
            for (idx, (filename, text, module, _, _, _)) in units.iter().enumerate() {
                let Some(unit) = checked[idx].as_ref() else {
                    continue;
                };
                let mut lir = vl_lir::lower_project(&unit.hir, &unit.typed, &plan);
                stdlib().link_with_plan(&mut lir, &plan);
                lir.entrypoint = entrypoint_module.as_deref() == Some(module.as_str());
                lir.entrypoint_module = entrypoint_module.clone();
                if matches!(emit, Some(Emit::Lir)) {
                    outputs.push((
                        project_output_path(&out_dir, module, &extension),
                        ProjectOutput::Text(lir.dump()),
                    ));
                    let _ = (filename, text);
                } else {
                    let backend = vl_codegen::lookup(target)
                        .expect("project target was validated before building");
                    let (artifact, backend_diags) = backend.emit(&lir);
                    if json {
                        if backend_diags.iter().any(|d| d.is_error()) {
                            failed = true;
                        }
                        json_entries.push((filename.clone(), text.clone(), backend_diags));
                    } else if emit_all(&backend_diags, filename, text) {
                        failed = true;
                    }
                    if let Some(a) = artifact {
                        outputs.push((
                            project_output_path(&out_dir, module, &extension),
                            a.bytes
                                .map(ProjectOutput::Bytes)
                                .unwrap_or(ProjectOutput::Text(a.text)),
                        ));
                    } else {
                        failed = true;
                    }
                }
            }
            // If world validation failed after lowering (defensive), `batch`
            // would have been `None`; reaching here means lowering is allowed.
        } else {
            // Frontend or world errors already emitted above; no artifacts.
            failed = true;
        }
    }
    // Final artifacts are transactional: preflight every destination, write
    // the complete set to a private directory, then publish it.
    if (!failed && !driver_failed) || (shallow_emit && !driver_failed) {
        let mut paths = HashSet::new();
        if let Some((path, _)) = outputs.iter().find(|(path, _)| !paths.insert(path.clone())) {
            if json {
                driver_diags.push(driver_diagnostic(
                    &format!("multiple source files produce {}", path.display()),
                    "E602",
                ));
            } else {
                emit_driver_error(
                    &format!("multiple source files produce {}", path.display()),
                    "E602",
                );
            }
            driver_failed = true;
        }
        if !driver_failed {
            let output_name = out_dir
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("out");
            let staging =
                out_parent.join(format!(".{output_name}.vl-staging-{}", std::process::id()));
            let backup =
                out_parent.join(format!(".{output_name}.vl-backup-{}", std::process::id()));
            if staging.exists() {
                if json {
                    driver_diags.push(driver_diagnostic(
                        &format!("staging path already exists: {}", staging.display()),
                        "E601",
                    ));
                } else {
                    emit_driver_error(
                        &format!("staging path already exists: {}", staging.display()),
                        "E601",
                    );
                }
                driver_failed = true;
            } else if let Err(error) = fs::create_dir(&staging) {
                if json {
                    driver_diags.push(driver_diagnostic(
                        &format!(
                            "cannot create staging folder {}: {error}",
                            staging.display()
                        ),
                        "E601",
                    ));
                } else {
                    emit_driver_error(
                        &format!(
                            "cannot create staging folder {}: {error}",
                            staging.display()
                        ),
                        "E601",
                    );
                }
                driver_failed = true;
            } else {
                for (path, output) in &outputs {
                    let staged_path =
                        staging.join(path.file_name().expect("output path has a filename"));
                    if let Err(message) = write_project_output(&staged_path, output) {
                        if json {
                            driver_diags.push(driver_diagnostic(&message, "E601"));
                        } else {
                            emit_driver_error(&message, "E601");
                        }
                        driver_failed = true;
                        break;
                    }
                }
                if !driver_failed {
                    if backup.exists() {
                        driver_failed = true;
                        if json {
                            driver_diags.push(driver_diagnostic(
                                &format!("backup path already exists: {}", backup.display()),
                                "E601",
                            ));
                        } else {
                            emit_driver_error(
                                &format!("backup path already exists: {}", backup.display()),
                                "E601",
                            );
                        }
                    } else if out_dir.exists() && fs::rename(&out_dir, &backup).is_err() {
                        driver_failed = true;
                        if json {
                            driver_diags.push(driver_diagnostic(
                                &format!(
                                    "cannot stage existing output folder {}",
                                    out_dir.display()
                                ),
                                "E601",
                            ));
                        } else {
                            emit_driver_error(
                                &format!(
                                    "cannot stage existing output folder {}",
                                    out_dir.display()
                                ),
                                "E601",
                            );
                        }
                    } else if let Err(error) = fs::rename(&staging, &out_dir) {
                        driver_failed = true;
                        if json {
                            driver_diags.push(driver_diagnostic(
                                &format!(
                                    "cannot publish output folder {}: {error}",
                                    out_dir.display()
                                ),
                                "E601",
                            ));
                        } else {
                            emit_driver_error(
                                &format!(
                                    "cannot publish output folder {}: {error}",
                                    out_dir.display()
                                ),
                                "E601",
                            );
                        }
                        if backup.exists() {
                            let _ = fs::rename(&backup, &out_dir);
                        }
                    } else {
                        let _ = fs::remove_dir_all(&backup);
                    }
                }
                // Individual files were only written inside staging. If
                // publication failed, leave the prior output untouched.
                let _ = fs::remove_dir_all(&staging);
            }
        }
    }

    if json {
        let mut refs: Vec<(&str, &str, &[vl_common::Diagnostic])> = Vec::new();
        if !driver_diags.is_empty() {
            refs.push(("<driver>", "", &driver_diags));
        }
        for (filename, text, diags) in &json_entries {
            refs.push((filename, text, diags));
        }
        emit_json(&refs);
    }

    if driver_failed {
        ExitCode::from(2)
    } else if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn build_project(
    target: &str,
    emit: Option<Emit>,
    out_override: Option<&PathBuf>,
    fmt: Format,
) -> ExitCode {
    let project = match load_project(Path::new(".")) {
        Ok(project) => project,
        Err(message) => {
            return fail_driver(fmt, &message, "E602");
        }
    };
    build_project_at(&project, target, emit, out_override, fmt)
}

fn check_project(project: &Project, fmt: Format) -> ExitCode {
    let json = fmt == Format::Json;
    let mut driver_diags: Vec<vl_common::Diagnostic> = Vec::new();
    let source = match resolve_project_path(&project.root, &project.config.source).canonicalize() {
        Ok(path) => path,
        Err(error) => {
            return fail_driver(
                fmt,
                &format!("cannot resolve project source folder: {error}"),
                "E603",
            );
        }
    };
    let target_modules = vl_codegen::modules();
    let compiler_modules = vl_codegen::modules();
    let mut files = Vec::new();
    if let Err(message) = collect_vl_files(&source, &mut files) {
        return fail_driver(fmt, &message, "E603");
    }
    files.sort();
    let mut units = Vec::new();
    let mut modules = target_modules.clone();
    let mut failed = false;
    let mut entrypoint_module = None;
    let mut source_names = HashSet::new();
    let mut catalog_collision_reported = false;
    for file in files {
        let (name, text) = match read_input(&file) {
            Ok(value) => value,
            Err(message) => {
                if json {
                    driver_diags.push(driver_diagnostic(&message, "E600"));
                } else {
                    emit_driver_error(&message, "E600");
                }
                failed = true;
                continue;
            }
        };
        let module = match project_module_for_file(&project.config, &source, &file) {
            Ok(value) => value,
            Err(message) => {
                if json {
                    driver_diags.push(driver_diagnostic(&message, "E602"));
                } else {
                    emit_driver_error(&message, "E602");
                }
                failed = true;
                continue;
            }
        };
        let collides = source_module_collides(&module, &compiler_modules, &target_modules);
        if collides {
            if !catalog_collision_reported {
                if json {
                    driver_diags.push(driver_diagnostic(
                        &format!("source module `{module}` is reserved by the compiler module catalog (collides with a target module)"),
                        "E602",
                    ));
                } else {
                    emit_driver_error(
                        &format!("source module `{module}` is reserved by the compiler module catalog (collides with a target module)"),
                        "E602",
                    );
                }
                catalog_collision_reported = true;
            }
            failed = true;
        }
        let new_source = source_names.insert(module.clone());
        if !new_source {
            if json {
                driver_diags.push(driver_diagnostic(
                    &format!("duplicate source module `{module}`"),
                    "E602",
                ));
            } else {
                emit_driver_error(&format!("duplicate source module `{module}`"), "E602");
            }
            failed = true;
        }
        let (tokens, mut diags) = vl_lex::lex(&text);
        let (ast, mut parse_diags) = vl_syntax::parse_with_module(&tokens, &text, &module);
        diags.append(&mut parse_diags);
        let (interface, mut interface_diags) = vl_semantic::collect_interface_quiet(&ast);
        let mut interface = interface;
        interface.parse_poisoned = !diags.is_empty();
        diags.append(&mut interface_diags);
        if !collides && new_source {
            for item in &ast.items {
                if let vl_syntax::Item::Function { name, span, .. } = item {
                    if name == "main" {
                        if entrypoint_module.is_some() {
                            diags.push(
                                vl_common::Diagnostic::error(
                                    "project defines more than one `main` function",
                                )
                                .with_label(*span, "additional entrypoint declared here")
                                .with_code("E401"),
                            );
                        } else {
                            entrypoint_module = Some(module.clone());
                        }
                    }
                }
            }
        }
        if collides {
            units.push((name, text, ast, diags, true));
            continue;
        }
        if new_source {
            // Duplicate source names report an error but retain the first
            // interface as the canonical catalog entry.
            modules.push(interface.as_spec());
        }
        units.push((name, text, ast, diags, false));
    }
    modules.sort_by_key(|module| module.path.as_string());
    // Adapt to the batch shape `(filename, text, module, ast, diags, skip)`.
    let mut batch_units: Vec<(
        String,
        String,
        String,
        vl_syntax::Program,
        Vec<vl_common::Diagnostic>,
        bool,
    )> = units
        .into_iter()
        .map(|(name, text, ast, diags, skip)| {
            let module = ast.module.clone();
            (name, text, module, ast, diags, skip)
        })
        .collect();
    batch_units.sort_by(|l, r| l.2.cmp(&r.2));
    // Batch frontend runs the world fixed point and validates; `check` stops
    // successfully after validation (no lowering).
    let batch = batch_frontend(&mut batch_units, &modules, entrypoint_module.as_deref());
    if json {
        for (_, _, _, _, diags, _) in &batch_units {
            failed |= diags.iter().any(|d| d.is_error());
        }
    } else {
        for (name, text, _, _, diags, _) in &batch_units {
            failed |= emit_all(diags, name, text);
        }
    }
    if batch.is_none()
        && !batch_units
            .iter()
            .any(|(_, _, _, _, diags, _)| diags.iter().any(|d| d.is_error()))
        && driver_diags.iter().all(|d| !d.is_error())
    {
        // Defensive: world produced no plan without diagnostics (impossible).
        if json {
            driver_diags.push(driver_diagnostic(
                "internal error: world produced no plan without diagnostics",
                "E500",
            ));
        }
        failed = true;
    }
    if json {
        let mut refs: Vec<(&str, &str, &[vl_common::Diagnostic])> = Vec::new();
        if !driver_diags.is_empty() {
            refs.push(("<driver>", "", &driver_diags));
        }
        for (name, text, _, _, diags, _) in &batch_units {
            refs.push((name, text, diags));
        }
        emit_json(&refs);
    }
    if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Collect `.vl` files from `paths` (files directly, directories
/// recursively), for `vl fmt`. Errors on missing paths and non-`.vl` files.
fn collect_fmt_files(paths: &[PathBuf], files: &mut Vec<PathBuf>) -> Result<(), String> {
    for path in paths {
        let meta = fs::symlink_metadata(path)
            .map_err(|e| format!("cannot inspect {}: {e}", path.display()))?;
        if meta.is_dir() {
            collect_vl_files(path, files)?;
        } else if meta.is_file() && path.extension().is_some_and(|ext| ext == "vl") {
            files.push(path.clone());
        } else {
            return Err(format!(
                "cannot format {}: not a `.vl` file",
                path.display()
            ));
        }
    }
    Ok(())
}

/// `vl fmt`: rewrite files canonically, or with `--check` only report the
/// ones that would change. Files with lex/parse errors are reported (exit 1)
/// and left untouched.
fn fmt_paths(paths: &[PathBuf], check: bool) -> ExitCode {
    let mut files = Vec::new();
    if let Err(message) = collect_fmt_files(paths, &mut files) {
        emit_driver_error(&message, "E600");
        return ExitCode::from(2);
    }
    files.sort();
    if files.is_empty() {
        emit_driver_error("no `.vl` files to format", "E600");
        return ExitCode::from(2);
    }
    let mut failed = false;
    let mut dirty = Vec::new();
    for file in &files {
        let (name, text) = match read_input(file) {
            Ok(v) => v,
            Err(e) => {
                emit_driver_error(&e, "E600");
                failed = true;
                continue;
            }
        };
        let module = source_module(file);
        match vl_fmt::format(&text, &module) {
            Ok(formatted) => {
                if formatted != text {
                    if check {
                        dirty.push(name);
                    } else if let Err(e) = fs::write(file, formatted) {
                        emit_driver_error(&format!("cannot write {}: {e}", file.display()), "E601");
                        failed = true;
                    }
                }
            }
            Err(diags) => {
                emit_all(&diags, &name, &text);
                failed = true;
            }
        }
    }
    if failed {
        ExitCode::from(1)
    } else if check && !dirty.is_empty() {
        for name in &dirty {
            println!("would reformat: {name}");
        }
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn try_write_out(out: &Option<PathBuf>, bytes: &[u8]) -> Result<(), vl_common::Diagnostic> {
    match out {
        Some(path) => fs::write(path, bytes).map_err(|error| {
            driver_diagnostic(&format!("cannot write {}: {error}", path.display()), "E601")
        }),
        None => {
            use std::io::Write;
            std::io::stdout().write_all(bytes).map_err(|error| {
                driver_diagnostic(&format!("cannot write stdout: {error}"), "E601")
            })
        }
    }
}

fn write_out(out: &Option<PathBuf>, text: &str) {
    if let Err(diag) = try_write_out(out, text.as_bytes()) {
        emit_driver_error(&diag.message, "E601");
        std::process::exit(2);
    }
}

fn write_artifact(
    out: &Option<PathBuf>,
    artifact: &vl_codegen::Artifact,
) -> Result<(), vl_common::Diagnostic> {
    try_write_out(
        out,
        artifact
            .bytes
            .as_deref()
            .unwrap_or(artifact.text.as_bytes()),
    )
}

fn emit_driver_error(message: &str, code: &str) {
    let diagnostic = vl_common::Diagnostic::error(message).with_code(code);
    emit_all(&[diagnostic], "<driver>", "");
}
