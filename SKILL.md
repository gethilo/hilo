---
name: hilo
description: "Agent-first virtual filesystem — pre-computes dependency graphs, metadata, and semantic context for AI coding agents. Written in Rust, 10 crates, 9-language AST parsing."
version: "0.1.0"
license: MIT
language: rust
repository: https://github.com/gethilo/hilo
coding-hermes: true
foreman: hilo-foreman
---

# Hilo

An agent-first virtual filesystem. Give your AI coding agent a pre-built map of every codebase it touches — dependencies, entrypoints, test coverage, blast radius — without burning context window on file reads.

## Quick Start

Clone and build:

```bash
# Fast check (0.5s)
cargo check --workspace

# Build the CLI binary (~20m first time due to duckdb-sys from source)
cargo build --release -p hilo-cli

# Install
cp target/release/hilo ~/.cargo/bin/hilo

# Initialize Hilo on a project
cd /path/to/your/project
hilo init
hilo graph warm
hilo classify
```

## Usage

```bash
# Query what imports a file
hilo graph related src/main.rs

# Reverse: what depends on this file (blast radius)
hilo graph impact src/auth/mod.rs

# Aggregate graph stats
hilo graph stats

# Semantic code search (TF-IDF + BM25, no embeddings)
hilo graph search "authentication middleware"

# Start the MCP server (JSON-RPC over stdio)
hilo serve --mcp
```

## Testing

```bash
# Run all test suites (476+ tests across 10 crates)
cargo test --workspace

# Specific crate
cargo test -p hilo-graph

# Determinism tests (byte-identical output verification)
cargo test -p hilo-graph --test determinism_test
```

## Linting & Formatting

```bash
# Format
cargo fmt --all

# Clippy (warnings as errors)
cargo clippy --workspace -- -D warnings
```

## Workspace Structure

```
hilo-core/          # Manifest, config, sandbox, workspace, virtual dirs
hilo-metadata/      # xattr read/write, inventory files (JSONL)
hilo-graph/         # AST parsing (9 langs), DuckDB graph, edges, impact, classify,
                    #   provenance, signal engine, semantic search, determinism tests
hilo-cli/           # CLI shim (init, meta, graph, classify, mount, serve, workspace)
hilo-mcp/           # MCP server (10 tools), JSON-RPC
hilo-backends/      # S3 (read/write-thru), Git (clone/pull), local
hilo-fuse/          # FUSE daemon, mount ops, xattr passthrough, workspace mount
hilo-triggers/      # inotify watchers, debounce, re-discover on change
hilo-plugins/       # WASM plugin runtime (Extism)
hilo-permissions/   # Manifest-driven access control
hilo-ffi/           # UniFFI bindings (Kotlin, Swift, Python)
```

## MCP Tools

| Tool | Description |
|------|-------------|
| `vfs_get_metadata` | Read xattrs for a file |
| `vfs_set_metadata` | Write xattrs for a file |
| `vfs_graph_related` | Query forward/reverse dependency edges |
| `vfs_graph_impact` | Transitive blast radius analysis |
| `vfs_graph_stats` | Aggregate graph statistics |
| `vfs_graph_understand` | Harmonic multi-resolution context (MAP → SIGNATURES → DETAIL) |
| `vfs_graph_search` | Semantic code search (TF-IDF + BM25 + RRF) |
| `vfs_list_directory` | List virtual directory contents |
| `vfs_resolve_path` | Resolve path through backends |
| `vfs_rule_check` | Run DuckDB rules against graph |

## Key Design Rules

1. **Metadata, not injection.** Never modify file content. Metadata lives in xattrs + JSONL inventory.
2. **xattr namespace:** `user.vfs.*` (e.g., `user.vfs.feature`, `user.vfs.role`)
3. **JSONL for edges.** `.vfs/graph/edges.jsonl` — append-only, git-friendly, streamable.
4. **DuckDB for queries.** Loaded from JSONL at mount/query time. Rebuildable. Not source of truth.
5. **Inventory as truth.** `.vfs/manifest.yaml`, `.vfs/graph/edges.jsonl`, `.vfs/backends/mounts.yaml`
6. **MCP as fallback.** When agent tools don't expose xattrs, MCP server provides `vfs_get_metadata`, `vfs_graph_related`, etc.

## Agent Context

This project is managed by the coding-hermes autonomous pipeline.

- **Foreman:** hilo-foreman (coding-hermes cron)
- **Quality gates:** GitReins Tier 1 (secrets, lint, build, test) + Tier 2 (LLM evaluation)
- **Agent skills:** coding-hermes, coding-hermes-cron, hilo-usage, gitreins
- **Task board:** `.coding-hermes/tasks.md`
- **Rinnegan upgrade batch (v0.2):** Provenance tracking, signal engine, semantic search, determinism tests — all complete

## Git Workflow

```bash
# Pre-commit: GitReins guards (secrets, clippy, tests)
gitreins commit -m "feat(graph): description"

# Push
git push origin master
```

**Repo:** https://github.com/gethilo/hilo
**Branch:** master
