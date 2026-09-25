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

## The under-count (fixed 2026-08-24: GAP-048 pkg-family matching)

The run-2-era resolution layer matched **exact** `pkg:serde` edge targets
only, while brace-group expansion (GAP-035) emits **per-member** edges —
`use serde::{Serialize, Deserialize}` → edges to `pkg:serde::Serialize`,
`pkg:serde::Deserialize`, not `pkg:serde`. On serde that was 7 exact edges
vs 53+ member edges; 148 unique files import serde in some form; impact
returned 6. GAP-048 (closed 2026-08-24) added pkg-family matching:
`pkg:<name>` now also matches `pkg:<name>::*` members and `pkg:<name>_*`
companion crates (serde_derive/serde_test convention). On serde today:
`impact serde/src/lib.rs` = 147, `impact 'pkg:serde'` = 148. File-level
counts are no longer lower bounds. Live caveat: pkg expansion is
CWD-anchored (DF-WARPFS-55).

## How classify actually behaves (updated 2026-09-05: GAP-049 landed)

Run-2-era classify tagged tests well (151/208 on serde, incl. nested
test_suite/), but crate-root lib.rs files with hundreds of importers got
role `unknown` and only build.rs got `entrypoint`. GAP-049 (closed
2026-09-05) fixed the role heuristics: build.rs/build.zig → `build`,
lib.rs/mod.rs crate and module roots → `library`. Verify on a fresh
classify run before trusting cached role xattrs from older graphs.

## The tested_by hole (status 2026-09-25: emitter still missing; DF-WARPFS-37/39)

GAP-052's "nothing emits tested_by" finding predates run-12's Java work:
the Java path NOW emits tested_by edges (1744 on gson), but they target
`pkg:` pseudo-nodes and no consumer resolves them to files — so
`graph module` still reports Tests: 0.0% while FFI/`graph untested` count
emitting files as a proxy (58.02%). It is still a "not a test file"
filter, not file coverage (DF-WARPFS-37/39 track the pkg-target +
arithmetic-contradiction halves; GAP-052's Rust-side emitter gap stands
for Rust corpora).

## MCP stdout hygiene (resolved 2026-08-24: GAP-050 shipped)

`hilo serve --mcp` used to write a tracing INFO event to stdout at
startup, crashing naive JSON-RPC clients on the first line. GAP-050
(closed 2026-08-24) moved tracing to stderr and added a purity regression
test. Run-12 (2026-09-23) measured zero non-JSON stdout lines across six
client sessions; `serverInfo.version` also now reports the real version
(run 4's "serverInfo lies: 0.2.0" defect is fixed). stderr-only tracing
is shipped behavior — no client workaround needed.

## The right way today (updated)

1. init → warm → classify; expect ~35s/200 files (debug) / faster release.
2. File-level impact/related WORK with pkg-family matching (GAP-048):
   counts are full, not lower bounds. Live caveat: pkg expansion is
   CWD-anchored (DF-WARPFS-55).
3. Symbols: `graph understand <path>` (file paths accepted) — clean
   per-line symbol lists (GAP-053 fixed 2026-08-24).
4. MCP: plain JSON-RPC client; stdout is pure (GAP-050 shipped), tracing
   is stderr-only.
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

## Run 12 — 2026-09-23 (warpfs-dogfood, MCP server surface)

**The surface nobody had driven.** Runs 1–11 never connected a real MCP
client to the 0.3.x server (run 1 drove 6 of the 0.2.x-era 15 tools). This
run wired a raw NDJSON JSON-RPC 2.0 client over stdio — the exact shape
Claude Code and Hermes use — against a scratch copy of hermes-canopy.

**What the server got right (and it is a lot):**

- **Handshake and schema are honest now.** `serverInfo.version` reports
  `0.3.1-dev` (run 4 recorded "serverInfo lies: 0.2.0" — fixed). `tools/list`
  returns exactly 17 tools with real descriptive schemas, including which
  argument is required (`path` for most, `task` for understand). My own first
  battery failed 4 calls by guessing `file=` — the failure message
  (`missing 'path' argument`) is actionable, and the schemas name the right
  key. Check the schema before filing a "wrong arg name" finding: it is the
  documented-command-form trap in MCP shape.
- **Error messages earned their keep.** Unknown file → `-32603` listing all
  three accepted id forms (bare path / `sys:` / `pkg:`) — DF-WARPFS-2's fix
  works end-to-end over MCP. Unknown tool → clean `Unknown tool`. Zero
  non-JSON stdout lines across six client sessions: the GAP-050 stdout
  hygiene fix is real (tracing INFO goes to stderr).
- **The agent-facing tools are fast and real.** `vfs_graph_impact`
  internal/card/service.go depth 3 → total=27, all 27 returned, file set
  identical to the CLI's own impact output. `vfs_graph_search` "context
  compiler" → top hit is the real compiler file. `vfs_graph_understand`
  "card storage and sync" → anchored on 8 real card files with tiered
  excerpts. Stats match the CLI byte-for-byte in content. Warm battery ≈ 1 s
  end-to-end including process spawn; in-server answers are tens of ms.
- **Metadata survives server restart** (xattr written via MCP `vfs_set_metadata`,
  read back through a fresh process) — same persistence guarantee run 1 proved
  over CLI, now proven over the server surface.

**But the front door is broken: `vfs_list_directory` silently returns empty.**
`{"entries":[],"total":0}` with rc=0 on internal/card, frontend, frontend/src,
cmd, `.`, an absolute path — 7/7 real populated directories — and even on a
*file* path (which should be a type error, also silent-empty). This is the
same "0 results and no error = treat as hang" class the 2026-09-20 FUSE run
caught in readdir. Proof the capability exists in-process:
`vfs_workspace_ephemeral` on the same tree enumerated frontend/dist files
correctly, so path resolution and enumeration work elsewhere in the same
server binary. The tool that orients an agent in a new repo — "what is in
this directory?" — is the one that answers a confident zero. Filed
DF-WARPFS-41. Until it lands: agents should use `vfs_graph_search` or
`vfs_workspace_ephemeral` for orientation, never trust `vfs_list_directory`'s
empty answer (skills/hilo-usage/SKILL.md updated this run).

**Two more findings from the same battery:**

- **stats' file census disagrees with warm's own coverage line** — warm prints
  `705 files` (659 contribute edges + 46 no imports), stats prints
  `Total files: 666`, same tree, same run, same edges.jsonl. 39 covered files
  vanish between the two headline numbers. Filed DF-WARPFS-43 (two-numbers-
  one-board law: they must agree or the difference must be visible).
- **The MCP stdout-hygiene section of this file is stale** — it documents
  stdout INFO as a live trap "until GAP-050 lands"; GAP-050 has landed and
  this run measured 0 violations. Filed DF-WARPFS-42 (docs-only).

**The right way over MCP today (0.3.x):** one persistent client connection
(amortize the ~1 s spawn), `initialize` → `tools/list` → drive
`vfs_graph_impact` / `vfs_graph_related` / `vfs_graph_search` /
`vfs_graph_understand` / `vfs_graph_stats` / `vfs_get_metadata` /
`vfs_set_metadata` with `path`/`task` arguments exactly as the schemas say.
For directory orientation use `vfs_workspace_ephemeral` (works) — do not
trust `vfs_list_directory`. Errors are informative; batch independent calls;
the server is stateless per-process except xattrs, which persist.

## Run 13 — 2026-09-24 (coding-hermes-tools-dogfood, docs site + fix re-verification)

**The surface nobody visits:** the public docs site at
https://gethilo.github.io/hilo/ — the front door every new user walks
through. It is LIVE (redeploys on every push to docs/, last 2026-09-24) and
it is broken in two ways a first-touch user hits immediately:

- **All five "Guides" links on the landing page 404.** `docs/index.html`
  links `getting-started`, `graph-engine`, `mcp-tools`, `cli-reference`,
  `architecture` with no extension; Pages serves the repo's docs/ verbatim
  with no Jekyll rewriting, so only the literal `*.md` URLs work
  (`getting-started` → 404, `getting-started.md` → 200). The links were
  written for a site generator that was never configured.
- **`dashboard.html` was a frozen v0.2 snapshot that lied.** Footer said
  "Generated 2026-07-12"; it claimed 15 MCP tools (17 real), 10 crates
  (11), stale test/file counts, "static_analysis · lsp PASS" (AGENTS.md
  records those guard legs disabled as fake-green), and Recent Commits
  ending ~70 commits ago. Nothing regenerated it. **Deleted 2026-09-24
  (DF-WARPFS-47)** — a dashboard that lies is worse than no dashboard;
  cite live `hilo graph stats` / `tools/list` output instead.

Lesson for the project: CI redeploys the site on every docs/ push, so the
site is always "fresh" — freshness of deploy is not freshness of content.

**Fix re-verification by real use (the other half of the angle).** Four
defects closed on 09-23/24; each re-tested against the canopy corpus with
the exact behavior that broke before, not by reading tests:

- DF-41 `vfs_list_directory` → returns 15/11 real entries, errors on bad
  paths. Fixed. (Update to the run-12 lesson above: the tool is trustworthy
  again as of commit 8476574.)
- DF-33 concurrency → two simultaneous MCP servers on one repo both answer
  impact+stats; 8 parallel CLI graph commands, 8/8 rc=0 (read-only DuckDB
  opens). The "one agent per repo" limitation is gone.
- DF-30/DF-32 trigger mounts → `graph stats` works WHILE a --triggers mount
  is up (no standing DB lock), and the daemon self-exits ~1s after
  `fusermount3 -u` with no manual kill. The run-10/11 leak class is dead.
- DF-28/DF-29 → defaults load from `hilo init`'s `triggers: []` manifest,
  and nested-directory file writes now fire parse-and-diff (new files append
  edges in ~0.5s). Root-only watching is gone.

**And one NEW defect found only because re-verification used the tool the
way a user does:** edits to EXISTING `.tsx` files never fire parse-and-diff
(0/8 .tsx writes incl. 30s waits) while .ts/.rs edits fire instantly. The
engine parses tsx fine — `default_triggers()` (hilo-cli/src/commands/
mount.rs:502) just doesn't watch `*.tsx`/`*.jsx`. React repos get a living
map that silently ignores every component edit. Filed DF-WARPFS-45.

**The bunker leg's own lesson:** the QA battery could not run at all this
tick — bunker-las-02's bunkerd was stuck in `activating` and refused spawn
connections twice (15:33Z). The manual install procedure ran on las-03
instead. One server outage should not cost a whole dogfood tick its install
leg — the skill's designated fallback (las-03) is the working answer; the
qa script only knows one server.

## Run 15 (2026-09-25, MCP server via the OFFICIAL SDK): the integration path a real agent uses

Run 12 drove 8/17 tools with a hand-rolled subprocess NDJSON client. Run 15 drove the
REMAINING 9 (related, module, understand-natural-tasks, list_directory, workspace
ephemeral/wipe, backend status/sync, rule_check) through the official `mcp` Python SDK —
the exact stack Claude Code / Hermes integrations use. Verdict: SHIPPABLE.

- **Schemas served to the client are accurate** (`module_name`, `name`, `task`+`budget`/
  `resolution`) — a real integration never guesses arg names. The spec §11 doc is what
  lies (DF-WARPFS-52): it still says `module`/`rule_name`, omits `task`, and shows
  response wrappers the tools don't emit (`related` returns a bare edge list, not
  `{files:[...]}`).
- `vfs_graph_related` returns a flat LIST of edge objects
  `{from,to,relation,confidence,provenance,scope}` — parse accordingly.
- `test_coverage_pct: 0.0` on a Rust module is BY DESIGN (graph.rs:1905: `.rs` files are
  excluded from node-rule coverage because pkg: nodes are Cargo crates — one crate-root
  `tested_by` edge would mark a whole `src/` covered). Do not file this; Go/Python repos
  get real percentages.
- DF-WARPFS-41 (list_directory silent-empty) is FIXED — verified live on this run.
- `understand` anchor recall drops on one-generic-word tasks ("workspace" → 3 anchors, all
  correct but few). Use a 3-6 word concrete task ("trigger debounce", "parse edges
  duckdb") — recall is excellent there (DF-WARPFS-53 asks for worked examples in help).
- SSE transport does NOT exist despite spec §11/manifest (`hilo serve` = clap error,
  `--mcp` is the only mode). stdio only.
- No-project: exits 1 with `run 'hilo init' first` — clean contract.
- Numbers: understand 186 ms ± 8 warm (n=12) on a 110-file repo; handshake 0.01 s;
  warm 2.24 s; zero non-JSON stdout bytes across all sessions.

## Run 14 — 2026-09-24 (task-router-dogfood, workspace / multi-repo surface)

**The surface nobody had ever driven:** `hilo workspace mount|unmount|sync|
ephemeral|wipe` — spec §6's multi-repo flagship. 14 runs in and this was still
virgin; the angle rule ("runs 1-13 passed the CLI/graph surface green, take the
untouched one") sent run 14 here, and it was the right call: the surface is
🔴 DOES-NOT-DELIVER end to end.

**How it is built (and why it fails):** the workspace FUSE implementation is a
SEPARATE hardcoded-inode-map filesystem (`hilo-fuse/src/workspace_mount.rs`),
not the main `ops.rs` FUSE stack. `populate_mount_children()` walks exactly one
level of the backing repo; nested directories get inodes but nothing ever fills
them, so every nested path ENOENTs forever (DF-WARPFS-48, P0). The mount is
hardcoded `MountOption::RO` (workspace_mount.rs:516) with
`read_only: true` from the CLI (workspace.rs:50) — the spec §6 per-repo
`writable:` flag is decorative, while the mount log prints "(rw)" (DF-WARPFS-49).
The manifest is a second YAML dialect (`WorkspaceManifest` in
hilo-core/src/workspace.rs:38: repos/backends/mounts/auto_dependency_order ONLY)
that rejects `hilo init`'s own `.vfs/manifest.yaml`, spec §4's `at:` field, and
boolean `auto_pull` — with zero documentation anywhere (DF-WARPFS-50). The
backend half WORKS: managed worktrees clone cleanly into
`~/.hilo/worktrees/<name>/` (provenance-clean, complete trees). The failure is
entirely the FUSE presentation layer.

**The right way, for whoever fixes this:** the single-repo FUSE (ops.rs, run 7)
already solves nested readdir/lookup and xattr passthrough — the workspace
mount should reuse that engine per-backend instead of a second hand-rolled
inode map; then a two-level `cat` regression test through the mount, and a
cross-repo `external:` edge test for `graph warm --workspace` (currently a
silent 0-file no-op, DF-WARPFS-51).

**Fixture + evidence:** `/tmp/dogfood-warpfs-run14/` (two Go repos with a real
cross-repo import, bare remotes), full transcript in
`docs/dogfood/2026-09-24-run14-workspace-integration.md`. Perf: mount-to-listable
111ms cold, unmount 10.0ms ± 0.6ms — no PERF row; the surface is broken, not slow.

## Run 16 — 2026-09-25 (warpfs-dogfood, Go FFI consumer): how the resolver turns CWD into graph truth

**How this surface is built.** `hilo-ffi` compiles to `libhilo_ffi.so` with a
UniFFI `.udl` as source of truth; `uniffi-bindgen-go` generates cgo bindings
that `#include <hilo.h>` and link the `.so` directly (contrary to the README,
which says Go "does not consume this `.so` directly" — it does). `HiloHandle`
canonicalizes and stores the repo root in its constructor, then every method
joins that root: `graph_db()` opens `<root>/.vfs/graph/graph.db`,
`graph_subject()` strips the root prefix from absolute paths and passes a bare
repo-relative path (the "subject") down to `hilo_graph::compute_impact`.

**The design seam that failed.** The BFS in `impact.rs` needs to expand a file
subject into its package node (`parser.py` → `pkg:plugin.terminal_jail.interruptor.parser`)
so it can hop file → package → importers. That expansion (`PkgResolver::pkg_node`
→ `python_module_for_file` / `go_package_for_file` / `crate_name_for_file`)
works by **walking the filesystem**: climb parents looking for `__init__.py`
/ `go.mod` / `Cargo.toml`. But it receives the bare relative subject, and
nothing anchors the walk at the handle root — the process CWD decides where
`__init__.py` is found. When CWD == repo root the walk happens to land in the
right package and the answer is right. From any other CWD the walk dies at a
non-package parent and returns `None`, and the impact BFS then only sees
direct edges — silently (0 dependents, rc=0). No error anywhere, because
`None` is a legal "no package node" answer.

**Why it took a three-surface bisect:** the Go consumer failed while the
Python FFI probe "worked" — because that probe was (accidentally) run from
the repo copy's root, and the working/non-working split followed CWD, not
binding. The ladder that localized it: (1) Go depth sweep — depth had no
effect, so the argument lowering was innocent; (2) explicit `pkg:` start node
through the Go bindings — worked perfectly (264-byte response), so the
cgo/RustBuffer path was innocent; (3) same query, Python FFI, only CWD
changed — flipped 3 → 0. Root cause in one file/line:
`hilo-graph/src/resolution.rs:359` (`current.join("__init__.py").is_file()`)
and its siblings — filesystem walks anchored at CWD instead of root.

**The right way:** the subject's package-node expansion must be anchored at
the root the handle already canonicalized (thread the root into
`PkgResolver`, or resolve the subject to an absolute path before the walk and
strip the prefix from the resulting module name). The CLI inherits the same
behavior via the same resolver — fixing it at the resolver fixes both
surfaces, and the regression test is cheap: build a graph in a tmpdir,
compute impact from a different CWD, assert the pkg-mediated dependents are
still returned.

## Run 17 — 2026-09-25 (coding-hermes-tools-dogfood, plugin surface + manifest re-test): the happy path was the untested path

**How the plugin surface is built.** Three pieces that have never met each
other: (1) `hilo-core/src/manifest.rs:644-682` parses a `plugins:` manifest
block — name, wasm path, `hooks:` (`on`/`languages`/`priority`),
`provides:` — under `deny_unknown_fields`, so the spec §4 example parses
clean; (2) `hilo-plugins` holds a registry (`PluginRegistry::discover` scans
`.vfs/plugins/` for `.wasm`, honest header checks from DF-WARPFS-22) and a
runtime whose `dispatch_hook` *simulates* declared hooks (canned `AddEdge`);
(3) `hilo-cli/src/commands/plugin.rs` builds a throwaway runtime from the
`load` file argument, prints honest metadata, and persists the file into
`.vfs/plugins/` (also DF-WARPFS-22). Nothing reads `manifest.plugins`, and
`hilo-fuse`/`hilo-triggers` reference no plugin code — so declared hooks can
never dispatch, and the loaded runtime dies when the CLI exits.

**The error I hit.** Following the documented loop — module into
`.vfs/plugins/`, then `hilo plugin load <path>` — loads successfully
(`loaded plugin: victim`, `persisted to: …`, exit 0) and silently leaves a
**0-byte file**: `std::fs::copy(path, dest)` with source inside the
destination directory copies the file onto itself, and std::fs::copy opens
the destination with O_TRUNC before the read. The next `plugin list` drops
the empty file (header check on 0 bytes fails, correctly). Loading the same
bytes from outside `.vfs/plugins` works — the only reason every prior probe
(run 9's text file, the 22 fix's tests) missed it: they all loaded from
outside.

**The right way (until fixed).** Keep module sources outside `.vfs/plugins`
(`plugins-src/` or anywhere else) and `hilo plugin load` them from there;
the persist step then copies a real file and `plugin list` sees it. Never
load a file that already lives in `.vfs/plugins/` — DF-WARPFS-58.

**Permissions re-test after DF-31.** The honest status note lives only in
`docs/hilo-permissions.md:5-21`; the spec's "Single Source of Truth" §4
still shows a `permissions:` block with `mode: 0444` examples, and a
re-probe this run confirms a `mode: 0600` rule on `src/**` has no effect
(file serves at the 0644 default through the mount). Residual honesty gap
filed as DF-WARPFS-60: the disclosure needs to reach README/spec, not just
the crate page.

**The lesson.** Run 9 tested load with garbage and found honesty bugs; run
17 tested load with a *valid* plugin from the *documented* location and
found data loss. Whenever a fix adds a write path, dogfood it at the
location the docs' own examples use.

## Run 18 — 2026-09-25 (warpfs-dogfood, cross-surface session): the caches are the graph, and only the cold-start knows it

**How the pieces fit.** Hilo's graph state lives in three places that must
agree: `.vfs/graph/edges.jsonl` (the append-only INVENTORY — declared
source of truth, git-tracked), `.vfs/graph/.parse_cache.json` (per-file
parse state, keyed by content hash, used to skip unchanged files), and
`.vfs/graph/graph.db` (the DuckDB query cache, rebuilt from the inventory
"at mount/query time" per the design rules). Every graph answer a user
gets — `stats`, `related`, `impact`, and every MCP `vfs_graph_*` tool —
serves from graph.db. The trigger engine (inotify → debounce → re-parse
changed file → APPEND new edges to edges.jsonl → update graph.db) is what
keeps all three in sync between explicit warms.

**Why it breaks only at cold-start.** Run 18 drove the surfaces
together on a fresh 4-file project and found the sync is one-way per
event: the daemon's incremental update rewrites graph.db from its own
partial view of the event rather than merging against the inventory, so a
write landing inside the daemon's cold-start reconcile window leaves
graph.db persistently EMPTY-ish (2 distinct edges vs 5 in the JSONL) while
edges.jsonl stays correct. The user-visible symptom is brutal precisely
because the design is otherwise honest: `graph impact` answers from the
wrong cache silently (missing dependents, rc=0), and the documented
recovery, `graph warm`, is defeated by the parse cache — warm sees
"unchanged" files, skips parsing, and exits 0 having written an empty
graph.db. The caches, not the inventory, became the graph. Recovery needs
`rm graph.db .parse_cache.json` — a two-file ritual no doc mentions
(DF-WARPFS-61 P0, with DF-WARPFS-64 P2 covering the missing-db no-rebuild
case). The window also surfaced two smaller faces of the same root:
readers can catch a raw DuckDB lock-conflict error and an empty-graph read
in the seconds before the daemon settles (the DF-WARPFS-30 steady-state
lock fix does not cover startup), and every trigger re-parse re-appends
the file's FULL edge set — `client.rs → pkg:parser` was in edges.jsonl
×3 after three real edits — inflating the git-tracked inventory
(DF-WARPFS-62 P2).

**The fresh-box face.** On the bunker install leg (Debian 13, release
build 24m19s), `hilo mount <project>/mt --daemon` — mount point INSIDE the
served tree, the natural way to test hilo on hilo's own repo — daemonized,
mounted, and then hung on every read (ls/getfattr/cat all block). The
identical inner-mount works flawlessly on the dev host, so the hang is
environment-dependent (DF-WARPFS-63 P1). Same lesson as run 7, now
twice-proven: install-green ≠ mount-works on a stock box; only a real
read on the ephemeral machine sees it. Also new from this leg: `/tmp`
smoke dirs on a bunker box carry uid-residue from previous agents' users
(init failed with EACCES until the smoke moved under `$HOME`), and the
`--daemon` flag discards the tracing subscriber stderr, so a hung mount
gives the user zero diagnostics.

**The right way, until fixed.** (1) After starting a `--triggers` mount,
run `hilo graph warm` ONCE and then sanity-check
`hilo graph stats` against `wc -l`/distinct-count of edges.jsonl; if they
disagree, `rm .vfs/graph/graph.db .vfs/graph/.parse_cache.json && hilo
graph warm` before trusting any impact answer. (2) Never treat `warm`'s
"graph unchanged" as proof the db is populated — it only means the parse
cache skipped. (3) Mount OUTSIDE the project tree on stock boxes until
DF-WARPFS-63 has a diagnosis. (4) The regression test that would pin all
of this: warm → start trigger mount → edit immediately → poll stats 10×
→ assert every poll equals the distinct edge count of edges.jsonl.

## Run 19 — 2026-09-25 (warpfs-dogfood): the backend sync suite's first real remote

**How the sync stack is built, and why run 19 found what it found.** The GAP-55 sync engine
is three layers: (1) a driver per backend kind (native S3 = aws-sdk-s1 wrapped in a
hand-rolled current-thread tokio Runtime so the sync API can stay blocking; external tools =
rclone et al. invoked as subprocesses), (2) a planner (`planner.rs`) that walks local keys and
remote keys, compares sizes + mtimes, and resolves every difference by last-writer-wins with
the §13.9 tie-breaks (equal → push-on-Push/pull-on-Pull/no-op-on-Both; deletes push-only),
and (3) `execute_sync` which performs transfers, aligns mtimes after each one (the ping-pong
guard), and appends anything "resolved" to `.vfs/sync/conflicts.jsonl`. The §7.1 trigger hook
(`hilo-triggers/src/sync_hook.rs`) re-uses the planner incrementally: inotify events collect
into a dirty set, a settle task flushes by running filtered plans per mount.

**Error 1: the opaque 400 (DF-WARPFS-65).** `endpoint_client()` in s3.rs builds the S3 client
for explicit-endpoint mounts with credentials read from exactly two env vars. Every other AWS
auth path (shared credentials file / `AWS_PROFILE`, SSO, configs) silently degrades to
empty-string credentials — the request goes out signed by nothing and Hetzner answers 400
InvalidArgument with no body, which the SDK renders as `service error` with no code. The
reason it took a cross-check to localize: `backend setup` looks at the whole ambient AWS chain
and happily reports "credentials: found", so the tool told me twice that auth was fine while
the client disagreed. The boto3 probe (same endpoint, same profile creds, list + upload OK)
isolated the defect to hilo's client construction in one step — the shim technique of the
run-20 bunker lesson, applied to a network client.

**Error 2: the runtime-in-runtime panic (DF-WARPFS-66).** `SyncHook::new` is called from
mount setup; with a backend mount present it constructs the S3Driver, whose constructor does
`Runtime::new()` + `block_on()`. On a thread already driving an async runtime tokio panics.
Because the panic happens on a spawned thread, the mount survives, prints "triggers enabled,
13 active", and the sync hook enablement is simply never reached — no error, no line, just a
stack trace the user gets no help interpreting. The lesson generalizes the run-18 cold-start
finding: the danger zone of this codebase is not any single subsystem but the seams where a
blocking API meets an async context — every new call site that touches a *Driver from
non-test code needs a "which thread is this?" check.

**Error 3 and 4: the planner's honesty leaks (67, 68).** LWW resolution is the planner's
normal mode of operation, but `record_conflict` is called on the resolution path, so a
workspace mirroring a remote that never diverged still accumulates "conflicts". And `--pull`
walks the union of keys, transferring local-only files too, so its transfer count is
unreconcilable against the remote listing. Both are meaning-erosion bugs rather than data
bugs: the sync gets the right bytes where they belong, but its own reporting can no longer be
trusted to describe what it did.

**The right way to dogfood this suite:** mount against a REAL store with a dedicated prefix
(real stores surface credential, checksum and region behavior that MinIO fixtures don't);
cross-check failures with a second client (boto3) before blaming the network; verify §7.1 by
grep-counting its enablement line, not by trusting "triggers enabled, N active"; and read
conflicts.jsonl after every sync — a ledger that grows during a no-conflict session is the
finding.

## Run 20 — 2026-09-25 (coding-hermes-tools-dogfood, Python FFI × live backend): the embedder's view is finally real, and the ledger lies faster than before

Full report: docs/dogfood/2026-09-25-run20-python-ffi-backend.md. Angle: the
one surface combination no run had touched — the Python UniFFI consumer driven
against a project with a LIVE MinIO backend, with CLI cross-checks on every
answer.

**How the surfaces were made to agree.** The backend was stood up from the
repo's own docker-compose.yml (MinIO RELEASE.2025-09-07, bucket created with
`mc` inside the container — an independent client kept as ground truth for
every sync claim). The mount was created purely through the documented CLI
(`backend mount --type s3 --bucket hilo-run20 --endpoint http://127.0.0.1:9000
--at s3data`), then the FFI was asked the question run 8 proved unanswerable:
`vfs_resolve_backend("s3data/src/lib.rs")`. Answer came back
`backend=s3, remote_url=s3://hilo-run20/, cached=true` — the constants era is
verifiably dead (DF-WARPFS-12-era rewrites did the work; nobody had checked).

**DF-WARPFS-55's CWD fix, re-proven from a second language.** Run 16 proved
the fix from Go; run 20 proved it from Python: embedding process at /tmp,
`HiloHandle(absolute_root)`, stats and impact identical to the CLI at the repo
root. The handle-root design (constructor takes the repo, no ambient CWD) is
what makes both proofs trivially possible — the earlier raw-path namespace
functions (DF-WARPFS-57) remain the awkward half.

**The pull→graph→FFI chain, and the two lies found in it.** A remote-only file
dropped into MinIO, `backend sync --pull`, `graph warm --changed`, then FFI
impact: the file joined the graph and dependents resolved correctly — backend
files are first-class graph citizens, the thing the broken §7.1 hook (DF-66)
prevents happening automatically. But the same pull printed "6 conflicts
recorded" on a workspace that had diverged NOWHERE (push → pull, seconds
apart, identical mtimes): the LWW planner keeps no sync state, so it re-litig
ates the full key set every sync and logs routine RemoteWins decisions as
conflicts (DF-WARPFS-69, P1). Run 19's DF-67 was 3 rows; the growth is
per-sync-full-set. The lesson stands but the diagnosis is sharper now: it is
not over-logging of edge cases, it is the ABSENCE of sync state.

**Metadata through a mount is a frozen snapshot (DF-WARPFS-70).** CLI sets
`user.vfs.role=library`; the mount started afterwards does not see it; writes
through the mount are refused (read-only). While a mount is up, the README's
headline surface is read-only and stale. Root cause shape: the daemon loads
its xattr view at startup instead of passing host xattrs through at getattr —
the same "state at startup" class as run 18's trigger cold-start bug.

**Fresh-box economics.** The FFI recipe (bindgen → debug build → rename →
import) ran from zero on stock Debian 13 in 985s including rustup minimal;
libfuse3-4 present-by-default claim VERIFIED. Two install attempts were burned
by las-03's /tmp residue (stale uid-1004 files → Permission-denied redirects
that masquerade as build failures — BUILD_RC=1 with 0 crates compiled), hence
DF-WARPFS-74: scratch under $HOME, always.
