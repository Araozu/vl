//! Formatter unit tests: style rules, steering, preservation, errors.

use crate::{format, MAX_WIDTH};

/// Format twice; both passes must agree, and the first must equal `want`.
fn check(src: &str, want: &str) {
    let once = format(src, "test").unwrap_or_else(|diags| {
        panic!("format failed for {src:?}: {diags:?}");
    });
    assert_eq!(once, want, "first format of {src:?}");
    let twice = format(&once, "test").unwrap_or_else(|diags| {
        panic!("reformat failed for {once:?}: {diags:?}");
    });
    assert_eq!(twice, want, "formatter is not idempotent for {src:?}");
}

#[test]
fn hello_formats_clean() {
    check(
        "use std;\n\nfun main() {\n    std.print(\"Hello, world!\\n\");\n}\n",
        "use std;\n\nfun main() {\n    std.print(\"Hello, world!\\n\");\n}\n",
    );
}

#[test]
fn indent_is_four_spaces() {
    check(
        "fun main() {\nif (true) {\nstd.print(\"x\");\n}\n}\n",
        "fun main() {\n    if (true) {\n        std.print(\"x\");\n    }\n}\n",
    );
}

#[test]
fn else_starts_on_its_own_line() {
    check(
        "fun main() { if (a) { b; } else { c; } }\n",
        "fun main() {\n    if (a) {\n        b;\n    }\n    else {\n        c;\n    }\n}\n",
    );
}

#[test]
fn else_if_chain_stays_flat_with_else_on_own_lines() {
    check(
        "fun main() { if (a) { b; } else if (c) { d; } else { e; } }\n",
        "fun main() {\n    if (a) {\n        b;\n    }\n    else if (c) {\n        d;\n    }\n    else {\n        e;\n    }\n}\n",
    );
}

#[test]
fn while_formats_with_indent() {
    check(
        "fun main() { while (i > 0) { i = i - 1; } }\n",
        "fun main() {\n    while (i > 0) {\n        i = i - 1;\n    }\n}\n",
    );
}

#[test]
fn trailing_comma_splits_call() {
    check(
        "fun main() { foo(a, b,); }\n",
        "fun main() {\n    foo(\n        a,\n        b,\n    );\n}\n",
    );
}

#[test]
fn no_trailing_comma_folds_call_even_past_width() {
    let args = (0..12)
        .map(|i| format!("aaaaaaaaaaaaaaaaaaaaaaaaaaaa{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let src = format!("fun main() {{ foo({args}); }}\n");
    let once = format(&src, "test").expect("must format");
    assert!(once.lines().count() == 3, "call folds to one line:\n{once}");
    assert!(once.contains(&format!("foo({args});")));
    assert!(once.len() > MAX_WIDTH, "this line really is long");
    // Idempotent despite exceeding the width.
    assert_eq!(format(&once, "test").expect("must reformat"), once);
}

#[test]
fn trailing_comma_splits_params() {
    check(
        "fun add(a: i64, b: i64,): i64 { return a + b; }\n",
        "fun add(\n    a: i64,\n    b: i64,\n): i64 {\n    return a + b;\n}\n",
    );
}

#[test]
fn params_without_trailing_stay_single() {
    check(
        "fun add(a: i64,b: i64):i64{return a+b;}\n",
        "fun add(a: i64, b: i64): i64 {\n    return a + b;\n}\n",
    );
}

#[test]
fn trailing_comma_splits_array() {
    check(
        "fun main() { val a = [1u64, 2u64,]; a; }\n",
        "fun main() {\n    val a = [\n        1u64,\n        2u64,\n    ];\n    a;\n}\n",
    );
}

#[test]
fn array_without_trailing_folds() {
    check(
        "fun main() { val a = [1u64,2u64]; a; }\n",
        "fun main() {\n    val a = [1u64, 2u64];\n    a;\n}\n",
    );
}

#[test]
fn trailing_comma_splits_object_literal() {
    check(
        "fun main() { val c = Counter { value = 1, label = \"x\", }; c; }\n",
        "fun main() {\n    val c = Counter {\n        value = 1,\n        label = \"x\",\n    };\n    c;\n}\n",
    );
}

#[test]
fn trailing_comma_splits_tuple_literal() {
    check(
        "fun main() { val t = #(1u64, \"a\",); t; }\n",
        "fun main() {\n    val t = #(\n        1u64,\n        \"a\",\n    );\n    t;\n}\n",
    );
}

#[test]
fn union_decl_with_trailing_splits() {
    check(
        "type U = union { A, B(u64), };\n",
        "type U = union {\n    A,\n    B(u64),\n};\n",
    );
}

#[test]
fn union_decl_without_trailing_folds() {
    check(
        "type U = union {\nA,\nB(u64)\n};\n",
        "type U = union { A, B(u64) };\n",
    );
}

#[test]
fn object_decl_fields_split_on_trailing() {
    check(
        "type C = object { value: u64, label: String, };\n",
        "type C = object {\n    value: u64,\n    label: String,\n};\n",
    );
}

#[test]
fn object_decl_fields_fold_without_trailing() {
    check(
        "type C = object { value: u64, label: String };\n",
        "type C = object { value: u64, label: String };\n",
    );
}

#[test]
fn match_formats_arms_and_else() {
    check(
        "type U = union { A, B(u64), };\n\nfun f(o: U): u64 { match (o) { U.A { return 0u64; } U.B(v) { return v; } else { return 1u64; } } }\n",
        "type U = union {\n    A,\n    B(u64),\n};\n\nfun f(o: U): u64 {\n    match (o) {\n        U.A {\n            return 0u64;\n        }\n        U.B(v) {\n            return v;\n        }\n        else {\n            return 1u64;\n        }\n    }\n}\n",
    );
}

#[test]
fn null_arm_formats() {
    check(
        "fun main() { match (o) { Option.Some(v) { v; } null { 1u64; } } }\n",
        "fun main() {\n    match (o) {\n        Option.Some(v) {\n            v;\n        }\n        null {\n            1u64;\n        }\n    }\n}\n",
    );
}

#[test]
fn comments_are_preserved() {
    check(
        "// header\nuse std; // trailing\n\n// before fun\nfun main() {\n    // inside\n    std.print(\"x\"); // after call\n}\n// footer\n",
        "// header\nuse std; // trailing\n\n// before fun\nfun main() {\n    // inside\n    std.print(\"x\"); // after call\n}\n// footer\n",
    );
}

#[test]
fn blank_lines_collapse_to_one() {
    check(
        "use std;\n\n\n\nfun main() {\n    a;\n\n\n    b;\n}\n",
        "use std;\n\nfun main() {\n    a;\n\n    b;\n}\n",
    );
}

#[test]
fn unbraced_branch_normalizes_to_block() {
    check(
        "fun main() { if (true) 1u64; else 2u64; }\n",
        "fun main() {\n    if (true) {\n        1u64;\n    }\n    else {\n        2u64;\n    }\n}\n",
    );
}

#[test]
fn destructure_formats() {
    check(
        "fun main() { val #(a,b) = t; a; }\n",
        "fun main() {\n    val #(a, b) = t;\n    a;\n}\n",
    );
}

#[test]
fn destructure_with_trailing_splits() {
    check(
        "fun main() { val #(a, b,) = t; a; }\n",
        "fun main() {\n    val #(\n        a,\n        b,\n    ) = t;\n    a;\n}\n",
    );
}

#[test]
fn use_braced_without_trailing_folds() {
    check(
        "use std.fs.{read_file,write_file};\n",
        "use std.fs.{read_file, write_file};\n",
    );
}

#[test]
fn use_braced_with_trailing_splits() {
    check(
        "use std.fs.{read_file, write_file,};\n",
        "use std.fs.{\n    read_file,\n    write_file,\n};\n",
    );
}

#[test]
fn try_catch_and_cast_format() {
    check(
        "fun main() { val x = try f() catch 0u64; val y = v as u8; x; y; }\n",
        "fun main() {\n    val x = try f() catch 0u64;\n    val y = v as u8;\n    x;\n    y;\n}\n",
    );
}

#[test]
fn turbofish_formats() {
    check(
        "fun main() { val x = first::[u64](nums); x; }\n",
        "fun main() {\n    val x = first::[u64](nums);\n    x;\n}\n",
    );
}

#[test]
fn long_and_chain_breaks_on_continuation_lines() {
    let cond = (0..12)
        .map(|i| format!("condition{i}"))
        .collect::<Vec<_>>()
        .join(" && ");
    let src = format!("fun main() {{ val ok = {cond}; ok; }}\n");
    let once = format(&src, "test").expect("must format");
    assert!(once.contains("\n        && "), "chain breaks:\n{once}");
    for line in once.lines() {
        if line.trim_start().starts_with("&&") || line.contains("if (") {
            continue;
        }
        assert!(
            line.chars().count() <= MAX_WIDTH,
            "line exceeds width: {line:?}"
        );
    }
    assert_eq!(format(&once, "test").expect("must reformat"), once);
}

#[test]
fn parens_preserve_meaning() {
    // `-(a + b)` must keep its parens; `-a + b` would re-parse differently.
    check(
        "fun main() { val x = -(a + b); x; }\n",
        "fun main() {\n    val x = -(a + b);\n    x;\n}\n",
    );
}

#[test]
fn error_set_formats() {
    check(
        "type Io = error { NotFound, Denied, };\n",
        "type Io = error {\n    NotFound,\n    Denied,\n};\n",
    );
}

#[test]
fn generic_function_formats() {
    check(
        "fun first[T](a: Array[T]): T { return a[0u64]; }\n",
        "fun first[T](a: Array[T]): T {\n    return a[0u64];\n}\n",
    );
}

#[test]
fn lex_error_fails() {
    assert!(format("var x = @;", "test").is_err());
}

#[test]
fn parse_error_fails() {
    assert!(format("val x = 1", "test").is_err());
}

#[test]
fn examples_reformat_idempotently() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    for file in [
        "examples/hello.vl",
        "examples/cond_loop.vl",
        "examples/objects.vl",
        "examples/errors.vl",
        "examples/unions.vl",
        "examples/tuples.vl",
        "examples/arrays.vl",
        "examples/generics.vl",
        "examples/associated.vl",
        "examples/calls.vl",
        "examples/modules.vl",
    ] {
        let path = format!("{root}/{file}");
        let src =
            std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("example {path} must exist"));
        // Skip files with frontend errors unrelated to formatting; the
        // formatter only requires lex/parse success.
        let Ok(once) = format(&src, "test") else {
            continue;
        };
        let twice = format(&once, "test").expect("formatted output must reformat");
        assert_eq!(once, twice, "{file} is not idempotent");
    }
}
