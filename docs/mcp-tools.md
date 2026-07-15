# MCP Tools

Hilo exposes 15 tools via JSON-RPC over stdio. Agents query the
dependency graph and metadata without reading files.

## Tool List

### `vfs_get_metadata`

Read all `user.vfs.*` extended attributes for a file.

```json
{
  "name": "vfs_get_metadata",
  "arguments": { "path": "src/main.rs" }
}
```

Returns: `{ "user.vfs.role": "entrypoint", "user.vfs.status": "stable" }`

### `vfs_graph_related`

Find files related to a path via the dependency graph.

```json
{
  "name": "vfs_graph_related",
  "arguments": {
    "path": "src/main.rs",
    "relation": "imports",
    "direction": "forward"
  }
}
```

- `relation` (optional): `"imports"`, `"tested_by"`, `"tests"`, or omit for all
- `direction` (optional): `"forward"` (outgoing) or `"reverse"` (incoming)

### `vfs_graph_stats`

Aggregate statistics about the graph.

```json
{ "name": "vfs_graph_stats", "arguments": {} }
```

Returns: `{ "total_edges": 2252, "unique_files": 716, "unique_dependencies": 531, "top_dependencies": [["sys:gtest/gtest.h", 349], ...] }`

### `vfs_graph_impact`

Transitive impact analysis — what depends on this file?

```json
{
  "name": "vfs_graph_impact",
  "arguments": {
    "path": "sys:metacall/metacall.h",
    "max_depth": 3
  }
}
```

Returns: `{ "dependents": [{"path": "...", "depth": 1}, ...], "total": 175 }`

### `vfs_rule_list`

List all rules defined in the manifest.

```json
{ "name": "vfs_rule_list", "arguments": {} }
```

### `vfs_rule_check`

Execute a named rule against the graph.

```json
{
  "name": "vfs_rule_check",
  "arguments": { "name": "stale-files" }
}
```

### `vfs_list_directory`

List entries in a virtual directory from backends.

```json
{
  "name": "vfs_list_directory",
  "arguments": { "path": "/" }
}
```

### `vfs_resolve_path`

Resolve a virtual path to its real storage location.

```json
{
  "name": "vfs_resolve_path",
  "arguments": { "path": "src/main.rs" }
}
```

Returns: `{ "real_path": "/home/user/project/src/main.rs", "backend": "local", "cached": false }`

### `vfs_set_metadata`

Set a Hilo extended attribute (`user.vfs.*`) on a file. Returns the previous value for the attribute if one existed.

```json
{
  "name": "vfs_set_metadata",
  "arguments": {
    "path": "src/auth.rs",
    "key": "feature",
    "value": "authentication"
  }
}
```

- `path` (required): path to the file
- `key` (required): attribute name — do NOT include the `user.vfs.` prefix; it is added automatically (e.g., `"feature"` becomes `"user.vfs.feature"`)
- `value` (required): attribute value to set

Returns: `{ "success": true, "path": "src/auth.rs", "key": "user.vfs.feature", "value": "authentication", "previous_value": null }`

### `vfs_graph_untested`

List files that have import edges but no test coverage — i.e., no `tested_by` edge points at them. Useful for finding untested source files.

```json
{ "name": "vfs_graph_untested", "arguments": {} }
```

Takes no arguments.

Returns: `{ "files": ["src/parser.rs", "src/signal.rs"], "total": 2 }`

### `vfs_graph_module`

Get per-module (directory-prefixed) file listing and test coverage statistics from the dependency graph.

```json
{
  "name": "vfs_graph_module",
  "arguments": { "module_name": "src/auth/" }
}
```

- `module_name` (required): directory prefix to query (e.g., `"src/auth/"`, `"pkg/"`)

Returns: `{ "module": "src/auth/", "files": ["src/auth/login.rs", "src/auth/middleware.rs"], "edges_count": 24, "test_coverage_pct": 67.0 }`

### `vfs_backend_status`

Get backend information for a file — which backend owns it (local, s3, git), whether it's cached locally, its remote URL (for s3/git backends), and last sync state.

```json
{
  "name": "vfs_backend_status",
  "arguments": { "path": "src/main.rs" }
}
```

- `path` (required): file path to query backend status for

Returns: `{ "backend": "local", "cache_hit": true, "cache_path": "/home/user/project/src/main.rs", "remote_url": null, "last_synced": null }`

### `vfs_sync_backend`

Sync the backend for a file. For local backends this is a no-op (always in sync). For S3/git backends, reports the current cache state.

```json
{
  "name": "vfs_sync_backend",
  "arguments": { "path": "docs/index.html" }
}
```

- `path` (required): file path to sync the backend for

Returns: `{ "synced_files": 1, "errors": [] }`

### `vfs_graph_understand`

Harmonic multi-resolution context compression — budgeted, tiered output from the dependency graph.

```json
{
  "name": "vfs_graph_understand",
  "arguments": {
    "task": "rate limiter",
    "budget": 6000,
    "resolution": "harmonic"
  }
}
```

- `task` (required): natural language description of what the agent is working on
- `budget` (optional): max token budget (default 6000)
- `resolution` (optional): `"harmonic"` (MAP → SIGNATURES → DETAIL) or `"flat"`

Returns: `{ "text": "...", "files": SignalFile[], "tokens_estimate": 4120, "anchors": ["src/middleware.rs"] }`

### `vfs_graph_search`

Deterministic semantic code search using TF-IDF + Okapi BM25 + Reciprocal Rank Fusion. No embeddings, no external API calls.

```json
{
  "name": "vfs_graph_search",
  "arguments": {
    "query": "authentication middleware",
    "limit": 10
  }
}
```

- `query` (required): natural language search query
- `limit` (optional): max results (default 10)

Returns: `{ "results": [{ "file_path": "src/auth.rs", "symbols": ["AuthMiddleware"], "score": 0.89, "provenance": "lexical" }], "total": 5 }`

## Integration

Add to your MCP client config:

```json
{
  "mcpServers": {
    "hilo": {
      "command": "hilo-cli",
      "args": ["serve", "--mcp"],
      "cwd": "/path/to/your/project"
    }
  }
}
```

### Supported clients

- **Hermes Agent** — native MCP client, auto-discovers tools
- **Claude Desktop** — add to `claude_desktop_config.json`
- **Continue** — add to `~/.continue/config.json`
- **Cline** — add to MCP servers list
