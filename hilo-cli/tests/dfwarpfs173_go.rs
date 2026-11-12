//! DF-WARPFS-173: Go module-local imports and hard impact errors.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const BIN: &str = env!("CARGO_BIN_EXE_hilo");

fn temp_repo(label: &str) -> std::path::PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock moved backwards")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("hilo-{label}-{}-{stamp}", std::process::id()));
    fs::create_dir_all(&path).unwrap();
    path
}

fn run(repo: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .output()
        .expect("hilo must spawn")
}

fn init(repo: &Path) {
    let output = run(repo, &["init"]);
    assert!(
        output.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn single_word_go_module_import_is_a_pkg_dependency() {
    let repo = temp_repo("dfwarpfs173-go");
    fs::write(repo.join("go.mod"), "module probea\n\ngo 1.23\n").unwrap();
    fs::create_dir_all(repo.join("app")).unwrap();
    fs::create_dir_all(repo.join("util")).unwrap();
    fs::write(
        repo.join("app/main.go"),
        "package main\n\nimport \"probea/util\"\n\nfunc main() { util.Value() }\n",
    )
    .unwrap();
    fs::write(
        repo.join("util/util.go"),
        "package util\n\nfunc Value() {}\n",
    )
    .unwrap();
    init(&repo);

    let warm = run(&repo, &["graph", "warm"]);
    assert!(
        warm.status.success(),
        "warm failed: {}",
        String::from_utf8_lossy(&warm.stderr)
    );
    let impact = run(&repo, &["graph", "impact", "util/util.go"]);
    let stdout = String::from_utf8_lossy(&impact.stdout);
    let stderr = String::from_utf8_lossy(&impact.stderr);
    assert!(impact.status.success(), "impact failed: {stderr}");
    assert!(
        stdout.contains("app/main.go"),
        "single-word module import must reach its importer; stdout={stdout} stderr={stderr}"
    );
    assert!(
        !stdout.contains("std:probea/util"),
        "module-local import must not be classified as stdlib; stdout={stdout}"
    );
    let _ = fs::remove_dir_all(repo);
}

#[test]
fn initialized_cold_graph_impact_never_reports_success_for_missing_warm() {
    let repo = temp_repo("dfwarpfs173-cold");
    fs::write(repo.join("main.go"), "package main\n").unwrap();
    init(&repo);

    let impact = run(&repo, &["graph", "impact", "main.go"]);
    let stderr = String::from_utf8_lossy(&impact.stderr);
    assert!(
        !impact.status.success(),
        "cold initialized impact must fail or auto-parse, never report a hard warm error with exit 0; stderr={stderr}"
    );
    assert!(
        stderr.contains("No complete graph warm found")
            || stderr.contains("DuckDB")
            || stderr.contains("Binder"),
        "cold impact failure must identify the hard graph/warm error; stderr={stderr}"
    );
    let _ = fs::remove_dir_all(repo);
}
