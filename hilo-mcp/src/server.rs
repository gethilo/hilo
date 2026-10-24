//! stdio-based MCP server loop.
//!
//! Reads newline-delimited JSON-RPC 2.0 requests from stdin and writes
//! newline-delimited JSON-RPC 2.0 responses to stdout.

use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex, OnceLock};

use tracing::{debug, info, warn};

use crate::error::{McpError, McpResult};
use crate::rate_limiter::{RateLimiter, BACKPRESSURE_WARNING};
use crate::tools;

/// The process-wide rate limiter, shared by the stdio loop ([`run`]) and
/// [`handle_request`]. `handle_request` is the unit/integration test seam —
/// it must answer exactly like the live loop — so the warning attachment
/// reads the limiter the loop drives rather than a per-call instance.
///
/// REV-WARPFS-5 metrics endpoint note: because the handle is process-wide,
/// an embedded HTTP transport (REV-WARPFS-3) can serve `/metrics` from
/// [`server_metrics_json`] and observe the same counters the stdio loop
/// mutates. A stdio-only process never serves HTTP, so the endpoint is
/// unreachable there by construction.
fn shared_limiter() -> &'static Arc<Mutex<RateLimiter>> {
    static LIMITER: OnceLock<Arc<Mutex<RateLimiter>>> = OnceLock::new();
    LIMITER.get_or_init(|| Arc::new(Mutex::new(RateLimiter::new(0))))
}

/// Replace the process-wide limiter with one built for `rate_limit_rps`.
///
/// `run` calls this once on startup so the counters describe THIS server
/// session (a test or an embedder may have driven the shared limiter
/// earlier). Pending locks are waited on, never poisoned-and-ignored: a
/// panicking holder would leave every later request unratable.
fn reset_shared_limiter(rate_limit_rps: u32) {
    let mut guard = match shared_limiter().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    *guard = RateLimiter::new(rate_limit_rps);
}

/// A clone of the shared limiter handle for embedders that want to drive
/// admission themselves (e.g. an HTTP transport sharing this process).
pub fn limiter_handle() -> Arc<Mutex<RateLimiter>> {
    Arc::clone(shared_limiter())
}

/// REV-WARPFS-5: the metrics snapshot of the live limiter, as served by the
/// HTTP transport's `/metrics` endpoint —
/// `{"rate": <rps>, "queue_depth": <n>, "rejected": <n>}`. `rate` mirrors
/// the configured sustained rate (0 = unlimited). Returns zeros before the
/// first `run` (no limiter configured yet).
pub fn server_metrics_json() -> serde_json::Value {
    let guard = match shared_limiter().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard.metrics_json()
}

/// REV-WARPFS-5: `/metrics` route handler, provided by the hilo-mcp crate so
/// the HTTP transport (REV-WARPFS-3) serves the canonical payload shape
/// instead of inventing its own. Returns the HTTP status and the JSON body;
/// the transport renders them. The endpoint is only REACHABLE over HTTP —
/// the stdio server has no socket, so a stdio-only deployment can never
/// serve this route.
pub fn metrics_response() -> (u16, serde_json::Value) {
    (200, server_metrics_json())
}

/// Run the MCP server on stdin/stdout with optional rate limiting.
///
/// `rate_limit_rps` is the maximum requests per second. 0 disables rate limiting.
/// Blocks until stdin reaches EOF (client disconnects).
///
/// REV-WARPFS-5: admitted responses carry `"warning": "high queue depth"`
/// while the token bucket is more than 80% consumed (backpressure signal);
/// `rate_limit_rps = 0` (unlimited) never warns.
///
/// GAP-094: every tool call opens the graph (`GraphDB::open`), so a
/// fingerprint miss used to replay the whole `edges.jsonl` inside the call —
/// measured at a single 207.2 s call. This server is the long-lived process the
/// hazard belongs to, so it arms the request-path reconcile budget: a call may
/// replay for [`hilo_graph::DEFAULT_REQUEST_PATH_RECONCILE_BUDGET_MS`], then it
/// answers from `edges.jsonl` and the next call resumes the checkpoint. A
/// project can override the cap (or set `0` for unbounded) in its manifest's
/// `performance.duckdb.reconcile_budget_ms`.
///
/// Run the MCP server on stdin/stdout with optional rate limiting.
///
/// `rate_limit_rps` is the maximum requests per second. 0 disables rate limiting.
/// Blocks until stdin reaches EOF (client disconnects).
///
/// GAP-094: every tool call opens the graph (`GraphDB::open`), so a
/// fingerprint miss used to replay the whole `edges.jsonl` inside the call —
/// measured at a single 207.2 s call. This server is the long-lived process the
/// hazard belongs to, so it arms the request-path reconcile budget: a call may
/// replay for [`hilo_graph::DEFAULT_REQUEST_PATH_RECONCILE_BUDGET_MS`], then it
/// answers from `edges.jsonl` and the next call resumes the checkpoint. A
/// project can override the cap (or set `0` for unbounded) in its manifest's
/// `performance.duckdb.reconcile_budget_ms`.
pub fn run(rate_limit_rps: u32) -> McpResult<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    run_with(rate_limit_rps, stdin.lock(), stdout.lock())
}

/// Extract a JSON-RPC request id for a rate-limit rejection.
///
/// This deliberately accepts only the request-id types permitted by JSON-RPC:
/// strings and numbers. Missing, null, boolean, object, and array ids use the
/// protocol-safe `null` fallback. Parsing failures also use `null` because
/// there is no usable id to echo.
fn request_id_for_rejection(line: &str) -> serde_json::Value {
    serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .and_then(|request| request.get("id").cloned())
        .filter(|id| id.is_string() || id.is_number())
        .unwrap_or(serde_json::Value::Null)
}

/// Injectable core of [`run`]: the transport is parameters, so the FULL loop
/// (rate-limit admission, backpressure warnings, NDJSON framing) is
/// integration-testable in-process — `hilo-mcp` cannot reference the
/// `hilo-cli` binary (`CARGO_BIN_EXE_*` only resolves same-crate binaries),
/// and driving a real subprocess per test would serialize the suite.
pub fn run_with<R: BufRead, W: Write>(
    rate_limit_rps: u32,
    reader: R,
    mut writer: W,
) -> McpResult<()> {
    hilo_graph::set_request_path_reconcile_budget_ms(
        hilo_graph::DEFAULT_REQUEST_PATH_RECONCILE_BUDGET_MS,
    );

    // REV-WARPFS-5: the stdio loop drives the process-wide limiter so
    // `handle_request` (and an embedded HTTP transport) observes the same
    // counters. Reset first so the metrics describe this session.
    reset_shared_limiter(rate_limit_rps);
    let limiter = limiter_handle();

    info!(
        "MCP server started (rate_limit_rps={rate_limit_rps}, reconcile_budget_ms={})",
        hilo_graph::request_path_reconcile_budget_ms()
    );

    for line_result in reader.lines() {
        let line = line_result?;

        // Skip blank lines between requests.
        if line.trim().is_empty() {
            continue;
        }

        // Parse just enough of the request before admission so a rejected
        // JSON-RPC error can echo its request id. The admitted path still
        // goes through `handle_request`, which owns normal request parsing
        // and routing.
        let request_id = request_id_for_rejection(&line);

        // Rate-limit check before processing.
        let mut admission = Admission::Allowed;
        {
            let mut lim = match limiter.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            if !lim.check() {
                admission = Admission::Rejected(lim.retry_after_secs().ceil() as u64);
            }
        }
        if let Admission::Rejected(retry_secs) = admission {
            warn!("rate limit exceeded, retry after {retry_secs}s");
            let response = serde_json::json!({
                "jsonrpc": "2.0",
                "id": request_id,
                "error": {
                    "code": -32000,
                    "message": format!(
                        "Rate limit exceeded. Retry after {} seconds.",
                        retry_secs
                    ),
                    "data": {
                        "retry_after_seconds": retry_secs
                    }
                }
            });
            let response_line = serde_json::to_string(&response)?;
            writer.write_all(response_line.as_bytes())?;
            writer.write_all(b"\n")?;
            writer.flush()?;
            continue;
        }

        match handle_request(&line)? {
            Some(response) => {
                let response_line = serde_json::to_string(&response)?;
                writer.write_all(response_line.as_bytes())?;
                writer.write_all(b"\n")?;
                writer.flush()?;
            }
            None => {
                // Notification (no id) — no response required.
            }
        }
    }

    info!("MCP server stopped");
    Ok(())
}

/// Admission outcome of the token-bucket check in [`run`].
enum Admission {
    Allowed,
    Rejected(u64),
}

/// Parse a JSON-RPC request line, route it, and produce the response value.
///
/// Returns `Ok(None)` for notifications (requests without an `id`).
/// Returns `Ok(Some(value))` with a full JSON-RPC response object otherwise.
/// The response object includes `"jsonrpc":"2.0"`, the matching `"id"`, and
/// either a `"result"` or an `"error"`.
pub fn handle_request(line: &str) -> McpResult<Option<serde_json::Value>> {
    // --- Parse -------------------------------------------------------
    let parsed: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => {
            return Ok(Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": {
                    "code": -32700,
                    "message": "Parse error"
                }
            })));
        }
    };

    // --- Extract id / method / params --------------------------------
    let id = parsed.get("id").cloned().unwrap_or(serde_json::Value::Null);

    // Notifications (no id) get no response.
    if id.is_null() {
        return Ok(None);
    }

    let method = parsed.get("method").and_then(|v| v.as_str()).unwrap_or("");

    let params = parsed
        .get("params")
        .cloned()
        .unwrap_or(serde_json::json!({}));

    // --- Route -------------------------------------------------------
    //
    // `inner` is Ok(value) for successful handler results, Err(e) for
    // handler failures (which become -32603 errors).  The unknown-method
    // arm diverges early with a -32601 response.
    debug!("MCP method: {method}");
    let inner: Result<serde_json::Value, McpError> = match method {
        "initialize" => Ok(serde_json::json!({
            "protocolVersion": "2024-11-05",
            "serverInfo": {
                "name": "hilo-mcp",
                // DF-WARPFS-17: always the crate's real version — a
                // hand-written literal went stale (0.2.0) through the 0.3.0
                // release. This cannot drift again.
                "version": env!("CARGO_PKG_VERSION")
            },
            "capabilities": {
                "tools": {}
            }
        })),

        "tools/list" => Ok(serde_json::json!({
            "tools": tools::list_tools()
        })),

        "tools/call" => (|| -> Result<serde_json::Value, McpError> {
            let tool_name = params
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| McpError::Protocol("missing 'name' in params".into()))?;

            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::json!({}));

            let tool_result = tools::call_tool(tool_name, &arguments)?;

            Ok(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string(&tool_result)?
                }]
            }))
        })(),

        _ => {
            return Ok(Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32601,
                    "message": format!("Method not found: {method}")
                }
            })));
        }
    };

    // --- Build response ----------------------------------------------
    //
    // REV-WARPFS-5 backpressure signal: an admitted response carries
    // `"warning": "high queue depth"` while the shared token bucket is more
    // than 80% consumed. Unlimited mode (`rate_limit_rps = 0`) can never be
    // under backpressure, so responses there stay untouched. The shared
    // limiter is the same instance the stdio loop admits through, so the
    // signal reflects the server's live state (and integration tests can
    // drive it through `limiter_handle()`).
    let warning = {
        let lim = match shared_limiter().lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if lim.under_backpressure() {
            Some(BACKPRESSURE_WARNING.to_string())
        } else {
            None
        }
    };
    let response = match inner {
        Ok(result_value) => {
            let mut obj = serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result_value
            });
            if let Some(w) = warning {
                obj["warning"] = serde_json::Value::String(w);
            }
            obj
        }
        Err(e) => {
            let mut obj = serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32603,
                    "message": e.to_string()
                }
            });
            if let Some(w) = warning {
                obj["warning"] = serde_json::Value::String(w);
            }
            obj
        }
    };

    Ok(Some(response))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialize tests that mutate the process-wide limiter: the shared
    /// limiter IS process state, so parallel test threads would see each
    /// other's bucket.
    fn limiter_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Install a fresh limiter with the given rate for one test, restoring
    /// unlimited on drop (even on panic) so later tests are unaffected.
    struct WithLimiter;
    impl WithLimiter {
        fn new(rate: u32) -> Self {
            let mut guard = match shared_limiter().lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            *guard = RateLimiter::new(rate);
            Self
        }

        fn limiter(&self) -> Arc<Mutex<RateLimiter>> {
            limiter_handle()
        }
    }
    impl Drop for WithLimiter {
        fn drop(&mut self) {
            let mut guard = match shared_limiter().lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            *guard = RateLimiter::new(0);
        }
    }

    fn initialize_line(id: u64) -> String {
        format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"initialize","params":{{}}}}"#)
    }

    #[test]
    fn response_without_backpressure_has_no_warning() {
        let _lock = limiter_test_lock();
        let env = WithLimiter::new(10); // full bucket, depth 0
        let resp = handle_request(&initialize_line(1))
            .unwrap()
            .expect("initialize gets a response");
        assert!(resp.get("warning").is_none(), "full bucket must not warn");
        assert_eq!(resp["result"]["serverInfo"]["name"], "hilo-mcp");
        drop(env);
    }

    #[test]
    fn response_under_backpressure_carries_warning() {
        let _lock = limiter_test_lock();
        let env = WithLimiter::new(2);
        let handle = env.limiter();
        {
            let mut lim = handle.lock().unwrap();
            // Drain the bucket: depth 2/2 = 100% > 80%.
            assert!(lim.check());
            assert!(lim.check());
            assert!(lim.under_backpressure());
        }
        let resp = handle_request(&initialize_line(2))
            .unwrap()
            .expect("initialize gets a response");
        assert_eq!(
            resp["warning"], BACKPRESSURE_WARNING,
            "drained bucket must attach the backpressure warning"
        );
        // The frame stays a valid success response.
        assert_eq!(resp["result"]["serverInfo"]["name"], "hilo-mcp");
    }

    #[test]
    fn error_frames_carry_the_warning_too() {
        let _lock = limiter_test_lock();
        let env = WithLimiter::new(2);
        let handle = env.limiter();
        {
            let mut lim = handle.lock().unwrap();
            lim.check();
            lim.check();
        }
        let resp = handle_request(
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"vfs_no_such_tool","arguments":{}}}"#,
        )
        .unwrap()
        .expect("tool call gets a response");
        assert_eq!(resp["error"]["code"], -32603);
        assert_eq!(resp["warning"], BACKPRESSURE_WARNING);
    }

    #[test]
    fn unlimited_mode_never_warns_under_hammering() {
        let _lock = limiter_test_lock();
        let _env = WithLimiter::new(0);
        for id in 100..130 {
            let resp = handle_request(&initialize_line(id))
                .unwrap()
                .expect("initialize gets a response");
            assert!(
                resp.get("warning").is_none(),
                "rate_limit_rps = 0 must stay warning-free"
            );
        }
    }

    #[test]
    fn metrics_response_serves_the_snapshot() {
        let _lock = limiter_test_lock();
        let env = WithLimiter::new(10);
        let handle = env.limiter();
        {
            // Set depth deterministically via the test seam — a check()
            // loop can gain a refill token mid-loop on a loaded host and
            // flake the expected depth.
            let mut lim = handle.lock().unwrap();
            lim.force_tokens(6.0); // depth 4/10
        }
        let (status, body) = metrics_response();
        assert_eq!(status, 200);
        assert_eq!(body["rate"], 10.0);
        assert_eq!(body["queue_depth"], 4);
        assert_eq!(body["rejected"], 0);
    }

    #[test]
    fn metrics_before_any_run_are_zeroed() {
        let _lock = limiter_test_lock();
        let _env = WithLimiter::new(0);
        let (_, body) = metrics_response();
        assert_eq!(body["rate"], 0.0);
        assert_eq!(body["queue_depth"], 0);
        assert_eq!(body["rejected"], 0);
    }
}
