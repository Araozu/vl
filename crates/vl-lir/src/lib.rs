//! vl-lir: low-level IR. Typed HIR -> flat three-address code.
//!
//! [`LirProgram`] is deliberately target-agnostic: a list of [`Function`]s
//! each holding [`Instr`]s over virtual [`Reg`]isters. `vl-codegen` lowers
//! this to real targets; since the final target is still undecided, this
//! crate must NOT grow target-specific hacks — add a new backend instead.

use std::collections::HashMap;

use vl_common::Span;
use vl_hir::{HirBinOp, HirExpr, HirItem, HirProgram, HirStmt};

/// Virtual register.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Reg(pub u32);

/// Three-address instructions. Strings are carried as raw bytes; codegen is
/// intentionally not implemented yet.
#[derive(Debug, Clone)]
pub enum Instr {
    Const {
        dst: Reg,
        value: i64,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LirOp {
    Add,
    Sub,
    Mul,
    Div,
}

impl std::fmt::Display for LirOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LirOp::Add => write!(f, "add"),
            LirOp::Sub => write!(f, "sub"),
            LirOp::Mul => write!(f, "mul"),
            LirOp::Div => write!(f, "div"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Function {
    pub name: String,
    pub instrs: Vec<Instr>,
}

#[derive(Debug, Clone, Default)]
pub struct LirProgram {
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
        Instr::Const { dst, value, .. } => format!("%{} = const {value}", dst.0),
        Instr::StringConst { dst, value, .. } => {
            format!("%{} = string {value:?}", dst.0)
        }
        Instr::Param { dst, index, .. } => format!("%{} = param {index}", dst.0),
        Instr::Copy { dst, src, .. } => format!("%{} = copy %{}", dst.0, src.0),
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
    }
}

struct Lowerer {
    next: u32,
    instrs: Vec<Instr>,
    /// Bindings for parameters are enough to make calls useful without
    /// changing the existing v0 treatment of locals and globals.
    params: HashMap<u32, Reg>,
}

impl Lowerer {
    fn reg(&mut self) -> Reg {
        let r = Reg(self.next);
        self.next += 1;
        r
    }
}

/// Lower typed HIR to LIR. Poisoned (`Error`-typed) nodes are skipped —
/// errors were already reported, so no new diagnostics are produced here.
pub fn lower(prog: &HirProgram, typed: &vl_typecheck::TypedProgram) -> LirProgram {
    let mut out = LirProgram::default();
    for item in &prog.items {
        match item {
            HirItem::Let { value, span, .. } => {
                let mut l = Lowerer {
                    next: 0,
                    instrs: vec![],
                    params: HashMap::new(),
                };
                if let Some(r) = l.lower_expr(value, typed) {
                    l.instrs.push(Instr::Ret {
                        src: r,
                        span: *span,
                    });
                }
                out.functions.push(Function {
                    name: "<global>".into(),
                    instrs: l.instrs,
                });
            }
            HirItem::Fn {
                name, params, body, ..
            } => {
                let mut l = Lowerer {
                    next: 0,
                    instrs: vec![],
                    params: HashMap::new(),
                };

                for (index, (_, def, span)) in params.iter().enumerate() {
                    let dst = l.reg();
                    l.instrs.push(Instr::Param {
                        dst,
                        index,
                        span: *span,
                    });
                    if let Some(def) = def {
                        l.params.insert(def.0, dst);
                    }
                }

                let mut last = None;
                for stmt in body {
                    match stmt {
                        HirStmt::Let { value, .. } => {
                            last = l.lower_expr(value, typed);
                        }
                        HirStmt::Expr(e) => {
                            last = l.lower_expr(e, typed);
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
                            value: 0,
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
            HirExpr::Int { value, span, .. } => {
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
            HirExpr::Var { span, .. } => {
                // Parameters have real registers. Other variables still use
                // the v0 placeholder until full local/global storage lands.
                if let HirExpr::Var { def: Some(def), .. } = expr {
                    if let Some(reg) = self.params.get(&def.0) {
                        return Some(*reg);
                    }
                }
                let dst = self.reg();
                self.instrs.push(Instr::Const {
                    dst,
                    value: 0,
                    span: *span,
                });
                Some(dst)
            }
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
            } => {
                let l = self.lower_expr(lhs, typed)?;
                let r = self.lower_expr(rhs, typed)?;
                let dst = self.reg();
                let op = match op {
                    HirBinOp::Add => LirOp::Add,
                    HirBinOp::Sub => LirOp::Sub,
                    HirBinOp::Mul => LirOp::Mul,
                    HirBinOp::Div => LirOp::Div,
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let src = "function add(a, b) { a + b; } function main() { add(1, 2); }";
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
}
