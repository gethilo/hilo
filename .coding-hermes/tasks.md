# WarpFS — Phase 1 Coding Tasks

## [x] warpfs-core — Manifest parsing and config types
- **Priority:** P0 (blocking) ✅ COMPLETE 2026-06-20
- **Model:** glm-5.2
- **Crate:** warpfs-core
- **Files:** warpfs-core/src/lib.rs (2L), warpfs-core/src/manifest.rs (910L), warpfs-core/src/config.rs (17L)
- **Tests:** warpfs-core/tests/manifest_test.rs (519L) — 10/10 pass
- **AC:** ✅ Parse .vfs/manifest.yaml into typed Rust structs. All 13 sections (project, interfaces, repos, backends, metadata, graph, permissions, triggers, rules, plugins, discovery, sandbox, performance) parse without error. Unknown keys rejected. Default values applied for optional fields. Custom deserializers for string_or_int (permission modes) and string_or_vec (trigger on field). Rust keyword handling via serde rename (ref→git_ref, async→r#async).

## [x] warpfs-metadata — xattr read/write and inventory file I/O
- **Priority:** P0 (blocking) ✅ COMPLETE 2026-06-20
- **Model:** glm-5.2
- **Crate:** warpfs-metadata
- **Files:** warpfs-metadata/src/lib.rs, warpfs-metadata/src/xattr.rs, warpfs-metadata/src/inventory.rs
- **Tests:** warpfs-metadata/tests/
- **AC:** Read/write user.vfs.* xattrs on files. Create .vfs/ directory structure. Append to edges.jsonl. Read/write backends/mounts.yaml. Functions tested with tempdir.

## [ ] warpfs-graph — tree-sitter parsing and DuckDB graph
- **Priority:** P0
- **Model:** glm-5.2
- **Crate:** warpfs-graph
- **Files:** warpfs-graph/src/lib.rs, warpfs-graph/src/parser.rs, warpfs-graph/src/graph.rs, warpfs-graph/src/duckdb.rs
- **Tests:** warpfs-graph/tests/
- **AC:** Parse Go files with tree-sitter, extract imports. Generate edges.jsonl entries (from, to, rel). Initialize DuckDB graph.db. Query edges via DuckDB (COUNT, GROUP BY, DISTINCT). Works on a real Go repo test fixture.

## [ ] warpfs-cli — CLI shim (init, meta, graph, serve)
- **Priority:** P1
- **Model:** glm-5.2
- **Crate:** warpfs-cli
- **Files:** warpfs-cli/src/main.rs, warpfs-cli/src/commands/
- **Tests:** tests/cli/
- **AC:** `warpfs init` creates .vfs/ + default manifest.yaml. `warpfs meta <path>` reads xattrs. `warpfs graph discover` runs parser → edges.jsonl → DuckDB. `warpfs graph stats` prints hotspot/orphan counts. All commands return non-zero on error.

## [ ] warpfs-mcp — MCP server (stdio transport, 3 tools)
- **Priority:** P1
- **Model:** glm-5.2
- **Crate:** warpfs-mcp
- **Files:** warpfs-mcp/src/lib.rs, warpfs-mcp/src/server.rs, warpfs-mcp/src/tools/
- **Tests:** tests/mcp/
- **AC:** `warpfs serve --mcp` starts stdio MCP server. Tools: vfs_get_metadata, vfs_graph_related, vfs_graph_stats. Each tool returns valid JSON-RPC. Server cleanly exits on stdin close. Tested with mcp-cli or equivalent.
