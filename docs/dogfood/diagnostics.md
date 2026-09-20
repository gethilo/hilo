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
5. Build with `-p hilo-cli` (hyphen) (GAP-051 — fixed 2026-08-24).

---

# 2026-09-11 Update — Third Real-Use Run (containerd, 5489 Go files — vendor-guard corpus)

Why this corpus: the PERF-005 safety fix (refuse vendor/HOME graphs) landed
the night before this run (fcebefe). containerd ships a committed `vendor/`
tree (4122 Go files vs 1367 project files) — exactly the input that used to
poison graphs and burn hours of CPU, so it is the honest test of the guard.

## How the vendor guard behaves (and why it looks like data loss)

`hilo graph warm` on containerd prints "parsing 1367/1367 files..." and
"Discovered 14070 edges across 1287 files". The 4122 vendor files never
appear. Verified three ways that this is policy, not loss: file counts
match the non-vendor tree exactly; zero of 14070 edges reference `vendor/`;
`stats` total files ≈ 1367. The graph is *cleaner* than pre-PERF-005 runs
on the same shape — but nothing in the output says so (GAP-058). Until an
exclusion report lands, the right way to confirm a guard is:
`grep -c '"vendor/' .vfs/graph/edges.jsonl` → 0.

## The Go resolution asymmetry (the one real gap left)

The query-time resolution layer (the GAP-034/GAP-048 machinery) maps files
to package nodes for **Rust crate roots** (lib.rs/mod.rs) but has no rule
for **Go packages**: a Go file lives in a *directory package* whose node is
`pkg:<module-path>/<dir>`. Consequences, all verified:

- `impact <dir/file.go>` → empty; `related <file> --direction reverse` →
  empty; MCP `vfs_graph_impact` file-form → `{"dependents":[],"total":0}`.
- `impact 'pkg:github.com/containerd/containerd/v2/core/mount'` → exactly
  the 126 grep-verified importers (depth 1). The EDGE DATA is complete and
  precise — only the file→node lookup is missing (GAP-057).
- Manual resolution works today: `<module>/<dir>` from go.mod + file path.
  E.g. for `core/mount/lookup_unix.go` query
  `pkg:github.com/containerd/containerd/v2/core/mount`.

For agents: until GAP-057 lands, never report "no dependents" for a Go file
without trying the directory's `pkg:` node first.

## Empty vs unknown

`related` on a path that is neither on disk nor in the graph answers
"No incoming edges" with exit 0 — indistinguishable from a true answer.
`impact` errors ("is not in the graph"). Same input, two philosophies
(GAP-059). Trust impact's error; double-check related's empty.

## Install truth (fresh box)

Ran the README path on a bare Debian 13 bunker user: rustup (not present by
default, standard install), then `cargo build --release` → RC=0 in 18m52s
(499 crates; README says 15-20 min — accurate). clang/cmake, listed as
requirements, were NOT needed. Smoke (init→warm→stats on hilo itself) green
in 3s. A fresh user following the README succeeds.

## The Python resolution asymmetry (run 4)

GAP-057's fix (9431cac) taught the query-time resolution layer to map a Go
file to its directory package (`pkg:<module>/<dir>`). Python got nothing:
`fastapi/routing.py` should resolve to `pkg:fastapi.routing` (the exact
node its importers already write edges to), but the resolver only handles
Rust crate roots and Go dirs. Verified on fastapi @ 50113da: file-form
impact/related → empty on CLI and MCP while pkg-form is 10/10 exact
against grep ground truth. Same lesson as run 3, one language later:
**when file-form answers "No dependents", resolve the package yourself
and query `pkg:` before believing it.** For Python: `<file>.py` minus the
`.py`, slashes → dots, stopping at the package root (where `__init__.py`
chains start).

## Coverage: collected, not consumed (run 4)

tested_by edges ARE emitted for Python test files (2225 on fastapi; each
test file emits both `imports` and `tested_by` to the same pkg nodes).
But `graph untested` and `graph module` only match incoming tested_by
edges against the file itself — they never resolve the file to its pkg
node — so flagship files with 8+ real test edges still report "untested"
and module coverage reads 0.0%. Until GAP-066 lands, derive Python
coverage from edges.jsonl directly:
`grep '"tested_by"' .vfs/graph/edges.jsonl | grep -c 'pkg:<pkg.name>'`.

## Silent exclusions, three flavors (runs 3–4)

Warm's output counts never mention what they skipped. Three distinct
mechanisms hide behind the same silence, all filed under the GAP-058
family: (1) vendor/dependency guard (run 3, policy, verified clean via
edge grep); (2) `__init__.py` skip (run 4: 183/203 exclusions — policy
or bug, undocumented either way); (3) AST parse failures (run 4: 20 real
Python files, zero warnings). A user cannot distinguish "protected by
policy" from "we dropped your file". The fix is one exclusion report at
end of warm, per category.

## MCP transport reality (run 4)

`hilo serve --mcp` speaks NDJSON JSON-RPC 2.0 on stdin/stdout (one
message per line, per the MCP stdio transport spec) — a client sending
LSP-style `Content-Length` framing gets `-32700 Parse error`. The
2026-08-13 run's client worked because it used line framing; this run's
first client attempt used LSP framing and hung. Provenance from the edge
parser (`ast_exact conf=1.0`) is exposed in both CLI and MCP impact
output — useful for trusting the graph.

## Silent malformed writes (run 4)

`hilo meta <path> --set key=value` exits 0 and writes an xattr whose
*name* contains `=` with an empty value (verified via getfattr and MCP
vfs_get_metadata). Nothing validates the attr token. The documented form
is `--set <attr> --value <val> <path>`; attribute-first, value flag,
path last, no `=` in the attr.

## Install truth, second clean box (run 4)

Same recipe as run 3 on a fresh bunker agent: rustup minimal profile →
`cargo build --release`. Confirms run 3's findings: rustup absent by
default (expected), clang/CMake unnecessary (GAP-060/068 docs drift),
libfuse3-dev not needed unless mounting. Operational note for anyone
running the build over ssh: the full clean build exceeds a 590 s command
window (dies mid duckdb-sys ~7.5 min in); the build resumes cleanly from
target/, so re-run rather than restart — or run it under nohup/tmux on
the box.

## TS/JS edge dialect: `local:` targets, and what breaks without resolution (run 5)

vite edges read `{"from":".../server/hmr.ts","to":"local:./pluginContainer"}`
— TS/JS relative imports become `local:<relative-specifier>` pseudo-nodes,
not `pkg:` nodes. The resolution layer learned Go dirs (GAP-057) and Python
modules (GAP-064) but not this dialect, so (a) file-form impact/related are
silently empty (GAP-069), (b) `local:`-targeted tested_by edges don't reach
untested/module (GAP-071), (c) search prints `local:` node names a user
cannot open (GAP-072). The edge data itself is exact (ast_exact conf=1.0);
every gap lives in the query/resolution layer. The one-language-per-run
pattern is now Rust → Go → Python → TS/JS: **after each fix, the next
corpus finds the next dialect.** Expect the next languages (Java/Kotlin/C#)
to need their own pass.

## The clean→warm trap (run 5, GAP-070)

`graph clean` deletes edges.jsonl + graph.db and tells the user to re-run
warm; warm then short-circuits on `.parse_cache.json` ("all cached, graph
unchanged") and stats reports an empty graph. Recovery requires deleting
the parse cache by hand — an undocumented internal file — because warm has
no `--force`. Lesson: **state owned by different layers must be
invalidated together**, or the tool's own recovery instructions lie.
Workaround that always works: `rm .vfs/graph/.parse_cache.json && hilo
graph warm` (10.5s on vite; rebuild is byte-identical, so determinism is
not the casualty).

## Wrong-path UX paid off (run 5, small win)

A query for a file at its pre-move path (`node/pluginContainer.ts`, the
file actually lives under `node/server/`) produced: "not in the graph (no
such file and no matching graph node)" — naming both failure modes. That
message let me self-correct in one step instead of suspecting the graph.
Contrast with the silent-empty result for a path that IS in the graph
(GAP-069): the tool is loud exactly where it should be loud — it just
isn't loud about the one case that matters most.

## Install truth, third clean box (run 5)

Same recipe, fresh agent (da91c36c), public-repo clone at exact HEAD:
rustup absent by default (expected), clang/CMake absent and unneeded
(third confirmation of GAP-068). See the 2026-09-13 integration report
for timing.


## Run 6 — 2026-09-20 (warpfs-dogfood)

- Fresh-bunker install proven on a third machine (Debian 13): clone →
  rustup → release build 1146s → smoke pass. No sudo used at any point;
  the README dependency list is additive-only on trixie (base image
  already had the libfuse3-4 runtime lib; the build never needed the dev
  headers or pkg-config).
- Spawn-infra diagnosis: the CLI reported deadline_exceeded at exactly
  the CLI-side timeout. After verifying the repo source already carries
  the 300s request budget (cli/spawn.go 137), the 0.1.4 CLI was rebuilt
  from ~/bunker and REUSED — the daemon was not rebuilt or edited. The
  server journal shows the kill happening inside the rootless installer
  ~45-49s in even so, so the CLI fix alone may not be the whole story;
  DF-WARPFS-3 asks the bunker project to arm the rootlessInstallerCacheDir
  host-cache seam and re-check the server-side budget.
- Consumer surface verdict: no P0/P1 product defects found. GAP-070
  (clean → warm strands the user) regression-checked as fixed: clean
  removed 4 cache files, next warm fully rebuilt, stats healthy.
