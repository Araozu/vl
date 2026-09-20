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
    Check { file: PathBuf },
    /// Compile a file or the current project; print or write target output.
    Build {
        /// Source file. Omit this to build the current directory's project.
        file: Option<PathBuf>,
        /// Which backend to use (target platform TBD; see `vl-codegen`).
        #[arg(long, default_value = "naravm")]
        target: String,
        /// Dump an intermediate instead of compiling.
        #[arg(long, value_enum)]
        emit: Option<Emit>,
        /// Write a single-file artifact here, or override a project's output folder.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Run a project script; omit the name to run the `run` script.
    Run { name: Option<String> },
    /// List available codegen backends.
    Targets,
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

fn read_input(file: &Path) -> Result<(String, String), String> {
    match fs::read_to_string(file) {
        Ok(text) => Ok((file.display().to_string(), text)),
        Err(e) => Err(format!("cannot read {}: {e}", file.display())),
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
}

fn run_frontend(
    text: &str,
    modules: &[vl_common::ModuleSpec],
    module: &str,
) -> Result<Frontend, Vec<vl_common::Diagnostic>> {
    let mut diags = Vec::new();

    let (toks, mut d) = vl_lex::lex(text);
    diags.append(&mut d);
    let (ast, mut d) = vl_syntax::parse_with_module(&toks, text, module);
    diags.append(&mut d);
    let (res, mut d) = vl_semantic::resolve_with_modules(&ast, modules);
    diags.append(&mut d);
    if !diags.iter().any(|d| d.is_error()) {
        // `main` is optional: snippets and libraries compile without an
        // entrypoint. Whether a runnable program defines a usable
        // entrypoint is validated by a higher stage (the VM/loader), not
        // the compiler. When `main` is present, its shape is still checked
        // here so mistakes surface early.
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
            if mains[0].0 != 0 {
                diags.push(
                    vl_common::Diagnostic::error("`main` must not take parameters")
                        .with_label(mains[0].2, "entrypoint declared here")
                        .with_code("E401"),
                );
            }
            // The entrypoint returns nothing; `void` keeps VL's type surface
            // total while the VM decides its own halt representation.
            // (A generic `main` is rejected by typechecking with E401.)
            if mains[0].1 != Some(vl_common::VlType::Void) {
                diags.push(
                    vl_common::Diagnostic::error("`main` must return `void`")
                        .with_label(mains[0].2, "entrypoint declared here")
                        .with_note("omit the return type (it defaults to `void`)")
                        .with_code("E401"),
                );
            }
        }
    }
    let hir = vl_hir::lower(&ast, &res);
    let (typed, mut d) = vl_typecheck::check(&hir);
    diags.append(&mut d);
    // Boundary guard: no unresolved `int`/`Param`/nested-`Error` type may
    // reach lowering without a diagnostic. E500s here are compiler bugs.
    diags.append(&mut typed.validate_normalized(&hir, &diags));

    if diags.iter().any(|d| d.is_error()) {
        return Err(diags);
    }
    let lir = vl_lir::lower(&hir, &typed);
    Ok(Frontend { lir })
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.cmd {
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
        Cmd::Check { file } => {
            let (name, text) = match read_input(&file) {
                Ok(v) => v,
                Err(e) => {
                    emit_driver_error(&e, "E600");
                    return ExitCode::from(2);
                }
            };
            // `check` validates frontend semantics independently of a codegen
            // target; target capability checks belong to `build`.
            let modules = vl_codegen::modules();
            let module = source_module(&file);
            match run_frontend(&text, &modules, &module) {
                Ok(_) => {
                    write_out(&None, &format!("ok: {name} checks clean\n"));
                    ExitCode::SUCCESS
                }
                Err(diags) => {
                    emit_all(&diags, &name, &text);
                    ExitCode::from(1)
                }
            }
        }
        Cmd::Build {
            file,
            target,
            emit,
            out,
        } => match file {
            Some(file) => build_single(&file, &target, emit, &out),
            None => build_project(&target, emit, out.as_ref()),
        },
        Cmd::Run { name } => run_project_script(name.as_deref()),
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

fn build_single(file: &Path, target: &str, emit: Option<Emit>, out: &Option<PathBuf>) -> ExitCode {
    let (name, text) = match read_input(file) {
        Ok(v) => v,
        Err(e) => {
            emit_driver_error(&e, "E600");
            return ExitCode::from(2);
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
        write_out(out, &dump);
        return if emit_all(&diags, &name, &text) {
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
    let module = source_module(file);
    let fe = match run_frontend(&text, &modules, &module) {
        Ok(fe) => fe,
        Err(diags) => {
            emit_all(&diags, &name, &text);
            return ExitCode::from(1);
        }
    };

    if let Some(dump) = matches!(emit, Some(Emit::Lir)).then(|| fe.lir.dump()) {
        write_out(out, &dump);
        return ExitCode::SUCCESS;
    }

    let Some(backend) = vl_codegen::lookup(target) else {
        let d = vl_common::Diagnostic::error(format!(
            "unknown target `{target}` (have: {})",
            vl_codegen::all_targets().join(", ")
        ))
        .with_code("E501");
        emit_all(&[d], &name, &text);
        return ExitCode::from(2);
    };

    let (artifact, backend_diags) = backend.emit(&fe.lir);
    let failed = emit_all(&backend_diags, &name, &text);
    match artifact {
        Some(a) if !failed => {
            write_artifact(out, &a);
            ExitCode::SUCCESS
        }
        Some(a) => {
            write_artifact(out, &a);
            ExitCode::from(1)
        }
        None => ExitCode::from(1),
    }
}

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
            diags: Vec::new(),
        };
    }

    let backend = vl_codegen::lookup(target).expect("project target was validated before building");
    let (artifact, diags) = backend.emit(&frontend.lir);
    let output = artifact.map(|artifact| match artifact.bytes {
        Some(bytes) => ProjectOutput::Bytes(bytes),
        None => ProjectOutput::Text(artifact.text),
    });
    ProjectFileBuild { output, diags }
}

fn build_project(target: &str, emit: Option<Emit>, out_override: Option<&PathBuf>) -> ExitCode {
    let project = match load_project(Path::new(".")) {
        Ok(project) => project,
        Err(message) => {
            emit_driver_error(&message, "E602");
            return ExitCode::from(2);
        }
    };
    let source_dir = resolve_project_path(&project.root, &project.config.source);
    if !source_dir.is_dir() {
        emit_driver_error(
            &format!(
                "project source folder does not exist: {}",
                source_dir.display()
            ),
            "E603",
        );
        return ExitCode::from(2);
    }

    let mut files = Vec::new();
    if let Err(message) = collect_vl_files(&source_dir, &mut files) {
        emit_driver_error(&message, "E603");
        return ExitCode::from(2);
    }
    files.sort();
    if files.is_empty() {
        emit_driver_error(
            &format!("no `.vl` files found in {}", source_dir.display()),
            "E603",
        );
        return ExitCode::from(2);
    }

    let needs_backend = matches!(emit, Some(Emit::Asm) | None);
    if needs_backend && vl_codegen::lookup(target).is_none() {
        let diagnostic = vl_common::Diagnostic::error(format!(
            "unknown target `{target}` (have: {})",
            vl_codegen::all_targets().join(", ")
        ))
        .with_code("E501");
        emit_all(&[diagnostic], "<driver>", "");
        return ExitCode::from(2);
    }

    let out_dir = out_override
        .map(|path| resolve_project_path(&project.root, path))
        .unwrap_or_else(|| resolve_project_path(&project.root, &project.config.out));
    if let Err(e) = fs::create_dir_all(&out_dir) {
        emit_driver_error(
            &format!("cannot create output folder {}: {e}", out_dir.display()),
            "E601",
        );
        return ExitCode::from(2);
    }

    let extension = project_output_extension(emit, target);
    let mut failed = false;
    let mut driver_failed = false;
    let mut output_paths = HashSet::new();
    for file in files {
        let (filename, text) = match read_input(&file) {
            Ok(input) => input,
            Err(message) => {
                emit_driver_error(&message, "E600");
                driver_failed = true;
                continue;
            }
        };
        let module = match project_module_for_file(&project.config, &source_dir, &file) {
            Ok(module) => module,
            Err(message) => {
                emit_driver_error(&message, "E602");
                driver_failed = true;
                continue;
            }
        };
        let built = compile_project_source(&text, &module, target, emit);
        let file_failed = emit_all(&built.diags, &filename, &text);
        failed |= file_failed;
        if let Some(output) = built.output {
            // Token/AST output remains useful for malformed files. Final
            // artifacts are only written when their frontend/backend is clean.
            if !file_failed || matches!(emit, Some(Emit::Tokens) | Some(Emit::Ast)) {
                let output_path = project_output_path(&out_dir, &module, &extension);
                if !output_paths.insert(output_path.clone()) {
                    emit_driver_error(
                        &format!("multiple source files produce {}", output_path.display()),
                        "E602",
                    );
                    driver_failed = true;
                } else if let Err(message) = write_project_output(&output_path, &output) {
                    emit_driver_error(&message, "E601");
                    driver_failed = true;
                }
            }
        }
    }

    if driver_failed {
        ExitCode::from(2)
    } else if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn write_out(out: &Option<PathBuf>, text: &str) {
    match out {
        Some(p) => fs::write(p, text).unwrap_or_else(|e| {
            emit_driver_error(&format!("cannot write {}: {e}", p.display()), "E601");
            std::process::exit(2);
        }),
        None => {
            use std::io::Write;
            if let Err(e) = std::io::stdout().write_all(text.as_bytes()) {
                emit_driver_error(&format!("cannot write stdout: {e}"), "E601");
                std::process::exit(2);
            }
        }
    }
}

fn write_artifact(out: &Option<PathBuf>, artifact: &vl_codegen::Artifact) {
    if let Some(bytes) = &artifact.bytes {
        match out {
            Some(path) => fs::write(path, bytes).unwrap_or_else(|e| {
                emit_driver_error(&format!("cannot write {}: {e}", path.display()), "E601");
                std::process::exit(2);
            }),
            None => {
                use std::io::Write;
                std::io::stdout().write_all(bytes).unwrap_or_else(|e| {
                    emit_driver_error(&format!("cannot write stdout: {e}"), "E601");
                    std::process::exit(2);
                });
            }
        }
    } else {
        write_out(out, &artifact.text);
    }
}

fn emit_driver_error(message: &str, code: &str) {
    let diagnostic = vl_common::Diagnostic::error(message).with_code(code);
    emit_all(&[diagnostic], "<driver>", "");
}
