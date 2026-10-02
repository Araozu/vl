use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_project(name: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must be after the Unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("vl-{name}-{}-{nonce}", std::process::id()));
    fs::create_dir_all(&path).expect("create temporary project");
    path
}

fn run(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vl"))
        .current_dir(root)
        .args(args)
        .output()
        .expect("run vl")
}

#[test]
fn init_creates_a_project_config_and_source_folder() {
    let root = temp_project("init");
    let output = run(&root, &["init", "demo"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(root.join("vl.toml")).expect("read vl.toml"),
        "module = \"demo\"\nsource = \"src\"\nout = \"out\"\n"
    );
    assert!(root.join("src").is_dir());
    assert!(!run(&root, &["init", "again"]).status.success());
    fs::remove_dir_all(root).expect("remove temporary project");
}

#[test]
fn init_without_a_module_uses_the_current_directory_name() {
    let root = temp_project("derived-module");
    let output = run(&root, &["init"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let config = fs::read_to_string(root.join("vl.toml")).expect("read vl.toml");
    let module = config
        .lines()
        .find_map(|line| line.strip_prefix("module = \"")?.strip_suffix('"'))
        .expect("module in vl.toml");
    assert!(!module.is_empty());
    assert!(module
        .chars()
        .all(|ch| ch == '_' || ch.is_ascii_alphanumeric()));
    fs::remove_dir_all(root).expect("remove temporary project");
}

#[test]
fn project_build_uses_default_source_and_flat_custom_output() {
    let root = temp_project("build");
    fs::create_dir_all(root.join("src/foo")).expect("create source tree");
    fs::write(
        root.join("vl.toml"),
        "module = \"project\"\nout = \"artifacts\"\n",
    )
    .expect("write project config");
    fs::write(root.join("src/main.vl"), "fun main() { }").expect("write main");
    fs::write(root.join("src/foo/bar.vl"), "fun helper() { }").expect("write nested module");

    let output = run(&root, &["build"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let main_artifact = root.join("artifacts/project__main.naravm");
    let nested_artifact = root.join("artifacts/project__foo__bar.naravm");
    assert!(main_artifact.is_file());
    assert!(nested_artifact.is_file());
    assert!(!root.join("out").exists());

    let bytes = fs::read(nested_artifact).expect("read nested artifact");
    assert!(bytes
        .windows(b"project.foo.bar".len())
        .any(|w| w == b"project.foo.bar"));
    fs::remove_dir_all(root).expect("remove temporary project");
}

#[test]
fn single_file_build_inside_project_builds_the_whole_project() {
    let root = temp_project("single-file-project-build");
    fs::create_dir_all(root.join("src/lib")).expect("create source tree");
    fs::write(root.join("vl.toml"), "module = \"demo\"\n").expect("write config");
    fs::write(
        root.join("src/main.vl"),
        "use demo.lib; fun main() { lib.run(); }",
    )
    .expect("write main");
    fs::write(root.join("src/lib.vl"), "fun run() { }").expect("write library");

    let output = run(&root, &["build", "src/main.vl", "--emit", "lir"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(root.join("out/demo__main.lir").is_file());
    assert!(root.join("out/demo__lib.lir").is_file());
    fs::remove_dir_all(root).expect("remove temporary project");
}

#[test]
fn project_scripts_run_default_and_named_commands() {
    let root = temp_project("scripts");
    fs::write(
        root.join("vl.toml"),
        "module = \"scripts\"\n[scripts]\nrun = \"echo default-script > script-marker.txt\"\ncheck = \"echo named-script\"\nfail = \"exit 37\"\n",
    )
    .expect("write project config");

    let default_output = run(&root, &["run"]);
    assert!(
        default_output.status.success(),
        "{}",
        String::from_utf8_lossy(&default_output.stderr)
    );
    assert_eq!(
        fs::read_to_string(root.join("script-marker.txt"))
            .expect("script writes relative to the project root")
            .trim(),
        "default-script"
    );

    let named_output = run(&root, &["run", "check"]);
    assert!(
        named_output.status.success(),
        "{}",
        String::from_utf8_lossy(&named_output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&named_output.stdout).trim(),
        "named-script"
    );

    let failed_output = run(&root, &["run", "fail"]);
    assert_eq!(failed_output.status.code(), Some(37));

    fs::remove_dir_all(root).expect("remove temporary project");
}

#[test]
fn project_run_reports_missing_script() {
    let root = temp_project("missing-script");
    fs::write(root.join("vl.toml"), "module = \"missing\"\n").expect("write project config");

    let output = run(&root, &["run"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("project script `run` is not defined"));

    fs::remove_dir_all(root).expect("remove temporary project");
}

#[test]
fn project_imports_are_resolved_before_file_order_and_emitted_qualified() {
    let root = temp_project("imports");
    fs::create_dir_all(root.join("src/lib")).expect("create source tree");
    fs::write(root.join("vl.toml"), "module = \"demo\"\n").expect("write config");
    fs::write(
        root.join("src/main.vl"),
        "use demo.lib.math.add; fun main() { val value = add(1u64, 2u64); }",
    )
    .expect("write importer");
    fs::write(
        root.join("src/lib/math.vl"),
        "fun add(a: u64, b: u64): u64 { return a + b; }",
    )
    .expect("write provider");

    let output = run(&root, &["build", "--emit", "lir"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let main_lir = fs::read_to_string(root.join("out/demo__main.lir")).expect("main lir");
    assert!(main_lir.contains("call demo.lib.math::add"), "{main_lir}");
    assert!(root.join("out/demo__lib__math.lir").is_file());
    let output = run(&root, &["build"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bytes = fs::read(root.join("out/demo__main.naravm")).expect("main artifact");
    assert!(bytes
        .windows(b"demo.lib.math".len())
        .any(|w| w == b"demo.lib.math"));
    assert!(bytes.windows(b"add".len()).any(|w| w == b"add"));
    fs::remove_dir_all(root).expect("remove temporary project");
}

#[test]
fn project_unions_construct_and_match_across_modules() {
    let root = temp_project("unions-xmod");
    fs::create_dir_all(root.join("src/lib")).expect("create source tree");
    fs::write(root.join("vl.toml"), "module = \"demo\"\n").expect("write config");
    fs::write(
        root.join("src/lib/shapes.vl"),
        "type Shape = union { Circle(f64), Rect(f64, f64), Dot, }; fun sides(s: Shape): u64 { match (s) { Shape.Circle(r) { r; return 1u64; } Shape.Rect(w, h) { w; h; return 4u64; } Shape.Dot { return 0u64; } } }",
    )
    .expect("write provider");
    fs::write(
        root.join("src/main.vl"),
        "use demo.lib.shapes; fun main() { val a = shapes.Shape.Circle(1.0f64); val b = demo.lib.shapes.Shape.Dot; val n = shapes.sides(a); val m = shapes.sides(b); n; m; match (a) { shapes.Shape.Circle(r) { r; } else { 0.0f64; } } }",
    )
    .expect("write importer");

    let output = run(&root, &["build", "--emit", "lir"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let main_lir = fs::read_to_string(root.join("out/demo__main.lir")).expect("main lir");
    assert!(
        main_lir.contains("new_variant demo.lib.shapes.Shape.Circle"),
        "{main_lir}"
    );
    assert!(main_lir.contains("tag_of"), "{main_lir}");
    assert!(main_lir.contains("payload_get"), "{main_lir}");
    let output = run(&root, &["build"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bytes = fs::read(root.join("out/demo__main.naravm")).expect("main artifact");
    assert_eq!(&bytes[..4], b"nara");
    fs::remove_dir_all(root).expect("remove temporary project");
}

#[test]
fn project_rejects_a_second_main_in_any_module() {
    let root = temp_project("library-main");
    fs::create_dir_all(root.join("src/lib")).expect("create source tree");
    fs::write(root.join("vl.toml"), "module = \"demo\"\n").expect("write config");
    fs::write(
        root.join("src/main.vl"),
        "use demo.lib; fun main() { lib.main(); }",
    )
    .expect("write project main");
    fs::write(root.join("src/lib.vl"), "fun main() { }").expect("write library main");

    let output = run(&root, &["build"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("[E401]"));
    fs::remove_dir_all(root).expect("remove temporary project");
}

#[test]
fn malformed_provider_does_not_cascade_into_consumer_e500() {
    let root = temp_project("poisoned-provider");
    fs::create_dir_all(root.join("src")).expect("create source tree");
    fs::write(root.join("vl.toml"), "module = \"demo\"\n").expect("write config");
    fs::write(root.join("src/provider.vl"), "fun broken(value) { }").expect("write provider");
    fs::write(
        root.join("src/main.vl"),
        "use demo.provider; fun main() { provider.broken(1u64); }",
    )
    .expect("write consumer");

    let output = run(&root, &["build"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(!stderr.contains("[E500]"), "{stderr}");
    fs::remove_dir_all(root).expect("remove temporary project");
}

#[test]
fn malformed_provider_does_not_report_missing_recovered_export() {
    let root = temp_project("parse-poisoned-export");
    fs::create_dir_all(root.join("src")).expect("create source tree");
    fs::write(root.join("vl.toml"), "module = \"demo\"\n").expect("write config");
    fs::write(root.join("src/provider.vl"), "fun broken(value: u64) { ").expect("write provider");
    fs::write(
        root.join("src/main.vl"),
        "use demo.provider.broken; fun main() { broken(1u64); }",
    )
    .expect("write consumer");

    let output = run(&root, &["check", "src/provider.vl"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(!stderr.contains("[E203]"), "{stderr}");
    fs::remove_dir_all(root).expect("remove temporary project");
}

#[test]
fn project_errors_do_not_leave_final_artifacts() {
    let root = temp_project("transaction");
    fs::create_dir_all(root.join("src")).expect("create source tree");
    fs::write(root.join("vl.toml"), "module = \"demo\"\n").expect("write config");
    fs::write(
        root.join("src/main.vl"),
        "use demo.missing; fun main() { missing.run(); }",
    )
    .expect("write broken source");
    let output = run(&root, &["build"]);
    assert!(!output.status.success());
    assert!(!root.join("out/demo__main.naravm").exists());
    fs::remove_dir_all(root).expect("remove temporary project");
}

#[test]
fn project_rejects_output_overlapping_source_before_publication() {
    let root = temp_project("source-output-overlap");
    fs::create_dir_all(root.join("src/out")).expect("create source tree");
    fs::write(
        root.join("vl.toml"),
        "module = \"demo\"\nout = \"src/out\"\n",
    )
    .expect("write config");
    fs::write(root.join("src/main.vl"), "fun main() { }").expect("write main");
    fs::write(root.join("src/out/keep.vl"), "source must survive").expect("write sentinel");

    let output = run(&root, &["build"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("E602"));
    assert_eq!(
        fs::read_to_string(root.join("src/out/keep.vl")).expect("read sentinel"),
        "source must survive"
    );
    fs::remove_dir_all(root).expect("remove temporary project");
}

#[test]
fn shallow_project_emit_publishes_malformed_sources_and_removes_stale_files() {
    let root = temp_project("shallow-malformed");
    fs::create_dir_all(root.join("src")).expect("create source tree");
    fs::create_dir_all(root.join("out")).expect("create output tree");
    fs::write(root.join("vl.toml"), "module = \"demo\"\n").expect("write config");
    fs::write(root.join("src/main.vl"), "fun main( {").expect("write malformed source");
    fs::write(root.join("out/stale.tokens"), "stale").expect("write stale output");

    let output = run(&root, &["build", "--emit", "tokens"]);
    assert!(!output.status.success());
    assert!(root.join("out/demo__main.tokens").is_file());
    assert!(!root.join("out/stale.tokens").exists());
    fs::remove_dir_all(root).expect("remove temporary project");
}

#[test]
fn project_collision_uses_broad_catalog_for_naravm() {
    let root = temp_project("broad-collision");
    fs::create_dir_all(root.join("src")).expect("create source tree");
    fs::write(root.join("vl.toml"), "module = \"std\"\n").expect("write config");
    fs::write(root.join("src/nested.vl"), "fun read() { }").expect("write source");

    let output = run(&root, &["build", "--target", "naravm"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("collides with a target module"));
    let output = run(&root, &["check", "src/nested.vl"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("compiler module catalog"));
    fs::remove_dir_all(root).expect("remove temporary project");
}

#[test]
fn catalog_collision_does_not_run_frontend_resolution() {
    let root = temp_project("collision-no-frontend");
    fs::create_dir_all(root.join("src")).expect("create source tree");
    fs::write(root.join("vl.toml"), "module = \"std\"\n").expect("write config");
    fs::write(
        root.join("src/main.vl"),
        "use std.not_there; fun main() { not_there.run(); }",
    )
    .expect("write source");

    let output = run(&root, &["build", "--emit", "lir"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("compiler module catalog"), "{stderr}");
    assert!(!stderr.contains("[E202]"), "{stderr}");

    let output = run(&root, &["check", "src/main.vl"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("compiler module catalog"), "{stderr}");
    assert!(!stderr.contains("[E202]"), "{stderr}");
    fs::remove_dir_all(root).expect("remove temporary project");
}

#[test]
fn project_check_uses_source_catalog() {
    let root = temp_project("check-imports");
    fs::create_dir_all(root.join("src")).expect("create source tree");
    fs::write(root.join("vl.toml"), "module = \"demo\"\n").expect("write config");
    fs::write(
        root.join("src/main.vl"),
        "use demo.lib; fun main() { lib.run(); }",
    )
    .expect("write main");
    fs::write(root.join("src/lib.vl"), "fun run() { }").expect("write library");
    let output = run(&root, &["check", "src/main.vl"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(root).expect("remove temporary project");
}

#[test]
fn imported_generic_checks_and_builds() {
    let root = temp_project("generic-import");
    fs::create_dir_all(root.join("src")).expect("create source tree");
    fs::write(root.join("vl.toml"), "module = \"demo\"\n").expect("write config");
    fs::write(
        root.join("src/main.vl"),
        "use demo.lib.id; fun main() { val x = id(1u64); }",
    )
    .expect("write main");
    fs::write(
        root.join("src/lib.vl"),
        "fun id[T](value: T): T { return value; }",
    )
    .expect("write library");
    let output = run(&root, &["check", "src/main.vl"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = run(&root, &["build", "--emit", "lir"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let main_lir = fs::read_to_string(root.join("out/demo__main.lir")).expect("main lir");
    assert!(main_lir.contains("call demo.lib::id$u64"), "{main_lir}");
    let lib_lir = fs::read_to_string(root.join("out/demo__lib.lir")).expect("lib lir");
    assert!(lib_lir.contains("fn id$u64:"), "{lib_lir}");
    assert!(!lib_lir.contains("fn id:\n"), "{lib_lir}");
    let output = run(&root, &["build"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(root.join("out/demo__main.naravm").is_file());
    assert!(root.join("out/demo__lib.naravm").is_file());
    fs::remove_dir_all(root).expect("remove temporary project");
}

#[test]
fn imported_generic_supports_inference_and_turbofish() {
    let root = temp_project("generic-infer-turbofish");
    fs::create_dir_all(root.join("src")).expect("create source tree");
    fs::write(root.join("vl.toml"), "module = \"demo\"\n").expect("write config");
    fs::write(
        root.join("src/main.vl"),
        "use demo.lib.id; fun main() { val a = id(1u64); val b = id::[String](\"hi\"); a; b; }",
    )
    .expect("write main");
    fs::write(
        root.join("src/lib.vl"),
        "fun id[T](value: T): T { return value; }",
    )
    .expect("write library");
    let output = run(&root, &["check", "src/main.vl"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = run(&root, &["build", "--emit", "lir"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lib_lir = fs::read_to_string(root.join("out/demo__lib.lir")).expect("lib lir");
    assert!(lib_lir.contains("fn id$u64:"), "{lib_lir}");
    assert!(lib_lir.contains("fn id$String:"), "{lib_lir}");
    fs::remove_dir_all(root).expect("remove temporary project");
}

#[test]
fn imported_generic_emits_once_for_many_callers() {
    let root = temp_project("generic-dedup");
    fs::create_dir_all(root.join("src")).expect("create source tree");
    fs::write(root.join("vl.toml"), "module = \"demo\"\n").expect("write config");
    fs::write(
        root.join("src/main.vl"),
        "use demo.lib.id; use demo.other; fun main() { val a = id(1u64); val b = other.get(2u64); a; b; }",
    )
    .expect("write main");
    fs::write(
        root.join("src/lib.vl"),
        "fun id[T](value: T): T { return value; }",
    )
    .expect("write library");
    fs::write(
        root.join("src/other.vl"),
        "use demo.lib; fun get[T](value: T): T { return lib.id(value); }",
    )
    .expect("write other");
    let output = run(&root, &["build", "--emit", "lir"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lib_lir = fs::read_to_string(root.join("out/demo__lib.lir")).expect("lib lir");
    assert_eq!(lib_lir.matches("fn id$u64:").count(), 1, "{lib_lir}");
    fs::remove_dir_all(root).expect("remove temporary project");
}

#[test]
fn imported_generic_forwarding_chain_converges() {
    let root = temp_project("generic-forward");
    fs::create_dir_all(root.join("src")).expect("create source tree");
    fs::write(root.join("vl.toml"), "module = \"demo\"\n").expect("write config");
    fs::write(
        root.join("src/lib.vl"),
        "fun id[T](value: T): T { return value; }",
    )
    .expect("write lib");
    fs::write(
        root.join("src/mid.vl"),
        "use demo.lib; fun wrap[T](x: T): T { return lib.id(x); }",
    )
    .expect("write mid");
    fs::write(
        root.join("src/main.vl"),
        "use demo.mid; fun main() { val x = mid.wrap(1u64); }",
    )
    .expect("write main");
    let output = run(&root, &["build", "--emit", "lir"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mid_lir = fs::read_to_string(root.join("out/demo__mid.lir")).expect("mid lir");
    assert!(mid_lir.contains("call demo.lib::id$u64"), "{mid_lir}");
    fs::remove_dir_all(root).expect("remove temporary project");
}

#[test]
fn imported_generic_budget_exceeded_is_one_e303() {
    let root = temp_project("generic-budget");
    fs::create_dir_all(root.join("src")).expect("create source tree");
    fs::write(root.join("vl.toml"), "module = \"demo\"\n").expect("write config");
    fs::write(root.join("src/lib.vl"), "fun grow[T](x: T) { grow([x]); }").expect("write lib");
    fs::write(
        root.join("src/main.vl"),
        "use demo.lib; fun main() { lib.grow(1u64); }",
    )
    .expect("write main");
    let output = run(&root, &["check", "src/main.vl"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("E303"), "{stderr}");
    assert!(
        stderr.contains("demolib") || stderr.contains("demo.lib") || stderr.contains("grow"),
        "{stderr}"
    );
    assert!(!root.join("out/demo__main.naravm").exists());
    fs::remove_dir_all(root).expect("remove temporary project");
}

#[test]
fn failed_project_leaves_previous_output_untouched() {
    let root = temp_project("transaction-keep");
    fs::create_dir_all(root.join("src")).expect("create source tree");
    fs::write(root.join("vl.toml"), "module = \"demo\"\n").expect("write config");
    fs::write(root.join("src/main.vl"), "fun main() { }").expect("write main");
    let output = run(&root, &["build"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let artifact = root.join("out/demo__main.naravm");
    assert!(artifact.is_file());
    let before = fs::read(&artifact).expect("read artifact");
    fs::write(root.join("src/main.vl"), "fun main( {").expect("break source");
    let output = run(&root, &["build"]);
    assert!(!output.status.success());
    let after = fs::read(&artifact).expect("previous output must survive");
    assert_eq!(before, after);
    fs::remove_dir_all(root).expect("remove temporary project");
}

#[test]
fn cyclic_generic_call_graph_converges() {
    let root = temp_project("generic-cycle");
    fs::create_dir_all(root.join("src")).expect("create source tree");
    fs::write(root.join("vl.toml"), "module = \"demo\"\n").expect("write config");
    fs::write(
        root.join("src/lib.vl"),
        "use demo.mid; fun ping[T](x: T): T { return mid.pong(x); }",
    )
    .expect("write lib");
    fs::write(
        root.join("src/mid.vl"),
        "use demo.lib; fun pong[T](x: T): T { return lib.ping(x); } fun wrap[T](x: T): T { return pong(x); }",
    )
    .expect("write mid");
    // Converging same-type cycle would recurse forever at runtime, so check
    // only (no execution): it must terminate the fixed point, not hit budget.
    // Use a converging variant that terminates via a monomorphic base.
    fs::write(
        root.join("src/main.vl"),
        "use demo.mid; fun main() { val x = mid.wrap(1u64); }",
    )
    .expect("write main");
    // This cycle is same-type recursive (`ping[u64]` <-> `pong[u64]`) and would
    // diverge at runtime, but the compiler must terminate (cache hit) rather
    // than hang or hit the expanding budget. We assert it builds; runtime
    // divergence is out of scope for this test.
    // To keep the test terminating at runtime, we do not execute it.
    let output = run(&root, &["build", "--emit", "lir"]);
    // Same-type mutual recursion converges via cache hits (no E303).
    // If the implementation hangs, this test times out; if it mis-budgets, it fails.
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(root).expect("remove temporary project");
}

#[test]
fn malformed_manifest_is_not_checked_as_a_standalone_file() {
    let root = temp_project("malformed-manifest");
    fs::create_dir_all(root.join("src")).expect("create source tree");
    fs::write(root.join("vl.toml"), "module = [").expect("write malformed config");
    fs::write(root.join("src/main.vl"), "fun main() { }").expect("write main");

    let output = run(&root, &["check", "src/main.vl"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot parse project configuration"));
    fs::remove_dir_all(root).expect("remove temporary project");
}

fn parse_stdout_json(output: &Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout).expect("stdout must be one JSON document")
}

#[test]
fn check_json_reports_machine_readable_diagnostics() {
    let root = temp_project("check-json");
    let file = root.join("bad.vl");
    fs::write(&file, "val x = y;\n").expect("write source");

    let output = run(&root, &["check", "bad.vl", "--format", "json"]);
    assert_eq!(output.status.code(), Some(1));
    let report = parse_stdout_json(&output);
    assert_eq!(report["ok"], false);
    let diags = report["diagnostics"].as_array().expect("diagnostics array");
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0]["severity"], "error");
    assert_eq!(diags[0]["code"], "E201");
    let span = &diags[0]["labels"][0]["span"];
    assert_eq!(span["line_start"], 1);
    assert_eq!(span["column_start"], 9);

    fs::write(&file, "fun main() { }\n").expect("write clean source");
    let output = run(&root, &["check", "bad.vl", "--format", "json"]);
    assert!(output.status.success());
    let report = parse_stdout_json(&output);
    assert_eq!(report["ok"], true);
    assert!(report["diagnostics"].as_array().expect("array").is_empty());
    // Human mode still prints the friendly line and stays exit 0.
    let output = run(&root, &["check", "bad.vl"]);
    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("checks clean"),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    fs::remove_dir_all(root).expect("remove temporary project");
}

#[test]
fn check_json_reads_stdin_without_project_lookup() {
    use std::io::Write;
    use std::process::Stdio;
    let root = temp_project("check-stdin");
    // A vl.toml exists, but `-` must not trigger project discovery.
    fs::write(root.join("vl.toml"), "module = \"demo\"\n").expect("write config");
    let mut child = Command::new(env!("CARGO_BIN_EXE_vl"))
        .current_dir(&root)
        .args(["check", "-", "--format", "json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn vl");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"val x = 1")
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait");
    assert_eq!(output.status.code(), Some(1));
    let report = parse_stdout_json(&output);
    assert_eq!(report["ok"], false);
    assert_eq!(report["diagnostics"][0]["file"], "<stdin>");
    assert_eq!(report["diagnostics"][0]["code"], "E100");
    fs::remove_dir_all(root).expect("remove temporary project");
}

#[test]
fn build_json_requires_out_and_reports_on_stdout() {
    let root = temp_project("build-json");
    fs::write(root.join("bad.vl"), "fun main() { }\n").expect("write source");

    // Without `--out` the artifact would share stdout with the report.
    let output = run(&root, &["build", "bad.vl", "--format", "json"]);
    assert_eq!(output.status.code(), Some(2));
    let report = parse_stdout_json(&output);
    assert_eq!(report["ok"], false);
    assert_eq!(report["diagnostics"][0]["code"], "E601");

    let output = run(
        &root,
        &["build", "bad.vl", "--format", "json", "--out", "bad.out"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = parse_stdout_json(&output);
    assert_eq!(report["ok"], true);
    assert!(root.join("bad.out").is_file());
    fs::remove_dir_all(root).expect("remove temporary project");
}

#[test]
fn single_file_json_reports_artifact_write_errors_in_every_emit_mode() {
    let root = temp_project("json-write-error");
    fs::write(root.join("valid.vl"), "fun main() {}").expect("write source");
    for emit in [None, Some("tokens"), Some("ast"), Some("lir"), Some("asm")] {
        let mut args = vec![
            "build",
            "valid.vl",
            "--format",
            "json",
            "--out",
            "missing/output",
        ];
        if let Some(emit) = emit {
            args.extend(["--emit", emit]);
        }
        let output = run(&root, &args);
        assert_eq!(output.status.code(), Some(2), "emit {emit:?}");
        assert!(
            output.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report = parse_stdout_json(&output);
        assert_eq!(report["ok"], false, "{report}");
        assert!(
            report["diagnostics"]
                .as_array()
                .expect("diagnostics array")
                .iter()
                .any(|d| d["code"] == "E601"),
            "{report}"
        );
    }
    fs::remove_dir_all(root).expect("remove temporary project");
}

#[test]
fn shallow_project_dumps_do_not_validate_duplicate_entrypoints() {
    let root = temp_project("shallow-entrypoints");
    fs::create_dir_all(root.join("src")).expect("source directory");
    fs::write(root.join("vl.toml"), "module = \"demo\"\n").expect("config");
    for name in ["a", "b"] {
        fs::write(root.join(format!("src/{name}.vl")), "fun main() {}").expect("source");
    }
    for emit in ["tokens", "ast"] {
        let output = run(&root, &["build", "--emit", emit, "--format", "json"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let report = parse_stdout_json(&output);
        assert_eq!(report["ok"], true, "{report}");
    }
    let output = run(&root, &["build", "--format", "json"]);
    assert!(!output.status.success());
    let report = parse_stdout_json(&output);
    assert!(
        report["diagnostics"]
            .as_array()
            .expect("diagnostics array")
            .iter()
            .any(|d| d["code"] == "E401"),
        "{report}"
    );
    fs::remove_dir_all(root).expect("remove temporary project");
}

#[test]
fn imported_generic_readonly_projection_cannot_return_mutable_capability() {
    let root = temp_project("imported-readonly-projection");
    fs::create_dir_all(root.join("src")).expect("source directory");
    fs::write(root.join("vl.toml"), "module = \"demo\"\n").expect("config");
    fs::write(
        root.join("src/lib.vl"),
        "fun first[T](a: Array[T]): T { val x = a[0u64]; return x; }",
    )
    .expect("provider");
    fs::write(root.join("src/main.vl"), "use demo.lib.{first}; type Foo = object { value: u64, }; fun main() { val a: Array[*Foo] = [Foo { value = 1u64 }]; var leaked = first(a); leaked.value = 2u64; }").expect("caller");
    let output = run(&root, &["build", "--format", "json"]);
    assert!(!output.status.success(), "mutable capability escaped");
    let report = parse_stdout_json(&output);
    assert!(
        report["diagnostics"]
            .as_array()
            .expect("diagnostics")
            .iter()
            .any(|d| d["severity"] == "error"),
        "{report}"
    );
    assert!(
        !report["diagnostics"]
            .as_array()
            .expect("diagnostics")
            .iter()
            .any(|d| d["code"] == "E500"),
        "{report}"
    );
    fs::remove_dir_all(root).expect("remove temporary project");
}

#[test]
fn warning_only_single_file_checks_and_builds_keep_diagnostics() {
    let root = temp_project("json-success-warnings");
    fs::write(
        root.join("warn.vl"),
        "fun main() { val x = 1u64; val x = 2u64; x; }",
    )
    .expect("source");
    for args in [
        vec!["check", "warn.vl", "--format", "json"],
        vec!["build", "warn.vl", "--format", "json", "--out", "warn.nara"],
        vec![
            "build", "warn.vl", "--emit", "lir", "--format", "json", "--out", "warn.lir",
        ],
    ] {
        let output = run(&root, &args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let report = parse_stdout_json(&output);
        assert_eq!(report["ok"], true, "{report}");
        assert!(
            report["diagnostics"]
                .as_array()
                .expect("diagnostics")
                .iter()
                .any(|d| d["severity"] == "warning"),
            "{report}"
        );
    }
    fs::remove_dir_all(root).expect("remove temporary project");
}

#[test]
fn imported_identity_cannot_launder_a_generic_readonly_projection() {
    let root = temp_project("imported-identity-projection");
    fs::create_dir_all(root.join("src")).expect("source directory");
    fs::write(root.join("vl.toml"), "module = \"demo\"\n").expect("config");
    fs::write(root.join("src/lib.vl"), "fun id[T](x: T): T { return x; }").expect("provider");
    fs::write(root.join("src/main.vl"), "use demo.lib.{id}; type Foo = object { value: u64, }; fun first[T](a: Array[T]): T { return id(a[0u64]); } fun main() { val a: Array[*Foo] = [Foo { value = 1u64 }]; var leaked = first(a); leaked.value = 2u64; }").expect("caller");
    let output = run(&root, &["build", "--format", "json"]);
    assert!(!output.status.success(), "mutable capability escaped");
    let report = parse_stdout_json(&output);
    assert!(
        report["diagnostics"]
            .as_array()
            .expect("diagnostics")
            .iter()
            .any(|d| d["severity"] == "error"),
        "{report}"
    );
    assert!(
        !report["diagnostics"]
            .as_array()
            .expect("diagnostics")
            .iter()
            .any(|d| d["code"] == "E500"),
        "{report}"
    );
    fs::remove_dir_all(root).expect("remove temporary project");
}
