//! vl-stdlib: the embedded VL standard prelude.
//!
//! The prelude is VL source (`prelude.vl`) compiled into the compiler with
//! `include_str!`. [`inject`] lazily merges the helpers a program actually
//! calls into that program as *local* items, so:
//!
//! - helpers lower as ordinary local calls: no provider artifacts, no
//!   cross-module boundary (E207/E208) or `std`-reservation issues;
//! - programs that use nothing from the prelude are byte-identical to a
//!   build without it (goldens stay clean);
//! - helpers needing VM natives call them qualified (`string.len`,
//!   `math.mod_u64`, `fmt.u64_to_s`); injection adds the matching `use`
//!   only when the user lacks that alias.
//!
//! The prelude may only contain monomorphic functions over scalar/`String`
//! types with no globals: helpers merge into any program as locals, so
//! generics, object layouts, and global state stay out by construction.

use std::collections::{HashMap, HashSet};

use vl_common::Span;
use vl_syntax::{Expr, Item, Program, Stmt};

/// Embedded prelude source. paths are relative to this file.
pub const PRELUDE_SRC: &str = include_str!("../prelude.vl");

/// Lex + parse the embedded prelude. The caller picks the module because
/// only the items are kept; injection discards the program shell.
pub fn parse_prelude() -> (Program, Vec<vl_common::Diagnostic>) {
    let (toks, mut diags) = vl_lex::lex(PRELUDE_SRC);
    let (prog, mut d) = vl_syntax::parse_with_module(&toks, PRELUDE_SRC, "std.prelude");
    diags.append(&mut d);
    (prog, diags)
}

/// VM-backed imports each prelude helper needs, as `use` paths.
/// Must stay in sync with the qualified calls in `prelude.vl`
/// (enforced by `prelude_imports_match_bodies` below).
fn required_uses(name: &str) -> &'static [&'static [&'static str]] {
    match name {
        "is_empty" | "strings_equal" => &[&["std", "string"]],
        "u64_to_string" => &[&["std", "fmt"]],
        "is_even" => &[&["std", "math"]],
        _ => &[],
    }
}

/// Lazily inject referenced prelude helpers into `user`, returning the
/// merged program (same module). A no-op when nothing is referenced or a
/// user definition shadows the helper. Never fails: a broken embedded
/// prelude (a compiler bug, covered by tests) leaves the user program alone
/// rather than mis-rendering prelude spans against user source.
pub fn inject(user: Program) -> Program {
    let (prelude, diags) = parse_prelude();
    if diags.iter().any(|d| d.is_error()) {
        return user;
    }
    let mut available: HashMap<String, &Item> = HashMap::new();
    for item in &prelude.items {
        if let Item::Function { name, .. } = item {
            available.insert(name.clone(), item);
        }
    }
    if available.is_empty() {
        return user;
    }

    let mut defined: HashSet<String> = HashSet::new();
    let mut use_aliases: HashMap<String, String> = HashMap::new();
    for item in &user.items {
        match item {
            Item::Function { name, .. } | Item::Let { name, .. } => {
                defined.insert(name.clone());
            }
            Item::Object { name, .. } => {
                defined.insert(name.clone());
            }
            Item::Use { path, names, .. } => {
                let joined = path.join(".");
                match names {
                    Some(ns) => {
                        for n in ns {
                            use_aliases.insert(n.clone(), joined.clone());
                        }
                    }
                    None => {
                        if let Some(alias) = path.last() {
                            use_aliases.insert(alias.clone(), joined);
                        }
                    }
                }
            }
        }
    }

    // Fixpoint over bare-called names so future helpers may call each other.
    let mut wanted: HashSet<String> = HashSet::new();
    for item in &user.items {
        collect_bare_calls_in_item(item, &mut wanted);
    }
    loop {
        let mut grown = false;
        let snapshot: Vec<String> = wanted.iter().cloned().collect();
        for name in snapshot {
            if let Some(Item::Function { .. }) = available.get(&name) {
                if !defined.contains(&name) {
                    let mut more = HashSet::new();
                    collect_bare_calls_in_item(available[&name], &mut more);
                    for dep in more {
                        if available.contains_key(&dep)
                            && !defined.contains(&dep)
                            && wanted.insert(dep)
                        {
                            grown = true;
                        }
                    }
                }
            }
        }
        if !grown {
            break;
        }
    }

    // Drop helpers the user shadowed, then helpers whose import aliases
    // are taken by something else (adding our `use` would E206).
    let mut selected: Vec<&String> = wanted
        .iter()
        .filter(|n| available.contains_key(*n) && !defined.contains(*n))
        .collect();
    selected.sort();
    let mut ok: Vec<&String> = Vec::new();
    let mut pending_uses: Vec<Vec<String>> = Vec::new();
    for name in selected {
        let mut blocked = false;
        for req in required_uses(name) {
            let path = req.join(".");
            let alias = req.last().expect("use path has a leaf").to_string();
            match use_aliases.get(&alias) {
                Some(existing) if *existing == path => {}
                Some(_) => {
                    blocked = true;
                    break;
                }
                None if defined.contains(&alias) => {
                    blocked = true;
                    break;
                }
                None => {
                    if !pending_uses.iter().any(|p| p.join(".") == path) {
                        pending_uses.push(req.iter().map(|s| s.to_string()).collect());
                    }
                    use_aliases.insert(alias, path);
                }
            }
        }
        if !blocked {
            ok.push(name);
        }
    }
    if ok.is_empty() {
        return user;
    }

    let mut items: Vec<Item> = Vec::new();
    for path in &pending_uses {
        items.push(Item::Use {
            path: path.clone(),
            names: None,
            span: Span::empty(0),
        });
    }
    // Prelude order is deterministic; sort selected names for stability.
    for name in ok {
        items.push((*available[name]).clone());
    }
    items.extend(user.items);
    Program {
        module: user.module,
        items,
    }
}

fn collect_bare_calls_in_item(item: &Item, out: &mut HashSet<String>) {
    match item {
        Item::Function { body, .. } => {
            for stmt in body {
                collect_bare_calls_in_stmt(stmt, out);
            }
        }
        Item::Let { value, .. } => collect_bare_calls_in_expr(value, out),
        Item::Use { .. } | Item::Object { .. } => {}
    }
}

fn collect_bare_calls_in_stmt(stmt: &Stmt, out: &mut HashSet<String>) {
    match stmt {
        Stmt::Let { value, .. } | Stmt::Assign { value, .. } | Stmt::Expr(value) => {
            collect_bare_calls_in_expr(value, out);
        }
        Stmt::IndexAssign {
            array,
            index,
            value,
            ..
        } => {
            collect_bare_calls_in_expr(array, out);
            collect_bare_calls_in_expr(index, out);
            collect_bare_calls_in_expr(value, out);
        }
        Stmt::FieldAssign { base, value, .. } => {
            collect_bare_calls_in_expr(base, out);
            collect_bare_calls_in_expr(value, out);
        }
        Stmt::If {
            condition,
            then_body,
            else_body,
            ..
        } => {
            collect_bare_calls_in_expr(condition, out);
            for s in then_body {
                collect_bare_calls_in_stmt(s, out);
            }
            if let Some(body) = else_body {
                for s in body {
                    collect_bare_calls_in_stmt(s, out);
                }
            }
        }
        Stmt::While {
            condition, body, ..
        } => {
            collect_bare_calls_in_expr(condition, out);
            for s in body {
                collect_bare_calls_in_stmt(s, out);
            }
        }
        Stmt::Return { value, .. } => {
            if let Some(value) = value {
                collect_bare_calls_in_expr(value, out);
            }
        }
        Stmt::Break { .. } | Stmt::Continue { .. } => {}
    }
}

fn collect_bare_calls_in_expr(expr: &Expr, out: &mut HashSet<String>) {
    match expr {
        Expr::Call { callee, args, .. } => {
            if callee.len() == 1 {
                out.insert(callee[0].clone());
            }
            for arg in args {
                collect_bare_calls_in_expr(arg, out);
            }
        }
        Expr::ArrayLiteral { elems, .. } => {
            for e in elems {
                collect_bare_calls_in_expr(e, out);
            }
        }
        Expr::ObjectLiteral { fields, .. } => {
            for (_, _, e) in fields {
                collect_bare_calls_in_expr(e, out);
            }
        }
        Expr::Index { base, index, .. } => {
            collect_bare_calls_in_expr(base, out);
            collect_bare_calls_in_expr(index, out);
        }
        Expr::Field { base, .. }
        | Expr::Unary { rhs: base, .. }
        | Expr::Cast { inner: base, .. } => collect_bare_calls_in_expr(base, out),
        Expr::Binary { lhs, rhs, .. } => {
            collect_bare_calls_in_expr(lhs, out);
            collect_bare_calls_in_expr(rhs, out);
        }
        Expr::Literal(..) | Expr::String(..) | Expr::Var { .. } => {}
    }
}

/// Qualified `alias.fn` roots used by one prelude helper body.
#[cfg(test)]
fn qualifier_roots(item: &Item) -> HashSet<String> {
    let mut roots = HashSet::new();
    fn expr_roots(expr: &Expr, roots: &mut HashSet<String>) {
        match expr {
            Expr::Call { callee, args, .. } => {
                if callee.len() == 2 {
                    roots.insert(callee[0].clone());
                }
                for arg in args {
                    expr_roots(arg, roots);
                }
            }
            Expr::ArrayLiteral { elems, .. } => {
                for e in elems {
                    expr_roots(e, roots);
                }
            }
            Expr::ObjectLiteral { fields, .. } => {
                for (_, _, e) in fields {
                    expr_roots(e, roots);
                }
            }
            Expr::Index { base, index, .. } => {
                expr_roots(base, roots);
                expr_roots(index, roots);
            }
            Expr::Field { base, .. }
            | Expr::Unary { rhs: base, .. }
            | Expr::Cast { inner: base, .. } => expr_roots(base, roots),
            Expr::Binary { lhs, rhs, .. } => {
                expr_roots(lhs, roots);
                expr_roots(rhs, roots);
            }
            Expr::Literal(..) | Expr::String(..) | Expr::Var { .. } => {}
        }
    }
    fn stmt_roots(stmt: &Stmt, roots: &mut HashSet<String>) {
        match stmt {
            Stmt::Let { value, .. } | Stmt::Assign { value, .. } | Stmt::Expr(value) => {
                expr_roots(value, roots);
            }
            Stmt::IndexAssign {
                array,
                index,
                value,
                ..
            } => {
                expr_roots(array, roots);
                expr_roots(index, roots);
                expr_roots(value, roots);
            }
            Stmt::FieldAssign { base, value, .. } => {
                expr_roots(base, roots);
                expr_roots(value, roots);
            }
            Stmt::If {
                condition,
                then_body,
                else_body,
                ..
            } => {
                expr_roots(condition, roots);
                for s in then_body {
                    stmt_roots(s, roots);
                }
                if let Some(body) = else_body {
                    for s in body {
                        stmt_roots(s, roots);
                    }
                }
            }
            Stmt::While {
                condition, body, ..
            } => {
                expr_roots(condition, roots);
                for s in body {
                    stmt_roots(s, roots);
                }
            }
            Stmt::Return { value, .. } => {
                if let Some(value) = value {
                    expr_roots(value, roots);
                }
            }
            Stmt::Break { .. } | Stmt::Continue { .. } => {}
        }
    }
    if let Item::Function { body, .. } = item {
        for stmt in body {
            stmt_roots(stmt, &mut roots);
        }
    }
    roots
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_user(src: &str) -> Program {
        let (toks, _) = vl_lex::lex(src);
        let (prog, diags) = vl_syntax::parse(&toks, src);
        assert!(diags.is_empty(), "{diags:?}");
        prog
    }

    fn fn_names(prog: &Program) -> Vec<String> {
        prog.items
            .iter()
            .filter_map(|i| match i {
                Item::Function { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn prelude_parses_clean() {
        let (_, diags) = parse_prelude();
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn prelude_functions_are_all_monomorphic_and_stateless() {
        let (prog, _) = parse_prelude();
        for item in &prog.items {
            match item {
                Item::Function {
                    name,
                    type_params,
                    params,
                    ret,
                    ..
                } => {
                    assert!(type_params.is_empty(), "{name} must be monomorphic");
                    assert!(ret.is_some(), "{name} needs a return type");
                    for p in params {
                        assert!(p.ty.is_some(), "{name} param needs a type");
                    }
                }
                Item::Use { .. } => {}
                other => panic!("prelude allows only fun/use, got {other:?}"),
            }
        }
    }

    #[test]
    fn prelude_imports_match_bodies() {
        // Every qualified root a helper calls must be declared in
        // `required_uses`, and every declared path must be imported by the
        // prelude source itself (so it typechecks standalone).
        let (prog, _) = parse_prelude();
        let imported: HashSet<String> = prog
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Use { path, .. } => Some(path.join(".")),
                _ => None,
            })
            .collect();
        for item in &prog.items {
            let Item::Function { name, .. } = item else {
                continue;
            };
            let mut covered: HashSet<String> = HashSet::new();
            for req in required_uses(name) {
                let path = req.join(".");
                assert!(
                    imported.contains(&path),
                    "{name} requires `use {path}` in prelude.vl"
                );
                covered.insert(req.last().expect("leaf").to_string());
            }
            for root in qualifier_roots(item) {
                assert!(
                    covered.contains(&root),
                    "{name} calls `{root}.*` without declaring it"
                );
            }
        }
    }

    #[test]
    fn inject_is_noop_without_references() {
        let user = parse_user("fun main() { val x = 1u64; x; }");
        let merged = inject(user);
        assert_eq!(fn_names(&merged), vec!["main".to_string()]);
        assert!(!merged.items.iter().any(|i| matches!(i, Item::Use { .. })));
    }

    #[test]
    fn inject_pulls_only_referenced_helpers_plus_uses() {
        let user = parse_user("fun main() { val m = max_u64(1u64, 2u64); m; }");
        let merged = inject(user);
        assert_eq!(
            fn_names(&merged),
            vec!["max_u64".to_string(), "main".to_string()]
        );
        assert!(!merged.items.iter().any(|i| matches!(i, Item::Use { .. })));
    }

    #[test]
    fn inject_adds_missing_use_for_vm_backed_helpers() {
        let user = parse_user("fun main() { val e = is_empty(\"s\"); e; }");
        let merged = inject(user);
        assert!(fn_names(&merged).contains(&"is_empty".to_string()));
        assert!(
            merged.items.iter().any(|i| matches!(
                i,
                Item::Use { path, .. } if path == &vec!["std".to_string(), "string".to_string()]
            )),
            "{merged:?}"
        );
    }

    #[test]
    fn inject_reuses_an_existing_use_alias() {
        let user = parse_user("use std.string; fun main() { val e = is_empty(\"s\"); e; }");
        let merged = inject(user);
        let uses = merged
            .items
            .iter()
            .filter(|i| matches!(i, Item::Use { .. }))
            .count();
        assert_eq!(uses, 1, "{merged:?}");
    }

    #[test]
    fn user_definition_shadows_helper() {
        let user = parse_user(
            "fun max_u64(a: u64, b: u64): u64 { return a; } fun main() { val m = max_u64(1u64, 2u64); m; }",
        );
        let merged = inject(user);
        assert_eq!(
            fn_names(&merged),
            vec!["max_u64".to_string(), "main".to_string()]
        );
    }

    #[test]
    fn prelude_full_frontend_and_naravm_emit() {
        use vl_codegen::Target;
        // End-to-end through the real pipeline with the broad catalog:
        // every helper must resolve, typecheck, lower, and emit.
        let (prelude, pdiags) = parse_prelude();
        assert!(pdiags.is_empty(), "{pdiags:?}");
        let (res, mut diags) = vl_semantic::resolve_with_modules(&prelude, &vl_codegen::modules());
        assert!(diags.is_empty(), "{diags:?}");
        let hir = vl_hir::lower(&prelude, &res);
        let (typed, mut d) = vl_typecheck::check(&hir);
        diags.append(&mut d);
        diags.append(&mut typed.validate_normalized(&hir, &diags));
        assert!(diags.is_empty(), "{diags:?}");
        let lir = vl_lir::lower(&hir, &typed);
        let (artifact, ediags) = vl_codegen::NaraVmTarget.emit(&lir);
        assert!(ediags.is_empty(), "{ediags:?}");
        assert_eq!(&artifact.unwrap().bytes.unwrap()[..4], b"nara");
    }
}
