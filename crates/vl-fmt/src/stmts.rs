//! Statement rendering: bindings, assignments, `if`/`else`, `while`,
//! `match`. `else` always starts on its own line.

use super::{indent, Fmt};
use vl_common::Span;
use vl_syntax::Stmt;

use super::items::kind_text;

pub(crate) fn stmt_start(s: &Stmt) -> usize {
    match s {
        Stmt::Let { span, .. }
        | Stmt::Assign { span, .. }
        | Stmt::IndexAssign { span, .. }
        | Stmt::FieldAssign { span, .. }
        | Stmt::TupleAssign { span, .. }
        | Stmt::Destructure { span, .. }
        | Stmt::If { span, .. }
        | Stmt::Match { span, .. }
        | Stmt::While { span, .. }
        | Stmt::Break { span }
        | Stmt::Continue { span }
        | Stmt::Return { span, .. } => span.start,
        Stmt::Expr(e) => e.span().start,
    }
}

pub(crate) fn stmt_end(s: &Stmt) -> usize {
    match s {
        Stmt::Let { span, .. }
        | Stmt::Assign { span, .. }
        | Stmt::IndexAssign { span, .. }
        | Stmt::FieldAssign { span, .. }
        | Stmt::TupleAssign { span, .. }
        | Stmt::Destructure { span, .. }
        | Stmt::If { span, .. }
        | Stmt::Match { span, .. }
        | Stmt::While { span, .. }
        | Stmt::Break { span }
        | Stmt::Continue { span }
        | Stmt::Return { span, .. } => span.end,
        Stmt::Expr(e) => e.span().end,
    }
}

impl<'a> Fmt<'a> {
    pub(crate) fn stmt(&mut self, s: &Stmt, level: usize, first: bool) {
        self.lead(stmt_start(s), level, first);
        self.out.push_str(&indent(level));
        match s {
            Stmt::Let {
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
            Stmt::Assign { name, value, .. } => {
                self.out.push_str(&format!("{name} = "));
                self.finish_value(value, level);
                self.out.push(';');
            }
            Stmt::IndexAssign {
                array,
                index,
                value,
                ..
            } => {
                self.write_expr(array, level);
                self.out.push('[');
                self.write_expr(index, level);
                self.out.push_str("] = ");
                self.finish_value(value, level);
                self.out.push(';');
            }
            Stmt::FieldAssign {
                base, field, value, ..
            } => {
                self.write_expr(base, level);
                self.out.push_str(&format!(".{field} = "));
                self.finish_value(value, level);
                self.out.push(';');
            }
            Stmt::TupleAssign {
                base, index, value, ..
            } => {
                self.write_expr(base, level);
                self.out.push_str(&format!(".`{index} = "));
                self.finish_value(value, level);
                self.out.push(';');
            }
            Stmt::Destructure {
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
            Stmt::If {
                condition,
                then_body,
                else_body,
                span,
            } => self.write_if(condition, then_body, else_body, *span, level),
            Stmt::Match {
                scrutinee,
                arms,
                else_body,
                span,
            } => self.write_match(scrutinee, arms, else_body, *span, level),
            Stmt::While {
                condition, body, ..
            } => {
                self.out.push_str("while (");
                self.write_expr(condition, level);
                self.out.push_str(") ");
                if self.is_braced(condition.span().end) {
                    let open = self.find_open_brace(condition.span().end);
                    let close = self.matching_close(open);
                    self.block_at(body, level, open, close);
                } else {
                    self.normalize_branch(body, level);
                }
            }
            Stmt::Break { .. } => self.out.push_str("break;"),
            Stmt::Continue { .. } => self.out.push_str("continue;"),
            Stmt::Return { value, .. } => match value {
                Some(v) => {
                    self.out.push_str("return ");
                    self.finish_value(v, level);
                    self.out.push(';');
                }
                None => self.out.push_str("return;"),
            },
            Stmt::Expr(e) => {
                self.finish_value(e, level);
                self.out.push(';');
            }
        }
        if let Some(comment) = self.trailing(stmt_end(s)) {
            self.out.push(' ');
            self.out.push_str(&comment);
        }
        self.out.push('\n');
    }

    /// `if (c) {...}` with `else` on its own line. An `else` holding exactly
    /// one `if` renders as an `else if` chain without extra braces.
    fn write_if(
        &mut self,
        condition: &vl_syntax::Expr,
        then_body: &[Stmt],
        else_body: &Option<Vec<Stmt>>,
        _span: Span,
        level: usize,
    ) {
        self.out.push_str("if (");
        self.write_expr(condition, level);
        self.out.push_str(") ");
        let cond_end = condition.span().end;
        let then_close = if self.is_braced(cond_end) {
            let open = self.find_open_brace(cond_end);
            let close = self.matching_close(open);
            self.block_at(then_body, level, open, close);
            Some(close)
        } else {
            self.normalize_branch(then_body, level);
            None
        };
        let Some(else_body) = else_body else {
            return;
        };
        // `else if` chain: the else holds exactly one `if`.
        if let [Stmt::If {
            condition,
            then_body,
            else_body,
            span,
        }] = else_body.as_slice()
        {
            self.out.push('\n');
            self.out.push_str(&indent(level));
            self.out.push_str("else ");
            self.write_if(condition, then_body, else_body, *span, level);
            return;
        }
        // Plain `else`: look for its `{` after the then-block.
        let search = then_close.map_or_else(
            || then_body.last().map_or(cond_end, stmt_end),
            |close| close + 1,
        );
        let open = self.find_open_brace(search);
        // Comments between the then-block and `else` stay with `else`.
        self.out.push('\n');
        self.lead(open, level, true);
        self.out.push_str(&indent(level));
        self.out.push_str("else ");
        if self.is_braced(search) {
            let close = self.matching_close(open);
            self.block_at(else_body, level, open, close);
        } else {
            self.normalize_branch(else_body, level);
        }
    }

    fn write_match(
        &mut self,
        scrutinee: &vl_syntax::Expr,
        arms: &[vl_syntax::MatchArm],
        else_body: &Option<Vec<Stmt>>,
        _span: Span,
        level: usize,
    ) {
        self.out.push_str("match (");
        self.write_expr(scrutinee, level);
        self.out.push_str(") ");
        let open = self.find_open_brace(scrutinee.span().end);
        let close = self.matching_close(open);
        // A comment between the scrutinee and `{` rides along inside.
        self.lead(open, level + 1, true);
        self.out.push_str("{\n");
        for (i, arm) in arms.iter().enumerate() {
            self.lead(arm.span.start, level + 1, i == 0);
            self.out.push_str(&indent(level + 1));
            if arm.path.len() == 1 && arm.path[0] == "null" {
                self.out.push_str("null");
            } else {
                self.out.push_str(&arm.path.join("."));
            }
            if !arm.bindings.is_empty() {
                let last_end = arm.bindings.last().map_or(0, |(_, s)| s.end);
                if self.has_trailing(last_end, b')')
                    || arm
                        .bindings
                        .iter()
                        .any(|(_, s)| self.has_comment_in(s.start, s.end))
                {
                    self.out.push_str("(\n");
                    for (name, span) in &arm.bindings {
                        self.lead(span.start, level + 2, true);
                        self.out.push_str(&indent(level + 2));
                        self.out.push_str(name);
                        self.out.push(',');
                        if let Some(comment) = self.trailing(span.end) {
                            self.out.push(' ');
                            self.out.push_str(&comment);
                        }
                        self.out.push('\n');
                    }
                    self.out.push_str(&indent(level + 1));
                    self.out.push(')');
                } else {
                    let inner = arm
                        .bindings
                        .iter()
                        .map(|(name, _)| name.clone())
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.out.push_str(&format!("({inner})"));
                }
            }
            self.out.push(' ');
            let anchor = arm
                .bindings
                .last()
                .map_or(arm.path_span.end, |(_, s)| s.end);
            if self.is_braced(anchor) {
                let body_open = self.find_open_brace(anchor);
                let body_close = self.matching_close(body_open);
                self.block_at(&arm.body, level + 1, body_open, body_close);
            } else {
                // Unreachable per the grammar (arm bodies are brace
                // blocks); normalize defensively.
                self.normalize_branch(&arm.body, level + 1);
            }
            if let Some(comment) = self.trailing(arm.span.end) {
                self.out.push(' ');
                self.out.push_str(&comment);
            }
            self.out.push('\n');
        }
        if let Some(else_body) = else_body {
            let search = arms.last().map_or(open + 1, |a| a.span.end);
            let else_open = self.find_open_brace(search);
            // No extra newline: arms (or `{`) already ended the line.
            // Comments between the last arm and `else` stay with `else`.
            self.lead(else_open, level + 1, true);
            self.out.push_str(&indent(level + 1));
            self.out.push_str("else ");
            if self.is_braced(search) {
                let else_close = self.matching_close(else_open);
                self.block_at(else_body, level + 1, else_open, else_close);
            } else {
                self.normalize_branch(else_body, level + 1);
            }
            if let Some(comment) = self.trailing(else_body.last().map_or(else_open, stmt_end)) {
                self.out.push(' ');
                self.out.push_str(&comment);
            }
            self.out.push('\n');
        }
        self.lead(close, level + 1, true);
        self.out.push_str(&indent(level));
        self.out.push('}');
    }
}
