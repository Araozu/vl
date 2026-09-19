//! vl driver: CLI that wires the whole pipeline together.
//!
//! Pipeline: text --lex--> tokens --parse--> AST --resolve--> scopes
//! --lower--> HIR --check--> typed HIR --lower--> LIR --emit--> target.
//!
//! Every stage appends to one `Vec<Diagnostic>`; all printing goes through
//! Ariadne (`vl_common::diagnostic::emit_all`). Exit code 1 iff any error.

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use vl_common::diagnostic::emit_all;

#[derive(Debug, Parser)]
#[command(name = "vl", version, about = "VL — vibecoded language driver")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, clap::Subcommand)]
enum Cmd {
    /// Lex a file and print tokens.
    Lex { file: PathBuf },
    /// Parse a file and print the AST.
    Parse { file: PathBuf },
    /// Run the full frontend (lex..typecheck) and report errors.
    Check { file: PathBuf },
    /// Compile a file; print or write the target output.
    Build {
        file: PathBuf,
        /// Which backend to use (target platform TBD; see `vl-codegen`).
        #[arg(long, default_value = "naravm")]
        target: String,
        /// Dump an intermediate instead of compiling.
        #[arg(long, value_enum)]
        emit: Option<Emit>,
        /// Write output here instead of stdout.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// List available codegen backends.
    Targets,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Emit {
    Tokens,
    Ast,
    Lir,
    Asm,
}

fn read_input(file: &PathBuf) -> Result<(String, String), String> {
    match fs::read_to_string(file) {
        Ok(text) => Ok((file.display().to_string(), text)),
        Err(e) => Err(format!("cannot read {}: {e}", file.display())),
    }
}

/// Full frontend: returns typed artifacts or the collected diagnostics.
struct Frontend {
    lir: vl_lir::LirProgram,
}

fn run_frontend(
    filename: &str,
    text: &str,
    modules: &[vl_common::ModuleSpec],
) -> Result<Frontend, Vec<vl_common::Diagnostic>> {
    let mut diags = Vec::new();

    let (toks, mut d) = vl_lex::lex(text);
    diags.append(&mut d);
    let module = std::path::Path::new(filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(filename);
    let (ast, mut d) = vl_syntax::parse_with_module(&toks, text, module);
    diags.append(&mut d);
    let (res, mut d) = vl_semantic::resolve_with_modules(&ast, modules);
    diags.append(&mut d);
    if !diags.iter().any(|d| d.is_error()) {
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
                } if name == "main" => Some((params.len(), *ret, *span)),
                _ => None,
            })
            .collect::<Vec<_>>();
        if mains.is_empty() {
            diags.push(
                vl_common::Diagnostic::error("program must define `function main()`")
                    .with_code("E400"),
            );
        } else {
            if mains[0].0 != 0 {
                diags.push(
                    vl_common::Diagnostic::error("`main` must not take parameters")
                        .with_label(mains[0].2, "entrypoint declared here")
                        .with_code("E401"),
                );
            }
            // The entrypoint returns nothing; `void` keeps VL's type surface
            // total while the VM decides its own halt representation.
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

    if diags.iter().any(|d| d.is_error()) {
        return Err(diags);
    }
    let lir = vl_lir::lower(&hir, &typed);
    Ok(Frontend { lir })
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.cmd {
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
            match run_frontend(&name, &text, &modules) {
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
        } => {
            let (name, text) = match read_input(&file) {
                Ok(v) => v,
                Err(e) => {
                    emit_driver_error(&e, "E600");
                    return ExitCode::from(2);
                }
            };
            // Lex/parse emits are intentionally shallow: they must remain
            // useful for broken or incomplete files and do not require a
            // `main` function or backend support.
            if matches!(emit, Some(Emit::Tokens) | Some(Emit::Ast)) {
                let (toks, mut diags) = vl_lex::lex(&text);
                let dump = if matches!(emit, Some(Emit::Ast)) {
                    let (ast, mut parse_diags) = vl_syntax::parse(&toks, &text);
                    diags.append(&mut parse_diags);
                    format!("{ast:#?}\n")
                } else {
                    format!("{toks:#?}\n")
                };
                write_out(&out, &dump);
                return if emit_all(&diags, &name, &text) {
                    ExitCode::from(1)
                } else {
                    ExitCode::SUCCESS
                };
            }
            // LIR is target-neutral: it must be possible to dump it with an
            // unknown target name and with modules only supported by another
            // backend. Backend lookup/capabilities apply only to final asm.
            let target_specific = matches!(emit, Some(Emit::Asm) | None);
            let modules = if target_specific {
                vl_codegen::modules_for_target(&target)
            } else {
                vl_codegen::modules()
            };
            let fe = match run_frontend(&name, &text, &modules) {
                Ok(fe) => fe,
                Err(diags) => {
                    emit_all(&diags, &name, &text);
                    return ExitCode::from(1);
                }
            };

            // Intermediate dumps short-circuit codegen.
            let dumped = match emit {
                Some(Emit::Lir) => Some(fe.lir.dump()),
                Some(Emit::Tokens) | Some(Emit::Ast) => unreachable!("handled above"),
                Some(Emit::Asm) | None => None,
            };
            if let Some(dump) = dumped {
                write_out(&out, &dump);
                return ExitCode::SUCCESS;
            }

            let Some(backend) = vl_codegen::lookup(&target) else {
                let d = vl_common::Diagnostic::error(format!(
                    "unknown target `{target}` (have: {})",
                    vl_codegen::all_targets().join(", ")
                ))
                .with_code("E501");
                emit_all(&[d], &name, &text);
                return ExitCode::from(2);
            };

            let (artifact, backend_diags) = backend.emit(&fe.lir);
            // Backend warnings print but don't fail unless errors present.
            let failed = emit_all(&backend_diags, &name, &text);
            match artifact {
                Some(a) if !failed => {
                    write_artifact(&out, &a);
                    ExitCode::SUCCESS
                }
                Some(a) => {
                    write_artifact(&out, &a);
                    ExitCode::from(1)
                }
                None => ExitCode::from(1),
            }
        }
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
