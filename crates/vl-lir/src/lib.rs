//! vl-lir: low-level IR. Typed HIR -> flat three-address code.
//!
//! [`LirProgram`] is deliberately target-agnostic: a list of [`Function`]s
//! each holding [`Instr`]s over virtual [`Reg`]isters. `vl-codegen` lowers
//! this to real targets; since the final target is still undecided, this
//! crate must NOT grow target-specific hacks — add a new backend instead.

use std::collections::{HashMap, HashSet};

use vl_common::Scalar;
use vl_common::Span;
use vl_hir::{HirBinOp, HirExpr, HirItem, HirProgram, HirStmt, HirUnOp};
use vl_typecheck::Ty;

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
    /// Tail value of a function body / global initializer.
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

struct Lowerer {
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

impl Lowerer {
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
                params,
                ret,
                body,
                ..
            } => {
                let param_tys = params
                    .iter()
                    .map(|(_, _, t, _)| t.map(Ty::from_vl).unwrap_or(Ty::Error))
                    .collect::<Vec<_>>();
                let ret_ty = ret.map(Ty::from_vl).unwrap_or(Ty::Error);
                let mut l = Lowerer {
                    next: 0,
                    instrs: vec![],
                    bindings: HashMap::new(),
                    global_values: global_values.clone(),
                    evaluating_globals: HashSet::new(),
                    next_label: 0,
                    loop_stack: Vec::new(),
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

                let mut last = None;
                for stmt in body {
                    match stmt {
                        HirStmt::Let { def, value, .. } => {
                            last = l.lower_expr(value, typed);
                            if let (Some(def), Some(reg)) = (def, last) {
                                l.bindings.insert(def.0, reg);
                            }
                        }
                        HirStmt::Expr(e) => {
                            last = l.lower_expr(e, typed);
                        }
                        HirStmt::If {
                            condition,
                            then_body,
                            else_body,
                            span,
                        } => {
                            l.lower_if(condition, then_body, else_body.as_deref(), typed, *span);
                        }
                        HirStmt::Assign {
                            def, value, span, ..
                        } => {
                            l.lower_assign(def.as_ref(), value, typed, *span);
                        }
                        HirStmt::While {
                            condition,
                            body,
                            span,
                        } => {
                            l.lower_while(condition, body, typed, *span);
                        }
                        HirStmt::Break { span } => {
                            l.lower_break(*span);
                        }
                        HirStmt::Continue { span } => {
                            l.lower_continue(*span);
                        }
                    }
                }
                // Bodies always return something; default to 0.
                let ret = match last {
                    Some(r) => r,
                    None => {
                        let r = l.reg();
                        l.instrs.push(Instr::Const {
                            dst: r,
                            value: Scalar::I64(0),
                            span: Span::empty(0),
                        });
                        r
                    }
                };
                l.instrs.push(Instr::Ret {
                    src: ret,
                    span: Span::empty(0),
                });
                out.functions.push(Function {
                    name: name.clone(),
                    param_tys,
                    ret: ret_ty,
                    instrs: l.instrs,
                });
            }
        }
    }
    out
}

impl Lowerer {
    fn lower_expr(&mut self, expr: &HirExpr, typed: &vl_typecheck::TypedProgram) -> Option<Reg> {
        if typed.type_of_id(expr.id()) == Some(vl_typecheck::Ty::Error) {
            return None;
        }
        match expr {
            HirExpr::Literal { value, span, .. } => {
                let dst = self.reg();
                self.instrs.push(Instr::Const {
                    dst,
                    value: *value,
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
            HirExpr::Call {
                name, args, span, ..
            } => {
                let mut arg_regs = Vec::with_capacity(args.len());
                for arg in args {
                    arg_regs.push(self.lower_expr(arg, typed)?);
                }
                let dst = self.reg();
                self.instrs.push(Instr::Call {
                    dst,
                    callee: name.clone(),
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
        let src = "function add(a: i64, b: i64): i64 { a + b; } function main() { add(1, 2); }";
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
        let src = r#"function greet(name: string, n: u64): string { name; } function main() { greet("hi", 1u64); }"#;
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
        let src = "function main() { let x = 7; x + 1; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        let (typed, diags) = vl_typecheck::check(&hir);
        assert!(diags.is_empty());
        let dump = lower(&hir, &typed).dump();
        assert!(dump.contains("const 7i64"), "{dump}");
        assert!(!dump.contains("const 0i64"), "{dump}");
    }
}
