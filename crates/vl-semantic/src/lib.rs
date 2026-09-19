//! vl-semantic: name resolution over the AST.
//!
//! Builds lexical scopes for top-level items and function bodies,
//! reports undefined names and duplicate definitions. The resulting
//! [`Resolution`] is consumed by `vl-hir` lowering.

use std::collections::HashMap;

use vl_common::{Diagnostic, ModuleSpec, Span};
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
    pub kind: DefKind,
    /// Compiler-owned extern signature. `Some` for `External` defs resolved
    /// from the module catalog; `None` for locals and poisoned imports.
    pub sig: Option<vl_common::FuncSig>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefKind {
    Local,
    External,
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
            kind: DefKind::Local,
            sig: None,
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
    modules: Vec<ModuleSpec>,
    imports: HashMap<String, ModuleSpec>,
    poisoned_imports: std::collections::HashSet<String>,
    loop_depth: usize,
}

pub fn resolve(prog: &Program) -> (Resolution, Vec<Diagnostic>) {
    resolve_with_modules(prog, &default_modules())
}

pub fn resolve_with_modules(
    prog: &Program,
    modules: &[ModuleSpec],
) -> (Resolution, Vec<Diagnostic>) {
    let mut r = Resolver {
        scopes: vec![HashMap::new()],
        out: Resolution::default(),
        diags: vec![],
        modules: modules.to_vec(),
        imports: HashMap::new(),
        poisoned_imports: std::collections::HashSet::new(),
        loop_depth: 0,
    };

    for item in &prog.items {
        if let Item::Use { path, names, span } = item {
            r.resolve_use(path, names.as_deref(), *span);
        }
    }

    // Pass 1: declare top-level names so forward references work.
    for item in &prog.items {
        match item {
            Item::Use { .. } => {}
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
            Item::Use { .. } => {}
            Item::Let { value, .. } => {
                r.resolve_expr(value);
            }
            Item::Function { params, body, .. } => {
                r.scopes.push(HashMap::new());
                for p in params {
                    if r.scopes
                        .last()
                        .is_some_and(|scope| scope.contains_key(&p.name))
                    {
                        let previous = r
                            .scopes
                            .last()
                            .and_then(|scope| scope.get(&p.name))
                            .and_then(|id| r.out.defs.iter().find(|d| d.id == *id))
                            .map(|d| d.span);
                        let mut diagnostic =
                            Diagnostic::error(format!("duplicate parameter `{}`", p.name))
                                .with_label(p.name_span, "redefined here")
                                .with_code("E200");
                        if let Some(previous) = previous {
                            diagnostic = diagnostic.with_bare_label(previous);
                        }
                        r.diags.push(diagnostic);
                        continue;
                    }
                    let id = r.out.intern_def(p.name.clone(), p.name_span);
                    r.scopes.last_mut().unwrap().insert(p.name.clone(), id);
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
            Stmt::Assign {
                name,
                name_span,
                value,
                ..
            } => {
                self.resolve_expr(value);
                match self.lookup(name) {
                    Some(id) => {
                        self.out.uses.insert((name_span.start, name_span.end), id);
                    }
                    None => {
                        self.diags.push(
                            Diagnostic::error(format!("cannot find `{name}` in this scope"))
                                .with_label(*name_span, "undefined variable")
                                .with_note("did you mean to `let`-bind it first?")
                                .with_code("E201"),
                        );
                    }
                }
            }
            Stmt::Expr(e) => self.resolve_expr(e),
            Stmt::Return { value, .. } => {
                if let Some(e) = value {
                    self.resolve_expr(e);
                }
            }
            Stmt::Break { span } => {
                if self.loop_depth == 0 {
                    self.diags.push(
                        Diagnostic::error("`break` outside of a loop")
                            .with_label(*span, "no enclosing `while`")
                            .with_code("E204"),
                    );
                }
            }
            Stmt::Continue { span } => {
                if self.loop_depth == 0 {
                    self.diags.push(
                        Diagnostic::error("`continue` outside of a loop")
                            .with_label(*span, "no enclosing `while`")
                            .with_code("E204"),
                    );
                }
            }
            Stmt::While {
                condition, body, ..
            } => {
                self.resolve_expr(condition);
                self.loop_depth += 1;
                self.scopes.push(HashMap::new());
                for stmt in body {
                    self.resolve_stmt(stmt);
                }
                self.scopes.pop();
                self.loop_depth -= 1;
            }
            Stmt::If {
                condition,
                then_body,
                else_body,
                ..
            } => {
                self.resolve_expr(condition);
                self.scopes.push(HashMap::new());
                for stmt in then_body {
                    self.resolve_stmt(stmt);
                }
                self.scopes.pop();
                if let Some(body) = else_body {
                    self.scopes.push(HashMap::new());
                    for stmt in body {
                        self.resolve_stmt(stmt);
                    }
                    self.scopes.pop();
                }
            }
        }
    }

    fn resolve_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Literal(_, _) | Expr::String(_, _) => {}
            Expr::Var { path, span } => match self.lookup_path(path, *span) {
                Some(id) => {
                    self.out.uses.insert((span.start, span.end), id);
                }
                None => {
                    self.diags.push(
                        Diagnostic::error(format!(
                            "cannot find `{}` in this scope",
                            path.join(".")
                        ))
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
                match self.lookup_path(callee, *callee_span) {
                    Some(id) => {
                        self.out
                            .uses
                            .insert((callee_span.start, callee_span.end), id);
                    }
                    None => {
                        self.diags.push(
                            Diagnostic::error(format!(
                                "cannot find `{}` in this scope",
                                callee.join(".")
                            ))
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

    fn resolve_use(&mut self, path: &[String], names: Option<&[String]>, span: Span) {
        let key = path.join(".");
        let Some(module) = self
            .modules
            .iter()
            .find(|m| m.path.as_string() == key)
            .cloned()
        else {
            // `use std.print;` is a single-export import: the parent path is
            // a known module and the leaf is one of its exports. It behaves
            // like `use std.{print};` and brings `print` into scope.
            if names.is_none() && path.len() >= 2 {
                let parent_key = path[..path.len() - 1].join(".");
                let leaf = path[path.len() - 1].clone();
                if let Some(parent) = self
                    .modules
                    .iter()
                    .find(|m| m.path.as_string() == parent_key)
                    .cloned()
                {
                    if parent.lookup(&leaf).is_some() {
                        self.imports.insert(
                            leaf.clone(),
                            ModuleSpec {
                                path: vl_common::ModulePath::new(vec![parent_key, leaf]),
                                exports: vec![],
                            },
                        );
                        return;
                    }
                    self.poisoned_imports.insert(leaf.clone());
                    self.poisoned_imports.insert(key.clone());
                    self.diags.push(
                        Diagnostic::error(format!("module `{parent_key}` has no export `{leaf}`"))
                            .with_label(span, "unknown module export")
                            .with_code("E203"),
                    );
                    return;
                }
            }
            if let Some(name) = path.last() {
                self.poisoned_imports.insert(name.clone());
            }
            self.poisoned_imports.insert(key.clone());
            self.diags.push(
                Diagnostic::error(format!("cannot find module `{key}`"))
                    .with_label(span, "unknown module")
                    .with_code("E202"),
            );
            return;
        };
        match names {
            None => {
                self.imports
                    .insert(path.last().cloned().unwrap_or_default(), module);
            }
            Some(names) => {
                for name in names {
                    if module.lookup(name).is_none() {
                        self.poisoned_imports.insert(name.clone());
                        self.poisoned_imports.insert(format!("{key}.{name}"));
                        self.diags.push(
                            Diagnostic::error(format!("module `{key}` has no export `{name}`"))
                                .with_label(span, "unknown module export")
                                .with_code("E203"),
                        );
                    } else {
                        self.imports.insert(
                            name.clone(),
                            ModuleSpec {
                                path: vl_common::ModulePath::new(vec![key.clone(), name.clone()]),
                                exports: vec![],
                            },
                        );
                    }
                }
            }
        }
    }

    /// Resolve the compiler-owned signature for a bare imported name
    /// (`use std.print;` then `print()`). The synthetic import path encodes
    /// the parent module + leaf (`std.print`), which we split to find the
    /// original export.
    fn sig_for_bare_import(&self, alias: &str) -> Option<vl_common::FuncSig> {
        let synthetic = self.imports.get(alias)?;
        // Synthetic singletons carry no exports; real module aliases (e.g.
        // `use std.string`) do and are not bare-callable.
        if !synthetic.exports.is_empty() {
            return None;
        }
        let full = synthetic.path.as_string();
        let (parent_key, leaf) = full.rsplit_once('.')?;
        // `ModulePath::new(vec![key, name])` stores the dotted key as one
        // segment, so split the string form rather than the segments.
        let parent = self
            .modules
            .iter()
            .find(|m| m.path.as_string() == parent_key)?;
        parent.lookup(leaf).map(|e| e.sig.clone())
    }

    fn lookup_path(&mut self, path: &[String], span: Span) -> Option<DefId> {
        if path.len() == 1 {
            if let Some(id) = self.lookup(&path[0]) {
                return Some(id);
            }
            if self.imports.contains_key(&path[0]) {
                let sig = self.sig_for_bare_import(&path[0]);
                return Some(self.external_def(path[0].clone(), span, sig));
            }
            if self.poisoned_imports.contains(&path[0]) {
                return Some(self.external_def(path[0].clone(), span, None));
            }
            return None;
        }
        if self.poisoned_imports.contains(&path.join(".")) {
            return Some(self.external_def(path.join("."), span, None));
        }
        // `use missing.module;` poisons the imported alias (`module`), not
        // only the full source path. Treat qualified uses through that alias
        // as poisoned too, so the E202 root cause does not cascade into E201.
        if self.poisoned_imports.contains(&path[0]) {
            return Some(self.external_def(path.join("."), span, None));
        }
        let module = self.imports.get(&path[0]).cloned()?;
        if path.len() != 2 {
            return None;
        }
        let export = module.lookup(&path[1])?;
        let sig = export.sig.clone();
        Some(self.external_def(path.join("."), span, Some(sig)))
    }

    fn external_def(&mut self, name: String, span: Span, sig: Option<vl_common::FuncSig>) -> DefId {
        let id = DefId(self.out.defs.len() as u32);
        self.out.defs.push(Def {
            id: id.clone(),
            name,
            span,
            kind: DefKind::External,
            sig,
        });
        id
    }
}

pub fn default_modules() -> Vec<ModuleSpec> {
    use vl_common::VlType as T;
    vec![
        ModuleSpec::new(
            &["std"],
            &[
                ("print", &[("value", T::String)], T::Void),
                ("print_u64", &[("value", T::U64)], T::Void),
            ],
        ),
        ModuleSpec::new(
            &["std", "fs"],
            &[
                ("open", &[("path", T::String)], T::File),
                ("read", &[("file", T::File)], T::String),
            ],
        ),
        ModuleSpec::new(
            &["std", "string"],
            &[("len", &[("value", T::String)], T::U64)],
        ),
    ]
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
    fn break_outside_a_loop_is_an_error() {
        let (_, diags) = resolve_src("function main() { break; }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("outside of a loop"));
    }

    #[test]
    fn break_inside_while_resolves() {
        let (_, diags) = resolve_src("function main() { while (true) { break; } }");
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
    }

    #[test]
    fn assignment_to_an_undefined_name_errors() {
        let (_, diags) = resolve_src("function main() { x = 1; }");
        assert!(diags.iter().any(|d| d.code.as_deref() == Some("E201")));
    }

    #[test]
    fn assignment_to_a_bound_local_resolves() {
        let (_, diags) = resolve_src("function main() { let x = 1; x = 2; }");
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
    }

    #[test]
    fn undefined_variable_errors() {
        let (_, diags) = resolve_src("let x = y;");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains('y'));
    }

    #[test]
    fn shadowing_is_a_warning_only() {
        let (toks, _) = vl_lex::lex("function f(x: i64): i64 { let x = 1; x; }");
        let (prog, _) = vl_syntax::parse(&toks, "");
        let (_, diags) = resolve(&prog);
        assert!(diags.iter().all(|d| !d.is_error()));
    }

    #[test]
    fn call_callee_and_args_resolve() {
        let (_, diags) = resolve_src(
            "function add(a: i64, b: i64): i64 { a + b; } function main() { add(1, 2); }",
        );
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
        let (_, diags) = resolve_src("function main() { helper(); } function helper(): i64 { 1; }");
        assert!(diags.iter().all(|d| !d.is_error()));
    }

    #[test]
    fn duplicate_parameters_are_an_error() {
        let (_, diags) = resolve_src("function f(x: i64, x: i64): i64 { x; }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("duplicate parameter"));
    }

    #[test]
    fn poisoned_module_alias_suppresses_qualified_use_cascade() {
        let (_, diags) = resolve_src("use missing.module; function main() { module.foo(); }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("cannot find module"));
    }

    #[test]
    fn single_export_use_brings_bare_name_into_scope() {
        let (_, diags) = resolve_src("use std.string.len; function main() { len(\"s\"); }");
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
    }

    #[test]
    fn bare_import_carries_its_signature() {
        let (res, diags) = resolve_src("use std.string.len; function main() { len(\"s\"); }");
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
        let def = res.defs.iter().find(|d| d.name == "len").expect("len def");
        let sig = def.sig.as_ref().expect("extern sig");
        assert_eq!(sig.ret, vl_common::VlType::U64);
        assert_eq!(sig.params.len(), 1);
    }

    #[test]
    fn single_export_use_of_std_print_resolves() {
        let (toks, _) = vl_lex::lex("use std.print; function main() { print(\"hi\"); }");
        let (prog, _) = vl_syntax::parse(&toks, "");
        let (_, diags) = resolve_with_modules(&prog, &vl_codegen_modules());
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
    }

    #[test]
    fn single_export_use_with_unknown_export_is_one_error() {
        let (_, diags) = resolve_src("use std.string.bogus; function main() { bogus(); }");
        assert_eq!(diags.iter().filter(|d| d.is_error()).count(), 1);
        assert!(diags[0].message.contains("no export `bogus`"));
    }

    fn vl_codegen_modules() -> Vec<ModuleSpec> {
        use vl_common::VlType as T;
        vec![
            ModuleSpec::new(
                &["std"],
                &[
                    ("print", &[("value", T::String)], T::Void),
                    ("print_u64", &[("value", T::U64)], T::Void),
                ],
            ),
            ModuleSpec::new(
                &["std", "fs"],
                &[
                    ("open", &[("path", T::String)], T::File),
                    ("read", &[("file", T::File)], T::String),
                ],
            ),
            ModuleSpec::new(
                &["std", "string"],
                &[("len", &[("value", T::String)], T::U64)],
            ),
        ]
    }
}
