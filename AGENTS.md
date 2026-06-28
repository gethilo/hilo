# AGENTS.md — Hilo

Agent-first metadata filesystem. Written in Rust, 10 crates, 9-language AST parsing.

## Build & Test

```bash
cargo check --workspace          # Fast (0.5s)
cargo build --workspace           # Slow (~20m, duckdb-sys from source)
cargo test --workspace            # 31 suites
cargo fmt --all                   # Apply formatting
cargo clippy --workspace -- -D warnings
```

## Workspace Structure

```
hilo-core/          # Manifest, config, sandbox, workspace, virtual dirs
hilo-metadata/      # xattr read/write, inventory files (JSONL)
hilo-graph/         # AST parsing (9 langs), DuckDB graph, edges, impact, classify
hilo-cli/           # CLI shim (init, meta, graph, classify, mount, serve, workspace)
hilo-mcp/           # MCP server (8 tools), JSON-RPC
hilo-backends/      # S3 (read/write-thru), Git (clone/pull), local
hilo-fuse/          # FUSE daemon, mount ops, xattr passthrough, workspace mount
hilo-triggers/      # inotify watchers, debounce, re-discover on change
hilo-plugins/       # WASM plugin runtime (Extism)
hilo-permissions/   # Manifest-driven access control
```

## Git Conventions

- **Pre-commit:** GitReins guards (secrets, tests, static_analysis, lsp)
- **Commit:** `gitreins commit -m "message"` — guards run before commit
- **Push:** `git push origin master`
- **Repo:** `github.com/gethilo/hilo`

## Key Design Rules

1. **Metadata, not injection.** Never modify file content. Metadata lives in xattrs + JSONL inventory.
2. **xattr namespace:** `user.vfs.*` (e.g., `user.vfs.feature`, `user.vfs.role`)
3. **JSONL for edges.** `.vfs/graph/edges.jsonl` — append-only, git-friendly, streamable.
4. **DuckDB for queries.** Loaded from JSONL at mount/query time. Rebuildable. Not source of truth.
5. **Inventory as truth.** `.vfs/manifest.yaml`, `.vfs/graph/edges.jsonl`, `.vfs/backends/mounts.yaml`
6. **MCP as fallback.** When agent tools don't expose xattrs, MCP server provides `vfs_get_metadata`, `vfs_graph_related`, etc.

## Adding a Feature

1. Identify which crate the feature belongs to
2. Write code, add tests
3. `cargo check --workspace` — must pass
4. `cargo test --workspace` — must pass
5. `cargo fmt --all` — apply
6. `cargo clippy --workspace -- -D warnings` — must pass
7. `gitreins commit -m "description"` — guards run
8. Push
