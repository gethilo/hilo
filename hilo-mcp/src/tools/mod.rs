//! Tool definitions and dispatch for the Hilo MCP server.
//!
//! Seventeen tools are exposed:
//! - `vfs_get_metadata`     — read Hilo xattrs for a file
//! - `vfs_set_metadata`     — write Hilo xattr for a file
//! - `vfs_graph_related`    — find related files via the dependency graph
//! - `vfs_graph_stats`      — summary statistics about the graph
//! - `vfs_graph_untested`   — list files with imports but no test coverage
//! - `vfs_graph_module`     — per-module file listing and coverage statistics
//! - `vfs_graph_impact`     — transitive impact analysis for a file
//! - `vfs_graph_understand` — harmonic multi-resolution context (signal engine)
//! - `vfs_graph_search`     — semantic code search (TF-IDF + BM25, deterministic)
//! - `vfs_rule_list`        — list all rules defined in the manifest
//! - `vfs_rule_check`       — execute a named rule query against the graph
//! - `vfs_list_directory`   — list entries in a virtual directory
//! - `vfs_resolve_path`     — resolve a virtual path to real storage
//! - `vfs_backend_status`   — get backend information for a file
//! - `vfs_sync_backend`     — sync the backend for a file
//! - `vfs_workspace_ephemeral` — list ephemeral (rebuildable) files
//! - `vfs_workspace_wipe`   — plan or apply a wipe of ephemeral files

use std::path::Path;

use serde::Serialize;

use crate::error::{McpError, McpResult};
use hilo_backends::{EphemeralClass, EphemeralMatcher, IgnoreMatcher};

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
                    },
                    "keys": {
                        "type": "array",
                        "items": {
                            "type": "string"
                        },
                        "description": "Optional xattr short-name filter (e.g. ['feature', 'risk'])"
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
            name: "vfs_graph_understand".into(),
            description: "Get harmonic multi-resolution context for a task — MAP (file→symbols), SIGNATURES (one-line per symbol), DETAIL (whitespace-minified source). Position-ordered, budget-controlled, deterministic.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "task": {"type": "string", "description": "Natural-language task description (e.g. 'rate limiter middleware')"},
                    "budget": {"type": "integer", "description": "Token budget (default: 6000)"},
                    "resolution": {"type": "string", "description": "Output mode: 'harmonic' (3-tier, default) or 'flat' (single-tier)"}
                },
                "required": ["task"]
            }),
        },
        Tool {
            name: "vfs_graph_search".into(),
            description: "Semantic code search using TF-IDF + BM25 + Reciprocal Rank Fusion. Finds files by meaning, not just literal text matches. Deterministic, no external API calls.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Search query (e.g. 'authentication middleware', 'rate limiter')"},
                    "limit": {"type": "integer", "description": "Max results to return (default: 20)"}
                },
                "required": ["query"]
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
            description: "Sync the backend for a file — returns count of synced files and any errors. Ignored/ephemeral paths are local-only and report skipped_ignored instead of transferring.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File path to sync the backend for"}
                },
                "required": ["path"]
            }),
        },
        Tool {
            name: "vfs_workspace_ephemeral".into(),
            description: "List ephemeral (rebuildable/redownloadable) files in the workspace — path, size, and the deciding rule. Defaults to the workspace root; an optional path limits the listing to that subtree.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Optional path limiting the listing to a subtree"}
                }
            }),
        },
        Tool {
            name: "vfs_workspace_wipe".into(),
            description: "Plan or apply a wipe of ephemeral files. dry_run defaults to true (planned only); pass dry_run=false to delete. Files with user.vfs.ephemeral=false are never removed.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Optional path limiting the wipe to a subtree"},
                    "dry_run": {"type": "boolean", "description": "When true (default) only plan; when false, delete"}
                }
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
        "vfs_graph_understand" => graph_understand(arguments),
        "vfs_graph_search" => graph_search(arguments),
        "vfs_rule_list" => rule_list(arguments),
        "vfs_rule_check" => rule_check(arguments),
        "vfs_list_directory" => list_directory(arguments),
        "vfs_resolve_path" => resolve_path_mcp(arguments),
        "vfs_backend_status" => backend_status(arguments),
        "vfs_sync_backend" => sync_backend(arguments),
        "vfs_workspace_ephemeral" => workspace_ephemeral(arguments),
        "vfs_workspace_wipe" => workspace_wipe(arguments),
        other => Err(McpError::Protocol(format!("Unknown tool: {other}"))),
    }
}

// ---------------------------------------------------------------------------
// Tool implementations
// ---------------------------------------------------------------------------

/// Default path to the DuckDB graph database (relative to CWD).
const GRAPH_DB_PATH: &str = ".vfs/graph/graph.db";

/// Whether any on-disk graph data exists for the current workspace.
///
/// True when the DuckDB cache exists OR a sibling `edges.jsonl` does. The
/// graph tools use this to decide between "genuinely no graph yet" (all-zero
/// / empty answers) and "cache absent but data available" — in the latter
/// case [`hilo_graph::GraphDB::open`] reconciles `edges.jsonl` into a fresh
/// cache, so answering zeros would silently report an empty graph
/// (GAP-096).
fn graph_data_present() -> bool {
    const EDGES_JSONL_PATH: &str = ".vfs/graph/edges.jsonl";
    Path::new(GRAPH_DB_PATH).exists() || Path::new(EDGES_JSONL_PATH).exists()
}

/// Resolution root for `pkg:`-node coverage lookups (GAP-066).
///
/// Graph edge paths and [`GRAPH_DB_PATH`] are both relative to the process
/// working directory (the repo root when the MCP server is launched there),
/// so coverage queries must join the file paths onto that same directory
/// before resolving them to package nodes.
fn graph_root() -> std::path::PathBuf {
    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
}

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
///
/// JIT: on first access the file is parsed on-the-fly and cached — no
/// `hilo graph warm` pre-requisite needed.
///
/// GAP-062: a path that is neither cached nor on disk fails loudly with the
/// same error contract CLI `graph related` / `vfs_graph_impact` use — a silent
/// empty list is indistinguishable from a real node with no edges.
fn graph_related(arguments: &serde_json::Value) -> McpResult<serde_json::Value> {
    let target = arguments["path"]
        .as_str()
        .ok_or_else(|| McpError::Protocol("missing 'path' argument".into()))?;

    let relation = arguments["relation"].as_str();
    let direction = arguments["direction"].as_str().unwrap_or("forward");

    // Ensure the .vfs/graph directory exists (create on first use).
    let db_parent = Path::new(GRAPH_DB_PATH).parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(db_parent).ok();

    let db = hilo_graph::GraphDB::open(GRAPH_DB_PATH)?;
    let dir = hilo_graph::Direction::parse(direction);

    // Try exact path first, then common prefixes. A candidate that IS in the
    // graph is authoritative: its edge list is the answer, even when that list
    // is empty (a real node with no matching edges is a valid empty result).
    let candidates = [
        target.to_string(),
        target.trim_start_matches("/home/").to_string(),
        target.trim_start_matches("./").to_string(),
        target.trim_start_matches('/').to_string(),
    ];

    let mut resolved = false;
    let mut edges = Vec::new();
    for candidate in &candidates {
        if db.file_in_graph(candidate)? {
            resolved = true;
            edges = db.related(candidate, relation, dir)?;
            if !edges.is_empty() {
                break;
            }
        }
    }

    // No candidate is cached: fall through to the shared JIT contract
    // (`GraphDB::related_or_parse`, the same call CLI `graph related` and
    // `vfs_graph_impact` make). It parses an on-disk file on the fly and, for a
    // path that is neither cached nor on disk, returns the loud
    // "is not in the graph" GraphError instead of an empty list.
    if !resolved {
        edges = db.related_or_parse(target, relation, dir)?;
    }

    let result: Vec<serde_json::Value> = edges
        .iter()
        .map(|e| {
            // GAP-083: name the granularity of this row's `to` endpoint. A row
            // matched through a `pkg:<crate>` node (GAP-034) is crate-level;
            // a row whose `to` is a file is file-level. Without it an MCP
            // client cannot tell "0 files import this" from "N files import
            // this file's crate" — the CLI prints the same distinction.
            serde_json::json!({
                "from": e.from,
                "to": e.to,
                "relation": e.rel,
                "scope": if e.to.starts_with("pkg:") || e.to.starts_with("sys:") {
                    "crate"
                } else {
                    "file"
                },
                "provenance": e.provenance,
                "confidence": e.confidence,
            })
        })
        .collect();
    Ok(serde_json::Value::Array(result))
}

/// `vfs_graph_stats` — aggregate statistics about the dependency graph.
///
/// When no graph data exists yet (neither the DuckDB cache nor a sibling
/// `edges.jsonl` — common in a fresh project) we return an all-zeros stats
/// object instead of an error. When the cache is absent but `edges.jsonl`
/// exists, `GraphDB::open` reconciles it first, so the answer reflects the
/// real edge set (GAP-096: this used to answer zeros and silently disagree
/// with the CLI on the same corpus).
fn graph_stats(_arguments: &serde_json::Value) -> McpResult<serde_json::Value> {
    if !graph_data_present() {
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
    if !graph_data_present() {
        return Ok(serde_json::json!({
            "files": [],
            "total": 0
        }));
    }

    let db = hilo_graph::GraphDB::open(GRAPH_DB_PATH)?;
    // GAP-066: resolve covered files through their `pkg:` nodes, relative to
    // the same root the graph paths are stored against.
    let files = db.untested_files_at(&graph_root())?;
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

    if !graph_data_present() {
        return Ok(serde_json::json!({
            "module": module_name,
            "files": [],
            "edges_count": 0,
            "test_coverage_pct": 0.0,
        }));
    }

    let db = hilo_graph::GraphDB::open(GRAPH_DB_PATH)?;
    let stats = db.module_files_at(&graph_root(), module_name)?;
    Ok(serde_json::to_value(stats)?)
}

/// `vfs_graph_impact` — transitive impact analysis for a file.
///
/// Uses BFS over the dependency graph to find all files that depend on
/// the given path, up to `max_depth` hops (default 5).
///
/// JIT: on first access the start file is parsed on-the-fly and cached —
/// no `hilo graph warm` pre-requisite needed.
fn graph_impact(arguments: &serde_json::Value) -> McpResult<serde_json::Value> {
    let path_str = arguments["path"]
        .as_str()
        .ok_or_else(|| McpError::Protocol("missing 'path' argument".into()))?;

    let max_depth: u32 = arguments["max_depth"]
        .as_u64()
        .unwrap_or(5)
        .try_into()
        .unwrap_or(5);

    // Ensure the .vfs/graph directory exists (create on first use).
    let db_parent = Path::new(GRAPH_DB_PATH).parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(db_parent).ok();

    let db = hilo_graph::GraphDB::open(GRAPH_DB_PATH)?;

    // JIT: parse start file on cache miss, then BFS over cache.
    let results = db.impact_or_parse(path_str, max_depth)?;

    Ok(serde_json::json!({
        "dependents": results,
        "total": results.len(),
        "max_depth_reached": false
    }))
}

/// `vfs_graph_understand` — harmonic multi-resolution context for a task.
///
/// Produces budgeted, tiered output (MAP → SIGNATURES → DETAIL) from the
/// dependency graph. The agent gets the shape of the code first, exact lines
/// last — position-ordered to beat "lost in the middle" attention loss.
///
/// If the graph database doesn't exist, returns an empty result with a
/// helpful message.
fn graph_understand(arguments: &serde_json::Value) -> McpResult<serde_json::Value> {
    let task = arguments["task"]
        .as_str()
        .ok_or_else(|| McpError::Protocol("missing 'task' argument".into()))?;

    let token_budget = arguments["budget"].as_u64().unwrap_or(6000) as usize;
    let resolution_str = arguments["resolution"].as_str().unwrap_or("harmonic");
    let resolution = match resolution_str.to_lowercase().as_str() {
        "flat" => hilo_graph::Resolution::Flat,
        _ => hilo_graph::Resolution::Harmonic,
    };

    // Ensure the .vfs/graph directory exists (create on first use).
    let db_parent = Path::new(GRAPH_DB_PATH).parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(db_parent).ok();

    if !graph_data_present() {
        return Ok(serde_json::json!({
            "text": format!("No graph database found at {GRAPH_DB_PATH}. Run `hilo graph warm` first."),
            "files": [],
            "tokens_estimate": 0,
            "anchors": []
        }));
    }

    let db = hilo_graph::GraphDB::open(GRAPH_DB_PATH)?;
    let opts = hilo_graph::SignalOpts {
        token_budget,
        resolution,
        ..Default::default()
    };

    let result = hilo_graph::understand(&db, task, &opts)?;

    Ok(serde_json::json!({
        "text": result.text,
        "files": result.files,
        "tokens_estimate": result.tokens_estimate,
        "anchors": result.anchors
    }))
}

/// `vfs_graph_search` — semantic code search using TF-IDF + BM25 + RRF.
///
/// Finds files by meaning (camelCase/snake_case tokenization + classical
/// NLP ranking) rather than literal substring matching. Fully
/// deterministic: same query + same graph → byte-identical results.
fn graph_search(arguments: &serde_json::Value) -> McpResult<serde_json::Value> {
    let query = arguments["query"]
        .as_str()
        .ok_or_else(|| McpError::Protocol("missing 'query' argument".into()))?;

    let limit = arguments["limit"].as_u64().unwrap_or(20) as usize;

    // Ensure the .vfs/graph directory exists.
    let db_parent = Path::new(GRAPH_DB_PATH).parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(db_parent).ok();

    if !graph_data_present() {
        return Ok(serde_json::json!({
            "results": [],
            "total": 0,
            "message": format!("No graph database found at {GRAPH_DB_PATH}. Run `hilo graph warm` first.")
        }));
    }

    let db = hilo_graph::GraphDB::open(GRAPH_DB_PATH)?;
    // GAP-077: MCP search indexes file-defined symbols so exact symbol
    // names surface their defining file for agents.
    let opts = hilo_graph::SearchOpts {
        limit,
        index_symbols: true,
        root: None,
    };
    let results = hilo_graph::search(&db, query, &opts)?;

    Ok(serde_json::json!({
        "results": results,
        "total": results.len()
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
            "No manifest found. Run `hilo init` in the project directory first.".into(),
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
    if !graph_data_present() {
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
    let resolved = resolve_backend(path)?;
    Ok(serde_json::json!({
        "real_path": resolved.real_path,
        "backend": resolved.backend,
        "cached": resolved.cached,
        "sync_status": resolved.sync_status,
        "remote_url": resolved.remote_url,
    }))
}

// ---------------------------------------------------------------------------
// Backend tools
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct BackendResolution {
    real_path: String,
    backend: String,
    cached: bool,
    sync_status: String,
    remote_url: Option<String>,
    cache_path: Option<String>,
    managed: bool,
}

fn mount_remote_url(mount: &hilo_backends::MountEntry) -> Option<String> {
    if mount.kind == "s3" {
        mount.bucket.as_ref().map(|bucket| {
            let prefix = mount
                .prefix
                .as_deref()
                .unwrap_or("")
                .trim_start_matches('/');
            format!("s3://{bucket}/{prefix}")
        })
    } else {
        mount.remote.clone()
    }
}

/// Resolve ownership from the canonical `.vfs/backends/mounts.yaml` store.
/// No network client is constructed for this read-only status operation.
fn resolve_backend(path: &str) -> McpResult<BackendResolution> {
    let cwd = std::env::current_dir().map_err(McpError::Io)?;
    let mounts_path = cwd.join(".vfs/backends/mounts.yaml");
    let mount = if mounts_path.is_file() {
        let entries = hilo_backends::read_mount_entries(&mounts_path)
            .map_err(|error| McpError::Protocol(error.to_string()))?;
        hilo_backends::mount_for_path(&entries, path).cloned()
    } else {
        None
    };

    let requested = Path::new(path);
    let real_path = if mount.is_some() {
        cwd.join(path.trim_start_matches('/'))
    } else if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        cwd.join(requested)
    };
    let cached = real_path.exists();

    if let Some(mount) = mount {
        let remote_url = mount_remote_url(&mount);
        Ok(BackendResolution {
            real_path: real_path.to_string_lossy().into_owned(),
            backend: mount.kind,
            cached,
            sync_status: "unknown (no sync timestamp recorded)".into(),
            cache_path: cached.then(|| real_path.to_string_lossy().into_owned()),
            remote_url,
            managed: true,
        })
    } else {
        Ok(BackendResolution {
            real_path: real_path.to_string_lossy().into_owned(),
            backend: "local-only".into(),
            cached,
            sync_status: if cached {
                "not applicable (unmanaged local path)".into()
            } else {
                "not found on disk (unmanaged path)".into()
            },
            cache_path: None,
            remote_url: None,
            managed: false,
        })
    }
}

/// `vfs_backend_status` — return backend-level details for a file.
fn backend_status(arguments: &serde_json::Value) -> McpResult<serde_json::Value> {
    let path = arguments["path"]
        .as_str()
        .ok_or_else(|| McpError::Protocol("missing 'path' argument".into()))?;
    let resolved = resolve_backend(path)?;

    Ok(serde_json::json!({
        "backend": resolved.backend,
        "cache_hit": resolved.cached,
        "cache_path": resolved.cache_path,
        "remote_url": resolved.remote_url,
        "last_synced": resolved.sync_status,
        "managed": resolved.managed,
    }))
}

/// `vfs_sync_backend` — reject unimplemented syncs rather than fabricate success.
///
/// Ignore-aware paths still report an intentional skip. Every other path is
/// directed to the real CLI sync engine until MCP owns an executable sync
/// implementation.
fn sync_backend(arguments: &serde_json::Value) -> McpResult<serde_json::Value> {
    let path = arguments["path"]
        .as_str()
        .ok_or_else(|| McpError::Protocol("missing 'path' argument".into()))?;

    if let Some(skipped) = skipped_ignore_count(path)? {
        return Ok(serde_json::json!({
            "synced_files": 0,
            "errors": [],
            "skipped_ignored": skipped,
        }));
    }

    let resolved = resolve_backend(path)?;
    let target = resolved
        .remote_url
        .as_deref()
        .unwrap_or("unmanaged local storage");
    Err(McpError::Protocol(format!(
        "backend sync is not supported by this MCP surface for '{}' (backend {}, origin {}); use 'hilo backend sync'",
        path, resolved.backend, target
    )))
}

/// Returns `Some(n)` when `path` is excluded from sync (ignored by the
/// workspace ignore rules, or ephemeral without `user.vfs.sync = upstream`),
/// and `None` when it is a normal sync candidate or the check cannot be
/// applied (path outside the workspace, no matcher loadable).
fn skipped_ignore_count(path: &str) -> McpResult<Option<u32>> {
    let cwd = std::env::current_dir().map_err(McpError::Io)?;
    let p = Path::new(path);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    };
    // Only paths inside the workspace have an ignore context.
    let rel = match abs.strip_prefix(&cwd) {
        Ok(r) => r.to_path_buf(),
        Err(_) => return Ok(None),
    };
    let rel_str = rel.to_string_lossy().replace('\\', "/");

    if let Ok(matcher) = IgnoreMatcher::load(&cwd, None, false) {
        if matcher.is_ignored(&rel_str) {
            return Ok(Some(1));
        }
    }
    if let Ok(matcher) = EphemeralMatcher::load(&cwd, None) {
        let xattr_ephemeral = hilo_metadata::xattr::get_vfs_xattr(&abs, "ephemeral")
            .map_err(|e| McpError::Protocol(format!("failed to read user.vfs.ephemeral: {e}")))?
            .map(|v| v == "true" || v == "1");
        if matcher.classify(&rel, abs.is_dir(), xattr_ephemeral) == EphemeralClass::Ephemeral {
            // Explicit `user.vfs.sync = upstream` re-includes an ephemeral
            // file (spec §13.16); everything else is skipped.
            let sync_upstream = hilo_metadata::xattr::get_vfs_xattr(&abs, "sync")
                .map_err(|e| McpError::Protocol(format!("failed to read user.vfs.sync: {e}")))?
                .map(|v| v == "upstream")
                .unwrap_or(false);
            if !sync_upstream {
                return Ok(Some(1));
            }
        }
    }
    Ok(None)
}

/// `vfs_workspace_ephemeral` — list ephemeral (rebuildable/redownloadable)
/// files in the workspace.
///
/// Mirrors `hilo workspace ephemeral`: classification uses the built-in
/// ephemeral catalog (spec §5.1) plus the workspace `.hiloephemeral` file
/// when present. Defaults to the whole workspace root; the optional `path`
/// argument limits the listing to that subtree.
fn workspace_ephemeral(arguments: &serde_json::Value) -> McpResult<serde_json::Value> {
    let root = std::env::current_dir().map_err(McpError::Io)?;
    let matcher = EphemeralMatcher::load(&root, None)
        .map_err(|e| McpError::Protocol(format!("failed to load ephemeral catalog: {e}")))?;
    let entries = matcher
        .scan(&root)
        .map_err(|e| McpError::Protocol(format!("failed to scan workspace: {e}")))?;

    let filter = arguments["path"].as_str();
    let mut out = Vec::new();
    let mut total_bytes: u64 = 0;
    for e in entries {
        if let Some(f) = filter {
            if !e.path.starts_with(f) {
                continue;
            }
        }
        total_bytes += e.size;
        out.push(serde_json::json!({
            "path": e.path,
            "size": e.size,
            "reason": e.reason,
        }));
    }

    Ok(serde_json::json!({ "entries": out, "total_bytes": total_bytes }))
}

/// `vfs_workspace_wipe` — plan or apply a wipe of ephemeral files.
///
/// Mirrors `hilo workspace wipe --ephemeral`: the default is a dry-run plan;
/// with `dry_run: false` the ephemeral files are deleted and the freed bytes
/// reported. `user.vfs.ephemeral = false` is the only wipe protector (spec
/// §5.2.2); the optional `path` argument limits the wipe to a subtree.
fn workspace_wipe(arguments: &serde_json::Value) -> McpResult<serde_json::Value> {
    let root = std::env::current_dir().map_err(McpError::Io)?;
    let matcher = EphemeralMatcher::load(&root, None)
        .map_err(|e| McpError::Protocol(format!("failed to load ephemeral catalog: {e}")))?;
    let entries = matcher
        .scan(&root)
        .map_err(|e| McpError::Protocol(format!("failed to scan workspace: {e}")))?;

    let filter = arguments["path"].as_str();
    let dry_run = arguments["dry_run"].as_bool().unwrap_or(true);

    let mut removed = Vec::new();
    let mut freed_bytes: u64 = 0;
    for e in entries {
        if let Some(f) = filter {
            if !e.path.starts_with(f) {
                continue;
            }
        }
        let full = root.join(&e.path);
        // user.vfs.ephemeral = false is the ONLY wipe protector.
        if let Some(v) = hilo_metadata::xattr::get_vfs_xattr(&full, "ephemeral").map_err(|err| {
            McpError::Protocol(format!("failed to read user.vfs.ephemeral: {err}"))
        })? {
            if v == "false" || v == "0" {
                continue;
            }
        }
        if !dry_run {
            std::fs::remove_file(&full).map_err(|err| {
                McpError::Protocol(format!("failed to remove {}: {err}", full.display()))
            })?;
        }
        freed_bytes += e.size;
        removed.push(serde_json::json!({
            "path": e.path,
            "bytes": e.size,
        }));
    }

    Ok(serde_json::json!({ "removed": removed, "freed_bytes": freed_bytes }))
}
