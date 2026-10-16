//! REV-WARPFS-5 — backpressure + rate-limiter integration tests.
//!
//! These drive the REAL stdio server loop (`server::run_with`) with
//! in-memory pipes, so admission, NDJSON framing, the `-32000` rejection
//! path and the `"warning": "high queue depth"` backpressure signal are
//! exercised end-to-end — not through the `handle_request` seam alone.
//!
//! `run_with` exists exactly for this: `CARGO_BIN_EXE_*` cannot reference
//! the `hilo-cli` binary from `hilo-mcp`'s test suite, and spawning a real
//! subprocess per test would serialize the suite.

use std::io::BufWriter;
use std::sync::{Mutex, MutexGuard, OnceLock};

use hilo_mcp::rate_limiter::BACKPRESSURE_WARNING;
use hilo_mcp::server::{limiter_handle, run_with, server_metrics_json};

/// Serialize every test in this file: `run_with` resets the process-wide
/// limiter and the warning attachment reads it, so parallel test threads
/// would corrupt each other's buckets.
fn lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// Drive the real loop: `requests` NDJSON lines in, parsed JSON-RPC
/// responses out. The whole call holds the loop's lifetime — the pump
/// drains its input and returns when stdin reaches EOF.
fn drive(requests: &[String], rate_limit_rps: u32) -> Vec<serde_json::Value> {
    let input = requests.join("\n");
    let mut output: Vec<u8> = Vec::new();
    {
        let reader = input.as_bytes();
        let writer = BufWriter::new(&mut output);
        run_with(rate_limit_rps, reader, writer).expect("server loop must not fail");
    }
    let text = String::from_utf8(output).expect("responses are UTF-8 JSON");
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("every response line is valid JSON"))
        .collect()
}

fn stats_call(id: usize) -> String {
    // vfs_graph_stats with no graph answers from an empty project — the
    // cheapest admitted method, and it exercises the full tool path.
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"vfs_graph_stats","arguments":{{}}}}}}"#
    )
}

/// AC (REV-WARPFS-5): send 100 requests with rate_limit_rps=10 — the burst
/// capacity is 10, so requests beyond the burst are rejected `-32000` and
/// the survivors are served while the bucket sits above the 80% backpressure
/// threshold: SOME responses must carry the warning.
#[test]
fn rate_limiter_backpressure_one_hundred_requests_at_10rps() {
    let _guard = lock();
    let requests: Vec<String> = (0..100).map(stats_call).collect();
    let responses = drive(&requests, 10);

    // Every request gets exactly one response frame (rejected ones answer
    // with "id": null, so the id map is smaller than the frame list).
    assert_eq!(
        responses.len(),
        100,
        "one response frame per request line, rejections included"
    );

    let warned: Vec<&serde_json::Value> = responses
        .iter()
        .filter(|r| r.get("warning").is_some())
        .collect();
    let rejected: Vec<&serde_json::Value> = responses
        .iter()
        .filter(|r| {
            r.get("error")
                .and_then(|e| e.get("code"))
                .and_then(|c| c.as_i64())
                == Some(-32000)
        })
        .collect();

    assert!(
        !warned.is_empty(),
        "under a 100-request burst at rps=10 at least one admitted response \
         must carry the backpressure warning"
    );
    for w in &warned {
        assert_eq!(
            w["warning"], BACKPRESSURE_WARNING,
            "warning field must be the documented constant"
        );
        assert!(
            w.get("result").is_some() || w.get("error").is_some(),
            "a warned frame is still a complete JSON-RPC response"
        );
    }
    assert!(
        !rejected.is_empty(),
        "requests beyond the 10-token burst must be rejected -32000"
    );
    for r in &rejected {
        assert_eq!(
            r["id"],
            serde_json::Value::Null,
            "rejections predate parsing"
        );
        assert!(
            r["error"]["data"]["retry_after_seconds"].is_u64(),
            "rejection must carry retry_after_seconds"
        );
    }
    // The first request always finds a full bucket: it can never warn.
    assert_eq!(responses[0]["id"], 0, "first request is admitted");
    assert!(
        responses[0].get("warning").is_none(),
        "a full bucket at first admission must not warn"
    );
    // Warned + rejected + clean < 100+... every frame is exactly one class.
    assert!(
        warned.len() + rejected.len() <= responses.len(),
        "classes are disjoint"
    );

    // Metrics after the run: the same counters the loop just drove.
    let m = server_metrics_json();
    assert_eq!(m["rate"], 10.0);
    assert_eq!(m["rejected"], rejected.len() as u64);
    assert!(m["queue_depth"].is_u64());
}

/// Backward compatibility: the same 100-request hammering with
/// rate_limit_rps = 0 (unlimited) never warns and never rejects.
#[test]
fn unlimited_never_warns_through_the_real_loop() {
    let _guard = lock();
    let requests: Vec<String> = (0..100).map(stats_call).collect();
    let responses = drive(&requests, 0);

    assert_eq!(responses.len(), 100);
    for r in &responses {
        assert!(
            r.get("warning").is_none(),
            "unlimited mode must stay warning-free: {r}"
        );
        assert!(
            r.get("error")
                .and_then(|e| e.get("code"))
                .and_then(|c| c.as_i64())
                != Some(-32000),
            "unlimited mode must never reject"
        );
    }
    let m = server_metrics_json();
    assert_eq!(m["rate"], 0.0);
    assert_eq!(m["rejected"], 0);
    assert_eq!(m["queue_depth"], 0);
}

/// The loop's output is strict NDJSON: exactly one line per response frame.
#[test]
fn loop_output_is_newline_delimited() {
    let _guard = lock();
    let requests: Vec<String> = (0..5).map(stats_call).collect();
    let input = requests.join("\n");
    let mut output: Vec<u8> = Vec::new();
    {
        let writer = BufWriter::new(&mut output);
        run_with(0, input.as_bytes(), writer).expect("server loop must not fail");
    }
    let text = String::from_utf8(output).unwrap();
    assert_eq!(text.lines().count(), 5, "one line per response");
    assert!(text.ends_with('\n'), "final frame is newline-terminated");
}

/// Admission mutates the shared limiter the embedder can observe via
/// `limiter_handle()` — the same handle an HTTP transport's /metrics
/// endpoint would read.
#[test]
fn shared_limiter_reflects_loop_admission() {
    let _guard = lock();
    let requests: Vec<String> = (0..20).map(stats_call).collect();
    let _responses = drive(&requests, 10);
    let lim = limiter_handle();
    let guard = lim.lock().unwrap_or_else(|p| p.into_inner());
    assert_eq!(
        guard.admitted_count() + guard.rejected_count(),
        20,
        "every request is counted"
    );
    assert!(
        guard.rejected_count() > 0,
        "20 requests at capacity 10 must reject some"
    );
}
