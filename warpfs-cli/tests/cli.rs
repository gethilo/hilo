//! Integration tests for the WarpFS CLI binary.
//!
//! These tests exercise the compiled `warpfs` binary via [`std::process::Command`].
//! They intentionally avoid tree-sitter, DuckDB, and xattr dependencies so they
//! pass in any CI environment — only filesystem operations are exercised.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Path to the compiled CLI binary, injected by Cargo at compile time.
const BIN: &str = env!("CARGO_BIN_EXE_warpfs-cli");

/// Create a unique temporary directory under the system temp dir.
///
/// Uses `std::env::temp_dir` instead of the `tempfile` crate (which is not a
/// dependency of this crate). Each call produces a unique path from the process
/// id and the current nanosecond timestamp to avoid collisions between parallel
/// test runs.
fn unique_tempdir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock moved backwards")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("warpfs-test-{label}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&dir).expect("failed to create temp dir");
    dir
}

// ─────────────────────── init ───────────────────────

#[test]
fn init_creates_vfs_and_manifest() {
    let dir = unique_tempdir("init");
    let output = Command::new(BIN)
        .arg("init")
        .current_dir(&dir)
        .output()
        .expect("failed to spawn warpfs init");

    assert!(
        output.status.success(),
        "init exited non-zero: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // .vfs/ directory tree must exist.
    assert!(dir.join(".vfs").exists(), ".vfs/ was not created");

    // manifest.yaml must exist and contain version: 2.
    let manifest_path = dir.join(".vfs").join("manifest.yaml");
    assert!(manifest_path.exists(), "manifest.yaml was not created");
    let manifest = fs::read_to_string(&manifest_path).expect("failed to read manifest");
    assert!(
        manifest.contains("version: 2"),
        "manifest should contain 'version: 2', got:\n{manifest}"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn init_is_idempotent() {
    let dir = unique_tempdir("idempotent");

    for i in 0..2 {
        let output = Command::new(BIN)
            .arg("init")
            .current_dir(&dir)
            .output()
            .expect("failed to spawn warpfs init");
        assert!(
            output.status.success(),
            "init pass {i} exited non-zero: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Running twice should not have destroyed or corrupted the manifest.
    let manifest = fs::read_to_string(dir.join(".vfs").join("manifest.yaml"))
        .expect("failed to read manifest after double-init");
    assert!(manifest.contains("version: 2"));

    let _ = fs::remove_dir_all(&dir);
}

// ─────────────────────── meta ───────────────────────

#[test]
fn meta_nonexistent_file_errors() {
    let output = Command::new(BIN)
        .args(["meta", "/nonexistent/path/to/no/such/file"])
        .output()
        .expect("failed to spawn warpfs meta");

    assert!(
        !output.status.success(),
        "meta should exit non-zero for a nonexistent file"
    );
}

// ─────────────────────── graph ───────────────────────

#[test]
fn graph_stats_no_data_prints_message() {
    let dir = unique_tempdir("graph-stats");

    let output = Command::new(BIN)
        .args(["graph", "stats"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn warpfs graph stats");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "graph stats should succeed (exit 0) when there is no data: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("No graph data"),
        "expected a 'No graph data' message, got:\n{stdout}"
    );

    let _ = fs::remove_dir_all(&dir);
}

// ─────────────────────── serve ───────────────────────

#[test]
fn serve_mcp_exits_cleanly_on_eof() {
    // `serve --mcp` starts the MCP stdio server.  With no stdin piped
    // (Command::output gives an empty/closed stdin) the server reads EOF
    // immediately and exits 0.
    let output = Command::new(BIN)
        .args(["serve", "--mcp"])
        .output()
        .expect("failed to spawn warpfs serve --mcp");

    assert!(
        output.status.success(),
        "serve --mcp should exit 0 on stdin EOF: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn serve_without_flag_errors() {
    let output = Command::new(BIN)
        .args(["serve"])
        .output()
        .expect("failed to spawn warpfs serve");

    assert!(
        !output.status.success(),
        "serve without --mcp should exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--mcp"),
        "stderr should mention --mcp, got: {stderr}"
    );
}
