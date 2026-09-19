//! vl-lir: low-level IR. Typed HIR -> flat three-address code.
//!
//! [`LirProgram`] is deliberately target-agnostic: a list of [`Function`]s
//! each holding [`Instr`]s over virtual [`Reg`]isters. `vl-codegen` lowers
//! this to real targets; since the final target is still undecided, this
//! crate must NOT grow target-specific hacks — add a new backend instead.
//! Returns are explicit: only `return expr;` / `return;` emits [`Instr::Ret`]
//! for a function; trailing expression values are discarded and the
//! fallthrough epilogue returns a dummy zero (`void` backends ignore it).

use std::collections::{HashMap, HashSet};

use vl_common::Scalar;
use vl_common::Span;
use vl_hir::{HirBinOp, HirExpr, HirItem, HirProgram, HirStmt, HirUnOp};
use vl_typecheck::{subst_ty, Ty};

/// Virtual register.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Reg(pub u32);

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
        callee: String,
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
    /// Explicit `return` (or fallthrough / global initializer value).
    Ret {
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

#[derive(Debug, Clone, Default)]
pub struct LirProgram {
    pub module: String,
    pub functions: Vec<Function>,
}

impl LirProgram {
    /// Human-readable dump (`vl build --emit lir`, golden tests).
    pub fn dump(&self) -> String {
        let mut out = String::new();
        for f in &self.functions {
            out.push_str(&format!("fn {}:\n", f.name));
            for (i, ins) in f.instrs.iter().enumerate() {
                out.push_str(&format!("  {i:>3}: {}\n", fmt_instr(ins)));
            }
        }
        out
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
            format!("%{} = call {callee}({args})", dst.0)
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
        Instr::Ret { src, .. } => format!("ret %{}", src.0),
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
    /// Values currently available in this function. Locals are assigned when
    /// declared; globals are materialized lazily from their initializer.
    /// Assignment writes in place (`Copy` into the bound register) so values
    /// stay correct across branches and loop iterations without phi nodes.
    bindings: HashMap<u32, Reg>,
    global_values: HashMap<u32, HirExpr>,
    evaluating_globals: HashSet<u32>,
    next_label: u32,
    loop_stack: Vec<LoopTargets>,
    /// Instance substitution (`Param` -> concrete) for monomorphized bodies;
    /// empty when lowering non-generic code.
    env: HashMap<String, Ty>,
    /// Mangled instance name when lowering a monomorphized body (used to
    /// resolve inner generic calls per instance); `None` for root code.
    outer: Option<String>,
    typed: &'t vl_typecheck::TypedProgram,
}

fn globals(prog: &HirProgram) -> HashMap<u32, HirExpr> {
    prog.items
        .iter()
        .filter_map(|item| match item {
            HirItem::Let {
                def: Some(def),
                value,
                ..
            } => Some((def.0, value.clone())),
            _ => None,
        })
        .collect()
}

impl Lowerer<'_> {
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

    /// Element type of the array produced by `node` (an `ArrayLiteral`,
    /// `Array.new` call, or any array-typed expression).
    fn array_elem_of(&self, id: vl_hir::HirId) -> Option<Ty> {
        match self.resolved_ty(id)? {
            Ty::Array(elem) => Some(*elem),
            _ => None,
        }
    }
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
                if let Some(def) = def {
                    l.bindings.insert(def.0, reg);
                }
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
/// level does not end with an unconditional `return`.
fn lower_fn_epilogue(l: &mut Lowerer, topped_return: bool) {
    let ends_with_ret = topped_return && matches!(l.instrs.last(), Some(Instr::Ret { .. }));
    if !ends_with_ret {
        let r = l.reg();
        l.instrs.push(Instr::Const {
            dst: r,
            value: Scalar::Int(0),
            span: Span::empty(0),
        });
        l.instrs.push(Instr::Ret {
            src: r,
            span: Span::empty(0),
        });
    }
}

/// Lower typed HIR to LIR. Poisoned (`Error`-typed) nodes are skipped —
/// errors were already reported, so no new diagnostics are produced here.
pub fn lower(prog: &HirProgram, typed: &vl_typecheck::TypedProgram) -> LirProgram {
    let global_values = globals(prog);
    let mut out = LirProgram {
        module: prog.module.clone(),
        functions: Vec::new(),
    };
    for item in &prog.items {
        match item {
            HirItem::Let {
                id, value, span, ..
            } => {
                let mut l = Lowerer {
                    next: 0,
                    instrs: vec![],
                    bindings: HashMap::new(),
                    global_values: global_values.clone(),
                    evaluating_globals: HashSet::new(),
                    next_label: 0,
                    loop_stack: Vec::new(),
                    env: HashMap::new(),
                    outer: None,
                    typed,
                };
                if let Some(r) = l.lower_expr(value, typed) {
                    if let HirItem::Let { def: Some(def), .. } = item {
                        l.bindings.insert(def.0, r);
                    }
                    l.instrs.push(Instr::Ret {
                        src: r,
                        span: *span,
                    });
                }
                // Globals record their value type so backends can treat the
                // initializer uniformly with function returns.
                let ret = typed
                    .type_of_id(*id)
                    .or_else(|| typed.type_of_id(value.id()))
                    .unwrap_or(Ty::Error);
                out.functions.push(Function {
                    name: "<global>".into(),
                    param_tys: Vec::new(),
                    ret,
                    instrs: l.instrs,
                });
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
                    .map(|(_, _, t, _)| t.as_ref().map(Ty::from_vl).unwrap_or(Ty::Error))
                    .collect::<Vec<_>>();
                let ret_ty = ret.as_ref().map(Ty::from_vl).unwrap_or(Ty::Error);
                let mut l = Lowerer {
                    next: 0,
                    instrs: vec![],
                    bindings: HashMap::new(),
                    global_values: global_values.clone(),
                    evaluating_globals: HashSet::new(),
                    next_label: 0,
                    loop_stack: Vec::new(),
                    env: HashMap::new(),
                    outer: None,
                    typed,
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
            .cloned()
            .zip(inst.args.iter().cloned())
            .collect();
        let mut l = Lowerer {
            next: 0,
            instrs: vec![],
            bindings: HashMap::new(),
            global_values: global_values.clone(),
            evaluating_globals: HashSet::new(),
            next_label: 0,
            loop_stack: Vec::new(),
            env,
            outer: Some(m.clone()),
            typed,
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
            param_tys: inst.sig.param_tys.clone(),
            ret: inst.sig.ret.clone(),
            instrs: l.instrs,
        });
    }
    out
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
            HirExpr::Var { def: Some(def), .. } => {
                if let Some(reg) = self.bindings.get(&def.0) {
                    return Some(*reg);
                }
                let value = self.global_values.get(&def.0)?.clone();
                if !self.evaluating_globals.insert(def.0) {
                    return None;
                }
                let reg = self.lower_expr(&value, typed);
                self.evaluating_globals.remove(&def.0);
                if let Some(reg) = reg {
                    self.bindings.insert(def.0, reg);
                }
                reg
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
            HirExpr::Call {
                id,
                name,
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
                // Monomorphized callees: root code consults `root_calls`,
                // instance bodies consult `inst_calls` for their own outer
                // instance. Unmapped names call through unchanged.
                let callee = match &self.outer {
                    Some(outer) => self
                        .typed
                        .inst_calls
                        .get(&(outer.clone(), id.0))
                        .cloned()
                        .unwrap_or_else(|| name.clone()),
                    None => self
                        .typed
                        .root_calls
                        .get(&id.0)
                        .cloned()
                        .unwrap_or_else(|| name.clone()),
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
        }
    }

    fn lower_stmt(&mut self, stmt: &HirStmt, typed: &vl_typecheck::TypedProgram) {
        match stmt {
            HirStmt::Let { def, value, .. } => {
                if let Some(reg) = self.lower_expr(value, typed) {
                    if let Some(def) = def {
                        self.bindings.insert(def.0, reg);
                    }
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
    /// already-bound register. The bindings map is unchanged, so branches and
    /// loops that assign keep working without phi nodes; branch-local `let`s
    /// are still pruned by the caller restoring the incoming map.
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
        // Assignment to a not-yet-materialized global: materialize first.
        if let Some(init) = self.global_values.get(&def.0).cloned() {
            if !self.evaluating_globals.insert(def.0) {
                return;
            }
            let base = self.lower_expr(&init, typed);
            self.evaluating_globals.remove(&def.0);
            if let Some(base) = base {
                self.bindings.insert(def.0, base);
                self.instrs.push(Instr::Copy {
                    dst: base,
                    src,
                    span,
                });
            }
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
        // Drop loop-local `let`s; in-place `Copy` writes to shared registers
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrays_lower_to_dedicated_instrs() {
        let src = "function get(a: Array[u64]): u64 { a[0u64] = 1u64; return a[1u64]; } function main() { let a = Array.new::[u64](2u64); let b = [1u64, 2u64]; }";
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
    fn generic_templates_emit_only_instances() {
        let src = "function id[T](x: T): T { return x; } function main() { let a = id(1u64); a; }";
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
        let src = "function main() { let i = 0; while (i < 10) { i = i + 1; } }";
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
        let src = "function main() { let x = true && false; x; }";
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
        let src = "function main() { !true; }";
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
        let src = "let x = 1 + 2;";
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        let (typed, _) = vl_typecheck::check(&hir);
        let lir = lower(&hir, &typed);
        let dump = lir.dump();
        assert!(dump.contains("add"));
        assert!(dump.contains("ret"));
    }

    #[test]
    fn lowers_call_and_parameter_registers() {
        let src =
            "function add(a: i64, b: i64): i64 { return a + b; } function main() { add(1, 2); }";
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
        let src = r#"function greet(name: string, n: u64): string { return name; } function main() { greet("hi", 1u64); }"#;
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
    fn globals_record_their_value_type() {
        let src = r#"let x = 1u64;"#;
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        let (typed, diags) = vl_typecheck::check(&hir);
        assert!(diags.is_empty(), "{diags:?}");
        let lir = lower(&hir, &typed);
        assert_eq!(lir.functions.len(), 1);
        assert!(lir.functions[0].param_tys.is_empty());
        assert_eq!(lir.functions[0].ret, Ty::U64);
    }

    #[test]
    fn local_reads_use_the_declared_value() {
        let src = "function f(): i64 { let x = 7; return x + 1; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        let (typed, diags) = vl_typecheck::check(&hir);
        assert!(diags.is_empty());
        let dump = lower(&hir, &typed).dump();
        assert!(dump.contains("const 7int"), "{dump}");
        // Explicit `return` is the tail: no default-zero fallthrough.
        assert!(!dump.contains("const 0int"), "{dump}");
    }

    #[test]
    fn bare_tail_values_are_discarded_without_implicit_return() {
        let src = "function main() { let x = 7; x + 1; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        let (typed, diags) = vl_typecheck::check(&hir);
        assert!(diags.is_empty(), "{diags:?}");
        let dump = lower(&hir, &typed).dump();
        // Discarded tail still lowers, but the function epilogue is the
        // default zero (void fallthrough), not the tail value.
        assert!(dump.contains("const 7int"), "{dump}");
        assert!(dump.contains("const 0int"), "{dump}");
    }

    #[test]
    fn explicit_return_emits_ret_and_skips_default() {
        let src = "function f(): i64 { return 1; } function m() { return; }";
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
}
