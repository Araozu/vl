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
