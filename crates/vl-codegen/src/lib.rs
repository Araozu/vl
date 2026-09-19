//! vl-codegen: backends. LIR -> target output.
//!
//! The final target platform is **undecided**, so this crate is a stable
//! [`Target`] trait plus thin placeholder backends:
//!
//! - [`DummyTarget`]: human-readable pseudo-assembly, used by tests and
//!   `--emit asm` until a real target lands.
//! - [`StackVmTarget`]: stack-machine text format sketch (still TBD).
//! - [`NaraVmTarget`]: executable Naravm 0.2 vmfiles: `function main()`
//!   becomes the `<entrypoint>` function plus one Nara function per other
//!   user function. Integer/float arithmetic, comparisons, and control flow
//!   plus `std.print` / `std.print_u64` and user-function calls lower to
//!   `calli`.
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

/// Naravm 0.2 executable vmfile backend: compiles `function main()` to the
/// `<entrypoint>` function plus one Nara function per other user function
/// (see the internals book for the supported subset). Calls to
/// `std.print` / `std.print_u64` and to user functions lower to `calli`;
/// anything else is a diagnostic.
pub struct NaraVmTarget;

impl Target for NaraVmTarget {
    fn name(&self) -> &'static str {
        "naravm"
    }

    fn emit(&self, prog: &LirProgram) -> (Option<Artifact>, Vec<Diagnostic>) {
        if !prog.functions.iter().any(|f| f.name == "main") {
            return (
                None,
                vec![Diagnostic::error("program must define `function main()`").with_code("E400")],
            );
        };
        let mut diags = Vec::new();
        let bytes = match nara_vmfile(prog, &mut diags) {
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
    File,
}

impl NaraKind {
    /// Map a VL-level type to its register file. `None` for `Void`/`Error`,
    /// which never reach codegen through the driver (frontends reject them).
    fn of_ty(ty: vl_typecheck::Ty) -> Option<Self> {
        match ty {
            vl_typecheck::Ty::U64 => Some(NaraKind::U64),
            vl_typecheck::Ty::I64 => Some(NaraKind::I64),
            vl_typecheck::Ty::F64 => Some(NaraKind::F64),
            vl_typecheck::Ty::Bool => Some(NaraKind::Bool),
            vl_typecheck::Ty::U8 => Some(NaraKind::U8),
            vl_typecheck::Ty::String => Some(NaraKind::String),
            vl_typecheck::Ty::File => Some(NaraKind::File),
            vl_typecheck::Ty::Void | vl_typecheck::Ty::Error => None,
        }
    }

    /// Reference kinds live in `rf`, everything else in `rv`.
    fn is_ref(self) -> bool {
        matches!(self, NaraKind::String | NaraKind::File)
    }

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
    /// Next userland parameter slots for the function being emitted. Value
    /// and reference parameters count independently from `rv11` / `rf31`
    /// (matching the Naravm native convention).
    param_vi: u8,
    param_ri: u8,
}

struct NaraPatch {
    pos: usize,
    len: usize,
    target: u32,
    span: Span,
}

impl NaraEmit {
    /// Reset per-function state before emitting the next Nara function. The
    /// constant/blob/value pools (and diagnostics) are shared across the
    /// whole program; everything else starts over.
    fn reset_fn(&mut self, last_use: std::collections::HashMap<vl_lir::Reg, usize>) {
        self.bytecode = Vec::new();
        self.rv_map.clear();
        self.rf_map.clear();
        self.kinds.clear();
        self.invalid.clear();
        self.next_rv = 0;
        self.next_rf = 0x20;
        self.free_rv.clear();
        self.free_rf.clear();
        self.last_use = last_use;
        self.label_pos.clear();
        self.patches.clear();
        self.one_rv = None;
        self.bias_rv = None;
        self.param_vi = 0;
        self.param_ri = 0;
    }

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

/// Per-function emission context: the LIR signature plus program-wide
/// callee tables built by the pre-pass.
struct NaraFnCtx<'a> {
    func: &'a vl_lir::Function,
    is_main: bool,
    sigs: &'a std::collections::HashMap<&'a str, &'a vl_lir::Function>,
    fn_consts: &'a std::collections::HashMap<String, usize>,
    print_fn_idx: usize,
    print_u64_fn_idx: usize,
}

fn nara_vmfile(prog: &LirProgram, diags: &mut Vec<Diagnostic>) -> Option<Vec<u8>> {
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
        last_use: std::collections::HashMap::new(),
        label_pos: std::collections::HashMap::new(),
        patches: Vec::new(),
        one_rv: None,
        bias_rv: None,
        param_vi: 0,
        param_ri: 0,
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

    // Pre-pass: intern every user function name plus a
    // `Function{module, function}` constant for it, so (mutually) recursive
    // calls resolve even when the callee is emitted later. `main` maps to
    // the `<entrypoint>` name, which is what the VM registers.
    let mut fn_consts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut fn_names: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for f in &prog.functions {
        if f.name == "<global>" || fn_consts.contains_key(&f.name) {
            continue;
        }
        if f.name == "main" {
            if let Some(idx) = nara_push_fn_const(&mut e, module_idx, entry_name_idx) {
                fn_consts.insert(f.name.clone(), idx);
            }
            continue;
        }
        let Some(name_idx) = e.add_string(f.name.as_bytes(), Span::empty(0)) else {
            continue;
        };
        if let Some(idx) = nara_push_fn_const(&mut e, module_idx, name_idx) {
            fn_consts.insert(f.name.clone(), idx);
            fn_names.insert(f.name.clone(), name_idx);
        }
    }
    if e.diags.iter().any(|d| d.is_error()) {
        diags.append(&mut e.diags);
        return None;
    }

    let mut sigs: std::collections::HashMap<&str, &vl_lir::Function> =
        std::collections::HashMap::new();
    for f in &prog.functions {
        if f.name != "<global>" {
            sigs.entry(f.name.as_str()).or_insert(f);
        }
    }

    let mut functions_out: Vec<(usize, Vec<u8>)> = Vec::new();
    for f in &prog.functions {
        if f.name == "<global>" {
            continue;
        }
        let is_main = f.name == "main";
        e.reset_fn(nara_last_use(f));
        let ctx = NaraFnCtx {
            func: f,
            is_main,
            sigs: &sigs,
            fn_consts: &fn_consts,
            print_fn_idx,
            print_u64_fn_idx,
        };
        for (idx, ins) in f.instrs.iter().enumerate() {
            nara_instr(&mut e, ins, &ctx);
            nara_free_dead(&mut e, ins, idx);
            if e.diags.iter().any(|d| d.is_error()) {
                break;
            }
        }
        if e.diags.iter().any(|d| d.is_error()) {
            break;
        }
        if !nara_resolve_jumps(&mut e) {
            break;
        }
        let name_idx = if is_main {
            entry_name_idx
        } else {
            fn_names.get(&f.name).copied().unwrap_or(entry_name_idx)
        };
        functions_out.push((name_idx, std::mem::take(&mut e.bytecode)));
    }
    if e.diags.iter().any(|d| d.is_error()) {
        diags.append(&mut e.diags);
        return None;
    }
    Some(serialize_nara(
        &e.values,
        &e.blob,
        &e.constants,
        &functions_out,
        module_idx,
    ))
}

/// Push a `Function{module, function}` constant with the shared pool-limit
/// diagnostic (E405) instead of silently overflowing the 8-bit index space.
fn nara_push_fn_const(e: &mut NaraEmit, module: usize, function: usize) -> Option<usize> {
    let idx = e.constants.len();
    e.constants
        .push(NaraConstant::Function { module, function });
    if idx > u8::MAX as usize {
        e.diags.push(
            Diagnostic::error("Naravm constant pool has more than 256 entries")
                .with_label(Span::empty(0), "defined here")
                .with_note("function references use an 8-bit constant index")
                .with_code("E405"),
        );
        return None;
    }
    Some(idx)
}

/// Resolve jump targets for the function in `e.bytecode`. Offsets are
/// relative to the end of their own instruction (matches the VM's `rip`
/// after reading the offset).
fn nara_resolve_jumps(e: &mut NaraEmit) -> bool {
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
    !e.diags.iter().any(|d| d.is_error())
}

/// Scan one function for the last textual use of every LIR register. Uses are
/// operand positions (BinOp sides, copy sources, call args, branch
/// conditions, return values); definitions do not count.
fn nara_last_use(func: &vl_lir::Function) -> std::collections::HashMap<vl_lir::Reg, usize> {
    use vl_lir::Instr as I;
    let mut last = std::collections::HashMap::new();
    let mut touch = |reg: vl_lir::Reg, idx: usize| {
        last.insert(reg, idx);
    };
    for (idx, ins) in func.instrs.iter().enumerate() {
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

fn nara_instr(e: &mut NaraEmit, ins: &Instr, ctx: &NaraFnCtx) {
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
        Instr::Param { dst, index, span } => {
            if ctx.is_main {
                e.diags.push(
                    Diagnostic::error(
                        "Naravm backend only supports `function main()` with no parameters",
                    )
                    .with_label(*span, "parameter here")
                    .with_code("E403"),
                );
                return;
            }
            nara_param(e, ctx, *dst, *index, *span);
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
            if kind.is_ref() {
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
            // User functions first: a user function may share a bare name
            // with a std export, and the LIR callee spelling alone cannot
            // tell them apart (imports are resolved away before lowering).
            if ctx.sigs.contains_key(callee.as_str()) {
                nara_user_call(e, ctx, *dst, callee, args, *span);
            } else if callee == "std.print" || callee == "print" {
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
                nara_calli(e, ctx.print_fn_idx, *span);
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
                nara_calli(e, ctx.print_u64_fn_idx, *span);
            } else {
                e.diags.push(
                    Diagnostic::error(format!(
                        "Naravm backend does not support call `{callee}` yet"
                    ))
                    .with_label(*span, "unsupported call")
                    .with_note(
                        "only `std.print`, `std.print_u64`, and user functions lower to Naravm calls",
                    )
                    .with_code("E404"),
                );
                e.invalid.insert(*dst);
            }
        }
        Instr::Ret { src, span } => nara_ret(e, ctx, *src, *span),
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

/// Callee prologue for one `Param`: copy the incoming argument register
/// (`rv11+i` for values, `rf31+j` for references — independent sequences)
/// into a fresh machine register. `Void`/`Error` params cannot occur past
/// the frontend; poison quietly instead of cascading.
fn nara_param(e: &mut NaraEmit, ctx: &NaraFnCtx, dst: vl_lir::Reg, index: usize, span: Span) {
    let ty = ctx
        .func
        .param_tys
        .get(index)
        .copied()
        .unwrap_or(vl_typecheck::Ty::Error);
    let Some(kind) = NaraKind::of_ty(ty) else {
        e.invalid.insert(dst);
        return;
    };
    if kind.is_ref() {
        if e.param_ri >= 9 {
            e.diags.push(
                Diagnostic::error(
                    "Naravm backend supports at most 9 reference parameters per function",
                )
                .with_label(span, "parameter here")
                .with_code("E404"),
            );
            e.invalid.insert(dst);
            return;
        }
        let src = 0x31 + e.param_ri;
        e.param_ri += 1;
        if !e.last_use.contains_key(&dst) {
            // Dead parameter: the slot is still consumed positionally, but
            // no machine register is spent on it.
            return;
        }
        let Some(rf) = e.fresh_rf(span) else {
            e.invalid.insert(dst);
            return;
        };
        e.rf_map.insert(dst, rf);
        e.kinds.insert(dst, kind);
        e.bytecode.extend_from_slice(&[0x05, rf, src]); // cprf
    } else {
        if e.param_vi >= 15 {
            e.diags.push(
                Diagnostic::error(
                    "Naravm backend supports at most 15 value parameters per function",
                )
                .with_label(span, "parameter here")
                .with_code("E404"),
            );
            e.invalid.insert(dst);
            return;
        }
        let src = 0x11 + e.param_vi;
        e.param_vi += 1;
        if !e.last_use.contains_key(&dst) {
            return;
        }
        let Some(rv) = e.fresh_rv(span) else {
            e.invalid.insert(dst);
            return;
        };
        e.rv_map.insert(dst, rv);
        e.kinds.insert(dst, kind);
        e.bytecode.extend_from_slice(&[0x04, rv, src]); // cpv
    }
}

/// One spilled caller register: the value and reference stacks are
/// independent, so replaying the push sequence in reverse restores both.
enum NaraSpill {
    V(u8),
    F(u8),
}

/// Call a user function: spill live caller registers (the register file is
/// VM-global, shared across frames), move actuals into the callee's param
/// slots, `calli`, copy the return out, then restore the spills.
fn nara_user_call(
    e: &mut NaraEmit,
    ctx: &NaraFnCtx,
    dst: vl_lir::Reg,
    callee: &str,
    args: &[vl_lir::Reg],
    span: Span,
) {
    let Some(callee_fn) = ctx.sigs.get(callee).copied() else {
        e.diags.push(
            Diagnostic::error(format!(
                "Naravm backend does not support call `{callee}` yet"
            ))
            .with_label(span, "unsupported call")
            .with_note(
                "only `std.print`, `std.print_u64`, and user functions lower to Naravm calls",
            )
            .with_code("E404"),
        );
        e.invalid.insert(dst);
        return;
    };
    // Poisoned actuals stay quiet.
    for arg in args {
        if e.invalid.contains(arg) {
            e.invalid.insert(dst);
            return;
        }
    }
    if args.len() != callee_fn.param_tys.len() {
        e.diags.push(
            Diagnostic::error(format!(
                "codegen: arity mismatch calling `{callee}` (compiler bug)"
            ))
            .with_label(span, "call emitted here")
            .with_code("E500"),
        );
        e.invalid.insert(dst);
        return;
    }
    // Partition actuals by the caller's tracked register kind with the same
    // two independent counters the prologue uses.
    let mut actuals: Vec<(bool, u8)> = Vec::with_capacity(args.len());
    let mut values = 0u8;
    let mut refs = 0u8;
    for arg in args {
        match e.kinds.get(arg).copied() {
            Some(kind) if kind.is_ref() => {
                let Some(src) = e.rf_map.get(arg).copied() else {
                    e.diags.push(
                        Diagnostic::error(
                            "Naravm backend could not resolve a call argument (compiler bug)",
                        )
                        .with_label(span, "call emitted here")
                        .with_code("E500"),
                    );
                    e.invalid.insert(dst);
                    return;
                };
                refs += 1;
                actuals.push((true, src));
            }
            Some(_) => {
                let Some(src) = e.rv_map.get(arg).copied() else {
                    e.diags.push(
                        Diagnostic::error(
                            "Naravm backend could not resolve a call argument (compiler bug)",
                        )
                        .with_label(span, "call emitted here")
                        .with_code("E500"),
                    );
                    e.invalid.insert(dst);
                    return;
                };
                values += 1;
                actuals.push((false, src));
            }
            None => {
                e.diags.push(
                    Diagnostic::error(
                        "Naravm backend could not resolve a call argument (compiler bug)",
                    )
                    .with_label(span, "call emitted here")
                    .with_code("E500"),
                );
                e.invalid.insert(dst);
                return;
            }
        }
    }
    if values > 15 || refs > 9 {
        e.diags.push(
            Diagnostic::error(
                "Naravm backend supports at most 15 value and 9 reference arguments per call",
            )
            .with_label(span, "call emitted here")
            .with_code("E404"),
        );
        e.invalid.insert(dst);
        return;
    }
    // Reserve the return register before spilling so it cannot alias a live
    // caller register. This emits no code, only reserves a register number.
    let ret_kind = NaraKind::of_ty(callee_fn.ret);
    let ret_rv = match ret_kind {
        Some(kind) if !kind.is_ref() => match e.fresh_rv(span) {
            Some(rv) => Some(rv),
            None => {
                e.invalid.insert(dst);
                return;
            }
        },
        _ => None,
    };
    let ret_rf = match ret_kind {
        Some(kind) if kind.is_ref() => match e.fresh_rf(span) {
            Some(rf) => Some(rf),
            None => {
                e.invalid.insert(dst);
                return;
            }
        },
        _ => None,
    };

    let mut rvs: Vec<u8> = e.rv_map.values().copied().collect();
    rvs.sort_unstable();
    let mut rfs: Vec<u8> = e.rf_map.values().copied().collect();
    rfs.sort_unstable();
    let mut spills: Vec<NaraSpill> = Vec::new();
    for rv in rvs {
        e.bytecode.extend_from_slice(&[0x06, rv]); // pushv
        spills.push(NaraSpill::V(rv));
    }
    // Cached comparison temporaries are live machine state too; without a
    // spill the callee (which allocates from register 0) would clobber them.
    if let Some(one) = e.one_rv {
        e.bytecode.extend_from_slice(&[0x06, one]);
        spills.push(NaraSpill::V(one));
    }
    if let Some(bias) = e.bias_rv {
        e.bytecode.extend_from_slice(&[0x06, bias]);
        spills.push(NaraSpill::V(bias));
    }
    for rf in rfs {
        e.bytecode.extend_from_slice(&[0x08, rf]); // pushrf
        spills.push(NaraSpill::F(rf));
    }
    let mut vi = 0u8;
    let mut ri = 0u8;
    for (is_ref, src) in &actuals {
        if *is_ref {
            e.bytecode.extend_from_slice(&[0x05, 0x31 + ri, *src]); // cprf
            ri += 1;
        } else {
            e.bytecode.extend_from_slice(&[0x04, 0x11 + vi, *src]); // cpv
            vi += 1;
        }
    }
    let Some(fn_idx) = ctx.fn_consts.get(callee).copied() else {
        e.diags.push(
            Diagnostic::error(format!(
                "codegen: missing function constant for `{callee}` (compiler bug)"
            ))
            .with_label(span, "call emitted here")
            .with_code("E500"),
        );
        e.invalid.insert(dst);
        return;
    };
    nara_calli(e, fn_idx, span);
    match ret_kind {
        // `Void`/`Error` returns map to nothing; the destination stays dead.
        None => {}
        Some(kind) if kind.is_ref() => {
            let rf = ret_rf.expect("reserved above");
            e.bytecode.extend_from_slice(&[0x05, rf, 0x31]); // cprf rf, rf31
            e.rf_map.insert(dst, rf);
            e.kinds.insert(dst, kind);
        }
        Some(kind) => {
            let rv = ret_rv.expect("reserved above");
            if rv != 0x11 {
                e.bytecode.extend_from_slice(&[0x04, rv, 0x11]); // cpv rv, rv11
            }
            e.rv_map.insert(dst, rv);
            e.kinds.insert(dst, kind);
        }
    }
    for spill in spills.iter().rev() {
        match spill {
            NaraSpill::V(rv) => e.bytecode.extend_from_slice(&[0x07, *rv]), // popv
            NaraSpill::F(rf) => e.bytecode.extend_from_slice(&[0x09, *rf]), // poprf
        }
    }
}

/// Function epilogue: move the tail value into the return slot (`rv11` /
/// `rf31` per the declared return kind), then `ret`. `main` and `void`
/// functions emit a bare `ret` as before.
fn nara_ret(e: &mut NaraEmit, ctx: &NaraFnCtx, src: vl_lir::Reg, span: Span) {
    if ctx.is_main {
        e.bytecode.push(0x00);
        return;
    }
    let ret = match NaraKind::of_ty(ctx.func.ret) {
        None => {
            e.bytecode.push(0x00);
            return;
        }
        Some(kind) => kind,
    };
    if e.invalid.contains(&src) {
        e.bytecode.push(0x00);
        return;
    }
    if ret.is_ref() {
        match e.rf_map.get(&src).copied() {
            Some(s) => {
                e.bytecode.extend_from_slice(&[0x05, 0x31, s]); // cprf rf31, src
            }
            None => {
                e.diags.push(
                    Diagnostic::error(
                        "Naravm backend could not resolve the return register (compiler bug)",
                    )
                    .with_label(span, "return emitted here")
                    .with_code("E500"),
                );
            }
        }
        e.bytecode.push(0x00);
        return;
    }
    match e.rv_map.get(&src).copied() {
        Some(s) => {
            if s != 0x11 {
                e.bytecode.extend_from_slice(&[0x04, 0x11, s]); // cpv rv11, src
            }
        }
        None => {
            e.diags.push(
                Diagnostic::error(
                    "Naravm backend could not resolve the return register (compiler bug)",
                )
                .with_label(span, "return emitted here")
                .with_code("E500"),
            );
        }
    }
    e.bytecode.push(0x00);
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
    if kind.is_ref() {
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
    fn naravm_emits_user_calls_with_mixed_params_string_return_and_recursion() {
        let lir = lir_of(
            r#"
use std;
function add(a: u64, b: u64): u64 { a + b; }
function greet(name: string, n: u64): string { name; }
function fact(n: u64): u64 {
    let r = 1u64;
    if (n == 0u64) { r; } else { r = n * fact(n - 1u64); }
    r;
}
function main() {
    std.print(greet("hi\n", 1u64));
    std.print_u64(add(fact(3u64), 1u64));
}
"#,
        );
        let (artifact, diags) = NaraVmTarget.emit(&lir);
        assert!(diags.is_empty(), "{diags:?}");
        let bytes = artifact.unwrap().bytes.unwrap();
        assert_eq!(&bytes[..4], b"nara");
        // calli = 0x20: main -> greet/add/fact plus the recursive fact call.
        assert!(bytes.contains(&0x20), "no calli in {bytes:?}");
    }

    #[test]
    fn naravm_rejects_unknown_callee_with_e404() {
        use vl_common::Span;
        use vl_lir::{Function, Instr, LirProgram, Reg};
        let lir = LirProgram {
            module: "t".into(),
            functions: vec![Function {
                name: "main".into(),
                param_tys: vec![],
                ret: vl_typecheck::Ty::Void,
                instrs: vec![
                    Instr::Const {
                        dst: Reg(0),
                        value: vl_common::Scalar::U64(1),
                        span: Span::empty(0),
                    },
                    Instr::Call {
                        dst: Reg(1),
                        callee: "nope".into(),
                        args: vec![Reg(0)],
                        span: Span::empty(0),
                    },
                    Instr::Ret {
                        src: Reg(1),
                        span: Span::empty(0),
                    },
                ],
            }],
        };
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
