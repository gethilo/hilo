# hilo-mcp — MCP Server

A Model Context Protocol server that exposes 17 tools over stdio JSON-RPC 2.0.
Agents query the dependency graph, search semantically, read/write metadata, and
manage backends and workspace files through MCP without file reads.

**Crate:** `hilo-mcp`
**Transport:** stdio — newline-delimited JSON-RPC 2.0 on stdin/stdout
**Tools:** 17 — `hilo_mcp::tools::list_tools()`, the exact descriptor list `tools/list` returns
**Protocol version** (reported by `initialize`): `2024-11-05`

## Public API Surface

The crate is a library plus a stdio server loop — there is no `McpServer`
builder type and no `ToolRegistry`: `tools/list` is served directly from
`tools::list_tools()`.

| Item | Description |
|------|-------------|
| `server::run(rate_limit_rps: u32) -> McpResult<()>` | Run the stdio server loop. Blocks until stdin reaches EOF. `0` disables rate limiting. |
| `server::handle_request(line: &str) -> McpResult<Option<serde_json::Value>>` | Route one JSON-RPC request line and return the response value; `None` for a notification (a request without `id`). |
| `tools::list_tools() -> Vec<Tool>` | All tool descriptors (`name`, `description`, `inputSchema`) returned by `tools/list`. |
| `tools::call_tool(name: &str, arguments: &serde_json::Value) -> McpResult<serde_json::Value>` | Execute one tool by name; an unknown name is a protocol error. |
| `rate_limiter::RateLimiter` | Token bucket. `RateLimiter::new(rate_rps: u32)` (an integer, not a float), `check() -> bool`, `retry_after_secs() -> f64`. |
| `error::McpError`, `error::McpResult<T>` | Error type; handler errors surface as JSON-RPC `-32603` with the error's `Display` text as `message`. |

## MCP Tools

`tools/list` returns exactly 17 descriptors. The table below mirrors
`hilo-mcp/src/tools/mod.rs` (and `hilo-mcp/tests/mcp_test.rs` keeps it that
way).

| Tool | Input | Output | Description |
|------|-------|--------|-------------|
| `vfs_get_metadata` | `{path, keys?}` | `{path, size, mtime, backend, hash, xattrs}` | Read `user.vfs.*` xattrs plus file stats for a file |
| `vfs_set_metadata` | `{path, key, value}` | `{success, path, key, value, previous_value}` | Set one `user.vfs.*` xattr, returning the previous value |
| `vfs_graph_related` | `{path, relation?, direction?}` | array of `{from, to, relation, scope, provenance, confidence}` | Forward (outgoing) or reverse (incoming) dependency edges |
| `vfs_graph_stats` | `{}` | `{total_edges, total_files, edge_types, orphans, most_connected}` | Aggregate statistics about the dependency graph |
| `vfs_graph_untested` | `{}` | `{files, total}` | Files with import edges but no `tested_by` edges |
| `vfs_graph_module` | `{module_name}` | `{module, files, edges_count, test_coverage_pct}` | Per-module file listing and test coverage |
| `vfs_graph_impact` | `{path, max_depth?}` | `{dependents, total, max_depth_reached}` | Files that depend on this file, directly or transitively |
| `vfs_graph_understand` | `{task, budget?, resolution?}` | `{text, files, anchors, tokens_estimate}` | Harmonic multi-resolution context for a task (MAP → SIGNATURES → DETAIL) |
| `vfs_graph_search` | `{query, limit?}` | `{results, total}` (or a `message` when `.vfs/graph/graph.db` is missing) | Deterministic semantic search (TF-IDF + BM25 + reciprocal rank fusion) |
| `vfs_rule_list` | `{}` | `{rules, total}` | Rules defined in the manifest |
| `vfs_rule_check` | `{name}` | `{rule, description, matches, total}` (or `{rule, error}` when the rule's query fails) | Execute a named rule query against the graph |
| `vfs_list_directory` | `{path}` | `{entries, total}` | Entries in a virtual directory (backends mount table), with the real local filesystem as fallback when the virtual listing is empty; errors on nonexistent paths and file paths |
| `vfs_resolve_path` | `{path}` | `{real_path, backend, cached, sync_status}` (or `{error, path}` when unresolved) | Resolve a virtual path to its real storage location |
| `vfs_backend_status` | `{path}` | `{backend, cache_path, cache_hit, remote_url, last_synced}` | Backend/cache/sync state for a file |
| `vfs_sync_backend` | `{path}` | `{synced_files, errors, skipped_ignored}` | Sync the backend for a file; ignored/ephemeral paths report `skipped_ignored` |
| `vfs_workspace_ephemeral` | `{path?}` | `{entries, total_bytes}` | List ephemeral (rebuildable/redownloadable) files in the workspace |
| `vfs_workspace_wipe` | `{path?, dry_run?}` | `{removed, freed_bytes}` | Plan (`dry_run` defaults to true) or apply a wipe of ephemeral files |

`Input` lists **every** property of the tool's `tools/list` `inputSchema`; a
trailing `?` marks an argument that is *not* in the schema's `required` array,
i.e. optional. Arguments without `?` are required, and a `tools/call` that
omits one is answered with JSON-RPC `-32603` naming the missing argument.
`vfs_workspace_wipe` never removes a file carrying `user.vfs.ephemeral` of
`false` (or `0`).

## Rate Limiting

Rate limiting is **manifest-configured**, never a command-line option:
`hilo serve` reads `performance.rate_limit_rps` from the project manifest
(`.vfs/manifest.yaml`, falling back to a root-level `manifest.yaml`) and
passes it to `hilo_mcp::server::run`. The only flag `hilo serve` accepts is
the required `--mcp`.

```yaml
# .vfs/manifest.yaml
performance:
  rate_limit_rps: 100  # requests per second, 0 = unlimited (default)
```

The limiter is a token bucket whose capacity and refill rate are both the
configured rate. When a request arrives with an empty bucket, the server
answers with a JSON-RPC error frame — code `-32000`, message
`Rate limit exceeded. Retry after N seconds.`, and
`data.retry_after_seconds` — carrying `"id": null`, because the limiter runs
before the request is parsed.

## Usage

```bash
# Inside a Hilo project (hilo init creates .vfs/manifest.yaml).
hilo init
hilo serve --mcp
```

`hilo serve --mcp` speaks JSON-RPC 2.0 on stdin/stdout and is the supported
integration path. `--mcp` is required (the only implemented server mode), and
the server refuses to start outside an initialized project, naming
`hilo init`. Diagnostics go to stderr; stdout carries JSON-RPC frames only.

Set the rate limiting in the manifest (above): the server takes no
command-line override for it.
