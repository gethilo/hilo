# AGENTS.md — Hilo

Agent-first metadata filesystem. Written in Rust, 11 crates (incl. hilo-ffi UniFFI bindings), 26-language AST parsing.

## Build & Test — WHERE TO BUILD (policy)

**This box is a shared 16-core agent host. Heavy Rust builds belong on the CI/build box, not here.**

- **Default: build/test on bunker-las-03.** It is the designated CI/build host (`ssh bunker3`, repo at `~/warpfs`, kept in sync with origin). Run all of: full `cargo build --workspace`, full `cargo test --workspace`, release builds, and any build expected to take >2 minutes there.
- **Build locally ONLY if the task needs the local binary or local target/ artifacts**: running the compiled `hilo` against local corpora/repo state, FUSE mounts, or profiling on this machine. Even then prefer scoped builds (`cargo check -p hilo_graph`, `cargo test -p hilo_graph --lib`) over workspace-wide ones.
- Keep the two checkouts in sync: `git push` your branch, then `ssh bunker3 'cd ~/warpfs && git fetch && git checkout <branch>'`. Never rsync the working tree — pull from origin so provenance stays clean.
- First build on las-03 is slow (duckdb-sys from source, ~20m); incremental builds after that are fast. Leave the target/ cache in place — do not `cargo clean` there.

```bash
# ON bunker-las-03 (default for heavy jobs):
ssh bunker3 'cd ~/warpfs && git fetch origin && git checkout <branch> && ~/.cargo/bin/cargo build --workspace'
# LOCALLY (only when you need the local binary/artifacts):
cargo check --workspace          # Fast (0.5s)
cargo build -p hilo-cli --release  # scoped local build when required
cargo test -p hilo_graph --lib   # scoped local tests
```


## Workspace Structure

```
hilo-core/          # Manifest, config, sandbox, workspace, virtual dirs
hilo-metadata/      # xattr read/write, inventory files (JSONL)
hilo-graph/         # AST parsing (26 langs), DuckDB graph, edges, impact, classify
hilo-cli/           # CLI shim (init, meta, graph, classify, mount, serve, workspace)
hilo-mcp/           # MCP server (17 tools), JSON-RPC
hilo-backends/      # S3 (read/write-thru), Git (clone/pull), local
hilo-fuse/          # FUSE daemon, mount ops, xattr passthrough, workspace mount
hilo-triggers/      # inotify watchers, debounce, re-discover on change
hilo-plugins/       # WASM plugin runtime (Extism)
hilo-permissions/   # Manifest-driven access control
hilo-ffi/           # UniFFI bindings (Go, Python, Kotlin, Swift)
```

## Git Conventions

- **Pre-commit:** GitReins guards — **secrets + tests** (`guards.test_command` = `scripts/rust-lint.sh`: `cargo clippy --workspace --all-targets -- -D warnings`, a fail-closed rust-analyzer proof, then `cargo test -p hilo_graph --lib`). The native `static_analysis` and `lsp` legs are **disabled**: both map a missing, timed-out or broken tool to an empty finding list and still print `clean` / PASS (INT-GITREINS-002/003), so a rustup shim with no component installed read as a green tick. Enforcement lives in the script instead, which exits non-zero (4 = tool unusable, 5 = hung).
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
