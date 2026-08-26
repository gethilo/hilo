//! Integration tests for the Hilo CLI binary.
//!
//! These tests exercise the compiled `hilo` binary via [`std::process::Command`].
//! They intentionally avoid tree-sitter, DuckDB, and xattr dependencies so they
//! pass in any CI environment — only filesystem operations are exercised.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Path to the compiled CLI binary, injected by Cargo at compile time.
const BIN: &str = env!("CARGO_BIN_EXE_hilo");

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
    let dir =
        std::env::temp_dir().join(format!("hilo-test-{label}-{}-{nanos}", std::process::id()));
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
        .expect("failed to spawn hilo init");

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
            .expect("failed to spawn hilo init");
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
        .expect("failed to spawn hilo meta");

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
        .expect("failed to spawn hilo graph stats");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "graph stats should succeed (exit 0) when there is no data: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("Graph cache is empty"),
        "expected a 'Graph cache is empty' message, got:\n{stdout}"
    );

    let _ = fs::remove_dir_all(&dir);
}

// ─────────────────────── classify ───────────────────────

#[test]
fn classify_dry_run_does_not_require_vfs() {
    // `classify --dry-run` on an empty directory should exit 0 gracefully.
    let dir = unique_tempdir("classify-dry");

    let output = Command::new(BIN)
        .args(["classify", "--dry-run"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo classify");

    assert!(
        output.status.success(),
        "classify --dry-run should succeed even on empty dir: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn classify_dry_run_with_source_file() {
    // `classify --dry-run` with a source file should print a classification.
    let dir = unique_tempdir("classify-src");
    let src_dir = dir.join("src");
    fs::create_dir_all(&src_dir).expect("failed to create src dir");
    fs::write(src_dir.join("main.rs"), "fn main() {}").expect("failed to write main.rs");

    let output = Command::new(BIN)
        .args(["classify", "--dry-run", "-v"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo classify");

    assert!(
        output.status.success(),
        "classify --dry-run -v should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("entrypoint") || stdout.contains("library") || stdout.contains("main.rs"),
        "classify --dry-run -v should mention a classification for main.rs, got:\n{stdout}"
    );

    let _ = fs::remove_dir_all(&dir);
}

// ─────────────────────── graph warm ───────────────────────

#[test]
fn graph_warm_creates_graph_directory() {
    // `graph warm` should create `.vfs/graph/` and produce edges.
    let dir = unique_tempdir("warm");

    // Create a small project with imports.
    let src = dir.join("src");
    fs::create_dir_all(&src).expect("failed to create src");
    // main.go imports fmt and helper — helper.go defines helper.
    fs::write(
        src.join("main.go"),
        "package main\nimport \"fmt\"\nfunc main() { fmt.Println(\"hi\") }\n",
    )
    .expect("failed to write main.go");
    fs::write(
        src.join("helper.go"),
        "package main\nfunc Helper() string { return \"help\" }\n",
    )
    .expect("failed to write helper.go");

    // Initialize VFS first.
    let init_output = Command::new(BIN)
        .arg("init")
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo init");
    assert!(
        init_output.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init_output.stderr)
    );

    // Run graph warm.
    let output = Command::new(BIN)
        .args(["graph", "warm"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo graph warm");

    assert!(
        output.status.success(),
        "graph warm should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The .vfs/graph/ directory should exist.
    assert!(
        dir.join(".vfs").join("graph").exists(),
        ".vfs/graph/ was not created by graph warm"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn graph_warm_language_filter_unknown_errors() {
    // `--language` with an unsupported language should exit non-zero.
    let dir = unique_tempdir("warm-lang");

    // Init first.
    let init_output = Command::new(BIN)
        .arg("init")
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo init");
    assert!(
        init_output.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init_output.stderr)
    );

    let output = Command::new(BIN)
        .args(["graph", "warm", "--language", "cobol"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo graph warm");

    assert!(
        !output.status.success(),
        "graph warm with unknown language should exit non-zero"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown language") || stderr.contains("cobol"),
        "stderr should mention the unknown language, got: {stderr}"
    );

    let _ = fs::remove_dir_all(&dir);
}

// ─────────────────────── graph impact (absent path) ───────────────────────

#[test]
fn graph_impact_nonexistent_file_errors() {
    let dir = unique_tempdir("impact");

    let output = Command::new(BIN)
        .args(["graph", "impact", "nonexistent.rs"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo graph impact");

    // Contract (GAP-039): a path absent from the graph AND from disk must fail
    // loudly with a non-zero exit and a "not in the graph" error — not silently
    // succeed with an empty result that is indistinguishable from a real node
    // with no dependents.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "graph impact on nonexistent file should exit non-zero, stderr: {stderr}"
    );
    assert!(
        stderr.contains("is not in the graph"),
        "stderr should identify the path as not in the graph, got: {stderr}"
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
        .expect("failed to spawn hilo serve --mcp");

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
        .expect("failed to spawn hilo serve");

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

#[test]
fn mcp_stdio_stdout_is_pure_jsonrpc() {
    use std::io::Write;
    use std::process::Stdio;

    let dir = unique_tempdir("mcp-purity");
    let mut child = Command::new(BIN)
        .args(["serve", "--mcp"])
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn hilo serve --mcp");

    // Naive client: send initialize -> tools/list -> tools/call, then EOF.
    {
        let stdin = child.stdin.as_mut().expect("stdin not piped");
        stdin
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n")
            .expect("failed to write initialize");
        stdin
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n")
            .expect("failed to write tools/list");
        stdin
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"vfs_get_metadata\",\"arguments\":{\"path\":\"/nonexistent/hilo-mcp-purity-test\"}}}\n")
            .expect("failed to write tools/call");
    } // stdin dropped -> EOF -> server exits

    let output = child
        .wait_with_output()
        .expect("failed to wait for hilo serve --mcp");
    assert!(
        output.status.success(),
        "serve --mcp exited non-zero: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Every stdout line must be a JSON-RPC response — zero non-JSON-RPC bytes.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        3,
        "expected exactly 3 JSON-RPC responses, got {lines:?}"
    );
    for (line, id) in lines.iter().zip([1i64, 2, 3]) {
        let v: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("stdout line is not valid JSON ({e}): {line:?}"));
        assert_eq!(v["jsonrpc"], "2.0", "stdout line is not JSON-RPC: {line}");
        assert_eq!(v["id"].as_i64(), Some(id), "response id mismatch: {line}");
    }

    // Tracing logs must have gone to stderr, proving stdout is protocol-only.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("MCP server started"),
        "startup log should be on stderr, got: {stderr}"
    );

    let _ = fs::remove_dir_all(&dir);
}

// ─────────────────────── ignore check ───────────────────────

#[test]
fn ignore_check_reports_decision_and_rule() {
    let dir = unique_tempdir("ignore-check");
    fs::write(dir.join(".hiloignore"), "*.bin\nbuild/\n!keep.bin\n").expect("write .hiloignore");

    // Ignored path: prints ignored:true with the matching rule.
    let out = Command::new(BIN)
        .args(["ignore", "check", "a.bin"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo ignore check");
    assert!(
        out.status.success(),
        "ignore check exited non-zero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("ignored: true"),
        "expected ignored:true, got: {stdout}"
    );
    assert!(
        stdout.contains("rule: *.bin"),
        "expected rule line, got: {stdout}"
    );
    assert!(
        stdout.contains("source: "),
        "expected source line, got: {stdout}"
    );

    // Re-included path: not ignored, but the deciding rule is reported.
    let out = Command::new(BIN)
        .args(["ignore", "check", "keep.bin"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo ignore check");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("ignored: false"),
        "expected ignored:false for re-included path, got: {stdout}"
    );
    assert!(
        stdout.contains("rule: !keep.bin"),
        "expected negation rule reported, got: {stdout}"
    );

    // Unmatched path: not ignored, no rule.
    let out = Command::new(BIN)
        .args(["ignore", "check", "src/main.rs"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo ignore check");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("ignored: false"),
        "expected ignored:false for unmatched path, got: {stdout}"
    );
    assert!(
        stdout.contains("rule: (none)"),
        "expected rule: (none) for unmatched path, got: {stdout}"
    );

    let _ = fs::remove_dir_all(&dir);
}
