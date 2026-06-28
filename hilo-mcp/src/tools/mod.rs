//! Tool definitions and dispatch for the Hilo MCP server.
//!
//! Thirteen tools are exposed:
//! - `vfs_get_metadata`   — read Hilo xattrs for a file
//! - `vfs_set_metadata`   — write Hilo xattr for a file
//! - `vfs_graph_related`  — find related files via the dependency graph
//! - `vfs_graph_stats`    — summary statistics about the graph
//! - `vfs_graph_untested` — list files with imports but no test coverage
//! - `vfs_graph_module`   — per-module file listing and coverage statistics
//! - `vfs_graph_impact`   — transitive impact analysis for a file
//! - `vfs_rule_list`      — list all rules defined in the manifest
//! - `vfs_rule_check`     — execute a named rule query against the graph
//! - `vfs_list_directory` — list entries in a virtual directory
//! - `vfs_resolve_path`   — resolve a virtual path to real storage
//! - `vfs_backend_status`  — get backend information for a file
//! - `vfs_sync_backend`    — sync the backend for a file

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
            description: "Read Hilo extended attributes for a file.".into(),
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
            name: "vfs_set_metadata".into(),
            description: "Set a Hilo extended attribute on a file.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file"
                    },
                    "key": {
                        "type": "string",
                        "description": "Attribute name (e.g., 'feature', 'risk', 'purpose')"
                    },
                    "value": {
                        "type": "string",
                        "description": "Attribute value to set"
                    }
                },
                "required": ["path", "key", "value"]
            }),
        },
        Tool {
            name: "vfs_graph_related".into(),
            description: "Find files related to the given file via the dependency graph. Supports forward (outgoing) and reverse (incoming) queries.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path to find related files for"
                    },
                    "relation": {
                        "type": "string",
                        "description": "Filter by relation type (e.g. 'imports', 'tested_by', 'tests')"
                    },
                    "direction": {
                        "type": "string",
                        "description": "Query direction: 'forward' (outgoing, default) or 'reverse' (incoming)"
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
        Tool {
            name: "vfs_graph_untested".into(),
            description: "List files that have import edges but no test coverage (no tested_by edges).".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        },
        Tool {
            name: "vfs_graph_module".into(),
            description: "Get per-module file listing and test coverage statistics from the dependency graph.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "module_name": {
                        "type": "string",
                        "description": "Directory prefix to query (e.g., 'src/auth/')"
                    }
                },
                "required": ["module_name"]
            }),
        },
        Tool {
            name: "vfs_graph_impact".into(),
            description: "Find all files that depend on the given file, directly or transitively.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File path to compute impact for"},
                    "max_depth": {"type": "integer", "description": "Maximum traversal depth (default: 5)"}
                },
                "required": ["path"]
            }),
        },
        Tool {
            name: "vfs_rule_list".into(),
            description: "List all rules defined in the Hilo manifest (stale-files, untested-critical, transitive-impact, etc.).".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        },
        Tool {
            name: "vfs_rule_check".into(),
            description: "Execute a named rule query against the dependency graph and return matching files.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Name of the rule to execute (e.g., 'stale-files')"
                    }
                },
                "required": ["name"]
            }),
        },
        Tool {
            name: "vfs_list_directory".into(),
            description: "List entries in a virtual directory from the backends mount table.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Virtual directory path to list"}
                },
                "required": ["path"]
            }),
        },
        Tool {
            name: "vfs_resolve_path".into(),
            description: "Resolve a virtual path to its real storage location.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Virtual path to resolve"}
                },
                "required": ["path"]
            }),
        },
        Tool {
            name: "vfs_backend_status".into(),
            description: "Get backend information for a file — which backend owns it, cache status, remote URL, and last sync state.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File path to query backend status for"}
                },
                "required": ["path"]
            }),
        },
        Tool {
            name: "vfs_sync_backend".into(),
            description: "Sync the backend for a file — returns count of synced files and any errors.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File path to sync the backend for"}
                },
                "required": ["path"]
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
        "vfs_set_metadata" => set_metadata(arguments),
        "vfs_graph_related" => graph_related(arguments),
        "vfs_graph_stats" => graph_stats(arguments),
        "vfs_graph_untested" => graph_untested(arguments),
        "vfs_graph_module" => graph_module(arguments),
        "vfs_graph_impact" => graph_impact(arguments),
        "vfs_rule_list" => rule_list(arguments),
        "vfs_rule_check" => rule_check(arguments),
        "vfs_list_directory" => list_directory(arguments),
        "vfs_resolve_path" => resolve_path_mcp(arguments),
        "vfs_backend_status" => backend_status(arguments),
        "vfs_sync_backend" => sync_backend(arguments),
        other => Err(McpError::Protocol(format!("Unknown tool: {other}"))),
    }
}

// ---------------------------------------------------------------------------
// Tool implementations
// ---------------------------------------------------------------------------

/// Default path to the DuckDB graph database (relative to CWD).
const GRAPH_DB_PATH: &str = ".vfs/graph/graph.db";

/// Default path to the manifest file (relative to CWD).
const MANIFEST_PATH: &str = "manifest.yaml";

/// Fallback manifest path used when the primary path doesn't exist.
const MANIFEST_FALLBACK_PATH: &str = ".vfs/manifest.yaml";

/// `vfs_get_metadata` — read file stats + Hilo xattrs.
///
/// Returns `{path, size, mtime, backend, hash, xattrs: {…}}` per spec §21.1.
/// Optional `keys` array filters xattrs to only the requested short names
/// (e.g. `["feature", "risk"]`).
///
/// `size` and `mtime` come from `std::fs::metadata`.  `backend` is read from
/// the `user.vfs.backend` xattr (default `"local"`).  `hash` is read from
/// `user.vfs.hash` (default `null`).
fn get_metadata(arguments: &serde_json::Value) -> McpResult<serde_json::Value> {
    use std::time::UNIX_EPOCH;

    let path_str = arguments["path"]
        .as_str()
        .ok_or_else(|| McpError::Protocol("missing 'path' argument".into()))?;
    let path = Path::new(path_str);

    // ── file stats from the OS ──────────────────────────────────────────
    let meta = std::fs::metadata(path)
        .map_err(|e| McpError::Protocol(format!("cannot stat '{path_str}': {e}")))?;
    let size = meta.len();
    let mtime: Option<String> = meta.modified().ok().and_then(|st| {
        let secs = st.duration_since(UNIX_EPOCH).ok()?.as_secs();
        Some(format_iso8601(secs))
    });

    // ── optional keys filter ────────────────────────────────────────────
    let keys_filter: Option<Vec<String>> =
        arguments.get("keys").and_then(|v| v.as_array()).map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        });

    // ── collect xattrs ──────────────────────────────────────────────────
    let full_names = hilo_metadata::list_vfs_xattrs(path)?;

    let mut xattrs = serde_json::Map::new();
    let mut backend: String = "local".into();
    let mut hash: Option<String> = None;

    for full_name in full_names {
        let short_name = full_name.strip_prefix("user.vfs.").unwrap_or(&full_name);

        // Honour the optional keys filter (match on short name).
        if let Some(ref keys) = keys_filter {
            if !keys.iter().any(|k| k == short_name) {
                continue;
            }
        }

        match hilo_metadata::get_vfs_xattr(path, short_name)? {
            Some(val) => {
                // Capture backend / hash for the top-level fields.
                if short_name == "backend" {
                    backend = val.clone();
                }
                if short_name == "hash" {
                    hash = Some(val.clone());
                }
                xattrs.insert(full_name, serde_json::Value::String(val));
            }
            None => {
                xattrs.insert(full_name, serde_json::Value::Null);
            }
        }
    }

    Ok(serde_json::json!({
        "path": path_str,
        "size": size,
        "mtime": mtime,
        "backend": backend,
        "hash": hash,
        "xattrs": xattrs,
    }))
}

/// Format a Unix timestamp (seconds since epoch) as ISO 8601.
///
/// Uses Hinnant's civil-from-days algorithm so no external date library is
/// required.  Output: `"2026-06-28T14:30:00Z"`.
fn format_iso8601(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let (year, month, day) = days_to_ymd(days);
    let remaining = secs % 86400;
    let hours = remaining / 3600;
    let minutes = (remaining % 3600) / 60;
    let secs_rem = remaining % 60;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hours, minutes, secs_rem
    )
}

/// Convert days since Unix epoch to (year, month, day).
///
/// Algorithm: civil_from_days (Howard Hinnant).  Works for the full
/// `chrono` date range — no 2038 problem.
fn days_to_ymd(days: i64) -> (i64, u32, u32) {
    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m as u32, d as u32)
}

/// `vfs_set_metadata` — set a Hilo xattr on a file.
///
/// Returns the previous value if the attribute was already set (useful
/// for agents that want to restore state), or `null` if this is a new
/// attribute.
fn set_metadata(arguments: &serde_json::Value) -> McpResult<serde_json::Value> {
    let path_str = arguments["path"]
        .as_str()
        .ok_or_else(|| McpError::Protocol("missing 'path' argument".into()))?;
    let key = arguments["key"]
        .as_str()
        .ok_or_else(|| McpError::Protocol("missing 'key' argument".into()))?;
    let value = arguments["value"]
        .as_str()
        .ok_or_else(|| McpError::Protocol("missing 'value' argument".into()))?;

    let path = Path::new(path_str);

    // Reject empty keys.
    if key.is_empty() {
        return Err(McpError::Protocol("'key' must not be empty".into()));
    }

    // Read the previous value before overwriting.
    let previous = hilo_metadata::get_vfs_xattr(path, key)?;

    // Set the new value.
    hilo_metadata::set_vfs_xattr(path, key, value)?;

    Ok(serde_json::json!({
        "success": true,
        "path": path_str,
        "key": format!("user.vfs.{}", key.trim_start_matches("user.vfs.")),
        "value": value,
        "previous_value": previous,
    }))
}

/// `vfs_graph_related` — find related files for a given path.
///
/// Supports both forward (outgoing) and reverse (incoming) queries, with
/// optional relation-type filtering (e.g. "imported_by", "tested_by").
fn graph_related(arguments: &serde_json::Value) -> McpResult<serde_json::Value> {
    let target = arguments["path"]
        .as_str()
        .ok_or_else(|| McpError::Protocol("missing 'path' argument".into()))?;

    let relation = arguments["relation"].as_str();
    let direction = arguments["direction"].as_str().unwrap_or("forward");

    if !Path::new(GRAPH_DB_PATH).exists() {
        return Ok(serde_json::Value::Array(vec![]));
    }

    let db = hilo_graph::GraphDB::open(GRAPH_DB_PATH)?;
    let dir = hilo_graph::Direction::parse(direction);

    // Try exact path first, then common prefixes
    let candidates = [
        target.to_string(),
        target.trim_start_matches("/home/").to_string(),
        target.trim_start_matches("./").to_string(),
        target.trim_start_matches('/').to_string(),
    ];

    let mut edges = Vec::new();
    for candidate in &candidates {
        if (candidate != target || edges.is_empty()) && db.file_in_graph(candidate)? {
            edges = db.related(candidate, relation, dir)?;
            if !edges.is_empty() {
                break;
            }
        }
    }

    // If still no edges found, try as a dependency query (package-level)
    if edges.is_empty() {
        edges = db.related(target, relation, dir)?;
    }

    let result: Vec<serde_json::Value> = edges
        .iter()
        .map(|e| {
            serde_json::json!({
                "from": e.from,
                "to": e.to,
                "relation": e.rel,
            })
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
            "total_files": 0,
            "most_connected": null,
            "orphans": [],
            "edge_types": {}
        }));
    }

    let db = hilo_graph::GraphDB::open(GRAPH_DB_PATH)?;
    let stats = db.stats()?;
    Ok(serde_json::to_value(stats)?)
}

/// `vfs_graph_untested` — list files that import others but have no tests.
///
/// Queries the DuckDB graph for source files that have `imports` edges
/// (they import other files) but no `tested_by` edge pointing at them
/// (no test file claims to cover them).
///
/// When no graph database exists, returns an empty list.
fn graph_untested(_arguments: &serde_json::Value) -> McpResult<serde_json::Value> {
    if !Path::new(GRAPH_DB_PATH).exists() {
        return Ok(serde_json::json!({
            "files": [],
            "total": 0
        }));
    }

    let db = hilo_graph::GraphDB::open(GRAPH_DB_PATH)?;
    let files = db.untested_files()?;
    Ok(serde_json::json!({
        "files": files,
        "total": files.len()
    }))
}

/// `vfs_graph_module` — per-module file listing and coverage statistics.
///
/// Queries the DuckDB graph for all distinct files whose path starts with
/// `module_name` (directory prefix).  Returns the file list, total edge
/// count touching the module, and test coverage percentage.
///
/// When no graph database exists, returns an empty result.
fn graph_module(arguments: &serde_json::Value) -> McpResult<serde_json::Value> {
    let module_name = arguments["module_name"]
        .as_str()
        .ok_or_else(|| McpError::Protocol("missing 'module_name' argument".into()))?;

    if module_name.is_empty() {
        return Err(McpError::Protocol("'module_name' must not be empty".into()));
    }

    if !Path::new(GRAPH_DB_PATH).exists() {
        return Ok(serde_json::json!({
            "module": module_name,
            "files": [],
            "edges_count": 0,
            "test_coverage_pct": 0.0,
        }));
    }

    let db = hilo_graph::GraphDB::open(GRAPH_DB_PATH)?;
    let stats = db.module_files(module_name)?;
    Ok(serde_json::to_value(stats)?)
}

/// `vfs_graph_impact` — transitive impact analysis for a file.
///
/// Uses BFS over the dependency graph to find all files that depend on
/// the given path, up to `max_depth` hops (default 5).
fn graph_impact(arguments: &serde_json::Value) -> McpResult<serde_json::Value> {
    let path_str = arguments["path"]
        .as_str()
        .ok_or_else(|| McpError::Protocol("missing 'path' argument".into()))?;

    let max_depth: u32 = arguments["max_depth"]
        .as_u64()
        .unwrap_or(5)
        .try_into()
        .unwrap_or(5);

    if !Path::new(GRAPH_DB_PATH).exists() {
        return Ok(serde_json::json!({"dependents": [], "total": 0, "max_depth_reached": false}));
    }

    let db = hilo_graph::GraphDB::open(GRAPH_DB_PATH)?;
    let results = hilo_graph::impact::compute_impact(db.conn(), path_str, max_depth)?;
    Ok(serde_json::json!({
        "dependents": results,
        "total": results.len(),
        "max_depth_reached": false
    }))
}

// ---------------------------------------------------------------------------
// Rule tools
// ---------------------------------------------------------------------------

/// Load the manifest from the primary or fallback path.
fn load_manifest() -> McpResult<hilo_core::manifest::Manifest> {
    let primary = Path::new(MANIFEST_PATH);
    let fallback = Path::new(MANIFEST_FALLBACK_PATH);

    let path = if primary.exists() {
        primary
    } else if fallback.exists() {
        fallback
    } else {
        return Err(McpError::Protocol(
            "No manifest found. Create a manifest.yaml or .vfs/manifest.yaml file.".into(),
        ));
    };

    let path_str = path.to_str().unwrap_or(MANIFEST_PATH);
    hilo_core::manifest::Manifest::from_file(path_str)
        .map_err(|e| McpError::Protocol(format!("Failed to load manifest: {e}")))
}

/// `vfs_rule_list` — return all rules defined in the manifest.
///
/// Each rule includes its name, description, and SQL query.
fn rule_list(_arguments: &serde_json::Value) -> McpResult<serde_json::Value> {
    let manifest = load_manifest()?;
    let rules: Vec<serde_json::Value> = manifest
        .rules
        .iter()
        .map(|r| {
            serde_json::json!({
                "name": r.name,
                "description": r.description,
                "query": r.query,
            })
        })
        .collect();
    Ok(serde_json::json!({ "rules": rules, "total": rules.len() }))
}

/// `vfs_rule_check` — execute a named rule query against the graph.
///
/// Returns matching rows.  If the rule's SQL is invalid the error is
/// returned as a structured JSON object (never a panic).
fn rule_check(arguments: &serde_json::Value) -> McpResult<serde_json::Value> {
    let rule_name = arguments["name"]
        .as_str()
        .ok_or_else(|| McpError::Protocol("missing 'name' argument".into()))?;

    let manifest = load_manifest()?;

    let query_rule = manifest
        .rules
        .iter()
        .find(|r| r.name == rule_name)
        .ok_or_else(|| {
            McpError::Protocol(format!(
                "Rule '{rule_name}' not found in manifest. Available: {}",
                manifest
                    .rules
                    .iter()
                    .map(|r| r.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?;

    // Build the engine-compatible rule.
    let rule = hilo_graph::Rule {
        name: query_rule.name.clone(),
        description: query_rule.description.clone(),
        query: query_rule.query.clone(),
    };

    // Open the graph database.  If it doesn't exist, return empty results
    // rather than an error — the graph just hasn't been populated yet.
    if !Path::new(GRAPH_DB_PATH).exists() {
        return Ok(serde_json::json!({
            "rule": rule.name,
            "description": rule.description,
            "matches": [],
            "total": 0,
        }));
    }

    let db = hilo_graph::GraphDB::open(GRAPH_DB_PATH)?;

    match hilo_graph::RuleEngine::check(db.conn(), &rule) {
        Ok(result) => Ok(serde_json::json!({
            "rule": result.rule,
            "description": result.description,
            "matches": result.matches,
            "total": result.total,
        })),
        Err(err) => {
            // Return the error as structured JSON — never panic.
            Ok(serde_json::json!({
                "rule": err.rule,
                "error": err.error,
            }))
        }
    }
}

fn list_directory(arguments: &serde_json::Value) -> McpResult<serde_json::Value> {
    let path = arguments["path"]
        .as_str()
        .ok_or_else(|| McpError::Protocol("missing path".into()))?;
    let manifest = load_manifest()?;
    let mut entries = hilo_core::virtual_dir::list_directory(&manifest, path);

    // Fallback: if no backends are configured, list the workspace directory
    if entries.is_empty() && path == "/" {
        if let Ok(cwd) = std::env::current_dir() {
            if let Ok(dir_entries) = std::fs::read_dir(&cwd) {
                for entry in dir_entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                    entries.push(hilo_core::virtual_dir::DirEntry {
                        name,
                        entry_type: if is_dir {
                            "directory".to_string()
                        } else {
                            "file".to_string()
                        },
                        backend: Some("local".to_string()),
                        size: entry.metadata().ok().map(|m| m.len()),
                        r#virtual: false,
                    });
                }
            }
        }
    }

    Ok(serde_json::json!({ "entries": entries, "total": entries.len() }))
}
fn resolve_path_mcp(arguments: &serde_json::Value) -> McpResult<serde_json::Value> {
    let path = arguments["path"]
        .as_str()
        .ok_or_else(|| McpError::Protocol("missing path".into()))?;
    let manifest = load_manifest()?;
    match hilo_core::virtual_dir::resolve_path(&manifest, path) {
        Some(r) => Ok(serde_json::to_value(r)?),
        None => Ok(serde_json::json!({"error":"not found","path":path})),
    }
}

// ---------------------------------------------------------------------------
// Backend tools
// ---------------------------------------------------------------------------

/// Helper: resolve a path through the manifest if available, otherwise
/// fall back to a simple local-filesystem check.
fn resolve_backend(path: &str) -> McpResult<hilo_core::virtual_dir::ResolvedPath> {
    // Try loading the manifest first.
    if let Ok(manifest) = load_manifest() {
        if let Some(r) = hilo_core::virtual_dir::resolve_path(&manifest, path) {
            return Ok(r);
        }
    }

    // Fallback: resolve against the current working directory.
    // If path is already absolute, use it directly.
    let p = Path::new(path);
    let resolved = if p.is_absolute() {
        p.to_path_buf()
    } else {
        let cwd = std::env::current_dir().map_err(McpError::Io)?;
        cwd.join(p)
    };
    let exists = resolved.exists();
    Ok(hilo_core::virtual_dir::ResolvedPath {
        real_path: resolved.to_string_lossy().to_string(),
        backend: "local".to_string(),
        cached: exists,
        sync_status: if exists {
            "synced".to_string()
        } else {
            "not found on disk".to_string()
        },
    })
}

/// `vfs_backend_status` — return backend-level details for a file.
///
/// Resolves the path to its real storage location and returns information
/// about which backend owns it, whether it's cached, the remote URL (if
/// applicable), and the last sync state.
fn backend_status(arguments: &serde_json::Value) -> McpResult<serde_json::Value> {
    let path = arguments["path"]
        .as_str()
        .ok_or_else(|| McpError::Protocol("missing 'path' argument".into()))?;

    let resolved = resolve_backend(path)?;

    // Build remote_url and cache_path based on backend type.
    let (remote_url, cache_path) = match resolved.backend.as_str() {
        "s3" => {
            // Try to get bucket/prefix from the manifest for a richer URL.
            let url = if let Ok(manifest) = load_manifest() {
                manifest
                    .backends
                    .s3
                    .iter()
                    .find(|s3| path.starts_with(&s3.at))
                    .map(|s3| format!("s3://{}/{}", s3.bucket, s3.prefix.as_deref().unwrap_or("")))
            } else {
                None
            };
            (
                url,
                if resolved.cached {
                    Some(resolved.real_path.clone())
                } else {
                    None
                },
            )
        }
        "git" => {
            let url = if let Ok(manifest) = load_manifest() {
                manifest
                    .backends
                    .remote
                    .iter()
                    .find(|remote| path.starts_with(&remote.at))
                    .map(|r| r.url.clone())
            } else {
                None
            };
            (
                url,
                if resolved.cached {
                    Some(resolved.real_path.clone())
                } else {
                    None
                },
            )
        }
        _ => {
            // Local backend — no remote URL, no cache path.
            (None, None)
        }
    };

    Ok(serde_json::json!({
        "backend": resolved.backend,
        "cache_hit": resolved.cached,
        "cache_path": cache_path,
        "remote_url": remote_url,
        "last_synced": resolved.sync_status,
    }))
}

/// `vfs_sync_backend` — sync the backend for a file.
///
/// For local backends, returns synced_files = 1 (always in sync).
/// For S3/git backends, reports the current cache state.
fn sync_backend(arguments: &serde_json::Value) -> McpResult<serde_json::Value> {
    let path = arguments["path"]
        .as_str()
        .ok_or_else(|| McpError::Protocol("missing 'path' argument".into()))?;

    let resolved = resolve_backend(path)?;

    let (synced_files, errors): (u32, Vec<String>) = match resolved.backend.as_str() {
        "local" => {
            if resolved.cached {
                (1, vec![])
            } else {
                (0, vec![format!("file not found: {}", path)])
            }
        }
        "s3" | "git" => {
            if resolved.cached {
                (1, vec![])
            } else {
                (
                    0,
                    vec![format!(
                        "{} backend not synced: file not cached locally",
                        resolved.backend
                    )],
                )
            }
        }
        other => (0, vec![format!("unknown backend type: {}", other)]),
    };

    Ok(serde_json::json!({
        "synced_files": synced_files,
        "errors": errors,
    }))
}
