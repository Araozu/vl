//! Item rendering: `use`, objects, unions, error sets, functions.

use super::{indent, Fmt};
use vl_common::Span;
use vl_syntax::{AssociatedFn, BindingKind, DestructureBinding, Item, Param, TypeParam};

pub(crate) fn kind_text(kind: &BindingKind) -> &'static str {
    match kind {
        BindingKind::Var => "var",
        BindingKind::Val => "val",
    }
}

pub(crate) fn param_start(p: &Param) -> usize {
    p.name_span.start
}

pub(crate) fn param_end(p: &Param) -> usize {
    p.ty_span.map_or(p.name_span.end, |s| s.end)
}

impl<'a> Fmt<'a> {
    pub(crate) fn program(&mut self, prog: &vl_syntax::Program) {
        for (i, item) in prog.items.iter().enumerate() {
            self.item(item, 0, i == 0);
        }
    }

    fn item_span(item: &Item) -> Span {
        match item {
            Item::Use { span, .. }
            | Item::Object { span, .. }
            | Item::Union { span, .. }
            | Item::Error { span, .. }
            | Item::Let { span, .. }
            | Item::Destructure { span, .. }
            | Item::Function { span, .. } => *span,
        }
    }

    pub(crate) fn item(&mut self, item: &Item, level: usize, first: bool) {
        let span = Self::item_span(item);
        self.lead(span.start, level, first);
        self.out.push_str(&indent(level));
        match item {
            Item::Use { path, names, .. } => self.use_item(path, names, span, level),
            Item::Object {
                name,
                type_params,
                fields,
                methods,
                ..
            } => self.object_decl(name, type_params, fields, methods, span, level),
            Item::Union {
                name,
                type_params,
                variants,
                methods,
                ..
            } => {
                if methods.is_empty() {
                    let heads: Vec<(String, usize)> = variants
                        .iter()
                        .map(|v| {
                            let end = v.payload.last().map_or(v.name_span.end, |(_, s)| s.end);
                            (variant_head(&v.name, &v.payload), end)
                        })
                        .collect();
                    self.nominal_decl(name, Some(type_params), "union", &heads, span, level);
                } else {
                    self.union_decl(name, type_params, variants, methods, span, level);
                }
            }
            Item::Error { name, variants, .. } => {
                let heads: Vec<(String, usize)> = variants
                    .iter()
                    .map(|v| {
                        let end = v.payload.last().map_or(v.name_span.end, |(_, s)| s.end);
                        (variant_head(&v.name, &v.payload), end)
                    })
                    .collect();
                self.nominal_decl(name, None, "error", &heads, span, level);
            }
            Item::Let {
                kind,
                name,
                ty,
                value,
                ..
            } => {
                self.out.push_str(&format!("{} {name}", kind_text(kind)));
                if let Some(ty) = ty {
                    self.out.push_str(&format!(": {ty}"));
                }
                self.out.push_str(" = ");
                self.finish_value(value, level);
                self.out.push(';');
            }
            Item::Destructure {
                kind,
                bindings,
                ty,
                value,
                ..
            } => {
                self.out.push_str(kind_text(kind));
                self.out.push(' ');
                self.emit_pattern(bindings, level);
                if let Some(ty) = ty {
                    self.out.push_str(&format!(": {ty}"));
                }
                self.out.push_str(" = ");
                self.finish_value(value, level);
                self.out.push(';');
            }
            Item::Function {
                name,
                name_span,
                type_params,
                params,
                ret,
                ret_span,
                body,
                ..
            } => self.function(
                name,
                *name_span,
                type_params,
                params,
                ret,
                *ret_span,
                body,
                span,
                level,
            ),
        }
        if let Some(comment) = self.trailing(span.end) {
            self.out.push(' ');
            self.out.push_str(&comment);
        }
        self.out.push('\n');
    }

    fn use_item(&mut self, path: &[String], names: &Option<Vec<String>>, span: Span, level: usize) {
        let dotted = path.join(".");
        let Some(names) = names else {
            self.out.push_str(&format!("use {dotted};"));
            return;
        };
        if names.is_empty() {
            self.out.push_str(&format!("use {dotted};"));
            return;
        }
        // `Item::Use` keeps no name spans; detect the trailing comma by
        // scanning backwards from `;` past the closing `}`.
        let trailing = {
            let body = self.slice(span.start, span.end);
            match body.rfind('}') {
                Some(close) => {
                    let mut before = &body[..close];
                    // Drop a trailing `//` comment on the last line.
                    if let Some(nl) = before.rfind('\n') {
                        if let Some(comment) = before[nl..].find("//") {
                            before = &before[..nl + comment];
                        }
                    }
                    before
                        .trim_end_matches([' ', '\t', '\r', '\n'])
                        .ends_with(',')
                }
                None => false,
            }
        };
        let commented = self.has_comment_in(span.start, span.end);
        if !trailing && !commented {
            self.out
                .push_str(&format!("use {dotted}.{{{}}};", names.join(", ")));
            return;
        }
        // Span-less names: hoist any inner comments ahead of the list.
        self.out.push_str(&format!("use {dotted}.{{"));
        self.out.push('\n');
        self.lead(span.end, level + 1, true);
        for name in names {
            self.out.push_str(&indent(level + 1));
            self.out.push_str(name);
            self.out.push(',');
            self.out.push('\n');
        }
        self.out.push_str(&indent(level));
        self.out.push_str("};");
    }

    /// Shared `union` / `error` body: `type Name[params] = union|error {...};`.
    /// `heads` holds one pre-rendered `Name` / `Name(T, U)` head plus its
    /// source end per variant, in source order.
    fn nominal_decl(
        &mut self,
        name: &str,
        type_params: Option<&[TypeParam]>,
        keyword: &str,
        heads: &[(String, usize)],
        span: Span,
        level: usize,
    ) {
        self.out.push_str(&format!("type {name}"));
        if let Some(params) = type_params {
            self.emit_type_params(params, level);
        }
        self.out.push_str(&format!(" = {keyword} "));
        if heads.is_empty() {
            let open = self.find_open_brace(span.start);
            let close = self.matching_close(open);
            self.out.push('{');
            self.lead(close, level + 1, true);
            self.out.push_str("};");
            return;
        }
        let last_end = heads.last().map_or(span.start, |(_, end)| *end);
        let trailing = self.has_trailing_variant(last_end);
        let commented = self.has_comment_in(span.start, span.end);
        if !trailing && !commented {
            let inner = heads
                .iter()
                .map(|(head, _)| head.clone())
                .collect::<Vec<_>>()
                .join(", ");
            self.out.push_str(&format!("{{ {inner} }};"));
            return;
        }
        let open = self.find_open_brace(span.start);
        let close = self.matching_close(open);
        self.lead(open, level + 1, true);
        self.out.push_str("{\n");
        // Flush comments ahead of each variant in source order. Pre-rendered
        // heads carry no spans, so each variant's end bounds the search.
        for (head, end) in heads {
            self.lead(*end, level + 1, true);
            self.out.push_str(&indent(level + 1));
            self.out.push_str(head);
            self.out.push(',');
            if let Some(comment) = self.trailing(*end) {
                self.out.push(' ');
                self.out.push_str(&comment);
            }
            self.out.push('\n');
        }
        self.lead(close, level + 1, true);
        self.out.push_str(&indent(level));
        self.out.push_str("};");
    }

    fn object_decl(
        &mut self,
        name: &str,
        type_params: &[TypeParam],
        fields: &[vl_syntax::ObjectField],
        methods: &[AssociatedFn],
        span: Span,
        level: usize,
    ) {
        self.out.push_str(&format!("type {name}"));
        if !type_params.is_empty() {
            self.emit_type_params(type_params, level);
        }
        self.out.push_str(" = object ");
        if fields.is_empty() && methods.is_empty() {
            let open = self.find_open_brace(span.start);
            let close = self.matching_close(open);
            self.out.push('{');
            self.lead(close, level + 1, true);
            self.out.push_str("};");
            return;
        }
        // Members in source order (fields and methods interleave).
        enum Member<'x> {
            Field(&'x vl_syntax::ObjectField),
            Method(&'x AssociatedFn),
        }
        let mut members: Vec<(usize, usize, Member)> = Vec::new();
        for f in fields {
            let end = f.ty_span.map_or(f.name_span.end, |s| s.end);
            members.push((f.name_span.start, end, Member::Field(f)));
        }
        for m in methods {
            members.push((m.span.start, m.span.end, Member::Method(m)));
        }
        members.sort_by_key(|(start, _, _)| *start);
        let last_end = members
            .iter()
            .map(|(_, end, _)| *end)
            .max()
            .unwrap_or(span.start);
        let trailing = self.has_trailing(last_end, b'}');
        let commented = self.has_comment_in(span.start, span.end);
        let single = methods.is_empty() && !trailing && !commented;
        if single {
            let inner = members
                .iter()
                .map(|(_, _, m)| match m {
                    Member::Field(f) => match &f.ty {
                        Some(ty) => format!("{}: {ty}", f.name),
                        None => f.name.clone(),
                    },
                    Member::Method(_) => unreachable!("methods force multiline"),
                })
                .collect::<Vec<_>>()
                .join(", ");
            self.out.push_str(&format!("{{ {inner} }};"));
            return;
        }
        let open = self.find_open_brace(span.start);
        let close = self.matching_close(open);
        self.lead(open, level + 1, true);
        self.out.push_str("{\n");
        for (i, (start, end, member)) in members.iter().enumerate() {
            self.lead(*start, level + 1, i == 0);
            self.out.push_str(&indent(level + 1));
            match member {
                Member::Field(f) => {
                    match &f.ty {
                        Some(ty) => self.out.push_str(&format!("{}: {ty}", f.name)),
                        None => self.out.push_str(&f.name),
                    }
                    self.out.push(',');
                }
                Member::Method(m) => {
                    self.method(m, level + 1);
                    self.out.push(',');
                }
            }
            if let Some(comment) = self.trailing(*end) {
                self.out.push(' ');
                self.out.push_str(&comment);
            }
            self.out.push('\n');
        }
        self.lead(close, level + 1, true);
        self.out.push_str(&indent(level));
        self.out.push_str("};");
    }

    /// Union with associated functions: variants plus `fun` members in
    /// source order (like objects).
    fn union_decl(
        &mut self,
        name: &str,
        type_params: &[TypeParam],
        variants: &[vl_syntax::UnionVariant],
        methods: &[AssociatedFn],
        span: Span,
        level: usize,
    ) {
        self.out.push_str(&format!("type {name}"));
        if !type_params.is_empty() {
            self.emit_type_params(type_params, level);
        }
        self.out.push_str(" = union ");
        enum UMember<'x> {
            Variant(&'x vl_syntax::UnionVariant),
            Method(&'x AssociatedFn),
        }
        let mut members: Vec<(usize, usize, UMember)> = Vec::new();
        for v in variants {
            let end = v.payload.last().map_or(v.name_span.end, |(_, s)| s.end);
            members.push((v.name_span.start, end, UMember::Variant(v)));
        }
        for m in methods {
            members.push((m.span.start, m.span.end, UMember::Method(m)));
        }
        members.sort_by_key(|(start, _, _)| *start);
        let open = self.find_open_brace(span.start);
        let close = self.matching_close(open);
        self.lead(open, level + 1, true);
        self.out.push_str("{\n");
        for (i, (start, end, member)) in members.iter().enumerate() {
            self.lead(*start, level + 1, i == 0);
            self.out.push_str(&indent(level + 1));
            match member {
                UMember::Variant(v) => {
                    self.out.push_str(&v.name);
                    if !v.payload.is_empty() {
                        let inner = v
                            .payload
                            .iter()
                            .map(|(ty, _)| ty.to_string())
                            .collect::<Vec<_>>()
                            .join(", ");
                        self.out.push_str(&format!("({inner})"));
                    }
                    self.out.push(',');
                }
                UMember::Method(m) => {
                    self.method(m, level + 1);
                    self.out.push(',');
                }
            }
            if let Some(comment) = self.trailing(*end) {
                self.out.push(' ');
                self.out.push_str(&comment);
            }
            self.out.push('\n');
        }
        self.lead(close, level + 1, true);
        self.out.push_str(&indent(level));
        self.out.push_str("};");
    }

    fn method(&mut self, m: &AssociatedFn, level: usize) {
        self.fun_head(
            &m.name,
            &m.type_params,
            &m.params,
            &m.ret,
            m.ret_span,
            level,
        );
        self.out.push(' ');
        let anchor = header_end(m.name_span, &m.type_params, &m.params, m.ret_span);
        let open = self.find_open_brace(anchor);
        let close = self.matching_close(open);
        self.block_at(&m.body, level, open, close);
    }

    #[allow(clippy::too_many_arguments)]
    fn function(
        &mut self,
        name: &str,
        name_span: Span,
        type_params: &[TypeParam],
        params: &[Param],
        ret: &Option<vl_common::VlType>,
        ret_span: Option<Span>,
        body: &[vl_syntax::Stmt],
        span: Span,
        level: usize,
    ) {
        self.out.push_str(&format!("fun {name}"));
        self.emit_type_params(type_params, level);
        self.emit_params(params, level);
        let show_ret = !(matches!(ret, Some(ty) if ty.is_void()) && ret_span.is_none());
        if show_ret {
            if let Some(ret) = ret {
                self.out.push_str(&format!(": {ret}"));
            }
        }
        self.out.push(' ');
        let anchor = header_end(name_span, type_params, params, ret_span).max(span.start);
        let open = self.find_open_brace(anchor);
        let close = self.matching_close(open);
        self.block_at(body, level, open, close);
    }

    fn fun_head(
        &mut self,
        name: &str,
        type_params: &[TypeParam],
        params: &[Param],
        ret: &Option<vl_common::VlType>,
        ret_span: Option<Span>,
        level: usize,
    ) {
        self.out.push_str(&format!("fun {name}"));
        self.emit_type_params(type_params, level);
        self.emit_params(params, level);
        let show_ret = !(matches!(ret, Some(ty) if ty.is_void()) && ret_span.is_none());
        if show_ret {
            if let Some(ret) = ret {
                self.out.push_str(&format!(": {ret}"));
            }
        }
    }

    /// Emit `[...]` type parameters directly (multiline when steered).
    fn emit_type_params(&mut self, params: &[TypeParam], level: usize) {
        if params.is_empty() {
            return;
        }
        let last_end = params.last().map_or(0, |p| p.span.end);
        let trailing = self.has_trailing(last_end, b']');
        let commented = params
            .iter()
            .any(|p| self.has_comment_in(p.span.start, p.span.end));
        if !trailing && !commented {
            self.out.push_str(&format!(
                "[{}]",
                params
                    .iter()
                    .map(|p| match p.bound {
                        Some(bound) => format!("{} extends {bound}", p.name),
                        None => p.name.clone(),
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            return;
        }
        self.out.push_str("[\n");
        for p in params {
            self.lead(p.span.start, level + 1, true);
            self.out.push_str(&indent(level + 1));
            match p.bound {
                Some(bound) => self.out.push_str(&format!("{} extends {bound}", p.name)),
                None => self.out.push_str(&p.name),
            }
            self.out.push(',');
            if let Some(comment) = self.trailing(p.span.end) {
                self.out.push(' ');
                self.out.push_str(&comment);
            }
            self.out.push('\n');
        }
        self.out.push_str(&indent(level));
        self.out.push(']');
    }

    /// Emit `(...)` parameters directly (multiline when steered).
    fn emit_params(&mut self, params: &[Param], level: usize) {
        if params.is_empty() {
            self.out.push_str("()");
            return;
        }
        let last_end = params.last().map_or(0, param_end);
        let trailing = self.has_trailing(last_end, b')');
        let commented = params
            .iter()
            .any(|p| self.has_comment_in(param_start(p), param_end(p)));
        if !trailing && !commented {
            self.out.push_str(&format!(
                "({})",
                params
                    .iter()
                    .map(|p| match &p.ty {
                        Some(ty) => format!("{}: {ty}", p.name),
                        None => p.name.clone(),
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            return;
        }
        self.out.push_str("(\n");
        for p in params {
            self.lead(param_start(p), level + 1, true);
            self.out.push_str(&indent(level + 1));
            match &p.ty {
                Some(ty) => self.out.push_str(&format!("{}: {ty}", p.name)),
                None => self.out.push_str(&p.name),
            }
            self.out.push(',');
            if let Some(comment) = self.trailing(param_end(p)) {
                self.out.push(' ');
                self.out.push_str(&comment);
            }
            self.out.push('\n');
        }
        self.out.push_str(&indent(level));
        self.out.push(')');
    }

    /// Emit a `#(...)` destructure pattern (multiline when steered).
    pub(crate) fn emit_pattern(&mut self, bindings: &[DestructureBinding], level: usize) {
        if bindings.is_empty() {
            self.out.push_str("#()");
            return;
        }
        let last_end = bindings.last().map_or(0, |b| b.binding_span.end);
        let trailing = self.has_trailing(last_end, b')');
        let commented = bindings.iter().any(|b| {
            self.has_comment_in(
                b.field_span.map_or(b.binding_span.start, |s| s.start),
                b.binding_span.end,
            )
        });
        if !trailing && !commented {
            let inner = bindings
                .iter()
                .map(|b| match &b.field {
                    Some(field) => format!("{field}: {}", b.binding),
                    None => b.binding.clone(),
                })
                .collect::<Vec<_>>()
                .join(", ");
            self.out.push_str(&format!("#({inner})"));
            return;
        }
        self.out.push_str("#(\n");
        for b in bindings {
            let start = b.field_span.map_or(b.binding_span.start, |s| s.start);
            self.lead(start, level + 1, true);
            self.out.push_str(&indent(level + 1));
            match &b.field {
                Some(field) => self.out.push_str(&format!("{field}: {}", b.binding)),
                None => self.out.push_str(&b.binding),
            }
            self.out.push(',');
            if let Some(comment) = self.trailing(b.binding_span.end) {
                self.out.push(' ');
                self.out.push_str(&comment);
            }
            self.out.push('\n');
        }
        self.out.push_str(&indent(level));
        self.out.push(')');
    }
}

/// End offset of a function header: everything before the body `{`.
fn header_end(
    name_span: Span,
    type_params: &[TypeParam],
    params: &[Param],
    ret_span: Option<Span>,
) -> usize {
    let mut end = name_span.end;
    for tp in type_params {
        end = end.max(tp.span.end);
    }
    for p in params {
        end = end.max(param_end(p));
    }
    if let Some(span) = ret_span {
        end = end.max(span.end);
    }
    end
}

/// Pre-rendered union/error variant head: `Name` or `Name(T, U)`.
/// Payload lists never take trailing commas (the parser rejects them),
/// so they always render single-line.
fn variant_head(name: &str, payload: &[(vl_common::VlType, Span)]) -> String {
    if payload.is_empty() {
        return name.to_string();
    }
    let inner = payload
        .iter()
        .map(|(ty, _)| ty.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    format!("{name}({inner})")
}
