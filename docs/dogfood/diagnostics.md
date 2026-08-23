# Hilo Diagnostics — How It Works, Where It Breaks

Written 2026-08-13 after a deep real-use run on a fresh ripgrep clone.
This explains how Hilo is built, why the graph behaves the way it does, the
errors encountered (mine and the project's own history), and the right way to
use it today.

## How Hilo is built (from the outside in)

- **11 crates** (`hilo-core` manifest/sandbox, `hilo-metadata` xattr+JSONL,
  `hilo-graph` AST parsing + DuckDB, `hilo-cli`, `hilo-mcp`, `hilo-backends`,
  `hilo-fuse`, `hilo-triggers` inotify watchers, `hilo-plugins` WASM,
  `hilo-permissions`, `hilo-ffi` UniFFI bindings).
- **Metadata, not injection**: file contents are never modified. Everything lives
  in xattrs (`user.vfs.*`) + JSONL inventory.
- **The graph pipeline**: `hilo graph warm` parses every source file with the
  tree-sitter-based 26-language parsers (rayon-parallel — 111 files in 1.2s) and
  appends edges to `.vfs/graph/edges.jsonl` (append-only, git-friendly). On query,
  DuckDB (`.vfs/graph/graph.db`) serves the graph; the JSONL is the source of
  truth and the DB is rebuilt/reconciled from it (write-through on trigger
  updates, read-through reconciliation — commits `e2b2af4`, `4196997`).
- **Queries are JIT**: `related`/`impact` auto-parse files on first access; `warm`
  is an optional batch warmup. `hilo graph clean` deletes edges.jsonl + the DuckDB
  cache for a from-scratch rebuild (added 2026-08-12, GAP-026).
- **Entry points**: CLI (`hilo`), MCP server (`hilo serve --mcp`, 15 `vfs_*`
  tools over stdio JSON-RPC), FUSE mount (`hilo mount <dir> [--daemon]`).

## The critical design property (and the bug it hides)

**Every edge in the graph points at `pkg:<name>` pseudo-nodes — never at files.**
Verified empirically: on ripgrep, 256/256 edges have targets like `pkg:std`,
`pkg:globset`, `pkg:regex_automata`; **zero** edges target a file path, even for
intra-crate `use crate::foo` imports. There is no resolution step that maps a
`pkg:<name>` node to the file that defines that crate/module.

Why this matters: `hilo graph impact <file>` and `related <file> --direction
reverse` traverse *file* nodes. With no file→file edges and no pkg:→file
resolution, those queries are **structurally guaranteed to return empty on every
repo**. The README's headline example (`hilo graph impact 'sys:gtest/gtest.h'`)
uses the symbol form, which works; the Quickstart's `hilo graph impact <file>`
form cannot. The MCP tools inherit the same limitation
(`vfs_graph_impact` → `{"dependents":[]}`).

The `pkg:` symbol form works (verified: `impact 'pkg:globset'` found the 3/3
files that import globset on disk), but the `pkg:` namespace is undiscoverable —
nothing in the docs tells a user to query `pkg:globset` instead of the file path.

## Other structural quirks found in real use

1. **Truncated brace-groups (parser bug).** Rust `use crate::{a, b}` /
   `use foo::{x, y}` produces an edge target of literally `pkg:{\n    a` — the
   parser takes the raw text up to the first newline instead of expanding the
   brace group. 27 of 256 ripgrep edges were this artifact; they leak into
   `graph search` results and `graph stats` "Top dependencies".

2. **Classification is path-pattern-based and shallow.** `classify` matched
   `crates/*/tests/*.rs` but NOT ripgrep's top-level `tests/` dir (12 files) or
   `benches/` — 5 of 19 real test/bench files tagged. Then `graph untested`
   (which lists files not covered by test files) reports **every** file,
   including files already classified as test. The "test coverage" promise
   therefore returns 100%-untested on a repo with a substantial test suite.

3. **Symbol extraction is incomplete.** `graph understand` (multi-resolution
   harmonic context: it ranks files by symbols matching a natural-language task)
   showed "(no symbols extracted)" for files that plainly define public items
   (`crates/globset/src/lib.rs` defines `Glob`/`GlobBuilder`/`GlobSetBuilder`).
   `graph search` is lexical-substring over file paths + extracted symbols, with
   low scores — searching `GlobSetBuilder` returned the right file but labeled
   with symbol `glob`.

4. **Count semantics.** `graph stats` "Total edges: 164" vs 256 lines in
   edges.jsonl: DuckDB dedupes multi-provenance pairs (same from/to, different
   provenance rows). Known to the maintainers (board note, tick 100) but
   unexplained to users; it looks like data loss.

## The right way to use Hilo TODAY (until GAP-034..038 land)

- **For blast-radius questions**: query the symbol form — `hilo graph impact
  'pkg:<crate>' --max-depth N`, or MCP `vfs_graph_impact` with
  `path: "pkg:<crate>"`. Do NOT pass a file path and trust an empty result.
- **Warm once after init** (`hilo graph warm`) for batch parsing; queries are JIT
  so they work without it, but warm gives consistent `stats`.
- **Rebuild corrupted graphs** with `hilo graph clean` + `warm`.
- **Metadata**: `hilo meta --set <attr> --value <val> <path>` (note the
  argument order — attr, then --value, then path; `--read` does not exist).
- **MCP**: `hilo serve --mcp` requires `hilo init` to have run in the project
  (error message now says so, GAP-031); 15 tools, stdio transport, JSON-RPC 2.0.
- **Mount**: `hilo mount <dir> --daemon` returns immediately and persists;
  unmount with `fusermount -u <dir>`.
- **Do not trust** `graph untested` or `graph impact <file>` output until
  GAP-034/036 land.

## Project history that explains the current state

- Renamed warpfs → Hilo (repo github.com/gethilo/hilo); a stale
  `edges.jsonl` from the rename era caused phantom `warpfs-*` files until
  `graph clean` was added (GAP-018, commit d2902e2).
- 30+ docs-drift gaps (GAP-001..033) were found and fixed by stand-in PM
  hunter sweeps (2026-08-04..13) — the docs now largely match the CLI. The
  remaining gaps are *data quality*, not docs: exactly the layer green tests
  don't see (530+ tests pass; the graph pipeline tests use small fixtures that
  don't exercise file-level resolution or top-level tests/ dirs).
- `hilo mount --daemon` (GAP-027, commit 82cdfbe) and git/local backend wiring
  (GAP-025, commit 05248f0) are recent and worked in this run.

## Errors encountered in this run (and fixes that worked)

| Symptom | Root cause | Fix |
|---|---|---|
| `impact <file>` empty | no file→file edges / pkg: resolution | use `pkg:` form (GAP-034) |
| `related <file> --direction reverse` empty | same | use `pkg:` form (GAP-034) |
| `understand <file>` printed whole-repo MAP | `<TASK>` is a natural-language task, not a path | pass a task string |
| untested = 82/82 | classification + untested logic (GAP-036) | none yet |
| `pkg:{` in search/stats | brace-group truncation (GAP-035) | none yet |

---

# 2026-08-23 Update — Second Real-Use Run (serde corpus, 208 files)

The 2026-08-13 diagnostics said file-level queries were *structurally*
empty because every edge targets `pkg:*` pseudo-nodes. Since then, the
foreman landed a **query-time pkg-resolution layer** (d1472bb, GAP-034):
file-level `impact`/`related` queries now map the file to its `pkg:<name>`
node and traverse. Verified working on serde: `impact serde/src/lib.rs`
returns 6 real dependents (was always empty). GAP-035 (brace-group
expansion), GAP-043 (intra-crate file→file edges — 177 on serde), GAP-044
(understand symbols), GAP-045 (pkg labeling) all verified live.

## Why the under-count happens (the new structural limit)

The resolution layer matches **exact** `pkg:serde` edge targets only.
But GAP-035's brace-group expansion emits **per-member** edges —
`use serde::{Serialize, Deserialize}` → edges to `pkg:serde::Serialize`,
`pkg:serde::Deserialize`, not `pkg:serde`. On serde: 7 exact edges vs 53+
member edges; 148 unique files import serde in some form; impact returns 6.
So the two fixes (GAP-034 resolution, GAP-035 expansion) interact badly:
the expansion multiplied the edge forms the resolution layer doesn't match.
The right fix direction is prefix matching (`pkg:serde` matches
`pkg:serde::*`) in the resolution layer, or resolving member edges back to
the defining file. Tracked as GAP-048 (P0).

## How classify actually behaves (learned the hard way)

`hilo classify` role heuristics on real code: tests detected well
(151/208 on serde, incl. nested test_suite/), but crate-root lib.rs files
with hundreds of importers get role `unknown`; only 8 files got `library`
(internals/*, private/*); the only `entrypoint`s were 4 build.rs scripts.
So role xattrs are only trustworthy for tests today (GAP-049).

## The tested_by hole

Nothing in the pipeline ever emits `tested_by` edges (0/749 on serde,
0/256 on ripgrep in run 1). `graph untested` therefore lists all non-test
files — including crate roots that the test_suite imports everywhere. It
is a "not a test file" filter, not a coverage report (GAP-052).

## MCP stdout hygiene

`hilo serve --mcp` writes a tracing INFO event to stdout at startup.
MCP stdio framing requires stdout to be pure JSON-RPC; a naive client
crashes on the first line. Log to stderr (GAP-050).

## The right way today (updated)

1. init → warm → classify; expect ~35s/200 files (debug) / faster release.
2. File-level impact/related WORK — but treat counts as lower bounds while
   GAP-048 is open; cross-check with `impact 'pkg:<crate>'` and `stats`.
3. Symbols: `graph understand <path>` (file paths accepted) — real symbols,
   ugly formatting (GAP-053).
4. MCP: use a client that skips non-JSON lines until GAP-050 lands.
5. Build with `-p hilo-cli` (hyphen), not `-p hilo_cli` (GAP-051).
