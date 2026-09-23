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

## Run 7 — 2026-09-20 (warpfs-dogfood, FUSE mount deep dive)

This run changed angle: runs 1-6 and every tick since exercised the CLI /
CAG / MCP surface, so the **mount** — the flagship of "agent-first virtual
filesystem" — had never been used as a user would use it. It does not hold up.

### Why the empty-directory hang happens (DF-WARPFS-5)

`hilo-fuse/src/ops.rs:277` (`readdir`) always terminates with
`reply.ok()` at line 364 — **except** on the path at line 315-321, where a
missing inode entry does `reply.error(ENOENT); return;` … and on the path
where `populate_directory` (line 121) inserts nothing. Trace it:

1. `readdir(ino)` calls `populate_directory(ino)` at offset 0.
2. `populate_directory` iterates `std::fs::read_dir` and inserts an inode
   **per child**. For a directory with zero children the loop body never
   runs — nothing is inserted.
3. Back in `readdir`, `files.get(&ino)` is consulted for `"."` (line 293) and
   then for `dir_entry` (line 315). If the inode is absent, the handler
   returns having replied to nothing.
4. The kernel waits for a reply that never arrives → `ls`/`find` block until
   killed. Deterministic, 100% reproducible, and **only** for empty dirs:
   every non-empty sibling answers in ~110 ms.

The right way (for whoever fixes it): reply with at least `.` and `..` on
every code path — an empty directory must still emit those two entries.
Never return from a `readdir` handler without a reply.

Reproduce in 30 seconds:

```bash
mkdir -p /tmp/et && cd /tmp/et && mkdir -p emptydir withfile && echo hi > withfile/a.txt
hilo init && hilo graph warm && mkdir -p /tmp/m && hilo mount /tmp/m --daemon
sleep 2
timeout 5 ls -a /tmp/m/emptydir     # hangs: rc=124, no output
timeout 5 ls -a /tmp/m/withfile     # fine: rc=0, ". .. a.txt"
timeout 8 find /tmp/m -type f       # 0 rows forever, while disk yields 1+
fusermount3 -u /tmp/m
```

### Why the mount fails on a stock box (DF-WARPFS-6)

The error names `allow_other`, but nobody asked for `allow_other` — the
generated manifest says `allow_other: false` and the CLI flag defaults off.
The real coupling is inside `fuser` 0.15.1
(`src/mnt/mount_options.rs:26`, dependency source, not Hilo code):

> `AutoUnmount` requires `AllowOther` or `AllowRoot`. If `AutoUnmount` is set
> and neither `Allow...` is set, the FUSE configuration must permit
> `allow_other`, otherwise mounting will fail.

Hilo sets `auto_unmount: true` (`hilo-cli/src/commands/mount.rs:67`) and
`allow_other: false`, so the mount **must** be refused wherever
`/etc/fuse.conf` leaves `user_allow_other` commented out — which is the
default on every Debian/Ubuntu install. Nothing in `hilo mount --help`
turns `auto_unmount` off, so there is no CLI escape hatch.

This is the classic "works on my machine" shape: the dev host has
`user_allow_other` set, so months of local testing never saw it. The bunker
leg is what surfaced it. **Lesson: a mount test that only ever runs on one
host proves nothing about the mount.**

### Why `target/` and `.git/` are served despite being ignored (DF-WARPFS-7)

`grep -rn ignore hilo-fuse/src/*.rs` → zero hits. The mount tree comes from a
raw `std::fs::read_dir` walk (`ops.rs:125`). The ignore stack (`hilo ignore
check target/` → `ignored: true`) is wired into the graph/ephemeral paths but
never into the mount, so the two surfaces of the same tool disagree about
what the project contains. On this workspace that means a mount exposing a
130 GB `target/`.

### How the mount was tested (so the next run can go further)

- Fidelity: `stat -c %s` + `sha256sum` + `getfattr` compared disk↔mount (all
  matched; xattr round-trip through the mount confirmed).
- Traversal: per-directory `find`/`ls -R` timing with caps, then bisected by
  directory class (empty vs non-empty) — that bisect is what isolated the
  empty-dir bug from the "big target/" noise.
- Fresh box: mount attempted on the ephemeral bunker agent after the install
  leg — which is how DF-WARPFS-6 was found at all.
- Honest limitation: mount `stat` latency (105 ms/call) was **not** a Hilo
  defect — disk measured the same under this host's load (~14). Recorded here
  so a future run does not file it as one.

## Run 8 — 2026-09-20 (warpfs-dogfood, backend overlay + FFI bindings)

Runs 1–6 swept the CLI/graph surface; run 7 took the FUSE mount. This run took
the two surfaces nobody had touched: the **S3 backend overlay** and the
**UniFFI bindings**. Full report:
`docs/dogfood/2026-09-20-run8-backends-ffi-integration.md`.

### How the backend layer is built (and why it breaks)

There is a **legacy path and a §9 path** in `hilo-cli/src/commands/backend.rs`,
and which one you get is decided by a flag-presence test at the top of
`run_mount`:

```rust
let new_surface = matches!(kind, "gdrive"|"onedrive"|"dropbox"|"external")
    || args.remote.is_some() || args.tool.is_some() || args.mode.is_some()
    || args.ignore_file.is_some() || args.poll_secs != 60 || args.no_default_ignores;
if !new_surface { return run_mount_legacy(args); }
```

`run_mount_legacy` is a print statement with a comment admitting it
("In a real implementation, this would register the backend…"). So the plain
form of `backend mount` — the one `backend setup` prints in its own next-steps
line — exits 0 having done nothing (D1). Adding `--tool native` to the identical
command routes to `run_mount_new`, which validates the driver and appends to
`.vfs/backends/mounts.yaml`.

The two halves then disagree about where mounts live: `run_mount_new` writes
`mounts.yaml`, while `run_list` reads `.vfs/manifest.yaml` under `backends:` and
still carries `// Phase 3: read manifest backends`. A mount that exists is
invisible to `backend list` (D3).

### Why `backend sync` dies on the first push (D2)

`S3Driver` wraps `S3Client`, whose `head_object_meta` converts a missing object
to `Ok(None)` by **string-matching the Display of the error**:

```rust
Err(e) if format!("{}", e).contains("NotFound") => Ok(None),
Err(e) => Err(S3Error::from(e)),
```

The 404 a real S3 service returns for a HEAD on an absent key does not
necessarily render as "NotFound", so the "remote counterpart is absent → upload
it" branch is never taken and the whole sync aborts:

```
plan s3data: 1 to transfer, …          <- the driver's own plan says 1 file
error: s3data: backend error: aws sdk error: s3: aws error: service error
```

The mechanism is visible in the endpoint's request log (a `HEAD …404` right
before the failure), and the discriminator is decisive: pre-create the remote
key so the HEAD returns 200 and the same command succeeds. Two sync
implementations coexist (`commands/workspace.rs` uses `SyncEngine`+`S3Client`,
`commands/backend.rs` uses `BackendRegistry`+`S3Driver`) and only the
`workspace` one handles a fresh bucket (D4). Read the source of the one you are
actually running — they do not share a code path.

**Endpoint selection is ambient.** `S3Client::new` only builds an explicit
endpoint client when `AWS_ENDPOINT_URL` is set; otherwise it takes
`aws_config::defaults(…).load()`, i.e. `~/.aws/config` + the ambient credential
chain. That is why `backend sync` with the env var unset silently targeted a
real cloud endpoint from `~/.aws/config` and reported a real object to transfer
before failing (D4). When testing this surface, always know which of the two
paths is live — the CLI will not tell you.

### The S3 integration suite is phantom coverage (D5)

`hilo-backends/tests/s3_integration_test.rs` gates every test on
`check_minio_available()`, which curls `{endpoint}/minio/health/live` and
requires **200**. Anything that is not MinIO fails that probe (`moto` answers
404 there, 200 on `/`), so every test takes the `require_minio!` early
`return`s — and is still reported `ok`. Measured: 7 passed in 0.23s with no
endpoint, and **7 passed in 0.05s with a live S3-compatible server answering on
the configured URL**. The file's own header documents the skip as intentional
("so `cargo test` never fails on machines without Docker/MinIO"); the defect is
that a skip is indistinguishable from a pass in the report, and the readiness
gate proves "is this MinIO" rather than "can I reach S3". Do not treat this
suite's green as evidence about any S3 endpoint.

### The FFI layer (D6/D7/D8)

`hilo-ffi` really does build and really does export a UniFFI ABI — the UDL's 8
functions are present (`uniffi_hilo_ffi_fn_func_*`, plus checksums and
`UNIFFI_META_UDL_HILO`). That half is honest. What is missing is a usable path
in:

- **No generator in the repo.** No `[[bin]]`, no `uniffi_bindgen_main`;
  `uniffi-bindgen generate …` exits 127 and no doc says how to install it (D8).
- **Hard-coded resolution.** Every graph function opens the literal
  `.vfs/graph/graph.db` relative to the process CWD, and `vfs_graph_related`'s
  `path` argument is never used for resolution; `vfs_rule_check` probes
  `manifest.yaml`/`.vfs/manifest.yaml` the same way (D6). The CWD-dependence is
  the same architecture the CLI has — but the CLI is *documented* as
  per-repo and the FFI is for embedding in a host application, where the CWD is
  someone else's.
- **Constants dressed as facts.** `vfs_resolve_backend` always answers
  `backend:"local"`, `last_synced:"synced"`; the MCP twins
  (`vfs_backend_status`, `vfs_sync_backend`, `vfs_resolve_path`) do the same
  (`tools/mod.rs:922-1005`). Measured over MCP with an S3 backend mounted: still
  `{"backend":"local",…,"last_synced":"synced"}` (D7).

### Diagnostics and inspection recipe that worked

- `nm -D --defined-only target/debug/libhilo_ffi.so | grep uniffi` — the only
  cheap way to prove what a language binding will actually link against.
- A throwaway S3-compatible server (`moto[server]`, `pip install "moto[server]"`)
  plus the AWS CLI for bucket/object ground truth: `aws --endpoint-url … s3 ls
  --recursive` is the independent check on every "sync completed" claim.
- **Compare the plan count against the bucket listing, every time.** The
  `plan … N to transfer` line and the bucket are independent measurements; three
  of this run's findings came from them disagreeing.
- Read the endpoint's own request log. The `HEAD … 404` line is what turned
  "aws sdk error: service error" into an exact mechanism.
- Keep credentials for scratch endpoints in a file and source it, never on a
  command line — and never in the repo.

### Honest limitation

The conflict test is **not** a defect. `docs/` documents last-writer-wins by
mtime, and both directions were verified to follow it (local newer → upload and
keep local; remote newer → download and overwrite local). No conflict artifact
is produced because none is promised. Recorded here so a future run does not
file it as data loss — but note the local edit is genuinely unrecoverable once
overwritten, which is worth knowing before putting a directory under sync.

## Run 9 (2026-09-22) — git backend + plugins: how the doors are wired, and where they silently open onto nothing

### How the backend mount path is layered (and why the bug hides there)

`hilo backend mount` has two code paths that disagree:

1. **The default/auto path** (no `--tool`, or `--tool auto`): prints
   `mounted <kind> … at <path>`, returns 0, writes nothing. For `--type git`
   and `--type local` this is the *only* path a docs-following user takes,
   and it is a no-op dressed as success.
2. **The explicit-tool path** (`--tool native|rclone|…`): runs the config
   validator first, and the validator's type whitelist is
   `s3|gdrive|onedrive|dropbox|external` — git and local are hard-rejected
   with `unknown backend type`.

So the validator that knows git doesn't exist is only reachable through the
flag the user was never told to pass. The diagnosis recipe that found it in
one step: after a mount reports success, `cat .vfs/backends/mounts.yaml`.
Exists → the mount really persisted. Missing → you are on the silent path.
That single check separates "mounted" from "claimed mounted" — use it before
any other debugging, and re-check `backend list` even when the file exists
(DF-WARPFS-11: list reads the wrong manifest and still reports none).

### The two run-8 defects that reproduce at v0.3.0, mechanism unchanged

- **`backend list` blindness**: mount persists to
  `.vfs/backends/mounts.yaml`; list reads `.vfs/manifest.yaml`. Verified
  identical at HEAD 917a991 with an S3 `--tool native` mount on disk.
- **Opaque S3 sync errors**: `aws sdk error: s3: aws error: service error`
  on first `backend sync --pull/--push` against a nonexistent bucket. The
  underlying cause (run 8): the SDK error is mapped to NotFound by
  string-matching the Display text, so everything that doesn't match becomes
  the opaque form with no code, no endpoint, no hint. Run-8's discriminator
  (pre-create the remote key, the identical command then succeeds) still
  applies.

### Plugin surface: trust nothing until a byte-level check exists

`hilo plugin load <file>` parsed a 9-byte text file as a plugin and
announced hooks/edge_types for it. Until load-time validates the WASM magic
(`\0asm`) and reports a real parse failure, "loaded plugin: X" carries no
information — and `plugin list` reading `.vfs/plugins/` (never created by
load) means there is no persistence probe either. The only honest check
today: `ls .vfs/plugins/` after a load. If it is empty, nothing happened.

### Memory ceiling: verified working in real use (GAP-092..095 hold)

The v0.3.0 RAM work was re-verified behaviorally, not by test names:
`hilo graph warm` over hilo's own 107-file source tree peaked at 211 MB RSS
(`/usr/bin/time -v`), `graph stats` at 47 MB, on a machine with no per-op
limits. Edge counts matched (870 distinct/1,062 raw). Nothing regressed.

### Session recipe for backend probing (the one that worked)

```bash
hilo backend mount --type git --url <URL> --at code && cat .vfs/backends/mounts.yaml   # expect: missing
hilo backend mount --type s3 --bucket B --prefix P --at s3 --tool native && cat .vfs/backends/mounts.yaml  # exists
hilo backend list                                                                       # still blind
```

`backend setup` (flag form: `--type s3`, never positional) is the one
backend UX that tells the truth about the five live types — worth extending
to git/local the day they become real.

### Run 10 (2026-09-23): triggers — the event that never had a directory

The run-10 discriminator (nested write silent, root write fires in 0.5 s)
pins the class: **inotify `name` is relative to the WATCHED directory, not
to the project.** Any consumer that converts it to a path without
reconstructing the watch directory silently addresses the wrong file. The
fix shape is not "fix parse-and-diff" — it is "reconstruct the full path
once, in the event loop, where the watch table is in scope, and carry it on
the FileEvent" (the reconstruction already exists at engine.rs:203-209 for
the sync hook; it just never feeds the event). One truth at the source
beats per-consumer patching.

Three transferable lessons from the same run:

1. **A pipeline whose failures all log via `info!` under a binary with no
   subscriber has no error surface at all.** hilo-cli never installs a
   logger, so nine distinct failure branches in parse_and_diff_sync
   (ENOENT, unsupported ext, parser init, parse fail, append fail…) all
   vanish. A "watching N triggers active" banner should be paired with a
   per-fire line and an installed subscriber — the banner is a promise, the
   log lines are its receipt.
2. **`key: []` and key-absent must not be conflated.** `load_triggers`
   treats a manifest `triggers: []` as "user's choice" (zero triggers)
   while absent → defaults. Both are silent. Empty-by-manifest should
   announce itself ("manifest disables all 9 default triggers") or fall
   back; anything else turns the flag `--triggers` into a printed lie.
3. **DuckDB is single-writer: exactly one process may own graph.db.** The
   trigger engine opens it eagerly and holds it; every later CLI graph
   command (and every second mount) then hard-fails with a lock error that
   names the holder PID but never says "that's your mount". Ownership +
   lazy attach + a human hint in the error is the whole fix.

The testing method is the reusable part: a **root-level canary file with
imports** (`printf 'use crate::…;' > probe_root.rs`) is a 1-second
liveness probe for parse-and-diff — count lines in edges.jsonl before and
after. It separates "engine dead" (no root fire) from "engine blind to
subdirs" (root fires, nested silent) from "engine lock-starved" (banner
shows the DB error). That probe should become a `hilo graph selfcheck`
someday.

### Run 11 (2026-09-23): Java, concurrency, and the teardown that does not tear down

**1. A single-writer database behind a read-mostly interface is a
concurrency bug waiting for its second caller.** Hilo's graph lives in DuckDB
and every command — including `graph stats`, which only reads — opens it with
`Connection::open` (hilo-graph/src/graph.rs:1080), which takes the file's
exclusive write lock. One user is fine; an agent platform is not. Measured:
8 concurrent CLI queries → 7 fail with `Conflicting lock is held`. The same
code shape appears in three separate run findings now (mount holds it while
mounted, DF-WARPFS-30; two commands collide, DF-WARPFS-33; the unmounted
process keeps it, DF-WARPFS-32). The fix is architectural, not per-site: a
read-only open path for query-only commands, plus an owner-aware retry.

**2. Ownership of the DB handle is what makes an unmount a lie.** `fusermount3 -u`
detaches the FUSE session; it does not stop the daemon. Because the trigger
engine owns its DB handle on a thread of its own, the process outlived the
unmount and kept the lock for as long as it was observed (≥26 s, until killed),
while a plain mount exited in <4 s. The design question is "who owns the
handle and when is it dropped", and the acceptance test is a *process* check
after unmount, not a mountpoint check — `mountpoint -q` returning false is
exactly the state that fools the user.

**3. Edge dialects are the whole product surface for blast radius, and the docs
do not name them.** Hilo stores targets as `pkg:`, `sys:`, and `local:` nodes
depending on the language, and resolves a bare file path to them per language.
Java is the fourth language where that resolution is unimplemented: the
`pkg:` form answered exactly (112/112 against grep) while the file form — the
form the docs use everywhere (`docs/cli-reference.md:148`) — returned
"No dependents found" for a class with 194 edges pointing at it. Worse, the
same unresolved data made `graph stats` print that class as an *orphan*, i.e.
the tool contradicted its own inventory in the same breath. A field that is
silently wrong in one id dialect and exact in another needs a test that runs
every dialect the docs advertise for every language the docs advertise —
otherwise the language table in `docs/graph-engine.md` is a promise nobody
checks.

**4. The reusable probe for this class of defect is the two-dialect answer.**
Query the same relationship in file form and in dialect form, and diff the
counts against grep. On any new language corpus: one grep, two queries,
three numbers. That is what caught this run's P1 in under a minute, and it is
what a `hilo graph doctor` subcommand should do.

**5. Signal for the "understanding" surfaces is the same trick.** For
`graph understand`, ask a question whose answer lives in a *named* file
("how does Gson serialize null fields" → `Gson.java:186`) and check whether
that file reaches `## DETAIL`. Symbol extraction that returns `@Override` and
`context)` is not a weaker answer, it is a different kind of output — and
because the pack is formatted, it reads as an answer.
