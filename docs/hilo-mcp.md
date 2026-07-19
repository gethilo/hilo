# hilo-mcp — MCP Server

A Model Context Protocol server implementing 15 tools over stdio JSON-RPC. Agents query the dependency graph, search semantically, read/write metadata, and manage backends through MCP without file reads.

**Crate:** `hilo-mcp`  
**Transport:** stdio (JSON-RPC 2.0)  
**Tools:** 15

## Public API Surface

### Types

| Type | Description |
|------|-------------|
| `server::McpServer` | Main MCP server — initialize, handle requests, run loop |
| `tools::ToolRegistry` | Registration of all 15 MCP tools |
| `rate_limiter::RateLimiter` | Token-bucket rate limiter — configurable RPS cap |
| `error::McpError` | JSON-RPC error types with standard error codes |

### MCP Tools

| Tool | Input | Output | Description |
|------|-------|--------|-------------|
| `vfs_get_metadata` | `{path, key?, backend?, hash?}` | metadata objects | Read xattrs for a file |
| `vfs_set_metadata` | `{path, key, value, dry_run?}` | confirmation | Write xattrs to a file |
| `vfs_list_directory` | `{path}` | directory listing | List virtual directory contents |
| `vfs_resolve_path` | `{path}` | resolved path info | Resolve path through backend layer |
| `vfs_graph_related` | `{path, direction?}` | edges with provenance | Forward/reverse dependency edges |
| `vfs_graph_impact` | `{path}` | impact chain | Transitive blast-radius analysis |
| `vfs_graph_stats` | `{}` | aggregate stats | Total edges, files, orphans, top deps |
| `vfs_graph_module` | `{module}` | module edges | All edges for a given module |
| `vfs_graph_untested` | `{}` | untested files | Files with no test coverage edges |
| `vfs_graph_understand` | `{task, budget?, resolution?}` | compressed context | Harmonic multi-resolution codebase context |
| `vfs_graph_search` | `{query, limit?}` | search results | Deterministic semantic search (TF-IDF + BM25) |
| `vfs_rule_list` | `{}` | rule definitions | List available DuckDB rules |
| `vfs_rule_check` | `{rule_id?}` | rule results | Run rules against the graph |
| `vfs_backend_status` | `{backend_id?}` | backend statuses | Check backend connectivity/health |
| `vfs_sync_backend` | `{backend_id}` | sync result | Trigger backend sync operation |

### Rate Limiting

The server supports token-bucket rate limiting configured via `manifest.yaml`:

```yaml
performance:
  rate_limit_rps: 100  # requests per second, 0 = unlimited
```

When a request is rate-limited, the server returns JSON-RPC error `-32000` with `retry_after_seconds` in the data field.

## Usage Example

```bash
# Start the MCP server (stdio transport)
hilo serve --mcp

# With rate limiting
hilo serve --mcp --rate-limit 100
```

```rust
use hilo_mcp::server::McpServer;
use hilo_mcp::rate_limiter::RateLimiter;

let rate_limiter = RateLimiter::new(100.0); // 100 req/s
let server = McpServer::new(
    graph_db_path,
    manifest_path,
    Some(rate_limiter),
);
server.run().await?;
```
