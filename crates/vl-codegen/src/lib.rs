//! vl-codegen: backends. LIR -> target output.
//!
//! The final target platform is **undecided**, so this crate is a stable
//! [`Target`] trait plus thin placeholder backends:
//!
//! - [`DummyTarget`]: human-readable pseudo-assembly, used by tests and
//!   `--emit asm` until a real target lands.
//! - [`StackVmTarget`]: stack-machine text format sketch (still TBD).
//! - [`NaraVmTarget`]: executable Naravm 0.2 vmfiles.
//!
//! Rule: new targets = new types implementing [`Target`]. Never branch
//! the LIR or the driver on target names.

use vl_common::Scalar;
use vl_common::{Diagnostic, Span};
use vl_lir::{Instr, LirOp, LirProgram};

/// Compiled artifact: text plus the target that produced it.
#[derive(Debug, Clone)]
pub struct Artifact {
    pub target: String,
    pub text: String,
    /// Binary payloads are used by targets whose output is not text.
    pub bytes: Option<Vec<u8>>,
}

/// Every backend implements this. Keep it object-safe (`&self`, no generics).
pub trait Target {
    fn name(&self) -> &'static str;
    fn emit(&self, prog: &LirProgram) -> (Option<Artifact>, Vec<Diagnostic>);
}

/// Modules known to the target environment. Frontend resolution consumes the
/// same catalog, so imports and emitted calls cannot drift apart.
///
/// This is where the language's extern type surface is *declared*: every
/// export carries VL-level param names/types and a return type. Backends map
/// these VL types to target concepts (e.g. VL `string` -> Naravm blob).
/// Fallible VM operations (`!File`, `!String` via `errno`/`0x30`) are modeled
/// as plain returns for now; error handling is out of scope for VL.
pub fn modules() -> Vec<vl_common::ModuleSpec> {
    use vl_common::VlType as T;
    vec![
        vl_common::ModuleSpec::new(
            &["std"],
            &[
                ("print", &[("value", T::String)], T::Void),
                ("print_u64", &[("value", T::U64)], T::Void),
            ],
        ),
        vl_common::ModuleSpec::new(
            &["std", "fs"],
            &[
                ("open", &[("path", T::String)], T::File),
                ("read", &[("file", T::File)], T::String),
            ],
        ),
        vl_common::ModuleSpec::new(
            &["std", "string"],
            &[("len", &[("value", T::String)], T::U64)],
        ),
    ]
}

/// Module surface available to a concrete backend. The broad `modules`
/// catalog remains useful to frontend/library tests; drivers should resolve
/// against this target-specific view so accepted calls are actually emit-able.
pub fn modules_for_target(target: &str) -> Vec<vl_common::ModuleSpec> {
    use vl_common::VlType as T;
    match target {
        "naravm" => vec![vl_common::ModuleSpec::new(
            &["std"],
            &[("print", &[("value", T::String)], T::Void)],
        )],
        _ => modules(),
    }
}

/// All backends the driver knows about.
pub fn all_targets() -> Vec<&'static str> {
    vec![
        NaraVmTarget.name(),
        DummyTarget.name(),
        StackVmTarget.name(),
    ]
}

/// Look up a backend by `--target` flag value.
pub fn lookup(name: &str) -> Option<Box<dyn Target>> {
    match name {
        "naravm" => Some(Box::new(NaraVmTarget)),
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
        let mut text = format!("; vl dummy target — module {}\n", prog.module);
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
                bytes: None,
            }),
            vec![],
        )
    }
}

fn dummy_instr(ins: &Instr) -> String {
    match ins {
        Instr::Const { dst, value, .. } => format!("mov %{}, {}", dst.0, scalar_text(*value)),
        Instr::StringConst { dst, .. } => format!("string %{} (unsupported)", dst.0),
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
        Instr::BranchIfFalse { cond, target, .. } => {
            format!("branch_if_false %{}, L{}", cond.0, target)
        }
        Instr::Jump { target, .. } => format!("jump L{}", target),
        Instr::Label { id, .. } => format!("L{}:", id),
    }
}

fn scalar_text(value: Scalar) -> String {
    match value {
        Scalar::U64(v) => format!("{v}u64"),
        Scalar::I64(v) => format!("{v}i64"),
        Scalar::F64(v) => format!("{}f64", f64::from_bits(v)),
        Scalar::Bool(v) => v.to_string(),
        Scalar::U8(v) => format!("{v}u8"),
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
        let mut text = format!("# vl stackvm sketch — module {}\n", prog.module);
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
                bytes: None,
            }),
            diags,
        )
    }
}

// --------------------------------------------------------- Naravm ---

/// Naravm 0.2 executable vmfile backend.
pub struct NaraVmTarget;

impl Target for NaraVmTarget {
    fn name(&self) -> &'static str {
        "naravm"
    }

    fn emit(&self, prog: &LirProgram) -> (Option<Artifact>, Vec<Diagnostic>) {
        let Some(main) = prog.functions.iter().find(|f| f.name == "main") else {
            return (
                None,
                vec![Diagnostic::error("program must define `function main()`").with_code("E400")],
            );
        };
        let mut diags = Vec::new();
        let bytes = match nara_vmfile(prog, main, &mut diags) {
            Some(bytes) if diags.iter().all(|d| !d.is_error()) => bytes,
            _ => return (None, diags),
        };
        (
            Some(Artifact {
                target: self.name().into(),
                text: String::new(),
                bytes: Some(bytes),
            }),
            diags,
        )
    }
}

#[derive(Clone)]
enum NaraConstant {
    String { offset: usize, len: usize },
    Function { module: usize, function: usize },
}

fn nara_vmfile(
    prog: &LirProgram,
    main: &vl_lir::Function,
    diags: &mut Vec<Diagnostic>,
) -> Option<Vec<u8>> {
    let mut blob = Vec::new();
    let mut constants = Vec::new();
    let add_string = |blob: &mut Vec<u8>, constants: &mut Vec<NaraConstant>, value: &[u8]| {
        let offset = blob.len();
        blob.extend_from_slice(value);
        let idx = constants.len();
        constants.push(NaraConstant::String {
            offset,
            len: value.len(),
        });
        idx
    };
    add_string(&mut blob, &mut constants, prog.module.as_bytes());
    let entry_name_idx = add_string(&mut blob, &mut constants, b"<entrypoint>");
    let std_idx = add_string(&mut blob, &mut constants, b"std");
    let print_idx = add_string(&mut blob, &mut constants, b"print");
    let print_fn_idx = constants.len();
    constants.push(NaraConstant::Function {
        module: std_idx,
        function: print_idx,
    });
    let mut bytecode = Vec::new();
    let mut string_regs = std::collections::HashMap::new();
    let mut invalid_string_regs = std::collections::HashSet::new();
    for ins in &main.instrs {
        match ins {
            Instr::StringConst { dst, value, .. } => {
                let idx = add_string(&mut blob, &mut constants, value);
                if idx > u8::MAX as usize {
                    diags.push(
                        Diagnostic::error("Naravm constant pool has more than 256 entries")
                            .with_note("string references use an 8-bit constant index")
                            .with_code("E405"),
                    );
                    invalid_string_regs.insert(*dst);
                    continue;
                }
                string_regs.insert(*dst, idx);
                bytecode.extend_from_slice(&[0x03, 0x31, idx as u8]); // lrf rf31 #idx
            }
            Instr::Call {
                callee, args, span, ..
            } if callee == "std.print" || callee == "print" => {
                if args.len() != 1 {
                    diags.push(
                        Diagnostic::error("std.print expects one string argument")
                            .with_label(*span, "invalid call")
                            .with_code("E401"),
                    );
                } else if invalid_string_regs.contains(&args[0]) {
                    // The defining string instruction already reported the
                    // root cause; do not cascade an unsupported-argument
                    // diagnostic from the poisoned register.
                } else if !string_regs.contains_key(&args[0]) {
                    diags.push(
                        Diagnostic::error("Naravm backend requires a string argument to std.print")
                            .with_label(*span, "unsupported argument")
                            .with_code("E402"),
                    );
                } else {
                    bytecode.extend_from_slice(&[
                        0x20,
                        (print_fn_idx >> 8) as u8,
                        print_fn_idx as u8,
                    ]);
                }
            }
            Instr::Ret { .. } => bytecode.push(0x00),
            Instr::Const { .. }
            | Instr::Copy { .. }
            | Instr::Param { .. }
            | Instr::BinOp { .. }
            | Instr::BranchIfFalse { .. }
            | Instr::Jump { .. }
            | Instr::Label { .. } => {
                diags.push(
                    Diagnostic::error(
                        "Naravm backend only supports the hello-world subset currently",
                    )
                    .with_code("E403"),
                );
            }
            Instr::Call { callee, span, .. } => {
                diags.push(
                    Diagnostic::error(format!(
                        "Naravm backend does not support call `{callee}` yet"
                    ))
                    .with_label(*span, "unsupported call")
                    .with_code("E404"),
                );
            }
        }
    }
    if diags.iter().any(|d| d.is_error()) {
        return None;
    }
    // Keep the value section valid even though hello world uses only refs.
    let functions = vec![(entry_name_idx, bytecode)];
    Some(serialize_nara(
        prog.module.as_bytes(),
        &blob,
        &constants,
        &functions,
    ))
}

fn serialize_nara(
    module: &[u8],
    blob: &[u8],
    constants: &[NaraConstant],
    functions: &[(usize, Vec<u8>)],
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"nara");
    put_u16(&mut out, 0);
    put_u16(&mut out, 2);
    let module_idx = constants.iter().position(|c| matches!(c, NaraConstant::String { offset, len } if &blob[*offset..*offset + *len] == module)).unwrap_or(0);
    put_u32(&mut out, module_idx as u32);
    put_u32(&mut out, 0);
    put_u32(&mut out, blob.len() as u32);
    out.extend_from_slice(blob);
    pad4(&mut out);
    put_u32(&mut out, constants.len() as u32);
    for constant in constants {
        out.push(match constant {
            NaraConstant::String { .. } => 2,
            NaraConstant::Function { .. } => 3,
        });
    }
    pad4(&mut out);
    for constant in constants {
        match constant {
            NaraConstant::String { offset, len } => {
                put_u32(&mut out, *offset as u32);
                put_u32(&mut out, *len as u32);
            }
            NaraConstant::Function { module, function } => {
                put_u32(&mut out, *module as u32);
                put_u32(&mut out, *function as u32);
            }
        }
    }
    put_u32(&mut out, functions.len() as u32);
    for (name, code) in functions {
        put_u32(&mut out, *name as u32);
        put_u32(&mut out, code.len() as u32);
        out.extend_from_slice(code);
        pad4(&mut out);
    }
    out
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_be_bytes());
}
fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}
fn pad4(out: &mut Vec<u8>) {
    while (out.len() & 3) != 0 {
        out.push(0xff);
    }
}

fn stackvm_instr(ins: &Instr) -> String {
    match ins {
        Instr::Const { value, .. } => format!("push {}", scalar_text(*value)),
        Instr::StringConst { dst, .. } => format!("string %{} (unsupported)", dst.0),
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
        Instr::BranchIfFalse { cond, target, .. } => {
            format!("branch_if_false %{} -> L{}", cond.0, target)
        }
        Instr::Jump { target, .. } => format!("jump L{}", target),
        Instr::Label { id, .. } => format!("L{}:", id),
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
        let lir = lir_of("function main() { 1 + 2; }");
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
        let lir =
            lir_of("function add(a: i64, b: i64): i64 { a + b; } function main() { add(1, 2); }");
        let (art, diags) = DummyTarget.emit(&lir);
        assert!(diags.is_empty());
        let text = art.unwrap().text;
        assert!(text.contains("param %0, 0"), "{text}");
        assert!(text.contains("call %2, add(%0, %1)"), "{text}");

        let (art, diags) = StackVmTarget.emit(&lir);
        assert_eq!(diags.len(), 1);
        assert!(art.unwrap().text.contains("call add(%0, %1)"));
    }

    #[test]
    fn naravm_rejects_constant_pool_indices_that_do_not_fit() {
        let mut src = String::from("function main() {");
        for i in 0..252 {
            src.push_str(&format!("let s{i} = \"s{i}\";"));
        }
        src.push_str("std.print(\"target\");}");
        let lir = lir_of(&src);
        let (artifact, diags) = NaraVmTarget.emit(&lir);
        assert!(artifact.is_none());
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("E405")),
            "{diags:?}"
        );
    }

    #[test]
    fn naravm_accepts_bare_print_from_single_export_use() {
        let src = "use std.print; function main() { print(\"hi\\n\"); }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        let (res, rdiags) = vl_semantic::resolve_with_modules(&prog, &modules());
        assert!(rdiags.iter().all(|d| !d.is_error()), "{rdiags:?}");
        let hir = vl_hir::lower(&prog, &res);
        let (typed, tdiags) = vl_typecheck::check(&hir);
        assert!(tdiags.is_empty(), "{tdiags:?}");
        let lir = vl_lir::lower(&hir, &typed);
        assert!(lir.dump().contains("call print"), "{}", lir.dump());
        let (artifact, diags) = NaraVmTarget.emit(&lir);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(&artifact.unwrap().bytes.unwrap()[..4], b"nara");
    }

    #[test]
    fn target_module_catalogs_are_specific() {
        assert!(modules_for_target("naravm")
            .iter()
            .any(|m| m.path.as_string() == "std"));
        assert!(!modules_for_target("naravm")
            .iter()
            .any(|m| m.path.as_string() == "std.fs"));
        assert!(modules_for_target("dummy")
            .iter()
            .any(|m| m.path.as_string() == "std.fs"));
    }
}
