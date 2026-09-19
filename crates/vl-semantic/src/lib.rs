//! vl-semantic: name resolution over the AST.
//!
//! Builds lexical scopes for top-level items and function bodies,
//! reports undefined names and duplicate definitions. The resulting
//! [`Resolution`] is consumed by `vl-hir` lowering.

use std::collections::HashMap;

use vl_common::{Diagnostic, Span};
use vl_syntax::{Expr, Item, Program, Stmt};

/// A definition site: which item/scope and which binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefId(pub u32);

/// Where a name was defined.
#[derive(Debug, Clone)]
pub struct Def {
    pub id: DefId,
    pub name: String,
    pub span: Span,
}

/// Resolution result: every variable *use* span maps to its [`Def`].
#[derive(Debug, Default)]
pub struct Resolution {
    /// Keyed by the use-site span start (spans are unique per node here).
    pub uses: HashMap<(usize, usize), DefId>,
    pub defs: Vec<Def>,
}

impl Resolution {
    fn intern_def(&mut self, name: String, span: Span) -> DefId {
        let id = DefId(self.defs.len() as u32);
        self.defs.push(Def {
            id: id.clone(),
            name,
            span,
        });
        id
    }

    pub fn def_of(&self, use_span: Span) -> Option<&Def> {
        self.uses
            .get(&(use_span.start, use_span.end))
            .and_then(|id| self.defs.iter().find(|d| d.id == *id))
    }

    /// Definition-site lookup: which [`Def`] was declared *at* `span`.
    /// (Use-sites go through [`def_of`](Self::def_of); def-sites are not uses.)
    pub fn def_at(&self, def_span: Span) -> Option<&Def> {
        self.defs
            .iter()
            .find(|d| d.span.start == def_span.start && d.span.end == def_span.end)
    }
}

struct Resolver {
    scopes: Vec<HashMap<String, DefId>>,
    out: Resolution,
    diags: Vec<Diagnostic>,
}

pub fn resolve(prog: &Program) -> (Resolution, Vec<Diagnostic>) {
    let mut r = Resolver {
        scopes: vec![HashMap::new()],
        out: Resolution::default(),
        diags: vec![],
    };

    // Pass 1: declare top-level names so forward references work.
    for item in &prog.items {
        match item {
            Item::Let {
                name, name_span, ..
            } => {
                r.declare_global(name.clone(), *name_span);
            }
            Item::Function {
                name, name_span, ..
            } => {
                r.declare_global(name.clone(), *name_span);
            }
        }
    }

    // Pass 2: resolve bodies.
    for item in &prog.items {
        match item {
            Item::Let { value, .. } => {
                r.resolve_expr(value);
            }
            Item::Function { params, body, .. } => {
                r.scopes.push(HashMap::new());
                for (p, s) in params {
                    let id = r.out.intern_def(p.clone(), *s);
                    r.scopes.last_mut().unwrap().insert(p.clone(), id);
                }
                for stmt in body {
                    r.resolve_stmt(stmt);
                }
                r.scopes.pop();
            }
        }
    }

    (r.out, r.diags)
}

impl Resolver {
    fn declare_global(&mut self, name: String, span: Span) {
        let global = &mut self.scopes[0];
        if let Some(prev) = global
            .get(&name)
            .and_then(|id| self.out.defs.iter().find(|d| d.id == *id).map(|d| d.span))
        {
            self.diags.push(
                Diagnostic::error(format!("duplicate definition of `{name}`"))
                    .with_label(span, "redefined here")
                    .with_bare_label(prev)
                    .with_code("E200"),
            );
            return;
        }
        let id = self.out.intern_def(name.clone(), span);
        global.insert(name, id);
    }

    fn declare_local(&mut self, name: String, span: Span) {
        let top = self.scopes.last_mut().unwrap();
        if top.contains_key(&name) {
            self.diags.push(
                Diagnostic::warning(format!("`{name}` shadows a previous binding"))
                    .with_label(span, "shadowing definition"),
            );
        }
        let id = self.out.intern_def(name.clone(), span);
        top.insert(name, id);
    }

    fn lookup(&self, name: &str) -> Option<DefId> {
        for scope in self.scopes.iter().rev() {
            if let Some(id) = scope.get(name) {
                return Some(id.clone());
            }
        }
        None
    }

    fn resolve_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Let {
                name,
                name_span,
                value,
                ..
            } => {
                self.resolve_expr(value);
                self.declare_local(name.clone(), *name_span);
            }
            Stmt::Expr(e) => self.resolve_expr(e),
        }
    }

    fn resolve_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Int(_, _) => {}
            Expr::Var(name, span) => match self.lookup(name) {
                Some(id) => {
                    self.out.uses.insert((span.start, span.end), id);
                }
                None => {
                    self.diags.push(
                        Diagnostic::error(format!("cannot find `{name}` in this scope"))
                            .with_label(*span, "undefined variable")
                            .with_note("did you mean to `let`-bind it first?")
                            .with_code("E201"),
                    );
                }
            },
            Expr::Call {
                callee,
                callee_span,
                args,
                ..
            } => {
                // Callee is a plain name use so `function` items resolve
                // (including forward references via the global pre-pass).
                match self.lookup(callee) {
                    Some(id) => {
                        self.out
                            .uses
                            .insert((callee_span.start, callee_span.end), id);
                    }
                    None => {
                        self.diags.push(
                            Diagnostic::error(format!("cannot find `{callee}` in this scope"))
                                .with_label(*callee_span, "undefined function")
                                .with_note("did you mean to `function`-define it first?")
                                .with_code("E201"),
                        );
                    }
                }
                for arg in args {
                    self.resolve_expr(arg);
                }
            }
            Expr::Unary { rhs, .. } => self.resolve_expr(rhs),
            Expr::Binary { lhs, rhs, .. } => {
                self.resolve_expr(lhs);
                self.resolve_expr(rhs);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve_src(src: &str) -> (Resolution, Vec<Diagnostic>) {
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        resolve(&prog)
    }

    #[test]
    fn undefined_variable_errors() {
        let (_, diags) = resolve_src("let x = y;");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains('y'));
    }

    #[test]
    fn shadowing_is_a_warning_only() {
        let (toks, _) = vl_lex::lex("function f(x) { let x = 1; x; }");
        let (prog, _) = vl_syntax::parse(&toks, "");
        let (_, diags) = resolve(&prog);
        assert!(diags.iter().all(|d| !d.is_error()));
    }

    #[test]
    fn call_callee_and_args_resolve() {
        let (_, diags) =
            resolve_src("function add(a, b) { a + b; } function main() { add(1, 2); }");
        assert!(diags.iter().all(|d| !d.is_error()));
    }

    #[test]
    fn undefined_callee_errors() {
        let (_, diags) = resolve_src("function main() { nope(1); }");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("nope"));
    }

    #[test]
    fn forward_call_resolves_via_global_prepass() {
        let (_, diags) = resolve_src("function main() { helper(); } function helper() { 1; }");
        assert!(diags.iter().all(|d| !d.is_error()));
    }
}
