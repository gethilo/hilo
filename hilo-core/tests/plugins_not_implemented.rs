//! DF-WARPFS-59: manifest-declared plugin hooks must never be silent.
//!
//! `Manifest::parse` prints a NOT-IMPLEMENTED warning to stderr for every
//! declared plugin (AC1). eprintln output is process-global, so the warning
//! cannot be captured inside the test process itself — this test instead
//! RE-EXECS the test binary as a child with a sentinel env var set, lets
//! that child run the parse, and asserts the parent captured the warning on
//! the child's stderr.

use std::process::Command;

const SENTINEL: &str = "DF_WARPFS_59_EMIT_WARNING_CHILD";

fn manifest_with_plugin_yaml() -> String {
    "project:\n  name: test\nplugins:\n  - name: sql-scanner\n    wasm: .vfs/plugins/sql-scanner.wasm\n    hooks:\n      - on: file_write\n        languages: [go]\n        priority: 10\n"
        .to_string()
}

#[test]
fn parse_of_plugin_declaring_manifest_warns_on_stderr() {
    if std::env::var_os(SENTINEL).is_some() {
        // Child arm: parse a plugin-declaring manifest, then exit before the
        // test harness prints anything else on stderr.
        let manifest = hilo_core::manifest::Manifest::parse(&manifest_with_plugin_yaml())
            .expect("plugin-declaring manifest must still parse");
        assert_eq!(manifest.plugins.len(), 1);
        std::process::exit(0);
    }

    // Parent arm: re-exec THIS test binary with the sentinel set and capture
    // the child's stderr.
    let exe = std::env::current_exe().expect("current_exe available under cargo test");
    let output = Command::new(exe)
        .arg("--exact")
        .arg("parse_of_plugin_declaring_manifest_warns_on_stderr")
        .arg("--nocapture")
        .env(SENTINEL, "1")
        .env_remove("RUST_LOG")
        .output()
        .expect("re-exec of the test binary");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "child arm must succeed; stderr was:\n{stderr}"
    );
    assert!(
        stderr.contains("sql-scanner"),
        "warning must name the declared plugin; stderr was:\n{stderr}"
    );
    assert!(
        stderr.contains("NOT IMPLEMENTED"),
        "warning must say plugin execution is NOT IMPLEMENTED; stderr was:\n{stderr}"
    );
    assert!(
        stderr.contains("file_write"),
        "warning must name the declared hook; stderr was:\n{stderr}"
    );
}

#[test]
fn parse_of_manifest_without_plugins_is_quiet() {
    if std::env::var_os(SENTINEL).is_some() {
        let _ = hilo_core::manifest::Manifest::parse("project:\n  name: test\n")
            .expect("minimal manifest parses");
        std::process::exit(0);
    }

    let exe = std::env::current_exe().expect("current_exe available under cargo test");
    let output = Command::new(exe)
        .arg("--exact")
        .arg("parse_of_manifest_without_plugins_is_quiet")
        .arg("--nocapture")
        .env(SENTINEL, "1")
        .env_remove("RUST_LOG")
        .output()
        .expect("re-exec of the test binary");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "child arm must succeed");
    assert!(
        !stderr.contains("NOT IMPLEMENTED"),
        "a manifest without plugins must not warn; stderr was:\n{stderr}"
    );
}
