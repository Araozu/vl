//! Executable regressions for whole-workspace review findings.
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use vl_codegen::Target;

fn compile(source: &str) -> Vec<u8> {
    let stdlib = vl_stdlib::load();
    let mut catalog = vl_codegen::modules();
    stdlib.extend_catalog(&mut catalog);
    let extra: Vec<_> = stdlib
        .checked_modules()
        .iter()
        .map(|(hir, typed)| (hir, typed))
        .collect();
    let checked = vl_frontend::check_text(source, "review", &catalog, &extra)
        .unwrap_or_else(|diags| panic!("frontend diagnostics: {diags:?}"));
    let mut lir = vl_lir::lower_project(&checked.hir, &checked.typed, &checked.plan);
    stdlib.link_with_plan(&mut lir, &checked.plan);
    lir.entrypoint = true;
    lir.entrypoint_module = Some("review".into());
    let (artifact, diags) = vl_codegen::NaraVmTarget.emit(&lir);
    assert!(!diags.iter().any(|d| d.is_error()), "{diags:?}");
    artifact
        .expect("successful emission")
        .bytes
        .expect("VM bytes")
}

fn run(source: &str, expected: &str) {
    let bytes = compile(source);
    let vm = std::env::var_os("NARAVM_BIN")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join("projects/zig/naravm/zig-out/bin/naravm"))
                .filter(|path| path.is_file())
        });
    let Some(vm) = vm else {
        // Compilation is always exercised; execution needs the external VM.
        return;
    };
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "vl-review-regression-{}-{}.nara",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&path, bytes).expect("write temporary VM artifact");
    let output = std::process::Command::new(vm).arg(&path).output();
    std::fs::remove_file(&path).expect("remove temporary VM artifact");
    let output = output.expect("execute Naravm");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
}

#[test]
fn scalar_error_payload_has_space_for_tag_code_and_payload() {
    run("use std; type E = error { A(u64), }; fun f(): E!u64 { return E.A(7u64); } fun main() { val x = f() catch 0u64; std.print_u64(x); }", "0");
}

fn live_locals_source(operation: &str) -> (String, String) {
    let mut source = String::from(
        "use std; use std.string; fun pick(a: u64, b: u64, c: u64): u64 { return c; } fun main() {",
    );
    for n in 0..20 {
        source.push_str(&format!("val x{n} = {n}u64;"));
    }
    source.push_str(operation);
    let mut suffix = String::new();
    for n in 0..20 {
        source.push_str(&format!("std.print_u64(x{n}); std.print(\"|\");"));
        suffix.push_str(&format!("{n}|"));
    }
    source.push('}');
    (source, suffix)
}

#[test]
fn overlapping_argument_moves_preserve_later_actuals() {
    let (source, suffix) =
        live_locals_source("std.print_u64(pick(x0, x0, x15)); std.print(\"|\");");
    run(&source, &format!("15|{suffix}"));
}

#[test]
fn checked_native_preserves_live_argument_registers() {
    let (source, suffix) =
        live_locals_source("val s = string.slice(\"abc\", 0u64, 1u64) catch \"\"; std.print(s);");
    run(&source, &format!("a{suffix}"));
}

#[test]
fn signed_comparison_temporaries_are_reused() {
    let source = format!(
        "use std; fun main() {{ val a: i64 = -1; val b: i64 = 1; {} }}",
        "if (a < b) { std.print(\"x\"); }".repeat(20)
    );
    run(&source, &"x".repeat(20));
}

#[test]
fn unused_arithmetic_results_do_not_exhaust_registers() {
    let source = format!(
        "use std; fun main() {{ {} std.print(\"done\"); }}",
        "1u64 + 2u64;".repeat(40)
    );
    run(&source, "done");
}

#[test]
fn floating_equality_obeys_zero_and_nan_semantics() {
    run("use std; fun main() { val z = 0.0f64; val negative = -1.0f64 * z; if (z == negative) { std.print(\"equal\"); } else { std.print(\"wrong\"); } val n = z / z; if (n != n) { std.print(\"nan\"); } else { std.print(\"wrong\"); } val inf = 1.0f64 / z; val neginf = -1.0f64 / z; if (inf == inf && neginf == neginf && inf != neginf) { std.print(\"inf\"); } else { std.print(\"wrong\"); } val negnan = -1.0f64 * n; if (negnan != negnan) { std.print(\"negative-nan\"); } else { std.print(\"wrong\"); } }", "equalnaninfnegative-nan");
}

#[test]
fn object_tuple_fields_store_values() {
    run("use std; type Box = object { t: *#(u64, u64), }; fun main() { var t: *#(u64, u64) = #(1u64, 2u64); var b = Box { t = t }; t.`0 = 9u64; std.print_u64(b.t.`0); b.t = t; t.`0 = 8u64; std.print_u64(b.t.`0); }", "19");
}

#[test]
fn top_level_destructuring_keeps_initializers_and_reads() {
    run("use std; val #(a, b) = #(11u64, 22u64); fun main() { std.print_u64(a); std.print_u64(b); }", "1122");
}

#[test]
fn nullable_and_fallible_success_conversions_compose() {
    run("use std; type E = error { Bad, }; fun make(): E!?u64 { return 7u64; } fun main() { val x = make() catch null; if (x == null) { std.print(\"null\"); } else { std.print(\"some\"); } }", "some");
}

fn check_source(source: &str) -> Result<vl_frontend::FrontendOk, Vec<vl_common::Diagnostic>> {
    let stdlib = vl_stdlib::load();
    let mut catalog = vl_codegen::modules();
    stdlib.extend_catalog(&mut catalog);
    let extra: Vec<_> = stdlib
        .checked_modules()
        .iter()
        .map(|(hir, typed)| (hir, typed))
        .collect();
    vl_frontend::check_text(source, "review", &catalog, &extra)
}

#[test]
fn generic_readonly_projections_cannot_escape_through_aliases_or_calls() {
    let heads = [
        "fun first[T](a: Array[T]): T { return a[0u64]; }",
        "fun first[T](a: Array[T]): T { val x = a[0u64]; return x; }",
        "fun first[T](a: Array[T]): T { val #(x, y) = #(a[0u64], 0u64); return x; }",
        "fun first[T](a: Array[T]): T { val pair = #(a[0u64], 0u64); return pair.`0; }",
        "fun id[T](x: T): T { return x; } fun first[T](a: Array[T]): T { return id(a[0u64]); }",
    ];
    for head in heads {
        for forward in [false, true] {
            let main = "fun main() { val a: Array[*Foo] = [Foo { value = 1u64 }]; var leaked = first(a); leaked.value = 2u64; }";
            let source = if forward {
                format!("type Foo = object {{ value: u64, }}; {main} {head}")
            } else {
                format!("type Foo = object {{ value: u64, }}; {head} {main}")
            };
            let diags = match check_source(&source) {
                Ok(_) => panic!("mutable capability escaped: {source}"),
                Err(diags) => diags,
            };
            assert!(diags.iter().any(|d| d.is_error()), "{diags:?}");
            assert!(
                !diags.iter().any(|d| d.code.as_deref() == Some("E500")),
                "{diags:?}"
            );
        }
    }
}

#[test]
fn generic_projections_keep_valid_scalar_and_mutable_receiver_contracts() {
    for source in [
        "fun first[T](a: Array[T]): T { return a[0u64]; } fun main() { val x = first([1u64]); x; }",
        "type Foo = object { value: u64, }; fun first[T](a: *Array[T]): T { return a[0u64]; } fun main() { var a: *Array[*Foo] = [Foo { value = 1u64 }]; var x = first(a); x.value = 2u64; }",
        "type Foo = object { value: u64, }; fun keep[T](x: T, a: Array[T]): T { a[0u64]; return x; } fun main() { var f = Foo { value = 1u64 }; val a: Array[*Foo] = [f]; var x = keep(f, a); x.value = 2u64; }",
        "type Foo = object { value: u64, }; fun keep[T](x: T, a: Array[T]): T { var pair = #(a[0u64], x); val #(projected, original) = pair; return original; } fun main() { var f = Foo { value = 1u64 }; val a: Array[*Foo] = [f]; var x = keep(f, a); x.value = 2u64; }",
        "type Foo = object { value: u64, }; fun keep[T](x: T, a: Array[T]): T { var pair = #(a[0u64], x); return pair.`1; } fun main() { var f = Foo { value = 1u64 }; val a: Array[*Foo] = [f]; var x = keep(f, a); x.value = 2u64; }",
    ] {
        check_source(source).unwrap_or_else(|diags| panic!("valid generic contract rejected: {diags:?}\n{source}"));
    }
}

#[test]
fn inferred_tuple_destructuring_and_contextual_wrappers_normalize() {
    check_source("fun nullable(): ?Array[u64] { return []; } fun fallible(): !Array[u64] { return Array.new(1u64); } fun main() { val #(a, b) = #(1, 2); a; b; }")
        .unwrap_or_else(|diags| panic!("{diags:?}"));
}

#[test]
fn inferred_nominal_types_and_error_payloads_are_validated() {
    for source in [
        "type Num[T extends Numeric] = union { Some(T), }; fun main() { Num.Some(\"s\"); }",
        "type Num[T extends Numeric] = object { value: T, }; fun main() { Num { value = \"s\" }; }",
        "type Box[T] = union { Some(T), }; type Foo = object { value: u64, }; fun main() { var f = Foo { value = 1u64 }; Box.Some(f); }",
        "type E = error { Bad(no.such.Type), }; fun main() {}",
    ] {
        let diags = match check_source(source) { Ok(_) => panic!("invalid type accepted: {source}"), Err(diags) => diags };
        assert!(diags.iter().any(|d| d.is_error()), "{diags:?}");
        assert!(!diags.iter().any(|d| d.code.as_deref() == Some("E500")), "{diags:?}");
    }
}

#[test]
fn nested_explicit_generic_arguments_emit_concrete_specializations() {
    compile("fun id[U](x: U): U { return x; } fun wrap[T](x: T): #(T, T) { return id::[#(T, T)](#(x, x)); } fun main() { val p = wrap(1u64); p; }");
}

#[test]
fn nested_tuple_specializations_have_distinct_runtime_bodies() {
    run("use std; fun id[T](x: T): T { return x; } fun main() { val a = id(#(1u64, #(2u64, 3u64, 4u64))); val b = id(#(5u64, #(6u64, 7u64), 8u64)); std.print_u64(a.`1.`2); std.print_u64(b.`2); }", "48");
}

#[test]
fn unary_negation_preserves_float_and_generic_numeric_types_and_signed_zero() {
    run("use std; fun neg[T extends Numeric](x: T): T { return -x; } fun main() { val x = 1.0f64; if (-x == -1.0f64 && neg(x) == -1.0f64) { std.print(\"typed\"); } val z = -0.0f64; val reciprocal = 1.0f64 / z; val positivezero = 0.0f64; val neginf = -1.0f64 / positivezero; if (reciprocal == neginf) { std.print(\"negative-zero\"); } }", "typednegative-zero");
}

#[test]
fn signed_minimum_literal_spelling_is_accepted_without_positive_overflow() {
    for spelling in [
        "-9223372036854775808i64",
        "-9223372036854775808",
        "-0009223372036854775808i64",
    ] {
        let source = format!("use std; use std.fmt; fun main() {{ val x: i64 = {spelling}; std.print(fmt.i64_to_string(x)); }}");
        run(&source, "-9223372036854775808");
    }
    for spelling in ["9223372036854775808i64", "9223372036854775808"] {
        let source = format!("fun main() {{ val x: i64 = {spelling}; }}");
        assert!(check_source(&source).is_err(), "positive overflow accepted");
    }
}

#[test]
fn fallible_generic_calls_cannot_restore_projected_mutability() {
    let source = "type Foo = object { value: u64, }; fun wrap[T](x: T): !T { return x; } fun first[T](a: Array[T], fallback: T): T { return wrap(a[0u64]) catch fallback; } fun main() { val a: Array[*Foo] = [Foo { value = 1u64 }]; var fallback = Foo { value = 2u64 }; var leaked = first(a, fallback); leaked.value = 3u64; }";
    let diags = match check_source(source) {
        Ok(_) => panic!("fallible call laundered mutable capability"),
        Err(diags) => diags,
    };
    assert!(diags.iter().any(|d| d.is_error()), "{diags:?}");
    assert!(
        !diags.iter().any(|d| d.code.as_deref() == Some("E500")),
        "{diags:?}"
    );
}

#[test]
fn instantiated_nominal_arguments_keep_capability_restrictions() {
    let source = "type Foo = object { value: u64, }; type Box[T] = object { v: T, }; fun make[T](x: T): Box[T] { return Box { v = x }; } fun main() { var f = Foo { value = 1u64 }; val b = make(f); b; }";
    let diags = match check_source(source) {
        Ok(_) => panic!("nominal mutable type argument escaped through specialization"),
        Err(diags) => diags,
    };
    assert!(diags.iter().any(|d| d.is_error()), "{diags:?}");
    assert!(
        !diags.iter().any(|d| d.code.as_deref() == Some("E500")),
        "{diags:?}"
    );
}

#[test]
fn assigning_a_generic_readonly_projection_keeps_its_capability() {
    let source = "type Foo = object { value: u64, }; fun first[T](a: Array[T], fallback: T): T { var x = fallback; x = a[0u64]; return x; } fun main() { var fallback = Foo { value = 1u64 }; val a: Array[*Foo] = [fallback]; var leaked = first(a, fallback); leaked.value = 2u64; }";
    let diags = match check_source(source) {
        Ok(_) => panic!("mutable capability escaped through assignment"),
        Err(diags) => diags,
    };
    assert!(diags.iter().any(|d| d.is_error()), "{diags:?}");
    assert!(
        !diags.iter().any(|d| d.code.as_deref() == Some("E500")),
        "{diags:?}"
    );
}

#[test]
fn generic_readonly_projections_cannot_escape_through_fresh_arrays_or_tuple_stores() {
    for head in [
        "fun first[T](a: Array[T], fallback: T): T { var xs = [a[0u64]]; return xs[0u64]; }",
        "fun first[T](a: Array[T], fallback: T): T { var xs: *Array[T] = [a[0u64]]; return xs[0u64]; }",
        "fun first[T](a: Array[T], fallback: T): T { var pair = #(fallback, fallback); pair.`0 = a[0u64]; return pair.`0; }",
        "fun first[T](a: Array[T], fallback: T): T { var xs: *Array[T] = [fallback]; xs[0u64] = a[0u64]; return xs[0u64]; }",
    ] {
        let source = format!("type Foo = object {{ value: u64, }}; {head} fun main() {{ var fallback = Foo {{ value = 1u64 }}; val a: Array[*Foo] = [fallback]; var leaked = first(a, fallback); leaked.value = 2u64; }}");
        let diags = match check_source(&source) { Ok(_) => panic!("mutable capability escaped: {source}"), Err(diags) => diags };
        assert!(diags.iter().any(|d| d.is_error()), "{diags:?}");
        assert!(!diags.iter().any(|d| d.code.as_deref() == Some("E500")), "{diags:?}");
    }
}
