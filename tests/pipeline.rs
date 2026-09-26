//! Integration tests: drive the whole pipeline like the driver does.
//! Run with `cargo test --workspace` (or `./scripts/check.sh`).

fn frontend(src: &str) -> Result<vl_lir::LirProgram, Vec<vl_common::Diagnostic>> {
    let stdlib = vl_stdlib::load();
    let (toks, mut diags) = vl_lex::lex(src);
    let (ast, mut d) = vl_syntax::parse(&toks, src);
    diags.append(&mut d);
    // Same merged catalog + link step as the driver: helpers a snippet
    // calls behave as locals; untouched snippets compile exactly as before.
    let mut catalog = vl_codegen::modules();
    stdlib.extend_catalog(&mut catalog);
    let (res, mut d) = vl_semantic::resolve_with_modules(&ast, &catalog);
    diags.append(&mut d);
    let hir = vl_hir::lower(&ast, &res);
    // Same merged catalog as resolution, like the driver: nominal types
    // imported from target modules validate here too.
    let (typed, mut d) = vl_typecheck::check_with_modules(&hir, &catalog);
    diags.append(&mut d);
    // Same boundary guard as the driver: no unresolved type reaches LIR.
    diags.append(&mut typed.validate_normalized(&hir, &diags));
    if diags.iter().any(|d| d.is_error()) {
        return Err(diags);
    }
    // Single-file world for stdlib generics (same machinery as the driver).
    let mut world_refs = vec![(&hir, &typed)];
    for (shir, styped) in stdlib.checked_modules() {
        world_refs.push((shir, styped));
    }
    let (plan, world_diags) = vl_typecheck::world::plan_world(&world_refs);
    for (_, diag) in world_diags {
        diags.push(diag);
    }
    if diags.iter().any(|d| d.is_error()) {
        return Err(diags);
    }
    let mut lir = vl_lir::lower_project(&hir, &typed, &plan);
    stdlib.link_with_plan(&mut lir, &plan);
    Ok(lir)
}

#[test]
fn hello_compiles_to_lir() {
    let src = std::fs::read_to_string("examples/hello.vl").unwrap();
    let lir = frontend(&src).expect("hello.vl must compile");
    let dump = lir.dump();
    assert!(dump.contains("call std::print"), "{dump}");
    assert!(dump.contains("ret"), "{dump}");
}

#[test]
fn snippet_without_main_compiles_to_lir_and_naravm() {
    use vl_codegen::Target;
    // No `main`: snippets and libraries must compile. Entrypoint presence
    // is validated by the VM/loader, not the compiler.
    let lir = frontend("fun add(a: u64, b: u64): u64 { return a + b; }")
        .expect("snippet without main must compile");
    let dump = lir.dump();
    assert!(dump.contains("fn add:"), "{dump}");
    assert!(!dump.contains("fn main:"), "{dump}");
    let (artifact, diags) = vl_codegen::NaraVmTarget.emit(&lir);
    assert!(diags.is_empty(), "{diags:?}");
    let bytes = artifact.unwrap().bytes.unwrap();
    assert_eq!(&bytes[..4], b"nara");
}

#[test]
fn println_compiles_and_runs_on_naravm() {
    use vl_codegen::Target;
    let lir = frontend("use std; fun main() { std.println(\"hi\"); std.print(\"x\\n\"); }")
        .expect("println must compile");
    let dump = lir.dump();
    assert!(dump.contains("call std::println"), "{dump}");
    let (artifact, diags) = vl_codegen::NaraVmTarget.emit(&lir);
    assert!(diags.is_empty(), "{diags:?}");
    let bytes = artifact.unwrap().bytes.unwrap();
    assert_eq!(&bytes[..4], b"nara");
    assert!(bytes.contains(&0x20), "expected calli instructions");
}

#[test]
fn println_arg_types_are_checked() {
    let err =
        frontend("use std; fun main() { std.println(1); }").expect_err("println expects String");
    assert!(
        err.iter().any(|d| d.message.contains("expects `String`")),
        "{err:?}"
    );
}

#[test]
fn module_alias_is_not_treated_as_a_callable_import() {
    let err =
        frontend("use std; fun main() { std(); }").expect_err("a module alias is not callable");
    assert!(
        err.iter().any(|d| d.code.as_deref() == Some("E303")),
        "{err:?}"
    );
    assert!(
        !err.iter().any(|d| d.code.as_deref() == Some("E500")),
        "{err:?}"
    );
}

#[test]
fn naravm_emits_executable_vmfile() {
    use vl_codegen::Target;
    let src = std::fs::read_to_string("examples/hello.vl").unwrap();
    let lir = frontend(&src).unwrap();
    let (artifact, diags) = vl_codegen::NaraVmTarget.emit(&lir);
    assert!(diags.is_empty());
    let artifact = artifact.unwrap();
    assert_eq!(&artifact.bytes.as_ref().unwrap()[..4], b"nara");
}

#[test]
fn arith_matches_golden_lir() {
    let src = std::fs::read_to_string("examples/arith.vl").unwrap();
    let lir = frontend(&src).expect("arith.vl must compile");
    let golden = std::fs::read_to_string("tests/golden/arith.lir").unwrap();
    assert_eq!(lir.dump(), golden);
}

#[test]
fn capabilities_example_compiles_and_aliases() {
    let src = std::fs::read_to_string("examples/capabilities.vl").unwrap();
    // AST keeps source capabilities (`*Counter`).
    {
        let (toks, _) = vl_lex::lex(&src);
        let (ast, diags) = vl_syntax::parse(&toks, &src);
        assert!(diags.is_empty(), "{diags:?}");
        let text = format!("{ast:?}");
        assert!(text.contains("Mutable"), "{text}");
    }
    let lir = frontend(&src).expect("capabilities.vl must compile");
    let dump = lir.dump();
    // Mutable and read-only views erase to the same runtime ops.
    assert!(dump.contains("object_get"), "{dump}");
    assert!(dump.contains("object_set"), "{dump}");
    assert!(!dump.contains('*'), "{dump}");
    // AST keeps capabilities; LIR erases them (checked via dump above).
    use vl_codegen::Target;
    let (artifact, diags) = vl_codegen::NaraVmTarget.emit(&lir);
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(&artifact.unwrap().bytes.unwrap()[..4], b"nara");
}

#[test]
fn err_readonly_mutation_fails() {
    let src = std::fs::read_to_string("examples/err_readonly_mutation.vl").unwrap();
    let err = frontend(&src).expect_err("err_readonly_mutation.vl must fail");
    // One root cause per invalid operation: E310 (field), E205 (rebind),
    // E309 (upgrade). The valid downgrade stays quiet.
    assert_eq!(err.iter().filter(|d| d.is_error()).count(), 3, "{err:?}");
    assert!(
        err.iter().any(|d| d.code.as_deref() == Some("E310")),
        "{err:?}"
    );
    assert!(
        err.iter().any(|d| d.code.as_deref() == Some("E205")),
        "{err:?}"
    );
    assert!(
        err.iter().any(|d| d.code.as_deref() == Some("E309")),
        "{err:?}"
    );
}

#[test]
fn undefined_name_is_one_clean_error() {
    let src = std::fs::read_to_string("examples/err_undefined.vl").unwrap();
    let err = frontend(&src).expect_err("must fail");
    assert!(err.iter().any(|d| d.is_error()));
    let rendered = vl_common::diagnostic::render_all(&err, "err_undefined.vl", &src);
    assert!(rendered.contains('y'), "{rendered}");
}

#[test]
fn syntax_error_reports_without_panic() {
    let src = std::fs::read_to_string("examples/err_syntax.vl").unwrap();
    let err = frontend(&src).expect_err("must fail");
    assert!(!err.is_empty());
}

#[test]
fn function_calls_lower_to_lir() {
    let src = std::fs::read_to_string("examples/calls.vl").unwrap();
    let lir = frontend(&src).expect("calls.vl must compile");
    let dump = lir.dump();
    assert!(dump.contains("%0 = param 0"), "{dump}");
    assert!(dump.contains("call add(%0, %0)"), "{dump}");
    assert!(dump.contains("call twice(%2)"), "{dump}");
}

#[test]
fn naravm_emits_calls_vl_with_user_calls() {
    use vl_codegen::Target;
    let src = std::fs::read_to_string("examples/calls.vl").unwrap();
    let lir = frontend(&src).expect("calls.vl must compile");
    let (artifact, diags) = vl_codegen::NaraVmTarget.emit(&lir);
    assert!(diags.is_empty(), "{diags:?}");
    let bytes = artifact.unwrap().bytes.unwrap();
    assert_eq!(&bytes[..4], b"nara");
    // calli = 0x20: main -> twice, twice -> add.
    assert!(bytes.contains(&0x20), "expected calli instructions");
}

#[test]
fn naravm_emits_mixed_params_string_return_and_recursion() {
    use vl_codegen::Target;
    let lir = frontend(
        r#"
use std;
fun add(a: u64, b: u64): u64 { return a + b; }
fun greet(name: String): String { return name; }
fun fact(n: u64): u64 {
    var r = 1u64;
    if (n == 0u64) { r; } else { r = n * fact(n - 1u64); }
    return r;
}
fun main() {
    std.print(greet("hi\n"));
    std.print_u64(add(fact(3u64), 1u64));
}
"#,
    )
    .expect("mixed params, String return, and recursion must compile");
    let (artifact, diags) = vl_codegen::NaraVmTarget.emit(&lir);
    assert!(diags.is_empty(), "{diags:?}");
    let bytes = artifact.unwrap().bytes.unwrap();
    assert_eq!(&bytes[..4], b"nara");
    assert!(bytes.contains(&0x20), "expected calli instructions");
}

#[test]
fn strings_lower_to_byte_constants() {
    let lir = frontend(r#"val greeting = "hi\n";"#).expect("String must compile");
    let dump = lir.dump();
    assert!(dump.contains("string [104, 105, 10]"), "{dump}");
}

#[test]
fn unterminated_string_is_a_lex_error() {
    let err = frontend("val x = \"not closed\nlet y = 1;").expect_err("must fail");
    assert!(err
        .iter()
        .any(|d| d.message.contains("unterminated string")));
}

#[test]
fn module_imports_resolve_without_importing_descendants() {
    let src = std::fs::read_to_string("examples/modules.vl").unwrap();
    let lir = frontend(&src).expect("module imports must compile");
    let dump = lir.dump();
    assert!(dump.contains("call std.string::len"), "{dump}");
    assert!(dump.contains("call std.fs::open"), "{dump}");
    assert!(dump.contains("call std.fs::read"), "{dump}");
}

#[test]
fn unknown_module_export_is_a_single_error() {
    let (toks, _) = vl_lex::lex("use std.string.{missing}; fun main() { missing(); }");
    let (ast, _) = vl_syntax::parse(&toks, "");
    let (_, diags) = vl_semantic::resolve(&ast);
    assert!(diags
        .iter()
        .any(|d| d.message.contains("no export `missing`")));
}

/// Drive N source modules like the project driver does: parse every file,
/// collect all interfaces into one catalog, resolve/check all units, run the
/// world fixed point, then lower each with its plan. Returns one LIR program
/// per module, in `units` order.
fn frontend_project(
    units: &[(&str, &str)],
) -> Result<Vec<vl_lir::LirProgram>, Vec<vl_common::Diagnostic>> {
    let mut parsed = Vec::new();
    let mut diags = Vec::new();
    for (module, src) in units {
        let (toks, mut d) = vl_lex::lex(src);
        diags.append(&mut d);
        let (ast, mut d) = vl_syntax::parse_with_module(&toks, src, module);
        diags.append(&mut d);
        parsed.push(ast);
    }
    let stdlib = vl_stdlib::load();
    let mut modules = vl_codegen::modules();
    stdlib.extend_catalog(&mut modules);
    for ast in &parsed {
        let (interface, mut d) = vl_semantic::collect_interface_quiet(ast);
        diags.append(&mut d);
        modules.push(interface.as_spec());
    }
    if diags.iter().any(|d| d.is_error()) {
        return Err(diags);
    }
    let catalog = modules.clone();
    let mut hirs = Vec::new();
    let mut typeds = Vec::new();
    for ast in &parsed {
        let (res, mut d) = vl_semantic::resolve_with_modules(ast, &catalog);
        diags.append(&mut d);
        if d.iter().any(|diag| diag.is_error()) {
            continue;
        }
        let hir = vl_hir::lower(ast, &res);
        let (typed, mut d) = vl_typecheck::check_with_modules(&hir, &modules);
        diags.append(&mut d);
        diags.append(&mut typed.validate_normalized(&hir, &diags));
        hirs.push(hir);
        typeds.push(typed);
    }
    if diags.iter().any(|d| d.is_error()) {
        return Err(diags);
    }
    let mut world_refs: Vec<(&vl_hir::HirProgram, &vl_typecheck::TypedProgram)> =
        Vec::with_capacity(hirs.len() + stdlib.checked_modules().len());
    for (hir, typed) in hirs.iter().zip(typeds.iter()) {
        world_refs.push((hir, typed));
    }
    for (hir, typed) in stdlib.checked_modules() {
        world_refs.push((hir, typed));
    }
    let (plan, world_diags) = vl_typecheck::world::plan_world(&world_refs);
    for (_, diag) in world_diags {
        diags.push(diag);
    }
    if diags.iter().any(|d| d.is_error()) {
        return Err(diags);
    }
    let mut out = Vec::new();
    for (hir, typed) in hirs.iter().zip(typeds.iter()) {
        let mut lir = vl_lir::lower_project(hir, typed, &plan);
        stdlib.link_with_plan(&mut lir, &plan);
        if let Some(bad) = lir.validate_runtime() {
            diags.push(vl_common::Diagnostic::error(format!("internal: {bad}")).with_code("E500"));
            return Err(diags);
        }
        // No `Param`, template, or unmangled generic import may reach the backend.
        for f in &lir.functions {
            assert!(!f.name.contains("Param"), " Param leaked: {}", f.name);
        }
        for import in &lir.imports {
            assert!(
                !import.symbol.function.contains("Param"),
                "generic import leaked: {}",
                import.symbol.function
            );
        }
        out.push(lir);
    }
    Ok(out)
}

#[test]
fn cross_module_object_types_compile_to_lir_and_naravm() {
    use vl_codegen::Target;
    let programs = frontend_project(&[
        (
            "vl.person",
            "type Person = object { name: String, age: u64, }; fun new(name: String, age: u64): *Person { return Person { name = name, age = age, }; } fun print_name(person: Person) { person.name; }",
        ),
        (
            "vl.main",
            "use vl.person; fun main() { var rose = person.new(\"Rose\", 25); person.print_name(rose); rose.age = 26; val lit: vl.person.Person = vl.person.Person { name = \"Lit\", age = 40 }; person.print_name(lit); }",
        ),
    ])
    .expect("cross-module objects must compile");
    assert_eq!(programs.len(), 2);
    let importer = &programs[1];
    assert!(
        importer
            .imports
            .iter()
            .any(|i| i.symbol.module == "vl.person" && i.symbol.function == "new"),
        "{:?}",
        importer.imports
    );
    assert!(
        importer.dump().contains("object_set"),
        "{}",
        importer.dump()
    );
    assert!(
        importer.dump().contains("new_object vl.person.Person"),
        "{}",
        importer.dump()
    );
    for lir in &programs {
        let (artifact, diags) = vl_codegen::NaraVmTarget.emit(lir);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(&artifact.unwrap().bytes.unwrap()[..4], b"nara");
    }
}

#[test]
fn same_object_name_in_two_modules_stays_disjoint() {
    frontend_project(&[
        (
            "vl.person",
            "type Person = object { name: String, }; fun name_of(p: Person): String { return p.name; }",
        ),
        (
            "vl.other",
            "type Person = object { tag: u64, }; fun tag_of(p: Person): u64 { return p.tag; }",
        ),
        (
            "vl.main",
            "use vl.person; use vl.other; fun main() { val a: vl.person.Person = vl.person.Person { name = \"R\" }; val b: vl.other.Person = vl.other.Person { tag = 7 }; person.name_of(a); other.tag_of(b); }",
        ),
    ])
    .expect("same-named objects in different modules must stay disjoint");
}

#[test]
fn scalar_literals_and_if_lower_to_typed_control_flow() {
    let lir = frontend("fun main() { val x = 1u64; if (true) { x; } else { 255u8; } 1.5f64; }")
        .expect("scalar literals and if must compile");
    let dump = lir.dump();
    assert!(dump.contains("const 1u64"), "{dump}");
    assert!(dump.contains("const 1.5f64"), "{dump}");
    assert!(dump.contains("branch_if_false"), "{dump}");
    assert!(dump.contains("L0:"), "{dump}");
}

#[test]
fn if_requires_a_boolean_condition() {
    let err = frontend("fun main() { if (1) { 2; } }").expect_err("if condition must be bool");
    assert!(
        err.iter().any(|d| d.message.contains("must be bool")),
        "{err:?}"
    );
}

#[test]
fn unbraced_conditional_branches_compile() {
    let lir = frontend("fun main() { if (true) 1u64; else 2u64; }")
        .expect("unbraced branches must compile");
    assert!(lir.dump().contains("branch_if_false"));
}

#[test]
fn extern_call_arg_types_are_checked() {
    let err = frontend("use std; fun main() { std.print(1); }").expect_err("print expects String");
    assert!(
        err.iter().any(|d| d.message.contains("expects `String`")),
        "{err:?}"
    );
}

#[test]
fn extern_call_arity_is_checked() {
    let err = frontend("use std; fun main() { std.print(\"a\", \"b\"); }")
        .expect_err("print expects one arg");
    assert!(
        err.iter().any(|d| d.message.contains("expects 1")),
        "{err:?}"
    );
}

#[test]
fn while_countdown_lowers_to_jumps_copies_and_runs_on_naravm() {
    let src = std::fs::read_to_string("examples/cond_loop.vl").unwrap();
    let lir = frontend(&src).expect("cond_loop.vl must compile");
    let dump = lir.dump();
    assert!(dump.contains("branch_if_false"), "{dump}");
    assert!(dump.contains("jump"), "{dump}");
    assert!(dump.contains("copy"), "{dump}");

    use vl_codegen::Target;
    let (artifact, diags) = vl_codegen::NaraVmTarget.emit(&lir);
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(&artifact.unwrap().bytes.unwrap()[..4], b"nara");
}

#[test]
fn comparisons_and_short_circuit_logic_lower() {
    let lir = frontend(
        "fun main() { var a = 1; if (a <= 2 && a != 3 || !(a > 9)) { a; } while (a >= 1) { a = a - 1; } }",
    )
    .expect("comparisons must compile");
    let dump = lir.dump();
    for op in ["le", "ne", "gt", "ge", "not", "copy"] {
        assert!(dump.contains(op), "{op} missing in {dump}");
    }
    assert!(!dump.contains("= and"), "{dump}");
    assert!(!dump.contains("= or"), "{dump}");
}

#[test]
fn break_outside_a_loop_is_one_error() {
    let err = frontend("fun main() { break; }").expect_err("must fail");
    assert_eq!(err.iter().filter(|d| d.is_error()).count(), 1);
    assert!(err[0].message.contains("outside of a loop"));
}

#[test]
fn continue_outside_a_loop_is_one_error() {
    let err = frontend("fun main() { continue; }").expect_err("must fail");
    assert_eq!(err.iter().filter(|d| d.is_error()).count(), 1);
}

#[test]
fn assignment_type_mismatch_is_one_error() {
    let err = frontend(r#"fun main() { var x = 1; x = "s"; }"#).expect_err("must fail");
    assert_eq!(err.iter().filter(|d| d.is_error()).count(), 1);
    assert!(err.iter().any(|d| d.code.as_deref() == Some("E309")));
}

#[test]
fn while_condition_must_be_bool() {
    let err = frontend("fun main() { while (1) { 2; } }").expect_err("must fail");
    assert!(err
        .iter()
        .any(|d| d.message.contains("while condition must be bool")));
}

#[test]
fn missing_annotations_are_an_error() {
    let err = frontend("fun add(a, b) { return a + b; }").expect_err("must fail");
    assert!(
        err.iter().any(|d| d.code.as_deref() == Some("E104")),
        "{err:?}"
    );
}

#[test]
fn explicit_return_compiles_and_lowers_to_ret() {
    let lir = frontend("fun add(a: i64, b: i64): i64 { return a + b; } fun main() { add(1, 2); }")
        .expect("explicit return must compile");
    let dump = lir.dump();
    assert!(dump.contains("add"), "{dump}");
    assert!(dump.contains("ret"), "{dump}");
}

#[test]
fn missing_return_is_an_error() {
    let err = frontend("fun f(): i64 { val x = 1; }").expect_err("must fail");
    assert!(
        err.iter().any(|d| d.code.as_deref() == Some("E307")),
        "{err:?}"
    );
}

#[test]
fn trailing_expr_is_not_an_implicit_return() {
    let err = frontend(r#"fun f(): i64 { 1; }"#).expect_err("must fail");
    assert!(
        err.iter()
            .any(|d| d.message.contains("not all paths return")),
        "{err:?}"
    );
}

#[test]
fn bare_return_in_value_function_is_an_error() {
    let err = frontend("fun f(): i64 { return; }").expect_err("must fail");
    assert!(
        err.iter().any(|d| d.message.contains("returns nothing")),
        "{err:?}"
    );
}

#[test]
fn value_return_in_void_function_is_an_error() {
    let err = frontend("fun main() { return 1; }").expect_err("must fail");
    assert!(
        err.iter().any(|d| d.message.contains("returns `void`")),
        "{err:?}"
    );
}

#[test]
fn arrays_compile_through_frontend_to_naravm() {
    let src = std::fs::read_to_string("examples/arrays.vl").unwrap();
    let lir = frontend(&src).expect("arrays.vl must compile");
    let dump = lir.dump();
    for op in ["new_array", "array_lit", "array_get", "array_set"] {
        assert!(dump.contains(op), "{op} missing in {dump}");
    }

    use vl_codegen::Target;
    let (artifact, diags) = vl_codegen::NaraVmTarget.emit(&lir);
    assert!(diags.is_empty(), "{diags:?}");
    let bytes = artifact.unwrap().bytes.unwrap();
    assert_eq!(&bytes[..4], b"nara");
    // create = 0x26, createi = 0x27, getvat = 0x28, setvat/setvati = 0x29/0x2D.
    for op in [0x26u8, 0x27, 0x28, 0x2du8] {
        assert!(bytes.contains(&op), "no {op:#x} in {bytes:?}");
    }
}

#[test]
fn objects_compile_with_reference_field_semantics() {
    use vl_codegen::Target;
    let src = std::fs::read_to_string("examples/objects.vl").unwrap();
    let lir = frontend(&src).expect("objects.vl must compile");
    let dump = lir.dump();
    assert!(dump.contains("new_object Counter"), "{dump}");
    assert!(dump.contains("object_get"), "{dump}");
    assert!(dump.contains("object_set"), "{dump}");
    let (artifact, diags) = vl_codegen::NaraVmTarget.emit(&lir);
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(&artifact.unwrap().bytes.unwrap()[..4], b"nara");
}

#[test]
fn union_declaration_milestone_acceptance_probes() {
    let cases = [
        ("type C = object { a: u64 b: u64, };", "E100", None),
        ("type String = union { Nope, };", "E200", None),
        ("type U = union { A, A, };", "E200", None),
        (
            "type U = union { A(Nope) }; fun ok(): u64 { return 1u64; }",
            "E105",
            None,
        ),
        ("type U = union { A(no.such.Type), };", "E302", None),
        (
            "type U = union { A, }; fun main() { val u = U {}; u; }",
            "E302",
            Some("with an object literal"),
        ),
    ];
    for (src, code, marker) in cases {
        let err = frontend(src).expect_err("acceptance probe must fail");
        let errors = err.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "{src}: {err:?}");
        assert_eq!(errors[0].code.as_deref(), Some(code), "{src}: {err:?}");
        if let Some(marker) = marker {
            assert!(errors[0].message.contains(marker), "{src}: {err:?}");
        }
    }
}

#[test]
fn unions_example_constructs_and_matches() {
    let src = std::fs::read_to_string("examples/unions.vl").unwrap();
    let lir = frontend(&src).expect("unions.vl must compile");
    assert!(lir.objects.is_empty());
    let dump = lir.dump();
    assert!(dump.contains("fn unwrap_or:"), "{dump}");
    assert!(dump.contains("fn main:"), "{dump}");
    assert!(dump.contains("new_variant Option.Some#1"), "{dump}");
    assert!(dump.contains("new_variant Option.None#0"), "{dump}");
    assert!(dump.contains("tag_of"), "{dump}");
    assert!(dump.contains("payload_get"), "{dump}");
}

#[test]
fn nullable_example_uses_sugar_without_declaring_option() {
    let src = std::fs::read_to_string("examples/nullable.vl").unwrap();
    // No `type Option` declaration needed: `?u64` / `null` rest on the
    // builtin union.
    assert!(!src.contains("type Option"), "{src}");
    let lir = frontend(&src).expect("nullable.vl must compile");
    assert!(lir.objects.is_empty());
    let dump = lir.dump();
    assert!(dump.contains("fn unwrap_or:"), "{dump}");
    assert!(dump.contains("fn main:"), "{dump}");
    assert!(dump.contains("new_variant Option.Some#1"), "{dump}");
    assert!(dump.contains("new_variant Option.None#0"), "{dump}");
    assert!(dump.contains("tag_of"), "{dump}");
    assert!(dump.contains("payload_get"), "{dump}");
}

#[test]
fn nullable_sugar_compiles_to_naravm() {
    use vl_codegen::Target;
    let src = std::fs::read_to_string("examples/nullable.vl").unwrap();
    let lir = frontend(&src).expect("nullable.vl must compile");
    let (artifact, diags) = vl_codegen::NaraVmTarget.emit(&lir);
    assert!(diags.is_empty(), "{diags:?}");
    let bytes = artifact.unwrap().bytes.unwrap();
    assert_eq!(&bytes[..4], b"nara");
    for op in [0x27u8, 0x2c, 0x2d] {
        assert!(bytes.contains(&op), "expected opcode {op:#x}");
    }
}

#[test]
fn nullable_values_flow_through_signatures_and_generics() {
    let src = r#"
fun wrap[T](v: T): ?T { return v; }
fun get(o: ?u64, fallback: u64): u64 {
  match (o) {
    Option.Some(v) { return v; }
    null { return fallback; }
  }
}
fun main() {
  val a: ?u64 = null;
  val b: ?u64 = 41u64;
  val c = wrap(b);
  c;
  val d = get(a, 0u64);
  val e = get(null, 0u64);
  d; e;
  val nested: ??u64 = null;
  nested;
  val arr: Array[?u64] = [null, 1u64];
  arr;
  if (b == null) { b; } else { b; }
  if (b != null) { b; }
}
"#;
    let lir = frontend(src).expect("nullable plumbing must compile");
    let dump = lir.dump();
    assert!(dump.contains("new_variant Option.Some#1"), "{dump}");
    assert!(dump.contains("new_variant Option.None#0"), "{dump}");
    // `wrap(?u64)` monomorphizes over the nullable union argument.
    assert!(dump.contains("fn wrap$Union_Option_u64:"), "{dump}");
    assert!(dump.contains("tag_of"), "{dump}");
}

#[test]
fn nullable_errors_are_single_root_causes() {
    let cases = [
        // Bare `null` carries no `T` to infer.
        ("fun main() { val x = null; x; }", "E303"),
        // Incompatible payload for the inner type.
        ("fun main() { val x: ?u64 = \"s\"; x; }", "E309"),
        // Only nullables compare with `null`.
        (
            "fun main() { val x = 1u64; if (x == null) { x; } }",
            "E302",
        ),
        // `?void` is not a value type.
        ("fun f(x: ?void) { x; }", "E104"),
        // `null` arm alongside `None` is a duplicate.
        (
            "fun main() { val x: ?u64 = null; match (x) { Option.None { x; } null { x; } else { x; } } }",
            "E200",
        ),
        // `null` arm on a non-union scrutinee.
        (
            "fun main() { val x = 1u64; match (x) { null { x; } else { x; } } }",
            "E302",
        ),
    ];
    for (src, code) in cases {
        let err = frontend(src).expect_err("nullable probe must fail");
        let errors = err.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "{src}: {err:?}");
        assert_eq!(errors[0].code.as_deref(), Some(code), "{src}: {err:?}");
    }
}

#[test]
fn union_construction_and_match_compile_to_naravm() {
    use vl_codegen::Target;
    let src = std::fs::read_to_string("examples/unions.vl").unwrap();
    let lir = frontend(&src).expect("unions.vl must compile");
    let (artifact, diags) = vl_codegen::NaraVmTarget.emit(&lir);
    assert!(diags.is_empty(), "{diags:?}");
    let bytes = artifact.unwrap().bytes.unwrap();
    assert_eq!(&bytes[..4], b"nara");
    // Variant containers lower to memory-container ops: `createi` (0x27)
    // allocates, `getvati` (0x2c) reads the tag/payload, `setvati` (0x2d)
    // writes them.
    for op in [0x27u8, 0x2c, 0x2d] {
        assert!(bytes.contains(&op), "expected opcode {op:#x}");
    }
}

#[test]
fn union_values_flow_through_signatures_and_generics() {
    let src = r#"
type Option[T] = union { None, Some(T), };
type U = union { A, B(u64, String), };
fun id[T](x: T): T { return x; }
fun first(o: U): u64 {
  match (o) {
    U.A { return 0u64; }
    U.B(n, s) { s; return n; }
  }
}
fun main() {
  val u: U = U.B(7u64, "seven");
  val r = first(u);
  r;
  val m: *U = U.A;
  m;
  val w = id(U.A);
  w;
  val nested: Option[U] = Option.Some(U.A);
  nested;
  val arr: Array[U] = [U.A, U.B(1u64, "x")];
  arr;
}
"#;
    let lir = frontend(src).expect("union plumbing must compile");
    let dump = lir.dump();
    assert!(dump.contains("new_variant U.B#1"), "{dump}");
    assert!(dump.contains("payload_get"), "{dump}");
}

#[test]
fn union_errors_are_single_root_causes() {
    let cases = [
        // Scrutinee is not a union.
        (
            "type U = union { A, B, }; fun main() { val x = 1u64; match (x) { U.A { x; } else { x; } } }",
            "E302",
        ),
        // Construction arity mismatch.
        (
            "type U = union { A(u64), B, }; fun main() { val u = U.A(1u64, 2u64); u; }",
            "E303",
        ),
        // Payload type mismatch (monomorphic: call-style E306).
        (
            "type U = union { A(u64), B, }; fun main() { val u = U.A(\"s\"); u; }",
            "E306",
        ),
        // Payload type mismatch (generic).
        (
            "type Option[T] = union { None, Some(T), }; fun main() { val x: Option[String] = Option.Some(1u64); x; }",
            "E306",
        ),
        // Non-exhaustive without `else`.
        (
            "type U = union { A, B, }; fun main() { val u = U.A; match (u) { U.A { 1u64; } } }",
            "E309",
        ),
        // Arm matches a different union.
        (
            "type U = union { A, B, }; type V = union { X, }; fun main() { val u = U.A; match (u) { V.X { 1u64; } else { 2u64; } } }",
            "E302",
        ),
        // Duplicate arm.
        (
            "type U = union { A, B, }; fun main() { val u = U.A; match (u) { U.A { 1u64; } U.A { 2u64; } else { 3u64; } } }",
            "E200",
        ),
        // Bare generic union needs arguments.
        (
            "type Option[T] = union { None, Some(T), }; fun main() { val x: Option = Option.Some(1u64); x; }",
            "E302",
        ),
        // Wrong number of type arguments.
        (
            "type Option[T] = union { None, Some(T), }; fun main() { val x: Option[u64, u64] = Option.Some(1u64); x; }",
            "E302",
        ),
        // Nullary generic variant without annotation cannot infer.
        (
            "type Option[T] = union { None, Some(T), }; fun main() { val n = Option.None; n; }",
            "E303",
        ),
        // Nullary use of a payload variant.
        (
            "type U = union { A(u64), }; fun main() { val u = U.A; match (u) { U.A { 1u64; } else { 2u64; } } }",
            "E303",
        ),
        // Unknown variant.
        (
            "type U = union { A, }; fun main() { val x = U.B(1u64); x; }",
            "E302",
        ),
    ];
    for (src, code) in cases {
        let err = frontend(src).expect_err("union probe must fail");
        let errors = err.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "{src}: {err:?}");
        assert_eq!(errors[0].code.as_deref(), Some(code), "{src}: {err:?}");
    }
}

#[test]
fn union_match_accepts_full_coverage_without_else() {
    let src = "type U = union { A, B(u64), }; fun f(o: U): u64 { match (o) { U.A { return 0u64; } U.B(v) { return v; } } } fun main() { val r = f(U.B(3u64)); r; }";
    let lir = frontend(src).expect("fully covered match needs no else");
    assert!(lir.dump().contains("tag_of"), "{}", lir.dump());
}

#[test]
fn union_declarations_emit_no_layouts() {
    let lir = frontend("type U = union { A, }; fun main() { }")
        .expect("declaration-only union should compile");
    assert!(lir.objects.is_empty());
    assert_eq!(
        lir.functions.len(),
        1,
        "the example's ordinary main remains"
    );
    let dump = lir.dump();
    assert!(dump.contains("fn main:"), "{dump}");
    assert!(!dump.contains("new_object"), "{dump}");
}

#[test]
fn associated_functions_compile_with_sugar_and_run_on_naravm() {
    use vl_codegen::Target;
    let src = std::fs::read_to_string("examples/associated.vl").unwrap();
    let lir = frontend(&src).expect("associated.vl must compile");
    let dump = lir.dump();
    assert!(dump.contains("fn Counter.init:"), "{dump}");
    assert!(dump.contains("fn Counter.bump:"), "{dump}");
    assert!(dump.contains("fn Counter.get:"), "{dump}");
    // `counter.bump()` sugar lowers to the explicit namespaced call.
    assert!(
        dump.contains("call") && dump.contains("Counter.bump"),
        "{dump}"
    );
    assert!(!dump.contains("MethodCall"), "{dump}");
    let (artifact, diags) = vl_codegen::NaraVmTarget.emit(&lir);
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(&artifact.unwrap().bytes.unwrap()[..4], b"nara");
}

#[test]
fn associated_generic_methods_monomorphize() {
    let lir = frontend(
        "use std; type Box = object { tag: u64, fun wrap[T](self: Box, v: T): T { return v; }, }; fun main() { var b: *Box = Box { tag = 1 }; std.print_u64(b.wrap(7)); std.print(b.wrap(\"s\")); std.print_u64(Box.wrap::[u64](b, 8)); }",
    )
    .expect("generic methods must compile");
    let dump = lir.dump();
    assert!(dump.contains("fn Box.wrap$u64:"), "{dump}");
    assert!(dump.contains("fn Box.wrap$String:"), "{dump}");
    assert!(!dump.contains("fn Box.wrap:\n"), "{dump}");
    // The receiver counts as the first call argument.
    assert!(
        dump.contains("call") && dump.contains("Box.wrap$u64"),
        "{dump}"
    );
}

#[test]
fn associated_sugar_gate_is_one_error_each() {
    for (src, code, hint) in [
        (
            "type C = object { value: u64, fun bump(self: *C): *C { return self; }, }; fun main() { val c: C = C { value = 1 }; c.bump(); }",
            "E306",
            "read-only",
        ),
        (
            "type C = object { value: u64, }; fun main() { var c: *C = C { value = 1 }; c.bump(); }",
            "E302",
            "no associated function",
        ),
        (
            "type O = object { v: u64, }; type A = object { x: u64, fun f(o: O): u64 { return o.v; }, }; fun main() { var a: *A = A { x = 1 }; a.f(); }",
            "E303",
            "first parameter",
        ),
        (
            "type C = object { value: u64, fun value(self: C): u64 { return self.value; } };",
            "E200",
            "duplicate member",
        ),
        (
            "type C = object { value: u64, }; fun main() { C.bump(); }",
            "E302",
            "no associated function",
        ),
    ] {
        let err = frontend(src).expect_err("must fail");
        assert_eq!(err.iter().filter(|d| d.is_error()).count(), 1, "{src}: {err:?}");
        assert!(
            err.iter().any(|d| d.code.as_deref() == Some(code)),
            "{src}: {err:?}"
        );
        assert!(
            err.iter().any(|d| format!("{d:?}").contains(hint)),
            "{src}: {err:?}"
        );
    }
}

#[test]
fn cross_module_associated_functions_compile_to_lir_and_naravm() {
    use vl_codegen::Target;
    let programs = frontend_project(&[
        (
            "vl.person",
            "type Person = object { name: String, age: u64, fun hello(self: Person): String { return self.name; }, fun birthday(self: *Person): *Person { self.age = self.age + 1; return self; }, fun pick[T](self: Person, a: T, b: T): T { if (self.age == 0) { return a; } return b; }, };",
        ),
        (
            "vl.main",
            "use vl.person; fun main() { var rose: *vl.person.Person = vl.person.Person { name = \"Rose\", age = 25 }; rose.birthday(); person.Person.birthday(rose); val greeting = rose.hello(); val choice = rose.pick(1u64, 2u64); greeting; choice; }",
        ),
    ])
    .expect("cross-module associated functions must compile");
    assert_eq!(programs.len(), 2);
    let provider = &programs[0];
    let importer = &programs[1];
    let provider_dump = provider.dump();
    assert!(
        provider_dump.contains("fn Person.hello:"),
        "{provider_dump}"
    );
    assert!(
        provider_dump.contains("fn Person.birthday:"),
        "{provider_dump}"
    );
    assert!(
        provider_dump.contains("fn Person.pick$u64:"),
        "{provider_dump}"
    );
    let importer_dump = importer.dump();
    // Explicit alias-qualified, fully qualified, and sugar calls all lower
    // to owner-module calls.
    assert!(
        importer_dump.contains("call vl.person::Person.birthday"),
        "{importer_dump}"
    );
    assert!(
        importer_dump.contains("call vl.person::Person.hello"),
        "{importer_dump}"
    );
    assert!(
        importer_dump.contains("call vl.person::Person.pick$u64"),
        "{importer_dump}"
    );
    assert!(
        importer
            .imports
            .iter()
            .any(|i| i.symbol.module == "vl.person" && i.symbol.function == "Person.birthday"),
        "{:?}",
        importer.imports
    );
    for lir in &programs {
        let (artifact, diags) = vl_codegen::NaraVmTarget.emit(lir);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(&artifact.unwrap().bytes.unwrap()[..4], b"nara");
    }
}

#[test]
fn qualified_receiver_never_falls_back_to_same_named_local() {
    // `vl.main` declares its own `C`, but a `vl.a.C` receiver must resolve
    // to `vl.a`'s method (nominal identity), not the local one.
    let programs = frontend_project(&[
        (
            "vl.a",
            "type C = object { v: u64, fun m(self: C): u64 { return self.v; }, };",
        ),
        (
            "vl.main",
            "type C = object { v: u64, fun m(self: C): u64 { return 7; }, }; fun main() { val x: vl.a.C = vl.a.C { v = 3 }; val y = x.m(); y; }",
        ),
    ])
    .expect("qualified sugar must resolve to the foreign method");
    assert!(
        programs[1].dump().contains("call vl.a::C.m"),
        "{}",
        programs[1].dump()
    );
    assert!(
        !programs[1].dump().contains("call vl.main::C.m"),
        "{}",
        programs[1].dump()
    );
}

#[test]
fn global_dependent_foreign_method_is_e208_through_sugar() {
    let err = frontend_project(&[
        (
            "vl.a",
            "val shared: u64 = 1; type C = object { v: u64, fun m(self: C): u64 { return shared; }, };",
        ),
        (
            "vl.main",
            "fun main() { val x: vl.a.C = vl.a.C { v = 3 }; val y = x.m(); y; }",
        ),
    ])
    .expect_err("global-dependent foreign sugar must be E208");
    assert_eq!(err.iter().filter(|d| d.is_error()).count(), 1, "{err:?}");
    assert_eq!(err[0].code.as_deref(), Some("E208"), "{err:?}");
}

#[test]
fn array_new_needs_no_import() {
    let lir =
        frontend("fun main() { val a: *Array[u64] = Array.new::[u64](2u64); a[0u64] = 1u64; }")
            .expect("Array.new must compile without imports");
    assert!(lir.dump().contains("new_array"));
}

#[test]
fn array_element_mismatch_is_one_error() {
    let err = frontend("fun main() { val a = [1, 2.0f64]; a; }").expect_err("must fail");
    assert_eq!(err.iter().filter(|d| d.is_error()).count(), 1);
    assert!(
        err.iter()
            .any(|d| d.message.contains("expects `int` elements")),
        "{err:?}"
    );
}

#[test]
fn array_index_shapes_are_checked() {
    let err =
        frontend(r#"fun main() { val s = "hi"; val x = s[0u64]; x; }"#).expect_err("must fail");
    assert!(
        err.iter().any(|d| d.message.contains("cannot index")),
        "{err:?}"
    );

    let err =
        frontend("fun main() { val a = [1u64]; val x = a[true]; x; }").expect_err("must fail");
    assert!(
        err.iter().any(|d| d.message.contains("must be `u64`")),
        "{err:?}"
    );

    let err = frontend(r#"fun main() { val a: *Array[u64] = [1u64]; a[0u64] = "s"; }"#)
        .expect_err("must fail");
    assert!(
        err.iter().any(|d| d.message.contains("cannot store")),
        "{err:?}"
    );
}

#[test]
fn bare_return_in_void_function_compiles() {
    frontend("fun main() { return; }").expect("bare return in void must compile");
}

#[test]
fn generics_example_compiles_to_instances_and_runs_on_naravm() {
    let src = std::fs::read_to_string("examples/generics.vl").unwrap();
    let lir = frontend(&src).expect("generics.vl must compile");
    let dump = lir.dump();
    for name in ["first$u64", "first$String", "second$u64"] {
        assert!(dump.contains(name), "{name} missing in {dump}");
    }
    assert!(!dump.contains("fn first:\n"), "{dump}");
    assert!(!dump.contains("fn second:\n"), "{dump}");

    use vl_codegen::Target;
    let (artifact, diags) = vl_codegen::NaraVmTarget.emit(&lir);
    assert!(diags.is_empty(), "{diags:?}");
    let bytes = artifact.unwrap().bytes.unwrap();
    assert_eq!(&bytes[..4], b"nara");
}

#[test]
fn generic_inference_and_turbofish_agree() {
    let lir = frontend(
        "fun first[T](a: Array[T]): T { return a[0u64]; } fun main() { val a = first([7u64]); val b = first::[u64]([8u64]); a; b; }",
    )
    .expect("inferred and explicit calls must compile");
    let dump = lir.dump();
    assert!(dump.contains("call first$u64"), "{dump}");
    assert!(!dump.contains("call first("), "{dump}");
}

#[test]
fn bracket_call_suggests_turbofish() {
    let err = frontend("fun main() { f[T](1u64); }").expect_err("must fail");
    let rendered =
        vl_common::diagnostic::render_all(&err, "bracket.vl", "fun main() { f[T](1u64); }");
    assert!(rendered.contains("f::[T]"), "{rendered}");
}

#[test]
fn generic_main_is_rejected() {
    let err = frontend("fun main[T]() { return; }").expect_err("must fail");
    assert!(
        err.iter()
            .any(|d| d.message.contains("must not declare type parameters")),
        "{err:?}"
    );
}

#[test]
fn annotated_let_with_contextual_new_compiles() {
    let lir = frontend(
        "use std; fun first[T](a: Array[T]): T { return a[0]; } fun main() { val scores: *Array[u64] = Array.new(3); scores[0] = 10; val number = first(scores); std.print_u64(number); }",
    )
    .expect("annotated val must compile");
    let dump = lir.dump();
    assert!(dump.contains("call first$u64"), "{dump}");
    assert!(dump.contains("new_array"), "{dump}");

    use vl_codegen::Target;
    let (artifact, diags) = vl_codegen::NaraVmTarget.emit(&lir);
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(&artifact.unwrap().bytes.unwrap()[..4], b"nara");
}

#[test]
fn inference_needs_no_annotation() {
    // The turbofish is the escape hatch; plain calls must infer.
    let lir = frontend(
        "fun first[T](a: Array[T]): T { return a[0]; } fun main() { val numbers = [10, 20]; val number = first(numbers); number; }",
    )
    .expect("inference must work");
    assert!(lir.dump().contains("call first$u64"));
}

#[test]
fn annotated_let_mismatch_is_one_error() {
    let err = frontend("fun main() { val x: u64 = \"s\"; x; }").expect_err("must fail");
    assert_eq!(err.iter().filter(|d| d.is_error()).count(), 1);
    assert!(
        err.iter().any(|d| d.code.as_deref() == Some("E309")),
        "{err:?}"
    );
}

#[test]
fn as_casts_compile_to_cast_and_run_on_naravm() {
    use vl_codegen::Target;
    let lir = frontend(
        "fun take(x: u8): u8 { return x; } fun main() { val v = 200u64; val w = take(v as u8); val lit = 10 as u8; w; lit; }",
    )
    .expect("casts must compile");
    let dump = lir.dump();
    assert!(dump.contains("cast"), "{dump}");
    let (artifact, diags) = vl_codegen::NaraVmTarget.emit(&lir);
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(&artifact.unwrap().bytes.unwrap()[..4], b"nara");
}

#[test]
fn as_cast_out_of_range_is_one_error() {
    let err = frontend("fun main() { val x = 300 as u8; x; }").expect_err("must fail");
    assert_eq!(err.iter().filter(|d| d.is_error()).count(), 1);
    assert!(
        err.iter().any(|d| d.message.contains("out of range")),
        "{err:?}"
    );
}

#[test]
fn constrained_generics_compile_to_instances_and_run_on_naravm() {
    use vl_codegen::Target;
    let lir = frontend(
        "fun add[T extends Numeric](a: T, b: T): T { return a + b; } fun eq[T extends Comparable](a: T, b: T): bool { return a == b; } fun main() { val s = add(1u64, 2u64); val ok = eq(s, 3u64); ok; }",
    )
    .expect("constrained generics must compile");
    let dump = lir.dump();
    assert!(dump.contains("call add$u64"), "{dump}");
    assert!(!dump.contains("fn add:\n"), "{dump}");
    let (artifact, diags) = vl_codegen::NaraVmTarget.emit(&lir);
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(&artifact.unwrap().bytes.unwrap()[..4], b"nara");
}

#[test]
fn unconstrained_generic_operator_is_one_error() {
    let err =
        frontend("fun add[T](a: T, b: T): T { return a + b; } fun main() { add(1u64, 2u64); }")
            .expect_err("must fail");
    assert_eq!(err.iter().filter(|d| d.is_error()).count(), 1);
    assert!(err.iter().any(|d| d.message.contains("Numeric")), "{err:?}");
}

#[test]
fn generic_array_literal_int_defers_to_u8() {
    let lir = frontend("fun same[T](a: T, b: T): T { return a; } fun main() { same([1], [2u8]); }")
        .expect("nested int must defer to u8");
    let dump = lir.dump();
    assert!(dump.contains("call same$Array_u8"), "{dump}");
    assert!(dump.contains("const 1u8"), "{dump}");
}

#[test]
fn lir_boundary_holds_no_unresolved_types() {
    let (toks, _) = vl_lex::lex("fun main() { 1 + 2; }");
    let (ast, _) = vl_syntax::parse(&toks, "");
    let (res, _) = vl_semantic::resolve(&ast);
    let hir = vl_hir::lower(&ast, &res);
    let (typed, diags) = vl_typecheck::check(&hir);
    assert!(diags.iter().all(|d| !d.is_error()));
    assert!(typed.validate_normalized(&hir, &diags).is_empty());
    let lir = vl_lir::lower(&hir, &typed);
    assert!(!lir.dump().contains("int"), "{}", lir.dump());
}

#[test]
fn stdlib_string_math_fmt_externs_emit_mapped_natives() {
    use vl_codegen::Target;
    let lir = frontend(
        "use std.string; use std.math; use std.fmt; fun main() { val s = string.concat(\"a\", \"b\"); val n = string.len(s); val ok = string.eq(s, \"ab\"); val v = string.to_u64(\"42\"); val h = string.hex_to_u64(\"2a\"); val m = math.mod_u64(7u64, 3u64); val t = fmt.u64_to_s(m); t; n; ok; v; h; }",
    )
    .expect("stdlib v1 externs must compile");
    let dump = lir.dump();
    for call in [
        "call std.string::concat",
        "call std.string::len",
        "call std.string::eq",
        "call std.string::to_u64",
        "call std.string::hex_to_u64",
        "call std.math::mod_u64",
        "call std.fmt::u64_to_s",
    ] {
        assert!(dump.contains(call), "{call} missing in {dump}");
    }
    let (artifact, diags) = vl_codegen::NaraVmTarget.emit(&lir);
    assert!(diags.is_empty(), "{diags:?}");
    let bytes = artifact.unwrap().bytes.unwrap();
    assert_eq!(&bytes[..4], b"nara");
    // VL dotted names lower to the VM's `::` natives (e.g. `len` is
    // `byte_count` on the wire).
    for marker in [
        "std::string",
        "concat",
        "byte_count",
        "eq",
        "to_u64",
        "hex_to_u64",
        "std::math",
        "mod_u64",
        "std::fmt",
        "u64_to_s",
    ] {
        assert!(
            bytes.windows(marker.len()).any(|w| w == marker.as_bytes()),
            "{marker} missing"
        );
    }
    // The VL-module spelling must not leak into the artifact.
    assert!(!bytes
        .windows(b"std.string".len())
        .any(|w| w == b"std.string"));
}

#[test]
fn stdlib_extern_arg_types_are_checked() {
    let err = frontend("use std.string; fun main() { string.concat(1u64, \"b\"); }")
        .expect_err("concat expects Strings");
    assert!(
        err.iter().any(|d| d.message.contains("expects `String`")),
        "{err:?}"
    );
    let err = frontend("use std.math; fun main() { math.mod_u64(1u64); }")
        .expect_err("mod_u64 expects two args");
    assert!(
        err.iter().any(|d| d.message.contains("expects 2")),
        "{err:?}"
    );
}

#[test]
fn stdlib_helpers_link_lazily_behind_explicit_use() {
    use vl_codegen::Target;
    let lir = frontend("use std; use std.math; use std.fmt; fun main() { std.print(fmt.u64_to_string(math.max_u64(3u64, 9u64))); std.print_u64(math.clamp_u64(100u64, 0u64, 10u64)); }")
        .expect("stdlib helpers must link");
    let dump = lir.dump();
    for name in [
        "fn std$math$max_u64:",
        "fn std$fmt$u64_to_string:",
        "fn std$math$clamp_u64:",
    ] {
        assert!(dump.contains(name), "{name} missing in {dump}");
    }
    // Unreferenced helpers stay out; nothing is implicitly in scope.
    assert!(!dump.contains("std$math$is_even"), "{dump}");
    assert!(!dump.contains("std.math::"), "{dump}");
    let (artifact, diags) = vl_codegen::NaraVmTarget.emit(&lir);
    assert!(diags.is_empty(), "{diags:?}");
    let bytes = artifact.unwrap().bytes.unwrap();
    assert_eq!(&bytes[..4], b"nara");
    assert!(bytes.contains(&0x20), "expected calli instructions");
}

#[test]
fn stdlib_helpers_need_an_explicit_use() {
    let err = frontend("fun main() { val m = math.max_u64(1u64, 2u64); m; }")
        .expect_err("helpers need `use std.math`");
    assert_eq!(err.iter().filter(|d| d.is_error()).count(), 1, "{err:?}");
}

#[test]
fn stdlib_example_compiles_and_runs_on_naravm() {
    use vl_codegen::Target;
    let src = std::fs::read_to_string("examples/stdlib.vl").unwrap();
    let lir = frontend(&src).expect("stdlib.vl must compile");
    let dump = lir.dump();
    assert!(dump.contains("call std.string::concat"), "{dump}");
    assert!(dump.contains("call std$math$max_u64"), "{dump}");
    let (artifact, diags) = vl_codegen::NaraVmTarget.emit(&lir);
    assert!(diags.is_empty(), "{diags:?}");
    let bytes = artifact.unwrap().bytes.unwrap();
    assert_eq!(&bytes[..4], b"nara");
    assert!(bytes.contains(&0x20), "expected calli instructions");
}

#[test]
fn cross_module_generic_id_for_two_types() {
    use vl_codegen::Target;
    let programs = frontend_project(&[
        ("demo.lib", "fun id[T](value: T): T { return value; }"),
        (
            "demo.main",
            "use demo.lib.id; fun main() { val a = id(1u64); val b = id::[String](\"hi\"); a; b; }",
        ),
    ])
    .expect("cross-module generics must compile");
    assert_eq!(programs.len(), 2);
    let provider = &programs[0];
    let importer = &programs[1];
    let provider_dump = provider.dump();
    assert!(provider_dump.contains("fn id$u64:"), "{provider_dump}");
    assert!(provider_dump.contains("fn id$String:"), "{provider_dump}");
    assert!(!provider_dump.contains("fn id:\n"), "{provider_dump}");
    let importer_dump = importer.dump();
    assert!(
        importer_dump.contains("call demo.lib::id$u64"),
        "{importer_dump}"
    );
    assert!(
        importer_dump.contains("call demo.lib::id$String"),
        "{importer_dump}"
    );
    assert!(
        !importer_dump.contains("call demo.lib::id("),
        "{importer_dump}"
    );
    // Imports are concrete with concrete signatures.
    assert!(importer
        .imports
        .iter()
        .any(|i| i.symbol.module == "demo.lib" && i.symbol.function == "id$u64"));
    for lir in &programs {
        let (artifact, diags) = vl_codegen::NaraVmTarget.emit(lir);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(&artifact.unwrap().bytes.unwrap()[..4], b"nara");
    }
}

#[test]
fn cross_module_generic_forwarding_chain() {
    let programs = frontend_project(&[
        ("demo.lib", "fun id[T](value: T): T { return value; }"),
        (
            "demo.mid",
            "use demo.lib; fun wrap[T](x: T): T { return lib.id(x); }",
        ),
        (
            "demo.main",
            "use demo.mid; fun main() { val x = mid.wrap(1u64); }",
        ),
    ])
    .expect("forwarding chain must compile");
    assert!(programs[2].dump().contains("call demo.mid::wrap$u64"));
    assert!(programs[1].dump().contains("call demo.lib::id$u64"));
    assert!(programs[0].dump().contains("fn id$u64:"));
}

#[test]
fn cross_module_generic_import_forms_agree() {
    for src in [
        "use demo.lib; fun main() { val x = lib.id(1u64); }",
        "use demo.lib.id; fun main() { val x = id(1u64); }",
        "use demo.lib.{id}; fun main() { val x = id(1u64); }",
    ] {
        let programs = frontend_project(&[
            ("demo.lib", "fun id[T](value: T): T { return value; }"),
            ("demo.main", src),
        ])
        .expect("all import forms must compile");
        assert!(programs[1].dump().contains("id$u64"), "{src}");
    }
}

#[test]
fn cross_module_generic_dumps_are_deterministic() {
    let units = [
        ("demo.lib", "fun id[T](value: T): T { return value; }"),
        (
            "demo.main",
            "use demo.lib.id; fun main() { val a = id(1u64); val b = id(\"hi\"); a; b; }",
        ),
    ];
    let a = frontend_project(&units).expect("must compile");
    let mut reversed = units.to_vec();
    reversed.reverse();
    // `frontend_project` preserves input order for outputs but the plan is
    // sorted; provider instances must match regardless of source order.
    let b = frontend_project(&reversed).expect("must compile");
    let (a_lib, a_main) = (&a[0].dump(), &a[1].dump());
    // Find lib/main by module, not position (reversed input swaps positions).
    let (b_lib, b_main) = if b[0].module == "demo.lib" {
        (&b[0].dump(), &b[1].dump())
    } else {
        (&b[1].dump(), &b[0].dump())
    };
    // Compare as sets of lines (instruction order within functions is stable;
    // function order is sorted, so dumps must be identical).
    assert_eq!(a_lib, b_lib);
    assert_eq!(a_main, b_main);
}

#[test]
fn stdlib_generic_max_infers_and_links_lazily() {
    use vl_codegen::Target;
    let lir = frontend(
        "use std.math; fun main() { val a = math.max(3u64, 9u64); val b = math.max::[i64](-1i64, 5i64); a; b; }",
    )
    .expect("stdlib generic max must compile");
    let dump = lir.dump();
    assert!(dump.contains("std$math$max$u64"), "{dump}");
    assert!(dump.contains("std$math$max$i64"), "{dump}");
    assert!(!dump.contains("std.math::max("), "{dump}");
    assert!(!dump.contains("std$math$is_even"), "{dump}");
    let (artifact, diags) = vl_codegen::NaraVmTarget.emit(&lir);
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(&artifact.unwrap().bytes.unwrap()[..4], b"nara");
}

#[test]
fn stdlib_generic_max_reuses_one_instance() {
    let lir = frontend(
        "use std.math; fun main() { val a = math.max(1u64, 2u64); val b = math.max(3u64, 4u64); a; b; }",
    )
    .expect("must compile");
    assert_eq!(lir.dump().matches("fn std$math$max$u64:").count(), 1);
}

#[test]
fn stdlib_generic_bound_violation_is_one_error() {
    let err =
        frontend("use std.math; fun main() { math.max(\"a\", \"b\"); }").expect_err("must fail");
    assert_eq!(err.iter().filter(|d| d.is_error()).count(), 1, "{err:?}");
    assert_eq!(err[0].code.as_deref(), Some("E303"), "{err:?}");
}

#[test]
fn project_generic_calls_stdlib_generic_transitively() {
    let programs = frontend_project(&[
        (
            "demo.lib",
            "use std.math; fun double_max[T extends Numeric](a: T, b: T): T { return math.max(a, b); }",
        ),
        (
            "demo.main",
            "use demo.lib; fun main() { val m = lib.double_max(3u64, 9u64); }",
        ),
    ])
    .expect("project->stdlib generics must compile");
    let lib_dump = programs[0].dump();
    assert!(lib_dump.contains("double_max$u64"), "{lib_dump}");
    // The stdlib instance links into the consumer (no cross-artifact std call).
    assert!(
        lib_dump.contains("std$math$max$u64"),
        "stdlib instance must link locally: {lib_dump}"
    );
    assert!(!lib_dump.contains("std.math::max$"), "{lib_dump}");
}

#[test]
fn stdlib_generic_does_not_pull_unused_natives() {
    let lir = frontend("use std.math; fun main() { val m = math.max(1u64, 2u64); m; }")
        .expect("must compile");
    // `max` needs no natives; `mod_u64` (used only by `is_even`) must stay out.
    assert!(!lir.dump().contains("mod_u64"), "{}", lir.dump());
    assert!(
        !lir.imports.iter().any(|i| i.symbol.function == "mod_u64"),
        "{:?}",
        lir.imports
    );
}

#[test]
fn tuples_match_golden_lir() {
    let src = std::fs::read_to_string("examples/tuples.vl").unwrap();
    let lir = frontend(&src).expect("tuples.vl must compile");
    let golden = std::fs::read_to_string("tests/golden/tuples.lir").unwrap();
    assert_eq!(lir.dump(), golden);
}

#[test]
fn tuples_compile_and_run_on_naravm() {
    use vl_codegen::Target;
    let src = std::fs::read_to_string("examples/tuples.vl").unwrap();
    let lir = frontend(&src).expect("tuples.vl must compile");
    let dump = lir.dump();
    assert!(dump.contains("tuple_lit"), "{dump}");
    assert!(dump.contains("tuple_get"), "{dump}");
    assert!(dump.contains("tuple_set"), "{dump}");
    let (artifact, diags) = vl_codegen::NaraVmTarget.emit(&lir);
    assert!(diags.is_empty(), "{diags:?}");
    let bytes = artifact.unwrap().bytes.unwrap();
    assert_eq!(&bytes[..4], b"nara");
    // Tuple containers lower to memory-container ops: `createi` (0x27)
    // allocates, `getvati` (0x2c) reads, `setvati` (0x2d) writes.
    for op in [0x27u8, 0x2c, 0x2d] {
        assert!(bytes.contains(&op), "no {op:#x} in {bytes:?}");
    }
}

#[test]
fn tuple_copies_do_not_alias() {
    let lir = frontend("fun main() { var a = #(1u64, 2u64); var b = a; b.`0 = 99u64; }")
        .expect("tuple copy must compile");
    let dump = lir.dump();
    // One literal plus a copy (bind) and a positional write.
    assert!(dump.contains("tuple_lit"), "{dump}");
    assert!(dump.contains("tuple_set"), "{dump}");
}

#[test]
fn err_tuple_examples_fail() {
    for (file, code) in [
        ("examples/err_tuple_arity.vl", "E309"),
        ("examples/err_tuple_index.vl", "E302"),
        ("examples/err_tuple_readonly.vl", "E310"),
    ] {
        let src = std::fs::read_to_string(file).unwrap();
        let err = frontend(&src).expect_err(&format!("{file} must fail"));
        assert_eq!(
            err.iter().filter(|d| d.is_error()).count(),
            1,
            "{file}: {err:?}"
        );
        assert!(
            err.iter().any(|d| d.code.as_deref() == Some(code)),
            "{file}: {err:?}"
        );
    }
}

#[test]
fn err_union_examples_fail() {
    for (file, code) in [
        ("examples/err_union_match.vl", "E309"),
        ("examples/err_union_arity.vl", "E303"),
    ] {
        let src = std::fs::read_to_string(file).unwrap();
        let err = frontend(&src).expect_err(&format!("{file} must fail"));
        assert_eq!(
            err.iter().filter(|d| d.is_error()).count(),
            1,
            "{file}: {err:?}"
        );
        assert!(
            err.iter().any(|d| d.code.as_deref() == Some(code)),
            "{file}: {err:?}"
        );
    }
}

#[test]
fn brace_imported_object_type_calls_annotations_and_sugar_compile() {
    use vl_codegen::Target;
    let programs = frontend_project(&[
        (
            "vl.dog",
            "use std; type Dog = object { name: String, fun new(name: String): *Dog { return Dog { name = name, }; } fun barf(self: Dog) { std.println(\"BARF!\"); } };",
        ),
        (
            "vl.main",
            "use std; use vl.dog.{Dog}; fun main() { val dog = Dog.new(\"Doug\"); dog.barf(); val lit: Dog = Dog { name = \"Lit\" }; val back: *Dog = Dog.new(\"Bo\"); back.barf(); lit; }",
        ),
    ])
    .expect("brace-imported object types must compile");
    assert_eq!(programs.len(), 2);
    let importer_dump = programs[1].dump();
    assert!(
        importer_dump.contains("call vl.dog::Dog.new"),
        "{importer_dump}"
    );
    assert!(
        importer_dump.contains("call vl.dog::Dog.barf"),
        "{importer_dump}"
    );
    assert!(
        programs[1]
            .imports
            .iter()
            .any(|i| i.symbol.module == "vl.dog" && i.symbol.function == "Dog.new"),
        "{:?}",
        programs[1].imports
    );
    for lir in &programs {
        let (artifact, diags) = vl_codegen::NaraVmTarget.emit(lir);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(&artifact.unwrap().bytes.unwrap()[..4], b"nara");
    }
}

#[test]
fn single_imported_object_type_compiles() {
    let programs = frontend_project(&[
        (
            "vl.dog",
            "type Dog = object { name: String, fun new(name: String): *Dog { return Dog { name = name, }; } };",
        ),
        (
            "vl.main",
            "use vl.dog.Dog; fun main() { val dog: *Dog = Dog.new(\"Doug\"); dog; }",
        ),
    ])
    .expect("single-imported object types must compile");
    assert!(programs[1].dump().contains("call vl.dog::Dog.new"));
}

#[test]
fn brace_imported_union_constructs_and_matches() {
    let programs = frontend_project(&[
        ("vl.shapes", "type Shape = union { Circle(u64), Point, };"),
        (
            "vl.main",
            "use vl.shapes.{Shape}; fun area(s: Shape): u64 { match (s) { Shape.Circle(r) { return r; } Shape.Point { return 0u64; } } } fun main() { val a = Shape.Circle(3u64); val b: Shape = Shape.Point; val r = area(a); r; b; }",
        ),
    ])
    .expect("brace-imported unions must compile");
    let dump = programs[1].dump();
    assert!(dump.contains("Circle#0"), "{dump}");
    assert!(dump.contains("Point#1"), "{dump}");
    assert!(dump.contains("vl.shapes.Shape"), "{dump}");
}

#[test]
fn brace_type_import_errors_are_single_root_causes() {
    for (units, code) in [
        (
            vec![
                ("vl.dog", "type Dog = object { name: String, };"),
                ("vl.main", "use vl.dog.{Missing}; fun main() { }"),
            ],
            "E203",
        ),
        (
            vec![
                ("vl.a", "type Dog = object { v: u64, };"),
                ("vl.b", "type Dog = object { v: u64, };"),
                ("vl.main", "use vl.a.{Dog}; use vl.b.{Dog}; fun main() { }"),
            ],
            "E206",
        ),
        (
            vec![
                ("vl.dog", "type Dog = object { v: u64, };"),
                (
                    "vl.main",
                    "use vl.dog.{Dog}; type Dog = object { v: u64, }; fun main() { }",
                ),
            ],
            "E206",
        ),
    ] {
        let units: Vec<(&str, &str)> = units;
        let err = frontend_project(&units).expect_err("type import probe must fail");
        let errors = err.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "{units:?}: {err:?}");
        assert_eq!(errors[0].code.as_deref(), Some(code), "{units:?}: {err:?}");
    }
}

#[test]
fn errors_example_wraps_propagates_and_catches_to_naravm() {
    use vl_codegen::Target;
    let src = std::fs::read_to_string("examples/errors.vl").unwrap();
    let lir = frontend(&src).expect("errors.vl must compile");
    let dump = lir.dump();
    for op in ["wrap_ok", "wrap_err", "tag_of", "unwrap_ok", "unwrap_err"] {
        assert!(dump.contains(op), "{op} missing in {dump}");
    }
    let (artifact, diags) = vl_codegen::NaraVmTarget.emit(&lir);
    assert!(diags.is_empty(), "{diags:?}");
    let bytes = artifact.unwrap().bytes.unwrap();
    assert_eq!(&bytes[..4], b"nara");
    // Fallible containers lower to memory-container ops: `createi` (0x27)
    // allocates, `getvati` (0x2c) reads the tag/payload/code, `setvati`
    // (0x2d) writes them.
    for op in [0x27u8, 0x2c, 0x2d] {
        assert!(bytes.contains(&op), "expected opcode {op:#x}");
    }
}

#[test]
fn inferred_error_sets_flow_into_named() {
    let lir = frontend(
        "type E = error { A, }; fun g(): !u64 { return 1u64; } fun f(): E!u64 { val x = try g(); return x; } fun main() { val v = f() catch 0u64; v; }",
    )
    .expect("inferred-to-named must compile");
    assert!(lir.dump().contains("wrap_err"), "{}", lir.dump());
}

#[test]
fn fallible_void_side_effects_compile_to_naravm() {
    use vl_codegen::Target;
    let lir = frontend(
        "use std; type E = error { A, }; fun v(): E!void { return; } fun w(): E!void { } fun main() { v() catch std.print(\"a\"); w() catch std.print(\"b\"); }",
    )
    .expect("E!void must compile");
    assert!(lir.dump().contains("wrap_ok"), "{}", lir.dump());
    let (artifact, diags) = vl_codegen::NaraVmTarget.emit(&lir);
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(&artifact.unwrap().bytes.unwrap()[..4], b"nara");
}

#[test]
fn err_error_examples_fail() {
    for (file, code) in [
        ("examples/err_try_type.vl", "E304"),
        ("examples/err_unhandled.vl", "E309"),
    ] {
        let src = std::fs::read_to_string(file).unwrap();
        let err = frontend(&src).expect_err("{file} must fail");
        let errors = err.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "{file}: {err:?}");
        assert_eq!(errors[0].code.as_deref(), Some(code), "{file}: {err:?}");
    }
}

#[test]
fn fallible_misuse_is_single_root_causes() {
    for (src, code) in [
        ("fun main() { val x = try 5u64; x; }", "E304"),
        ("fun main() { val x = 5u64 catch 0u64; x; }", "E304"),
        (
            "type E = error { A, }; fun f(): E!u64 { return 1u64; } fun g(): u64 { return try f(); } fun main() { g(); }",
            "E307",
        ),
        (
            "type A = error { X, }; type B = error { Y, }; fun f(): A!u64 { return B.Y; } fun main() { val v = f() catch 0u64; v; }",
            "E307",
        ),
        (
            "type E = error { A, }; fun f(): E!u64 { return E.A; } fun main() { val x = f() catch \"s\"; x; }",
            "E309",
        ),
        (
            "type E = error { A, }; fun main() { val x: E = E.Bogus; x; }",
            "E302",
        ),
        ("fun f(): Bogus!u64 { return 1u64; } fun main() { val v = f() catch 0u64; v; }", "E105"),
        (
            "type E = error { A, }; fun main() { val x = E.A(1u64); x; }",
            "E303",
        ),
    ] {
        let err = frontend(src).expect_err("fallible probe must fail");
        let errors = err.iter().filter(|d| d.is_error()).collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "{src}: {err:?}");
        assert_eq!(errors[0].code.as_deref(), Some(code), "{src}: {err:?}");
    }
}

#[test]
fn cross_module_error_sets_compile_to_lir_and_naravm() {
    use vl_codegen::Target;
    let programs = frontend_project(&[
        (
            "vl.io",
            "type Io = error { NotFound, Denied, }; fun read(n: u64): Io!u64 { if (n == 0u64) { return Io.NotFound; } return n; }",
        ),
        (
            "vl.main",
            "use std; use vl.io; fun main() { std.print_u64(io.read(3u64) catch 100u64); std.print_u64(io.read(0u64) catch 101u64); val e: vl.io.Io = vl.io.Io.Denied; e; }",
        ),
    ])
    .expect("cross-module errors must compile");
    assert_eq!(programs.len(), 2);
    let importer_dump = programs[1].dump();
    assert!(
        importer_dump.contains("call vl.io::read"),
        "{importer_dump}"
    );
    assert!(
        importer_dump.contains("wrap_err") || importer_dump.contains("unwrap_ok"),
        "{importer_dump}"
    );
    for lir in &programs {
        let (artifact, diags) = vl_codegen::NaraVmTarget.emit(lir);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(&artifact.unwrap().bytes.unwrap()[..4], b"nara");
    }
}

#[test]
fn brace_imported_error_sets_construct() {
    let programs = frontend_project(&[
        ("vl.io", "type Io = error { NotFound, };"),
        (
            "vl.main",
            "use vl.io.{Io}; fun f(): Io!u64 { return Io.NotFound; } fun main() { val v = f() catch 0u64; v; }",
        ),
    ])
    .expect("brace-imported error sets must compile");
    assert!(
        programs[1].dump().contains("wrap_err"),
        "{}",
        programs[1].dump()
    );
}
