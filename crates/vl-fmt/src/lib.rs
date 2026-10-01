//! vl-fmt: zig-`fmt`-style code formatter for VL, non-overridable defaults.
//!
//! Canonical style (no configuration file, no flags change it):
//!
//! - 4 spaces per indent level, Unix newlines, single trailing newline.
//! - Trailing commas steer comma-list layout. Every `,`-separated list the
//!   parser accepts a trailing comma in (call args, function params, type
//!   parameters, union type arguments, array literals, tuple literals,
//!   object literals, destructure patterns, `use` braced names, object
//!   members, union/error variants, match bindings) renders multiline — one
//!   element per line with a trailing comma — when the source list had a
//!   trailing comma, and folds to a single line otherwise, even past
//!   [`MAX_WIDTH`]. Types themselves (`Array[T]`, `#(u64, String)`, `E!u64`,
//!   payload lists like `Some(T, U)`, turbofish arguments) always render
//!   single-line: they carry no spans for trailing-comma detection.
//! - `else` starts on its own line, never `} else {`. This covers `else if`
//!   chains and `match` `else` arms.
//! - Single-statement branches normalize to braced blocks (the AST does not
//!   record braced-ness, so `if (x) y;` becomes a three-line block).
//! - `//` comments and at most one blank line between items/statements are
//!   preserved. Comments inside span-less positions (type spellings,
//!   turbofish, `use` names) are hoisted to a nearby own line: content is
//!   never dropped, position is approximate.
//! - Files with lexer or parser errors are rejected with their diagnostics;
//!   the driver prints them and writes nothing.
//!
//! The formatter works from the AST plus the raw source text. Trailing
//! commas are detected by scanning the source after the last element's span
//! (the AST itself records no commas), and comments are extracted by a
//! dedicated scan because the lexer discards them.

use vl_common::{Diagnostic, Scalar, Span};
use vl_syntax::{BinOp, Expr, Program, Stmt, UnOp};

mod exprs;
mod items;
mod stmts;

#[cfg(test)]
mod tests;

/// Spaces per indent level. Not configurable.
pub const INDENT_WIDTH: usize = 4;
/// Preferred maximum line width. Comma lists ignore it by design (a missing
/// trailing comma folds to one line no matter how long); long `&&` / `||` /
/// `catch` chains in statement position break onto continuation lines.
pub const MAX_WIDTH: usize = 100;

/// Format `src` as the module `module`. Returns the canonical text, or the
/// lex/parse diagnostics when the file has errors (nothing is formatted).
pub fn format(src: &str, module: &str) -> Result<String, Vec<Diagnostic>> {
    let (toks, mut diags) = vl_lex::lex(src);
    let (prog, mut parsed) = vl_syntax::parse_with_module(&toks, src, module);
    diags.append(&mut parsed);
    if diags.iter().any(|d| d.is_error()) {
        return Err(diags);
    }
    Ok(render(&prog, src))
}

/// Render an already-checked program.
fn render(prog: &Program, src: &str) -> String {
    let mut f = Fmt {
        src,
        comments: extract_comments(src),
        ci: 0,
        last: 0,
        out: String::new(),
    };
    f.program(prog);
    // Trailing comments after the last item (e.g. a file-end note).
    f.lead(src.len(), 0, prog.items.is_empty());
    if f.out.is_empty() {
        return String::new();
    }
    if !f.out.ends_with('\n') {
        f.out.push('\n');
    }
    // Keep exactly one trailing newline.
    while f.out.ends_with("\n\n") {
        f.out.pop();
    }
    f.out
}

// ------------------------------------------------------------ comments ---

/// One `//` comment: byte span plus its exact text (no trailing newline).
pub(crate) struct Comment {
    span: Span,
    text: String,
}

/// Extract `//` comments, skipping `"..."` strings. Works on bytes: only
/// ASCII bytes (`"`, `/`, newline) are significant, so multibyte UTF-8
/// sequences pass through untouched.
fn extract_comments(src: &str) -> Vec<Comment> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' && bytes[i] != b'\n' && bytes[i] != b'\r'
                {
                    if bytes[i] == b'\\' {
                        i += 1;
                        if i < bytes.len() && bytes[i] != b'\n' && bytes[i] != b'\r' {
                            i += 1;
                        }
                    } else {
                        i += 1;
                    }
                }
                if i < bytes.len() && bytes[i] == b'"' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                let start = i;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                if let Some(raw) = src.get(start..i) {
                    let text = raw.trim_end_matches([' ', '\t', '\r']).to_owned();
                    out.push(Comment {
                        span: Span::new(start, i),
                        text,
                    });
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    out
}

// ----------------------------------------------------------- formatter ---

pub(crate) struct Fmt<'a> {
    pub(crate) src: &'a str,
    pub(crate) comments: Vec<Comment>,
    /// Next unconsumed comment.
    pub(crate) ci: usize,
    /// Source offset up to which comments/blanks have been accounted for.
    pub(crate) last: usize,
    pub(crate) out: String,
}

pub(crate) fn indent(level: usize) -> String {
    " ".repeat(level * INDENT_WIDTH)
}

impl<'a> Fmt<'a> {
    pub(crate) fn slice(&self, start: usize, end: usize) -> &'a str {
        let start = start.min(self.src.len());
        let end = end.min(self.src.len()).max(start);
        self.src.get(start..end).unwrap_or("")
    }

    /// True when `src[a..b]` contains a blank line (`\n` + only
    /// spaces/tabs/`\r` + `\n`).
    pub(crate) fn has_blank_between(&self, a: usize, b: usize) -> bool {
        let bytes = self.slice(a, b).as_bytes();
        let mut i = 0;
        while i < bytes.len() && bytes[i] != b'\n' {
            i += 1;
        }
        if i >= bytes.len() {
            return false;
        }
        i += 1;
        while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t' || bytes[i] == b'\r') {
            i += 1;
        }
        i < bytes.len() && bytes[i] == b'\n'
    }

    /// True when an unconsumed comment starts inside `[start, end)`.
    pub(crate) fn has_comment_in(&self, start: usize, end: usize) -> bool {
        self.comments[self.ci..]
            .iter()
            .any(|c| c.span.start >= start && c.span.start < end)
    }

    /// Trailing-comma probe: after `last_end` (the last element's end),
    /// skipping whitespace and `//` comments, is there a `,` whose next
    /// significant byte is `closer`?
    pub(crate) fn has_trailing(&self, last_end: usize, closer: u8) -> bool {
        let bytes = self.src.as_bytes();
        let mut i = last_end.min(bytes.len());
        let skip = |i: &mut usize| loop {
            while *i < bytes.len()
                && (bytes[*i] == b' '
                    || bytes[*i] == b'\t'
                    || bytes[*i] == b'\r'
                    || bytes[*i] == b'\n')
            {
                *i += 1;
            }
            if *i + 1 < bytes.len() && bytes[*i] == b'/' && bytes[*i + 1] == b'/' {
                while *i < bytes.len() && bytes[*i] != b'\n' {
                    *i += 1;
                }
            } else {
                break;
            }
        };
        skip(&mut i);
        if i < bytes.len() && bytes[i] == b',' {
            i += 1;
            skip(&mut i);
            i < bytes.len() && bytes[i] == closer
        } else {
            false
        }
    }

    /// Trailing-comma probe for union/error variants: like
    /// [`has_trailing`](Self::has_trailing), but tolerates the payload
    /// parens — the last payload type's end sits inside `)`, so one `)`
    /// may precede the comma (`B(u64),`).
    pub(crate) fn has_trailing_variant(&self, last_end: usize) -> bool {
        let bytes = self.src.as_bytes();
        let mut i = last_end.min(bytes.len());
        let skip = |i: &mut usize| loop {
            while *i < bytes.len()
                && (bytes[*i] == b' '
                    || bytes[*i] == b'\t'
                    || bytes[*i] == b'\r'
                    || bytes[*i] == b'\n')
            {
                *i += 1;
            }
            if *i + 1 < bytes.len() && bytes[*i] == b'/' && bytes[*i + 1] == b'/' {
                while *i < bytes.len() && bytes[*i] != b'\n' {
                    *i += 1;
                }
            } else {
                break;
            }
        };
        skip(&mut i);
        // Optional payload close paren (`B(u64)` ends inside it).
        if i < bytes.len() && bytes[i] == b')' {
            i += 1;
            skip(&mut i);
        }
        if i < bytes.len() && bytes[i] == b',' {
            i += 1;
            skip(&mut i);
            i < bytes.len() && bytes[i] == b'}'
        } else {
            false
        }
    }
    /// Emit own-line comments strictly before `target` at `level`, then one
    /// blank line when the source had a blank line after the last consumed
    /// comment (never when `first`, i.e. at the start of a file or block).
    pub(crate) fn lead(&mut self, target: usize, level: usize, first: bool) {
        while self.ci < self.comments.len() && self.comments[self.ci].span.start < target {
            if self.comments[self.ci].span.start < self.last {
                self.ci += 1;
                continue;
            }
            let start = self.comments[self.ci].span.start;
            // A blank line before the comment is preserved (except at the
            // start of a file or block, where leading blanks collapse).
            if !first
                && !self.out.is_empty()
                && !self.out.ends_with("\n\n")
                && self.has_blank_between(self.last, start)
            {
                self.out.push('\n');
            }
            let text = self.comments[self.ci].text.clone();
            let end = self.comments[self.ci].span.end;
            self.out.push_str(&indent(level));
            self.out.push_str(&text);
            self.out.push('\n');
            self.last = self.last.max(end);
            self.ci += 1;
        }
        if !first && self.has_blank_between(self.last, target) {
            self.out.push('\n');
        }
        // Blanks are detected between consecutive nodes only: account the
        // gap up to `target` so an earlier blank cannot fire twice.
        self.last = self.last.max(target);
    }

    /// Consume the next comment when it starts on the same source line as
    /// `anchor` (no `\n` between), returning its text for trailing use.
    pub(crate) fn trailing(&mut self, anchor: usize) -> Option<String> {
        if self.ci < self.comments.len() {
            let start = self.comments[self.ci].span.start;
            if start >= anchor {
                let between = self.slice(anchor, start);
                if !between.contains('\n') {
                    let text = self.comments[self.ci].text.clone();
                    let end = self.comments[self.ci].span.end;
                    self.last = self.last.max(end);
                    self.ci += 1;
                    return Some(text);
                }
            }
        }
        None
    }

    /// Skip whitespace and `//` comments from `from`, returning the first
    /// significant byte index (clamped to `src.len()`).
    fn skip_trivia(&self, from: usize) -> usize {
        let bytes = self.src.as_bytes();
        let mut i = from.min(bytes.len());
        loop {
            while i < bytes.len()
                && (bytes[i] == b' ' || bytes[i] == b'\t' || bytes[i] == b'\r' || bytes[i] == b'\n')
            {
                i += 1;
            }
            if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            break;
        }
        i
    }

    /// First `{` at/after `from`, skipping strings and `//` comments.
    /// Falls back to `src.len()` when there is none.
    pub(crate) fn find_open_brace(&self, from: usize) -> usize {
        let bytes = self.src.as_bytes();
        let mut i = from.min(bytes.len());
        while i < bytes.len() {
            match bytes[i] {
                b'"' => {
                    i = self.string_end(i);
                }
                b'/' if bytes.get(i + 1) == Some(&b'/') => {
                    while i < bytes.len() && bytes[i] != b'\n' {
                        i += 1;
                    }
                }
                b'{' => return i,
                _ => i += 1,
            }
        }
        bytes.len()
    }

    /// Index just past a `"..."` string starting at the quote `at`.
    fn string_end(&self, at: usize) -> usize {
        let bytes = self.src.as_bytes();
        let mut i = at + 1;
        while i < bytes.len() && bytes[i] != b'"' && bytes[i] != b'\n' && bytes[i] != b'\r' {
            if bytes[i] == b'\\' {
                i += 1;
                if i < bytes.len() {
                    i += 1;
                }
            } else {
                i += 1;
            }
        }
        if i < bytes.len() && bytes[i] == b'"' {
            i += 1;
        }
        i
    }

    /// Index of the `}` matching the `{` at `open`. Falls back to
    /// `src.len()` on unbalanced input (already rejected by parsing).
    pub(crate) fn matching_close(&self, open: usize) -> usize {
        let bytes = self.src.as_bytes();
        if bytes.get(open) != Some(&b'{') {
            return self.src.len();
        }
        let mut depth = 0usize;
        let mut i = open;
        while i < bytes.len() {
            match bytes[i] {
                b'"' => {
                    i = self.string_end(i);
                    continue;
                }
                b'/' if bytes.get(i + 1) == Some(&b'/') => {
                    while i < bytes.len() && bytes[i] != b'\n' {
                        i += 1;
                    }
                    continue;
                }
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return i;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        bytes.len()
    }

    /// True when the next significant byte at/after `from` is `{`.
    pub(crate) fn is_braced(&self, from: usize) -> bool {
        self.src.as_bytes().get(self.skip_trivia(from)) == Some(&b'{')
    }

    /// Emit a braced block whose source braces are `open`/`close`.
    /// The `{` is pushed by the caller convention here: this emits `{`,
    /// the statements at `level + 1`, then `}` at `level`.
    pub(crate) fn block_at(&mut self, stmts: &[Stmt], level: usize, open: usize, close: usize) {
        // A comment between the header and `{` rides along inside.
        self.lead(open, level + 1, true);
        self.out.push('{');
        if stmts.is_empty() {
            self.lead(close, level + 1, true);
            self.out.push('}');
            return;
        }
        self.out.push('\n');
        for (i, s) in stmts.iter().enumerate() {
            self.stmt(s, level + 1, i == 0);
        }
        self.lead(close, level + 1, true);
        self.out.push_str(&indent(level));
        self.out.push('}');
    }

    /// Emit a normalized braced block for an unbraced single branch.
    pub(crate) fn normalize_branch(&mut self, body: &[Stmt], level: usize) {
        self.out.push('{');
        if body.is_empty() {
            self.out.push('}');
            return;
        }
        self.out.push('\n');
        for (i, s) in body.iter().enumerate() {
            self.stmt(s, level + 1, i == 0);
        }
        self.out.push_str(&indent(level));
        self.out.push('}');
    }
}

// ------------------------------------------------------- single-line ---

pub(crate) fn scalar_text(s: &Scalar) -> String {
    match s {
        Scalar::Int(v) => format!("{v}"),
        Scalar::U64(v) => format!("{v}u64"),
        Scalar::I64(v) => format!("{v}i64"),
        Scalar::U8(v) => format!("{v}u8"),
        Scalar::Bool(v) => format!("{v}"),
        Scalar::F64(bits) => {
            let v = f64::from_bits(*bits);
            let short = format!("{v:?}");
            if short.contains('.') && !short.contains('e') && !short.contains('E') {
                format!("{short}f64")
            } else {
                // Shortest form uses an exponent, which the lexer cannot
                // re-read: fall back to plain decimal (always has a `.`).
                let mut long = format!("{v:.17}");
                while long.ends_with('0') {
                    long.pop();
                }
                if long.ends_with('.') {
                    long.push('0');
                }
                format!("{long}f64")
            }
        }
    }
}

/// Re-quote string bytes with VL escapes.
pub(crate) fn string_text(bytes: &[u8]) -> String {
    let mut buf = Vec::with_capacity(bytes.len() + 2);
    buf.push(b'"');
    for &b in bytes {
        match b {
            0 => buf.extend_from_slice(b"\\0"),
            b'\n' => buf.extend_from_slice(b"\\n"),
            b'\r' => buf.extend_from_slice(b"\\r"),
            b'\t' => buf.extend_from_slice(b"\\t"),
            b'\\' => buf.extend_from_slice(b"\\\\"),
            b'"' => buf.extend_from_slice(b"\\\""),
            0x20..=0x7E => buf.push(b),
            _ => buf.push(b),
        }
    }
    buf.push(b'"');
    String::from_utf8(buf).expect("string bytes derive from valid UTF-8 source plus ASCII escapes")
}

/// Precedence levels (higher binds tighter) for parenthesization.
fn prec(e: &Expr) -> u8 {
    match e {
        Expr::Binary { op, .. } => match op {
            BinOp::Or => 10,
            BinOp::And => 30,
            BinOp::Eq | BinOp::Ne => 40,
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => 60,
            BinOp::Add | BinOp::Sub => 70,
            BinOp::Mul | BinOp::Div => 80,
        },
        Expr::Catch { .. } => 20,
        Expr::Cast { .. } => 50,
        Expr::Unary { .. } | Expr::Try { .. } => 90,
        Expr::Index { .. } | Expr::Field { .. } | Expr::TupleIndex { .. } => 100,
        _ => 110,
    }
}

fn bin_op_text(op: &BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::And => "&&",
        BinOp::Or => "||",
    }
}

/// Wrap `e` in parentheses when its precedence is below `parent`.
fn wrap(e: &Expr, parent: u8, same_ok: bool) -> String {
    let s = single_expr(e);
    let p = prec(e);
    if p < parent || (p == parent && !same_ok) {
        format!("({s})")
    } else {
        s
    }
}

/// Render an expression on a single line, adding only the parentheses
/// needed for the text to re-parse to the same tree. Nested trailing commas
/// fold away here (the multiline writer handles lists that must split).
pub(crate) fn single_expr(e: &Expr) -> String {
    match e {
        Expr::Literal(s, _) => scalar_text(s),
        Expr::String(bytes, _) => string_text(bytes),
        Expr::Null(_) => "null".to_string(),
        Expr::Var { path, .. } => path.join("."),
        Expr::Call {
            callee,
            type_args,
            args,
            ..
        } => {
            let mut s = callee.join(".");
            if !type_args.is_empty() {
                let inner = type_args
                    .iter()
                    .map(|t| t.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                s.push_str(&format!("::[{inner}]"));
            }
            s.push('(');
            s.push_str(&args.iter().map(single_expr).collect::<Vec<_>>().join(", "));
            s.push(')');
            s
        }
        Expr::ArrayLiteral { elems, .. } => {
            format!(
                "[{}]",
                elems.iter().map(single_expr).collect::<Vec<_>>().join(", ")
            )
        }
        Expr::ObjectLiteral { name, fields, .. } => {
            if fields.is_empty() {
                return format!("{name} {{}}");
            }
            let inner = fields
                .iter()
                .map(|(field, _, value)| format!("{field} = {}", single_expr(value)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{name} {{ {inner} }}")
        }
        Expr::TupleLiteral { elems, .. } => {
            let inner = elems
                .iter()
                .map(|(name, _, value)| match name {
                    Some(n) => format!("{n} = {}", single_expr(value)),
                    None => single_expr(value),
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("#({inner})")
        }
        Expr::Index { base, index, .. } => {
            format!("{}[{}]", wrap(base, 100, true), single_expr(index))
        }
        Expr::Field { base, name, .. } => format!("{}.{name}", wrap(base, 100, true)),
        Expr::TupleIndex { base, index, .. } => format!("{}.`{index}", wrap(base, 100, true)),
        Expr::Unary { op, rhs, .. } => {
            let prefix = match op {
                UnOp::Neg => "-",
                UnOp::Not => "!",
            };
            format!("{prefix}{}", wrap(rhs, 90, true))
        }
        Expr::Try { inner, .. } => format!("try {}", wrap(inner, 90, true)),
        Expr::Catch { lhs, fallback, .. } => {
            // Right-associative: `a catch b catch c` needs no parens, but a
            // parenthesized left does to re-parse the same way.
            let left = wrap(lhs, 20, false);
            let right = wrap(fallback, 20, true);
            format!("{left} catch {right}")
        }
        Expr::Binary { op, lhs, rhs, .. } => {
            let level = match op {
                BinOp::Or => 10,
                BinOp::And => 30,
                BinOp::Eq | BinOp::Ne => 40,
                BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => 60,
                BinOp::Add | BinOp::Sub => 70,
                BinOp::Mul | BinOp::Div => 80,
            };
            // Left-associative: a same-precedence right child needs parens.
            format!(
                "{} {} {}",
                wrap(lhs, level, true),
                bin_op_text(op),
                wrap(rhs, level, false)
            )
        }
        Expr::Cast { inner, target, .. } => {
            format!("{} as {target}", wrap(inner, 50, true))
        }
    }
}

/// When `e` is a top-level `&&` / `||` / `catch` chain, flatten it into its
/// separator and operands for width-driven continuation lines.
pub(crate) fn split_chain(e: &Expr) -> Option<(&'static str, Vec<&Expr>)> {
    match e {
        Expr::Binary {
            op: BinOp::And,
            lhs,
            rhs,
            ..
        } => {
            let mut parts = Vec::new();
            flatten_and(lhs, &mut parts);
            parts.push(rhs);
            Some(("&&", parts))
        }
        Expr::Binary {
            op: BinOp::Or,
            lhs,
            rhs,
            ..
        } => {
            let mut parts = Vec::new();
            flatten_or(lhs, &mut parts);
            parts.push(rhs);
            Some(("||", parts))
        }
        Expr::Catch { lhs, fallback, .. } => {
            let mut parts = vec![lhs.as_ref()];
            let mut rest = fallback.as_ref();
            while let Expr::Catch {
                lhs: next,
                fallback: next_fallback,
                ..
            } = rest
            {
                parts.push(next.as_ref());
                rest = next_fallback.as_ref();
            }
            parts.push(rest);
            Some(("catch", parts))
        }
        _ => None,
    }
}

fn flatten_and<'x>(e: &'x Expr, out: &mut Vec<&'x Expr>) {
    if let Expr::Binary {
        op: BinOp::And,
        lhs,
        rhs,
        ..
    } = e
    {
        flatten_and(lhs, out);
        out.push(rhs);
    } else {
        out.push(e);
    }
}

fn flatten_or<'x>(e: &'x Expr, out: &mut Vec<&'x Expr>) {
    if let Expr::Binary {
        op: BinOp::Or,
        lhs,
        rhs,
        ..
    } = e
    {
        flatten_or(lhs, out);
        out.push(rhs);
    } else {
        out.push(e);
    }
}
