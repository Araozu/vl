//! vl-hir: high-level IR. Desugared, explicitly-scoped form of the AST.
//!
//! Differences from the AST: unary negation is lowered to `0 - x`,
//! every node carries a [`HirId`], and variable uses carry the [`DefId`]
//! resolved by `vl-semantic` (or `None` when resolution failed, so later
//! stages can skip rather than cascade errors). Returns are explicit:
//! only `return expr;` / `return;` yields a value; trailing expression
//! statements are discarded values, never implicit returns.
//!
//! Reference types are capabilities (`Foo` is a read-only view,
//! `*Foo` is a mutable view of the same GC allocation). HIR preserves
//! every qualified [`VlType`] exactly for type checking; it never erases
//! capability and never desugars field/index writes into pointer
//! operations. Binding assignment (`Assign`), field writes
//! (`FieldAssign`), and element writes (`IndexAssign`) stay distinct.

use vl_common::{GenericBound, Scalar, Span, VlType};

pub use vl_semantic::DefId;
use vl_syntax::{
    BinOp as AstBinOp, Expr as AstExpr, Item as AstItem, Program as AstProgram, Stmt as AstStmt,
    UnOp as AstUnOp,
};

pub use vl_syntax::BindingKind;

/// One generic type parameter with its optional bound
/// (`T` vs `T extends Numeric`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirTypeParam {
    pub name: String,
    pub bound: Option<GenericBound>,
}

/// Unique node id within one lowering run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HirId(pub u32);

#[derive(Debug, Clone)]
pub struct HirProgram {
    pub module: String,
    pub items: Vec<HirItem>,
}

/// One destructured binding: new local name plus how to read it.
/// `field` is `None` for positional (unnamed) access by `index`,
/// `Some(field)` for named access by field name.
#[derive(Debug, Clone)]
pub struct HirDestructureBinding {
    pub field: Option<String>,
    pub binding: String,
    pub index: usize,
    pub def: Option<DefId>,
    pub binding_span: Span,
}

#[derive(Debug, Clone)]
pub struct HirUnionVariant {
    pub name: String,
    pub name_span: Span,
    pub payload: Vec<(VlType, Span)>,
}

/// One `match` arm binding: a fresh implicit-`val` local for one payload
/// position. `def` is `None` when resolution failed (already reported).
#[derive(Debug, Clone)]
pub struct HirMatchBinding {
    pub binding: String,
    pub def: Option<DefId>,
    pub binding_span: Span,
}

/// One `match` arm: `Union.Variant` plus positional payload bindings.
/// `union` holds every path segment but the last (`["Option"]`,
/// `["m", "Option"]`); `variant` holds the last.
#[derive(Debug, Clone)]
pub struct HirMatchArm {
    pub union: String,
    pub variant: String,
    pub bindings: Vec<HirMatchBinding>,
    pub body: Vec<HirStmt>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum HirItem {
    Object {
        name: String,
        fields: Vec<(String, Option<VlType>, Span)>,
        span: Span,
    },
    Union {
        name: String,
        type_params: Vec<HirTypeParam>,
        variants: Vec<HirUnionVariant>,
        span: Span,
    },
    Let {
        id: HirId,
        def: Option<DefId>,
        kind: BindingKind,
        /// Optional annotation (`None` = infer; a failed annotation parses
        /// as `None` with `ty_span` set and poisons quietly downstream).
        ty: Option<VlType>,
        ty_span: Option<Span>,
        value: HirExpr,
        span: Span,
    },
    Destructure {
        id: HirId,
        kind: BindingKind,
        bindings: Vec<HirDestructureBinding>,
        ty: Option<VlType>,
        ty_span: Option<Span>,
        value: HirExpr,
        span: Span,
    },
    Fn {
        id: HirId,
        def: Option<DefId>,
        name: String,
        /// Declared type parameters (`[]` when monomorphic).
        type_params: Vec<HirTypeParam>,
        /// `(name, def, type, span)`. `ty` is `None` when the annotation was
        /// missing/unknown (already reported; typecheck poisons quietly).
        params: Vec<(String, Option<DefId>, Option<VlType>, Span)>,
        /// Declared return type. `None` means missing (already reported).
        ret: Option<VlType>,
        ret_span: Option<Span>,
        body: Vec<HirStmt>,
        span: Span,
    },
}

#[derive(Debug, Clone)]
pub enum HirStmt {
    Let {
        id: HirId,
        def: Option<DefId>,
        kind: BindingKind,
        /// Optional annotation, same encoding as [`HirItem::Let`].
        ty: Option<VlType>,
        ty_span: Option<Span>,
        value: HirExpr,
        span: Span,
    },
    Assign {
        id: HirId,
        def: Option<DefId>,
        value: HirExpr,
        span: Span,
    },
    /// Element write: `array[index] = value;`.
    IndexAssign {
        id: HirId,
        array: Box<HirExpr>,
        index: Box<HirExpr>,
        value: Box<HirExpr>,
        span: Span,
    },
    FieldAssign {
        id: HirId,
        base: Box<HirExpr>,
        field: String,
        value: Box<HirExpr>,
        span: Span,
    },
    TupleAssign {
        id: HirId,
        base: Box<HirExpr>,
        index: usize,
        value: Box<HirExpr>,
        span: Span,
    },
    Destructure {
        id: HirId,
        kind: BindingKind,
        bindings: Vec<HirDestructureBinding>,
        ty: Option<VlType>,
        ty_span: Option<Span>,
        value: HirExpr,
        span: Span,
    },
    If {
        condition: HirExpr,
        then_body: Vec<HirStmt>,
        else_body: Option<Vec<HirStmt>>,
        span: Span,
    },
    Match {
        scrutinee: HirExpr,
        arms: Vec<HirMatchArm>,
        else_body: Option<Vec<HirStmt>>,
        span: Span,
    },
    While {
        condition: HirExpr,
        body: Vec<HirStmt>,
        span: Span,
    },
    Break {
        span: Span,
    },
    Continue {
        span: Span,
    },
    Return {
        value: Option<HirExpr>,
        span: Span,
    },
    Expr(HirExpr),
}

#[derive(Debug, Clone)]
pub enum HirExpr {
    Literal {
        id: HirId,
        value: Scalar,
        span: Span,
    },
    String {
        id: HirId,
        value: Vec<u8>,
        span: Span,
    },
    /// Array literal: `[1, 2]`; integer element types are contextual.
    ArrayLiteral {
        id: HirId,
        elems: Vec<HirExpr>,
        span: Span,
    },
    ObjectLiteral {
        id: HirId,
        name: String,
        fields: Vec<(String, HirExpr)>,
        span: Span,
    },
    /// Union variant construction: `Option.Some(args)` or `Option.None`.
    /// Lowered from `Call`/`Field` sites recorded by `vl-semantic`
    /// (`Resolution::variants`); `type_args` carries an explicit turbofish
    /// (`Option.Some::[u64](...)`), empty when inference should fill in.
    Variant {
        id: HirId,
        union: String,
        variant: String,
        type_args: Vec<VlType>,
        args: Vec<HirExpr>,
        span: Span,
    },
    /// Element read: `array[index]`.
    Index {
        id: HirId,
        base: Box<HirExpr>,
        index: Box<HirExpr>,
        span: Span,
    },
    Field {
        id: HirId,
        base: Box<HirExpr>,
        name: String,
        span: Span,
    },
    TupleLiteral {
        id: HirId,
        elems: Vec<(Option<String>, HirExpr)>,
        span: Span,
    },
    TupleIndex {
        id: HirId,
        base: Box<HirExpr>,
        index: usize,
        span: Span,
    },
    Var {
        id: HirId,
        def: Option<DefId>,
        name: String,
        span: Span,
    },
    /// Null literal (`null`): empty value of any `?T`. Desugars to the
    /// builtin `Option.None`; typechecking infers the argument from context.
    Null { id: HirId, span: Span },
    Call {
        id: HirId,
        /// Resolved callee (`None` when resolution failed; quiet downstream).
        def: Option<DefId>,
        external: bool,
        /// Compiler-owned callable signature copied from the resolved `Def`.
        /// `None` for locals, poisoned imports, or unresolved callees. May be
        /// generic for imported source functions.
        extern_sig: Option<vl_common::FuncSig>,
        /// Source-versus-target linkage for imported calls. `None` for locals,
        /// poisoned imports, and synthetic builtins.
        extern_kind: Option<vl_common::ExportKind>,
        /// Qualified provider identity for imported and target functions.
        symbol: Option<vl_common::SymbolRef>,
        name: String,
        /// Explicit type arguments (`f::[u64]`); empty means infer.
        type_args: Vec<VlType>,
        args: Vec<HirExpr>,
        span: Span,
    },
    /// Instance sugar: `receiver.method(args)` where the receiver is a value
    /// path (`c.bump()`, `a.b.step()`). The resolver records these sites
    /// (no E201 there); typechecking validates that the method exists on the
    /// receiver's object type and that its first parameter takes the receiver
    /// (`self: Counter` / `self: *Counter`), then records the resolved
    /// target (`Owner.method`, possibly a generic instance) in the typed
    /// program for LIR. Semantically the call is
    /// `Owner.method(receiver, args...)`.
    MethodCall {
        id: HirId,
        receiver: Box<HirExpr>,
        method: String,
        method_span: Span,
        /// Explicit type arguments (`c.f::[u64](...)`); empty means infer.
        type_args: Vec<VlType>,
        args: Vec<HirExpr>,
        span: Span,
    },
    Binary {
        id: HirId,
        op: HirBinOp,
        lhs: Box<HirExpr>,
        rhs: Box<HirExpr>,
        span: Span,
    },
    Unary {
        id: HirId,
        op: HirUnOp,
        inner: Box<HirExpr>,
        span: Span,
    },
    /// Explicit conversion (`value as u8`).
    Cast {
        id: HirId,
        inner: Box<HirExpr>,
        target: VlType,
        target_span: Span,
        span: Span,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HirUnOp {
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HirBinOp {
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
    And,
    Or,
}

impl HirExpr {
    pub fn id(&self) -> HirId {
        match self {
            HirExpr::Literal { id, .. }
            | HirExpr::String { id, .. }
            | HirExpr::Null { id, .. }
            | HirExpr::ArrayLiteral { id, .. }
            | HirExpr::ObjectLiteral { id, .. }
            | HirExpr::Variant { id, .. }
            | HirExpr::TupleLiteral { id, .. }
            | HirExpr::TupleIndex { id, .. }
            | HirExpr::Index { id, .. }
            | HirExpr::Field { id, .. }
            | HirExpr::Var { id, .. }
            | HirExpr::Call { id, .. }
            | HirExpr::MethodCall { id, .. }
            | HirExpr::Binary { id, .. }
            | HirExpr::Unary { id, .. }
            | HirExpr::Cast { id, .. } => *id,
        }
    }

    pub fn span(&self) -> Span {
        match self {
            HirExpr::Literal { span, .. }
            | HirExpr::String { span, .. }
            | HirExpr::Null { span, .. }
            | HirExpr::ArrayLiteral { span, .. }
            | HirExpr::ObjectLiteral { span, .. }
            | HirExpr::Variant { span, .. }
            | HirExpr::TupleLiteral { span, .. }
            | HirExpr::TupleIndex { span, .. }
            | HirExpr::Index { span, .. }
            | HirExpr::Field { span, .. }
            | HirExpr::Var { span, .. }
            | HirExpr::Call { span, .. }
            | HirExpr::MethodCall { span, .. }
            | HirExpr::Binary { span, .. }
            | HirExpr::Unary { span, .. }
            | HirExpr::Cast { span, .. } => *span,
        }
    }
}

struct Lowerer<'a> {
    next: u32,
    res: &'a vl_semantic::Resolution,
}

impl<'a> Lowerer<'a> {
    fn id(&mut self) -> HirId {
        let id = HirId(self.next);
        self.next += 1;
        id
    }

    /// Use-site lookup (variable reads, callees).
    fn def_at(&self, span: Span) -> Option<DefId> {
        self.res.def_of(span).map(|d| d.id.clone())
    }

    /// Definition-site lookup (`var`/`val` names, `fun` names, params).
    fn def_at_site(&self, span: Span) -> Option<DefId> {
        self.res.def_at(span).map(|d| d.id.clone())
    }
}

/// Lower a parsed program with its resolution map. Infallible by design:
/// unresolved names become `def: None` and are reported by earlier stages.
///
/// Associated functions lower to ordinary `Fn` items named `Owner.method`
/// (so typechecking, monomorphization, and LIR reuse the free-function
/// paths); the fields-only `Object` item keeps the layout.
pub fn lower(prog: &AstProgram, res: &vl_semantic::Resolution) -> HirProgram {
    let mut l = Lowerer { next: 0, res };
    let items = prog
        .items
        .iter()
        .flat_map(|i| match i {
            AstItem::Use { .. } => Vec::new(),
            _ => l.lower_item(i),
        })
        .collect();
    HirProgram {
        module: prog.module.clone(),
        items,
    }
}

impl<'a> Lowerer<'a> {
    fn lower_item(&mut self, item: &AstItem) -> Vec<HirItem> {
        match item {
            AstItem::Use { .. } => unreachable!("use items are filtered before lowering"),
            AstItem::Object {
                name,
                fields,
                methods,
                span,
                ..
            } => {
                let mut out = Vec::with_capacity(1 + methods.len());
                out.push(HirItem::Object {
                    name: name.clone(),
                    fields: fields
                        .iter()
                        .map(|f| (f.name.clone(), f.ty.clone(), f.name_span))
                        .collect(),
                    span: *span,
                });
                for m in methods {
                    out.push(HirItem::Fn {
                        id: self.id(),
                        def: self.def_at_site(m.name_span),
                        name: format!("{name}.{}", m.name),
                        type_params: m
                            .type_params
                            .iter()
                            .map(|p| HirTypeParam {
                                name: p.name.clone(),
                                bound: p.bound,
                            })
                            .collect(),
                        params: m
                            .params
                            .iter()
                            .map(|p| {
                                (
                                    p.name.clone(),
                                    self.def_at_site(p.name_span),
                                    p.ty.clone(),
                                    p.name_span,
                                )
                            })
                            .collect(),
                        ret: m.ret.clone(),
                        ret_span: m.ret_span,
                        body: m.body.iter().map(|s| self.lower_stmt(s)).collect(),
                        span: m.span,
                    });
                }
                out
            }
            AstItem::Union {
                name,
                type_params,
                variants,
                span,
                ..
            } => vec![HirItem::Union {
                name: name.clone(),
                type_params: type_params
                    .iter()
                    .map(|p| HirTypeParam {
                        name: p.name.clone(),
                        bound: p.bound,
                    })
                    .collect(),
                variants: variants
                    .iter()
                    .map(|v| HirUnionVariant {
                        name: v.name.clone(),
                        name_span: v.name_span,
                        payload: v.payload.clone(),
                    })
                    .collect(),
                span: *span,
            }],
            AstItem::Let {
                value,
                span,
                name_span,
                kind,
                ty,
                ty_span,
                ..
            } => {
                let def = self.def_at_site(*name_span);
                vec![HirItem::Let {
                    id: self.id(),
                    def,
                    kind: *kind,
                    ty: ty.clone(),
                    ty_span: *ty_span,
                    value: self.lower_expr(value),
                    span: *span,
                }]
            }
            AstItem::Destructure {
                kind,
                bindings,
                ty,
                ty_span,
                value,
                span,
                ..
            } => {
                let lowered: Vec<HirDestructureBinding> = bindings
                    .iter()
                    .enumerate()
                    .map(|(i, b)| HirDestructureBinding {
                        field: b.field.clone(),
                        binding: b.binding.clone(),
                        index: i,
                        def: self.def_at_site(b.binding_span),
                        binding_span: b.binding_span,
                    })
                    .collect();
                vec![HirItem::Destructure {
                    id: self.id(),
                    kind: *kind,
                    bindings: lowered,
                    ty: ty.clone(),
                    ty_span: *ty_span,
                    value: self.lower_expr(value),
                    span: *span,
                }]
            }
            AstItem::Function {
                name,
                name_span,
                type_params,
                params,
                ret,
                ret_span,
                body,
                span,
                ..
            } => vec![HirItem::Fn {
                id: self.id(),
                def: self.def_at_site(*name_span),
                name: name.clone(),
                type_params: type_params
                    .iter()
                    .map(|p| HirTypeParam {
                        name: p.name.clone(),
                        bound: p.bound,
                    })
                    .collect(),
                params: params
                    .iter()
                    .map(|p| {
                        (
                            p.name.clone(),
                            self.def_at_site(p.name_span),
                            p.ty.clone(),
                            p.name_span,
                        )
                    })
                    .collect(),
                ret: ret.clone(),
                ret_span: *ret_span,
                body: body.iter().map(|s| self.lower_stmt(s)).collect(),
                span: *span,
            }],
        }
    }

    fn lower_stmt(&mut self, stmt: &AstStmt) -> HirStmt {
        match stmt {
            AstStmt::Let {
                value,
                span,
                name_span,
                kind,
                ty,
                ty_span,
                ..
            } => {
                let def = self.def_at_site(*name_span);
                HirStmt::Let {
                    id: self.id(),
                    def,
                    kind: *kind,
                    ty: ty.clone(),
                    ty_span: *ty_span,
                    value: self.lower_expr(value),
                    span: *span,
                }
            }
            AstStmt::Assign {
                name_span,
                value,
                span,
                ..
            } => {
                // Assignment writes through the resolved binding; the LHS is
                // a use-site (recorded by the resolver), not a new def-site.
                let def = self.def_at(*name_span);
                HirStmt::Assign {
                    id: self.id(),
                    def,
                    value: self.lower_expr(value),
                    span: *span,
                }
            }
            AstStmt::IndexAssign {
                array,
                index,
                value,
                span,
            } => HirStmt::IndexAssign {
                id: self.id(),
                array: Box::new(self.lower_expr(array)),
                index: Box::new(self.lower_expr(index)),
                value: Box::new(self.lower_expr(value)),
                span: *span,
            },
            AstStmt::FieldAssign {
                base,
                field,
                value,
                span,
                ..
            } => HirStmt::FieldAssign {
                id: self.id(),
                base: Box::new(self.lower_expr(base)),
                field: field.clone(),
                value: Box::new(self.lower_expr(value)),
                span: *span,
            },
            AstStmt::TupleAssign {
                base,
                index,
                value,
                span,
                ..
            } => HirStmt::TupleAssign {
                id: self.id(),
                base: Box::new(self.lower_expr(base)),
                index: *index,
                value: Box::new(self.lower_expr(value)),
                span: *span,
            },
            AstStmt::Destructure {
                kind,
                bindings,
                ty,
                ty_span,
                value,
                span,
                ..
            } => {
                let lowered: Vec<HirDestructureBinding> = bindings
                    .iter()
                    .enumerate()
                    .map(|(i, b)| HirDestructureBinding {
                        field: b.field.clone(),
                        binding: b.binding.clone(),
                        index: i,
                        def: self.def_at_site(b.binding_span),
                        binding_span: b.binding_span,
                    })
                    .collect();
                HirStmt::Destructure {
                    id: self.id(),
                    kind: *kind,
                    bindings: lowered,
                    ty: ty.clone(),
                    ty_span: *ty_span,
                    value: self.lower_expr(value),
                    span: *span,
                }
            }
            AstStmt::Expr(e) => HirStmt::Expr(self.lower_expr(e)),
            AstStmt::Return { value, span } => HirStmt::Return {
                value: value.as_ref().map(|e| self.lower_expr(e)),
                span: *span,
            },
            AstStmt::Break { span } => HirStmt::Break { span: *span },
            AstStmt::Continue { span } => HirStmt::Continue { span: *span },
            AstStmt::While {
                condition,
                body,
                span,
            } => HirStmt::While {
                condition: self.lower_expr(condition),
                body: body.iter().map(|s| self.lower_stmt(s)).collect(),
                span: *span,
            },
            AstStmt::If {
                condition,
                then_body,
                else_body,
                span,
            } => HirStmt::If {
                condition: self.lower_expr(condition),
                then_body: then_body.iter().map(|s| self.lower_stmt(s)).collect(),
                else_body: else_body
                    .as_ref()
                    .map(|body| body.iter().map(|s| self.lower_stmt(s)).collect()),
                span: *span,
            },
            AstStmt::Match {
                scrutinee,
                arms,
                else_body,
                span,
            } => HirStmt::Match {
                scrutinee: self.lower_expr(scrutinee),
                arms: arms
                    .iter()
                    .map(|arm| {
                        // `null` arm: sugar for the `None` case of a `?T`
                        // scrutinee. Desugars here to the builtin
                        // `Option.None` so typechecking and LIR reuse the
                        // ordinary union paths ("sugar all the way").
                        if arm.path == ["null"] {
                            return HirMatchArm {
                                union: "Option".to_string(),
                                variant: "None".to_string(),
                                bindings: Vec::new(),
                                body: arm.body.iter().map(|s| self.lower_stmt(s)).collect(),
                                span: arm.span,
                            };
                        }
                        let (variant, union_path) = arm
                            .path
                            .split_last()
                            .expect("parser guarantees 2+ segments");
                        // Import-alias heads were canonicalized by the
                        // resolver to the module-qualified spelling.
                        let union = self
                            .res
                            .match_patterns
                            .get(&(arm.path_span.start, arm.path_span.end))
                            .cloned()
                            .unwrap_or_else(|| union_path.join("."));
                        HirMatchArm {
                            union,
                            variant: variant.clone(),
                            bindings: arm
                                .bindings
                                .iter()
                                .map(|(name, span)| HirMatchBinding {
                                    binding: name.clone(),
                                    def: self.def_at_site(*span),
                                    binding_span: *span,
                                })
                                .collect(),
                            body: arm.body.iter().map(|s| self.lower_stmt(s)).collect(),
                            span: arm.span,
                        }
                    })
                    .collect(),
                else_body: else_body
                    .as_ref()
                    .map(|body| body.iter().map(|s| self.lower_stmt(s)).collect()),
                span: *span,
            },
        }
    }

    fn lower_expr(&mut self, expr: &AstExpr) -> HirExpr {
        match expr {
            AstExpr::Literal(value, s) => HirExpr::Literal {
                id: self.id(),
                value: *value,
                span: *s,
            },
            AstExpr::String(value, s) => HirExpr::String {
                id: self.id(),
                value: value.clone(),
                span: *s,
            },
            AstExpr::Null(span) => HirExpr::Null {
                id: self.id(),
                span: *span,
            },
            AstExpr::ArrayLiteral { elems, span } => HirExpr::ArrayLiteral {
                id: self.id(),
                elems: elems.iter().map(|e| self.lower_expr(e)).collect(),
                span: *span,
            },
            AstExpr::ObjectLiteral {
                name, fields, span, ..
            } => HirExpr::ObjectLiteral {
                id: self.id(),
                name: name.clone(),
                fields: fields
                    .iter()
                    .map(|(name, _, value)| (name.clone(), self.lower_expr(value)))
                    .collect(),
                span: *span,
            },
            AstExpr::Index { base, index, span } => HirExpr::Index {
                id: self.id(),
                base: Box::new(self.lower_expr(base)),
                index: Box::new(self.lower_expr(index)),
                span: *span,
            },
            AstExpr::Field { base, name, span } => {
                // A nullary variant use (`Option.None`) recorded by the
                // resolver. Anything else is an ordinary field read.
                if let Some(use_) = self.res.variants.get(&(span.start, span.end)).cloned() {
                    return HirExpr::Variant {
                        id: self.id(),
                        union: use_.union,
                        variant: use_.variant,
                        type_args: Vec::new(),
                        args: Vec::new(),
                        span: *span,
                    };
                }
                HirExpr::Field {
                    id: self.id(),
                    base: Box::new(self.lower_expr(base)),
                    name: name.clone(),
                    span: *span,
                }
            }
            AstExpr::TupleLiteral { elems, span } => HirExpr::TupleLiteral {
                id: self.id(),
                elems: elems
                    .iter()
                    .map(|(name, _, value)| (name.clone(), self.lower_expr(value)))
                    .collect(),
                span: *span,
            },
            AstExpr::TupleIndex {
                base, index, span, ..
            } => HirExpr::TupleIndex {
                id: self.id(),
                base: Box::new(self.lower_expr(base)),
                index: *index,
                span: *span,
            },
            AstExpr::Var { path, span: s } => HirExpr::Var {
                id: self.id(),
                def: self.def_at(*s),
                name: path.join("."),
                span: *s,
            },
            AstExpr::Call {
                callee,
                callee_span,
                type_args,
                args,
                span,
                ..
            } => {
                // Union variant construction (`Option.Some(args)`) recorded
                // by the resolver. It shares call syntax but is not a call.
                if let Some(use_) = self
                    .res
                    .variants
                    .get(&(callee_span.start, callee_span.end))
                    .cloned()
                {
                    return HirExpr::Variant {
                        id: self.id(),
                        union: use_.union,
                        variant: use_.variant,
                        type_args: type_args.clone(),
                        args: args.iter().map(|a| self.lower_expr(a)).collect(),
                        span: *span,
                    };
                }
                // Instance sugar (`receiver.method(args)`): rebuild the
                // receiver value path (all segments but the last) so
                // typechecking sees a real receiver expression. The resolver
                // recorded the head binding; field segments resolve by type.
                if let Some(head) = self
                    .res
                    .sugar_receivers
                    .get(&(callee_span.start, callee_span.end))
                    .cloned()
                {
                    let mut receiver = HirExpr::Var {
                        id: self.id(),
                        def: Some(head),
                        name: callee[0].clone(),
                        span: *callee_span,
                    };
                    for seg in &callee[1..callee.len() - 1] {
                        receiver = HirExpr::Field {
                            id: self.id(),
                            base: Box::new(receiver),
                            name: seg.clone(),
                            span: *callee_span,
                        };
                    }
                    return HirExpr::MethodCall {
                        id: self.id(),
                        receiver: Box::new(receiver),
                        method: callee[callee.len() - 1].clone(),
                        method_span: *callee_span,
                        type_args: type_args.clone(),
                        args: args.iter().map(|a| self.lower_expr(a)).collect(),
                        span: *span,
                    };
                }
                let def = self.def_at(*callee_span);
                let resolved = def
                    .as_ref()
                    .and_then(|d| self.res.defs.iter().find(|r| r.id == *d));
                let external = resolved.is_some_and(|r| {
                    matches!(
                        r.kind,
                        vl_semantic::DefKind::External | vl_semantic::DefKind::ImportedFunction
                    )
                });
                let extern_sig = if external {
                    resolved.and_then(|r| r.sig.clone())
                } else {
                    None
                };
                let extern_kind = if external {
                    resolved.and_then(|r| r.export_kind)
                } else {
                    None
                };
                let symbol = resolved.and_then(|r| r.symbol.clone());
                HirExpr::Call {
                    id: self.id(),
                    def,
                    external,
                    extern_sig,
                    extern_kind,
                    symbol,
                    name: callee.join("."),
                    type_args: type_args.clone(),
                    args: args.iter().map(|a| self.lower_expr(a)).collect(),
                    span: *span,
                }
            }
            AstExpr::Unary { op, rhs, span } => match op {
                // Desugar `-x` into `0 - x`.
                AstUnOp::Neg => {
                    let rhs = self.lower_expr(rhs);
                    let zero = match &rhs {
                        HirExpr::Literal { value, .. } => scalar_zero(*value),
                        _ => Scalar::Int(0),
                    };
                    let zero_span = Span::empty(span.start);
                    HirExpr::Binary {
                        id: self.id(),
                        op: HirBinOp::Sub,
                        lhs: Box::new(HirExpr::Literal {
                            id: self.id(),
                            value: zero,
                            span: zero_span,
                        }),
                        rhs: Box::new(rhs),
                        span: *span,
                    }
                }
                AstUnOp::Not => HirExpr::Unary {
                    id: self.id(),
                    op: HirUnOp::Not,
                    inner: Box::new(self.lower_expr(rhs)),
                    span: *span,
                },
            },
            AstExpr::Binary { op, lhs, rhs, span } => {
                let op = match op {
                    AstBinOp::Add => HirBinOp::Add,
                    AstBinOp::Sub => HirBinOp::Sub,
                    AstBinOp::Mul => HirBinOp::Mul,
                    AstBinOp::Div => HirBinOp::Div,
                    AstBinOp::Eq => HirBinOp::Eq,
                    AstBinOp::Ne => HirBinOp::Ne,
                    AstBinOp::Lt => HirBinOp::Lt,
                    AstBinOp::Le => HirBinOp::Le,
                    AstBinOp::Gt => HirBinOp::Gt,
                    AstBinOp::Ge => HirBinOp::Ge,
                    AstBinOp::And => HirBinOp::And,
                    AstBinOp::Or => HirBinOp::Or,
                };
                HirExpr::Binary {
                    id: self.id(),
                    op,
                    lhs: Box::new(self.lower_expr(lhs)),
                    rhs: Box::new(self.lower_expr(rhs)),
                    span: *span,
                }
            }
            AstExpr::Cast {
                inner,
                target,
                target_span,
                span,
            } => HirExpr::Cast {
                id: self.id(),
                inner: Box::new(self.lower_expr(inner)),
                target: target.clone(),
                target_span: *target_span,
                span: *span,
            },
        }
    }
}

fn scalar_zero(value: Scalar) -> Scalar {
    match value {
        Scalar::Int(_) => Scalar::Int(0),
        Scalar::U64(_) => Scalar::U64(0),
        Scalar::I64(_) => Scalar::I64(0),
        Scalar::F64(_) => Scalar::F64(0.0f64.to_bits()),
        Scalar::Bool(_) => Scalar::Bool(false),
        Scalar::U8(_) => Scalar::U8(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tuples_lower_with_index_assign_and_destructure() {
        let src = "fun main() { val t = #(1u64, \"a\"); val a = t.`0; var m = #(1u64, 2u64); m.`0 = 3u64; val #(p, q) = t; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, rdiags) = vl_semantic::resolve(&prog);
        assert!(rdiags.iter().all(|d| !d.is_error()), "{rdiags:?}");
        let hir = lower(&prog, &res);
        match &hir.items[0] {
            HirItem::Fn { body, .. } => {
                assert!(
                    matches!(&body[0], HirStmt::Let { value: HirExpr::TupleLiteral { elems, .. }, .. } if elems.len() == 2)
                );
                assert!(matches!(
                    &body[1],
                    HirStmt::Let {
                        value: HirExpr::TupleIndex { index: 0, .. },
                        ..
                    }
                ));
                assert!(matches!(&body[3], HirStmt::TupleAssign { index: 0, .. }));
                assert!(
                    matches!(&body[4], HirStmt::Destructure { bindings, .. } if bindings.len() == 2)
                );
            }
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn not_lowers_to_explicit_unary() {
        let (toks, _) = vl_lex::lex("fun main() { !true; }");
        let (prog, _) = vl_syntax::parse(&toks, "");
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = lower(&prog, &res);
        match &hir.items[0] {
            HirItem::Fn { body, .. } => {
                assert!(matches!(&body[0], HirStmt::Expr(HirExpr::Unary { .. })))
            }
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn arrays_lower() {
        let src = "fun main() { var a = [1u64, 2u64]; a[0u64] = 3u64; val x = a[1u64]; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, rdiags) = vl_semantic::resolve(&prog);
        assert!(rdiags.iter().all(|d| !d.is_error()), "{rdiags:?}");
        let hir = lower(&prog, &res);
        match &hir.items[0] {
            HirItem::Fn { body, .. } => {
                assert!(matches!(&body[0], HirStmt::Let { .. }));
                assert!(matches!(&body[1], HirStmt::IndexAssign { .. }));
                assert!(matches!(&body[2], HirStmt::Let { .. }));
            }
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn generics_plumb_type_params_and_args() {
        let src = "fun first[T](a: Array[T]): T { return a[0u64]; } fun main() { first([1u64]); first::[u64]([2u64]); }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, rdiags) = vl_semantic::resolve(&prog);
        assert!(rdiags.iter().all(|d| !d.is_error()), "{rdiags:?}");
        let hir = lower(&prog, &res);
        match &hir.items[0] {
            HirItem::Fn {
                type_params,
                params,
                ret,
                ..
            } => {
                assert_eq!(
                    type_params
                        .iter()
                        .map(|p| p.name.clone())
                        .collect::<Vec<_>>(),
                    vec!["T".to_string()]
                );
                assert!(type_params[0].bound.is_none());
                assert!(matches!(params[0].2, Some(VlType::Array(_))));
                assert!(matches!(ret, Some(VlType::Param(_))));
            }
            other => panic!("expected fn, got {other:?}"),
        }
        match &hir.items[1] {
            HirItem::Fn { body, .. } => {
                match &body[0] {
                    HirStmt::Expr(HirExpr::Call { type_args, .. }) => {
                        assert!(type_args.is_empty());
                    }
                    other => panic!("expected inferred call, got {other:?}"),
                }
                match &body[1] {
                    HirStmt::Expr(HirExpr::Call { type_args, .. }) => {
                        assert_eq!(type_args.len(), 1);
                    }
                    other => panic!("expected turbofish call, got {other:?}"),
                }
            }
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn annotated_lets_carry_their_types() {
        let src = "val scores: Array[u64] = Array.new::[u64](3); fun main() { val n: u64 = 1; n; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, rdiags) = vl_semantic::resolve(&prog);
        assert!(rdiags.iter().all(|d| !d.is_error()), "{rdiags:?}");
        let hir = lower(&prog, &res);
        match &hir.items[0] {
            HirItem::Let { ty, ty_span, .. } => {
                assert!(matches!(ty, Some(VlType::Array(_))));
                assert!(ty_span.is_some());
            }
            other => panic!("expected val, got {other:?}"),
        }
        match &hir.items[1] {
            HirItem::Fn { body, .. } => match &body[0] {
                HirStmt::Let { ty, .. } => assert_eq!(*ty, Some(VlType::U64)),
                other => panic!("expected val, got {other:?}"),
            },
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn assign_links_the_resolved_binding() {
        let src = "fun main() { var x = 1; x = 2; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        let (res, rdiags) = vl_semantic::resolve(&prog);
        assert!(rdiags.iter().all(|d| !d.is_error()), "{rdiags:?}");
        let hir = lower(&prog, &res);
        match &hir.items[0] {
            HirItem::Fn { body, .. } => {
                assert!(matches!(&body[1], HirStmt::Assign { def: Some(_), .. }))
            }
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn while_and_break_lower() {
        let src = "fun main() { while (true) { break; } }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        let (res, rdiags) = vl_semantic::resolve(&prog);
        assert!(rdiags.iter().all(|d| !d.is_error()), "{rdiags:?}");
        let hir = lower(&prog, &res);
        match &hir.items[0] {
            HirItem::Fn { body, .. } => match &body[0] {
                HirStmt::While { body, .. } => {
                    assert!(matches!(body[0], HirStmt::Break { .. }))
                }
                other => panic!("expected while, got {other:?}"),
            },
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn neg_desugars_to_sub() {
        let (toks, _) = vl_lex::lex("val x = -1;");
        let (prog, _) = vl_syntax::parse(&toks, "val x = -1;");
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = lower(&prog, &res);
        assert_eq!(hir.items.len(), 1);
        assert!(matches!(hir.items[0], HirItem::Let { .. }));
    }

    #[test]
    fn call_links_callee_def() {
        let src = "fun add(a: i64, b: i64): i64 { return a + b; } fun main() { add(1, 2); }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = lower(&prog, &res);
        assert_eq!(hir.items.len(), 2);
        match &hir.items[1] {
            HirItem::Fn { body, .. } => match &body[0] {
                HirStmt::Expr(HirExpr::Call {
                    name, args, def, ..
                }) => {
                    assert_eq!(name, "add");
                    assert_eq!(args.len(), 2);
                    assert!(def.is_some());
                }
                other => panic!("expected call, got {other:?}"),
            },
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn objects_lower_with_field_reads_and_writes() {
        let src = "type Counter = object { value: u64, }; fun main() { var c = Counter { value = 1 }; c.value = c.value + 1; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, rdiags) = vl_semantic::resolve(&prog);
        assert!(rdiags.is_empty(), "{rdiags:?}");
        let hir = lower(&prog, &res);
        assert!(matches!(hir.items[0], HirItem::Object { .. }));
        assert!(matches!(
            hir.items[1],
            HirItem::Fn { ref body, .. }
                if body.iter().any(|stmt| matches!(stmt, HirStmt::FieldAssign { .. }))
        ));
    }

    #[test]
    fn unions_lower_with_variant_and_payload_spans_intact() {
        let src = "type U[T] = union { Some(T, u64), };";
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, rdiags) = vl_semantic::resolve(&prog);
        assert!(rdiags.is_empty(), "{rdiags:?}");
        let hir = lower(&prog, &res);
        let ast_variant = match &prog.items[0] {
            vl_syntax::Item::Union { variants, .. } => &variants[0],
            other => panic!("expected AST union, got {other:?}"),
        };
        match &hir.items[0] {
            HirItem::Union { variants, .. } => {
                assert_eq!(variants[0].name, ast_variant.name);
                assert_eq!(variants[0].name_span, ast_variant.name_span);
                assert_eq!(variants[0].payload, ast_variant.payload);
            }
            other => panic!("expected HIR union, got {other:?}"),
        }
    }

    #[test]
    fn variant_construction_lowers_to_variant_nodes() {
        let src = "type Option[T] = union { None, Some(T), }; fun main() { val a = Option.Some(1u64); val n = Option.None; a; n; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, rdiags) = vl_semantic::resolve(&prog);
        assert!(rdiags.iter().all(|d| !d.is_error()), "{rdiags:?}");
        let hir = lower(&prog, &res);
        match &hir.items[1] {
            HirItem::Fn { body, .. } => {
                match &body[0] {
                    HirStmt::Let { value, .. } => match value {
                        HirExpr::Variant {
                            union,
                            variant,
                            args,
                            ..
                        } => {
                            assert_eq!(union, "Option");
                            assert_eq!(variant, "Some");
                            assert_eq!(args.len(), 1);
                        }
                        other => panic!("expected variant, got {other:?}"),
                    },
                    other => panic!("expected let, got {other:?}"),
                }
                match &body[1] {
                    HirStmt::Let { value, .. } => {
                        assert!(
                            matches!(
                                value,
                                HirExpr::Variant { args, .. } if args.is_empty()
                            ),
                            "expected nullary variant, got {value:?}"
                        );
                    }
                    other => panic!("expected let, got {other:?}"),
                }
            }
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn match_lowers_with_arm_bindings() {
        let src = "type U = union { A(u64), B, }; fun main() { val u = U.B; match (u) { U.A(v) { v; } else { 0u64; } } }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, rdiags) = vl_semantic::resolve(&prog);
        assert!(rdiags.iter().all(|d| !d.is_error()), "{rdiags:?}");
        let hir = lower(&prog, &res);
        match &hir.items[1] {
            HirItem::Fn { body, .. } => match &body[1] {
                HirStmt::Match {
                    arms, else_body, ..
                } => {
                    assert_eq!(arms.len(), 1);
                    assert_eq!(arms[0].union, "U");
                    assert_eq!(arms[0].variant, "A");
                    assert_eq!(arms[0].bindings.len(), 1);
                    assert_eq!(arms[0].bindings[0].binding, "v");
                    assert!(arms[0].bindings[0].def.is_some());
                    assert!(else_body.is_some());
                }
                other => panic!("expected match, got {other:?}"),
            },
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn null_lowers_to_a_null_node() {
        let src = "fun main() { val x: ?u64 = null; x; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, rdiags) = vl_semantic::resolve(&prog);
        assert!(rdiags.iter().all(|d| !d.is_error()), "{rdiags:?}");
        let hir = lower(&prog, &res);
        match &hir.items[0] {
            HirItem::Fn { body, .. } => match &body[0] {
                HirStmt::Let { value, ty, .. } => {
                    assert!(matches!(value, HirExpr::Null { .. }));
                    assert!(matches!(ty, Some(VlType::Nullable(_))));
                }
                other => panic!("expected let, got {other:?}"),
            },
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn null_match_arm_desugars_to_option_none() {
        let src =
            "fun main() { val x: ?u64 = null; match (x) { Option.Some(v) { v; } null { 0u64; } } }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, rdiags) = vl_semantic::resolve(&prog);
        assert!(rdiags.iter().all(|d| !d.is_error()), "{rdiags:?}");
        let hir = lower(&prog, &res);
        match &hir.items[0] {
            HirItem::Fn { body, .. } => match &body[1] {
                HirStmt::Match { arms, .. } => {
                    assert_eq!(arms.len(), 2);
                    assert_eq!(arms[0].union, "Option");
                    assert_eq!(arms[0].variant, "Some");
                    assert_eq!(arms[1].union, "Option");
                    assert_eq!(arms[1].variant, "None");
                    assert!(arms[1].bindings.is_empty());
                }
                other => panic!("expected match, got {other:?}"),
            },
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn return_lowers_with_value() {
        let src = "fun f(): i64 { return 1; } fun m() { return; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, rdiags) = vl_semantic::resolve(&prog);
        assert!(rdiags.iter().all(|d| !d.is_error()), "{rdiags:?}");
        let hir = lower(&prog, &res);
        match &hir.items[0] {
            HirItem::Fn { body, .. } => {
                assert!(matches!(&body[0], HirStmt::Return { value: Some(_), .. }))
            }
            other => panic!("expected fn, got {other:?}"),
        }
        match &hir.items[1] {
            HirItem::Fn { body, .. } => {
                assert!(matches!(&body[0], HirStmt::Return { value: None, .. }))
            }
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn associated_functions_lower_to_namespaced_fns() {
        let src = "type Counter = object { value: u64, fun bump(self: *Counter): *Counter { return self; }, }; fun main() { var c: *Counter = Counter { value = 1 }; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, rdiags) = vl_semantic::resolve(&prog);
        assert!(rdiags.iter().all(|d| !d.is_error()), "{rdiags:?}");
        let hir = lower(&prog, &res);
        assert_eq!(hir.items.len(), 3);
        assert!(matches!(hir.items[0], HirItem::Object { .. }));
        match &hir.items[1] {
            HirItem::Fn {
                name, def, params, ..
            } => {
                assert_eq!(name, "Counter.bump");
                assert!(def.is_some());
                assert_eq!(params.len(), 1);
            }
            other => panic!("expected method fn, got {other:?}"),
        }
        assert!(matches!(&hir.items[2], HirItem::Fn { name, .. } if name == "main"));
    }

    #[test]
    fn sugar_call_lowers_to_method_call_with_receiver() {
        let src = "type C = object { value: u64, fun f(self: C): u64 { return self.value; }, }; fun main() { var c: *C = C { value = 1 }; c.f(); }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, rdiags) = vl_semantic::resolve(&prog);
        assert!(rdiags.iter().all(|d| !d.is_error()), "{rdiags:?}");
        let hir = lower(&prog, &res);
        match hir.items.last().expect("main") {
            HirItem::Fn { body, .. } => match body.last().expect("call") {
                HirStmt::Expr(HirExpr::MethodCall {
                    receiver,
                    method,
                    args,
                    ..
                }) => {
                    assert_eq!(method, "f");
                    assert!(args.is_empty());
                    assert!(
                        matches!(&**receiver, HirExpr::Var { def: Some(_), name, .. } if name == "c")
                    );
                }
                other => panic!("expected method call, got {other:?}"),
            },
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn unresolved_call_poisoned_not_panic() {
        let src = "fun main() { nope(1); }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = lower(&prog, &res);
        match &hir.items[0] {
            HirItem::Fn { body, .. } => assert!(matches!(
                &body[0],
                HirStmt::Expr(HirExpr::Call { def: None, .. })
            )),
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn casts_and_bounds_lower() {
        let src = "fun add[T extends Numeric](a: T, b: T): T { val c = a as u64; return a + b; } fun main() { add(1u64, 2u64); }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, rdiags) = vl_semantic::resolve(&prog);
        assert!(rdiags.iter().all(|d| !d.is_error()), "{rdiags:?}");
        let hir = lower(&prog, &res);
        match &hir.items[0] {
            HirItem::Fn {
                type_params, body, ..
            } => {
                assert_eq!(type_params.len(), 1);
                assert_eq!(type_params[0].bound, Some(vl_common::GenericBound::Numeric));
                assert!(matches!(
                    &body[0],
                    HirStmt::Let {
                        value: HirExpr::Cast { .. },
                        ..
                    }
                ));
            }
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn mutable_types_survive_lowering() {
        let src = "type Child = object { value: u64, }; type Parent = object { child: *Child, children: *Array[*Child], }; fun edit(parent: *Parent): *Parent { val x: *Child = parent.child; x; return parent; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, rdiags) = vl_semantic::resolve(&prog);
        assert!(rdiags.iter().all(|d| !d.is_error()), "{rdiags:?}");
        let hir = lower(&prog, &res);
        match &hir.items[1] {
            HirItem::Object { fields, .. } => {
                assert_eq!(
                    fields[0].1,
                    Some(VlType::Mutable(Box::new(VlType::Object("Child".into()))))
                );
                assert_eq!(
                    fields[1].1,
                    Some(VlType::Mutable(Box::new(VlType::Array(Box::new(
                        VlType::Mutable(Box::new(VlType::Object("Child".into())))
                    )))))
                );
            }
            other => panic!("expected object, got {other:?}"),
        }
        match &hir.items[2] {
            HirItem::Fn {
                params, ret, body, ..
            } => {
                assert_eq!(
                    params[0].2,
                    Some(VlType::Mutable(Box::new(VlType::Object("Parent".into()))))
                );
                assert_eq!(
                    *ret,
                    Some(VlType::Mutable(Box::new(VlType::Object("Parent".into()))))
                );
                // A binding introduces a fresh local; its initializer reads `parent`.
                let param_def = params[0].1.clone().expect("param def");
                let param = res.defs.iter().find(|d| d.id == param_def).expect("param");
                assert_eq!(param.kind, vl_semantic::DefKind::Parameter);
                assert_eq!(param.name, "parent");
                match &body[0] {
                    HirStmt::Let { ty, def, value, .. } => {
                        assert_eq!(
                            *ty,
                            Some(VlType::Mutable(Box::new(VlType::Object("Child".into()))))
                        );
                        let local_def = def.clone().expect("val def must survive");
                        let local = res
                            .defs
                            .iter()
                            .find(|d| d.id == local_def)
                            .expect("local def");
                        assert_eq!(local.kind, vl_semantic::DefKind::Local);
                        assert_eq!(local.name, "x");
                        // Initializer `parent.child` reads through the param.
                        match value {
                            HirExpr::Field { base, name, .. } => {
                                assert_eq!(name, "child");
                                match &**base {
                                    HirExpr::Var { def, name, .. } => {
                                        assert_eq!(name, "parent");
                                        assert_eq!(*def, Some(param_def));
                                    }
                                    other => panic!("expected parent var, got {other:?}"),
                                }
                            }
                            other => panic!("expected field read, got {other:?}"),
                        }
                    }
                    other => panic!("expected val, got {other:?}"),
                }
                // `x;` reads the local, `return parent;` reads the parameter.
                match &body[1] {
                    HirStmt::Expr(HirExpr::Var { def, name, .. }) => {
                        assert_eq!(name, "x");
                        assert!(def.is_some());
                    }
                    other => panic!("expected x read, got {other:?}"),
                }
            }
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn mutable_call_type_args_and_assign_kinds_survive() {
        let src = "type Foo = object { value: u64, }; fun id[T](x: T): T { return x; } fun main() { val base = Foo { value = 1u64 }; var e = id::[*Foo](base); var arr = [1u64]; val c = base as Foo; e = base; e.value = 1u64; arr[0u64] = 2u64; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, rdiags) = vl_semantic::resolve(&prog);
        assert!(rdiags.iter().all(|d| !d.is_error()), "{rdiags:?}");
        let hir = lower(&prog, &res);
        match &hir.items[2] {
            HirItem::Fn { body, .. } => {
                // `var e = id::[*Foo](base)`: mutable turbofish survives.
                match &body[1] {
                    HirStmt::Let { def, value, .. } => {
                        let let_def = def.clone().expect("e def");
                        match value {
                            HirExpr::Call {
                                type_args,
                                args,
                                def: callee,
                                ..
                            } => {
                                assert_eq!(
                                    *type_args,
                                    vec![VlType::Mutable(Box::new(VlType::Object("Foo".into())))]
                                );
                                assert!(callee.is_some());
                                match &args[0] {
                                    HirExpr::Var { def, name, .. } => {
                                        assert_eq!(name, "base");
                                        assert!(def.is_some());
                                        // Argument refers to the earlier `base` local.
                                        match &body[0] {
                                            HirStmt::Let { def: base_def, .. } => {
                                                assert_eq!(*def, *base_def);
                                            }
                                            other => {
                                                panic!("expected base val, got {other:?}")
                                            }
                                        }
                                    }
                                    other => panic!("expected base var, got {other:?}"),
                                }
                            }
                            other => panic!("expected call, got {other:?}"),
                        }
                        // `e = base` targets the `e` local exactly.
                        match &body[4] {
                            HirStmt::Assign { def: target, .. } => {
                                assert_eq!(*target, Some(let_def));
                            }
                            other => panic!("expected assign, got {other:?}"),
                        }
                    }
                    other => panic!("expected val e, got {other:?}"),
                }
                // `val c = base as Foo` preserves the cast target exactly.
                match &body[3] {
                    HirStmt::Let { value, .. } => match value {
                        HirExpr::Cast { target, .. } => {
                            assert_eq!(*target, VlType::Object("Foo".into()));
                        }
                        other => panic!("expected cast, got {other:?}"),
                    },
                    other => panic!("expected val c, got {other:?}"),
                }
                assert!(matches!(&body[4], HirStmt::Assign { .. }));
                assert!(matches!(&body[5], HirStmt::FieldAssign { .. }));
                assert!(matches!(&body[6], HirStmt::IndexAssign { .. }));
            }
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn mutable_cast_and_top_let_targets_survive() {
        let src = "type Foo = object { value: u64, }; val g: *Foo = Foo { value = 1u64 }; fun main() { val c = g as Foo; c; }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, pdiags) = vl_syntax::parse(&toks, src);
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, rdiags) = vl_semantic::resolve(&prog);
        assert!(rdiags.iter().all(|d| !d.is_error()), "{rdiags:?}");
        let hir = lower(&prog, &res);
        match &hir.items[1] {
            HirItem::Let { ty, def, .. } => {
                assert_eq!(
                    *ty,
                    Some(VlType::Mutable(Box::new(VlType::Object("Foo".into()))))
                );
                assert!(def.is_some());
            }
            other => panic!("expected top val, got {other:?}"),
        }
    }

    #[test]
    fn imported_generic_preserves_sig_linkage_and_symbol() {
        use vl_common::{Export, FuncSig, ParamSig, TypeParamSig};
        let sig = FuncSig::generic(
            vec![TypeParamSig {
                name: "T".into(),
                bound: None,
            }],
            vec![ParamSig {
                name: "value".into(),
                ty: VlType::Param("T".into()),
            }],
            VlType::Param("T".into()),
        );
        let module = vl_common::ModuleSpec::new_source(
            &["demo", "lib"],
            vec![Export::source("id".into(), sig.clone())],
        );
        let (toks, _) = vl_lex::lex("use demo.lib.id; fun main() { id(1u64); }");
        let (prog, pdiags) = vl_syntax::parse(&toks, "");
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, rdiags) = vl_semantic::resolve_with_modules(&prog, &[module]);
        assert!(rdiags.iter().all(|d| !d.is_error()), "{rdiags:?}");
        let hir = lower(&prog, &res);
        match &hir.items[0] {
            HirItem::Fn { body, .. } => match &body[0] {
                HirStmt::Expr(value) => match value {
                    HirExpr::Call {
                        extern_sig,
                        extern_kind,
                        symbol,
                        ..
                    } => {
                        let sig = extern_sig.as_ref().expect("generic sig");
                        assert_eq!(sig.type_params.len(), 1);
                        assert_eq!(*extern_kind, Some(vl_common::ExportKind::Source));
                        let sym = symbol.as_ref().expect("symbol");
                        assert_eq!(sym.module.as_string(), "demo.lib");
                        assert_eq!(sym.name, "id");
                    }
                    other => panic!("expected call, got {other:?}"),
                },
                other => panic!("expected expr, got {other:?}"),
            },
            other => panic!("expected fn, got {other:?}"),
        }
    }

    #[test]
    fn unresolved_generic_provider_stays_poisoned() {
        let (toks, _) = vl_lex::lex("use demo.missing.id; fun main() { id(1u64); }");
        let (prog, _) = vl_syntax::parse(&toks, "");
        let (res, _) = vl_semantic::resolve_with_modules(&prog, &[]);
        let hir = lower(&prog, &res);
        match &hir.items[0] {
            HirItem::Fn { body, .. } => match &body[0] {
                HirStmt::Expr(value) => match value {
                    HirExpr::Call { def, .. } => {
                        // Poisoned import resolves to a def without sig/symbol.
                        assert!(def.is_some());
                    }
                    other => panic!("expected call, got {other:?}"),
                },
                other => panic!("expected expr, got {other:?}"),
            },
            other => panic!("expected fn, got {other:?}"),
        }
    }
}
