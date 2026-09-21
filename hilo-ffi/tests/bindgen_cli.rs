use std::process::Command;

#[test]
fn repository_bindgen_generates_python_artifacts() {
    let output = tempfile::tempdir().unwrap();
    let status = Command::new(env!("CARGO_BIN_EXE_uniffi-bindgen"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args([
            "generate",
            "src/hilo.udl",
            "--language",
            "python",
            "--no-format",
            "--out-dir",
        ])
        .arg(output.path())
        .status()
        .expect("run repository uniffi-bindgen");

    assert!(status.success(), "generator exited with {status}");
    assert!(
        output.path().join("hilo.py").is_file(),
        "Python module was not generated under {}",
        output.path().display()
    );
}
