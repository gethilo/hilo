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

// ─────────────────────── ignore check: builtins + nested ───────────────────────

#[test]
fn ignore_check_reports_builtin_defaults_and_no_defaults_flag() {
    let dir = unique_tempdir("ignore-check-builtins");
    // No .hiloignore: built-in defaults apply (spec §4.2).
    let out = Command::new(BIN)
        .args(["ignore", "check", "target/artifact.bin"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo ignore check");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("ignored: true"),
        "builtin target/ should apply: {stdout}"
    );
    assert!(
        stdout.contains("source: builtin defaults"),
        "expected builtin source, got: {stdout}"
    );

    // --no-default-ignores disables the builtins.
    let out = Command::new(BIN)
        .args([
            "ignore",
            "check",
            "target/artifact.bin",
            "--no-default-ignores",
        ])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo ignore check");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("ignored: false"),
        "no-default-ignores should disable builtins, got: {stdout}"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn ignore_check_reports_nested_ignore_source() {
    let dir = unique_tempdir("ignore-check-nested");
    fs::create_dir(dir.join("sub")).expect("mkdir sub");
    fs::write(dir.join("sub/.hiloignore"), "secret.txt\n").expect("write nested");
    let out = Command::new(BIN)
        .args(["ignore", "check", "sub/secret.txt"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo ignore check");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("ignored: true"),
        "nested rule should apply, got: {stdout}"
    );
    assert!(
        stdout.contains("source: nested sub"),
        "expected nested source, got: {stdout}"
    );
    let _ = fs::remove_dir_all(&dir);
}

// ─────────────────────── workspace ephemeral / wipe ───────────────────────

/// Build a scratch workspace: src/main.rs (persistent), target/artifact.bin
/// and node_modules/pkg/index.js (ephemeral by the built-in catalog).
fn ephemeral_fixture(label: &str) -> PathBuf {
    let dir = unique_tempdir(label);
    fs::create_dir_all(dir.join("src")).expect("mkdir src");
    fs::create_dir_all(dir.join("target")).expect("mkdir target");
    fs::create_dir_all(dir.join("node_modules/pkg")).expect("mkdir node_modules");
    fs::write(dir.join("src/main.rs"), "fn main() {}\n").expect("write main.rs");
    fs::write(dir.join("target/artifact.bin"), vec![0u8; 64]).expect("write artifact");
    fs::write(dir.join("node_modules/pkg/index.js"), vec![0u8; 32]).expect("write index");
    dir
}

#[test]
fn workspace_ephemeral_lists_ephemeral_files_as_tsv() {
    let dir = ephemeral_fixture("ephemeral-list");
    let out = Command::new(BIN)
        .args(["workspace", "ephemeral"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo workspace ephemeral");
    assert!(
        out.status.success(),
        "workspace ephemeral exited non-zero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("target/artifact.bin\t64\t"),
        "expected ephemeral TSV row for target/artifact.bin, got: {stdout}"
    );
    assert!(
        stdout.contains("node_modules/pkg/index.js\t32\t"),
        "expected ephemeral TSV row for node_modules/pkg/index.js, got: {stdout}"
    );
    assert!(
        !stdout.contains("src/main.rs"),
        "persistent src/main.rs must not be listed, got: {stdout}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn workspace_ephemeral_path_filter_limits_listing() {
    let dir = ephemeral_fixture("ephemeral-filter");
    let out = Command::new(BIN)
        .args(["workspace", "ephemeral", "node_modules"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo workspace ephemeral");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("node_modules/pkg/index.js"),
        "expected node_modules row, got: {stdout}"
    );
    assert!(
        !stdout.contains("target/artifact.bin"),
        "path filter must exclude target/, got: {stdout}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn workspace_wipe_dry_run_lists_plan_without_deleting() {
    let dir = ephemeral_fixture("wipe-dry");
    let out = Command::new(BIN)
        .args(["workspace", "wipe", "--ephemeral"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo workspace wipe");
    assert!(
        out.status.success(),
        "workspace wipe exited non-zero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("would remove\ttarget/artifact.bin"),
        "expected dry-run row, got: {stdout}"
    );
    assert!(
        stdout.contains("would free 96 bytes across 2 file(s)"),
        "expected freed-bytes summary, got: {stdout}"
    );
    // Nothing deleted on a dry run.
    assert!(
        dir.join("target/artifact.bin").exists(),
        "dry-run must not delete"
    );
    assert!(dir.join("src/main.rs").exists(), "src must survive");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn workspace_wipe_apply_deletes_only_ephemeral_and_reports_freed() {
    let dir = ephemeral_fixture("wipe-apply");
    let out = Command::new(BIN)
        .args(["workspace", "wipe", "--ephemeral", "--apply"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo workspace wipe");
    assert!(
        out.status.success(),
        "workspace wipe --apply exited non-zero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("removed\ttarget/artifact.bin"),
        "expected removed row, got: {stdout}"
    );
    assert!(
        stdout.contains("freed 96 bytes across 2 file(s)"),
        "expected freed summary, got: {stdout}"
    );
    assert!(
        !dir.join("target/artifact.bin").exists(),
        "ephemeral file must be deleted"
    );
    assert!(
        !dir.join("node_modules/pkg/index.js").exists(),
        "ephemeral file must be deleted"
    );
    assert!(
        dir.join("src/main.rs").exists(),
        "persistent src/main.rs must survive the wipe"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn workspace_wipe_respects_hiloephemeral_negation() {
    let dir = ephemeral_fixture("wipe-negation");
    fs::write(dir.join(".hiloephemeral"), "!target/keep.bin\n").expect("write .hiloephemeral");
    fs::write(dir.join("target/keep.bin"), vec![0u8; 8]).expect("write keep.bin");

    let out = Command::new(BIN)
        .args(["workspace", "wipe", "--ephemeral", "--apply"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo workspace wipe");
    assert!(
        out.status.success(),
        "workspace wipe exited non-zero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // target/keep.bin is re-included by the user rule: it survives; the
    // other ephemeral files are still removed.
    assert!(
        dir.join("target/keep.bin").exists(),
        "negated path must survive the wipe"
    );
    assert!(
        !dir.join("target/artifact.bin").exists(),
        "un-negated ephemeral file must be deleted"
    );
    let _ = fs::remove_dir_all(&dir);
}

// ─────────────────────── backend mount/sync/setup (§9) ───────────────────────

fn write_mounts_yaml(workspace: &std::path::Path, yaml: &str) {
    let dir = workspace.join(".vfs").join("backends");
    fs::create_dir_all(&dir).expect("failed to create .vfs/backends");
    fs::write(dir.join("mounts.yaml"), yaml).expect("failed to write mounts.yaml");
}

#[test]
fn backend_mount_new_surface_writes_mounts_yaml() {
    let dir = unique_tempdir("backend-mount");
    let output = Command::new(BIN)
        .args([
            "backend",
            "mount",
            "--type",
            "s3",
            "--bucket",
            "my-bucket",
            "--prefix",
            "workspace/",
            "--at",
            "/mnt/vfs/ws",
            "--tool",
            "native",
            "--mode",
            "mirror",
        ])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo backend mount");

    assert!(
        output.status.success(),
        "mount exited non-zero: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("mounted s3 s3://my-bucket/workspace/ at /mnt/vfs/ws"),
        "unexpected stdout: {stdout}"
    );

    let mounts = dir.join(".vfs").join("backends").join("mounts.yaml");
    assert!(mounts.exists(), "mounts.yaml was not written");
    let yaml = fs::read_to_string(&mounts).expect("failed to read mounts.yaml");
    assert!(yaml.contains("name: ws"), "mount name missing: {yaml}");
    assert!(yaml.contains("type: s3"), "type missing: {yaml}");
    assert!(yaml.contains("bucket: my-bucket"), "bucket missing: {yaml}");
    assert!(yaml.contains("at: /mnt/vfs/ws"), "at missing: {yaml}");
    assert!(yaml.contains("tool: native"), "tool missing: {yaml}");

    // Second mount with the same name must be rejected (exit 2, InvalidConfig).
    let dup = Command::new(BIN)
        .args([
            "backend",
            "mount",
            "--type",
            "s3",
            "--bucket",
            "other",
            "--at",
            "/mnt/vfs/ws",
            "--tool",
            "native",
        ])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn duplicate mount");
    assert_eq!(dup.status.code(), Some(2), "duplicate mount must exit 2");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn backend_mount_missing_tool_exits_4() {
    let dir = unique_tempdir("backend-mount-tool");
    let output = Command::new(BIN)
        .args([
            "backend",
            "mount",
            "--type",
            "gdrive",
            "--remote",
            "test:path",
            "--at",
            "/mnt/vfs/gd",
            "--tool",
            "gdrive",
        ])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo backend mount");

    // gdrive CLI is not installed in CI; ToolMissing must exit 4 (§12).
    assert_eq!(
        output.status.code(),
        Some(4),
        "expected exit 4, got {:?}; stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("required tool not found"),
        "expected ToolMissing message, got: {stderr}"
    );
    // Nothing may be written on failure.
    assert!(
        !dir.join(".vfs").exists(),
        "mounts.yaml must not be written on failed mount"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn backend_sync_local_pushes_pulls_and_filters() {
    let dir = unique_tempdir("backend-sync");
    let workspace = dir.join("workspace");
    let backend_root = dir.join("backend-root");
    fs::create_dir_all(&workspace).expect("failed to create workspace");
    fs::create_dir_all(&backend_root).expect("failed to create backend root");

    // Ignore *.tmp locally; never synced.
    fs::write(workspace.join(".hiloignore"), "*.tmp\n").expect("failed to write .hiloignore");
    fs::write(workspace.join("a.txt"), "hello\n").expect("failed to write a.txt");
    fs::write(workspace.join("x.tmp"), "local-only\n").expect("failed to write x.tmp");
    fs::create_dir_all(workspace.join("sub")).expect("failed to create sub");
    fs::write(workspace.join("sub/c.txt"), "nested\n").expect("failed to write sub/c.txt");

    write_mounts_yaml(
        &workspace,
        &format!(
            "- name: test\n  type: local\n  prefix: {}\n  mode: mirror\n",
            backend_root.display()
        ),
    );

    // Subtree filter: only sub/ is pushed.
    let filtered = Command::new(BIN)
        .args(["backend", "sync", "--push", "sub"])
        .current_dir(&workspace)
        .output()
        .expect("failed to spawn hilo backend sync");
    assert!(
        filtered.status.success(),
        "sync --push sub failed: {}",
        String::from_utf8_lossy(&filtered.stderr)
    );
    assert!(
        backend_root.join("sub/c.txt").exists(),
        "sub/c.txt should be pushed"
    );
    assert!(
        !backend_root.join("a.txt").exists(),
        "a.txt must not be pushed by a subtree-limited sync"
    );

    // Full push: a.txt lands, x.tmp stays local-only (ignored).
    let pushed = Command::new(BIN)
        .args(["backend", "sync", "--push"])
        .current_dir(&workspace)
        .output()
        .expect("failed to spawn hilo backend sync");
    assert!(
        pushed.status.success(),
        "sync --push failed: {}",
        String::from_utf8_lossy(&pushed.stderr)
    );
    let stdout = String::from_utf8_lossy(&pushed.stdout);
    assert!(
        stdout.contains("skipped ignored"),
        "expected skipped-ignored counter: {stdout}"
    );
    assert!(
        fs::read_to_string(backend_root.join("a.txt")).expect("read a.txt") == "hello\n",
        "a.txt content mismatch"
    );
    assert!(
        !backend_root.join("x.tmp").exists(),
        "ignored x.tmp must never be pushed"
    );

    // Idempotent: a second --both sync transfers nothing (equal mtimes after
    // mtime alignment → the spec's no-ping-pong tie-break).
    let again = Command::new(BIN)
        .args(["backend", "sync"])
        .current_dir(&workspace)
        .output()
        .expect("failed to spawn second sync");
    assert!(
        again.status.success(),
        "second sync failed: {}",
        String::from_utf8_lossy(&again.stderr)
    );
    assert!(
        String::from_utf8_lossy(&again.stdout).contains("0 to transfer"),
        "expected no-op second sync: {}",
        String::from_utf8_lossy(&again.stdout)
    );

    // Remote newer → pull updates the local copy.
    fs::write(backend_root.join("a.txt"), "from-remote\n").expect("failed to update remote");
    let pulled = Command::new(BIN)
        .args(["backend", "sync", "--pull"])
        .current_dir(&workspace)
        .output()
        .expect("failed to spawn pull sync");
    assert!(
        pulled.status.success(),
        "sync --pull failed: {}",
        String::from_utf8_lossy(&pulled.stderr)
    );
    assert!(
        fs::read_to_string(workspace.join("a.txt")).expect("read a.txt") == "from-remote\n",
        "pull did not update local copy"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn backend_sync_no_mounts_exits_2() {
    let dir = unique_tempdir("backend-sync-nomount");
    let output = Command::new(BIN)
        .args(["backend", "sync"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo backend sync");
    assert_eq!(
        output.status.code(),
        Some(2),
        "expected exit 2 without mounts, got {:?}",
        output.status.code()
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn backend_setup_reports_detection_and_next_steps() {
    let dir = unique_tempdir("backend-setup");
    let s3 = Command::new(BIN)
        .args(["backend", "setup", "--type", "s3"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo backend setup");
    assert!(
        s3.status.success(),
        "setup s3 failed: {}",
        String::from_utf8_lossy(&s3.stderr)
    );
    let stdout = String::from_utf8_lossy(&s3.stdout);
    assert!(stdout.contains("== s3 =="), "s3 header missing: {stdout}");
    assert!(
        stdout.contains("credentials:"),
        "credentials check missing: {stdout}"
    );
    assert!(
        stdout.contains("next steps:"),
        "next steps missing: {stdout}"
    );

    let gdrive = Command::new(BIN)
        .args(["backend", "setup", "--type", "gdrive"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo backend setup");
    assert!(
        gdrive.status.success(),
        "setup gdrive failed: {}",
        String::from_utf8_lossy(&gdrive.stderr)
    );
    let stdout = String::from_utf8_lossy(&gdrive.stdout);
    assert!(stdout.contains("gdrive:"), "gdrive line missing: {stdout}");
    assert!(stdout.contains("rclone:"), "rclone line missing: {stdout}");

    // Unknown type is a usage error (exit 1).
    let bad = Command::new(BIN)
        .args(["backend", "setup", "--type", "ftp"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo backend setup");
    assert!(!bad.status.success(), "unknown type must fail");
    let _ = fs::remove_dir_all(&dir);
}
