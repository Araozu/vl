//! Integration tests: drive the whole pipeline like the driver does.
//! Run with `cargo test --workspace` (or `./scripts/check.sh`).

fn frontend(src: &str) -> Result<vl_lir::LirProgram, Vec<vl_common::Diagnostic>> {
    let (toks, mut diags) = vl_lex::lex(src);
    let (ast, mut d) = vl_syntax::parse(&toks, src);
    diags.append(&mut d);
    let (res, mut d) = vl_semantic::resolve_with_modules(&ast, &vl_codegen::modules());
    diags.append(&mut d);
    let hir = vl_hir::lower(&ast, &res);
    let (typed, mut d) = vl_typecheck::check(&hir);
    diags.append(&mut d);
    if diags.iter().any(|d| d.is_error()) {
        return Err(diags);
    }
    Ok(vl_lir::lower(&hir, &typed))
}

#[test]
fn hello_compiles_to_lir() {
    let src = std::fs::read_to_string("examples/hello.vl").unwrap();
    let lir = frontend(&src).expect("hello.vl must compile");
    let dump = lir.dump();
    assert!(dump.contains("call std.print"), "{dump}");
    assert!(dump.contains("ret"), "{dump}");
}

#[test]
fn println_compiles_and_runs_on_naravm() {
    use vl_codegen::Target;
    let lir = frontend("use std; function main() { std.println(\"hi\"); std.print(\"x\\n\"); }")
        .expect("println must compile");
    let dump = lir.dump();
    assert!(dump.contains("call std.println"), "{dump}");
    let (artifact, diags) = vl_codegen::NaraVmTarget.emit(&lir);
    assert!(diags.is_empty(), "{diags:?}");
    let bytes = artifact.unwrap().bytes.unwrap();
    assert_eq!(&bytes[..4], b"nara");
    assert!(bytes.contains(&0x20), "expected calli instructions");
}

#[test]
fn println_arg_types_are_checked() {
    let err = frontend("use std; function main() { std.println(1); }")
        .expect_err("println expects string");
    assert!(
        err.iter().any(|d| d.message.contains("expects `string`")),
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
fn dummy_backend_emits_pseudo_asm() {
    use vl_codegen::Target;
    let src = std::fs::read_to_string("examples/hello.vl").unwrap();
    let lir = frontend(&src).unwrap();
    let (art, diags) = vl_codegen::DummyTarget.emit(&lir);
    assert!(diags.is_empty());
    let text = art.unwrap().text;
    assert!(text.contains("std.print") && text.contains("ret"), "{text}");
}

#[test]
fn function_calls_lower_to_lir_and_asm() {
    let src = std::fs::read_to_string("examples/calls.vl").unwrap();
    let lir = frontend(&src).expect("calls.vl must compile");
    let dump = lir.dump();
    assert!(dump.contains("%0 = param 0"), "{dump}");
    assert!(dump.contains("call add(%0, %0)"), "{dump}");
    assert!(dump.contains("call twice(%2)"), "{dump}");

    use vl_codegen::Target;
    let (artifact, diags) = vl_codegen::DummyTarget.emit(&lir);
    assert!(diags.is_empty());
    assert!(artifact.unwrap().text.contains("call"));
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
function add(a: u64, b: u64): u64 { return a + b; }
function greet(name: string): string { return name; }
function fact(n: u64): u64 {
    let r = 1u64;
    if (n == 0u64) { r; } else { r = n * fact(n - 1u64); }
    return r;
}
function main() {
    std.print(greet("hi\n"));
    std.print_u64(add(fact(3u64), 1u64));
}
"#,
    )
    .expect("mixed params, string return, and recursion must compile");
    let (artifact, diags) = vl_codegen::NaraVmTarget.emit(&lir);
    assert!(diags.is_empty(), "{diags:?}");
    let bytes = artifact.unwrap().bytes.unwrap();
    assert_eq!(&bytes[..4], b"nara");
    assert!(bytes.contains(&0x20), "expected calli instructions");
}

#[test]
fn strings_lower_to_byte_constants() {
    let lir = frontend(r#"let greeting = "hi\n";"#).expect("string must compile");
    let dump = lir.dump();
    assert!(dump.contains("string [104, 105, 10]"), "{dump}");
}

#[test]
fn unterminated_string_is_a_lex_error() {
    let err = frontend("let x = \"not closed\nlet y = 1;").expect_err("must fail");
    assert!(err
        .iter()
        .any(|d| d.message.contains("unterminated string")));
}

#[test]
fn module_imports_resolve_without_importing_descendants() {
    let src = std::fs::read_to_string("examples/modules.vl").unwrap();
    let lir = frontend(&src).expect("module imports must compile");
    let dump = lir.dump();
    assert!(dump.contains("call string.len"), "{dump}");
    assert!(dump.contains("call open"), "{dump}");
    assert!(dump.contains("call read"), "{dump}");
}

#[test]
fn unknown_module_export_is_a_single_error() {
    let (toks, _) = vl_lex::lex("use std.string.{missing}; function main() { missing(); }");
    let (ast, _) = vl_syntax::parse(&toks, "");
    let (_, diags) = vl_semantic::resolve(&ast);
    assert!(diags
        .iter()
        .any(|d| d.message.contains("no export `missing`")));
}

#[test]
fn scalar_literals_and_if_lower_to_typed_control_flow() {
    let lir =
        frontend("function main() { let x = 1u64; if (true) { x; } else { 255u8; } 1.5f64; }")
            .expect("scalar literals and if must compile");
    let dump = lir.dump();
    assert!(dump.contains("const 1u64"), "{dump}");
    assert!(dump.contains("const 1.5f64"), "{dump}");
    assert!(dump.contains("branch_if_false"), "{dump}");
    assert!(dump.contains("L0:"), "{dump}");
}

#[test]
fn if_requires_a_boolean_condition() {
    let err = frontend("function main() { if (1) { 2; } }").expect_err("if condition must be bool");
    assert!(
        err.iter().any(|d| d.message.contains("must be bool")),
        "{err:?}"
    );
}

#[test]
fn unbraced_conditional_branches_compile() {
    let lir = frontend("function main() { if (true) 1u64; else 2u64; }")
        .expect("unbraced branches must compile");
    assert!(lir.dump().contains("branch_if_false"));
}

#[test]
fn extern_call_arg_types_are_checked() {
    let err =
        frontend("use std; function main() { std.print(1); }").expect_err("print expects string");
    assert!(
        err.iter().any(|d| d.message.contains("expects `string`")),
        "{err:?}"
    );
}

#[test]
fn extern_call_arity_is_checked() {
    let err = frontend("use std; function main() { std.print(\"a\", \"b\"); }")
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
        "function main() { let a = 1; if (a <= 2 && a != 3 || !(a > 9)) { a; } while (a >= 1) { a = a - 1; } }",
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
    let err = frontend("function main() { break; }").expect_err("must fail");
    assert_eq!(err.iter().filter(|d| d.is_error()).count(), 1);
    assert!(err[0].message.contains("outside of a loop"));
}

#[test]
fn continue_outside_a_loop_is_one_error() {
    let err = frontend("function main() { continue; }").expect_err("must fail");
    assert_eq!(err.iter().filter(|d| d.is_error()).count(), 1);
}

#[test]
fn assignment_type_mismatch_is_one_error() {
    let err = frontend(r#"function main() { let x = 1; x = "s"; }"#).expect_err("must fail");
    assert_eq!(err.iter().filter(|d| d.is_error()).count(), 1);
    assert!(err.iter().any(|d| d.code.as_deref() == Some("E309")));
}

#[test]
fn while_condition_must_be_bool() {
    let err = frontend("function main() { while (1) { 2; } }").expect_err("must fail");
    assert!(err
        .iter()
        .any(|d| d.message.contains("while condition must be bool")));
}

#[test]
fn missing_annotations_are_an_error() {
    let err = frontend("function add(a, b) { return a + b; }").expect_err("must fail");
    assert!(
        err.iter().any(|d| d.code.as_deref() == Some("E104")),
        "{err:?}"
    );
}

#[test]
fn explicit_return_compiles_and_lowers_to_ret() {
    let lir = frontend(
        "function add(a: i64, b: i64): i64 { return a + b; } function main() { add(1, 2); }",
    )
    .expect("explicit return must compile");
    let dump = lir.dump();
    assert!(dump.contains("add"), "{dump}");
    assert!(dump.contains("ret"), "{dump}");
}

#[test]
fn missing_return_is_an_error() {
    let err = frontend("function f(): i64 { let x = 1; }").expect_err("must fail");
    assert!(
        err.iter().any(|d| d.code.as_deref() == Some("E307")),
        "{err:?}"
    );
}

#[test]
fn trailing_expr_is_not_an_implicit_return() {
    let err = frontend(r#"function f(): i64 { 1; }"#).expect_err("must fail");
    assert!(
        err.iter()
            .any(|d| d.message.contains("not all paths return")),
        "{err:?}"
    );
}

#[test]
fn bare_return_in_value_function_is_an_error() {
    let err = frontend("function f(): i64 { return; }").expect_err("must fail");
    assert!(
        err.iter().any(|d| d.message.contains("returns nothing")),
        "{err:?}"
    );
}

#[test]
fn value_return_in_void_function_is_an_error() {
    let err = frontend("function main() { return 1; }").expect_err("must fail");
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
fn array_new_needs_no_import() {
    let lir = frontend("function main() { let a = Array.new::[u64](2u64); a[0u64] = 1u64; }")
        .expect("Array.new must compile without imports");
    assert!(lir.dump().contains("new_array"));
}

#[test]
fn array_element_mismatch_is_one_error() {
    let err = frontend("function main() { let a = [1, 2.0f64]; a; }").expect_err("must fail");
    assert_eq!(err.iter().filter(|d| d.is_error()).count(), 1);
    assert!(
        err.iter()
            .any(|d| d.message.contains("expects `int` elements")),
        "{err:?}"
    );
}

#[test]
fn array_index_shapes_are_checked() {
    let err = frontend(r#"function main() { let s = "hi"; let x = s[0u64]; x; }"#)
        .expect_err("must fail");
    assert!(
        err.iter().any(|d| d.message.contains("cannot index")),
        "{err:?}"
    );

    let err =
        frontend("function main() { let a = [1u64]; let x = a[true]; x; }").expect_err("must fail");
    assert!(
        err.iter().any(|d| d.message.contains("must be `u64`")),
        "{err:?}"
    );

    let err =
        frontend(r#"function main() { let a = [1u64]; a[0u64] = "s"; }"#).expect_err("must fail");
    assert!(
        err.iter().any(|d| d.message.contains("cannot store")),
        "{err:?}"
    );
}

#[test]
fn bare_return_in_void_function_compiles() {
    frontend("function main() { return; }").expect("bare return in void must compile");
}

#[test]
fn generics_example_compiles_to_instances_and_runs_on_naravm() {
    let src = std::fs::read_to_string("examples/generics.vl").unwrap();
    let lir = frontend(&src).expect("generics.vl must compile");
    let dump = lir.dump();
    for name in ["first$u64", "first$string", "second$u64"] {
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
        "function first[T](a: Array[T]): T { return a[0u64]; } function main() { let a = first([7u64]); let b = first::[u64]([8u64]); a; b; }",
    )
    .expect("inferred and explicit calls must compile");
    let dump = lir.dump();
    assert!(dump.contains("call first$u64"), "{dump}");
    assert!(!dump.contains("call first("), "{dump}");
}

#[test]
fn bracket_call_suggests_turbofish() {
    let err = frontend("function main() { f[T](1u64); }").expect_err("must fail");
    let rendered =
        vl_common::diagnostic::render_all(&err, "bracket.vl", "function main() { f[T](1u64); }");
    assert!(rendered.contains("f::[T]"), "{rendered}");
}

#[test]
fn generic_main_is_rejected() {
    let err = frontend("function main[T]() { return; }").expect_err("must fail");
    assert!(
        err.iter()
            .any(|d| d.message.contains("must not declare type parameters")),
        "{err:?}"
    );
}

#[test]
fn annotated_let_with_contextual_new_compiles() {
    let lir = frontend(
        "use std; function first[T](a: Array[T]): T { return a[0]; } function main() { let scores: Array[u64] = Array.new(3); scores[0] = 10; let number = first(scores); std.print_u64(number); }",
    )
    .expect("annotated let must compile");
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
        "function first[T](a: Array[T]): T { return a[0]; } function main() { let numbers = [10, 20]; let number = first(numbers); number; }",
    )
    .expect("inference must work");
    assert!(lir.dump().contains("call first$u64"));
}

#[test]
fn annotated_let_mismatch_is_one_error() {
    let err = frontend("function main() { let x: u64 = \"s\"; x; }").expect_err("must fail");
    assert_eq!(err.iter().filter(|d| d.is_error()).count(), 1);
    assert!(
        err.iter().any(|d| d.code.as_deref() == Some("E309")),
        "{err:?}"
    );
}
