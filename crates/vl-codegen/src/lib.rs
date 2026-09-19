//! vl-codegen: backends. LIR -> target output.
//!
//! The final target platform is **undecided**, so this crate is a stable
//! [`Target`] trait plus thin placeholder backends:
//!
//! - [`DummyTarget`]: human-readable pseudo-assembly, used by tests and
//!   `--emit asm` until a real target lands.
//! - [`StackVmTarget`]: stack-machine text format sketch (still TBD).
//!
//! Rule: new targets = new types implementing [`Target`]. Never branch
//! the LIR or the driver on target names.

use vl_common::{Diagnostic, Span};
use vl_lir::{Instr, LirOp, LirProgram};

/// Compiled artifact: text plus the target that produced it.
#[derive(Debug, Clone)]
pub struct Artifact {
    pub target: String,
    pub text: String,
}

/// Every backend implements this. Keep it object-safe (`&self`, no generics).
pub trait Target {
    fn name(&self) -> &'static str;
    fn emit(&self, prog: &LirProgram) -> (Option<Artifact>, Vec<Diagnostic>);
}

/// All backends the driver knows about.
pub fn all_targets() -> Vec<&'static str> {
    vec![DummyTarget.name(), StackVmTarget.name()]
}

/// Look up a backend by `--target` flag value.
pub fn lookup(name: &str) -> Option<Box<dyn Target>> {
    match name {
        "dummy" => Some(Box::new(DummyTarget)),
        "stackvm" => Some(Box::new(StackVmTarget)),
        _ => None,
    }
}

// ---------------------------------------------------------- Dummy ---

/// Pseudo-assembly for humans and golden tests. NOT a real ISA.
pub struct DummyTarget;

impl Target for DummyTarget {
    fn name(&self) -> &'static str {
        "dummy"
    }

    fn emit(&self, prog: &LirProgram) -> (Option<Artifact>, Vec<Diagnostic>) {
        let mut text = String::from("; vl dummy target — pseudo-assembly\n");
        for f in &prog.functions {
            text.push_str(&format!("{}:\n", f.name));
            for ins in &f.instrs {
                text.push_str(&format!("  {}\n", dummy_instr(ins)));
            }
        }
        (
            Some(Artifact {
                target: self.name().into(),
                text,
            }),
            vec![],
        )
    }
}

fn dummy_instr(ins: &Instr) -> String {
    match ins {
        Instr::Const { dst, value, .. } => format!("mov %{}, {value}", dst.0),
        Instr::Param { dst, index, .. } => format!("param %{}, {index}", dst.0),
        Instr::Copy { dst, src, .. } => format!("mov %{}, %{}", dst.0, src.0),
        Instr::BinOp {
            dst, op, lhs, rhs, ..
        } => {
            let m = match op {
                LirOp::Add => "add",
                LirOp::Sub => "sub",
                LirOp::Mul => "mul",
                LirOp::Div => "div",
            };
            format!("{m} %{}, %{}, %{}", dst.0, lhs.0, rhs.0)
        }
        Instr::Call {
            dst, callee, args, ..
        } => {
            let args = args
                .iter()
                .map(|arg| format!("%{}", arg.0))
                .collect::<Vec<_>>()
                .join(", ");
            format!("call %{}, {callee}({args})", dst.0)
        }
        Instr::Ret { src, .. } => format!("ret %{}", src.0),
    }
}

// -------------------------------------------------------- StackVM ---

/// Sketch of a stack-machine text backend. Emits `push`/`add`/… lines;
/// the bytecode encoding is TBD with the target decision.
pub struct StackVmTarget;

impl Target for StackVmTarget {
    fn name(&self) -> &'static str {
        "stackvm"
    }

    fn emit(&self, prog: &LirProgram) -> (Option<Artifact>, Vec<Diagnostic>) {
        let diags =
            vec![
                Diagnostic::warning("stackvm backend is a sketch; output is not yet executable")
                    .with_note("track the target-platform decision before hardening this"),
            ];
        let mut text = String::from("# vl stackvm sketch\n");
        for f in &prog.functions {
            text.push_str(&format!(".fn {}\n", f.name));
            for ins in &f.instrs {
                text.push_str(&format!("  {}\n", stackvm_instr(ins)));
            }
        }
        (
            Some(Artifact {
                target: self.name().into(),
                text,
            }),
            diags,
        )
    }
}

fn stackvm_instr(ins: &Instr) -> String {
    match ins {
        Instr::Const { value, .. } => format!("push {value}"),
        Instr::Param { index, .. } => format!("param {index}"),
        Instr::Copy { .. } => "dup".into(),
        Instr::BinOp { op, .. } => match op {
            LirOp::Add => "add",
            LirOp::Sub => "sub",
            LirOp::Mul => "mul",
            LirOp::Div => "div",
        }
        .into(),
        Instr::Call { callee, args, .. } => {
            let args = args
                .iter()
                .map(|arg| format!("%{}", arg.0))
                .collect::<Vec<_>>()
                .join(", ");
            format!("call {callee}({args})")
        }
        Instr::Ret { .. } => "ret".into(),
    }
}

/// Out-of-range register ids would be a compiler bug; surface it loudly.
pub fn reg_oob(span: Span, reg: u32) -> Diagnostic {
    Diagnostic::error(format!(
        "codegen: register %{reg} out of range (compiler bug)"
    ))
    .with_label(span, "emitted here")
    .with_code("E500")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lir_of(src: &str) -> LirProgram {
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        let (typed, _) = vl_typecheck::check(&hir);
        vl_lir::lower(&hir, &typed)
    }

    #[test]
    fn dummy_emits_text() {
        let lir = lir_of("let x = 1 + 2;");
        let (art, diags) = DummyTarget.emit(&lir);
        assert!(diags.is_empty());
        assert!(art.unwrap().text.contains("add"));
    }

    #[test]
    fn unknown_target_is_none() {
        assert!(lookup("x86-64").is_none());
    }

    #[test]
    fn backends_emit_calls_and_parameters() {
        let lir = lir_of("function add(a, b) { a + b; } function main() { add(1, 2); }");
        let (art, diags) = DummyTarget.emit(&lir);
        assert!(diags.is_empty());
        let text = art.unwrap().text;
        assert!(text.contains("param %0, 0"), "{text}");
        assert!(text.contains("call %2, add(%0, %1)"), "{text}");

        let (art, diags) = StackVmTarget.emit(&lir);
        assert_eq!(diags.len(), 1);
        assert!(art.unwrap().text.contains("call add(%0, %1)"));
    }
}
