//! vl-codegen: backends. LIR -> target output.
//!
//! The final target platform is **undecided**, so this crate is a stable
//! [`Target`] trait plus thin placeholder backends:
//!
//! - [`DummyTarget`]: human-readable pseudo-assembly, used by tests and
//!   `--emit asm` until a real target lands.
//! - [`StackVmTarget`]: stack-machine text format sketch (still TBD).
//! - [`NaraVmTarget`]: executable Naravm 0.2 vmfiles for `function main()`:
//!   integer/float arithmetic, comparisons, and control flow plus
//!   `std.print` / `std.print_u64`.
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
            &[
                ("print", &[("value", T::String)], T::Void),
                ("print_u64", &[("value", T::U64)], T::Void),
            ],
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
        Instr::Not { dst, src, .. } => format!("not %{}, %{}", dst.0, src.0),
        Instr::BinOp {
            dst, op, lhs, rhs, ..
        } => {
            let m = match op {
                LirOp::Add => "add",
                LirOp::Sub => "sub",
                LirOp::Mul => "mul",
                LirOp::Div => "div",
                LirOp::Eq => "eq",
                LirOp::Ne => "ne",
                LirOp::Lt => "lt",
                LirOp::Le => "le",
                LirOp::Gt => "gt",
                LirOp::Ge => "ge",
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

/// Naravm 0.2 executable vmfile backend: compiles `function main()` to a
/// single entrypoint function (see the internals book for the supported
/// subset). Other functions in the LIR are not emitted; calls to anything
/// but `std.print` / `std.print_u64` are diagnostics.
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
    Value { value_idx: usize },
    String { offset: usize, len: usize },
    Function { module: usize, function: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NaraKind {
    U64,
    I64,
    F64,
    Bool,
    U8,
    String,
}

impl NaraKind {
    fn of_scalar(value: Scalar) -> Self {
        match value {
            Scalar::U64(_) => NaraKind::U64,
            Scalar::I64(_) => NaraKind::I64,
            Scalar::F64(_) => NaraKind::F64,
            Scalar::Bool(_) => NaraKind::Bool,
            Scalar::U8(_) => NaraKind::U8,
        }
    }

    fn scalar_bits(value: Scalar) -> u64 {
        match value {
            Scalar::U64(v) => v,
            Scalar::I64(v) => v as u64,
            Scalar::F64(v) => v,
            Scalar::Bool(v) => u64::from(v),
            Scalar::U8(v) => u64::from(v),
        }
    }

    fn is_integer(self) -> bool {
        matches!(self, NaraKind::U64 | NaraKind::I64 | NaraKind::U8)
    }
}

struct NaraEmit {
    blob: Vec<u8>,
    values: Vec<u64>,
    value_index: std::collections::HashMap<u64, usize>,
    constants: Vec<NaraConstant>,
    bytecode: Vec<u8>,
    diags: Vec<Diagnostic>,
    rv_map: std::collections::HashMap<vl_lir::Reg, u8>,
    rf_map: std::collections::HashMap<vl_lir::Reg, u8>,
    kinds: std::collections::HashMap<vl_lir::Reg, NaraKind>,
    invalid: std::collections::HashSet<vl_lir::Reg>,
    next_rv: u8,
    next_rf: u8,
    /// Recycled machine registers whose LIR value is dead past its last use.
    free_rv: Vec<u8>,
    free_rf: Vec<u8>,
    /// LIR reg -> index of its last textual use. After emitting that use the
    /// machine register is recycled. (Single static definition per reg, so a
    /// textual scan is sound; loop back-edges re-execute definitions before
    /// their uses.)
    last_use: std::collections::HashMap<vl_lir::Reg, usize>,
    label_pos: std::collections::HashMap<u32, usize>,
    patches: Vec<NaraPatch>,
    one_rv: Option<u8>,
    bias_rv: Option<u8>,
}

struct NaraPatch {
    pos: usize,
    len: usize,
    target: u32,
    span: Span,
}

impl NaraEmit {
    fn fresh_rv(&mut self, span: Span) -> Option<u8> {
        if let Some(rv) = self.free_rv.pop() {
            return Some(rv);
        }
        loop {
            let rv = self.next_rv;
            if rv > 0x1f {
                self.diags.push(reg_oob(span, rv as u32));
                return None;
            }
            self.next_rv += 1;
            // Reserve rv10 (error channel) and rv11 (call argument) for the
            // calling convention instead of general allocation.
            if rv == 0x10 || rv == 0x11 {
                continue;
            }
            return Some(rv);
        }
    }

    fn fresh_rf(&mut self, span: Span) -> Option<u8> {
        if let Some(rf) = self.free_rf.pop() {
            return Some(rf);
        }
        loop {
            let rf = self.next_rf;
            if rf > 0x3f {
                self.diags.push(reg_oob(span, rf as u32));
                return None;
            }
            self.next_rf += 1;
            // Reserve rf31 (userland 0x31) as the single reference argument
            // register for calls.
            if rf == 0x31 {
                continue;
            }
            return Some(rf);
        }
    }

    fn add_string(&mut self, value: &[u8], span: Span) -> Option<usize> {
        let offset = self.blob.len();
        self.blob.extend_from_slice(value);
        let idx = self.constants.len();
        self.constants.push(NaraConstant::String {
            offset,
            len: value.len(),
        });
        if idx > u8::MAX as usize {
            self.diags.push(
                Diagnostic::error("Naravm constant pool has more than 256 entries")
                    .with_label(span, "defined here")
                    .with_note("string references use an 8-bit constant index")
                    .with_code("E405"),
            );
            return None;
        }
        Some(idx)
    }

    fn add_value(&mut self, bits: u64, span: Span) -> Option<usize> {
        if let Some(idx) = self.value_index.get(&bits) {
            return Some(*idx);
        }
        let value_idx = self.values.len();
        self.values.push(bits);
        let idx = self.constants.len();
        self.constants.push(NaraConstant::Value { value_idx });
        self.value_index.insert(bits, idx);
        if idx > u8::MAX as usize {
            self.diags.push(
                Diagnostic::error("Naravm constant pool has more than 256 entries")
                    .with_label(span, "defined here")
                    .with_note("value references use an 8-bit constant index")
                    .with_code("E405"),
            );
            return None;
        }
        Some(idx)
    }

    fn ensure_one(&mut self, span: Span) -> Option<u8> {
        if let Some(rv) = self.one_rv {
            return Some(rv);
        }
        let idx = self.add_value(1, span)?;
        let rv = self.fresh_rv(span)?;
        self.bytecode.extend_from_slice(&[0x02, rv, idx as u8]);
        self.one_rv = Some(rv);
        Some(rv)
    }

    fn ensure_bias(&mut self, span: Span) -> Option<u8> {
        if let Some(rv) = self.bias_rv {
            return Some(rv);
        }
        let idx = self.add_value(0x8000_0000_0000_0000, span)?;
        let rv = self.fresh_rv(span)?;
        self.bytecode.extend_from_slice(&[0x02, rv, idx as u8]);
        self.bias_rv = Some(rv);
        Some(rv)
    }

    fn value_reg(&mut self, reg: vl_lir::Reg, span: Span) -> Option<u8> {
        if let Some(rv) = self.rv_map.get(&reg).copied() {
            return Some(rv);
        }
        if self.invalid.contains(&reg) {
            return None;
        }
        self.diags.push(
            Diagnostic::error("Naravm backend could not resolve a value register")
                .with_label(span, "used here")
                .with_code("E500"),
        );
        None
    }

    fn ref_reg(&mut self, reg: vl_lir::Reg, span: Span) -> Option<u8> {
        if let Some(rf) = self.rf_map.get(&reg).copied() {
            return Some(rf);
        }
        if self.invalid.contains(&reg) {
            return None;
        }
        self.diags.push(
            Diagnostic::error("Naravm backend requires a string argument to std.print")
                .with_label(span, "unsupported argument")
                .with_code("E402"),
        );
        None
    }
}

fn nara_vmfile(
    prog: &LirProgram,
    main: &vl_lir::Function,
    diags: &mut Vec<Diagnostic>,
) -> Option<Vec<u8>> {
    let mut e = NaraEmit {
        blob: Vec::new(),
        values: Vec::new(),
        value_index: std::collections::HashMap::new(),
        constants: Vec::new(),
        bytecode: Vec::new(),
        diags: Vec::new(),
        rv_map: std::collections::HashMap::new(),
        rf_map: std::collections::HashMap::new(),
        kinds: std::collections::HashMap::new(),
        invalid: std::collections::HashSet::new(),
        next_rv: 0,
        next_rf: 0x20,
        free_rv: Vec::new(),
        free_rf: Vec::new(),
        last_use: nara_last_use(main),
        label_pos: std::collections::HashMap::new(),
        patches: Vec::new(),
        one_rv: None,
        bias_rv: None,
    };
    // Pre-intern the module name, entrypoint, and std exports. These occupy
    // the first constant slots; user strings/values follow.
    let module_idx = e
        .add_string(prog.module.as_bytes(), Span::empty(0))
        .unwrap_or(0);
    let entry_name_idx = e.add_string(b"<entrypoint>", Span::empty(0)).unwrap_or(0);
    let std_idx = e.add_string(b"std", Span::empty(0)).unwrap_or(0);
    let print_idx = e.add_string(b"print", Span::empty(0)).unwrap_or(0);
    let print_u64_idx = e.add_string(b"print_u64", Span::empty(0)).unwrap_or(0);
    let print_fn_idx = e.constants.len();
    e.constants.push(NaraConstant::Function {
        module: std_idx,
        function: print_idx,
    });
    let print_u64_fn_idx = e.constants.len();
    e.constants.push(NaraConstant::Function {
        module: std_idx,
        function: print_u64_idx,
    });

    for (idx, ins) in main.instrs.iter().enumerate() {
        nara_instr(&mut e, ins, print_fn_idx, print_u64_fn_idx);
        nara_free_dead(&mut e, ins, idx);
        if e.diags.iter().any(|d| d.is_error()) {
            break;
        }
    }
    if e.diags.iter().any(|d| d.is_error()) {
        diags.append(&mut e.diags);
        return None;
    }
    // Resolve jump targets. Offsets are relative to the end of their own
    // instruction (matches the VM's `rip` after reading the offset).
    for patch in &e.patches {
        let Some(target_pos) = e.label_pos.get(&patch.target).copied() else {
            e.diags.push(
                Diagnostic::error(format!(
                    "codegen: unknown label L{} (compiler bug)",
                    patch.target
                ))
                .with_label(patch.span, "jump emitted here")
                .with_code("E500"),
            );
            continue;
        };
        let offset = target_pos as isize - (patch.pos + patch.len) as isize;
        let Ok(offset) = i16::try_from(offset) else {
            e.diags.push(
                Diagnostic::error("Naravm jump offset out of range (function too large)")
                    .with_label(patch.span, "jump emitted here")
                    .with_code("E500"),
            );
            continue;
        };
        let bytes = offset.to_be_bytes();
        e.bytecode[patch.pos + patch.len - 2] = bytes[0];
        e.bytecode[patch.pos + patch.len - 1] = bytes[1];
    }
    if e.diags.iter().any(|d| d.is_error()) {
        diags.append(&mut e.diags);
        return None;
    }
    let functions = vec![(entry_name_idx, e.bytecode.clone())];
    Some(serialize_nara(
        &e.values,
        &e.blob,
        &e.constants,
        &functions,
        module_idx,
    ))
}

/// Scan `main` for the last textual use of every LIR register. Uses are
/// operand positions (BinOp sides, copy sources, call args, branch
/// conditions, return values); definitions do not count.
fn nara_last_use(main: &vl_lir::Function) -> std::collections::HashMap<vl_lir::Reg, usize> {
    use vl_lir::Instr as I;
    let mut last = std::collections::HashMap::new();
    let mut touch = |reg: vl_lir::Reg, idx: usize| {
        last.insert(reg, idx);
    };
    for (idx, ins) in main.instrs.iter().enumerate() {
        match ins {
            I::BinOp { lhs, rhs, .. } => {
                touch(*lhs, idx);
                touch(*rhs, idx);
            }
            I::Copy { src, .. } => {
                touch(*src, idx);
            }
            I::Not { src, .. } => {
                touch(*src, idx);
            }
            I::Call { args, .. } => {
                for arg in args {
                    touch(*arg, idx);
                }
            }
            I::BranchIfFalse { cond, .. } => {
                touch(*cond, idx);
            }
            I::Ret { src, .. } => {
                touch(*src, idx);
            }
            I::Const { .. }
            | I::StringConst { .. }
            | I::Param { .. }
            | I::Jump { .. }
            | I::Label { .. } => {}
        }
    }
    last
}

/// Recycle machine registers whose LIR value dies at `idx`. `Copy`
/// destinations stay mapped (variable homes and `&&`/`||` join registers
/// remain live); internal one/bias registers are not LIR regs and are never
/// freed here.
fn nara_free_dead(e: &mut NaraEmit, ins: &Instr, idx: usize) {
    use vl_lir::Instr as I;
    let mut dead = Vec::new();
    match ins {
        I::BinOp { lhs, rhs, .. } => {
            dead.push(*lhs);
            dead.push(*rhs);
        }
        I::Copy { dst, src, .. } => {
            if dst != src {
                dead.push(*src);
            }
        }
        I::Not { src, .. } => {
            dead.push(*src);
        }
        I::Call { args, .. } => {
            dead.extend(args.iter().copied());
        }
        I::BranchIfFalse { cond, .. } => {
            dead.push(*cond);
        }
        I::Ret { src, .. } => {
            dead.push(*src);
        }
        I::Const { .. }
        | I::StringConst { .. }
        | I::Param { .. }
        | I::Jump { .. }
        | I::Label { .. } => {}
    }
    // A `Copy` destination is (re)defined here: its home register stays live
    // even when this is also its last textual use as a source elsewhere.
    let copy_dst = match ins {
        I::Copy { dst, .. } => Some(*dst),
        _ => None,
    };
    for reg in dead {
        if Some(reg) == copy_dst {
            continue;
        }
        if e.last_use.get(&reg).copied() != Some(idx) {
            continue;
        }
        if let Some(rv) = e.rv_map.remove(&reg) {
            e.free_rv.push(rv);
        }
        if let Some(rf) = e.rf_map.remove(&reg) {
            e.free_rf.push(rf);
        }
    }
}

fn nara_instr(e: &mut NaraEmit, ins: &Instr, print_fn_idx: usize, print_u64_fn_idx: usize) {
    match ins {
        Instr::Const { dst, value, span } => {
            let kind = NaraKind::of_scalar(*value);
            let bits = NaraKind::scalar_bits(*value);
            if !e.last_use.contains_key(dst) {
                // Dead definition (e.g. an unused `let`): still intern the
                // value so pool-limit diagnostics stay accurate, but spend no
                // machine register on it.
                e.add_value(bits, *span);
                e.kinds.insert(*dst, kind);
                return;
            }
            let Some(idx) = e.add_value(bits, *span) else {
                e.invalid.insert(*dst);
                return;
            };
            let Some(rv) = e.fresh_rv(*span) else {
                e.invalid.insert(*dst);
                return;
            };
            e.rv_map.insert(*dst, rv);
            e.kinds.insert(*dst, kind);
            e.bytecode.extend_from_slice(&[0x02, rv, idx as u8]); // lv
        }
        Instr::StringConst { dst, value, span } => {
            if !e.last_use.contains_key(dst) {
                e.add_string(value, *span);
                e.kinds.insert(*dst, NaraKind::String);
                return;
            }
            let Some(idx) = e.add_string(value, *span) else {
                e.invalid.insert(*dst);
                return;
            };
            let Some(rf) = e.fresh_rf(*span) else {
                e.invalid.insert(*dst);
                return;
            };
            e.rf_map.insert(*dst, rf);
            e.kinds.insert(*dst, NaraKind::String);
            e.bytecode.extend_from_slice(&[0x03, rf, idx as u8]); // lrf
        }
        Instr::Param { span, .. } => {
            e.diags.push(
                Diagnostic::error(
                    "Naravm backend only supports `function main()` with no parameters",
                )
                .with_label(*span, "parameter here")
                .with_code("E403"),
            );
        }
        Instr::Copy { dst, src, span } => {
            if e.invalid.contains(src) {
                e.invalid.insert(*dst);
                return;
            }
            let Some(kind) = e.kinds.get(src).copied() else {
                e.invalid.insert(*dst);
                return;
            };
            if e.rv_map
                .get(dst)
                .copied()
                .is_some_and(|d| Some(d) == e.rv_map.get(src).copied())
                || e.rf_map
                    .get(dst)
                    .copied()
                    .is_some_and(|d| Some(d) == e.rf_map.get(src).copied())
            {
                e.kinds.insert(*dst, kind);
                return;
            }
            if kind == NaraKind::String {
                let (Some(s), Some(d)) = (
                    e.rf_map.get(src).copied(),
                    e.rf_map.get(dst).copied().or_else(|| {
                        let rf = e.fresh_rf(*span)?;
                        e.rf_map.insert(*dst, rf);
                        Some(rf)
                    }),
                ) else {
                    e.invalid.insert(*dst);
                    return;
                };
                e.kinds.insert(*dst, kind);
                e.bytecode.extend_from_slice(&[0x05, d, s]); // cprf
            } else {
                let (Some(s), Some(d)) = (
                    e.rv_map.get(src).copied(),
                    e.rv_map.get(dst).copied().or_else(|| {
                        let rv = e.fresh_rv(*span)?;
                        e.rv_map.insert(*dst, rv);
                        Some(rv)
                    }),
                ) else {
                    e.invalid.insert(*dst);
                    return;
                };
                e.kinds.insert(*dst, kind);
                e.bytecode.extend_from_slice(&[0x04, d, s]); // cpv
            }
        }
        Instr::Not { dst, src, span } => {
            let Some(s) = e.value_reg(*src, *span) else {
                e.invalid.insert(*dst);
                return;
            };
            let Some(one) = e.ensure_one(*span) else {
                e.invalid.insert(*dst);
                return;
            };
            let Some(d) = e.fresh_rv(*span) else {
                e.invalid.insert(*dst);
                return;
            };
            e.rv_map.insert(*dst, d);
            e.kinds.insert(*dst, NaraKind::Bool);
            e.bytecode.extend_from_slice(&[0x12, d, s, one]); // xor
        }
        Instr::BinOp {
            dst,
            op,
            lhs,
            rhs,
            span,
        } => {
            nara_binop(e, *dst, *op, *lhs, *rhs, *span);
        }
        Instr::Call {
            dst,
            callee,
            args,
            span,
        } => {
            if callee == "std.print" || callee == "print" {
                if args.len() != 1 {
                    e.diags.push(
                        Diagnostic::error("std.print expects one string argument")
                            .with_label(*span, "invalid call")
                            .with_code("E401"),
                    );
                    e.invalid.insert(*dst);
                    return;
                }
                if e.invalid.contains(&args[0]) {
                    e.invalid.insert(*dst);
                    return;
                }
                let Some(s) = e.ref_reg(args[0], *span) else {
                    e.invalid.insert(*dst);
                    return;
                };
                e.bytecode.extend_from_slice(&[0x05, 0x31, s]); // cprf rf31, src
                nara_calli(e, print_fn_idx, *span);
            } else if callee == "std.print_u64" || callee == "print_u64" {
                if args.len() != 1 {
                    e.diags.push(
                        Diagnostic::error("std.print_u64 expects one u64 argument")
                            .with_label(*span, "invalid call")
                            .with_code("E401"),
                    );
                    e.invalid.insert(*dst);
                    return;
                }
                if e.invalid.contains(&args[0]) {
                    e.invalid.insert(*dst);
                    return;
                }
                match e.kinds.get(&args[0]).copied() {
                    Some(NaraKind::U64) | Some(NaraKind::U8) => {}
                    _ => {
                        e.diags.push(
                            Diagnostic::error(
                                "Naravm backend requires a u64 argument to std.print_u64",
                            )
                            .with_label(*span, "unsupported argument")
                            .with_code("E402"),
                        );
                        e.invalid.insert(*dst);
                        return;
                    }
                }
                let Some(s) = e.value_reg(args[0], *span) else {
                    e.invalid.insert(*dst);
                    return;
                };
                if s != 0x11 {
                    e.bytecode.extend_from_slice(&[0x04, 0x11, s]); // cpv rv11, src
                }
                nara_calli(e, print_u64_fn_idx, *span);
            } else {
                e.diags.push(
                    Diagnostic::error(format!(
                        "Naravm backend does not support call `{callee}` yet"
                    ))
                    .with_label(*span, "unsupported call")
                    .with_note("only `std.print` and `std.print_u64` lower to Naravm calls")
                    .with_code("E404"),
                );
                e.invalid.insert(*dst);
            }
        }
        Instr::Ret { .. } => e.bytecode.push(0x00),
        Instr::BranchIfFalse { cond, target, span } => {
            let Some(c) = e.value_reg(*cond, *span) else {
                return;
            };
            let pos = e.bytecode.len();
            e.bytecode.extend_from_slice(&[0x24, c, 0, 0]); // jz
            e.patches.push(NaraPatch {
                pos,
                len: 4,
                target: *target,
                span: *span,
            });
        }
        Instr::Jump { target, span } => {
            let pos = e.bytecode.len();
            e.bytecode.extend_from_slice(&[0x22, 0, 0]); // jmp
            e.patches.push(NaraPatch {
                pos,
                len: 3,
                target: *target,
                span: *span,
            });
        }
        Instr::Label { id, .. } => {
            e.label_pos.insert(*id, e.bytecode.len());
        }
    }
}

fn nara_calli(e: &mut NaraEmit, fn_idx: usize, span: Span) {
    if fn_idx > u16::MAX as usize {
        e.diags.push(
            Diagnostic::error("Naravm constant pool function index out of range")
                .with_label(span, "call emitted here")
                .with_code("E500"),
        );
        return;
    }
    e.bytecode
        .extend_from_slice(&[0x20, (fn_idx >> 8) as u8, fn_idx as u8]);
}

fn nara_binop(
    e: &mut NaraEmit,
    dst: vl_lir::Reg,
    op: LirOp,
    lhs: vl_lir::Reg,
    rhs: vl_lir::Reg,
    span: Span,
) {
    let fail = |e: &mut NaraEmit| {
        e.invalid.insert(dst);
    };
    // Resolve operand kinds before machine registers: string operands live in
    // reference registers, so resolving value registers first would misreport
    // them as a compiler bug instead of clean E404 diagnostics.
    let lkind = e.kinds.get(&lhs).copied();
    let rkind = e.kinds.get(&rhs).copied();
    if lkind != rkind || lkind.is_none() {
        if e.invalid.contains(&lhs) || e.invalid.contains(&rhs) {
            fail(e);
            return;
        }
        e.diags.push(
            Diagnostic::error("Naravm backend found mismatched operand types")
                .with_label(span, "emitted here")
                .with_code("E500"),
        );
        fail(e);
        return;
    }
    let kind = lkind.unwrap_or(NaraKind::I64);
    if kind == NaraKind::String {
        e.diags.push(
            Diagnostic::error(format!(
                "Naravm backend does not support `{op}` on strings yet"
            ))
            .with_label(span, "unsupported operation")
            .with_code("E404"),
        );
        fail(e);
        return;
    }
    let Some(l) = e.value_reg(lhs, span) else {
        fail(e);
        return;
    };
    let Some(r) = e.value_reg(rhs, span) else {
        fail(e);
        return;
    };
    let alloc = |e: &mut NaraEmit| {
        let d = e.fresh_rv(span)?;
        e.rv_map.insert(dst, d);
        Some(d)
    };
    match op {
        LirOp::Add | LirOp::Sub | LirOp::Mul | LirOp::Div => {
            let opcode = match (op, kind) {
                (LirOp::Add, NaraKind::U64) | (LirOp::Add, NaraKind::U8) => 0x30,
                (LirOp::Add, NaraKind::I64) => 0x31,
                (LirOp::Add, NaraKind::F64) => 0x32,
                (LirOp::Sub, NaraKind::U64) | (LirOp::Sub, NaraKind::U8) => 0x33,
                (LirOp::Sub, NaraKind::I64) => 0x34,
                (LirOp::Sub, NaraKind::F64) => 0x35,
                (LirOp::Mul, NaraKind::U64) | (LirOp::Mul, NaraKind::U8) => 0x36,
                (LirOp::Mul, NaraKind::I64) => 0x37,
                (LirOp::Mul, NaraKind::F64) => 0x38,
                (LirOp::Div, NaraKind::U64) | (LirOp::Div, NaraKind::U8) => 0x39,
                (LirOp::Div, NaraKind::I64) => 0x3a,
                (LirOp::Div, NaraKind::F64) => 0x3b,
                _ => {
                    e.diags.push(
                        Diagnostic::error(format!(
                            "Naravm backend does not support `{op}` on `{kind:?}` yet"
                        ))
                        .with_label(span, "unsupported operation")
                        .with_code("E404"),
                    );
                    fail(e);
                    return;
                }
            };
            let Some(d) = alloc(e) else {
                fail(e);
                return;
            };
            e.kinds.insert(dst, kind);
            e.bytecode.extend_from_slice(&[opcode, d, l, r]);
        }
        LirOp::Eq => {
            if !matches!(
                kind,
                NaraKind::U64 | NaraKind::I64 | NaraKind::F64 | NaraKind::Bool | NaraKind::U8
            ) {
                e.diags.push(
                    Diagnostic::error("Naravm backend does not support equality on strings yet")
                        .with_label(span, "unsupported operation")
                        .with_code("E404"),
                );
                fail(e);
                return;
            }
            let Some(d) = alloc(e) else {
                fail(e);
                return;
            };
            e.kinds.insert(dst, NaraKind::Bool);
            e.bytecode.extend_from_slice(&[0x0a, d, l, r]); // eq
        }
        LirOp::Ne => {
            if !matches!(
                kind,
                NaraKind::U64 | NaraKind::I64 | NaraKind::F64 | NaraKind::Bool | NaraKind::U8
            ) {
                e.diags.push(
                    Diagnostic::error("Naravm backend does not support equality on strings yet")
                        .with_label(span, "unsupported operation")
                        .with_code("E404"),
                );
                fail(e);
                return;
            }
            let (Some(d), Some(one)) = (alloc(e), e.ensure_one(span)) else {
                fail(e);
                return;
            };
            e.kinds.insert(dst, NaraKind::Bool);
            e.bytecode.extend_from_slice(&[0x0a, d, l, r]); // eq
            e.bytecode.extend_from_slice(&[0x12, d, d, one]); // xor 1
        }
        LirOp::Lt | LirOp::Le | LirOp::Gt | LirOp::Ge => {
            nara_compare(e, dst, op, kind, l, r, span);
        }
    }
}

/// Lower one ordering comparison. Unsigned kinds use `ltu` directly; signed
/// `i64` flips the sign bit on both sides first so the unsigned compare
/// yields signed order. `f64` and strings have no ISA compare: clean error.
fn nara_compare(
    e: &mut NaraEmit,
    dst: vl_lir::Reg,
    op: LirOp,
    kind: NaraKind,
    l: u8,
    r: u8,
    span: Span,
) {
    if !kind.is_integer() {
        e.diags.push(
            Diagnostic::error(format!(
                "Naravm backend does not support `{op}` on `{kind:?}` yet"
            ))
            .with_label(span, "unsupported operation")
            .with_note("order comparisons lower for u64/i64/u8; f64 and strings are rejected")
            .with_code("E404"),
        );
        e.invalid.insert(dst);
        return;
    }
    // Normalize to (ltu a b) optionally xored with 1:
    // Lt(a,b)=ltu(a,b); Gt(a,b)=ltu(b,a); Le=not Gt; Ge=not Lt.
    let (a, b, negate) = match op {
        LirOp::Lt => (l, r, false),
        LirOp::Gt => (r, l, false),
        LirOp::Le => (r, l, true),
        LirOp::Ge => (l, r, true),
        _ => unreachable!("ordering only"),
    };
    if kind == NaraKind::I64 {
        let (Some(bias), Some(d)) = (e.ensure_bias(span), e.fresh_rv(span)) else {
            e.invalid.insert(dst);
            return;
        };
        let (Some(ta), Some(tb)) = (e.fresh_rv(span), e.fresh_rv(span)) else {
            e.invalid.insert(dst);
            return;
        };
        e.rv_map.insert(dst, d);
        e.kinds.insert(dst, NaraKind::Bool);
        e.bytecode.extend_from_slice(&[0x12, ta, a, bias]); // xor sign
        e.bytecode.extend_from_slice(&[0x12, tb, b, bias]);
        e.bytecode.extend_from_slice(&[0x10, d, ta, tb]); // ltu
        if negate {
            let Some(one) = e.ensure_one(span) else {
                e.invalid.insert(dst);
                return;
            };
            e.bytecode.extend_from_slice(&[0x12, d, d, one]);
        }
        return;
    }
    let Some(d) = e.fresh_rv(span) else {
        e.invalid.insert(dst);
        return;
    };
    e.rv_map.insert(dst, d);
    e.kinds.insert(dst, NaraKind::Bool);
    e.bytecode.extend_from_slice(&[0x10, d, a, b]); // ltu
    if negate {
        let Some(one) = e.ensure_one(span) else {
            e.invalid.insert(dst);
            return;
        };
        e.bytecode.extend_from_slice(&[0x12, d, d, one]);
    }
}

fn serialize_nara(
    values: &[u64],
    blob: &[u8],
    constants: &[NaraConstant],
    functions: &[(usize, Vec<u8>)],
    module_idx: usize,
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"nara");
    put_u16(&mut out, 0);
    put_u16(&mut out, 2);
    put_u32(&mut out, module_idx as u32);
    put_u32(&mut out, values.len() as u32);
    for v in values {
        out.extend_from_slice(&v.to_be_bytes());
    }
    put_u32(&mut out, blob.len() as u32);
    out.extend_from_slice(blob);
    pad4(&mut out);
    put_u32(&mut out, constants.len() as u32);
    for constant in constants {
        out.push(match constant {
            NaraConstant::Value { .. } => 1,
            NaraConstant::String { .. } => 2,
            NaraConstant::Function { .. } => 3,
        });
    }
    pad4(&mut out);
    for constant in constants {
        match constant {
            NaraConstant::Value { value_idx } => {
                put_u32(&mut out, *value_idx as u32);
            }
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
        Instr::Not { .. } => "not".into(),
        Instr::BinOp { op, .. } => match op {
            LirOp::Add => "add",
            LirOp::Sub => "sub",
            LirOp::Mul => "mul",
            LirOp::Div => "div",
            LirOp::Eq => "eq",
            LirOp::Ne => "ne",
            LirOp::Lt => "lt",
            LirOp::Le => "le",
            LirOp::Gt => "gt",
            LirOp::Ge => "ge",
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
    fn countdown_while_emits_runnable_naravm() {
        let lir = lir_of(
            "use std; function main() { let i = 3u64; while (i > 0u64) { std.print_u64(i); i = i - 1u64; } }",
        );
        let (artifact, diags) = NaraVmTarget.emit(&lir);
        assert!(diags.is_empty(), "{diags:?}");
        let bytes = artifact.unwrap().bytes.unwrap();
        assert_eq!(&bytes[..4], b"nara");
        // A loop back-edge plus at least one conditional jump must be present.
        // jmp = 0x22, jz = 0x24.
        assert!(bytes.contains(&0x22), "no jmp in {bytes:?}");
        assert!(bytes.contains(&0x24), "no jz in {bytes:?}");
    }

    #[test]
    fn naravm_supports_signed_ordering_and_logic() {
        for src in [
            "function main() { let a = 0 - 5; if (a < 3) { a; } }",
            "function main() { if (true && !false) { 1; } }",
            "function main() { let i = 0; while (i < 3) { i = i + 1; if (i == 2) { continue; } } }",
        ] {
            let lir = lir_of(src);
            let (artifact, diags) = NaraVmTarget.emit(&lir);
            assert!(diags.is_empty(), "{src}: {diags:?}");
            assert!(artifact.is_some());
        }
    }

    #[test]
    fn naravm_rejects_float_ordering_with_e404() {
        let lir = lir_of("function main() { if (1.5f64 < 2.5f64) { 1; } }");
        let (artifact, diags) = NaraVmTarget.emit(&lir);
        assert!(artifact.is_none());
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("E404")),
            "{diags:?}"
        );
    }

    #[test]
    fn dummy_emits_control_flow() {
        let lir = lir_of("function main() { let i = 0; while (i < 1) { i = i + 1; } }");
        let (art, diags) = DummyTarget.emit(&lir);
        assert!(diags.is_empty());
        let text = art.unwrap().text;
        assert!(text.contains("jump") && text.contains("L0:"), "{text}");
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
