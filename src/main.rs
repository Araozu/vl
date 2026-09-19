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
        #[arg(long, default_value = "dummy")]
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

fn run_frontend(filename: &str, text: &str) -> Result<Frontend, Vec<vl_common::Diagnostic>> {
    let mut diags = Vec::new();

    let (toks, mut d) = vl_lex::lex(text);
    diags.append(&mut d);
    let module = std::path::Path::new(filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(filename);
    let (ast, mut d) = vl_syntax::parse_with_module(&toks, text, module);
    diags.append(&mut d);
    let (res, mut d) = vl_semantic::resolve_with_modules(&ast, &vl_codegen::modules());
    diags.append(&mut d);
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
                    eprintln!("vl: {e}");
                    return ExitCode::from(2);
                }
            };
            let (toks, diags) = vl_lex::lex(&text);
            for t in &toks {
                println!("{:?}\t{:?}", t.kind, t.span);
            }
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
                    eprintln!("vl: {e}");
                    return ExitCode::from(2);
                }
            };
            let (toks, mut diags) = vl_lex::lex(&text);
            let (ast, mut d) = vl_syntax::parse(&toks, &text);
            diags.append(&mut d);
            println!("{ast:#?}");
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
                    eprintln!("vl: {e}");
                    return ExitCode::from(2);
                }
            };
            match run_frontend(&name, &text) {
                Ok(_) => {
                    println!("ok: {name} checks clean");
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
                    eprintln!("vl: {e}");
                    return ExitCode::from(2);
                }
            };
            let fe = match run_frontend(&name, &text) {
                Ok(fe) => fe,
                Err(diags) => {
                    emit_all(&diags, &name, &text);
                    return ExitCode::from(1);
                }
            };
            let backend = vl_codegen::lookup(&target).unwrap_or_else(|| {
                eprintln!(
                    "vl: unknown target `{target}` (have: {})",
                    vl_codegen::all_targets().join(", ")
                );
                std::process::exit(2);
            });

            // Intermediate dumps short-circuit codegen.
            let dumped = match emit {
                Some(Emit::Tokens) => {
                    let (toks, _) = vl_lex::lex(&text);
                    Some(format!("{toks:#?}\n"))
                }
                Some(Emit::Ast) => {
                    let (toks, _) = vl_lex::lex(&text);
                    let (ast, _) = vl_syntax::parse(&toks, &text);
                    Some(format!("{ast:#?}\n"))
                }
                Some(Emit::Lir) => Some(fe.lir.dump()),
                Some(Emit::Asm) | None => None,
            };
            if let Some(dump) = dumped {
                write_out(&out, &dump);
                return ExitCode::SUCCESS;
            }

            let (artifact, backend_diags) = backend.emit(&fe.lir);
            // Backend warnings print but don't fail unless errors present.
            let failed = emit_all(&backend_diags, &name, &text);
            match artifact {
                Some(a) if !failed => {
                    write_out(&out, &a.text);
                    ExitCode::SUCCESS
                }
                Some(a) => {
                    write_out(&out, &a.text);
                    ExitCode::from(1)
                }
                None => ExitCode::from(1),
            }
        }
        Cmd::Targets => {
            for t in vl_codegen::all_targets() {
                println!("{t}");
            }
            ExitCode::SUCCESS
        }
    }
}

fn write_out(out: &Option<PathBuf>, text: &str) {
    match out {
        Some(p) => fs::write(p, text).unwrap_or_else(|e| {
            eprintln!("vl: cannot write {}: {e}", p.display());
            std::process::exit(2);
        }),
        None => print!("{text}"),
    }
}
