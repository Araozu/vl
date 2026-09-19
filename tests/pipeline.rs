//! Integration tests: drive the whole pipeline like the driver does.
//! Run with `cargo test --workspace` (or `./scripts/check.sh`).

fn frontend(src: &str) -> Result<vl_lir::LirProgram, Vec<vl_common::Diagnostic>> {
    let (toks, mut diags) = vl_lex::lex(src);
    let (ast, mut d) = vl_syntax::parse(&toks, src);
    diags.append(&mut d);
    let (res, mut d) = vl_semantic::resolve(&ast);
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
    assert!(
        dump.contains("mul"),
        "expected precedence: 2*3 first\n{dump}"
    );
    assert!(dump.contains("add"), "{dump}");
    assert!(dump.contains("ret"), "{dump}");
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
    assert!(text.contains("add") && text.contains("ret"), "{text}");
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
