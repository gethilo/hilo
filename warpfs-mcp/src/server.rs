//! stdio-based MCP server loop.
//!
//! Reads newline-delimited JSON-RPC 2.0 requests from stdin and writes
//! newline-delimited JSON-RPC 2.0 responses to stdout.

use std::io::{BufRead, BufReader, Write};

use crate::error::{McpError, McpResult};
use crate::tools;

/// Run the MCP server on stdin/stdout.
///
/// Blocks until stdin reaches EOF (client disconnects).
pub fn run() -> McpResult<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();

    for line_result in reader.lines() {
        let line = line_result?;

        // Skip blank lines between requests.
        if line.trim().is_empty() {
            continue;
        }

        match handle_request(&line)? {
            Some(response) => {
                let response_line = serde_json::to_string(&response)? + "\n";
                writer.write_all(response_line.as_bytes())?;
                writer.flush()?;
            }
            None => {
                // Notification (no id) — no response required.
            }
        }
    }

    Ok(())
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
    let id = parsed
        .get("id")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    // Notifications (no id) get no response.
    if id.is_null() {
        return Ok(None);
    }

    let method = parsed
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let params = parsed
        .get("params")
        .cloned()
        .unwrap_or(serde_json::json!({}));

    // --- Route -------------------------------------------------------
    //
    // `inner` is Ok(value) for successful handler results, Err(e) for
    // handler failures (which become -32603 errors).  The unknown-method
    // arm diverges early with a -32601 response.
    let inner: Result<serde_json::Value, McpError> = match method {
        "initialize" => Ok(serde_json::json!({
            "protocolVersion": "2024-11-05",
            "serverInfo": {
                "name": "warpfs-mcp",
                "version": "0.1.0"
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
    let response = match inner {
        Ok(result_value) => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result_value
        }),
        Err(e) => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32603,
                "message": e.to_string()
            }
        }),
    };

    Ok(Some(response))
}
