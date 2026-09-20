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
fn imported_generic_is_rejected_with_a_focused_diagnostic() {
    let root = temp_project("generic-import");
    fs::create_dir_all(root.join("src")).expect("create source tree");
    fs::write(root.join("vl.toml"), "module = \"demo\"\n").expect("write config");
    fs::write(
        root.join("src/main.vl"),
        "use demo.lib.id; fun main() { id(1u64); }",
    )
    .expect("write main");
    fs::write(
        root.join("src/lib.vl"),
        "fun id[T](value: T): T { return value; }",
    )
    .expect("write library");
    let output = run(&root, &["check", "src/main.vl"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("E207"));
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
