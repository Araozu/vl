//! Expression writer: trailing-comma-steered lists plus width-driven
//! chain splitting. See the crate docs for the layout contract.

use super::{indent, single_expr, split_chain, wrap, Fmt, MAX_WIDTH};
use vl_syntax::Expr;

impl<'a> Fmt<'a> {
    /// True when `e` is a comma list that must render multiline: it has a
    /// trailing comma in the source, or it contains a comment.
    pub(crate) fn list_needs_multi(&self, e: &Expr) -> bool {
        match e {
            Expr::Call { args, span, .. } => {
                !args.is_empty()
                    && (self.has_trailing(args.last().map_or(0, |a| a.span().end), b')')
                        || self.has_comment_in(span.start, span.end))
            }
            Expr::ArrayLiteral { elems, span } => {
                !elems.is_empty()
                    && (self.has_trailing(elems.last().map_or(0, |a| a.span().end), b']')
                        || self.has_comment_in(span.start, span.end))
            }
            Expr::ObjectLiteral { fields, span, .. } => {
                !fields.is_empty()
                    && (self.has_trailing(fields.last().map_or(0, |(_, _, v)| v.span().end), b'}')
                        || self.has_comment_in(span.start, span.end))
            }
            Expr::TupleLiteral { elems, span } => {
                !elems.is_empty()
                    && (self.has_trailing(elems.last().map_or(0, |(_, _, v)| v.span().end), b')')
                        || self.has_comment_in(span.start, span.end))
            }
            _ => false,
        }
    }

    /// Write an expression at `level`, splitting comma lists when steered.
    /// Non-list nodes with comments deeper inside recurse structurally so no
    /// comment is ever dropped by single-line folding.
    pub(crate) fn write_expr(&mut self, e: &Expr, level: usize) {
        match e {
            Expr::Call {
                callee,
                type_args,
                args,
                span,
                ..
            } if !args.is_empty()
                && (self.has_trailing(args.last().map_or(0, |a| a.span().end), b')')
                    || self.has_comment_in(span.start, span.end)) =>
            {
                self.out.push_str(&callee.join("."));
                if !type_args.is_empty() {
                    let inner = type_args
                        .iter()
                        .map(|t| t.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.out.push_str(&format!("::[{inner}]"));
                }
                self.out.push_str("(\n");
                for arg in args {
                    self.lead(arg.span().start, level + 1, true);
                    self.out.push_str(&indent(level + 1));
                    self.write_expr(arg, level + 1);
                    self.out.push(',');
                    if let Some(comment) = self.trailing(arg.span().end) {
                        self.out.push(' ');
                        self.out.push_str(&comment);
                    }
                    self.out.push('\n');
                }
                self.lead(span.end, level + 1, true);
                self.out.push_str(&indent(level));
                self.out.push(')');
            }
            Expr::ArrayLiteral { elems, span } if !elems.is_empty() && self.list_needs_multi(e) => {
                self.out.push('[');
                self.out.push('\n');
                for elem in elems {
                    self.lead(elem.span().start, level + 1, true);
                    self.out.push_str(&indent(level + 1));
                    self.write_expr(elem, level + 1);
                    self.out.push(',');
                    if let Some(comment) = self.trailing(elem.span().end) {
                        self.out.push(' ');
                        self.out.push_str(&comment);
                    }
                    self.out.push('\n');
                }
                self.lead(span.end, level + 1, true);
                self.out.push_str(&indent(level));
                self.out.push(']');
            }
            Expr::ObjectLiteral {
                name,
                type_args,
                fields,
                span,
                ..
            } if !fields.is_empty() && self.list_needs_multi(e) => {
                self.out.push_str(name);
                if !type_args.is_empty() {
                    let inner = type_args
                        .iter()
                        .map(|t| t.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.out.push_str(&format!("::[{inner}]"));
                }
                self.out.push_str(" {\n");
                for (field, _, value) in fields {
                    // The field name span approximates the element start.
                    self.lead(value.span().start, level + 1, true);
                    self.out.push_str(&indent(level + 1));
                    self.out.push_str(&format!("{field} = "));
                    self.write_expr(value, level + 1);
                    self.out.push(',');
                    if let Some(comment) = self.trailing(value.span().end) {
                        self.out.push(' ');
                        self.out.push_str(&comment);
                    }
                    self.out.push('\n');
                }
                self.lead(span.end, level + 1, true);
                self.out.push_str(&indent(level));
                self.out.push('}');
            }
            Expr::TupleLiteral { elems, span } if !elems.is_empty() && self.list_needs_multi(e) => {
                self.out.push_str("#(\n");
                for (name, _, value) in elems {
                    self.lead(value.span().start, level + 1, true);
                    self.out.push_str(&indent(level + 1));
                    if let Some(n) = name {
                        self.out.push_str(&format!("{n} = "));
                    }
                    self.write_expr(value, level + 1);
                    self.out.push(',');
                    if let Some(comment) = self.trailing(value.span().end) {
                        self.out.push(' ');
                        self.out.push_str(&comment);
                    }
                    self.out.push('\n');
                }
                self.lead(span.end, level + 1, true);
                self.out.push_str(&indent(level));
                self.out.push(')');
            }
            _ if self.has_comment_in(e.span().start, e.span().end) => {
                self.write_expr_deep(e, level);
            }
            _ => self.out.push_str(&single_expr(e)),
        }
    }

    /// Structural recursion for nodes that merely contain a comment deeper
    /// inside: re-emit operators/punctuation and let [`write_expr`] handle
    /// each child, so inner lists still split and comments still flush.
    fn write_expr_deep(&mut self, e: &Expr, level: usize) {
        match e {
            Expr::Binary { op, lhs, rhs, .. } => {
                let parent = match op {
                    vl_syntax::BinOp::Or => 10,
                    vl_syntax::BinOp::And => 30,
                    vl_syntax::BinOp::Eq | vl_syntax::BinOp::Ne => 40,
                    vl_syntax::BinOp::Lt
                    | vl_syntax::BinOp::Le
                    | vl_syntax::BinOp::Gt
                    | vl_syntax::BinOp::Ge => 60,
                    vl_syntax::BinOp::Add | vl_syntax::BinOp::Sub => 70,
                    vl_syntax::BinOp::Mul | vl_syntax::BinOp::Div => 80,
                };
                let text = match op {
                    vl_syntax::BinOp::Add => "+",
                    vl_syntax::BinOp::Sub => "-",
                    vl_syntax::BinOp::Mul => "*",
                    vl_syntax::BinOp::Div => "/",
                    vl_syntax::BinOp::Eq => "==",
                    vl_syntax::BinOp::Ne => "!=",
                    vl_syntax::BinOp::Lt => "<",
                    vl_syntax::BinOp::Le => "<=",
                    vl_syntax::BinOp::Gt => ">",
                    vl_syntax::BinOp::Ge => ">=",
                    vl_syntax::BinOp::And => "&&",
                    vl_syntax::BinOp::Or => "||",
                };
                self.write_child(lhs, level, parent, true);
                self.out.push(' ');
                self.out.push_str(text);
                self.out.push(' ');
                self.write_child(rhs, level, parent, false);
            }
            Expr::Catch { lhs, fallback, .. } => {
                self.write_child(lhs, level, 20, false);
                self.out.push_str(" catch ");
                self.write_child(fallback, level, 20, true);
            }
            Expr::Unary { op, rhs, .. } => {
                self.out.push_str(match op {
                    vl_syntax::UnOp::Neg => "-",
                    vl_syntax::UnOp::Not => "!",
                });
                self.write_child(rhs, level, 90, true);
            }
            Expr::Try { inner, .. } => {
                self.out.push_str("try ");
                self.write_child(inner, level, 90, true);
            }
            Expr::Cast { inner, target, .. } => {
                self.write_child(inner, level, 50, true);
                self.out.push_str(&format!(" as {target}"));
            }
            Expr::Index { base, index, .. } => {
                self.write_child(base, level, 100, true);
                self.out.push('[');
                self.write_expr(index, level);
                self.out.push(']');
            }
            Expr::Field { base, name, .. } => {
                self.write_child(base, level, 100, true);
                self.out.push('.');
                self.out.push_str(name);
            }
            Expr::TupleIndex { base, index, .. } => {
                self.write_child(base, level, 100, true);
                self.out.push_str(&format!(".`{index}"));
            }
            Expr::Call {
                callee,
                type_args,
                args,
                ..
            } => {
                // Root list needs no split, but a child might: keep one
                // line of punctuation and recurse per argument.
                self.out.push_str(&callee.join("."));
                if !type_args.is_empty() {
                    let inner = type_args
                        .iter()
                        .map(|t| t.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.out.push_str(&format!("::[{inner}]"));
                }
                self.out.push('(');
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        self.out.push_str(", ");
                    }
                    self.write_expr(arg, level);
                }
                self.out.push(')');
            }
            Expr::ArrayLiteral { elems, .. } => {
                self.out.push('[');
                for (i, elem) in elems.iter().enumerate() {
                    if i > 0 {
                        self.out.push_str(", ");
                    }
                    self.write_expr(elem, level);
                }
                self.out.push(']');
            }
            Expr::ObjectLiteral {
                name,
                type_args,
                fields,
                ..
            } => {
                self.out.push_str(name);
                if !type_args.is_empty() {
                    let inner = type_args
                        .iter()
                        .map(|t| t.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.out.push_str(&format!("::[{inner}]"));
                }
                self.out.push_str(" { ");
                for (i, (field, _, value)) in fields.iter().enumerate() {
                    if i > 0 {
                        self.out.push_str(", ");
                    }
                    self.out.push_str(&format!("{field} = "));
                    self.write_expr(value, level);
                }
                self.out.push_str(" }");
            }
            Expr::TupleLiteral { elems, .. } => {
                self.out.push_str("#(");
                for (i, (name, _, value)) in elems.iter().enumerate() {
                    if i > 0 {
                        self.out.push_str(", ");
                    }
                    if let Some(n) = name {
                        self.out.push_str(&format!("{n} = "));
                    }
                    self.write_expr(value, level);
                }
                self.out.push(')');
            }
            _ => self.out.push_str(&single_expr(e)),
        }
    }

    fn write_child(&mut self, child: &Expr, level: usize, parent: u8, same_ok: bool) {
        let p = super::precedence(child);
        if p < parent || (p == parent && !same_ok) {
            self.out.push('(');
            self.write_expr(child, level);
            self.out.push(')');
        } else {
            self.write_expr(child, level);
        }
    }

    /// Finish a `let` / assignment / expression-statement / return value.
    /// The `prefix` (`val x = `, `return `, ...) is already in `out` on the
    /// current line; this appends the value plus width-driven chain splits.
    pub(crate) fn finish_value(&mut self, value: &Expr, level: usize) {
        if self.list_needs_multi(value) || self.has_comment_in(value.span().start, value.span().end)
        {
            self.write_expr(value, level);
            return;
        }
        let s = single_expr(value);
        let current = self
            .out
            .rsplit('\n')
            .next()
            .map_or(0, |line| line.chars().count());
        if current + s.chars().count() < MAX_WIDTH {
            self.out.push_str(&s);
            return;
        }
        if let Some((op, parts)) = split_chain(value) {
            let parent = match op {
                "&&" => 30,
                "||" => 10,
                _ => 20,
            };
            self.out.push_str(&wrap(parts[0], parent, op != "catch"));
            for (i, part) in parts[1..].iter().enumerate() {
                self.out.push('\n');
                self.out.push_str(&indent(level + 1));
                self.out.push_str(op);
                self.out.push(' ');
                let same_ok = op == "catch" && i + 1 == parts.len() - 1;
                self.out.push_str(&wrap(part, parent, same_ok));
            }
            return;
        }
        self.out.push_str(&s);
    }
}
