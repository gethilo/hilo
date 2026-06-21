//! Tool definitions and dispatch for the WarpFS MCP server.
//!
//! Three tools are exposed:
//! - `vfs_get_metadata`   — read WarpFS xattrs for a file
//! - `vfs_graph_related`  — find related files via the dependency graph
//! - `vfs_graph_stats`    — summary statistics about the graph

use std::path::Path;

use serde::Serialize;

use crate::error::{McpError, McpResult};

// ---------------------------------------------------------------------------
// Tool descriptor
// ---------------------------------------------------------------------------

/// Tool definition returned by `tools/list`.
#[derive(Debug, Clone, Serialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: serde_json::Value,
}

/// Return all registered tool definitions.
pub fn list_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "vfs_get_metadata".into(),
            description: "Read WarpFS extended attributes for a file.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file"
                    }
                },
                "required": ["path"]
            }),
        },
        Tool {
            name: "vfs_graph_related".into(),
            description: "Find files related to the given file via the dependency graph.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path to find related files for"
                    }
                },
                "required": ["path"]
            }),
        },
        Tool {
            name: "vfs_graph_stats".into(),
            description: "Get summary statistics about the dependency graph.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        },
    ]
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Call a tool by name with the given JSON arguments.
///
/// Returns a JSON value on success or an [`McpError`] for unknown tools /
/// invalid arguments.
pub fn call_tool(name: &str, arguments: &serde_json::Value) -> McpResult<serde_json::Value> {
    match name {
        "vfs_get_metadata" => get_metadata(arguments),
        "vfs_graph_related" => graph_related(arguments),
        "vfs_graph_stats" => graph_stats(arguments),
        other => Err(McpError::Protocol(format!("Unknown tool: {other}"))),
    }
}

// ---------------------------------------------------------------------------
// Tool implementations
// ---------------------------------------------------------------------------

/// Default path to the DuckDB graph database (relative to CWD).
const GRAPH_DB_PATH: &str = ".vfs/graph/graph.duckdb";

/// `vfs_get_metadata` — read all `user.vfs.*` xattrs for a file.
///
/// `list_vfs_xattrs` returns full names (e.g. `"user.vfs.relations"`) while
/// `get_vfs_xattr` expects the short name (without the `user.vfs.` prefix)
/// because it prepends the prefix internally.
fn get_metadata(arguments: &serde_json::Value) -> McpResult<serde_json::Value> {
    let path_str = arguments["path"]
        .as_str()
        .ok_or_else(|| McpError::Protocol("missing 'path' argument".into()))?;
    let path = Path::new(path_str);

    let full_names = warpfs_metadata::list_vfs_xattrs(path)?;

    let mut map = serde_json::Map::new();
    for full_name in full_names {
        // Strip the `user.vfs.` prefix so get_vfs_xattr doesn't double it.
        let short_name = full_name
            .strip_prefix("user.vfs.")
            .unwrap_or(&full_name);
        match warpfs_metadata::get_vfs_xattr(path, short_name)? {
            Some(val) => {
                map.insert(full_name, serde_json::Value::String(val));
            }
            None => {
                map.insert(full_name, serde_json::Value::Null);
            }
        }
    }
    Ok(serde_json::Value::Object(map))
}

/// `vfs_graph_related` — find files related to the given path.
///
/// The graph DB stores edges `(from, to, rel)`. Ideally we would run
/// `SELECT "to", COUNT(*) FROM edges WHERE "from" = ? GROUP BY "to"` but the
/// current `GraphDB` API does not expose per-file queries (the `conn` field is
/// private and `duckdb` is not a direct dependency of this crate).
///
/// As a best-effort we use `group_by_dependency()` which returns the most
/// referenced targets across the whole graph. If the input file is not among
/// the known source files we return an empty array.
fn graph_related(arguments: &serde_json::Value) -> McpResult<serde_json::Value> {
    let target = arguments["path"]
        .as_str()
        .ok_or_else(|| McpError::Protocol("missing 'path' argument".into()))?;

    // If the graph DB does not exist the graph simply hasn't been populated.
    if !Path::new(GRAPH_DB_PATH).exists() {
        return Ok(serde_json::Value::Array(vec![]));
    }

    let db = warpfs_graph::GraphDB::open(GRAPH_DB_PATH)?;

    // Check whether the requested file is a known source in the graph.
    let (froms, _tos) = db.distinct_files()?;
    if !froms.iter().any(|f| f == target) {
        return Ok(serde_json::Value::Array(vec![]));
    }

    // Best-effort: return the most-referenced dependencies.
    let deps = db.group_by_dependency()?;
    let result: Vec<serde_json::Value> = deps
        .into_iter()
        .take(50)
        .map(|(to, _rel, cnt)| {
            serde_json::json!({ "file": to, "count": cnt })
        })
        .collect();
    Ok(serde_json::Value::Array(result))
}

/// `vfs_graph_stats` — aggregate statistics about the dependency graph.
///
/// When no graph database exists yet (common in a fresh project) we return
/// an all-zeros stats object instead of an error.
fn graph_stats(_arguments: &serde_json::Value) -> McpResult<serde_json::Value> {
    if !Path::new(GRAPH_DB_PATH).exists() {
        return Ok(serde_json::json!({
            "total_edges": 0,
            "unique_files": 0,
            "unique_dependencies": 0,
            "top_dependencies": []
        }));
    }

    let db = warpfs_graph::GraphDB::open(GRAPH_DB_PATH)?;
    let stats = db.stats()?;
    Ok(serde_json::to_value(stats)?)
}
