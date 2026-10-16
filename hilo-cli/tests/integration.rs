//! REV-WARPFS-3 — `hilo serve --http` integration test.
//!
//! Starts the real binary with `--http`, waits for the listener, curls
//! `/health` over a raw TCP socket (no HTTP client dependency), and asserts
//! the documented JSON contract: `{"status":"ok","version":"<cargo>"}`.
//! A second arm proves `--mcp` stdio mode still works unchanged (backward
//! compatibility), including its stdout-purity contract.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_hilo");

/// Create a minimal Hilo project: manifest with the HTTP transport bound to
/// port 0 (kernel-assigned ephemeral port).
///
/// Why not the 8766 default: shared fleet hosts run other services on it
/// (a bind conflict is the server's own fail-loud path, exercised in the
/// manual probe). Port 0 flows through the SAME manifest→server plumbing
/// (`interfaces.mcp.port` → `run_with_ready`), and the resolved address is
/// read back from the server's startup log, so the end-to-end contract is
/// unchanged.
fn init_project(dir: &Path) {
    std::fs::create_dir_all(dir.join(".vfs")).expect("create .vfs");
    std::fs::write(
        dir.join(".vfs/manifest.yaml"),
        "version: 2\nproject:\n  name: rev-warpfs-3-http\ninterfaces:\n  mcp:\n    port: 0\n",
    )
    .expect("write manifest");
}

/// Spawn `hilo serve` in `dir` with the given flags.
fn spawn_serve(dir: &Path, flags: &[&str]) -> std::process::Child {
    Command::new(BIN)
        .args(["serve"])
        .args(flags)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn hilo serve")
}

/// Wait until `addr:port` accepts a TCP connection (the server logged its
/// bind, but accept-readiness is the contract a client needs).
fn wait_for_listener(port: u16, deadline: Duration) -> Result<(), String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let start = Instant::now();
    loop {
        match TcpStream::connect_timeout(&addr, Duration::from_millis(250)) {
            Ok(_) => return Ok(()),
            Err(e) => {
                if start.elapsed() > deadline {
                    return Err(format!("port {port} never opened: {e}"));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// Read one log line from the server's stderr (blocking, bounded).
fn read_log_line(stderr: &mut impl Read, deadline: Duration) -> Option<String> {
    let mut line = Vec::new();
    let start = Instant::now();
    let mut byte = [0u8; 1];
    loop {
        match stderr.read(&mut byte) {
            Ok(0) => return None, // EOF
            Ok(_) => {
                if byte[0] == b'\n' {
                    return Some(String::from_utf8_lossy(&line).into_owned());
                }
                line.push(byte[0]);
            }
            Err(_) => return None,
        }
        if start.elapsed() > deadline {
            return None;
        }
    }
}

/// GET a path with `Connection: close` and return the raw HTTP response.
fn raw_get(addr: SocketAddr, path: &str) -> std::io::Result<String> {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes())?;
    let mut raw = String::new();
    stream.read_to_string(&mut raw)?;
    Ok(raw)
}

/// GET with retries while the server finishes binding.
fn get_with_retry(addr: SocketAddr, path: &str) -> String {
    let start = Instant::now();
    loop {
        match raw_get(addr, path) {
            Ok(raw) => return raw,
            Err(e) => {
                if start.elapsed() > Duration::from_secs(10) {
                    panic!("GET {path} never succeeded: {e}");
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// Kill a child and return (stdout, stderr) after exit.
fn stop(mut child: std::process::Child) -> (String, String) {
    let _ = child.kill();
    let out = child.wait_with_output().expect("reap serve child");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn http_health_endpoint() {
    let dir = tempfile::tempdir().expect("tempdir");
    init_project(dir.path());

    // Port 0: kernel-assigned, no collision under parallel test runs. The
    // manifest-driven port and its 8766 default are pinned by unit tests;
    // this end-to-end run pins the full request/response contract.
    let mut child = spawn_serve(dir.path(), &["--http"]);

    // The startup log names the resolved address on the stderr JSON lines.
    let deadline = Duration::from_secs(15);
    let start = Instant::now();
    let stderr = child.stderr.take().expect("stderr piped");
    let mut listener_addr: Option<SocketAddr> = None;
    {
        let mut buffered = std::io::BufReader::new(stderr);
        while start.elapsed() < deadline {
            match read_log_line(&mut buffered, deadline.saturating_sub(start.elapsed())) {
                Some(line) => {
                    // tracing JSON: {"timestamp":"...","level":"INFO","fields":{"message":"HTTP transport listening on 0.0.0.0:PORT"}} (span fields may append)
                    if let Some(idx) = line.find("HTTP transport listening on ") {
                        let rest = &line[idx + "HTTP transport listening on ".len()..];
                        let end = rest.find(['"', '}']).unwrap_or(rest.len());
                        if let Ok(addr) = rest[..end].parse::<SocketAddr>() {
                            listener_addr = Some(addr);
                            break;
                        }
                    }
                }
                None => break,
            }
        }
    }

    let addr = listener_addr.expect("server never logged its listening address");
    // Accept-readiness probe: bind alone is not the client contract.
    wait_for_listener(addr.port(), Duration::from_secs(10)).expect("listener never accepted");

    // The documented curl contract, driven over a raw socket.
    let raw = get_with_retry(addr, "/health");
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
    assert_eq!(parsed["status"], "ok", "body: {body}");
    assert_eq!(parsed["version"], "0.4.0", "body: {body}");

    // Byte-exact documented shape.
    assert_eq!(
        body.trim(),
        "{\"status\":\"ok\",\"version\":\"0.4.0\"}",
        "byte-exact /health body"
    );

    // An unknown route is a 404, not a panic or a hang.
    let raw = get_with_retry(addr, "/definitely-not-a-route");
    assert!(
        raw.starts_with("HTTP/1.1 404"),
        "expected 404 for unknown route, got: {raw}"
    );

    let (stdout, _stderr) = stop(child);
    assert!(
        stdout.trim().is_empty(),
        "stdout must stay protocol-clean; got: {stdout}"
    );
}

#[test]
fn serve_requires_at_least_one_mode() {
    let dir = tempfile::tempdir().expect("tempdir");
    init_project(dir.path());
    let output = Command::new(BIN)
        .args(["serve"])
        .current_dir(dir.path())
        .output()
        .expect("spawn hilo serve (no flags)");
    assert!(
        !output.status.success(),
        "serve with no mode flag must exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--http") && stderr.contains("--mcp"),
        "usage error must name both modes: {stderr}"
    );
}

#[test]
fn mcp_stdio_mode_still_works() {
    // Backward-compatibility arm: the pre-existing stdio behaviour must be
    // unchanged after the CLI gained --http (initialize -> tools/list ->
    // tools/call, then EOF exits 0 with pure JSON-RPC on stdout).
    let dir = tempfile::tempdir().expect("tempdir");
    init_project(dir.path());
    let mut child = spawn_serve(dir.path(), &["--mcp"]);

    {
        let stdin = child.stdin.as_mut().expect("stdin piped");
        stdin
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n")
            .expect("write initialize");
        stdin
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n")
            .expect("write tools/list");
    } // stdin dropped -> EOF -> server exits 0

    let output = child.wait_with_output().expect("reap serve --mcp child");
    assert!(
        output.status.success(),
        "serve --mcp must exit 0 on EOF: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 2, "two JSON-RPC responses expected: {stdout}");

    for line in &lines {
        let parsed: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("stdout line is not JSON-RPC: {e}: {line}"));
        assert_eq!(parsed["jsonrpc"], "2.0");
        assert!(parsed.get("result").is_some(), "line: {line}");
    }

    let init: serde_json::Value = serde_json::from_str(lines[0]).expect("initialize response");
    assert_eq!(init["result"]["serverInfo"]["name"], "hilo-mcp");
    assert_eq!(init["result"]["serverInfo"]["version"], "0.4.0");

    let tools: serde_json::Value = serde_json::from_str(lines[1]).expect("tools/list response");
    let count = tools["result"]["tools"]
        .as_array()
        .expect("tools array")
        .len();
    assert_eq!(count, 18, "18 vfs_* tools must still be exposed");
}
