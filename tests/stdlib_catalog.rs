//! The docs exporter must include both owners of the merged stdlib surface.
#[test]
fn stdlib_command_exports_native_and_generic_helper_signatures() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_vl"))
        .arg("stdlib")
        .output()
        .expect("run the VL driver");
    assert!(output.status.success(), "{:?}", output.stderr);
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("pure JSON stdout");
    let modules = json.as_array().expect("module array");
    let mut expected = vl_codegen::modules();
    vl_stdlib::load().extend_catalog(&mut expected);
    assert_eq!(modules.len(), expected.len());
    for module in expected {
        let path = module.path.as_string();
        let exported = modules
            .iter()
            .find(|m| m["module"] == path)
            .expect("every module exported");
        assert_eq!(
            exported["functions"].as_array().expect("functions").len(),
            module.exports.len()
        );
        assert_eq!(
            exported["errors"].as_array().expect("errors").len(),
            module.errors.len()
        );
    }
    let math = modules
        .iter()
        .find(|m| m["module"] == "std.math")
        .expect("math module");
    let max = math["functions"]
        .as_array()
        .expect("functions")
        .iter()
        .find(|f| f["name"] == "max")
        .expect("generic helper");
    assert_eq!(max["type_params"], serde_json::json!(["T extends Numeric"]));
    assert_eq!(
        max["params"],
        serde_json::json!([
            { "name": "a", "type": "T" }, { "name": "b", "type": "T" }
        ])
    );
    assert_eq!(max["returns"]["type"], "T");
    assert_eq!(max["implementation"], "source");
    let tcp = modules
        .iter()
        .find(|m| m["module"] == "std.net.tcp")
        .expect("native module");
    let read = tcp["functions"]
        .as_array()
        .expect("functions")
        .iter()
        .find(|f| f["name"] == "read")
        .expect("native read");
    assert_eq!(read["returns"]["type"], "TcpError!#(String, bool)");
    assert_eq!(read["implementation"], "native");
}
