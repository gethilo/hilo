//! GAP-094 — a resident `hilo serve --mcp` must never run an unbounded graph
//! replay inside a tool call.
//!
//! The row's measurement (2026-09-20, 20,000-edge corpus, mid-session append of
//! a copy of `edges.jsonl`): ONE tool call blocked for 207.2 s while the
//! resident process's RSS climbed on top of live state. The acceptance criteria
//! are those two numbers, so this test measures exactly them, against the real
//! binary, in the real long-lived shape:
//!
//! * every tool call returns inside [`CALL_BOUND`] — the server arms a 2 s
//!   request-path reconcile budget (`DEFAULT_REQUEST_PATH_RECONCILE_BUDGET_MS`)
//!   and answers from the canonical JSONL stream when it is spent;
//! * the process's peak RSS (`VmHWM`) does not scale with the corpus across the
//!   mid-session change, and every answer stays correct while the cache is
//!   partial (the degraded path must not answer empty);
//! * the reason is surfaced: the server's own log names the budget it hit.
//!
//! Set `HILO_BIN` to drive another build through the identical probe — the
//! pre-change binary is the RED control (it has no budget, so the same calls
//! replay 20k and then 40k rows inside the request).

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Path to the compiled CLI binary, injected by Cargo at compile time.
/// `HILO_BIN` overrides it so the same probe can drive a control build.
fn hilo_bin() -> String {
    std::env::var("HILO_BIN").unwrap_or_else(|_| env!("CARGO_BIN_EXE_hilo").to_string())
}

/// Stage the binary the probe will drive into the fixture directory and return
/// that private path.
///
/// `CARGO_TARGET_DIR` is shared with every concurrent worktree on this box, so
/// `target/debug/hilo` is a shared artifact: a sibling worktree's `cargo build`
/// overwrites it, and cargo will not necessarily relink ours back (its
/// fingerprint says our binary is up to date while the file on disk is
/// someone else's). An earlier run of this probe drove a pre-change binary that
/// way — the give-away was a 130 s "cold" call under a 2 s budget. Copying also
/// keeps a mid-run relink from swapping the binary between phases.
fn stage_binary(root: &Path) -> PathBuf {
    let source = PathBuf::from(hilo_bin());
    let staged = root.join("hilo-under-test");
    std::fs::copy(&source, &staged)
        .unwrap_or_else(|e| panic!("stage {} -> {}: {e}", source.display(), staged.display()));
    staged
}

/// Whether the staged binary is expected to be this change's build.
/// `HILO_EXPECT_MARKER=0` disables the identity check for a RED control build.
fn expect_own_build() -> bool {
    std::env::var("HILO_EXPECT_MARKER").map_or(true, |value| value != "0")
}

/// Rows in the synthetic corpus. The row measured 20,000 edges (3.3 MB): big
/// enough that the pre-change binary's per-call full replay is unmistakable.
const CORPUS_ROWS: usize = 20_000;

/// Edge endpoints the probe queries at every phase.
const TARGET: &str = "src/file_0.rs";
const TARGET_DEP: &str = "pkg:dep_0";

/// The armed default budget, asserted against the server's own log line.
const SERVER_BUDGET_MS: u64 = 2_000;

/// A call that takes longer than this is the defect: the pre-change binary
/// needs ~17 s for the cold 20k-row replay of this corpus and ~35 s for the
/// doubled one, while a request-path budget of 2 s bounds the budgeted build.
const CALL_BOUND: Duration = Duration::from_secs(8);

/// RPC read timeout, deliberately far above [`CALL_BOUND`] so a regression is
/// measured and reported instead of looking like a hang. The pre-change binary
/// needed more than four minutes for the cold call of this corpus under load,
/// so the timeout is generous enough to produce a number.
const READ_TIMEOUT: Duration = Duration::from_secs(900);

/// Peak-RSS growth allowed over the idle high-water mark once the corpus has
/// doubled mid-session. The row's pre-change numbers put the resident replay at
/// +467 MB over idle in this shape (DuckDB ceiling) against +96 MB for the
/// budgeted build; the corpus itself is 2.6 MB, so a request that ingests a
/// bounded number of rows cannot add hundreds of MB.
const RSS_GROWTH_BOUND_MB: u64 = 256;

/// Growth allowed *between* the pre-change and post-change peaks: the resident
/// peak must not follow the corpus when the corpus doubles.
const RSS_MIDSESSION_BOUND_MB: u64 = 128;

#[test]
fn gap094_resident_mcp_never_replays_unboundedly_inside_a_request() {
    let root = build_project();
    let binary = stage_binary(&root);
    let mut server = Server::start(&root, &binary);

    let idle = server.hwm_mb();
    let (hello, hello_wall) = server.rpc(
        &json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {}
        }),
        READ_TIMEOUT,
    );
    assert_eq!(
        hello["result"]["serverInfo"]["name"], "hilo-mcp",
        "the probe must be talking to the MCP server: {hello}"
    );
    let idle = idle.max(server.hwm_mb());
    // The staged binary must be THIS change's build: the shared target
    // directory means a sibling worktree's build can be sitting at the same
    // path, and the difference is otherwise invisible until a call takes 130 s.
    // `HILO_EXPECT_MARKER=0` opts out for the RED control, which is a
    // pre-change binary by design.
    if expect_own_build() {
        assert!(
            server.stderr_text().contains("reconcile_budget_ms="),
            "the staged binary is not this change's build (no armed reconcile budget in its log): \
             {} — rebuild with `cargo build -p hilo-cli`",
            server.stderr_text()
        );
    }

    // Phase 1 — cold graph: 20,000 rows have never been ingested. The
    // pre-change binary replays all of them inside this call; the budgeted
    // server stops at its budget and still answers correctly.
    let (cold, cold_wall) = server.tool_call("vfs_graph_related", json!({ "path": TARGET }));
    assert_related(&cold, "cold");
    let after_cold = server.hwm_mb();
    // Captured now, asserted at the end: the acceptance criteria are the wall
    // clock and the resident set, and they must be the failures a regression
    // reports first.
    let cold_log = server.stderr_text();

    // Phase 2 — warm call, unchanged file: no replay is possible, so the call
    // is a query on the checkpointed cache.
    let (warm, warm_wall) = server.tool_call("vfs_graph_related", json!({ "path": TARGET }));
    assert_related(&warm, "warm");
    let after_warm = server.hwm_mb();

    // Phase 3 — the row's trigger: append a copy of edges.jsonl (20k -> 40k
    // lines) while the server stays up, then call again.
    let total_bytes = append_copy(&root);
    let (changed, changed_wall) = server.tool_call("vfs_graph_related", json!({ "path": TARGET }));
    assert_related(&changed, "post-change");
    let after_change = server.hwm_mb();

    // The mid-session change's own peak, over the state right before it: the
    // row measured the hazard this way (before_hwm 103.9 MB -> after_replay_hwm
    // 135.5 MB). Absolute growth is reported against the idle high-water mark,
    // because DuckDB's first warm-up sets most of the absolute floor.
    let incremental = after_change.saturating_sub(after_warm);
    let growth = after_change.saturating_sub(idle);
    println!(
        "GAP-094 probe [{}]: idle_hwm={idle}MB cold={:?}(hwm {after_cold}MB) warm={:?}\
         (hwm {after_warm}MB) post-change={:?} hwm={after_change}MB growth_over_idle={growth}MB \
         incremental={incremental}MB corpus_bytes={total_bytes}",
        hilo_bin(),
        cold_wall,
        warm_wall,
        changed_wall,
    );
    println!("GAP-094 server log tail:\n{}", server.stderr_text());

    assert!(
        hello_wall < CALL_BOUND,
        "the handshake must be a handshake: {hello_wall:?}"
    );
    for (phase, wall) in [
        ("cold", cold_wall),
        ("warm", warm_wall),
        ("post-change", changed_wall),
    ] {
        assert!(
            wall <= CALL_BOUND,
            "the {phase} tool call exceeded the bounded wall clock: {wall:?} > {CALL_BOUND:?} \
             (the request-path reconcile budget is {SERVER_BUDGET_MS}ms and must bound every call)"
        );
    }
    assert!(
        growth <= RSS_GROWTH_BOUND_MB,
        "peak RSS grew {growth}MB over the idle high-water mark (bound {RSS_GROWTH_BOUND_MB}MB): \
         a request that ingests a bounded number of rows must not replay the corpus on top of \
         live state"
    );
    assert!(
        incremental <= RSS_MIDSESSION_BOUND_MB,
        "doubling the corpus mid-session raised the resident peak by {incremental}MB \
         (bound {RSS_MIDSESSION_BOUND_MB}MB): the peak must not follow the corpus size"
    );
    assert!(
        cold_log.contains("request-path budget"),
        "the server must name the budget it hit instead of blocking silently; stderr was:\n{cold_log}"
    );
    assert!(
        cold_log.contains(&format!("{SERVER_BUDGET_MS}ms")),
        "the armed default budget must be the one reported; stderr was:\n{cold_log}"
    );
}

/// `vfs_graph_related` must return the seeded edge — a partial cache is served
/// from `edges.jsonl`, never as an empty answer.
fn assert_related(response: &Value, phase: &str) {
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("{phase}: no tool text in {response}"));
    let edges: Value =
        serde_json::from_str(text).unwrap_or_else(|e| panic!("{phase}: unparseable {text}: {e}"));
    let rows = edges
        .as_array()
        .unwrap_or_else(|| panic!("{phase}: expected an edge array, got {edges}"));
    assert!(
        rows.iter()
            .any(|row| row["to"] == json!(TARGET_DEP) && row["from"] == json!(TARGET)),
        "{phase}: {TARGET} -> {TARGET_DEP} must be visible, got {rows:?}"
    );
}

/// Build a minimal Hilo project: a manifest (so `serve --mcp` accepts the root)
/// and a 20,000-edge `edges.jsonl` in the standard layout.
fn build_project() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "hilo_gap094_probe_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or_default()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(".vfs/graph")).expect("create the project layout");
    std::fs::write(
        root.join(".vfs/manifest.yaml"),
        "project:\n  name: gap094-resident-probe\nperformance:\n  duckdb:\n    threads: 4\n",
    )
    .expect("write the manifest");

    let mut body = String::with_capacity(CORPUS_ROWS * 64);
    for i in 0..CORPUS_ROWS {
        body.push_str(&format!(
            "{{\"from\":\"src/file_{i}.rs\",\"to\":\"pkg:dep_{i}\",\"rel\":\"imports\"}}\n"
        ));
    }
    std::fs::write(root.join(".vfs/graph/edges.jsonl"), body).expect("write edges.jsonl");
    root
}

/// Append a copy of `edges.jsonl` to itself while the server is up (the row's
/// experiment-2 trigger) and return the new size in bytes.
fn append_copy(root: &Path) -> u64 {
    let jsonl = root.join(".vfs/graph/edges.jsonl");
    let content = std::fs::read_to_string(&jsonl).expect("read edges.jsonl");
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&jsonl)
        .expect("open edges.jsonl for append");
    file.write_all(content.as_bytes()).expect("append the copy");
    std::fs::metadata(&jsonl).expect("stat edges.jsonl").len()
}

/// A resident `hilo serve --mcp` child plus its stdio and `/proc` hooks.
struct Server {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    stderr: Arc<Mutex<Vec<String>>>,
    pid: u32,
}

impl Server {
    fn start(root: &Path, binary: &Path) -> Self {
        let mut child = Command::new(binary)
            .arg("serve")
            .arg("--mcp")
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn `hilo serve --mcp`");
        let pid = child.id();
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");
        let stderr = child.stderr.take().expect("child stderr");

        // A dedicated reader per stream: the server logs to stderr on every
        // budget stop, and a full pipe would deadlock the child.
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) => {
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        let captured = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&captured);
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                match line {
                    Ok(line) => sink.lock().expect("stderr lock").push(line),
                    Err(_) => break,
                }
            }
        });

        Server {
            child,
            stdin,
            lines,
            stderr: captured,
            pid,
        }
    }

    /// One JSON-RPC round trip, returning the response and the wall clock it
    /// took *including* the server's own work.
    fn rpc(&mut self, request: &Value, timeout: Duration) -> (Value, Duration) {
        let started = Instant::now();
        writeln!(self.stdin, "{request}").expect("write the MCP request");
        self.stdin.flush().expect("flush the MCP request");
        let line = self
            .lines
            .recv_timeout(timeout)
            .expect("an MCP response inside the read timeout");
        let elapsed = started.elapsed();
        let response: Value =
            serde_json::from_str(&line).unwrap_or_else(|e| panic!("bad response {line}: {e}"));
        (response, elapsed)
    }

    fn tool_call(&mut self, tool: &str, arguments: Value) -> (Value, Duration) {
        self.rpc(
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": tool, "arguments": arguments }
            }),
            READ_TIMEOUT,
        )
    }

    /// Peak resident set (MB) of the server process — monotonic per process,
    /// which is what "peak RSS stays within N x warm baseline" asks about.
    fn hwm_mb(&self) -> u64 {
        proc_status_kb(self.pid, "VmHWM:") / 1024
    }

    fn stderr_text(&self) -> String {
        self.stderr.lock().expect("stderr lock").join("\n")
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Read one `kB`-valued field out of `/proc/<pid>/status`.
fn proc_status_kb(pid: u32, field: &str) -> u64 {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
    status
        .lines()
        .find_map(|line| line.strip_prefix(field))
        .and_then(|rest| rest.trim().trim_end_matches("kB").trim().parse().ok())
        .unwrap_or(0)
}
