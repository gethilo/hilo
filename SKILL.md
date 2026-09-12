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
hilo-mcp/           # MCP server (17 tools), JSON-RPC
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

**Blast radius on crate roots resolves the pkg family (GAP-048, P0 — fixed 2026-08-24).**
`impact serde/src/lib.rs` → 147 dependents (was 6); `impact 'pkg:serde'` → 148.
Family matching covers brace-expanded `pkg:serde::<member>` members (from
`use serde::{...}`) AND underscore companions (`serde_derive`/`serde_test`),
with an exact-boundary guard so `pkg:a_%` never leaks `pkg:ab` siblings.
Cross-check large counts against `impact 'pkg:<crate>'` and `graph stats`.

**classify roles: crate roots + build scripts now sane (GAP-049, fixed 2026-08-24).**
lib.rs/mod.rs crate/module roots classify as `library` even when they are
macro/re-export walls with few `pub fn` (serde crate roots, 148 importers);
build.rs/build.zig classify as `build`, never `entrypoint`. Remaining caveat:
role accuracy on non-root files still varies — `test` detection is the most
reliable (151/208 on serde).

**`graph untested` now reads real `tested_by` edges (GAP-052, P2 — fixed 2026-08-24).**
Test files' imports emit `tested_by` edges (visible in `graph stats` edge
types); `untested` = files with import edges but no incoming `tested_by` edge.

**MCP stdout is pure JSON-RPC (GAP-050, P2 — fixed 2026-08-24).** `hilo serve
--mcp` sends tracing to stderr; stdout carries only JSON-RPC 2.0 responses
(initialize → tools/list → tools/call), safe for naive clients.

**Build with `-p hilo-cli` (hyphen) (GAP-051, P2 — fixed 2026-08-24).**
SKILL.md line 25 now uses the real package name; hilo-cli is the only
hyphenated crate, `-p hilo_graph` for graph tests is correct.

**Still-verified-clean (from run 1, re-confirmed):** FUSE `--daemon` mount
instant + clean unmount; MCP 15 tools responding; meta/xattr round-trips;
graph clean → rewarm determinism (598/749 twice); git hooks incremental
warm; 6-language corpus parses; release impact query 0.84s.

## Field Notes — Dogfood 2026-09-11 (containerd corpus, 5489 Go files)

Third real-use run, master d734456 (one commit after PERF-005 landed).
**Verdict: ✅ SHIPPABLE (with one language-specific caveat).** All run-1/2
P0/P1 graph-data breaks are closed; every fix re-verified live:

- **PERF-005 verified in real use.** On containerd (4122 vendor Go files),
  warm parsed exactly the 1367 non-vendor files, 0/14070 edges reference
  `vendor/`. `hilo init`/`warm` in `$HOME` refuses with a clear
  `--allow-home` hint.
- **Go blast radius via pkg: FULL import path is EXACT.** grep ground truth
  for `core/mount` = 126 importing files; `hilo graph impact
  'pkg:github.com/containerd/containerd/v2/core/mount' --max-depth 1` → 126.
- **Go file→package resolution is the one remaining gap (GAP-057, P1).**
  File-form `impact`/`related --direction reverse` on Go files → empty
  (CLI) / `{dependents:[],total:0}` (MCP). Rust file-form works (17
  dependents on hilo-graph/src/lib.rs). Until fixed: for Go repos, resolve
  the file's directory to `pkg:<module path>/<dir>` and query that.
- **Not-found consistency (GAP-059):** `related` says "No incoming edges"
  (exit 0) for paths not in the graph; `impact` errors properly. Treat
  related's empty as unknown until the row lands.
- **Warm exclusion is silent (GAP-058):** on a guarded repo, warm prints
  "1367/1367 files" with no mention of the 4122 excluded vendor files.
  Don't misread it as data loss — check `stats` + edges.jsonl.
- **Re-verified clean:** tested_by/untested real (3409 tested_by edges on
  containerd); MCP stdout pure JSON-RPC (0 non-JSON lines, 17 tools);
  FUSE xattr passthrough through the mount; meta round-trips; incremental
  `warm --changed` (1 file → 1s); stats 26ms / impact 108ms at
  containerd scale.
- **Install-from-scratch (bunker las-bunker-03):** clone → rustup →
  `cargo build --release` RC=0 in 18m52s → quickstart smoke green. README
  15-20 min claim accurate; clang/cmake turn out to be unnecessary despite
  the requirement line (GAP-060 docs drift, benign).

## Field Notes — Dogfood 2026-09-12 (fastapi corpus, 1138 Python files)

Fourth real-use run, master b780fb8 (0.3.0). **Verdict: ✅ SHIPPABLE for
Rust/Go, 🟡 for Python** — one family of gaps, all resolution-layer:

- **Python file-form impact/related silently empty (GAP-064, P1).**
  `impact fastapi/routing.py` → "No dependents found" (rc 0); grep truth
  = 10 importers; `impact 'pkg:fastapi.routing'` → exact (10/10 in-scope
  + 7 docs_src importers, all ast_exact conf=1.0). The GAP-057 fix
  (9431cac) covered Go only. Workaround until fixed: file → drop `.py`,
  slashes → dots → query `pkg:<module>`.
- **Warm exclusion is silent on Python too (GAP-065, P2).** 936 of 1138
  `.py` parsed; 203 missing = 183 `__init__.py` (4/187 indexed —
  undocumented policy) + 20 real parse failures, zero warnings.
- **Coverage collected but not consumed (GAP-066, P2).** 2225 tested_by
  edges; `tests/test_sse.py → tested_by → pkg:fastapi.routing` exists,
  yet `untested` lists `fastapi/routing.py` and `graph module fastapi`
  says `Tests: 0.0%`. untested/module don't resolve file→pkg.
- **`meta --set attr=value <path>` silently writes a garbage xattr whose
  KEY contains `=` (GAP-067, P2).** Docs form is
  `--set <attr> --value <val> <path>`; nothing rejects the typo.
- **MCP transport is NDJSON** (one JSON-RPC message per line) — LSP-style
  Content-Length framing → `-32700 Parse error`. Client gotcha, not a
  bug. 17 tools, stdout pure (GAP-050 still fixed), GAP-062 re-confirmed
  over MCP (`related` ghost path → `[]`, isError=false).
- **Re-verified clean on Python:** pkg-form impact exact; stats 28 ms /
  impact 20 ms / search 23 ms / untested 19 ms; byte-deterministic
  stats; loud unknown-path errors on impact AND related (GAP-059 fix
  live, rc=1); PERF-004 instant `.md` guard; SIGPIPE quiet (GAP-063
  appears fixed — `| head -1` → rc 0, no panic); FUSE mount/xattr/cat/
  clean unmount; classify sane roles; meta round-trips with correct
  syntax.
