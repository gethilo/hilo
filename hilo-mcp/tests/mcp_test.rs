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
    assert!(tools.len() >= 5, "expected at least 5 tools");

    // Each tool must have name, description, and inputSchema.
    for t in tools {
        assert!(t["name"].is_string());
        assert!(t["description"].is_string());
        assert!(t["inputSchema"].is_object());
    }

    // Verify the three expected names.
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"vfs_get_metadata"));
    assert!(names.contains(&"vfs_set_metadata"));
    assert!(names.contains(&"vfs_graph_related"));
    assert!(names.contains(&"vfs_graph_stats"));
    assert!(names.contains(&"vfs_graph_untested"));
    assert!(names.contains(&"vfs_graph_module"));
    assert!(names.contains(&"vfs_graph_impact"));
    assert!(names.contains(&"vfs_graph_understand"));
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
// vfs_get_metadata — roundtrip with file stats and xattrs
// -------------------------------------------------------------------------

#[test]
fn test_get_metadata_roundtrip() {
    use std::fs;
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "hello world").unwrap();
    let file_str = file.to_str().unwrap();

    // Set a couple of xattrs via MCP.
    for (k, v) in [("feature", "auth-module"), ("risk", "critical-path")] {
        let set_req = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"vfs_set_metadata","arguments":{{"path":"{file_str}","key":"{k}","value":"{v}"}}}}}}"#
        );
        let resp = rpc(&set_req);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let r: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(r["success"], true);
    }

    // Now call vfs_get_metadata.
    let get_req = format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"vfs_get_metadata","arguments":{{"path":"{file_str}"}}}}}}"#
    );
    let get_resp = rpc(&get_req);
    let text = get_resp["result"]["content"][0]["text"]
        .as_str()
        .expect("content[0].text should be a string");
    let result: serde_json::Value =
        serde_json::from_str(text).expect("tool output should be valid JSON");

    // Top-level fields per spec §21.1.
    assert_eq!(result["path"], file_str);
    assert_eq!(result["size"], 11); // "hello world"
    assert!(result["mtime"].is_string()); // ISO 8601 timestamp
    assert_eq!(result["backend"], "local"); // no user.vfs.backend set
    assert!(result["hash"].is_null()); // no user.vfs.hash set

    // Xattrs nested under "xattrs".
    let xattrs = &result["xattrs"];
    assert_eq!(xattrs["user.vfs.feature"], "auth-module");
    assert_eq!(xattrs["user.vfs.risk"], "critical-path");
}

// -------------------------------------------------------------------------
// vfs_get_metadata — keys filter
// -------------------------------------------------------------------------

#[test]
fn test_get_metadata_keys_filter() {
    use std::fs;
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "hello").unwrap();
    let file_str = file.to_str().unwrap();

    // Set two xattrs.
    for (k, v) in [("feature", "auth"), ("risk", "low")] {
        let req = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"vfs_set_metadata","arguments":{{"path":"{file_str}","key":"{k}","value":"{v}"}}}}}}"#
        );
        rpc(&req);
    }

    // Call with keys filter — only request "feature".
    let get_req = format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"vfs_get_metadata","arguments":{{"path":"{file_str}","keys":["feature"]}}}}}}"#
    );
    let get_resp = rpc(&get_req);
    let text = get_resp["result"]["content"][0]["text"]
        .as_str()
        .expect("content[0].text should be a string");
    let result: serde_json::Value =
        serde_json::from_str(text).expect("tool output should be valid JSON");

    let xattrs = &result["xattrs"];
    // Only the requested key should be present.
    assert_eq!(xattrs["user.vfs.feature"], "auth");
    assert!(
        xattrs.get("user.vfs.risk").is_none(),
        "risk should not be in response when keys filter excludes it"
    );

    // Top-level fields still present.
    assert_eq!(result["path"], file_str);
    assert_eq!(result["size"], 5);
    assert_eq!(result["backend"], "local");
}

// -------------------------------------------------------------------------
// vfs_get_metadata — file with backend + hash xattrs
// -------------------------------------------------------------------------

#[test]
fn test_get_metadata_with_backend_and_hash() {
    use std::fs;
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "data").unwrap();
    let file_str = file.to_str().unwrap();

    // Set backend and hash xattrs.
    for (k, v) in [("backend", "s3"), ("hash", "sha256:abc123")] {
        let req = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"vfs_set_metadata","arguments":{{"path":"{file_str}","key":"{k}","value":"{v}"}}}}}}"#
        );
        rpc(&req);
    }

    let get_req = format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"vfs_get_metadata","arguments":{{"path":"{file_str}"}}}}}}"#
    );
    let get_resp = rpc(&get_req);
    let text = get_resp["result"]["content"][0]["text"]
        .as_str()
        .expect("content[0].text should be a string");
    let result: serde_json::Value =
        serde_json::from_str(text).expect("tool output should be valid JSON");

    assert_eq!(result["backend"], "s3");
    assert_eq!(result["hash"], "sha256:abc123");
    // Xattrs still contain them too.
    assert_eq!(result["xattrs"]["user.vfs.backend"], "s3");
    assert_eq!(result["xattrs"]["user.vfs.hash"], "sha256:abc123");
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
    assert_eq!(stats["total_files"], 0);
    assert!(stats["most_connected"].is_null());
    assert_eq!(stats["orphans"], serde_json::json!([]));
    assert_eq!(stats["edge_types"], serde_json::json!({}));
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

// -------------------------------------------------------------------------
// vfs_set_metadata — set an xattr and verify previous value
// -------------------------------------------------------------------------

#[test]
fn test_set_metadata_roundtrip() {
    use std::fs;
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "hello").unwrap();
    let file_str = file.to_str().unwrap();

    // Set a new attribute (no previous value).
    let set_req = format!(
        r#"{{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{{"name":"vfs_set_metadata","arguments":{{"path":"{file_str}","key":"feature","value":"auth-module"}}}}}}"#
    );
    let set_resp = rpc(&set_req);
    let text = set_resp["result"]["content"][0]["text"]
        .as_str()
        .expect("content[0].text should be a string");
    let result: serde_json::Value =
        serde_json::from_str(text).expect("tool output should be valid JSON");
    assert_eq!(result["success"], true);
    assert!(result["previous_value"].is_null());

    // Overwrite and check the previous value is returned.
    let overwrite_req = format!(
        r#"{{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{{"name":"vfs_set_metadata","arguments":{{"path":"{file_str}","key":"feature","value":"entrypoint"}}}}}}"#
    );
    let ov_resp = rpc(&overwrite_req);
    let ov_text = ov_resp["result"]["content"][0]["text"]
        .as_str()
        .expect("content[0].text should be a string");
    let ov_result: serde_json::Value =
        serde_json::from_str(ov_text).expect("tool output should be valid JSON");
    assert_eq!(ov_result["success"], true);
    assert_eq!(ov_result["previous_value"], "auth-module");
    assert_eq!(ov_result["value"], "entrypoint");
}

#[test]
fn test_set_metadata_empty_key_rejected() {
    use std::fs;
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "hello").unwrap();
    let file_str = file.to_str().unwrap();

    let req = format!(
        r#"{{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{{"name":"vfs_set_metadata","arguments":{{"path":"{file_str}","key":"","value":"x"}}}}}}"#
    );
    let resp = rpc(&req);
    assert!(resp.get("error").is_some(), "expected error for empty key");
    assert_eq!(resp["error"]["code"], -32603);
}

// -------------------------------------------------------------------------
// vfs_graph_untested — returns empty list when no graph exists
// -------------------------------------------------------------------------

#[test]
fn test_graph_untested_empty() {
    // No .vfs/graph/graph.db in the test CWD, so the tool returns empty.
    let req = r#"{"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"vfs_graph_untested","arguments":{}}}"#;
    let resp = rpc(req);

    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], 10);

    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("content[0].text should be a string");

    let result: serde_json::Value =
        serde_json::from_str(text).expect("tool output should be valid JSON");

    assert_eq!(result["total"], 0);
    assert_eq!(result["files"], serde_json::json!([]));
}

// -------------------------------------------------------------------------
// vfs_graph_untested — populated graph returns untested files
// -------------------------------------------------------------------------

#[test]
fn test_graph_untested_populated() {
    use hilo_graph::GraphDB;
    use hilo_metadata::inventory::Edge;

    // Build an in-memory graph with:
    // - src/main.go imports pkg/utils.go (imports edge)
    // - src/auth.go imports pkg/crypto.go (imports edge)
    // - src/auth_test.go is tested_by src/auth.go (tested_by edge)
    // Expected: src/main.go is untested (has imports edge, no tested_by edge)
    //           src/auth.go IS tested (has tested_by edge from auth_test.go)
    let db = GraphDB::open(":memory:").unwrap();
    db.insert_edges(&[
        Edge {
            from: "src/main.go".into(),
            to: "pkg/utils.go".into(),
            rel: "imports".into(),
            provenance: "ast_exact".into(),
            confidence: 1.0,
        },
        Edge {
            from: "src/auth.go".into(),
            to: "pkg/crypto.go".into(),
            rel: "imports".into(),
            provenance: "ast_exact".into(),
            confidence: 1.0,
        },
        Edge {
            from: "src/auth_test.go".into(),
            to: "src/auth.go".into(),
            rel: "tested_by".into(),
            provenance: "heuristic".into(),
            confidence: 0.8,
        },
    ])
    .unwrap();

    let untested = db.untested_files().unwrap();
    assert_eq!(untested.len(), 1);
    assert_eq!(untested[0], "src/main.go");
}

// -------------------------------------------------------------------------
// vfs_graph_module — returns empty when no graph exists
// -------------------------------------------------------------------------

#[test]
fn test_graph_module_empty() {
    let req = r#"{"jsonrpc":"2.0","id":11,"method":"tools/call","params":{"name":"vfs_graph_module","arguments":{"module_name":"src/auth/"}}}"#;
    let resp = rpc(req);

    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], 11);

    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("content[0].text should be a string");

    let result: serde_json::Value =
        serde_json::from_str(text).expect("tool output should be valid JSON");

    assert_eq!(result["module"], "src/auth/");
    assert_eq!(result["edges_count"], 0);
    assert_eq!(result["test_coverage_pct"], 0.0);
    assert_eq!(result["files"], serde_json::json!([]));
}

// -------------------------------------------------------------------------
// vfs_graph_module — populated graph returns correct module stats
// -------------------------------------------------------------------------

#[test]
fn test_graph_module_populated() {
    use hilo_graph::GraphDB;
    use hilo_metadata::inventory::Edge;

    // Build an in-memory graph with files in src/auth/ and src/main/:
    //   src/auth/login.go imports pkg/crypto.go
    //   src/auth/session.go imports pkg/crypto.go
    //   src/auth/login_test.go tested_by src/auth/login.go
    //   src/main/server.go imports src/auth/login.go
    let db = GraphDB::open(":memory:").unwrap();
    db.insert_edges(&[
        Edge {
            from: "src/auth/login.go".into(),
            to: "pkg/crypto.go".into(),
            rel: "imports".into(),
            provenance: "ast_exact".into(),
            confidence: 1.0,
        },
        Edge {
            from: "src/auth/session.go".into(),
            to: "pkg/crypto.go".into(),
            rel: "imports".into(),
            provenance: "ast_exact".into(),
            confidence: 1.0,
        },
        Edge {
            from: "src/auth/login_test.go".into(),
            to: "src/auth/login.go".into(),
            rel: "tested_by".into(),
            provenance: "heuristic".into(),
            confidence: 0.8,
        },
        Edge {
            from: "src/main/server.go".into(),
            to: "src/auth/login.go".into(),
            rel: "imports".into(),
            provenance: "ast_exact".into(),
            confidence: 1.0,
        },
    ])
    .unwrap();

    let stats = db.module_files("src/auth").unwrap();

    // Files in src/auth/: login.go, session.go, login_test.go (3)
    // pkg/crypto.go is NOT in src/auth/ (excluded)
    // src/main/server.go is NOT in src/auth/ (excluded)
    assert_eq!(stats.module, "src/auth");
    assert_eq!(stats.files.len(), 3);
    assert!(stats.files.contains(&"src/auth/login.go".to_string()));
    assert!(stats.files.contains(&"src/auth/session.go".to_string()));
    assert!(stats.files.contains(&"src/auth/login_test.go".to_string()));

    // Edges: login.go→crypto, session.go→crypto, login_test.go→login (3)
    // Plus: server.go→login.go (login.go is "to" but server.go is NOT in src/auth/)
    // The "to" side of the server.go→login edge includes login.go as "to" which IS in src/auth/
    // So: 3 edges with "from" in src/auth/ + 1 edge with "to" in src/auth/ but "from" outside = 4
    assert_eq!(stats.edges_count, 4);

    // Coverage: login.go has tested_by from login_test.go (1 tested / 3 total = 33.3%)
    assert!(
        (stats.test_coverage_pct - 33.3).abs() < 0.1,
        "expected ~33.3% coverage, got {}",
        stats.test_coverage_pct
    );
}

// -------------------------------------------------------------------------
// vfs_backend_status — returns backend info for a local file
// -------------------------------------------------------------------------

#[test]
fn test_backend_status_local() {
    use std::fs;
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "hello").unwrap();
    let file_str = file.to_str().unwrap();

    let req = format!(
        r#"{{"jsonrpc":"2.0","id":20,"method":"tools/call","params":{{"name":"vfs_backend_status","arguments":{{"path":"{file_str}"}}}}}}"#
    );
    let resp = rpc(&req);

    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], 20);

    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("content[0].text should be a string");
    let result: serde_json::Value =
        serde_json::from_str(text).expect("tool output should be valid JSON");

    assert_eq!(result["backend"], "local");
    assert_eq!(result["cache_hit"], true);
    assert!(result["cache_path"].is_null());
    assert!(result["remote_url"].is_null());
    assert_eq!(result["last_synced"], "synced");
}

// -------------------------------------------------------------------------
// vfs_backend_status — nonexistent file returns not-found sync status
// -------------------------------------------------------------------------

#[test]
fn test_backend_status_nonexistent() {
    let req = r#"{"jsonrpc":"2.0","id":21,"method":"tools/call","params":{"name":"vfs_backend_status","arguments":{"path":"/nonexistent/hilo-file-xyz"}}}"#;
    let resp = rpc(&req);

    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], 21);

    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("content[0].text should be a string");
    let result: serde_json::Value =
        serde_json::from_str(text).expect("tool output should be valid JSON");

    assert_eq!(result["backend"], "local");
    assert_eq!(result["cache_hit"], false);
    assert_eq!(result["last_synced"], "not found on disk");
}

// -------------------------------------------------------------------------
// vfs_sync_backend — local file reports synced
// -------------------------------------------------------------------------

#[test]
fn test_sync_backend_local() {
    use std::fs;
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "hello").unwrap();
    let file_str = file.to_str().unwrap();

    let req = format!(
        r#"{{"jsonrpc":"2.0","id":22,"method":"tools/call","params":{{"name":"vfs_sync_backend","arguments":{{"path":"{file_str}"}}}}}}"#
    );
    let resp = rpc(&req);

    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], 22);

    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("content[0].text should be a string");
    let result: serde_json::Value =
        serde_json::from_str(text).expect("tool output should be valid JSON");

    assert_eq!(result["synced_files"], 1);
    assert_eq!(result["errors"], serde_json::json!([]));
}

// -------------------------------------------------------------------------
// vfs_sync_backend — nonexistent file reports error
// -------------------------------------------------------------------------

#[test]
fn test_sync_backend_nonexistent() {
    let req = r#"{"jsonrpc":"2.0","id":23,"method":"tools/call","params":{"name":"vfs_sync_backend","arguments":{"path":"/nonexistent/hilo-file-xyz"}}}"#;
    let resp = rpc(&req);

    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], 23);

    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("content[0].text should be a string");
    let result: serde_json::Value =
        serde_json::from_str(text).expect("tool output should be valid JSON");

    assert_eq!(result["synced_files"], 0);
    let errors = result["errors"]
        .as_array()
        .expect("errors should be an array");
    assert!(!errors.is_empty());
    assert!(errors[0].as_str().unwrap().contains("file not found"));
}
