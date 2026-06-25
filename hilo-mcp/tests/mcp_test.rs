//! Integration tests for the hilo-mcp server.
//!
//! These tests exercise `handle_request` directly rather than spawning a
//! subprocess.  (`env!("CARGO_BIN_EXE_*")` only resolves binaries defined in
//! the *same* crate, so it cannot reference `hilo-cli` from `hilo-mcp`'s
//! test suite.)

use hilo_mcp::server::handle_request;

/// Helper: send a JSON-RPC line and return the response value.
fn rpc(line: &str) -> serde_json::Value {
    handle_request(line)
        .expect("handle_request should not return Err for well-formed requests")
        .expect("response should be Some (not a notification)")
}

// -------------------------------------------------------------------------
// initialize
// -------------------------------------------------------------------------

#[test]
fn test_initialize() {
    let resp = rpc(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#);

    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], 1);

    let result = &resp["result"];
    assert_eq!(result["protocolVersion"], "2024-11-05");

    let info = &result["serverInfo"];
    assert_eq!(info["name"], "hilo-mcp");
    assert!(info["version"].is_string());

    // capabilities should advertise tools
    assert!(result["capabilities"]["tools"].is_object());
}

// -------------------------------------------------------------------------
// tools/list
// -------------------------------------------------------------------------

#[test]
fn test_tools_list() {
    let resp = rpc(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#);

    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], 2);

    let tools = resp["result"]["tools"]
        .as_array()
        .expect("tools should be an array");
    assert!(tools.len() >= 4, "expected at least 4 tools");

    // Each tool must have name, description, and inputSchema.
    for t in tools {
        assert!(t["name"].is_string());
        assert!(t["description"].is_string());
        assert!(t["inputSchema"].is_object());
    }

    // Verify the three expected names.
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"vfs_get_metadata"));
    assert!(names.contains(&"vfs_graph_related"));
    assert!(names.contains(&"vfs_graph_stats"));
}

// -------------------------------------------------------------------------
// vfs_get_metadata — nonexistent file should produce an error, not a crash
// -------------------------------------------------------------------------

#[test]
fn test_get_metadata_nonexistent() {
    let req = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"vfs_get_metadata","arguments":{"path":"/nonexistent/hilo-test-file"}}}"#;
    let resp = rpc(req);

    // Must contain an error (not a result) — the file does not exist.
    assert!(
        resp.get("error").is_some(),
        "expected JSON-RPC error for nonexistent path, got: {resp}"
    );
    assert!(resp.get("result").is_none());
    assert_eq!(resp["error"]["code"], -32603);
}

// -------------------------------------------------------------------------
// vfs_graph_stats — empty graph (no .vfs/graph/graph.db in test CWD)
// -------------------------------------------------------------------------

#[test]
fn test_graph_stats_empty() {
    // The test working directory (hilo-mcp/) does not contain a
    // .vfs/graph/graph.db, so the tool should return all-zero stats.
    let req = r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"vfs_graph_stats","arguments":{}}}"#;
    let resp = rpc(req);

    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], 4);

    // The tool result is wrapped in a content array.
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("content[0].text should be a string");

    let stats: serde_json::Value =
        serde_json::from_str(text).expect("tool output should be valid JSON");

    assert_eq!(stats["total_edges"], 0);
    assert_eq!(stats["unique_files"], 0);
    assert_eq!(stats["unique_dependencies"], 0);
    assert_eq!(stats["top_dependencies"], serde_json::json!([]));
}

// -------------------------------------------------------------------------
// Unknown method
// -------------------------------------------------------------------------

#[test]
fn test_unknown_method() {
    let resp = rpc(r#"{"jsonrpc":"2.0","id":5,"method":"frobnicate","params":{}}"#);

    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], 5);

    let error = &resp["error"];
    assert_eq!(error["code"], -32601);
    let msg = error["message"]
        .as_str()
        .expect("error message should be a string");
    assert!(
        msg.contains("frobnicate"),
        "message should mention the method"
    );
}

// -------------------------------------------------------------------------
// Unknown tool name
// -------------------------------------------------------------------------

#[test]
fn test_unknown_tool() {
    let req = r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"nonexistent_tool","arguments":{}}}"#;
    let resp = rpc(req);

    assert_eq!(resp["error"]["code"], -32603);
}

// -------------------------------------------------------------------------
// Malformed JSON → parse error
// -------------------------------------------------------------------------

#[test]
fn test_parse_error() {
    let resp = rpc(r#"this is not json"#);

    assert_eq!(resp["error"]["code"], -32700);
    assert_eq!(resp["id"], serde_json::Value::Null);
}

// -------------------------------------------------------------------------
// Notification (no id) → no response
// -------------------------------------------------------------------------

#[test]
fn test_notification_no_response() {
    let result = handle_request(r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#);
    assert!(result.is_ok());
    assert!(
        result.unwrap().is_none(),
        "notifications should return None"
    );
}
