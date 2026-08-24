---
name: hilo
description: "Agent-first virtual filesystem — pre-computes dependency graphs, metadata, and semantic context for AI coding agents. Written in Rust, 11 crates, 26-language AST parsing."
version: "0.2.0"
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
# Run all test suites (476+ tests across 11 crates)
cargo test --workspace

# Specific crate
cargo test -p hilo_graph

# Determinism tests (byte-identical output verification)
cargo test -p hilo_graph --test determinism_test
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
hilo-graph/         # AST parsing (26 langs), DuckDB graph, edges, impact, classify,
                    #   provenance, signal engine, semantic search, determinism tests
hilo-cli/           # CLI shim (init, meta, graph, classify, mount, serve, workspace)
hilo-mcp/           # MCP server (15 tools), JSON-RPC
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
| `vfs_graph_untested` | List files that have import edges but no test coverage (no tested_by edges) |
| `vfs_graph_module` | Get per-module file listing and test coverage statistics from the dependency graph |
| `vfs_list_directory` | List virtual directory contents |
| `vfs_resolve_path` | Resolve path through backends |
| `vfs_rule_check` | Run DuckDB rules against graph |
| `vfs_rule_list` | List all rules defined in the Hilo manifest (stale-files, untested-critical, transitive-impact, etc.) |
| `vfs_backend_status` | Get backend information for a file — which backend owns it, cache status, remote URL, and last sync state |
| `vfs_sync_backend` | Sync the backend for a file — returns count of synced files and any errors |

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
- **Task board:** `.coding-hermes/board/tasks.jsonl`
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

## Field Notes — Verified in Real Use (dogfood 2026-08-13)

From a deep real-use run on a fresh ripgrep clone (111 files): init 5ms,
warm 1.2s/256 edges, classify 0.18s, FUSE `--daemon` mount instant + clean
unmount, MCP 15 tools over stdio all responding with structured JSON. The
plumbing is real. The graph DATA has known gaps — read before querying:

**The `pkg:` form is the only reliable blast-radius query.**
Every edge in the graph targets `pkg:<name>` pseudo-nodes; there are ZERO
file→file edges (verified 256/256 on ripgrep). Consequently:

- ✅ `hilo graph impact 'pkg:globset' --max-depth 2` → works (found 3/3 real
  importers). Use `pkg:<crate>` for impact/related queries via CLI AND MCP
  (`vfs_graph_impact` `path: "pkg:..."`).
- ❌ `hilo graph impact <file>` → "No dependents found" ALWAYS (GAP-034, P0).
  `related <file> --direction reverse` → "No incoming edges" ALWAYS (GAP-034).
  Do not trust empty results from file-form queries; they are structurally
  empty, not "no dependents".

**Coverage queries are not meaningful yet.** `graph untested` / MCP
`vfs_graph_untested` list files lacking `tested_by` edges — but no code path
ever emits `tested_by` edges, so on any repo it reports ~everything (82/82 on
ripgrep, including test files). Additionally `classify` misses top-level
`tests/` and `benches/` dirs (5 of 19 real test files tagged on ripgrep).
(GAP-036, P1.)

**Parser artifact:** Rust `use crate::{a, b}` / `use foo::{x, y}` brace-groups
are truncated to `pkg:{\n    a` edge targets (27/256 edges on ripgrep). These
leak into `graph search` results and `graph stats` "Top dependencies".
Ignore `pkg:{` rows. (GAP-035, P1; GAP-038 for stats/search hygiene.)

**Symbol extraction is partial.** `graph understand <task>` (positional arg is
a natural-language TASK, not a file path) shows "(no symbols extracted)" for
many symbol-rich files (globset lib.rs → nothing, though it defines
Glob/GlobBuilder/GlobSetBuilder). `graph search` is lexical (TF-IDF/BM25) with
low scores and can return the right file with the wrong symbol label. (GAP-037.)

**Count semantics:** `graph stats` "Total edges" is the DuckDB-deduped count
(164); `edges.jsonl` line count (256) is raw with multi-provenance pairs —
not data loss.

**Gotchas that burned a real user:**
- `hilo serve` requires `--mcp` (clap-required since GAP-003) and `hilo init`
  must have run in the project (GAP-031).
- `hilo meta --set <attr> --value <val> <path>` — attr first, then `--value`,
  then path; there is no `--read` flag (GAP-004).
- Binary is `hilo`, not `hilo-cli` (GAP-001).
- `hilo mount <dir> --daemon` returns immediately; unmount with
  `fusermount -u <dir>` (GAP-019/027).

**Right-way patterns for agents using Hilo today:**
1. `hilo init` → `hilo graph warm` → `hilo classify` in the target repo (~2s).
2. Blast radius: query `pkg:<crate>` symbol form, never file paths.
3. Orientation: `graph stats` + `graph search "<symbol>"` + `graph module <dir>`.
4. Metadata: `meta --set` then `getfattr -n user.vfs.<attr>` (or MCP
   `vfs_get_metadata`).
5. Trust FUSE (`--daemon`), MCP protocol, xattr round-trips, and speed claims —
   they all verified clean.

## Field Notes — Dogfood 2026-08-23 (serde corpus, 208 files)

Second real-use run, fresh serde clone + release binary from master.
Verdict: 🟡 PROMISING-BUT-ROUGH (up from structural breakage; still not
shippable). GAP-034/035/036/043/044/045 fixes ALL verified live. New gaps
GAP-048..053 added to board. Read before querying:

**Blast radius is a LOWER BOUND, not truth (GAP-048, P0 — open).**
`impact serde/src/lib.rs` → 6 dependents; 148 files actually import serde.
The pkg-resolution layer matches only exact `pkg:serde` targets; the 53+
brace-expanded `pkg:serde::<member>` edges (from `use serde::{...}`) don't
resolve. Cross-check small impact counts against `impact 'pkg:<crate>'`
and `graph stats`. Do not trust a small number from a file-form impact
query on brace-heavy code.

**classify roles: crate roots + build scripts now sane (GAP-049, fixed 2026-08-24).**
lib.rs/mod.rs crate/module roots classify as `library` even when they are
macro/re-export walls with few `pub fn` (serde crate roots, 148 importers);
build.rs/build.zig classify as `build`, never `entrypoint`. Remaining caveat:
role accuracy on non-root files still varies — `test` detection is the most
reliable (151/208 on serde).

**`graph untested` is not a coverage tool (GAP-052, P2 — open).**
Zero `tested_by` edges are ever emitted; untested = all non-test files.

**MCP stdout is polluted (GAP-050, P2 — open).** `hilo serve --mcp` logs an
INFO tracing event to stdout at startup; naive clients misparse it as the
initialize response. Use a client that skips non-JSON lines.

**Build with `-p hilo-cli` (hyphen) (GAP-051, P2 — fixed 2026-08-24).**
SKILL.md line 25 now uses the real package name; hilo-cli is the only
hyphenated crate, `-p hilo_graph` for graph tests is correct.

**Still-verified-clean (from run 1, re-confirmed):** FUSE `--daemon` mount
instant + clean unmount; MCP 15 tools responding; meta/xattr round-trips;
graph clean → rewarm determinism (598/749 twice); git hooks incremental
warm; 6-language corpus parses; release impact query 0.84s.
