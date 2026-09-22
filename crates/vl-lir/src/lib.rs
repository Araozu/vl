//! vl-lir: low-level IR. Typed HIR -> flat three-address code.
//!
//! [`LirProgram`] is deliberately target-agnostic: a list of [`Function`]s
//! each holding [`Instr`]s over virtual [`Reg`]isters. `vl-codegen` lowers
//! this to real targets; since the final target is still undecided, this
//! crate must NOT grow target-specific hacks — add a new backend instead.
//! Returns are explicit: only `return expr;` / `return;` emits [`Instr::Ret`]
//! for a function; trailing expression values are discarded and the
//! fallthrough epilogue returns a dummy zero (`void` backends ignore it).

use std::collections::HashMap;

use vl_common::Scalar;
use vl_common::Span;
use vl_hir::{HirBinOp, HirExpr, HirItem, HirProgram, HirStmt, HirUnOp};
use vl_typecheck::{subst_ty, Ty};

/// Virtual register.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Reg(pub u32);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FunctionRef {
    pub module: String,
    pub function: String,
}

impl std::fmt::Display for FunctionRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.module == "<anonymous>" {
            write!(f, "{}", self.function)
        } else {
            write!(f, "{}::{}", self.module, self.function)
        }
    }
}

#[derive(Debug, Clone)]
pub struct FunctionImport {
    pub symbol: FunctionRef,
    pub param_tys: Vec<Ty>,
    pub ret: Ty,
}

/// Three-address instructions. Strings are carried as raw bytes; backends
/// in `vl-codegen` lower them to target concepts.
#[derive(Debug, Clone)]
pub enum Instr {
    Const {
        dst: Reg,
        value: Scalar,
        span: Span,
    },
    StringConst {
        dst: Reg,
        value: Vec<u8>,
        span: Span,
    },
    /// Function parameter copied into a virtual register at entry.
    Param {
        dst: Reg,
        index: usize,
        span: Span,
    },
    Copy {
        dst: Reg,
        src: Reg,
        span: Span,
    },
    Not {
        dst: Reg,
        src: Reg,
        span: Span,
    },
    BinOp {
        dst: Reg,
        op: LirOp,
        lhs: Reg,
        rhs: Reg,
        span: Span,
    },
    Call {
        dst: Reg,
        callee: FunctionRef,
        args: Vec<Reg>,
        span: Span,
    },
    /// Allocate a zero-filled `Array[T]` with room for `len` (`u64`)
    /// elements. `elem` is the (concrete) element type.
    NewArray {
        dst: Reg,
        len: Reg,
        elem: Ty,
        span: Span,
    },
    /// Build an `Array[T]` from element registers, in order.
    ArrayLit {
        dst: Reg,
        elems: Vec<Reg>,
        elem: Ty,
        span: Span,
    },
    /// Read element `index` (a `u64` register) from an `Array[T]`.
    ArrayGet {
        dst: Reg,
        array: Reg,
        index: Reg,
        elem: Ty,
        span: Span,
    },
    /// Write `value` into element `index` of an `Array[T]`. Statement-only.
    ArraySet {
        array: Reg,
        index: Reg,
        value: Reg,
        elem: Ty,
        span: Span,
    },
    /// Allocate a named object and initialize its fields.
    NewObject {
        dst: Reg,
        name: String,
        fields: Vec<(String, Reg)>,
        span: Span,
    },
    ObjectGet {
        dst: Reg,
        object: Reg,
        name: String,
        ty: Ty,
        span: Span,
    },
    ObjectSet {
        object: Reg,
        name: String,
        value: Reg,
        ty: Ty,
        span: Span,
    },
    /// Build a fixed-arity heterogeneous tuple from element registers.
    /// `tys` holds the erased element types in order (names are a
    /// typecheck-only concern; codegen maps each position to a value or
    /// reference container slot).
    TupleLit {
        dst: Reg,
        elems: Vec<Reg>,
        tys: Vec<Ty>,
        span: Span,
    },
    /// Read tuple position `index` (positional; named access lowers to the
    /// field's position during HIR lowering, so names are erased here).
    TupleGet {
        dst: Reg,
        tuple: Reg,
        index: usize,
        tys: Vec<Ty>,
        span: Span,
    },
    /// Write tuple position `index`. Statement-only.
    TupleSet {
        tuple: Reg,
        index: usize,
        value: Reg,
        tys: Vec<Ty>,
        span: Span,
    },
    /// Build a union value: heap tag + payload. `tag` is the variant's
    /// declaration index; `tys` holds the erased payload types in order
    /// (used by backends for lane/slot assignment — no global union table
    /// needed, so generic instantiations work per site).
    NewVariant {
        dst: Reg,
        union: String,
        variant: String,
        tag: u32,
        args: Vec<Reg>,
        tys: Vec<Ty>,
        span: Span,
    },
    /// Read a union value's discriminant tag (a `u64` value).
    TagOf {
        dst: Reg,
        scrut: Reg,
        span: Span,
    },
    /// Read payload position `index` of the matched variant. `tys` is the
    /// matched variant's erased payload types (slot mapping mirrors
    /// [`Instr::NewVariant`]).
    PayloadGet {
        dst: Reg,
        scrut: Reg,
        index: usize,
        tys: Vec<Ty>,
        span: Span,
    },
    /// Explicit integer conversion (`value as u8`). Backends lower it to a
    /// value copy reinterpreting the 64-bit payload per `target` (literals
    /// were range-checked by typechecking; variables are unchecked).
    Cast {
        dst: Reg,
        src: Reg,
        target: Ty,
        span: Span,
    },
    /// Explicit `return` (or fallthrough / global initializer value).
    Ret {
        src: Reg,
        span: Span,
    },
    /// Load a module global by stable ID (see `LirProgram::globals`).
    /// Target-neutral: backends run initializers once before `main` and keep
    /// one shared cell per global.
    GlobalLoad {
        dst: Reg,
        global: u32,
        span: Span,
    },
    /// Store a module global by stable ID (rebinding or initializing).
    GlobalStore {
        global: u32,
        src: Reg,
        span: Span,
    },
    BranchIfFalse {
        cond: Reg,
        target: u32,
        span: Span,
    },
    Jump {
        target: u32,
        span: Span,
    },
    Label {
        id: u32,
        span: Span,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LirOp {
    Add,
    Sub,
    Mul,
    Div,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl std::fmt::Display for LirOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LirOp::Add => write!(f, "add"),
            LirOp::Sub => write!(f, "sub"),
            LirOp::Mul => write!(f, "mul"),
            LirOp::Div => write!(f, "div"),
            LirOp::Eq => write!(f, "eq"),
            LirOp::Ne => write!(f, "ne"),
            LirOp::Lt => write!(f, "lt"),
            LirOp::Le => write!(f, "le"),
            LirOp::Gt => write!(f, "gt"),
            LirOp::Ge => write!(f, "ge"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Function {
    pub name: String,
    /// Declared parameter types in order (`Ty::Error` when missing/poisoned).
    pub param_tys: Vec<Ty>,
    /// Declared return type (`Ty::Error` when missing/poisoned; globals
    /// record their value type here).
    pub ret: Ty,
    pub instrs: Vec<Instr>,
}

#[derive(Debug, Clone)]
pub struct ObjectDef {
    pub name: String,
    pub fields: Vec<(String, Ty)>,
}

/// One module global: ordered source-order initializer run once before
/// `main`. `ty` is runtime-erased (no `*`), `init` computes the initial
/// value with `result` holding it. Initializers may call functions that
/// read earlier globals; backends allocate module state before running any
/// initializer.
#[derive(Debug, Clone)]
pub struct Global {
    pub id: u32,
    pub name: String,
    pub ty: Ty,
    pub init: Vec<Instr>,
    pub result: Reg,
    pub span: Span,
}

#[derive(Debug, Clone, Default)]
pub struct LirProgram {
    pub module: String,
    /// Whether this module owns the standalone/project VM entrypoint.
    pub entrypoint: bool,
    /// Project entrypoint ownership, carried explicitly through the pipeline.
    pub entrypoint_module: Option<String>,
    pub objects: Vec<ObjectDef>,
    pub globals: Vec<Global>,
    pub functions: Vec<Function>,
    pub imports: Vec<FunctionImport>,
}

impl LirProgram {
    /// Human-readable dump (`vl build --emit lir`, golden tests).
    /// Displays runtime-erased types; capabilities have served their purpose.
    pub fn dump(&self) -> String {
        let mut out = String::new();
        for g in &self.globals {
            out.push_str(&format!("global %{} {} : {} =\n", g.id, g.name, g.ty));
            for (i, ins) in g.init.iter().enumerate() {
                out.push_str(&format!("  {i:>3}: {}\n", fmt_instr(ins)));
            }
            out.push_str(&format!("  init %{}\n", g.result.0));
        }
        for f in &self.functions {
            out.push_str(&format!("fn {}:\n", f.name));
            for (i, ins) in f.instrs.iter().enumerate() {
                out.push_str(&format!("  {i:>3}: {}\n", fmt_instr(ins)));
            }
        }
        out
    }

    /// Boundary validation: no capability, generic, literal, or poison type
    /// may reach backend emission (signatures, layouts, globals, imports, and
    /// every instruction's type metadata). Returns the offending description,
    /// if any.
    pub fn validate_runtime(&self) -> Option<String> {
        fn bad(ty: &Ty) -> bool {
            matches!(ty, Ty::Mutable(_) | Ty::Param(_) | Ty::Int | Ty::Error)
                || match ty {
                    Ty::Array(elem) => bad(elem),
                    Ty::Union(u) => u.args.iter().any(bad),
                    Ty::Tuple(fields) => fields.iter().any(|(_, t)| bad(t)),
                    _ => false,
                }
        }
        for import in &self.imports {
            for ty in import.param_tys.iter().chain(std::iter::once(&import.ret)) {
                if bad(ty) {
                    return Some(format!(
                        "import {}::{} has non-runtime type `{ty}`",
                        import.symbol.module, import.symbol.function
                    ));
                }
            }
        }
        for o in &self.objects {
            for (_, ty) in &o.fields {
                if bad(ty) {
                    return Some(format!(
                        "object {} field has non-runtime type `{ty}`",
                        o.name
                    ));
                }
            }
        }
        for g in &self.globals {
            if bad(&g.ty) {
                return Some(format!("global {} has non-runtime type `{}`", g.name, g.ty));
            }
            for ins in &g.init {
                if let Some(ty) = instr_ty(ins) {
                    if bad(ty) {
                        return Some(format!(
                            "global {} init has non-runtime type `{ty}`",
                            g.name
                        ));
                    }
                }
            }
        }
        for f in &self.functions {
            for ty in f.param_tys.iter().chain(std::iter::once(&f.ret)) {
                if bad(ty) {
                    return Some(format!("function {} has non-runtime type `{ty}`", f.name));
                }
            }
            for ins in &f.instrs {
                if let Some(ty) = instr_ty(ins) {
                    if bad(ty) {
                        return Some(format!(
                            "function {} instruction has non-runtime type `{ty}`",
                            f.name
                        ));
                    }
                }
                if let Some(tys) = instr_tuple_tys(ins) {
                    for ty in tys {
                        if bad(ty) {
                            return Some(format!(
                                "function {} instruction has non-runtime type `{ty}`",
                                f.name
                            ));
                        }
                    }
                }
                if let Some(tys) = instr_variant_tys(ins) {
                    for ty in tys {
                        if bad(ty) {
                            return Some(format!(
                                "function {} instruction has non-runtime type `{ty}`",
                                f.name
                            ));
                        }
                    }
                }
            }
        }
        for g in &self.globals {
            for ins in &g.init {
                if let Some(tys) = instr_tuple_tys(ins) {
                    for ty in tys {
                        if bad(ty) {
                            return Some(format!(
                                "global {} init has non-runtime type `{ty}`",
                                g.name
                            ));
                        }
                    }
                }
                if let Some(tys) = instr_variant_tys(ins) {
                    for ty in tys {
                        if bad(ty) {
                            return Some(format!(
                                "global {} init has non-runtime type `{ty}`",
                                g.name
                            ));
                        }
                    }
                }
            }
        }
        for i in &self.imports {
            for ty in i.param_tys.iter().chain(std::iter::once(&i.ret)) {
                if bad(ty) {
                    return Some(format!("import {} has non-runtime type `{ty}`", i.symbol));
                }
            }
        }
        None
    }
}

/// Runtime type metadata carried by one instruction, if any.
fn instr_ty(ins: &Instr) -> Option<&Ty> {
    match ins {
        Instr::NewArray { elem, .. }
        | Instr::ArrayLit { elem, .. }
        | Instr::ArrayGet { elem, .. }
        | Instr::ArraySet { elem, .. } => Some(elem),
        Instr::ObjectGet { ty, .. } | Instr::ObjectSet { ty, .. } => Some(ty),
        Instr::Cast { target, .. } => Some(target),
        // Tuples and variants carry a `Vec<Ty>`; validated element-wise in
        // `validate_runtime`.
        Instr::TupleLit { .. }
        | Instr::TupleGet { .. }
        | Instr::TupleSet { .. }
        | Instr::NewVariant { .. }
        | Instr::PayloadGet { .. } => None,
        _ => None,
    }
}

/// Element types carried by tuple instructions, if any.
fn instr_tuple_tys(ins: &Instr) -> Option<&Vec<Ty>> {
    match ins {
        Instr::TupleLit { tys, .. } | Instr::TupleGet { tys, .. } | Instr::TupleSet { tys, .. } => {
            Some(tys)
        }
        _ => None,
    }
}

/// Payload types carried by variant instructions, if any.
fn instr_variant_tys(ins: &Instr) -> Option<&Vec<Ty>> {
    match ins {
        Instr::NewVariant { tys, .. } | Instr::PayloadGet { tys, .. } => Some(tys),
        _ => None,
    }
}

fn fmt_instr(ins: &Instr) -> String {
    match ins {
        Instr::Const { dst, value, .. } => format!("%{} = const {}", dst.0, fmt_scalar(*value)),
        Instr::StringConst { dst, value, .. } => {
            format!("%{} = string {value:?}", dst.0)
        }
        Instr::Param { dst, index, .. } => format!("%{} = param {index}", dst.0),
        Instr::Copy { dst, src, .. } => format!("%{} = copy %{}", dst.0, src.0),
        Instr::Not { dst, src, .. } => format!("%{} = not %{}", dst.0, src.0),
        Instr::BinOp {
            dst, op, lhs, rhs, ..
        } => {
            format!("%{} = {op} %{} %{}", dst.0, lhs.0, rhs.0)
        }
        Instr::Call {
            dst, callee, args, ..
        } => {
            let args = args
                .iter()
                .map(|arg| format!("%{}", arg.0))
                .collect::<Vec<_>>()
                .join(", ");
            if callee.module == "<anonymous>" {
                format!("%{} = call {}({args})", dst.0, callee.function)
            } else {
                format!(
                    "%{} = call {}::{}({args})",
                    dst.0, callee.module, callee.function
                )
            }
        }
        Instr::NewArray { dst, len, elem, .. } => {
            format!("%{} = new_array %{} : {elem}", dst.0, len.0)
        }
        Instr::ArrayLit {
            dst, elems, elem, ..
        } => {
            let elems = elems
                .iter()
                .map(|e| format!("%{}", e.0))
                .collect::<Vec<_>>()
                .join(", ");
            format!("%{} = array_lit [{elems}] : {elem}", dst.0)
        }
        Instr::ArrayGet {
            dst,
            array,
            index,
            elem,
            ..
        } => {
            format!("%{} = array_get %{}[%{}] : {elem}", dst.0, array.0, index.0)
        }
        Instr::ArraySet {
            array,
            index,
            value,
            elem,
            ..
        } => {
            format!(
                "array_set %{}[%{}], %{} : {elem}",
                array.0, index.0, value.0
            )
        }
        Instr::NewObject {
            dst, name, fields, ..
        } => {
            let fields = fields
                .iter()
                .map(|(name, reg)| format!("{name}: %{}", reg.0))
                .collect::<Vec<_>>()
                .join(", ");
            format!("%{} = new_object {name} {{{fields}}}", dst.0)
        }
        Instr::ObjectGet {
            dst, object, name, ..
        } => format!("%{} = object_get %{}.{}", dst.0, object.0, name),
        Instr::ObjectSet {
            object,
            name,
            value,
            ..
        } => format!("object_set %{}.{} = %{}", object.0, name, value.0),
        Instr::TupleLit {
            dst, elems, tys, ..
        } => {
            let elems = elems
                .iter()
                .map(|e| format!("%{}", e.0))
                .collect::<Vec<_>>()
                .join(", ");
            let tys = tys
                .iter()
                .map(|t| format!("{t}"))
                .collect::<Vec<_>>()
                .join(", ");
            format!("%{} = tuple_lit [{elems}] : #({tys})", dst.0)
        }
        Instr::TupleGet {
            dst,
            tuple,
            index,
            tys,
            ..
        } => {
            let tys = tys
                .iter()
                .map(|t| format!("{t}"))
                .collect::<Vec<_>>()
                .join(", ");
            format!("%{} = tuple_get %{}[{index}] : #({tys})", dst.0, tuple.0)
        }
        Instr::TupleSet {
            tuple,
            index,
            value,
            tys,
            ..
        } => {
            let tys = tys
                .iter()
                .map(|t| format!("{t}"))
                .collect::<Vec<_>>()
                .join(", ");
            format!("tuple_set %{}[{index}], %{} : #({tys})", tuple.0, value.0)
        }
        Instr::NewVariant {
            dst,
            union,
            variant,
            tag,
            args,
            tys,
            ..
        } => {
            let args = args
                .iter()
                .map(|a| format!("%{}", a.0))
                .collect::<Vec<_>>()
                .join(", ");
            let tys = tys
                .iter()
                .map(|t| format!("{t}"))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "%{} = new_variant {union}.{variant}#{tag} [{args}] : ({tys})",
                dst.0
            )
        }
        Instr::TagOf { dst, scrut, .. } => {
            format!("%{} = tag_of %{}", dst.0, scrut.0)
        }
        Instr::PayloadGet {
            dst,
            scrut,
            index,
            tys,
            ..
        } => {
            let tys = tys
                .iter()
                .map(|t| format!("{t}"))
                .collect::<Vec<_>>()
                .join(", ");
            format!("%{} = payload_get %{}[{index}] : ({tys})", dst.0, scrut.0)
        }
        Instr::Cast {
            dst, src, target, ..
        } => {
            format!("%{} = cast %{} : {target}", dst.0, src.0)
        }
        Instr::Ret { src, .. } => format!("ret %{}", src.0),
        Instr::GlobalLoad { dst, global, .. } => format!("%{} = global_load %{}", dst.0, global),
        Instr::GlobalStore { global, src, .. } => format!("global_store %{}, %{}", global, src.0),
        Instr::BranchIfFalse { cond, target, .. } => {
            format!("branch_if_false %{} -> L{}", cond.0, target)
        }
        Instr::Jump { target, .. } => format!("jump -> L{}", target),
        Instr::Label { id, .. } => format!("L{}:", id),
    }
}

fn fmt_scalar(value: Scalar) -> String {
    match value {
        Scalar::Int(v) => format!("{v}int"),
        Scalar::U64(v) => format!("{v}u64"),
        Scalar::I64(v) => format!("{v}i64"),
        Scalar::F64(v) => format!("{}f64", f64::from_bits(v)),
        Scalar::Bool(v) => v.to_string(),
        Scalar::U8(v) => format!("{v}u8"),
    }
}

struct LoopTargets {
    break_target: u32,
    continue_target: u32,
}

struct Lowerer<'t> {
    next: u32,
    instrs: Vec<Instr>,
    /// Values currently available in this function. Locals (including params)
    /// are assigned when declared; globals live in module state and are
    /// accessed via explicit `GlobalLoad`/`GlobalStore` (never cached here).
    /// Assignment writes in place (`Copy` into the bound register) so values
    /// stay correct across branches and loop iterations without phi nodes.
    bindings: HashMap<u32, Reg>,
    /// Top-level `DefId.0` -> stable global ID (`LirProgram::globals` index).
    globals: HashMap<u32, u32>,
    next_label: u32,
    loop_stack: Vec<LoopTargets>,
    /// Instance substitution (`Param` -> concrete) for monomorphized bodies;
    /// empty when lowering non-generic code.
    env: HashMap<String, Ty>,
    /// Mangled instance name when lowering a monomorphized body (used to
    /// resolve inner generic calls per instance); `None` for root code.
    outer: Option<String>,
    /// World plan for project builds (`None` for single-module lowering).
    /// When present, concrete targets take precedence over HIR symbols.
    plan: Option<&'t vl_typecheck::world::MonomorphizationPlan>,
    /// Outer instance key when lowering a world-plan body (`None` for roots).
    outer_key: Option<vl_typecheck::InstanceKey>,
    typed: &'t vl_typecheck::TypedProgram,
    module: &'t str,
}

/// Runtime-erased type: capability qualifiers removed recursively.
/// LIR and backends never see `*`.
fn rt(ty: &Ty) -> Ty {
    ty.erase_capability()
}

fn collect_import_expr(
    expr: &HirExpr,
    typed: &vl_typecheck::TypedProgram,
    out: &mut Vec<FunctionImport>,
) {
    match expr {
        HirExpr::Call {
            symbol: Some(symbol),
            args,
            id,
            ..
        } => {
            let reference = FunctionRef {
                module: symbol.module.as_string(),
                function: symbol.name.clone(),
            };
            if !out.iter().any(|i| i.symbol == reference) {
                let param_tys = args
                    .iter()
                    .filter_map(|a| typed.type_of_id(a.id()).map(|t| rt(&t)))
                    .collect();
                let ret = typed.type_of_id(*id).map(|t| rt(&t)).unwrap_or(Ty::Error);
                out.push(FunctionImport {
                    symbol: reference,
                    param_tys,
                    ret,
                });
            }
            for arg in args {
                collect_import_expr(arg, typed, out);
            }
        }
        HirExpr::Call { args, .. } => {
            for arg in args {
                collect_import_expr(arg, typed, out)
            }
        }
        HirExpr::MethodCall {
            id, receiver, args, ..
        } => {
            // Foreign sugar targets import like foreign calls; the receiver
            // counts as the first parameter for the import signature.
            if let Some(sym) = typed.method_symbols.get(&id.0) {
                let reference = FunctionRef {
                    module: sym.module.as_string(),
                    function: sym.name.clone(),
                };
                if !out.iter().any(|i| i.symbol == reference) {
                    let mut param_tys = vec![typed
                        .type_of_id(receiver.id())
                        .map(|t| rt(&t))
                        .unwrap_or(Ty::Error)];
                    param_tys.extend(
                        args.iter()
                            .filter_map(|a| typed.type_of_id(a.id()).map(|t| rt(&t))),
                    );
                    let ret = typed.type_of_id(*id).map(|t| rt(&t)).unwrap_or(Ty::Error);
                    out.push(FunctionImport {
                        symbol: reference,
                        param_tys,
                        ret,
                    });
                }
            }
            collect_import_expr(receiver, typed, out);
            for arg in args {
                collect_import_expr(arg, typed, out)
            }
        }
        HirExpr::ArrayLiteral { elems, .. } => {
            for e in elems {
                collect_import_expr(e, typed, out)
            }
        }
        HirExpr::ObjectLiteral { fields, .. } => {
            for (_, e) in fields {
                collect_import_expr(e, typed, out)
            }
        }
        HirExpr::Variant { args, .. } => {
            for a in args {
                collect_import_expr(a, typed, out)
            }
        }
        HirExpr::Index { base, index, .. } => {
            collect_import_expr(base, typed, out);
            collect_import_expr(index, typed, out);
        }
        HirExpr::TupleLiteral { elems, .. } => {
            for (_, e) in elems {
                collect_import_expr(e, typed, out)
            }
        }
        HirExpr::TupleIndex { base, .. } => collect_import_expr(base, typed, out),
        HirExpr::Field { base, .. }
        | HirExpr::Unary { inner: base, .. }
        | HirExpr::Cast { inner: base, .. } => collect_import_expr(base, typed, out),
        HirExpr::Binary { lhs, rhs, .. } => {
            collect_import_expr(lhs, typed, out);
            collect_import_expr(rhs, typed, out);
        }
        HirExpr::Literal { .. } | HirExpr::String { .. } | HirExpr::Var { .. } => {}
    }
}

fn collect_import_stmt(
    stmt: &HirStmt,
    typed: &vl_typecheck::TypedProgram,
    out: &mut Vec<FunctionImport>,
) {
    match stmt {
        HirStmt::Let { value, .. } | HirStmt::Assign { value, .. } | HirStmt::Expr(value) => {
            collect_import_expr(value, typed, out)
        }
        HirStmt::Return {
            value: Some(value), ..
        } => collect_import_expr(value, typed, out),
        HirStmt::IndexAssign {
            array,
            index,
            value,
            ..
        } => {
            collect_import_expr(array, typed, out);
            collect_import_expr(index, typed, out);
            collect_import_expr(value, typed, out);
        }
        HirStmt::FieldAssign { base, value, .. } => {
            collect_import_expr(base, typed, out);
            collect_import_expr(value, typed, out);
        }
        HirStmt::TupleAssign { base, value, .. } => {
            collect_import_expr(base, typed, out);
            collect_import_expr(value, typed, out);
        }
        HirStmt::Destructure { value, .. } => collect_import_expr(value, typed, out),
        HirStmt::Match {
            scrutinee,
            arms,
            else_body,
            ..
        } => {
            collect_import_expr(scrutinee, typed, out);
            for arm in arms {
                for s in &arm.body {
                    collect_import_stmt(s, typed, out);
                }
            }
            if let Some(body) = else_body {
                for s in body {
                    collect_import_stmt(s, typed, out);
                }
            }
        }
        HirStmt::If {
            condition,
            then_body,
            else_body,
            ..
        } => {
            collect_import_expr(condition, typed, out);
            for s in then_body {
                collect_import_stmt(s, typed, out);
            }
            if let Some(body) = else_body {
                for s in body {
                    collect_import_stmt(s, typed, out);
                }
            }
        }
        HirStmt::While {
            condition, body, ..
        } => {
            collect_import_expr(condition, typed, out);
            for s in body {
                collect_import_stmt(s, typed, out);
            }
        }
        HirStmt::Return { value: None, .. } | HirStmt::Break { .. } | HirStmt::Continue { .. } => {}
    }
}

fn collect_imports(prog: &HirProgram, typed: &vl_typecheck::TypedProgram) -> Vec<FunctionImport> {
    let mut out = Vec::new();
    for item in &prog.items {
        match item {
            HirItem::Fn { body, .. } => {
                for stmt in body {
                    collect_import_stmt(stmt, typed, &mut out)
                }
            }
            HirItem::Let { value, .. } => collect_import_expr(value, typed, &mut out),
            HirItem::Destructure { value, .. } => collect_import_expr(value, typed, &mut out),
            HirItem::Object { .. } | HirItem::Union { .. } => {}
        }
    }
    out
}

impl Lowerer<'_> {
    fn typed_module(&self) -> String {
        self.module.to_owned()
    }
    fn reg(&mut self) -> Reg {
        let r = Reg(self.next);
        self.next += 1;
        r
    }

    fn label(&mut self) -> u32 {
        let label = self.next_label;
        self.next_label += 1;
        label
    }

    /// Recorded type with the current instance substitution applied.
    /// `None` means poisoned (already reported; the caller skips).
    fn resolved_ty(&self, id: vl_hir::HirId) -> Option<Ty> {
        let ty = subst_ty(&self.typed.type_of_id(id)?, &self.env);
        if ty == Ty::Error || !ty.is_concrete() {
            return None;
        }
        Some(ty)
    }

    /// Erased element type of the array produced by `node`.
    /// Looks through `*` so `*Array[T]` still yields `T`.
    fn array_elem_of(&self, id: vl_hir::HirId) -> Option<Ty> {
        self.resolved_ty(id)?.array_elem().cloned().map(|t| rt(&t))
    }

    /// Erased element types of the tuple produced by `node`, in order.
    /// Looks through `*` so `*#(...)` still yields its elements.
    fn tuple_tys_of(&self, id: vl_hir::HirId) -> Option<Vec<Ty>> {
        let ty = self.resolved_ty(id)?;
        let elems = ty.tuple_elems()?;
        Some(elems.into_iter().map(|(_, t)| rt(&t)).collect())
    }
}

/// Bind a `var`/`val` value in its own local home. Rebinding one local must not
/// change another that received the same value (`var b = a; a = 2;` leaves
/// `b` alone for values and references alike). Capabilities are already
/// erased, so all types alias the same way here.
fn bind_local(l: &mut Lowerer, def: Option<&vl_hir::DefId>, value: &HirExpr, reg: Reg) {
    let Some(def) = def else {
        return;
    };
    let dst = l.reg();
    l.instrs.push(Instr::Copy {
        dst,
        src: reg,
        span: value.span(),
    });
    l.bindings.insert(def.0, dst);
}

/// One top-level statement inside a function body. Shared by monomorphic
/// functions and monomorphized instances.
fn lower_fn_stmt(
    l: &mut Lowerer,
    stmt: &HirStmt,
    typed: &vl_typecheck::TypedProgram,
    topped_return: &mut bool,
) {
    match stmt {
        HirStmt::Let { def, value, .. } => {
            if let Some(reg) = l.lower_expr(value, typed) {
                bind_local(l, def.as_ref(), value, reg);
            }
            *topped_return = false;
        }
        HirStmt::Expr(e) => {
            // Discarded value: no implicit return.
            let _ = l.lower_expr(e, typed);
            *topped_return = false;
        }
        HirStmt::Return { value, span } => {
            l.lower_return(value.as_ref(), typed, *span);
            *topped_return = true;
        }
        HirStmt::If {
            condition,
            then_body,
            else_body,
            span,
        } => {
            l.lower_if(condition, then_body, else_body.as_deref(), typed, *span);
            *topped_return = false;
        }
        HirStmt::Match {
            scrutinee,
            arms,
            else_body,
            span,
        } => {
            l.lower_match(scrutinee, arms, else_body.as_deref(), typed, *span);
            *topped_return = false;
        }
        HirStmt::Assign {
            def, value, span, ..
        } => {
            l.lower_assign(def.as_ref(), value, typed, *span);
            *topped_return = false;
        }
        HirStmt::IndexAssign {
            array,
            index,
            value,
            span,
            ..
        } => {
            l.lower_index_assign(array, index, value, typed, *span);
            *topped_return = false;
        }
        HirStmt::FieldAssign {
            base,
            field,
            value,
            span,
            ..
        } => {
            l.lower_field_assign(base, field, value, typed, *span);
            *topped_return = false;
        }
        HirStmt::TupleAssign {
            base,
            index,
            value,
            span,
            ..
        } => {
            l.lower_tuple_assign(base, *index, value, typed, *span);
            *topped_return = false;
        }
        HirStmt::Destructure {
            bindings,
            value,
            span,
            ..
        } => {
            l.lower_destructure(bindings, value, typed, *span);
            *topped_return = false;
        }
        HirStmt::While {
            condition,
            body,
            span,
        } => {
            l.lower_while(condition, body, typed, *span);
            *topped_return = false;
        }
        HirStmt::Break { span } => {
            l.lower_break(*span);
            *topped_return = false;
        }
        HirStmt::Continue { span } => {
            l.lower_continue(*span);
            *topped_return = false;
        }
    }
}

/// Function epilogue: every function ends with a `Ret` so backends always
/// see a well-formed epilogue. Explicit `return` emits its own `Ret` inline
/// (VM `ret` transfers control immediately, so later instructions are
/// unreachable fallthrough); only emit the default fallthrough when the top
/// level does not end with an unconditional `return`. The payload is a
/// normalized `u64` zero (`void` backends ignore it); never an unresolved
/// `int`.
fn lower_fn_epilogue(l: &mut Lowerer, topped_return: bool) {
    let ends_with_ret = topped_return && matches!(l.instrs.last(), Some(Instr::Ret { .. }));
    if !ends_with_ret {
        let r = l.reg();
        l.instrs.push(Instr::Const {
            dst: r,
            value: Scalar::U64(0),
            span: Span::empty(0),
        });
        l.instrs.push(Instr::Ret {
            src: r,
            span: Span::empty(0),
        });
    }
}

/// Tuple position for one destructure binding given the base tuple type.
/// Unnamed tuples bind positionally (`b.index`); named tuples resolve by
/// explicit `field:` or by the binding name (shorthand). `None` when the
/// base is not a tuple or the field is unknown (already reported).
fn destructure_position(base: &Ty, b: &vl_hir::HirDestructureBinding) -> Option<usize> {
    let elems = base.tuple_elems()?;
    if elems.iter().all(|(n, _)| n.is_none()) {
        if b.index < elems.len() {
            return Some(b.index);
        }
        return None;
    }
    if let Some(field) = &b.field {
        return elems
            .iter()
            .position(|(n, _)| n.as_deref() == Some(field.as_str()));
    }
    // Shorthand: binding name is the field.
    if let Some(pos) = elems
        .iter()
        .position(|(n, _)| n.as_deref() == Some(b.binding.as_str()))
    {
        return Some(pos);
    }
    // Fallback to pattern index when in range (defensive; typecheck reports).
    if b.index < elems.len() {
        return Some(b.index);
    }
    None
}

/// Lower typed HIR to LIR. Poisoned (`Error`-typed) nodes are skipped —
/// errors were already reported, so no new diagnostics are produced here.
/// Capabilities are erased (`*Foo` -> `Foo`); globals become an ordered
/// table with explicit load/store operations run once before `main`.
pub fn lower(prog: &HirProgram, typed: &vl_typecheck::TypedProgram) -> LirProgram {
    // Runtime-erased object layouts (capabilities served their purpose).
    let mut objects = typed
        .objects
        .iter()
        .map(|(name, sig)| ObjectDef {
            name: name.clone(),
            fields: sig.fields.iter().map(|(n, t)| (n.clone(), rt(t))).collect(),
        })
        .collect::<Vec<_>>();
    objects.sort_by(|a, b| a.name.cmp(&b.name));

    // Stable global IDs in source order: top-level bindings with definitions.
    // Poisoned initializers still reserve an ID so later indices stay stable,
    // but their bodies are empty (backends never run them because lowering
    // is blocked on prior errors).
    //
    // Destructured bindings expand to one global each. The shared base
    // expression is evaluated once into a hidden base global immediately
    // before its bindings; each binding then loads the base and extracts
    // its element. Without this, the base would evaluate once per binding
    // (duplicating side effects).
    //
    // Entry: (def for `global_map`, debug name, item id, base/value expr,
    // span, destructure binding plus its hidden base gid).
    type GlobalEntry = (
        Option<u32>,
        String,
        vl_hir::HirId,
        HirExpr,
        Span,
        Option<(vl_hir::HirDestructureBinding, u32)>,
    );
    let mut global_items: Vec<GlobalEntry> = Vec::new();
    for item in &prog.items {
        match item {
            HirItem::Let {
                def: Some(def),
                value,
                id,
                span,
                ..
            } => {
                // Top-level name for debugging; HIR `Let` has no name field
                // here, so use the definition span? Use `binding#id` fallback and
                // try to recover the name from... HIR Item::Let has no name?
                // Actually HirItem::Let has no `name` in this version? Check:
                // it has `def` only. Use `g{id}`.
                global_items.push((
                    Some(def.0),
                    format!("g{}", def.0),
                    *id,
                    value.clone(),
                    *span,
                    None,
                ));
            }
            HirItem::Destructure {
                bindings,
                value,
                id,
                span,
                ..
            } => {
                let hidden_gid = global_items.len() as u32;
                global_items.push((
                    None,
                    format!("g{hidden_gid}_tuplebase"),
                    *id,
                    value.clone(),
                    *span,
                    None,
                ));
                for b in bindings {
                    if let Some(def) = &b.def {
                        global_items.push((
                            Some(def.0),
                            format!("g{}", def.0),
                            *id,
                            value.clone(),
                            *span,
                            Some((b.clone(), hidden_gid)),
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    // Hidden base globals carry no `DefId` and stay out of the map; only
    // real bindings are addressable by later code.
    let global_map: HashMap<u32, u32> = global_items
        .iter()
        .enumerate()
        .filter_map(|(idx, (def, _, _, _, _, _))| def.map(|d| (d, idx as u32)))
        .collect();

    let mut out = LirProgram {
        module: prog.module.clone(),
        entrypoint: false,
        entrypoint_module: None,
        objects,
        globals: Vec::new(),
        functions: Vec::new(),
        imports: collect_imports(prog, typed),
    };

    // Globals first (source order): initializers may read earlier globals via
    // `GlobalLoad`; forward references are poisoned (E305) and lower to empty.
    for (idx, (_def, name, id, value, span, binding)) in global_items.iter().enumerate() {
        let gid = idx as u32;
        // Destructured bindings take their element type from the base tuple
        // and load the hidden base global (evaluated once); plain lets use
        // the item/value type as before.
        if let Some((b, hidden)) = binding {
            let base_ty = typed
                .type_of_id(*id)
                .or_else(|| typed.type_of_id(value.id()))
                .unwrap_or(Ty::Error);
            let Some(pos) = destructure_position(&base_ty, b) else {
                out.globals.push(Global {
                    id: gid,
                    name: name.clone(),
                    ty: Ty::Error,
                    init: Vec::new(),
                    result: Reg(u32::MAX),
                    span: *span,
                });
                continue;
            };
            let Some(elems) = base_ty.tuple_elems() else {
                out.globals.push(Global {
                    id: gid,
                    name: name.clone(),
                    ty: Ty::Error,
                    init: Vec::new(),
                    result: Reg(u32::MAX),
                    span: *span,
                });
                continue;
            };
            let Some((_, elem_ty)) = elems.get(pos).cloned() else {
                out.globals.push(Global {
                    id: gid,
                    name: name.clone(),
                    ty: Ty::Error,
                    init: Vec::new(),
                    result: Reg(u32::MAX),
                    span: *span,
                });
                continue;
            };
            if elem_ty == Ty::Error || !elem_ty.is_concrete() {
                out.globals.push(Global {
                    id: gid,
                    name: name.clone(),
                    ty: Ty::Error,
                    init: Vec::new(),
                    result: Reg(u32::MAX),
                    span: *span,
                });
                continue;
            }
            let rty = rt(&elem_ty);
            let tys: Vec<Ty> = elems.into_iter().map(|(_, t)| rt(&t)).collect();
            let mut l = Lowerer {
                next: 0,
                instrs: vec![],
                bindings: HashMap::new(),
                globals: global_map.clone(),
                next_label: 0,
                loop_stack: Vec::new(),
                env: HashMap::new(),
                outer: None,
                plan: None,
                outer_key: None,
                typed,
                module: prog.module.as_str(),
            };
            // The hidden base global ran before us; load it and extract.
            // (Its own init lowered the shared base expression exactly once.)
            let base_reg = l.reg();
            l.instrs.push(Instr::GlobalLoad {
                dst: base_reg,
                global: *hidden,
                span: *span,
            });
            let dst = l.reg();
            l.instrs.push(Instr::TupleGet {
                dst,
                tuple: base_reg,
                index: pos,
                tys,
                span: *span,
            });
            out.globals.push(Global {
                id: gid,
                name: name.clone(),
                ty: rty,
                init: l.instrs,
                result: dst,
                span: *span,
            });
            continue;
        }
        let ty = typed
            .type_of_id(*id)
            .or_else(|| typed.type_of_id(value.id()))
            .unwrap_or(Ty::Error);
        // Poisoned globals lower to empty (lowering blocked on prior errors).
        if ty == Ty::Error || !ty.is_concrete() {
            out.globals.push(Global {
                id: gid,
                name: name.clone(),
                ty: Ty::Error,
                init: Vec::new(),
                result: Reg(u32::MAX),
                span: *span,
            });
            continue;
        }
        let rty = rt(&ty);
        let mut l = Lowerer {
            next: 0,
            instrs: vec![],
            bindings: HashMap::new(),
            globals: global_map.clone(),
            next_label: 0,
            loop_stack: Vec::new(),
            env: HashMap::new(),
            outer: None,
            plan: None,
            outer_key: None,
            typed,
            module: prog.module.as_str(),
        };
        if let Some(r) = l.lower_expr(value, typed) {
            out.globals.push(Global {
                id: gid,
                name: name.clone(),
                ty: rty,
                init: l.instrs,
                result: r,
                span: *span,
            });
        } else {
            out.globals.push(Global {
                id: gid,
                name: name.clone(),
                ty: rty,
                init: l.instrs,
                result: Reg(u32::MAX),
                span: *span,
            });
        }
    }

    for item in &prog.items {
        match item {
            HirItem::Object { .. } | HirItem::Union { .. } => {}
            HirItem::Let { .. } | HirItem::Destructure { .. } => {
                // Already emitted as globals above; no `<global>` functions.
            }
            HirItem::Fn {
                name,
                type_params,
                params,
                ret,
                body,
                ..
            } => {
                // Generic templates never emit directly: one function per
                // concrete instance is produced below.
                if !type_params.is_empty() {
                    continue;
                }
                let param_tys = params
                    .iter()
                    .map(|(_, _, t, _)| {
                        t.as_ref().map(|v| rt(&Ty::from_vl(v))).unwrap_or(Ty::Error)
                    })
                    .collect::<Vec<_>>();
                let ret_ty = ret
                    .as_ref()
                    .map(|v| rt(&Ty::from_vl(v)))
                    .unwrap_or(Ty::Error);
                let mut l = Lowerer {
                    next: 0,
                    instrs: vec![],
                    bindings: HashMap::new(),
                    globals: global_map.clone(),
                    next_label: 0,
                    loop_stack: Vec::new(),
                    env: HashMap::new(),
                    outer: None,
                    plan: None,
                    outer_key: None,
                    typed,
                    module: prog.module.as_str(),
                };

                for (index, (_, def, _, span)) in params.iter().enumerate() {
                    let dst = l.reg();
                    l.instrs.push(Instr::Param {
                        dst,
                        index,
                        span: *span,
                    });
                    if let Some(def) = def {
                        l.bindings.insert(def.0, dst);
                    }
                }

                let mut topped_return = false;
                for stmt in body {
                    lower_fn_stmt(&mut l, stmt, typed, &mut topped_return);
                }
                lower_fn_epilogue(&mut l, topped_return);
                out.functions.push(Function {
                    name: name.clone(),
                    param_tys,
                    ret: ret_ty,
                    instrs: l.instrs,
                });
            }
        }
    }
    // One function per concrete generic instance (sorted: deterministic
    // output for goldens). Templates themselves never emit.
    let mut mangled: Vec<&String> = typed.instances.keys().collect();
    mangled.sort();
    for m in mangled {
        let inst = &typed.instances[m];
        let template = prog.items.iter().find_map(|item| match item {
            HirItem::Fn {
                def: Some(d),
                type_params,
                params,
                body,
                ..
            } if d.0 == inst.orig => Some((type_params, params, body)),
            _ => None,
        });
        let Some((type_params, params, body)) = template else {
            continue;
        };
        let env: HashMap<String, Ty> = type_params
            .iter()
            .map(|p| p.name.clone())
            .zip(inst.args.iter().cloned())
            .collect();
        let mut l = Lowerer {
            next: 0,
            instrs: vec![],
            bindings: HashMap::new(),
            globals: global_map.clone(),
            next_label: 0,
            loop_stack: Vec::new(),
            env,
            outer: Some(m.clone()),
            plan: None,
            outer_key: None,
            typed,
            module: prog.module.as_str(),
        };
        for (index, (_, def, _, span)) in params.iter().enumerate() {
            let dst = l.reg();
            l.instrs.push(Instr::Param {
                dst,
                index,
                span: *span,
            });
            if let Some(def) = def {
                l.bindings.insert(def.0, dst);
            }
        }
        let mut topped_return = false;
        for stmt in body {
            lower_fn_stmt(&mut l, stmt, typed, &mut topped_return);
        }
        lower_fn_epilogue(&mut l, topped_return);
        out.functions.push(Function {
            name: m.clone(),
            param_tys: inst.sig.param_tys.iter().map(rt).collect(),
            ret: rt(&inst.sig.ret),
            instrs: l.instrs,
        });
    }
    out
}

/// Lower one stdlib generic instance body with its substitution and the world
/// plan. Used by `vl-stdlib` per-compilation linking; the caller assigns the
/// collision-proof local name and carries native imports.
#[allow(clippy::too_many_arguments)]
pub fn lower_stdlib_instance(
    hir: &HirProgram,
    typed: &vl_typecheck::TypedProgram,
    plan: &vl_typecheck::world::MonomorphizationPlan,
    outer_key: &vl_typecheck::InstanceKey,
    env: &HashMap<String, Ty>,
    params: &[(
        String,
        Option<vl_hir::DefId>,
        Option<vl_common::VlType>,
        Span,
    )],
    body: &[HirStmt],
    inst: &vl_typecheck::Instance,
) -> Option<Function> {
    // No globals inside stdlib helpers (enforced at load); use an empty map.
    let global_map: HashMap<u32, u32> = HashMap::new();
    // The mangled concrete name is assigned by the caller; use a placeholder
    // here for the outer mapping (only `outer_key` matters for nested calls).
    let placeholder = outer_key.mangled();
    let mut l = Lowerer {
        next: 0,
        instrs: vec![],
        bindings: HashMap::new(),
        globals: global_map,
        next_label: 0,
        loop_stack: Vec::new(),
        env: env.clone(),
        outer: Some(placeholder),
        plan: Some(plan),
        outer_key: Some(outer_key.clone()),
        typed,
        module: hir.module.as_str(),
    };
    for (index, (_, def, _, span)) in params.iter().enumerate() {
        let dst = l.reg();
        l.instrs.push(Instr::Param {
            dst,
            index,
            span: *span,
        });
        if let Some(def) = def {
            l.bindings.insert(def.0, dst);
        }
    }
    let mut topped_return = false;
    for stmt in body {
        lower_fn_stmt(&mut l, stmt, typed, &mut topped_return);
    }
    lower_fn_epilogue(&mut l, topped_return);
    Some(Function {
        name: String::new(),
        param_tys: inst.sig.param_tys.iter().map(rt).collect(),
        ret: rt(&inst.sig.ret),
        instrs: l.instrs,
    })
}

/// Lower one plan-sensitive monomorphic stdlib helper per compilation.
/// No substitution (monomorphic), but nested generic calls resolve via the
/// plan as roots of the helper's module.
pub fn lower_stdlib_mono(
    hir: &HirProgram,
    typed: &vl_typecheck::TypedProgram,
    plan: &vl_typecheck::world::MonomorphizationPlan,
    params: &[(
        String,
        Option<vl_hir::DefId>,
        Option<vl_common::VlType>,
        Span,
    )],
    body: &[HirStmt],
) -> Option<Function> {
    let global_map: HashMap<u32, u32> = HashMap::new();
    let mut l = Lowerer {
        next: 0,
        instrs: vec![],
        bindings: HashMap::new(),
        globals: global_map,
        next_label: 0,
        loop_stack: Vec::new(),
        env: HashMap::new(),
        outer: None,
        plan: Some(plan),
        outer_key: None,
        typed,
        module: hir.module.as_str(),
    };
    for (index, (_, def, _, span)) in params.iter().enumerate() {
        let dst = l.reg();
        l.instrs.push(Instr::Param {
            dst,
            index,
            span: *span,
        });
        if let Some(def) = def {
            l.bindings.insert(def.0, dst);
        }
    }
    let mut topped_return = false;
    for stmt in body {
        lower_fn_stmt(&mut l, stmt, typed, &mut topped_return);
    }
    lower_fn_epilogue(&mut l, topped_return);
    // Signature from HIR (monomorphic, runtime-erased).
    let param_tys = params
        .iter()
        .map(|(_, _, t, _)| t.as_ref().map(|v| rt(&Ty::from_vl(v))).unwrap_or(Ty::Error))
        .collect();
    // Return type: look up via first Param? Instead, derive from LIR? For
    // monomorphic helpers the cached body already has the signature; here we
    // reconstruct from HIR return annotation (must exist, else Error).
    // To avoid threading `ret` through, use the cached body's signature when
    // available? Simpler: leave `ret` as Error and let the caller fill from
    // checked FuncSigTy. For now, compute from HIR via a placeholder: the
    // caller (stdlib) will overwrite with the checked signature.
    Some(Function {
        name: String::new(),
        param_tys,
        ret: Ty::Error,
        instrs: l.instrs,
    })
}

/// Lower one project module with the complete world plan.
///
/// Differences from [`lower`]:
/// - Generic templates never emit directly; one concrete `Function` is emitted
///   per instance assigned to this owner in `plan.instances_by_owner`.
/// - Calls resolve through the plan before falling back to the HIR symbol,
///   so imported generic calls become concrete owner-module imports.
/// - Imports are built from emitted concrete code only (never from
///   uninstantiated templates, never the unmangled generic name),
///   deduplicated by concrete symbol and sorted deterministically.
/// - Capability erasure applies at the same boundary; backends see only
///   concrete locals and concrete cross-module imports.
pub fn lower_project(
    prog: &HirProgram,
    typed: &vl_typecheck::TypedProgram,
    plan: &vl_typecheck::world::MonomorphizationPlan,
) -> LirProgram {
    let mut objects = typed
        .objects
        .iter()
        .map(|(name, sig)| ObjectDef {
            name: name.clone(),
            fields: sig.fields.iter().map(|(n, t)| (n.clone(), rt(t))).collect(),
        })
        .collect::<Vec<_>>();
    objects.sort_by(|a, b| a.name.cmp(&b.name));

    let global_items: Vec<(u32, String, vl_hir::HirId, HirExpr, Span)> = prog
        .items
        .iter()
        .filter_map(|item| match item {
            HirItem::Let {
                def: Some(def),
                value,
                id,
                span,
                ..
            } => Some((def.0, format!("g{}", def.0), *id, value.clone(), *span)),
            _ => None,
        })
        .collect();
    let global_map: HashMap<u32, u32> = global_items
        .iter()
        .enumerate()
        .map(|(idx, (def, _, _, _, _))| (*def, idx as u32))
        .collect();

    let mut out = LirProgram {
        module: prog.module.clone(),
        entrypoint: false,
        entrypoint_module: None,
        objects,
        globals: Vec::new(),
        functions: Vec::new(),
        imports: Vec::new(),
    };

    for (idx, (_def, name, id, value, span)) in global_items.iter().enumerate() {
        let gid = idx as u32;
        let ty = typed
            .type_of_id(*id)
            .or_else(|| typed.type_of_id(value.id()))
            .unwrap_or(Ty::Error);
        if ty == Ty::Error || !ty.is_concrete() {
            out.globals.push(Global {
                id: gid,
                name: name.clone(),
                ty: Ty::Error,
                init: Vec::new(),
                result: Reg(u32::MAX),
                span: *span,
            });
            continue;
        }
        let rty = rt(&ty);
        let mut l = Lowerer {
            next: 0,
            instrs: vec![],
            bindings: HashMap::new(),
            globals: global_map.clone(),
            next_label: 0,
            loop_stack: Vec::new(),
            env: HashMap::new(),
            outer: None,
            plan: Some(plan),
            outer_key: None,
            typed,
            module: prog.module.as_str(),
        };
        if let Some(r) = l.lower_expr(value, typed) {
            out.globals.push(Global {
                id: gid,
                name: name.clone(),
                ty: rty,
                init: l.instrs,
                result: r,
                span: *span,
            });
        } else {
            out.globals.push(Global {
                id: gid,
                name: name.clone(),
                ty: rty,
                init: l.instrs,
                result: Reg(u32::MAX),
                span: *span,
            });
        }
    }

    for item in &prog.items {
        match item {
            HirItem::Object { .. } | HirItem::Union { .. } => {}
            HirItem::Let { .. } | HirItem::Destructure { .. } => {}
            HirItem::Fn {
                name,
                type_params,
                params,
                ret,
                body,
                ..
            } => {
                if !type_params.is_empty() {
                    continue;
                }
                let param_tys = params
                    .iter()
                    .map(|(_, _, t, _)| {
                        t.as_ref().map(|v| rt(&Ty::from_vl(v))).unwrap_or(Ty::Error)
                    })
                    .collect::<Vec<_>>();
                let ret_ty = ret
                    .as_ref()
                    .map(|v| rt(&Ty::from_vl(v)))
                    .unwrap_or(Ty::Error);
                let mut l = Lowerer {
                    next: 0,
                    instrs: vec![],
                    bindings: HashMap::new(),
                    globals: global_map.clone(),
                    next_label: 0,
                    loop_stack: Vec::new(),
                    env: HashMap::new(),
                    outer: None,
                    plan: Some(plan),
                    outer_key: None,
                    typed,
                    module: prog.module.as_str(),
                };
                for (index, (_, def, _, span)) in params.iter().enumerate() {
                    let dst = l.reg();
                    l.instrs.push(Instr::Param {
                        dst,
                        index,
                        span: *span,
                    });
                    if let Some(def) = def {
                        l.bindings.insert(def.0, dst);
                    }
                }
                let mut topped_return = false;
                for stmt in body {
                    lower_fn_stmt(&mut l, stmt, typed, &mut topped_return);
                }
                lower_fn_epilogue(&mut l, topped_return);
                out.functions.push(Function {
                    name: name.clone(),
                    param_tys,
                    ret: ret_ty,
                    instrs: l.instrs,
                });
            }
        }
    }

    // Owner instances from the plan, sorted deterministically by structural
    // key (owner, function, collision-free mangling). The emitted symbol is
    // derived via `key.mangled()` only here at the LIR boundary.
    if let Some(owned) = plan.instances_by_owner.get(&prog.module) {
        let mut keys: Vec<&vl_typecheck::InstanceKey> = owned.keys().collect();
        keys.sort();
        for key in keys {
            let inst = &owned[key];
            let m = key.mangled();
            let template = prog.items.iter().find_map(|item| match item {
                HirItem::Fn {
                    def: Some(d),
                    type_params,
                    params,
                    body,
                    ..
                } if d.0 == inst.orig => Some((type_params, params, body)),
                _ => None,
            });
            let Some((type_params, params, body)) = template else {
                continue;
            };
            let env: HashMap<String, Ty> = type_params
                .iter()
                .map(|p| p.name.clone())
                .zip(inst.args.iter().cloned())
                .collect();
            let mut l = Lowerer {
                next: 0,
                instrs: vec![],
                bindings: HashMap::new(),
                globals: global_map.clone(),
                next_label: 0,
                loop_stack: Vec::new(),
                env,
                outer: Some(m.clone()),
                plan: Some(plan),
                outer_key: Some(key.clone()),
                typed,
                module: prog.module.as_str(),
            };
            for (index, (_, def, _, span)) in params.iter().enumerate() {
                let dst = l.reg();
                l.instrs.push(Instr::Param {
                    dst,
                    index,
                    span: *span,
                });
                if let Some(def) = def {
                    l.bindings.insert(def.0, dst);
                }
            }
            let mut topped_return = false;
            for stmt in body {
                lower_fn_stmt(&mut l, stmt, typed, &mut topped_return);
            }
            lower_fn_epilogue(&mut l, topped_return);
            out.functions.push(Function {
                name: m,
                param_tys: inst.sig.param_tys.iter().map(rt).collect(),
                ret: rt(&inst.sig.ret),
                instrs: l.instrs,
            });
        }
    }

    out.imports = collect_project_imports(prog, typed, plan);
    out
}

/// Build imports from emitted concrete code only.
///
/// Walks non-generic bodies and global initializers (roots) plus the bodies of
/// emitted owner instances, resolving each call through the plan. Unmapped
/// monomorphic imports use their HIR symbol; uninstantiated templates are
/// never scanned and the unmangled generic name is never imported. Results are
/// deduplicated by concrete symbol (repeated uses must agree on types) and
/// sorted for deterministic dumps and artifacts.
fn collect_project_imports(
    prog: &HirProgram,
    typed: &vl_typecheck::TypedProgram,
    plan: &vl_typecheck::world::MonomorphizationPlan,
) -> Vec<FunctionImport> {
    use std::collections::BTreeMap;
    let mut by_symbol: BTreeMap<(String, String), FunctionImport> = BTreeMap::new();

    fn import_sig_for_target(
        plan: &vl_typecheck::world::MonomorphizationPlan,
        target: &vl_typecheck::world::ConcreteTarget,
    ) -> Option<(Vec<Ty>, Ty)> {
        plan.instances_by_owner
            .get(&target.key.template.module)?
            .get(&target.key)
            .map(|inst| {
                (
                    inst.sig.param_tys.iter().map(rt).collect(),
                    rt(&inst.sig.ret),
                )
            })
    }

    fn push_import(
        by_symbol: &mut BTreeMap<(String, String), FunctionImport>,
        symbol: FunctionRef,
        param_tys: Vec<Ty>,
        ret: Ty,
    ) {
        // Defensive: no generic, literal, or poison type may reach the backend.
        fn bad(ty: &Ty) -> bool {
            matches!(ty, Ty::Mutable(_) | Ty::Param(_) | Ty::Int | Ty::Error)
                || matches!(ty, Ty::Array(elem) if bad(elem))
        }
        if param_tys.iter().any(bad) || bad(&ret) {
            return;
        }
        let key = (symbol.module.clone(), symbol.function.clone());
        if let Some(existing) = by_symbol.get(&key) {
            // Repeated uses must agree on concrete types; keep the first
            // (typechecking already enforced consistency at each call site).
            if existing.param_tys != param_tys || existing.ret != ret {
                return;
            }
            return;
        }
        by_symbol.insert(
            key,
            FunctionImport {
                symbol,
                param_tys,
                ret,
            },
        );
    }

    fn walk_expr(
        prog: &HirProgram,
        typed: &vl_typecheck::TypedProgram,
        plan: &vl_typecheck::world::MonomorphizationPlan,
        outer: Option<&vl_typecheck::InstanceKey>,
        expr: &HirExpr,
        by_symbol: &mut BTreeMap<(String, String), FunctionImport>,
    ) {
        match expr {
            HirExpr::Call {
                id,
                symbol,
                extern_sig,
                args,
                ..
            } => {
                for arg in args {
                    walk_expr(prog, typed, plan, outer, arg, by_symbol);
                }
                let target = match outer {
                    Some(key) => plan.nested_targets.get(&(key.clone(), id.0)).cloned(),
                    None => plan.root_targets.get(&(prog.module.clone(), id.0)).cloned(),
                };
                // Unmapped source-generic calls are never imported unmangled
                // (the world should have mapped every generic in emitted code;
                // missing means lowering is blocked or the call was not emitted).
                if target.is_none() {
                    if let Some(sig) = extern_sig {
                        if !sig.type_params.is_empty() {
                            return;
                        }
                    }
                }
                if let Some(target) = target {
                    if target.key.template.module == prog.module {
                        return;
                    }
                    if let Some((param_tys, ret)) = import_sig_for_target(plan, &target) {
                        push_import(
                            by_symbol,
                            FunctionRef {
                                module: target.key.template.module.clone(),
                                function: target.key.mangled(),
                            },
                            param_tys,
                            ret,
                        );
                    }
                    return;
                }
                // No plan mapping: monomorphic import (or local call).
                if let Some(sym) = symbol {
                    let reference = FunctionRef {
                        module: sym.module.as_string(),
                        function: sym.name.clone(),
                    };
                    if reference.module == prog.module {
                        return;
                    }
                    // Only concrete monomorphic imports; generic templates
                    // without a plan mapping are never imported (they would
                    // be unmangled). Detect via the exported signature: if it
                    // is generic, skip (the world should have mapped it; if it
                    // didn't, lowering is blocked on errors or the call was
                    // inside an unemitted template).
                    // We approximate by checking the call's recorded types:
                    // generic template calls have `Param` in their arg/return
                    // types after substitution? For roots they are concrete.
                    // To stay safe, skip any import whose call-site types are
                    // not concrete-normalized.
                    let param_tys: Vec<Ty> = args
                        .iter()
                        .filter_map(|a| typed.type_of_id(a.id()).map(|t| rt(&t)))
                        .collect();
                    if param_tys.len() != args.len() {
                        return;
                    }
                    let ret = typed.type_of_id(*id).map(|t| rt(&t)).unwrap_or(Ty::Error);
                    // Skip non-runtime (Param/Int/Error/Mutable) — these are
                    // template-internal calls, not concrete imports.
                    fn bad(ty: &Ty) -> bool {
                        matches!(ty, Ty::Mutable(_) | Ty::Param(_) | Ty::Int | Ty::Error)
                            || matches!(ty, Ty::Array(elem) if bad(elem))
                    }
                    if param_tys.iter().any(bad) || bad(&ret) {
                        return;
                    }
                    push_import(by_symbol, reference, param_tys, ret);
                }
            }
            HirExpr::ArrayLiteral { elems, .. } => {
                for e in elems {
                    walk_expr(prog, typed, plan, outer, e, by_symbol);
                }
            }
            HirExpr::MethodCall {
                id, receiver, args, ..
            } => {
                walk_expr(prog, typed, plan, outer, receiver, by_symbol);
                for arg in args {
                    walk_expr(prog, typed, plan, outer, arg, by_symbol);
                }
                let target = match outer {
                    Some(key) => plan.nested_targets.get(&(key.clone(), id.0)).cloned(),
                    None => plan.root_targets.get(&(prog.module.clone(), id.0)).cloned(),
                };
                // Generic sugar without a plan mapping is never imported
                // unmangled (same rule as generic calls); only non-generic
                // sugar records a direct `method_targets` entry.
                if target.is_none() && !typed.method_targets.contains_key(&id.0) {
                    return;
                }
                if let Some(target) = target {
                    if target.key.template.module == prog.module {
                        return;
                    }
                    if let Some((param_tys, ret)) = import_sig_for_target(plan, &target) {
                        push_import(
                            by_symbol,
                            FunctionRef {
                                module: target.key.template.module.clone(),
                                function: target.key.mangled(),
                            },
                            param_tys,
                            ret,
                        );
                    }
                    return;
                }
                // No plan mapping: monomorphic foreign sugar (or local sugar).
                let Some(target) = typed.method_targets.get(&id.0) else {
                    return;
                };
                let vl_typecheck::MethodTarget::Foreign(sym) = target else {
                    return;
                };
                if sym.module.as_string() == prog.module {
                    return;
                }
                let reference = FunctionRef {
                    module: sym.module.as_string(),
                    function: sym.name.clone(),
                };
                let mut param_tys: Vec<Ty> = vec![typed
                    .type_of_id(receiver.id())
                    .map(|t| rt(&t))
                    .unwrap_or(Ty::Error)];
                param_tys.extend(
                    args.iter()
                        .filter_map(|a| typed.type_of_id(a.id()).map(|t| rt(&t))),
                );
                if param_tys.len() != args.len() + 1 {
                    return;
                }
                let ret = typed.type_of_id(*id).map(|t| rt(&t)).unwrap_or(Ty::Error);
                fn bad(ty: &Ty) -> bool {
                    matches!(ty, Ty::Mutable(_) | Ty::Param(_) | Ty::Int | Ty::Error)
                        || matches!(ty, Ty::Array(elem) if bad(elem))
                }
                if param_tys.iter().any(bad) || bad(&ret) {
                    return;
                }
                push_import(by_symbol, reference, param_tys, ret);
            }
            HirExpr::ObjectLiteral { fields, .. } => {
                for (_, e) in fields {
                    walk_expr(prog, typed, plan, outer, e, by_symbol);
                }
            }
            HirExpr::Variant { args, .. } => {
                for a in args {
                    walk_expr(prog, typed, plan, outer, a, by_symbol);
                }
            }
            HirExpr::TupleLiteral { elems, .. } => {
                for (_, e) in elems {
                    walk_expr(prog, typed, plan, outer, e, by_symbol);
                }
            }
            HirExpr::TupleIndex { base, .. } => {
                walk_expr(prog, typed, plan, outer, base, by_symbol);
            }
            HirExpr::Index { base, index, .. } => {
                walk_expr(prog, typed, plan, outer, base, by_symbol);
                walk_expr(prog, typed, plan, outer, index, by_symbol);
            }
            HirExpr::Field { base, .. }
            | HirExpr::Unary { inner: base, .. }
            | HirExpr::Cast { inner: base, .. } => {
                walk_expr(prog, typed, plan, outer, base, by_symbol)
            }
            HirExpr::Binary { lhs, rhs, .. } => {
                walk_expr(prog, typed, plan, outer, lhs, by_symbol);
                walk_expr(prog, typed, plan, outer, rhs, by_symbol);
            }
            HirExpr::Literal { .. } | HirExpr::String { .. } | HirExpr::Var { .. } => {}
        }
    }

    fn walk_stmt(
        prog: &HirProgram,
        typed: &vl_typecheck::TypedProgram,
        plan: &vl_typecheck::world::MonomorphizationPlan,
        outer: Option<&vl_typecheck::InstanceKey>,
        stmt: &HirStmt,
        by_symbol: &mut BTreeMap<(String, String), FunctionImport>,
    ) {
        match stmt {
            HirStmt::Let { value, .. } | HirStmt::Assign { value, .. } | HirStmt::Expr(value) => {
                walk_expr(prog, typed, plan, outer, value, by_symbol)
            }
            HirStmt::Return {
                value: Some(value), ..
            } => walk_expr(prog, typed, plan, outer, value, by_symbol),
            HirStmt::IndexAssign {
                array,
                index,
                value,
                ..
            } => {
                walk_expr(prog, typed, plan, outer, array, by_symbol);
                walk_expr(prog, typed, plan, outer, index, by_symbol);
                walk_expr(prog, typed, plan, outer, value, by_symbol);
            }
            HirStmt::FieldAssign { base, value, .. } => {
                walk_expr(prog, typed, plan, outer, base, by_symbol);
                walk_expr(prog, typed, plan, outer, value, by_symbol);
            }
            HirStmt::TupleAssign { base, value, .. } => {
                walk_expr(prog, typed, plan, outer, base, by_symbol);
                walk_expr(prog, typed, plan, outer, value, by_symbol);
            }
            HirStmt::Destructure { value, .. } => {
                walk_expr(prog, typed, plan, outer, value, by_symbol);
            }
            HirStmt::If {
                condition,
                then_body,
                else_body,
                ..
            } => {
                walk_expr(prog, typed, plan, outer, condition, by_symbol);
                for s in then_body {
                    walk_stmt(prog, typed, plan, outer, s, by_symbol);
                }
                if let Some(body) = else_body {
                    for s in body {
                        walk_stmt(prog, typed, plan, outer, s, by_symbol);
                    }
                }
            }
            HirStmt::Match {
                scrutinee,
                arms,
                else_body,
                ..
            } => {
                walk_expr(prog, typed, plan, outer, scrutinee, by_symbol);
                for arm in arms {
                    for s in &arm.body {
                        walk_stmt(prog, typed, plan, outer, s, by_symbol);
                    }
                }
                if let Some(body) = else_body {
                    for s in body {
                        walk_stmt(prog, typed, plan, outer, s, by_symbol);
                    }
                }
            }
            HirStmt::While {
                condition, body, ..
            } => {
                walk_expr(prog, typed, plan, outer, condition, by_symbol);
                for s in body {
                    walk_stmt(prog, typed, plan, outer, s, by_symbol);
                }
            }
            HirStmt::Return { value: None, .. }
            | HirStmt::Break { .. }
            | HirStmt::Continue { .. } => {}
        }
    }

    // Roots: non-generic functions and global initializers.
    for item in &prog.items {
        match item {
            HirItem::Fn {
                type_params, body, ..
            } if type_params.is_empty() => {
                for stmt in body {
                    walk_stmt(prog, typed, plan, None, stmt, &mut by_symbol);
                }
            }
            HirItem::Let { value, .. } => {
                walk_expr(prog, typed, plan, None, value, &mut by_symbol);
            }
            _ => {}
        }
    }
    // Emitted owner instances only (never uninstantiated templates).
    if let Some(owned) = plan.instances_by_owner.get(&prog.module) {
        // Template bodies by DefId for walking.
        let mut bodies: HashMap<u32, &[HirStmt]> = HashMap::new();
        for item in &prog.items {
            if let HirItem::Fn {
                def: Some(d), body, ..
            } = item
            {
                bodies.insert(d.0, body);
            }
        }
        let mut keys: Vec<&vl_typecheck::InstanceKey> = owned.keys().collect();
        keys.sort();
        for key in keys {
            let inst = &owned[key];
            let Some(body) = bodies.get(&inst.orig) else {
                continue;
            };
            for stmt in *body {
                walk_stmt(prog, typed, plan, Some(key), stmt, &mut by_symbol);
            }
        }
    }

    by_symbol.into_values().collect()
}

impl Lowerer<'_> {
    fn lower_expr(&mut self, expr: &HirExpr, typed: &vl_typecheck::TypedProgram) -> Option<Reg> {
        // Poisoned nodes (and, defensively, types that stayed generic) lower
        // to nothing — the error was already reported.
        self.resolved_ty(expr.id())?;
        match expr {
            HirExpr::Literal { value, span, .. } => {
                let dst = self.reg();
                let ty = self.resolved_ty(expr.id())?;
                let value = match (value, ty) {
                    (Scalar::Int(v), Ty::U64) => Scalar::U64(*v as u64),
                    (Scalar::Int(v), Ty::I64) => Scalar::I64(*v),
                    (Scalar::Int(v), Ty::U8) => Scalar::U8(*v as u8),
                    (value, _) => *value,
                };
                self.instrs.push(Instr::Const {
                    dst,
                    value,
                    span: *span,
                });
                Some(dst)
            }
            HirExpr::String { value, span, .. } => {
                let dst = self.reg();
                self.instrs.push(Instr::StringConst {
                    dst,
                    value: value.clone(),
                    span: *span,
                });
                Some(dst)
            }
            HirExpr::Var {
                def: Some(def),
                span,
                ..
            } => {
                if let Some(reg) = self.bindings.get(&def.0) {
                    return Some(*reg);
                }
                // Module globals live in shared state, never in a local home.
                // (Poisoned forward globals already returned `None` above.)
                if let Some(gid) = self.globals.get(&def.0).copied() {
                    let dst = self.reg();
                    self.instrs.push(Instr::GlobalLoad {
                        dst,
                        global: gid,
                        span: *span,
                    });
                    return Some(dst);
                }
                None
            }
            HirExpr::Var { .. } => None,
            HirExpr::ArrayLiteral {
                id, elems, span, ..
            } => {
                let mut regs = Vec::with_capacity(elems.len());
                for elem in elems {
                    regs.push(self.lower_expr(elem, typed)?);
                }
                let elem = self.array_elem_of(*id)?;
                let dst = self.reg();
                self.instrs.push(Instr::ArrayLit {
                    dst,
                    elems: regs,
                    elem,
                    span: *span,
                });
                Some(dst)
            }
            HirExpr::ObjectLiteral {
                name, fields, span, ..
            } => {
                let mut regs = Vec::with_capacity(fields.len());
                for (_, value) in fields {
                    regs.push(self.lower_expr(value, typed)?);
                }
                let dst = self.reg();
                self.instrs.push(Instr::NewObject {
                    dst,
                    name: name.clone(),
                    fields: fields
                        .iter()
                        .zip(regs)
                        .map(|((field, _), reg)| (field.clone(), reg))
                        .collect(),
                    span: *span,
                });
                Some(dst)
            }
            HirExpr::Variant {
                id,
                union,
                variant,
                args,
                span,
                ..
            } => {
                let mut regs = Vec::with_capacity(args.len());
                for arg in args {
                    regs.push(self.lower_expr(arg, typed)?);
                }
                // The node's recorded type carries the (instance-substituted)
                // union arguments; capabilities are already erased here.
                let union_ty = match self.resolved_ty(*id)? {
                    Ty::Union(u) => (*u).clone(),
                    Ty::Mutable(inner) => match *inner {
                        Ty::Union(u) => (*u).clone(),
                        _ => return None,
                    },
                    _ => return None,
                };
                let (tag, tys) = self.variant_layout(union, variant, &union_ty.args)?;
                if regs.len() != tys.len() {
                    return None;
                }
                let dst = self.reg();
                self.instrs.push(Instr::NewVariant {
                    dst,
                    union: union_ty.name,
                    variant: variant.clone(),
                    tag,
                    args: regs,
                    tys,
                    span: *span,
                });
                Some(dst)
            }
            HirExpr::Index {
                base, index, span, ..
            } => {
                // The element type comes from the array operand (the `Index`
                // node's own type *is* the element).
                let elem = self.array_elem_of(base.id());
                let array = self.lower_expr(base, typed)?;
                let index = self.lower_expr(index, typed)?;
                let elem = elem?;
                let dst = self.reg();
                self.instrs.push(Instr::ArrayGet {
                    dst,
                    array,
                    index,
                    elem,
                    span: *span,
                });
                Some(dst)
            }
            HirExpr::TupleLiteral { id, elems, span } => {
                let mut regs = Vec::with_capacity(elems.len());
                for (_, elem) in elems {
                    regs.push(self.lower_expr(elem, typed)?);
                }
                let tys = self.tuple_tys_of(*id)?;
                let dst = self.reg();
                self.instrs.push(Instr::TupleLit {
                    dst,
                    elems: regs,
                    tys,
                    span: *span,
                });
                Some(dst)
            }
            HirExpr::TupleIndex {
                base, index, span, ..
            } => {
                let tys = self.tuple_tys_of(base.id())?;
                let tuple = self.lower_expr(base, typed)?;
                let dst = self.reg();
                self.instrs.push(Instr::TupleGet {
                    dst,
                    tuple,
                    index: *index,
                    tys,
                    span: *span,
                });
                Some(dst)
            }
            HirExpr::Field {
                base, name, span, ..
            } => {
                // Named-tuple field reads lower to positional `TupleGet`
                // (names are erased after typecheck); objects use `ObjectGet`.
                if let Some(tys) = self.tuple_tys_of(base.id()) {
                    let base_ty = self.resolved_ty(base.id())?;
                    let elems = base_ty.tuple_elems()?;
                    let index = elems
                        .iter()
                        .position(|(n, _)| n.as_deref() == Some(name.as_str()))?;
                    let tuple = self.lower_expr(base, typed)?;
                    let dst = self.reg();
                    self.instrs.push(Instr::TupleGet {
                        dst,
                        tuple,
                        index,
                        tys,
                        span: *span,
                    });
                    return Some(dst);
                }
                let object = self.lower_expr(base, typed)?;
                let ty = self.resolved_ty(expr.id()).map(|t| rt(&t))?;
                let dst = self.reg();
                self.instrs.push(Instr::ObjectGet {
                    dst,
                    object,
                    name: name.clone(),
                    ty,
                    span: *span,
                });
                Some(dst)
            }
            HirExpr::Call {
                id,
                name,
                symbol,
                extern_sig,
                args,
                span,
                ..
            } => {
                // The `Array.new::[T](len)` builtin desugars to an
                // allocation: arity and argument types were enforced by
                // `vl-typecheck`. The element type rides along so ref-element
                // arrays allocate ref slots.
                if name == "Array.new" {
                    if args.len() != 1 {
                        return None;
                    }
                    let len = self.lower_expr(&args[0], typed)?;
                    let elem = self.array_elem_of(*id)?;
                    let dst = self.reg();
                    self.instrs.push(Instr::NewArray {
                        dst,
                        len,
                        elem,
                        span: *span,
                    });
                    return Some(dst);
                }
                // Concrete targets first: the world plan (project builds)
                // maps every generic call to its owner-module mangled symbol.
                // This fixes the ordering where an imported `symbol` would
                // override a mangled generic call target.
                if let Some(plan) = self.plan {
                    let target = match &self.outer_key {
                        Some(outer) => plan.nested_targets.get(&(outer.clone(), id.0)).cloned(),
                        None => plan
                            .root_targets
                            .get(&(self.module.to_owned(), id.0))
                            .cloned(),
                    };
                    if let Some(target) = target {
                        let callee = FunctionRef {
                            module: target.key.template.module.clone(),
                            function: target.key.mangled(),
                        };
                        let mut arg_regs = Vec::with_capacity(args.len());
                        for arg in args {
                            arg_regs.push(self.lower_expr(arg, typed)?);
                        }
                        let dst = self.reg();
                        self.instrs.push(Instr::Call {
                            dst,
                            callee,
                            args: arg_regs,
                            span: *span,
                        });
                        return Some(dst);
                    }
                }
                // Monomorphized callees: root code consults `root_calls`,
                // instance bodies consult `inst_calls` for their own outer
                // instance. Mapped names take precedence over the HIR symbol;
                // unmapped imported names use their concrete symbol.
                let mapped = match &self.outer {
                    Some(outer) => self.typed.inst_calls.get(&(outer.clone(), id.0)).cloned(),
                    None => self.typed.root_calls.get(&id.0).cloned(),
                };
                // Imported generic roots handled by the world plan also land
                // in `imported_root_calls`; resolve them here for single-module
                // lowering that carries a plan-less `typed` (defensive: the
                // project path above already returned).
                let mapped = mapped.or_else(|| {
                    self.typed
                        .imported_root_calls
                        .get(&id.0)
                        .map(|key| key.mangled())
                });
                // `imported_inst_calls` covers local-generic -> imported-generic
                // forwarding discovered locally (world covers the rest).
                let mapped = mapped.or_else(|| match &self.outer {
                    Some(outer) => self
                        .typed
                        .imported_inst_calls
                        .get(&(outer.clone(), id.0))
                        .map(|key| key.mangled()),
                    None => None,
                });
                // Defensive: a source-generic call without a concrete plan or
                // local mapping must never reach the backend as an unmangled
                // generic symbol. Skip it (typechecking already reported or
                // the world blocked lowering).
                if mapped.is_none() {
                    if let Some(sig) = extern_sig {
                        if !sig.type_params.is_empty() {
                            return None;
                        }
                    }
                }
                let callee = match mapped {
                    Some(function) => {
                        // Local mangled target: it lives in the current module
                        // unless the mapping came from an imported key (which
                        // carries its owner). For old local-only maps the owner
                        // is the caller; for imported keys resolve the owner.
                        if let Some(key) = self.typed.imported_root_calls.get(&id.0).or_else(|| {
                            match &self.outer {
                                Some(outer) => {
                                    self.typed.imported_inst_calls.get(&(outer.clone(), id.0))
                                }
                                None => None,
                            }
                        }) {
                            FunctionRef {
                                module: key.template.module.clone(),
                                function: key.mangled(),
                            }
                        } else {
                            FunctionRef {
                                module: self.typed_module(),
                                function,
                            }
                        }
                    }
                    None => symbol
                        .as_ref()
                        .map(|s| FunctionRef {
                            module: s.module.as_string(),
                            function: s.name.clone(),
                        })
                        .unwrap_or_else(|| FunctionRef {
                            module: self.typed_module(),
                            function: name.clone(),
                        }),
                };
                let mut arg_regs = Vec::with_capacity(args.len());
                for arg in args {
                    arg_regs.push(self.lower_expr(arg, typed)?);
                }
                let dst = self.reg();
                self.instrs.push(Instr::Call {
                    dst,
                    callee,
                    args: arg_regs,
                    span: *span,
                });
                Some(dst)
            }
            HirExpr::MethodCall {
                id,
                receiver,
                args,
                span,
                ..
            } => {
                // Instance sugar lowers exactly like the explicit
                // `Owner.method(receiver, args...)` call: concrete targets
                // first (world plan), then monomorphized names, then the
                // recorded direct target. Only non-generic sugar records a
                // direct target, so a missing mapping here means poison
                // (already reported) — never an unmangled generic template.
                if let Some(plan) = self.plan {
                    let target = match &self.outer_key {
                        Some(outer) => plan.nested_targets.get(&(outer.clone(), id.0)).cloned(),
                        None => plan
                            .root_targets
                            .get(&(self.module.to_owned(), id.0))
                            .cloned(),
                    };
                    if let Some(target) = target {
                        let callee = FunctionRef {
                            module: target.key.template.module.clone(),
                            function: target.key.mangled(),
                        };
                        let mut arg_regs = Vec::with_capacity(args.len() + 1);
                        arg_regs.push(self.lower_expr(receiver, typed)?);
                        for arg in args {
                            arg_regs.push(self.lower_expr(arg, typed)?);
                        }
                        let dst = self.reg();
                        self.instrs.push(Instr::Call {
                            dst,
                            callee,
                            args: arg_regs,
                            span: *span,
                        });
                        return Some(dst);
                    }
                }
                let mapped = match &self.outer {
                    Some(outer) => self.typed.inst_calls.get(&(outer.clone(), id.0)).cloned(),
                    None => self.typed.root_calls.get(&id.0).cloned(),
                };
                let mapped = mapped.or_else(|| {
                    self.typed
                        .imported_root_calls
                        .get(&id.0)
                        .map(|key| key.mangled())
                });
                let mapped = mapped.or_else(|| match &self.outer {
                    Some(outer) => self
                        .typed
                        .imported_inst_calls
                        .get(&(outer.clone(), id.0))
                        .map(|key| key.mangled()),
                    None => None,
                });
                let callee = match mapped {
                    Some(function) => {
                        if let Some(key) = self.typed.imported_root_calls.get(&id.0).or_else(|| {
                            match &self.outer {
                                Some(outer) => {
                                    self.typed.imported_inst_calls.get(&(outer.clone(), id.0))
                                }
                                None => None,
                            }
                        }) {
                            FunctionRef {
                                module: key.template.module.clone(),
                                function: key.mangled(),
                            }
                        } else {
                            FunctionRef {
                                module: self.typed_module(),
                                function,
                            }
                        }
                    }
                    None => match self.typed.method_targets.get(&id.0) {
                        Some(vl_typecheck::MethodTarget::Local(name)) => FunctionRef {
                            module: self.typed_module(),
                            function: name.clone(),
                        },
                        Some(vl_typecheck::MethodTarget::Foreign(sym)) => FunctionRef {
                            module: sym.module.as_string(),
                            function: sym.name.clone(),
                        },
                        None => return None,
                    },
                };
                let mut arg_regs = Vec::with_capacity(args.len() + 1);
                arg_regs.push(self.lower_expr(receiver, typed)?);
                for arg in args {
                    arg_regs.push(self.lower_expr(arg, typed)?);
                }
                let dst = self.reg();
                self.instrs.push(Instr::Call {
                    dst,
                    callee,
                    args: arg_regs,
                    span: *span,
                });
                Some(dst)
            }
            HirExpr::Binary {
                op, lhs, rhs, span, ..
            } => match op {
                HirBinOp::And => self.lower_and(lhs, rhs, typed, *span),
                HirBinOp::Or => self.lower_or(lhs, rhs, typed, *span),
                _ => {
                    let l = self.lower_expr(lhs, typed)?;
                    let r = self.lower_expr(rhs, typed)?;
                    let dst = self.reg();
                    let op = match op {
                        HirBinOp::Add => LirOp::Add,
                        HirBinOp::Sub => LirOp::Sub,
                        HirBinOp::Mul => LirOp::Mul,
                        HirBinOp::Div => LirOp::Div,
                        HirBinOp::Eq => LirOp::Eq,
                        HirBinOp::Ne => LirOp::Ne,
                        HirBinOp::Lt => LirOp::Lt,
                        HirBinOp::Le => LirOp::Le,
                        HirBinOp::Gt => LirOp::Gt,
                        HirBinOp::Ge => LirOp::Ge,
                        HirBinOp::And | HirBinOp::Or => unreachable!("handled above"),
                    };
                    self.instrs.push(Instr::BinOp {
                        dst,
                        op,
                        lhs: l,
                        rhs: r,
                        span: *span,
                    });
                    Some(dst)
                }
            },
            HirExpr::Unary {
                op, inner, span, ..
            } => match op {
                HirUnOp::Not => {
                    let src = self.lower_expr(inner, typed)?;
                    let dst = self.reg();
                    self.instrs.push(Instr::Not {
                        dst,
                        src,
                        span: *span,
                    });
                    Some(dst)
                }
            },
            HirExpr::Cast { inner, span, .. } => {
                // Explicit conversion: the inner value was range-checked by
                // typechecking (literals) or is an unchecked integer
                // reinterpretation (variables). Emit a cast so backends set
                // the target register class.
                let src = self.lower_expr(inner, typed)?;
                let target = self.resolved_ty(expr.id()).map(|t| rt(&t))?;
                let dst = self.reg();
                self.instrs.push(Instr::Cast {
                    dst,
                    src,
                    target,
                    span: *span,
                });
                Some(dst)
            }
        }
    }

    fn lower_stmt(&mut self, stmt: &HirStmt, typed: &vl_typecheck::TypedProgram) {
        match stmt {
            HirStmt::Let { def, value, .. } => {
                if let Some(reg) = self.lower_expr(value, typed) {
                    bind_local(self, def.as_ref(), value, reg);
                }
            }
            HirStmt::Expr(value) => {
                let _ = self.lower_expr(value, typed);
            }
            HirStmt::Return { value, span } => {
                self.lower_return(value.as_ref(), typed, *span);
            }
            HirStmt::If {
                condition,
                then_body,
                else_body,
                span,
            } => {
                self.lower_if(condition, then_body, else_body.as_deref(), typed, *span);
            }
            HirStmt::Match {
                scrutinee,
                arms,
                else_body,
                span,
            } => {
                self.lower_match(scrutinee, arms, else_body.as_deref(), typed, *span);
            }
            HirStmt::Assign {
                def, value, span, ..
            } => {
                self.lower_assign(def.as_ref(), value, typed, *span);
            }
            HirStmt::IndexAssign {
                array,
                index,
                value,
                span,
                ..
            } => {
                self.lower_index_assign(array, index, value, typed, *span);
            }
            HirStmt::FieldAssign {
                base,
                field,
                value,
                span,
                ..
            } => {
                self.lower_field_assign(base, field, value, typed, *span);
            }
            HirStmt::TupleAssign {
                base,
                index,
                value,
                span,
                ..
            } => {
                self.lower_tuple_assign(base, *index, value, typed, *span);
            }
            HirStmt::Destructure {
                bindings,
                value,
                span,
                ..
            } => {
                self.lower_destructure(bindings, value, typed, *span);
            }
            HirStmt::While {
                condition,
                body,
                span,
            } => {
                self.lower_while(condition, body, typed, *span);
            }
            HirStmt::Break { span } => {
                self.lower_break(*span);
            }
            HirStmt::Continue { span } => {
                self.lower_continue(*span);
            }
        }
    }

    /// Explicit `return`: `return expr;` moves the value into `Ret`;
    /// bare `return;` (for `void`) returns a dummy zero — backends ignore
    /// the payload for `void`/`main`. Poisoned values emit nothing (the
    /// error was already reported; the fallthrough default keeps LIR
    /// well-formed).
    fn lower_return(
        &mut self,
        value: Option<&HirExpr>,
        typed: &vl_typecheck::TypedProgram,
        span: Span,
    ) {
        match value {
            Some(e) => {
                if let Some(r) = self.lower_expr(e, typed) {
                    self.instrs.push(Instr::Ret { src: r, span });
                }
            }
            None => {
                let r = self.reg();
                self.instrs.push(Instr::Const {
                    dst: r,
                    value: Scalar::I64(0),
                    span,
                });
                self.instrs.push(Instr::Ret { src: r, span });
            }
        }
    }

    /// Assignment writes in place: evaluate the RHS then `Copy` it into the
    /// already-bound register (locals) or `GlobalStore` it (globals). The
    /// bindings map is unchanged, so branches and loops that assign keep
    /// working without phi nodes; branch-local bindings are still pruned by
    /// the caller restoring the incoming map.
    fn lower_assign(
        &mut self,
        def: Option<&vl_hir::DefId>,
        value: &HirExpr,
        typed: &vl_typecheck::TypedProgram,
        span: Span,
    ) {
        let Some(src) = self.lower_expr(value, typed) else {
            return;
        };
        let Some(def) = def else {
            return;
        };
        if let Some(dst) = self.bindings.get(&def.0).copied() {
            self.instrs.push(Instr::Copy { dst, src, span });
            return;
        }
        if let Some(gid) = self.globals.get(&def.0).copied() {
            self.instrs.push(Instr::GlobalStore {
                global: gid,
                src,
                span,
            });
        }
    }

    /// Element write: evaluate the array, index, and value, then emit one
    /// [`Instr::ArraySet`]. Poisoned sides emit nothing (already reported).
    /// The element type rides along so ref-element arrays pick ref stores.
    fn lower_index_assign(
        &mut self,
        array: &HirExpr,
        index: &HirExpr,
        value: &HirExpr,
        typed: &vl_typecheck::TypedProgram,
        span: Span,
    ) {
        // Element type comes from the array operand's recorded type (before
        // lowering shadows the name with registers).
        let elem = self.array_elem_of(array.id());
        let (Some(array), Some(index), Some(value)) = (
            self.lower_expr(array, typed),
            self.lower_expr(index, typed),
            self.lower_expr(value, typed),
        ) else {
            return;
        };
        let Some(elem) = elem else {
            return;
        };
        self.instrs.push(Instr::ArraySet {
            array,
            index,
            value,
            elem,
            span,
        });
    }

    fn lower_field_assign(
        &mut self,
        base: &HirExpr,
        field: &str,
        value: &HirExpr,
        typed: &vl_typecheck::TypedProgram,
        span: Span,
    ) {
        // Named-tuple field writes lower to positional `TupleSet`.
        if let Some(tys) = self.tuple_tys_of(base.id()) {
            let base_ty = self.resolved_ty(base.id());
            let index = base_ty
                .as_ref()
                .and_then(|t| t.tuple_elems())
                .and_then(|elems| elems.iter().position(|(n, _)| n.as_deref() == Some(field)));
            let (Some(object), Some(value), Some(index)) = (
                self.lower_expr(base, typed),
                self.lower_expr(value, typed),
                index,
            ) else {
                return;
            };
            self.instrs.push(Instr::TupleSet {
                tuple: object,
                index,
                value,
                tys,
                span,
            });
            // Global bases load by value: store the mutated container back.
            if let Some(gid) = self.global_base_of(base) {
                self.instrs.push(Instr::GlobalStore {
                    global: gid,
                    src: object,
                    span,
                });
            }
            return;
        }
        let value_id = value.id();
        let (Some(object), Some(value)) =
            (self.lower_expr(base, typed), self.lower_expr(value, typed))
        else {
            return;
        };
        let Some(ty) = self.resolved_ty(value_id).map(|t| rt(&t)) else {
            return;
        };
        self.instrs.push(Instr::ObjectSet {
            object,
            name: field.to_owned(),
            value,
            ty,
            span,
        });
    }

    /// Stable global ID behind a base expression, if it is a plain global
    /// variable. Tuple element writes through such a base must store the
    /// mutated container back: globals load by value (a copy), so a bare
    /// `TupleSet` on the loaded copy would vanish.
    fn global_base_of(&self, base: &HirExpr) -> Option<u32> {
        if let HirExpr::Var { def: Some(def), .. } = base {
            return self.globals.get(&def.0).copied();
        }
        None
    }

    /// Unnamed tuple element write: evaluate the base and value, then emit
    /// one [`Instr::TupleSet`]. The full erased element list rides along so
    /// the backend picks the value/ref slot for the position.
    fn lower_tuple_assign(
        &mut self,
        base: &HirExpr,
        index: usize,
        value: &HirExpr,
        typed: &vl_typecheck::TypedProgram,
        span: Span,
    ) {
        let tys = self.tuple_tys_of(base.id());
        let (Some(tuple), Some(value)) =
            (self.lower_expr(base, typed), self.lower_expr(value, typed))
        else {
            return;
        };
        let Some(tys) = tys else {
            return;
        };
        self.instrs.push(Instr::TupleSet {
            tuple,
            index,
            value,
            tys,
            span,
        });
        // Global bases load by value: store the mutated container back.
        if let Some(gid) = self.global_base_of(base) {
            self.instrs.push(Instr::GlobalStore {
                global: gid,
                src: tuple,
                span,
            });
        }
    }

    /// Destructure lowering: evaluate the base once, then one `TupleGet`
    /// per binding plus `bind_local` (same copy semantics as other locals).
    fn lower_destructure(
        &mut self,
        bindings: &[vl_hir::HirDestructureBinding],
        value: &HirExpr,
        typed: &vl_typecheck::TypedProgram,
        span: Span,
    ) {
        let base_ty = self.resolved_ty(value.id());
        let Some(tuple_reg) = self.lower_expr(value, typed) else {
            return;
        };
        let (Some(base_ty), Some(tys)) = (base_ty, self.tuple_tys_of(value.id())) else {
            return;
        };
        let Some(elems) = base_ty.tuple_elems() else {
            return;
        };
        for b in bindings {
            let pos = if elems.iter().all(|(n, _)| n.is_none()) {
                b.index
            } else if let Some(field) = &b.field {
                match elems
                    .iter()
                    .position(|(n, _)| n.as_deref() == Some(field.as_str()))
                {
                    Some(pos) => pos,
                    None => continue,
                }
            } else {
                match elems
                    .iter()
                    .position(|(n, _)| n.as_deref() == Some(b.binding.as_str()))
                {
                    Some(pos) => pos,
                    None => continue,
                }
            };
            if pos >= tys.len() {
                continue;
            }
            let dst = self.reg();
            self.instrs.push(Instr::TupleGet {
                dst,
                tuple: tuple_reg,
                index: pos,
                tys: tys.clone(),
                span,
            });
            // Bind the extracted element like any other local (globals use
            // `GlobalStore` via the same map when the def is top-level).
            if let Some(def) = &b.def {
                if let Some(gid) = self.globals.get(&def.0).copied() {
                    self.instrs.push(Instr::GlobalStore {
                        global: gid,
                        src: dst,
                        span,
                    });
                } else {
                    let home = self.reg();
                    self.instrs.push(Instr::Copy {
                        dst: home,
                        src: dst,
                        span,
                    });
                    self.bindings.insert(def.0, home);
                }
            }
        }
    }

    /// Short-circuit `&&`: sides evaluate at most once, left to right.
    fn lower_and(
        &mut self,
        lhs: &HirExpr,
        rhs: &HirExpr,
        typed: &vl_typecheck::TypedProgram,
        span: Span,
    ) -> Option<Reg> {
        let l = self.lower_expr(lhs, typed)?;
        let dst = self.reg();
        let false_label = self.label();
        let end_label = self.label();
        self.instrs.push(Instr::BranchIfFalse {
            cond: l,
            target: false_label,
            span,
        });
        let Some(r) = self.lower_expr(rhs, typed) else {
            self.instrs.push(Instr::Label {
                id: false_label,
                span,
            });
            self.instrs.push(Instr::Label {
                id: end_label,
                span,
            });
            return None;
        };
        self.instrs.push(Instr::BranchIfFalse {
            cond: r,
            target: false_label,
            span,
        });
        let true_tmp = self.reg();
        self.instrs.push(Instr::Const {
            dst: true_tmp,
            value: Scalar::Bool(true),
            span,
        });
        self.instrs.push(Instr::Copy {
            dst,
            src: true_tmp,
            span,
        });
        self.instrs.push(Instr::Jump {
            target: end_label,
            span,
        });
        self.instrs.push(Instr::Label {
            id: false_label,
            span,
        });
        let false_tmp = self.reg();
        self.instrs.push(Instr::Const {
            dst: false_tmp,
            value: Scalar::Bool(false),
            span,
        });
        self.instrs.push(Instr::Copy {
            dst,
            src: false_tmp,
            span,
        });
        self.instrs.push(Instr::Label {
            id: end_label,
            span,
        });
        Some(dst)
    }

    /// Short-circuit `||`: sides evaluate at most once, left to right.
    fn lower_or(
        &mut self,
        lhs: &HirExpr,
        rhs: &HirExpr,
        typed: &vl_typecheck::TypedProgram,
        span: Span,
    ) -> Option<Reg> {
        let l = self.lower_expr(lhs, typed)?;
        let dst = self.reg();
        let rhs_label = self.label();
        let false_label = self.label();
        let end_label = self.label();
        self.instrs.push(Instr::BranchIfFalse {
            cond: l,
            target: rhs_label,
            span,
        });
        let lhs_true_tmp = self.reg();
        self.instrs.push(Instr::Const {
            dst: lhs_true_tmp,
            value: Scalar::Bool(true),
            span,
        });
        self.instrs.push(Instr::Copy {
            dst,
            src: lhs_true_tmp,
            span,
        });
        self.instrs.push(Instr::Jump {
            target: end_label,
            span,
        });
        self.instrs.push(Instr::Label {
            id: rhs_label,
            span,
        });
        let Some(r) = self.lower_expr(rhs, typed) else {
            self.instrs.push(Instr::Label {
                id: false_label,
                span,
            });
            self.instrs.push(Instr::Label {
                id: end_label,
                span,
            });
            return None;
        };
        self.instrs.push(Instr::BranchIfFalse {
            cond: r,
            target: false_label,
            span,
        });
        let true_tmp = self.reg();
        self.instrs.push(Instr::Const {
            dst: true_tmp,
            value: Scalar::Bool(true),
            span,
        });
        self.instrs.push(Instr::Copy {
            dst,
            src: true_tmp,
            span,
        });
        self.instrs.push(Instr::Jump {
            target: end_label,
            span,
        });
        self.instrs.push(Instr::Label {
            id: false_label,
            span,
        });
        let false_tmp = self.reg();
        self.instrs.push(Instr::Const {
            dst: false_tmp,
            value: Scalar::Bool(false),
            span,
        });
        self.instrs.push(Instr::Copy {
            dst,
            src: false_tmp,
            span,
        });
        self.instrs.push(Instr::Label {
            id: end_label,
            span,
        });
        Some(dst)
    }

    fn lower_while(
        &mut self,
        condition: &HirExpr,
        body: &[HirStmt],
        typed: &vl_typecheck::TypedProgram,
        span: Span,
    ) {
        let start_label = self.label();
        let end_label = self.label();
        self.loop_stack.push(LoopTargets {
            break_target: end_label,
            continue_target: start_label,
        });
        let incoming = self.bindings.clone();
        self.instrs.push(Instr::Label {
            id: start_label,
            span,
        });
        let Some(cond) = self.lower_expr(condition, typed) else {
            self.instrs.push(Instr::Label {
                id: end_label,
                span,
            });
            self.loop_stack.pop();
            return;
        };
        self.instrs.push(Instr::BranchIfFalse {
            cond,
            target: end_label,
            span,
        });
        for stmt in body {
            self.lower_stmt(stmt, typed);
        }
        self.instrs.push(Instr::Jump {
            target: start_label,
            span,
        });
        self.instrs.push(Instr::Label {
            id: end_label,
            span,
        });
        // Drop loop-local bindings; in-place `Copy` writes to shared registers
        // stay visible, so assignments inside the loop persist.
        self.bindings.retain(|k, _| incoming.contains_key(k));
        self.loop_stack.pop();
    }

    fn lower_break(&mut self, span: Span) {
        // Outside a loop the resolver already reported E204; stay quiet.
        if let Some(targets) = self.loop_stack.last() {
            self.instrs.push(Instr::Jump {
                target: targets.break_target,
                span,
            });
        }
    }

    fn lower_continue(&mut self, span: Span) {
        if let Some(targets) = self.loop_stack.last() {
            self.instrs.push(Instr::Jump {
                target: targets.continue_target,
                span,
            });
        }
    }

    fn lower_if(
        &mut self,
        condition: &HirExpr,
        then_body: &[HirStmt],
        else_body: Option<&[HirStmt]>,
        typed: &vl_typecheck::TypedProgram,
        span: Span,
    ) {
        let Some(cond) = self.lower_expr(condition, typed) else {
            return;
        };
        let incoming = self.bindings.clone();
        let else_label = self.label();
        let end_label = self.label();
        self.instrs.push(Instr::BranchIfFalse {
            cond,
            target: else_label,
            span,
        });
        for stmt in then_body {
            self.lower_stmt(stmt, typed);
        }
        self.instrs.push(Instr::Jump {
            target: end_label,
            span,
        });
        self.instrs.push(Instr::Label {
            id: else_label,
            span,
        });
        self.bindings = incoming.clone();
        if let Some(body) = else_body {
            for stmt in body {
                self.lower_stmt(stmt, typed);
            }
        }
        self.bindings = incoming;
        self.instrs.push(Instr::Label {
            id: end_label,
            span,
        });
    }

    /// Declared tag + erased payload types for `union.variant` under concrete
    /// union arguments. `None` when unknown (typechecking already reported;
    /// lowering stands down).
    fn variant_layout(&self, union: &str, variant: &str, args: &[Ty]) -> Option<(u32, Vec<Ty>)> {
        let sig = self.typed.unions.get(union)?;
        let (tag, payload) = sig
            .variants
            .iter()
            .enumerate()
            .find(|(_, v)| v.name == *variant)
            .map(|(i, v)| (i as u32, &v.payload))?;
        if args.len() != sig.type_params.len() {
            return None;
        }
        let env: HashMap<String, Ty> = sig
            .type_params
            .iter()
            .cloned()
            .zip(args.iter().cloned())
            .collect();
        Some((
            tag,
            payload.iter().map(|t| rt(&subst_ty(t, &env))).collect(),
        ))
    }

    /// Lower `match`: read the discriminant once, then chain one tag
    /// comparison per arm (typechecking validated coverage and payload
    /// arity, so every arm binds exactly its variant's payloads).
    fn lower_match(
        &mut self,
        scrutinee: &HirExpr,
        arms: &[vl_hir::HirMatchArm],
        else_body: Option<&[HirStmt]>,
        typed: &vl_typecheck::TypedProgram,
        span: Span,
    ) {
        let Some(scrut) = self.lower_expr(scrutinee, typed) else {
            return;
        };
        // Resolve the union identity and every arm's layout before emitting:
        // anything unknown was already reported upstream.
        let union_ty = match self.resolved_ty(scrutinee.id()) {
            Some(Ty::Union(u)) => (*u).clone(),
            Some(Ty::Mutable(inner)) => match *inner {
                Ty::Union(u) => (*u).clone(),
                _ => return,
            },
            _ => return,
        };
        let mut layouts = Vec::with_capacity(arms.len());
        for arm in arms {
            let Some(layout) = self.variant_layout(&union_ty.name, &arm.variant, &union_ty.args)
            else {
                return;
            };
            layouts.push(layout);
        }
        let incoming = self.bindings.clone();
        let end_label = self.label();
        let tag_reg = self.reg();
        self.instrs.push(Instr::TagOf {
            dst: tag_reg,
            scrut,
            span,
        });
        let mut next_labels = Vec::with_capacity(arms.len());
        for _ in arms {
            next_labels.push(self.label());
        }
        for ((arm, (tag, tys)), next) in arms.iter().zip(layouts.iter()).zip(next_labels.iter()) {
            // `tag == <arm tag>`, else fall to the next arm.
            let tag_const = self.reg();
            self.instrs.push(Instr::Const {
                dst: tag_const,
                value: Scalar::U64(u64::from(*tag)),
                span,
            });
            let cond = self.reg();
            self.instrs.push(Instr::BinOp {
                dst: cond,
                op: LirOp::Eq,
                lhs: tag_reg,
                rhs: tag_const,
                span,
            });
            self.instrs.push(Instr::BranchIfFalse {
                cond,
                target: *next,
                span,
            });
            // Bind this variant's payloads, then run the arm.
            self.bindings = incoming.clone();
            for (i, b) in arm.bindings.iter().enumerate() {
                if i >= tys.len() {
                    break;
                }
                let dst = self.reg();
                self.instrs.push(Instr::PayloadGet {
                    dst,
                    scrut,
                    index: i,
                    tys: tys.clone(),
                    span,
                });
                if let Some(def) = &b.def {
                    if let Some(gid) = self.globals.get(&def.0).copied() {
                        self.instrs.push(Instr::GlobalStore {
                            global: gid,
                            src: dst,
                            span,
                        });
                    } else {
                        let home = self.reg();
                        self.instrs.push(Instr::Copy {
                            dst: home,
                            src: dst,
                            span,
                        });
                        self.bindings.insert(def.0, home);
                    }
                }
            }
            for stmt in &arm.body {
                self.lower_stmt(stmt, typed);
            }
            self.instrs.push(Instr::Jump {
                target: end_label,
                span,
            });
            self.instrs.push(Instr::Label { id: *next, span });
        }
        self.bindings = incoming.clone();
        if let Some(body) = else_body {
            for stmt in body {
                self.lower_stmt(stmt, typed);
            }
        }
        self.bindings = incoming;
        self.instrs.push(Instr::Label {
            id: end_label,
            span,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrays_lower_to_dedicated_instrs() {
        let src = "fun get(a: *Array[u64]): u64 { a[0u64] = 1u64; return a[1u64]; } fun main() { val a: *Array[u64] = Array.new::[u64](2u64); val b = [1u64, 2u64]; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        let (typed, diags) = vl_typecheck::check(&hir);
        assert!(diags.is_empty(), "{diags:?}");
        let dump = lower(&hir, &typed).dump();
        assert!(dump.contains("new_array"), "{dump}");
        assert!(dump.contains("array_lit"), "{dump}");
        assert!(dump.contains("array_get"), "{dump}");
        assert!(dump.contains("array_set"), "{dump}");
        assert!(!dump.contains("Array.new"), "{dump}");
    }

    #[test]
    fn objects_lower_to_dedicated_instrs() {
        let src = "type Counter = object { value: u64, }; fun main() { val c: *Counter = Counter { value = 1u64 }; c.value = c.value + 1u64; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        let (typed, diags) = vl_typecheck::check(&hir);
        assert!(diags.is_empty(), "{diags:?}");
        let dump = lower(&hir, &typed).dump();
        assert!(dump.contains("new_object Counter"), "{dump}");
        assert!(dump.contains("object_get"), "{dump}");
        assert!(dump.contains("object_set"), "{dump}");
    }

    #[test]
    fn union_declarations_emit_no_layouts_and_values_lower() {
        let src = "type U = union { A, };";
        let (toks, pdiags) = vl_lex::lex(src);
        let (prog, sdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        assert!(sdiags.is_empty(), "{sdiags:?}");
        let (res, rdiags) = vl_semantic::resolve(&prog);
        assert!(rdiags.is_empty(), "{rdiags:?}");
        let hir = vl_hir::lower(&prog, &res);
        let (typed, diags) = vl_typecheck::check(&hir);
        assert!(diags.is_empty(), "{diags:?}");
        let lir = lower(&hir, &typed);
        assert!(lir.objects.is_empty());
        assert!(lir.functions.is_empty());
        assert!(lir.dump().is_empty(), "{}", lir.dump());

        // Object-literal construction of a union stays an error and lowers
        // to no object allocation.
        let src = "type U = union { A, }; fun main() { val u = U {}; u; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        let (typed, diags) = vl_typecheck::check(&hir);
        assert_eq!(
            diags.iter().filter(|d| d.is_error()).count(),
            1,
            "{diags:?}"
        );
        let lir = lower(&hir, &typed);
        assert!(!lir.dump().contains("new_object U"), "{}", lir.dump());
    }

    #[test]
    fn variants_and_match_lower_to_dedicated_instrs() {
        let src = "type U = union { A, B(u64), }; fun main() { val u = U.B(1u64); match (u) { U.B(v) { v; } U.A { 0u64; } else { 2u64; } } }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, rdiags) = vl_semantic::resolve(&prog);
        assert!(rdiags.iter().all(|d| !d.is_error()), "{rdiags:?}");
        let hir = vl_hir::lower(&prog, &res);
        let (typed, diags) = vl_typecheck::check(&hir);
        assert!(diags.is_empty(), "{diags:?}");
        let dump = lower(&hir, &typed).dump();
        assert!(dump.contains("new_variant U.B#1"), "{dump}");
        assert!(dump.contains("tag_of"), "{dump}");
        assert!(dump.contains("payload_get"), "{dump}");
    }

    #[test]
    fn object_let_alias_gets_an_independent_rebinding_home() {
        let src = "type Counter = object { value: u64, }; fun main() { val a: *Counter = Counter { value = 1u64 }; var b = a; b = Counter { value = 2u64 }; a.value = 3u64; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        let (typed, diags) = vl_typecheck::check(&hir);
        assert!(diags.is_empty(), "{diags:?}");
        let dump = lower(&hir, &typed).dump();
        assert!(
            dump.contains("copy"),
            "object aliases need separate homes: {dump}"
        );
    }

    #[test]
    fn generic_templates_emit_only_instances() {
        let src = "fun id[T](x: T): T { return x; } fun main() { val a = id(1u64); a; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        let (typed, diags) = vl_typecheck::check(&hir);
        assert!(diags.is_empty(), "{diags:?}");
        let lir = lower(&hir, &typed);
        let names: Vec<&str> = lir.functions.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"id$u64"), "{names:?}");
        assert!(!names.contains(&"id"), "{names:?}");
        let dump = lir.dump();
        assert!(dump.contains("call id$u64"), "{dump}");
    }

    #[test]
    fn while_emits_labels_and_back_edge() {
        let src = "fun main() { var i = 0; while (i < 10) { i = i + 1; } }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        let (typed, diags) = vl_typecheck::check(&hir);
        assert!(diags.is_empty(), "{diags:?}");
        let dump = lower(&hir, &typed).dump();
        assert!(dump.contains("branch_if_false"), "{dump}");
        assert!(dump.contains("jump"), "{dump}");
        assert!(dump.contains("copy"), "{dump}");
        assert!(dump.contains("lt"), "{dump}");
    }

    #[test]
    fn logical_and_short_circuits_without_an_and_instr() {
        let src = "fun main() { val x = true && false; x; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        let (typed, diags) = vl_typecheck::check(&hir);
        assert!(diags.is_empty(), "{diags:?}");
        let dump = lower(&hir, &typed).dump();
        assert!(!dump.contains("= and"), "{dump}");
        assert!(dump.contains("branch_if_false"), "{dump}");
        assert!(!dump.contains("not"), "{dump}");
    }

    #[test]
    fn not_emits_a_not_instr() {
        let src = "fun main() { !true; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        let (typed, diags) = vl_typecheck::check(&hir);
        assert!(diags.is_empty(), "{diags:?}");
        let dump = lower(&hir, &typed).dump();
        assert!(dump.contains("not"), "{dump}");
    }

    #[test]
    fn lowers_add_chain() {
        let src = "val x = 1 + 2;";
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        let (typed, _) = vl_typecheck::check(&hir);
        let lir = lower(&hir, &typed);
        let dump = lir.dump();
        assert!(dump.contains("add"));
        assert!(dump.contains("global"), "{dump}");
    }

    #[test]
    fn lowers_call_and_parameter_registers() {
        let src = "fun add(a: i64, b: i64): i64 { return a + b; } fun main() { add(1, 2); }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        let (typed, diags) = vl_typecheck::check(&hir);
        assert!(diags.is_empty());
        let lir = lower(&hir, &typed);
        let dump = lir.dump();
        assert!(dump.contains("%0 = param 0"), "{dump}");
        assert!(dump.contains("%1 = param 1"), "{dump}");
        assert!(dump.contains("call add(%0, %1)"), "{dump}");
    }

    #[test]
    fn function_signatures_carry_param_and_return_types() {
        let src = r#"fun greet(name: String, n: u64): String { return name; } fun main() { greet("hi", 1u64); }"#;
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        let (typed, diags) = vl_typecheck::check(&hir);
        assert!(diags.is_empty(), "{diags:?}");
        let lir = lower(&hir, &typed);
        let greet = lir.functions.iter().find(|f| f.name == "greet").unwrap();
        assert_eq!(greet.param_tys, vec![Ty::String, Ty::U64]);
        assert_eq!(greet.ret, Ty::String);
        let main = lir.functions.iter().find(|f| f.name == "main").unwrap();
        assert!(main.param_tys.is_empty());
        assert_eq!(main.ret, Ty::Void);
    }

    #[test]
    fn tuples_lower_to_dedicated_instrs() {
        let src = "fun main() { val t = #(1u64, \"a\"); val a = t.`0; var m = #(1u64, 2u64); m.`0 = 3u64; val #(p, q) = t; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, rdiags) = vl_semantic::resolve(&prog);
        assert!(rdiags.iter().all(|d| !d.is_error()), "{rdiags:?}");
        let hir = vl_hir::lower(&prog, &res);
        let (typed, tdiags) = vl_typecheck::check(&hir);
        assert!(tdiags.is_empty(), "{tdiags:?}");
        let lir = lower(&hir, &typed);
        let dump = lir.dump();
        assert!(dump.contains("tuple_lit"), "{dump}");
        assert!(dump.contains("tuple_get"), "{dump}");
        assert!(dump.contains("tuple_set"), "{dump}");
    }

    #[test]
    fn tuple_writes_to_globals_store_back() {
        // Globals load by value, so `g.`0 = v;` must lower to
        // `tuple_set` + `global_store` — a bare `tuple_set` would mutate a
        // temporary copy and vanish.
        let src = "var g: *#(u64, u64) = #(1u64, 2u64); fun main() { g.`0 = 9u64; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        let (typed, diags) = vl_typecheck::check(&hir);
        assert!(diags.is_empty(), "{diags:?}");
        let lir = lower(&hir, &typed);
        let main = lir
            .functions
            .iter()
            .find(|f| f.name == "main")
            .expect("main");
        let has_set = main
            .instrs
            .iter()
            .any(|i| matches!(i, Instr::TupleSet { .. }));
        let has_store = main
            .instrs
            .iter()
            .any(|i| matches!(i, Instr::GlobalStore { .. }));
        assert!(has_set && has_store, "{}", lir.dump());
    }

    #[test]
    fn top_level_destructure_evaluates_base_once() {
        // The shared base must lower to exactly one call: a hidden base
        // global holds it and each binding loads from there.
        let src = "fun make(): #(u64, u64) { return #(7u64, 8u64); } val #(a, b) = make(); fun main() { a; b; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        let (typed, diags) = vl_typecheck::check(&hir);
        assert!(diags.is_empty(), "{diags:?}");
        let lir = lower(&hir, &typed);
        assert_eq!(lir.globals.len(), 3, "{}", lir.dump());
        let calls: usize = lir
            .globals
            .iter()
            .flat_map(|g| g.init.iter())
            .filter(|i| matches!(i, Instr::Call { .. }))
            .count();
        assert_eq!(calls, 1, "base must evaluate once: {}", lir.dump());
    }

    #[test]
    fn globals_record_their_value_type() {
        let src = r#"val x = 1u64;"#;
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        let (typed, diags) = vl_typecheck::check(&hir);
        assert!(diags.is_empty(), "{diags:?}");
        let lir = lower(&hir, &typed);
        assert_eq!(lir.functions.len(), 0);
        assert_eq!(lir.globals.len(), 1);
        assert_eq!(lir.globals[0].ty, Ty::U64);
    }

    #[test]
    fn local_reads_use_the_declared_value() {
        let src = "fun f(): u64 { val x = 7; return x + 1; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        let (typed, diags) = vl_typecheck::check(&hir);
        assert!(diags.is_empty(), "{diags:?}");
        let dump = lower(&hir, &typed).dump();
        assert!(dump.contains("const 7u64"), "{dump}");
        // Explicit `return` is the tail: no default-zero fallthrough.
        assert!(!dump.contains("const 0u64"), "{dump}");
    }

    #[test]
    fn bare_tail_values_are_discarded_without_implicit_return() {
        let src = "fun main() { val x = 7; x + 1; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        let (typed, diags) = vl_typecheck::check(&hir);
        assert!(diags.is_empty(), "{diags:?}");
        let dump = lower(&hir, &typed).dump();
        // Discarded tail still lowers, but the function epilogue is the
        // default zero (void fallthrough), not the tail value.
        assert!(dump.contains("const 7u64"), "{dump}");
        assert!(dump.contains("const 0u64"), "{dump}");
    }

    #[test]
    fn explicit_return_emits_ret_and_skips_default() {
        let src = "fun f(): i64 { return 1; } fun m() { return; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        let (typed, diags) = vl_typecheck::check(&hir);
        assert!(diags.is_empty(), "{diags:?}");
        let lir = lower(&hir, &typed);
        let dump = lir.dump();
        assert!(dump.contains("ret"), "{dump}");
        // `f` ends with its explicit return: exactly one `ret`.
        let f = lir.functions.iter().find(|f| f.name == "f").unwrap();
        assert_eq!(
            f.instrs
                .iter()
                .filter(|i| matches!(i, Instr::Ret { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn casts_lower_to_cast_instr_with_target_type() {
        let src = "fun main() { val v = 200u64; val x = v as u8; x; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        let (typed, diags) = vl_typecheck::check(&hir);
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.validate_normalized(&hir, &diags).is_empty());
        let dump = lower(&hir, &typed).dump();
        assert!(dump.contains("cast"), "{dump}");
        assert!(dump.contains(": u8"), "{dump}");
    }

    #[test]
    fn emitted_types_are_normalized() {
        // No `int`/`Param` survives to LIR in monomorphic code: `1 + 2`
        // defaults to `u64` and validates clean.
        let src = "fun main() { 1 + 2; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        let (typed, diags) = vl_typecheck::check(&hir);
        assert!(diags.is_empty(), "{diags:?}");
        assert!(typed.validate_normalized(&hir, &diags).is_empty());
        let dump = lower(&hir, &typed).dump();
        assert!(!dump.contains("int"), "{dump}");
        assert!(dump.contains("u64"), "{dump}");
    }

    #[test]
    fn capabilities_erase_to_identical_runtime_layouts() {
        let src = "type Foo = object { value: u64, }; fun read(v: Foo): u64 { return v.value; } fun edit(m: *Foo): u64 { return m.value; } fun main() { val e: *Foo = Foo { value = 1u64 }; read(e); }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        let (typed, diags) = vl_typecheck::check(&hir);
        assert!(diags.is_empty(), "{diags:?}");
        let lir = lower(&hir, &typed);
        assert!(
            lir.validate_runtime().is_none(),
            "{:?}",
            lir.validate_runtime()
        );
        let dump = lir.dump();
        assert!(!dump.contains('*'), "{dump}");
        // Both functions read via `object_get`; no `*` in types.
        assert!(dump.contains("object_get"), "{dump}");
    }

    #[test]
    fn globals_use_stable_ids_and_explicit_ops() {
        let src = "var a = 1u64; val b = 2u64; fun main() { val x = a + b; a = 3u64; x; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        let (typed, diags) = vl_typecheck::check(&hir);
        assert!(diags.is_empty(), "{diags:?}");
        let lir = lower(&hir, &typed);
        assert_eq!(lir.globals.len(), 2);
        assert_eq!(lir.globals[0].id, 0);
        assert_eq!(lir.globals[1].id, 1);
        let dump = lir.dump();
        assert!(dump.contains("global %0"), "{dump}");
        assert!(dump.contains("global_load"), "{dump}");
        assert!(dump.contains("global_store"), "{dump}");
        // No rematerialized `<global>` pseudo-functions.
        assert!(!dump.contains("<global>"), "{dump}");
        assert!(lir.validate_runtime().is_none());
    }

    #[test]
    fn primitive_rebinding_keeps_independent_homes() {
        // `var b = a; a = 2;` must not change `b`: every binding gets its own
        // home `copy`, for values and references alike.
        let src = "fun main() { var a = 1u64; val b = a; a = 2u64; b; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        let (typed, diags) = vl_typecheck::check(&hir);
        assert!(diags.is_empty(), "{diags:?}");
        let lir = lower(&hir, &typed);
        let dump = lir.dump();
        // Two lets -> two home copies (plus the rebinding copy).
        assert!(dump.matches("copy").count() >= 3, "{dump}");
    }

    #[test]
    fn no_capability_survives_lir() {
        let src = "type Foo = object { value: u64, }; fun main() { val e: *Foo = Foo { value = 1u64 }; val v: Foo = e; e.value = 2u64; v.value; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        let (typed, diags) = vl_typecheck::check(&hir);
        assert!(diags.is_empty(), "{diags:?}");
        let lir = lower(&hir, &typed);
        assert!(lir.validate_runtime().is_none());
        let dump = lir.dump();
        assert!(!dump.contains('*'), "{dump}");
        assert!(dump.contains("object_set"), "{dump}");
    }

    type CheckedTriple = (
        HirProgram,
        vl_typecheck::TypedProgram,
        vl_common::ModuleSpec,
    );

    #[allow(clippy::type_complexity)]
    fn check_two(
        src_a: &str,
        mod_a: &str,
        src_b: &str,
        mod_b: &str,
    ) -> (CheckedTriple, CheckedTriple, Vec<vl_common::ModuleSpec>) {
        let (toks, _) = vl_lex::lex(src_a);
        let (prog_a, _) = vl_syntax::parse_with_module(&toks, src_a, mod_a);
        let (interface_a, _) = vl_semantic::collect_interface_quiet(&prog_a);
        let spec_a = interface_a.as_spec();
        let (toks, _) = vl_lex::lex(src_b);
        let (prog_b, _) = vl_syntax::parse_with_module(&toks, src_b, mod_b);
        let (interface_b, _) = vl_semantic::collect_interface_quiet(&prog_b);
        let spec_b = interface_b.as_spec();
        let modules = vec![spec_a.clone(), spec_b.clone()];
        let catalog = modules.clone();
        let (res_a, _) = vl_semantic::resolve_with_modules(&prog_a, &catalog);
        let hir_a = vl_hir::lower(&prog_a, &res_a);
        let (typed_a, _) = vl_typecheck::check_with_modules(&hir_a, &modules);
        let (res_b, _) = vl_semantic::resolve_with_modules(&prog_b, &catalog);
        let hir_b = vl_hir::lower(&prog_b, &res_b);
        let (typed_b, _) = vl_typecheck::check_with_modules(&hir_b, &modules);
        ((hir_a, typed_a, spec_a), (hir_b, typed_b, spec_b), modules)
    }

    #[test]
    fn project_emit_uses_provider_mangled_symbol() {
        let ((hir_lib, typed_lib, _), (hir_main, typed_main, _), _) = check_two(
            "fun id[T](value: T): T { return value; }",
            "demo.lib",
            "use demo.lib.id; fun main() { val x = id(1u64); }",
            "demo.main",
        );
        let refs = vec![(&hir_lib, &typed_lib), (&hir_main, &typed_main)];
        let (plan, diags) = vl_typecheck::world::plan_world(&refs);
        assert!(diags.is_empty(), "{diags:?}");
        let provider = lower_project(&hir_lib, &typed_lib, &plan);
        let importer = lower_project(&hir_main, &typed_main, &plan);
        assert!(provider.functions.iter().any(|f| f.name == "id$u64"));
        assert!(!provider.functions.iter().any(|f| f.name == "id"));
        assert!(importer.dump().contains("call demo.lib::id$u64"));
        assert!(importer
            .imports
            .iter()
            .any(|i| i.symbol.function == "id$u64"));
        for ty in importer
            .imports
            .iter()
            .flat_map(|i| i.param_tys.iter().chain(std::iter::once(&i.ret)))
        {
            assert!(ty.is_concrete());
            assert!(!matches!(ty, Ty::Param(_)));
        }
    }

    #[test]
    fn project_imports_are_deterministic() {
        let ((hir_lib, typed_lib, _), (hir_main, typed_main, _), _) = check_two(
            "fun id[T](value: T): T { return value; }",
            "demo.lib",
            "use demo.lib.id; fun main() { val a = id(1u64); val b = id(\"hi\"); a; b; }",
            "demo.main",
        );
        let refs = vec![(&hir_lib, &typed_lib), (&hir_main, &typed_main)];
        let (plan, _) = vl_typecheck::world::plan_world(&refs);
        let importer = lower_project(&hir_main, &typed_main, &plan);
        let names: Vec<String> = importer
            .imports
            .iter()
            .map(|i| i.symbol.function.clone())
            .collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }
}
