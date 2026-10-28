//! HTTP transport for the Hilo MCP server (axum).
//!
//! `hilo serve --http` exposes the server over TCP on `0.0.0.0` at the port
//! from the project manifest (`interfaces.mcp.port`, default 8766). Today it
//! serves `GET /health` and `GET /metrics`; JSON-RPC over HTTP POST will grow
//! on the same router. The MCP transport is stdio-only (`hilo serve --mcp`,
//! see [`crate::server`]); unknown HTTP routes get a 404 naming that and the
//! two real endpoints. Both transports can run from one process.

use std::net::SocketAddr;

use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use tracing::info;

use crate::error::{McpError, McpResult};

/// Crate version reported by `/health`.
///
/// Same value the `initialize` handler advertises (DF-WARPFS-17: always the
/// crate's real version — a hand-written literal went stale once; this cannot
/// drift again, and other crates can quote it in tests).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Serialize)]
struct HealthStatus {
    status: &'static str,
    version: &'static str,
}

/// Build the HTTP router.
pub fn router() -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics))
        .fallback(not_found)
}

async fn health() -> Json<HealthStatus> {
    Json(HealthStatus {
        status: "ok",
        version: VERSION,
    })
}

/// Body for any route the HTTP surface does not serve.
///
/// The paths an MCP client author tries first (`POST /`, `POST /mcp`) are
/// NOT part of this surface: the MCP transport is stdio-only by design. The
/// 404 says so and names the two endpoints that do exist, instead of axum's
/// bare "page not found".
#[derive(Serialize)]
struct NotFoundHint {
    error: &'static str,
    hint: &'static str,
}

async fn not_found() -> (StatusCode, Json<NotFoundHint>) {
    (
        StatusCode::NOT_FOUND,
        Json(NotFoundHint {
            error: "not found",
            hint: "MCP transport is stdio-only: hilo serve --mcp. HTTP serves GET /health and GET /metrics.",
        }),
    )
}

/// `GET /metrics` — the live rate-limiter snapshot
/// (`{"rate", "queue_depth", "rejected"}`), served from the same
/// process-wide limiter the stdio loop admits through
/// ([`crate::server::server_metrics_json`]). A stdio-only process never binds
/// this router, so the endpoint exists exactly where it is observable.
async fn metrics() -> Json<serde_json::Value> {
    Json(crate::server::server_metrics_json())
}

/// Bind the HTTP listener on `0.0.0.0:{port}`.
///
/// Port 0 lets the OS pick an ephemeral port (used by tests and by operators
/// who want a collision-free port); the resolved address is returned so the
/// caller can log or probe the real endpoint.
async fn bind(port: u16) -> McpResult<(tokio::net::TcpListener, SocketAddr)> {
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    Ok((listener, local))
}

/// Serve HTTP on an already-bound listener until the process exits.
async fn serve_on(listener: tokio::net::TcpListener) -> McpResult<()> {
    axum::serve(listener, router()).await?;
    Ok(())
}

/// Run the HTTP transport on `0.0.0.0:{port}`, blocking the calling thread
/// until the process exits.
pub fn run(port: u16) -> McpResult<()> {
    run_with_ready(port, None)
}

/// [`run`] with a readiness signal.
///
/// `ready` receives `Ok(())` once the listener is bound, or `Err(reason)` if
/// startup failed (port taken, address unavailable), so a caller that starts
/// this on a side thread can fail fast and name the cause instead of racing
/// a health probe against startup. A dropped receiver is not an error: the
/// signal is best effort.
pub fn run_with_ready(
    port: u16,
    ready: Option<std::sync::mpsc::Sender<Result<(), String>>>,
) -> McpResult<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| McpError::Protocol(format!("tokio runtime: {e}")))?;
    runtime.block_on(async move {
        let (listener, addr) = match bind(port).await {
            Ok(binding) => binding,
            Err(e) => {
                if let Some(ready) = ready {
                    let _ = ready.send(Err(e.to_string()));
                }
                return Err(e);
            }
        };
        if let Some(ready) = ready {
            let _ = ready.send(Ok(()));
        }
        info!("HTTP transport listening on {addr}");
        serve_on(listener).await
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::time::Duration;

    /// Start the server on an ephemeral port in a side thread and return the
    /// resolved address. The side thread owns its own tokio runtime, mirroring
    /// how `hilo serve --http` runs it.
    fn start_server() -> SocketAddr {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            runtime.block_on(async move {
                let (listener, addr) = bind(0).await.expect("bind");
                tx.send(addr).expect("address receiver gone");
                if let Err(e) = serve_on(listener).await {
                    eprintln!("serve_on failed: {e}");
                }
            });
        });
        rx.recv_timeout(Duration::from_secs(10))
            .expect("server never bound")
    }

    /// GET a path over a hand-rolled HTTP/1.1 request (no client dependency)
    /// and return the raw response.
    fn raw_get(addr: SocketAddr, path: &str) -> std::io::Result<String> {
        let mut stream = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(2))?;
        let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes())?;
        let mut raw = String::new();
        stream.read_to_string(&mut raw)?;
        Ok(raw)
    }

    #[test]
    fn health_endpoint_serves_status_and_version() {
        let addr = start_server();

        let mut raw = None;
        let mut last_err = None;
        // The listener is bound before the address is handed over, but give a
        // loaded host a few attempts before declaring failure.
        for _ in 0..100 {
            match raw_get(addr, "/health") {
                Ok(response) => {
                    raw = Some(response);
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
        }
        let raw = raw.unwrap_or_else(|| {
            panic!(
                "GET /health never succeeded: {:?}",
                last_err.map(|e| e.to_string())
            )
        });

        assert!(
            raw.starts_with("HTTP/1.1 200"),
            "expected 200 for /health, got: {raw}"
        );
        let body = raw
            .split("\r\n\r\n")
            .nth(1)
            .expect("HTTP response with a body");
        let parsed: serde_json::Value =
            serde_json::from_str(body).unwrap_or_else(|e| panic!("body is not JSON: {e}: {body}"));
        assert_eq!(parsed["status"], "ok");
        assert_eq!(parsed["version"], VERSION);

        // Byte-exact documented shape (serde emits struct field order, compact).
        let expected = format!("{{\"status\":\"ok\",\"version\":\"{VERSION}\"}}");
        assert_eq!(body.trim(), expected);
    }

    #[test]
    fn metrics_endpoint_serves_the_live_limiter_snapshot() {
        let addr = start_server();

        // Unlimited by default (no server loop has run yet in this process),
        // so the shape is pinned but the counters are zeros.
        let mut raw = None;
        for _ in 0..100 {
            if let Ok(response) = raw_get(addr, "/metrics") {
                raw = Some(response);
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let raw = raw.expect("GET /metrics never succeeded");

        assert!(
            raw.starts_with("HTTP/1.1 200"),
            "expected 200 for /metrics, got: {raw}"
        );
        let body = raw
            .split("\r\n\r\n")
            .nth(1)
            .expect("HTTP response with a body");
        let parsed: serde_json::Value =
            serde_json::from_str(body).unwrap_or_else(|e| panic!("body is not JSON: {e}: {body}"));
        assert!(parsed.get("rate").is_some(), "rate missing: {body}");
        assert!(
            parsed.get("queue_depth").is_some(),
            "queue_depth missing: {body}"
        );
        assert!(parsed.get("rejected").is_some(), "rejected missing: {body}");
    }

    #[test]
    fn unknown_route_is_404() {
        let addr = start_server();

        let mut raw = None;
        for _ in 0..100 {
            if let Ok(response) = raw_get(addr, "/nope") {
                raw = Some(response);
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let raw = raw.expect("GET /nope never succeeded");

        assert!(
            raw.starts_with("HTTP/1.1 404"),
            "expected 404 for an unknown route, got: {raw}"
        );
    }

    /// The 404 body must point a lost MCP client author at the real
    /// surface: stdio-only MCP, plus the two HTTP endpoints that exist.
    /// POST / and POST /mcp are the paths clients try first (DF-WARPFS-154).
    #[test]
    fn fallback_404_body_names_stdio_transport_and_both_endpoints() {
        let addr = start_server();

        // One raw POST through the same hand-rolled client, exercising the
        // fallback with a request method the registered routes reject too.
        let raw = raw_post(
            addr,
            "/mcp",
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#,
        )
        .expect("POST /mcp never succeeded");

        assert!(
            raw.starts_with("HTTP/1.1 404"),
            "expected 404 for POST /mcp, got: {raw}"
        );

        let body = raw
            .split("\r\n\r\n")
            .nth(1)
            .expect("HTTP response with a body");
        let parsed: serde_json::Value =
            serde_json::from_str(body).unwrap_or_else(|e| panic!("body is not JSON: {e}: {body}"));

        let hint = parsed["hint"]
            .as_str()
            .unwrap_or_else(|| panic!("404 body missing a string 'hint' field: {body}"));
        assert!(
            hint.contains("stdio") && hint.contains("--mcp"),
            "hint must name the stdio transport: {hint}"
        );
        assert!(
            hint.contains("/health") && hint.contains("/metrics"),
            "hint must name both HTTP endpoints: {hint}"
        );
    }

    /// POST a raw HTTP/1.1 request (same no-client-dependency trick as
    /// [`raw_get`]) and return the raw response.
    fn raw_post(addr: SocketAddr, path: &str, json_body: &str) -> std::io::Result<String> {
        let mut stream = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(2))?;
        let request = format!(
            "POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json_body}",
            json_body.len()
        );
        stream.write_all(request.as_bytes())?;
        let mut raw = String::new();
        stream.read_to_string(&mut raw)?;
        Ok(raw)
    }
}
