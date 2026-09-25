# Dogfood Log

| Date | Project | Verdict | Run |
|---|---|---|---|
| 2026-08-13 | warpfs (Hilo) | 🟡 PROMISING-BUT-ROUGH | Deep real-use: full workflow on fresh ripgrep clone + MCP client + FUSE mount |

---

## 2026-08-13 — warpfs (Hilo) — 🟡 PROMISING-BUT-ROUGH

**Promise statement:** "An agent can answer structural questions about any codebase
(dependencies, entrypoints, test coverage, blast radius) by querying a pre-computed
metadata graph via CLI / MCP / FUSE — in <1s, without reading files."

**What was done (real use):** cloned `BurntSushi/ripgrep` (111 .rs files) into
`/tmp/dogfood-warpfs/ripgrep`, ran the documented workflow end-to-end:
`hilo init` (5ms) → `hilo graph warm` (1.2s, 256 edges) → `hilo classify` (0.18s) →
query battery (`graph stats/impact/related/search/untested/module/understand`,
`meta` set/read). Then connected a real MCP stdio client (JSON-RPC 2.0, the same
shape Claude Code/Hermes use) and drove 6 of the 15 `vfs_*` tools. Then
`hilo mount --daemon` + `ls`/`getfattr`/`cat` + clean unmount.

**Time-to-first-success:** ~2s to a working graph (init+warm+stats); first real
answer (`graph stats`) ~2 min from start of run including clone.

**Verdict evidence:**
- WORKS: init/warm/classify speed claims hold; xattr metadata round-trips
  (`meta --set` → `getfattr`); MCP protocol is clean (15 tools, structured JSON,
  no protocol errors); FUSE `--daemon` mount returns instantly, persists, unmounts
  cleanly.
- FAILS: the flagship file-level queries are structurally empty. 256/256 edges
  target `pkg:*` pseudo-nodes, **zero file→file edges**; `hilo graph impact <file>`
  (README Quickstart form) → "No dependents found", `related <file> --direction
  reverse` → "No incoming edges", MCP `vfs_graph_impact` → `{"dependents":[]}`.
  The `pkg:<name>` symbol form works (found 3/3 real importers of globset) but is
  undiscoverable. `graph untested` reported 82/82 files untested incl. test files
  (classify tagged 5 of 19 real test/bench files). `graph understand` extracts no
  symbols for symbol-rich files. 27 edges poisoned by `pkg:{` truncation.

**Top 3 findings (task IDs):**
1. GAP-034 (P0) — file-level impact/related queries always empty: no file→file edges, no pkg:→file resolution.
2. GAP-035 (P1) — `use crate::{a, b}` brace-groups truncated to `pkg:{` (27 edges).
3. GAP-036 (P1) — classify misses top-level `tests/`/`benches/`; untested = everything (82/82).

**Left behind:** docs/dogfood/2026-08-13-integration.md, docs/dogfood/diagnostics.md,
skills/hilo-usage/SKILL.md, tasks GAP-034..038, this log. Foreman woken 43200→900.

## 2026-08-23 — warpfs (Hilo) — 🟡 PROMISING-BUT-ROUGH (run 2, serde corpus)

**Promise:** agent-first VFS: pre-computed dependency graph + metadata so an
agent gets blast radius / deps / coverage without reading files.

**Reality:** materially improved since run 1 — every run-1 P0/P1 fix verified
live (file-level impact WORKS now, brace-group garbage gone, intra-crate
file→file edges, understand symbols, classify tests). But the headline blast
radius still answers 6/148 on serde (GAP-048 P0): pkg-resolution matches
exact targets only, missing brace-expanded member edges. classify role
metadata is wrong on crate roots (GAP-049). MCP logs to stdout (GAP-050).
tested_by edges never emitted (GAP-052). SKILL.md build command broken
(GAP-051). Plumbing (FUSE, MCP, xattr, hooks, determinism, speed) solid.

**Time-to-first-success:** ~3 min (init 30ms + warm 35s + classify 1.2s +
stats). First real answer (impact file): ~4 min. **Friction count:** 8
(6 gaps + stale ~/.cargo/bin/hilo Aug-15 binary + search pkg: hits unlabeled).

**Top 3 findings (task IDs):**
1. GAP-048 (P0) — impact under-counts 96%: 6/148 dependents on serde; resolution misses pkg:serde::* member edges.
2. GAP-049 (P1) — classify: crate roots role=unknown; build.rs as entrypoint; only tests trustworthy.
3. GAP-050 (P2) — MCP INFO log on stdout breaks naive JSON-RPC clients (framing violation).

**Left behind:** docs/dogfood/2026-08-23-integration.md, diagnostics.md
updated, SKILL.md field notes updated, tasks GAP-048..053, this log.
Foreman wake: PUT CooldownS 21600→900 (board has real work).

## 2026-09-11 — warpfs (Hilo) — ✅ SHIPPABLE (run 3, containerd corpus, 5489 Go files)

**Promise:** agent-first VFS: pre-computed dependency graph + metadata so an
agent gets blast radius / deps / coverage without reading files.

**Reality:** first run with no structural data break. PERF-005 (landed the
night before) verified in real use: vendor exclusion exact (1367/1367
non-vendor files parsed, 0/14070 vendor edges), HOME refusal fires with
--allow-home hint. Go blast radius via full-path pkg: form is EXACT —
126/126 vs grep ground truth for core/mount. Rust file-form impact works.
One real gap left: Go file→package resolution (GAP-057 P1) — file-form
queries on Go files silently empty while the pkg-form is exact.

**Time-to-first-success:** ~2 min warm (clone→init→warm 80s→first exact
impact); ~20 min cold machine (bunker install leg, build 18m52s RC=0,
smoke green). **Friction count:** 5 (GAP-057..061; zero blockers).

**Bunker install leg (las-bunker-03, agent 595d2c78):** PASS — clone
(public repo, no creds) → rustup → build 18m52s → init/warm/stats smoke
RC=0 in 3s → destroyed. README 15-20 min claim accurate; clang/cmake not
actually needed (benign docs drift, GAP-060).

**Top findings:** GAP-057 (P1, Go file→pkg resolution — file-form queries
empty though edge data is exact), GAP-058 (P2, warm exclusion silent —
looks like data loss), GAP-059 (P2, related empty-vs-unknown inconsistency).

**Left behind:** docs/dogfood/2026-09-11-integration.md,
docs/dogfood/diagnostics.md updated, SKILL.md run-3 field notes,
GAP-057..061 on the BOARD-V2 board. Foreman wake: warpfs cooldown
21600 → 900 (floor ridden in same PUT; verified tick fired — foreman
events on GAP-060/061 within one interval). Dogfood picker row itself
at 259200.

## 2026-09-12 — warpfs (Hilo) — ✅ SHIPPABLE (Rust/Go) / 🟡 Python (run 4, fastapi corpus, 1138 .py)

**Promise:** agent-first VFS: pre-computed dependency graph + metadata so an
agent gets blast radius / deps / coverage without reading files.

**Reality:** plumbing and Rust/Go paths re-verified clean; the one gap family
left is Python file→package resolution (GAP-057's Go-only fix not generalized):
file-form impact/related silently empty (0/10 vs grep truth) while pkg-form is
exact (10/10 + 7 out-of-scope importers, ast_exact conf=1.0). Coverage edges
(2225 tested_by) are emitted but untested/module don't consume them for
pkg-import languages. Warm exclusion still silent (183 __init__ + 20 parse
misses unreported). meta --set typo silently writes a garbage xattr key.

**Time-to-first-success:** init 16ms → warm 15.3s → first exact blast-radius
answer ~16s. **Friction count:** 6 (GAP-064..068 + GAP-062 re-confirmed via MCP).

**Verified clean (re-confirmed this run):** loud unknown-path errors on impact
AND related (GAP-059 live), PERF-004 instant .md guard, SIGPIPE quiet (GAP-063
appears fixed: `| head -1` rc=0 no panic), MCP 17 tools NDJSON + pure stdout
(GAP-050), FUSE mount/xattr/cat/unmount, classify roles, meta round-trips,
byte-deterministic stats, stats 28ms/impact 20ms at this scale.

**Top findings (task IDs):**
1. GAP-064 (P1) — Python file→pkg resolution missing; file-form queries empty though edge data exact.
2. GAP-065 (P2) — warm silent exclusions on Python: 183 __init__.py + 20 real parse misses, no report.
3. GAP-066 (P2) — tested_by collected but untested/module ignore pkg-targeted coverage edges (Tests: 0.0%).

**Left behind:** docs/dogfood/2026-09-12-integration.md, diagnostics.md run-4
sections, SKILL.md run-4 field notes, GAP-064..068 on the board (events 297-301).
Foreman wake: pending board check at run end (cooldown 43200 → 900 if rows land).

**Bunker install leg (las-bunker-03, agent 5aec2a70):** PASS — clone OK
(public repo) → rustup minimal → `cargo build --release` RC=0 (7m44s
incremental after a first-attempt 590s ssh timeout mid duckdb-sys; clean
build resumes from target/ and the first attempt's cargo kept running
headless, so the retry waited on the lock and finished it) →
`hilo 0.3.0` → init/warm/stats smoke on hilo itself: SMOKE_OK in 468s
total. Agent destroyed, bunker clean. Second consecutive box confirming
clang/CMake/libfuse3-dev unnecessary for the core path (GAP-068).

---

## 2026-09-13 — warpfs (Hilo) — 🟡 PROMISING-BUT-ROUGH (TS/JS) / ✅ SHIPPABLE (Rust/Go/Python) — run 5, vite corpus

**Promise statement:** "An agent can answer structural questions about any
codebase (dependencies, entrypoints, test coverage, blast radius) by querying
a pre-computed metadata graph via CLI / MCP / FUSE — in <1s, without reading
files."

**What was done (real use, run 5):** fresh release build at 6f0430b (21
commits past the run-4 binary; includes GAP-064/066/067/068 fixes). Corpus:
vitejs/vite @ main (tarball snapshot), first TS/JS language tested (prior:
Rust ×2, Go, Python). init → warm (13.3s, 908 files / 4168 edges) →
stats/impact/related/search/understand/classify → meta round-trips → FUSE
mount/xattr/cat/unmount → MCP battery (17 tools) → determinism +
incremental-warm checks → clean→warm recovery test. Plus the literal
briefing target: ran the warpfs-sync lane tick end-to-end (RC=0, 6/6 keys
verified, DuckBrain namespace event fresh).

**Verdict reasoning:** every subsystem behaves (speed claims hold, FUSE/MCP/
xattr round-trips clean, determinism byte-exact, the three run-4 fixes
verified live in real use) — but on TS/JS the flagship blast-radius question
returns a confident empty answer (GAP-069), a documented recovery path
strands the user (GAP-070), and coverage still reads 0.0% where test edges
exist (GAP-071). One language per run, continued: Rust → Go → Python → TS/JS.

**Time-to-first-success:** init 7ms → warm 13.3s → first exact blast-radius
answer (pkg: form) ≈14s. First *misleading* answer (file-form, looks like
success, wrong): also ≈14s — that is the GAP-069 hazard.

**Friction count:** 5 (GAP-069/070/071/072 + no `--force` for warm).

**Verified clean (re-confirmed or newly fixed this run):** GAP-058 exclusion
report live; GAP-067 rejection + canonical success line live; GAP-059 loud
unknown-path errors on impact AND related (correct even for my wrong path);
byte-deterministic stats across forced full rebuild (926-line output
identical); incremental warm reparsed exactly 1 touched file; FUSE daemon
mount/xattr/cat/unmount; MCP pure-JSON stdout, 17 tools; classify roles
sane (390 test files); stats 25ms / impact 18ms / search 20ms; sync lane
6/6 keys fresh.

**Top findings (task IDs):**
1. GAP-069 (P1) — TS/JS file→module resolution missing; file-form impact/related silently empty (24 incoming edges, 13 importers, grep truth 14).
2. GAP-070 (P1) — `graph clean` → `graph warm` leaves the graph empty (parse-cache short-circuit); the tool's own recovery instruction strands the user.
3. GAP-071 (P2) — `local:`-targeted tested_by edges unconsumed on TS/JS; module Tests: 0.0% with 8 spec files in-module (GAP-066 residual).

**Left behind:** docs/dogfood/2026-09-13-integration.md, diagnostics.md run-5
sections, skills/hilo-usage/SKILL.md (first in-repo usage skill), GAP-069..072
on the board (events 322-325), this log entry.

**Bunker install leg (las-bunker-03, agent da91c36c):** clone of public repo
at exact HEAD 6f0430b → rustup minimal 1.98.1 → `cargo build --release` RC=0
(1142s = 19m02s clean; README's 15-20 min claim accurate) → `hilo 0.3.0` +
init/warm/stats smoke on hilo itself. Third box confirming clang/CMake
unnecessary (GAP-068). Agent destroyed, bunker clean.

---

## 2026-09-20 (warpfs-dogfood tick 2026-09-20-00-06-24)

verdict: SHIPPABLE (PROMISING-BUT-ROUGH not warranted; two P2 UX/doc rows filed)
promise: agent gets a pre-built codebase map via xattr/CLI/MCP without file reads
consumer pass: scratch clone at HEAD a8a401d — init 9ms / warm 6.2s cold,
0.13s incremental (102/103 cache hits), impact+related+search+module+untested+
classify+xattr+MCP(17 tools) all worked; clean→warm rebuild healthy.
time-to-first-success: ~2 min (init+warm). Friction count: 2 (id-form UX,
README libfuse wording).
bunker install leg: PASSED — clone public repo a8a401d on fresh Debian 13,
rustup minimal, cargo build --release RC=0 1146s, smoke (version/init/warm/
stats) OK. agent 9d7da1d4 destroyed, key removed.
infra finding: 6 spawn 500s across las-03/04 from get.docker.com throughput
collapse + server-side kill (DF-WARPFS-3); CLI 0.1.4 (300s request budget)
built from ~/bunker and installed locally to unblock — 0.1.3 in PATH still
had the 30s CLI deadline (REUSED FIX, did not edit daemon).
rows: DF-WARPFS-1..4 appended to board (verified at tail, 164 lines).
left behind: docs/dogfood/2026-09-20-integration.md, this log entry.

---

## 2026-09-20 (warpfs-dogfood tick 2026-09-20-01-30-34) — run 7, FUSE mount deep dive

verdict: PROMISING-BUT-ROUGH (mount surface; CLI/graph surface still SHIPPABLE)
promise: agent mounts the repo as a virtual filesystem and answers structural
questions via getfattr/ls/cat — zero file reads.
angle: deliberately NOT the CLI/CAG pass that runs 1-6 covered. First run to
USE the mount as a user would.
consumer pass: fresh fixture repo + hilo's own repo mounted with --daemon.
VERIFIED WORKING: byte-exact size + sha256 fidelity disk↔mount; xattr
round-trip through the mount (hilo classify sets user.vfs.role, readable via
getfattr AND `hilo meta <mount-path>`); cat works; clean unmount via
fusermount3 -u with no leftover process; write attempted correctly refused (RO).
FOUND (4 rows): DF-WARPFS-5 empty-dir readdir never replies -> `find <mount>
-type f` = 0 rows and hangs (3/3 runs, 15s cap) vs 24 rows/111ms on disk;
DF-WARPFS-6 `hilo mount` EXIT=1 on stock Debian (auto_unmount forces
allow_other; /etc/fuse.conf default) — no CLI escape hatch;
DF-WARPFS-7 mount ignores the ignore stack (.git//.vfs//130GB target/ served
though `hilo ignore check target/` says ignored:true);
DF-WARPFS-8 fabricated mtimes (mtime = time of stat call).
time-to-first-success: never reached on the mount path (blocked twice);
friction count 3 blocking + 1 honesty defect + 2 doc gaps.
bunker install leg: PASSED — agent bd4b20a4 @ las-bunker-03, fresh Debian 13,
clone public repo b83a68f, rustup minimal, cargo build --release RC=0 1217s,
smoke (--version/--help/init/warm/stats) OK, MCP tools/list = 17. The SAME box
then failed `hilo mount` (DF-WARPFS-6) — install-green ≠ mount-works.
noted-not-filed: serve --help and SKILL.md say "15 tools" while tools/list
returns 17 (dev host + fresh bunker both) — folded into the DF-WARPFS-1..4
doc-drift family rather than a new row.
left behind: docs/dogfood/2026-09-20-run7-fuse-integration.md,
docs/dogfood/diagnostics.md Run 7 section, skills/hilo-usage/SKILL.md FUSE
section, rows DF-WARPFS-5..8 (events 431-435), this entry.

---

## 2026-09-20 (warpfs-dogfood tick 2026-09-20-04-54-10) — run 8, backend overlay + FFI bindings

verdict: PROMISING-BUT-ROUGH (backend-overlay + FFI surfaces; CLI/graph/FUSE unchanged)
promise: agent stores a workspace in a remote backend (S3/compat) and syncs it
two-way, and consumes Hilo's metadata + graph from Go/Python/Kotlin/Swift via
the UniFFI bindings.
angle: deliberately NOT the CLI/CAG pass (runs 1-6) and NOT the FUSE mount
(run 7). First run on either the backend overlay or the FFI bindings — run 7
explicitly left `.vfs/backends/mounts.yaml` unpopulated.
consumer pass: live S3-compatible endpoint (moto, 127.0.0.1:19001) + AWS CLI as
independent ground truth, scratch corpus with a real ignore case; then
`cargo build -p hilo_ffi` + `nm -D` on the produced .so; then a real stdio MCP
session driving vfs_backend_status / vfs_sync_backend / vfs_resolve_path.
VERIFIED WORKING: `workspace sync` two-way + byte-exact (3 MB binary sha256
matched local vs the object pulled back out of the bucket); remote-only object
pulled down; `.hiloignore` honoured (target/junk.o never uploaded; `workspace
ephemeral` agrees); `.vfs/` and the ignore file never transferred; `--dry-run`
printed exactly what the real run then uploaded; idempotent re-run (0/0/2
unchanged, later 6 unchanged); nested + unicode paths correct; last-writer-wins
by mtime confirmed in BOTH directions as documented; `backend setup` a useful
diagnostic; mount/sync failures fail fast with legible errors and exit 4 (missing
tool) / 2 (bad mode) and leave no partial state; FFI crate builds and really
exports the UniFFI ABI (8 `uniffi_hilo_ffi_fn_func_*` + checksums + META_UDL).
FOUND (9 rows, events 446-454): DF-WARPFS-9 (P0) the PLAIN documented `backend
mount` prints 'mounted s3://...' + exit 0 and registers NOTHING (legacy arm is a
print statement; the tool's own `setup` next-steps line recommends that form) —
adding any spec-9 flag makes the identical command write mounts.yaml;
DF-WARPFS-10 (P0) `backend sync` ALWAYS fails on the first push to a real
endpoint — its own plan says '1 to transfer' then 'aws sdk error: service error',
because S3Client::head_object_meta only maps an error to NotFound by
string-matching the Display text; DISCRIMINATOR RUN: pre-create the remote key so
the HEAD returns 200 and the identical command succeeds (1 transferred, exit 0);
DF-WARPFS-11 (P1) `backend list` can never see a mount — mount writes
.vfs/backends/mounts.yaml, list reads .vfs/manifest.yaml ('No backends configured
in manifest.' after a verified mount); DF-WARPFS-12 (P1) two sync engines with
different flags (workspace sync: no --pull/--push, needs no mount, works on a
fresh bucket; backend sync: opposite on all three) AND with AWS_ENDPOINT_URL
unset backend sync silently planned against the REAL ambient ~/.aws/config
endpoint while never telling the user which store it resolved; DF-WARPFS-13 (P1)
the S3 integration suite reports '7 passed' in 0.05s WITH a live endpoint
answering — the readiness gate requires MinIO's own /minio/health/live and a
returned-early skip is scored ok, i.e. phantom coverage for exactly the surface
DF-WARPFS-10 breaks; DF-WARPFS-14 (P1) every FFI graph function resolves
.vfs/graph/graph.db from the EMBEDDING PROCESS CWD and vfs_graph_related's path
argument never participates in finding the graph; DF-WARPFS-15 (P1)
vfs_resolve_backend and the MCP vfs_backend_status/vfs_sync_backend/
vfs_resolve_path return hardcoded constants — measured 'backend:local',
'last_synced:synced', 'synced_files:1' over MCP with an S3 backend mounted;
DF-WARPFS-16 (P2) the documented binding workflow exits 127 with no in-repo
generator and no doc saying how to install one; DF-WARPFS-17 (P2) MCP
initialize advertises serverInfo.version 0.2.0 while the workspace/CLI are 0.3.0.
not-filed-by-design: the conflict test is NOT a defect — last-writer-wins is what
the docs promise and both directions follow it (recorded in diagnostics.md so a
future run does not file it as data loss).
time-to-first-success: ~2 min (moto up + bucket + corpus + first dry-run/real
sync); friction count 5 blocking-or-honesty (9/10/11/13/15) + 4 friction/docs.
bunker install leg: PASSED — agent d6c6fc9a @ las-bunker-03, fresh Debian 13,
public clone at daaf1e7 (no credential minted, no visibility change), rustup
minimal 1.98.1, `cargo build --release` RC=0 in 2025s (33m45s — OVER the README's
15-20 min claim), smoke (--version 0.3.0 / init / warm 3s 994 edges / stats /
classify / MCP tools/list = 17) all OK. The P0 mount defects were then
reproduced on that SAME fresh box (plain mount -> 'mounted s3://some-bucket/' +
empty .vfs/backends/; same command + --tool native -> 156-byte mounts.yaml) and
`backend list` stayed blind in both cases. Agent destroyed, key removed.
left behind: docs/dogfood/2026-09-20-run8-backends-ffi-integration.md,
docs/dogfood/diagnostics.md Run 8 section, skills/hilo-usage/SKILL.md
backends+FFI section, rows DF-WARPFS-9..17 (events 446-454) + the audit event
455, this entry.

---

## 2026-09-22 (warpfs-dogfood tick 2026-09-22-19-02-14) — run 9, git backend + plugin surface at v0.3.0

verdict: 🔴 DOES-NOT-DELIVER (for the backend surface the CLI advertises; CLI/graph/MCP core remains shippable per runs 1–8)
promise: angle chosen by the skill's stale-surface rule — runs 1-8 covered
CLI/graph/MCP/FUSE/S3-FFI but never the git backend door the help text
advertises, nor `hilo plugin`. HEAD tested: 917a991 (v0.3.0, release build).
consumer pass: scratch workspace + bare remote (file://) — `backend mount
--type git` (documented form) exits 0 printing `mounted git … at code
(worktree code)` and persists NOTHING (no dir, no mounts.yaml, no error);
with `--tool native` the validator rejects: `unknown backend type: git
(expected s3|gdrive|onedrive|dropbox|external)`. `--type local` same. S3
control probe: default form also persists nothing (silent success); only
`--tool native` persists, `backend list` still blind (DF-WARPFS-11 live),
first sync fails opaque `aws sdk error: s3: aws error: service error`
(DF-WARPFS-10 live). `hilo plugin load` accepted a 9-byte TEXT file and
printed fabricated metadata (hooks:1) while creating nothing. `serve --mcp`
starts outside any project contrary to cli-reference and serves 17 tools
over an empty graph.
time-to-first-success: NEVER on the run-9 surface (git mount dead end);
~4 min on the graph side (init+warm+stats on fresh corpora, all healthy).
friction count: 6 blocking-or-honesty (19/20/21/22/24 + serve-refusal false)
friction count: +docs family (DF-WARPFS-23).
closed-not-done audit: DF-WARPFS-9/10/11/17 marked complete reproduce LIVE
at 917a991 — DF-WARPFS-21 files the audit + a read-back closure gate.
perf (Step 2b, hyperfine/usr-time, release build): stats 30ms ±6.6,
impact 17ms ±1.9 (4-file corpus, n=20); warm 3.48s / peak RSS 211MB and
stats 47MB peak on hilo's own 107-file tree (GAP-092..095 ceilings hold in
real use); binary startup 5.2ms despite 1.4GB binary. NO PERF ROW — nothing
slow enough to feel; the backend headline op is broken, not slow.
install leg: bunker-qa battery RAN on agent 043574ed @ bunker-las-02 (launch
19:09:36Z, collected+destroyed same session; 17 evidence rows). NOT a silent
pass, NOT a full pass: fresh-install = ENV-BLOCKED (bare las-02 agent image
has no pkg-config/libssl headers, agent is non-root so the README's documented
`sudo apt install ...` line cannot run; openssl-sys build died in build
script — graded ENV-BLOCKED by the harness, a server-image variance vs the
las-03 image where the 09-20/09-11 legs passed) — the README's install path
was NOT exercised end-to-end on a bare box this run; ci-pass OK (act 1 job
green: workspace build + clippy + tests inside a provisioned container);
chaos-shutdown/disconnect OK; upgrade UNVERIFIED (tar-sync carries no git
history, harness limitation); chaos-resource ENV-BLOCKED; docker-deploy
compose up OK but probe 000. Details → DF-WARPFS-25.
rows: DF-WARPFS-19..24 appended + verified (board 179 rows, 0 bad lines,
events 494-499).
left behind: docs/dogfood/2026-09-22-run9-git-backend-plugins.md,
docs/dogfood/diagnostics.md Run 9 section, skills/hilo-usage/SKILL.md
run-9 section, rows, this entry.

---

## 2026-09-23 (coding-hermes-tools-dogfood tick 2026-09-23-07-09-38) — run 10, triggers (living map) + permissions at v0.3.0-20-g8832da0

verdict: 🔴 DOES-NOT-DELIVER (for the triggers surface the flag advertises;
⚪ UNKNOWN-VALUE for permissions — no observable user surface; CLI/graph/
MCP/FUSE read paths remain shippable per runs 1–8, re-verified healthy)
promise: angle chosen by the stale-surface rule — runs 1-9 covered
CLI/graph/MCP/FUSE/backends/FFI/plugins/git-backend but never hilo-triggers
(the "every file save triggers parse-and-diff" living map, door:
`hilo mount --triggers`) nor hilo-permissions (manifest rules, "enforced by
FUSE and MCP"). HEAD tested: 8832da0 (v0.3.0-20, local release 0.3.1-dev).
consumer pass: fresh fd @ 9e8927e corpus (repo never used before);
init+warm healthy (268 edges/23 files). Triggers: stock manifest →
`--triggers` loads 0 triggers (banner says so, stdout still says "enabled";
DF-WARPFS-28 — `hilo init` writes `triggers: []`, empty list suppresses the
9 defaults). Workaround (delete the key) → defaults load, but parse-and-diff
fires ONLY for project-ROOT files: `src/cli.rs` write 3× silent for 30s,
root canary with imports appends 2 edges in 0.51s (DF-WARPFS-29, P0 —
FileEvent.path is the bare inotify name, engine.rs:195; the watch-dir
reconstruction at 203-209 never reaches the event; all failure branches
info!() under a binary with NO log subscriber). Every trigger mount also
holds the graph.db DuckDB lock: `graph stats` hard-fails while a mount is
up, and a second mount's engine silently loses the lock and runs with
impact disabled (DF-WARPFS-30). Permissions: manifest rules wired nowhere —
FUSE engine built from hardcoded default_protections() (ops.rs:113), mount
read-only by design, hilo-mcp has zero permission code despite the doc
claim (DF-WARPFS-31).
time-to-first-success: NEVER on the documented trigger workflow; the only
firing config (defaults + root-level file) is undiscoverable.
friction count: 4 (DF-WARPFS-28/29/30/31).
perf (Step 2b, hyperfine/usr-time, release build): stats 23.5ms ±2.5 warm
(n=20) / ~30ms cold; mount live 0.28s (daemon return 11ms); parse-and-diff
fires in 0.51s when it fires. NO PERF ROW — nothing slow enough to feel;
the waits were broken functionality, not latency.
install leg: bunker-qa launch OK (agent a8164048 @ bunker-las-02) →
collect OK 17 rows, agent destroyed. NOT a silent pass, NOT a full pass:
fresh-install = INFO ENV-BLOCKED (bare las-02 agent lacks pkg-config, build
died in a *-sys build script, agent non-root — RECURRENCE of DF-WARPFS-25's
server-image variance vs las-03, where the 09-20 leg passed); ci-pass OK
(act 1 job green in a provisioned container); chaos-disconnect/shutdown OK;
upgrade UNVERIFIED (tar-sync carries no git history, harness limitation);
chaos-resource ENV-BLOCKED; docker-deploy compose up OK but probe 000
(same as run 9). Installability on a bare box NOT proven this run; headline
feature NOT exercised on the ephemeral box. Evidence:
/tmp/bunker-qa-evidence-wf10.jsonl + board event 645.
rows: DF-WARPFS-28..31 appended + read-back verified (board 193 rows,
0 bad task lines; events 643-644; a sibling compaction of events.jsonl
(101 blanks + 1 dup) landed concurrently — verified 0 unique history lost).
left behind: docs/dogfood/2026-09-23-run10-triggers-permissions.md,
docs/dogfood/diagnostics.md Run 10 section, skills/hilo-usage/SKILL.md
run-10 section (incl. the root-canary liveness probe), rows, this entry.

---

## 2026-09-23 (warpfs-dogfood tick 2026-09-23-12-43-28) — run 11, Java corpus + concurrency + first FFI consumer

verdict: 🟡 PROMISING-BUT-ROUGH for the Java surface (pkg-form blast radius
EXACT 112/112; file-form silently empty and `stats` calls the same class an
orphan) / 🔴 DOES-NOT-DELIVER for concurrent access (7 of 8 parallel graph
commands exit 1; two MCP servers on one repo → the second agent's
vfs_graph_stats returns -32603 lock error) / ✅ the FFI consumer path WORKS
from Python (5/5 calls) once an undocumented library filename is fixed.
angle: chosen by the stale-surface rule — runs 1-10 covered CLI/graph (1-6),
FUSE (7), S3 backends + FFI ABI export (8), git backend + plugins (9),
triggers + permissions (10). This run took (a) the first **Java** corpus in
11 runs (docs/graph-engine.md advertises 26 languages; only Rust x2, Go,
Python, TS/JS had been exercised), (b) **concurrency** — the real working mode
of an "agent-first" tool, and (c) the **FFI consumer** path run 8 left at
ABI-export only (`nm -D`).
HEAD tested: 740f0d9 (v0.3.0-28). Binaries: release CLI `hilo 0.3.1-dev`
(build stamp v0.3.0-14-g2b26d3f, 2026-09-22) + `target/debug/libhilo_ffi.so`
built this run with the documented `cargo build -p hilo_ffi` (8s, warm cache).
Corpus: google/gson depth-1 clone — 264 .java files, 243 with edges.
consumer pass: init 12ms → warm cold 12.0s (4418 edges) → incremental 29ms;
`graph impact pkg:com.google.gson.Gson` → 112/112 == grep truth; MCP over
stdio (initialize 0.3.1-dev, vfs_graph_impact 112, vfs_graph_stats live); the
post-commit hook `hilo init` installs really fires `graph warm --changed` on
commit and appends the new edge (4418 → 4419) on Java; `meta --set/--value` +
getfattr byte-exact round-trip; plain FUSE `--daemon` mount + `fusermount3 -u`
clean (process exits in <4s).
FOUND (9 rows, DF-WARPFS-32..40): DF-32 (P1) a `--triggers` mount LEAKS after
unmount — daemon alive and graph.db locked >=26s until killed (2/2 runs),
while a plain mount exits in <4s; DF-33 (P1) every concurrent read fails (7/8
CLI; 2nd MCP server gets -32603) because each command opens graph.db
read-write (hilo-graph/src/graph.rs:1080); DF-34 (P1) Java file-form id
resolution missing — "No dependents found" for a class with 112 importers
while the pkg form is exact, and `stats` lists that class as an ORPHAN with
194 edges targeting it; DF-35 (P2) `graph understand` on Java emits decorator
noise and omits the defining file (Gson.java:186 for a serializeNulls
question); DF-36 (P2) classify puts public API classes in role=unknown and
metrics benchmarks in entrypoint; DF-37 (P2) 1744 tested_by edges yet
`graph module` prints Tests: 0.0%; DF-38 (P2) `hilo init` installs no
.gitignore entries so the first commit adds a 1.3MB graph.db + parse caches
while docs/inventory-policy.md reads as if it does; DF-39 (P2) coverage
arithmetic contradicts itself — `module` 0.0% vs `untested`/FFI 58.02%, and
0 of 1744 tested_by edges target a file (the number is emit-derived, not
per-file coverage); DF-40 (P2) the documented FFI workflow stops one step
short — bindings load `libuniffi_hilo.so` while cargo builds `libhilo_ffi.so`,
and no doc names it.
re-check: DF-WARPFS-28/30 re-confirmed live at this binary (no duplicate rows
filed). DF-WARPFS-29 (fixed 2bfae82) NOT re-tested — the local CLI predates
the fix and the tick window had no spare rebuild.
perf (Step 2b, hyperfine, release build, warm cache): stats 19.8ms ±1.8
(n=20), impact 34.5ms ±2.1 (n=20), understand 78.4ms ±3.1 (n=10), warm cold
12.0s / 264 files, incremental 29ms, MCP round trip 104ms. NO PERF ROW —
nothing slow enough that a user would notice; this run's waits were failures
(locked DB, empty answers, a 45min release build I chose), not product latency.
install leg: bunker-qa battery RAN on agent 8b0362e6 @ bunker-las-02 (launch
12:59:56Z, collect + destroy same session, 17 evidence rows). NOT a silent
pass, NOT a full pass: fresh-install = INFO ENV-BLOCKED — the bare las-02 agent
has no pkg-config and no sudo, and the build died in openssl-sys v0.9.117's
build script. THIRD consecutive occurrence of exactly that cell outcome (runs
9, 10, 11), already filed as DF-WARPFS-25 (pending) — recorded as recurrence,
not re-filed. Installability on a bare box was therefore NOT proven this run
either; on that image the README's own `sudo apt install ...` line cannot run.
ci-pass OK (act: 1 job green rc=0); chaos-disconnect OK (fail-fast rc=101);
chaos-shutdown OK (SIGTERM+SIGKILL recovery clean); upgrade FAIL/UNVERIFIED
(synced tree carries no git history — harness limitation, battery grades it
untested); docker-deploy INFO (compose up OK, probe 000, as runs 9/10);
chaos-resource INFO ENV-BLOCKED (same openssl-sys cause, 3G cap never
exercised); chaos-corruption N/A (no db/state files); chaos-errorpath INFO
(no start command detected).
ffi leg: CONSUMER EXERCISED for the first time. Bindings generation works with
the documented form (74496-byte hilo.py, docs' own `test -f` passes);
`cargo build -p hilo_ffi` (documented, debug) → libhilo_ffi.so in 8s with a
warm target/; `import hilo` then failed with `OSError: libuniffi_hilo.so:
cannot open shared object file` until the library was copied under that name.
After the rename all five calls succeeded against the gson graph:
vfs_get_metadata → 'dogfood-run11' (round-trip of an xattr written by the CLI
— cross-surface proof), HiloHandle(repo), vfs_graph_stats (243 files / 4419
edges / tested_pct 58.02), vfs_graph_impact → 112 (== grep), vfs_graph_related
→ 29 edges, vfs_list_directory → 21 entries. Barrier filed as DF-WARPFS-40.
rows: DF-WARPFS-32..40 appended + read-back verified (board 193 → 202 rows,
0 bad task lines, no duplicate ids; events 646 → 648, ids 647 and 648).
  note: events.jsonl carries 102 pre-existing blank lines and two pre-existing
  duplicate ids (null, 2) from a sibling compaction — NOT introduced by this
  run (census before/after: this run added exactly two lines, ids 647/648).
left behind: docs/dogfood/2026-09-23-run11-java-concurrency.md,
docs/dogfood/diagnostics.md Run 11 section, skills/hilo-usage/SKILL.md run-11
section (Java dialect row, concurrency + triggers-teardown warnings, .gitignore
note), rows, this entry.

## 2026-09-23 — run 12 — MCP server surface — 🟡 PROMISING-BUT-ROUGH

**Angle:** `hilo serve --mcp` (0.3.x, 17 tools) driven by a raw NDJSON
JSON-RPC 2.0 stdio client — the shape Claude Code/Hermes use — against a
scratch copy of hermes-canopy (666-file graph). First run to connect a real
MCP client to the 0.3.x server; run 1 drove 6 of the 0.2.x-era 15 tools.

**Promise vs reality:** "an agent can answer structural questions about a
codebase it has never seen, through 17 vfs_* tools, without reading files."
Holds for the graph tools: impact 27/27 exact (== CLI), search top hit = the
real compiler file, understand anchored on 8 real card files, stats == CLI
byte-for-byte, xattr writes persist across server restart, errors are
informative (all id forms listed), stdout pure JSON-RPC across 6 sessions,
serverInfo honest (0.3.1-dev). BUT the orientation tool
`vfs_list_directory` silently returns {"entries":[],"total":0} on 7/7 real
populated directories (rc=0, no error) — the "0 results and no error" hang
class; vfs_workspace_ephemeral enumerates the same tree correctly, so the
fix is in reach. Also: stats "Total files: 666" vs warm coverage "705
files" on the same tree (39-file gap, DF-WARPFS-43); docs still document
the fixed GAP-050 stdout-INFO trap as live (DF-WARPFS-42).

**Time-to-first-success:** ~2 min (handshake + tools/list + first real
impact answer). Friction: 1 (my own wrong arg name — schema-checked,
actionable error, not filed).

**Perf (hyperfine, release build, warm, n=20):** `graph stats` 19.1ms ±0.7,
`graph impact` depth-3 39.4ms ±2.7; MCP battery (spawn+8 tools) 0.97–2.02s
end-to-end; graph warm cold 9.06s / warm 0.09s (6593 edges, 659 files).
NO PERF ROW — nothing slow enough that a user would notice; the numbers
beat the README's own claims.

**install leg:** bunker-qa battery on agent 87de5bfd @ bunker-las-02
(launch 14:10Z, collect + destroy same session, agent verified gone).
fresh-install = INFO ENV-BLOCKED 4th consecutive run (bare image lacks
pkg-config, no sudo; build died in a *-sys script) — README's own
`sudo apt install` path unrunnable on the QA image. Re-filed with fix
options as DF-WARPFS-44 (root cause = DF-WARPFS-25). toolchain-bootstrap
cells all OK (rust stable, zig cc, make, compose+buildx plugins).

rows: DF-WARPFS-41..44 appended + read-back verified (board 202 → 206 rows,
0 bad lines, each id exactly once). left behind:
docs/dogfood/2026-09-23-run12-mcp-server.md,
docs/dogfood/diagnostics.md Run 12 section, skills/hilo-usage/SKILL.md
run-12 section (path/task arg names, list_directory distrust, stats-vs-warm
count), rows, this entry.

---

## 2026-09-24 (coding-hermes-tools-dogfood tick 2026-09-24-15-28-40) — run 13, public docs site + fix re-verification at ef4748d

verdict: 🟡 PROMISING-BUT-ROUGH — the product core is the best it has been in
13 runs (six fix families verified FIXED by real use; perf in spec), but the
public front door is broken (5/5 guide links 404 + a lying dashboard) and the
trigger engine has one more silent-empty hole (.tsx/.jsx never fire).
promise: angle by the stale-surface rule — no run had ever visited
https://gethilo.github.io/hilo/ as a first-touch user, and 4 defect
families closed 09-23/24 without a real-use re-verification. HEAD ef4748d
(local release 0.3.1-dev).
consumer pass: (a) docs site — landing 200/redeployed today, but all five
Guides links 404 (links omit .md; Pages serves raw docs/, no generator);
dashboard.html is a frozen v0.2 snapshot ("Generated 2026-07-12", 15 tools
vs 17 real, stale gates/commits) deployed live; (b) fix re-verification on
a scratch canopy copy (709 files, 6661 edges, release build) — DF-41
list_directory returns 15/11 real entries + explicit errors (FIXED);
DF-33 two simultaneous MCP servers both answer + 8/8 parallel CLI (FIXED);
DF-30 stats-works-during-triggers-mount (FIXED); DF-32 daemon self-exits
~1s after fusermount3 -u, no manual kill (FIXED); DF-28 defaults load from
init's `triggers: []` (FIXED); DF-29 nested new-file writes fire ~0.5s
(FIXED for new files); DF-38 managed .gitignore block installs, pre-existing
content preserved (FIXED). NEW: every .tsx write (3 edits to existing
App.tsx, 2 new .tsx files, 1 edit to a fresh copy) fired 0 edges across
30s waits while .ts/.rs edits fired instantly → default_triggers()
(mount.rs:502) watches 9 extensions, no *.tsx/*.jsx; engine parses tsx fine.
time-to-first-success: ~1 min on the product (init+warm+stats); the DOCS
path time-to-first-success is effectively NEVER (five dead links).
perf (hyperfine, release, warm, n=20): stats 33.1ms ±3.2, impact d3
26.8ms ±4.5; cold warm 17.2s/709 files. NO PERF ROW — nothing a user
would notice; matches README claims and runs 9-12.
install leg: bunker-las-02 bunkerd DOWN this tick (activating; spawn
connection refused ×2 at 15:33Z) → qa battery could not run (explicitly
recorded, NOT a silent pass). Manual skill procedure executed on
bunker-las-03 instead (the skill's designated build host): fresh agent
4f10f656, public clone at ef4748d (== HEAD), rustup minimal 1.98.1
(needed a clean RUSTUP_HOME — the stock -y install hit os error 39 rename
races; second wrinkle: /tmp is shared per-IP and owned by dead UIDs so
/tmp writes from my first command landed on 2026-09-19 files),
`cargo build --release` in flight at log time; smoke + destroy to follow.
rows: DF-WARPFS-45..47 appended + read-back verified (board 207 → 210 rows,
0 bad lines, each id exactly once, git diff --numstat == 3).
left behind: docs/dogfood/2026-09-24-run13-docs-site-fix-reverify.md,
diagnostics.md Run 13 section, skills/hilo-usage/SKILL.md run-13 section
(list_directory trust RETIRED, .tsx workaround, docs-site caveats), rows,
this entry.
2026-09-24 | run 14 | 🔴 DOES-NOT-DELIVER (workspace surface; rest of hilo remains
shippable per runs 7-13) | promise: spec §6 multi-repo workspace — declare repos,
mount as unified writable tree, cross-repo edges | reality: mount serves an EMPTY
tree above top level (every nested path ENOENT — P0 DF-WARPFS-48), hard-RO despite
writable:true and a "(rw)" log line (DF-WARPFS-49), manifest dialect undocumented
and rejects the help's own example file + spec §4 fields (DF-WARPFS-50),
warm --workspace no-ops at 0 files (DF-WARPFS-51); backend worktree clones work.
time-to-first-success: NEVER (flagship workflow impossible; 5 consecutive errors
just to reach a mount). install leg: PASS — bunker-las-03 fresh spawn e90e8950,
public clone f9223b9 (== HEAD), rustup minimal 1.98.1 (avoid /tmp — shared per-IP
with dead-UID files, write scratch to $HOME), cargo build --release RC=0 in 2538s
(box under load 11-15; run 13 measured 1610s idle — doc's 15-20min claim assumes a
quiet machine), smoke OK (init + warm 899 edges/103 files + stats + impact probe,
12s), agent destroyed + key removed. rows: DF-WARPFS-48..51 appended + read-back
verified (board 210 → 214, 0 dups) and committed f6f46e6, pushed
(f9223b9..f6f46e6). left behind: docs/dogfood/2026-09-24-run14-workspace-integration.md,
diagnostics.md Run 14 section, skills/hilo-usage/SKILL.md run-14 section. perf:
mount 111ms cold, unmount 10.0ms ±0.6 — no PERF row (nothing a user waits on).
2026-09-25 | run 15 | ✅ SHIPPABLE (MCP surface, official-SDK integration path) | promise: a user
can hilo init a real repo, warm the graph, point any MCP client at `hilo serve --mcp` and get
17 working vfs_* tools | angle: run 12 drove 8/17 tools with a hand-rolled NDJSON client; run 15
drove the remaining 9 (related, module, understand-natural-tasks, list_directory,
workspace ephemeral/wipe, backend status/sync, rule_check) through the official `mcp` Python
SDK (StdioServerParameters/ClientSession — the stack real integrations use), plus protocol
layer: initialize 2024-11-05, honest serverInfo 0.3.1-dev, ACCURATE served schemas, clean
stdout, clean no-project exit(1). DF-WARPFS-41 fix VERIFIED live (list_directory returns
entries; was 7/7 silent-empty in run 12). time-to-first-success: <1 min (init 12ms + warm
2.24s + handshake 0.01s); friction count: 2 (both documentation, not behavior).
rows: DF-WARPFS-53 (spec §11 drift: arg names + response shapes + phantom SSE transport —
doc lies where code is right), DF-WARPFS-54 (understand anchor recall on one-word tasks —
worked examples wanted in tool description). Appended + read-back verified (board 217 → 219,
each id once, numstat 2/0); first append attempt with duplicate id 52 was CAUGHT by the
read-back verify and rolled back before commit (restore board, re-id, re-append).
not a defect, checked and cleared: test_coverage_pct=0.0 on Rust modules is by design
(graph.rs:1905 crate-granularity exclusion); main.rs listed as orphan is correct (no
dependents for a bin entrypoint).
perf: understand 186ms ±8 warm (n=12), 187ms first-ever; handshake 0.01s; warm 2.24s;
no PERF row (nothing a user waits on).
install leg: battery path FAILED (bunker-qa.sh launch on las-03: agent 8924a5dc spawned, but
detached qa-run.sh handoff wrote a 0-byte script — "DETECT_UP_PREV_DIR: unbound variable" —
battery never started, silent death; las-02 was DOWN: connection refused) → manual install leg
on the same fresh agent per skill §1-5: PASS — public clone (HEAD 0e7873d), rustup minimal
1.98.1, cargo build --release -p hilo-cli RC=0 in 25m20s (box ~1/3 the speed of prior runs),
smoke OK on a fresh 2-file rust project (init + warm 1 edge + MCP over stdio NDJSON: init
0.000s, tools/list 17, graph_stats correct, understand 1047B) — NOTE the smoke also proved
hilo's fingerprint cache deliberately skips re-warm when a file's content is UNCHANGED (my
0-edge confusion was the cache, control-test confirmed intended behavior, no row).
my own probes confirmed: client-side tool, server clean — do NOT file the transient
'client hang' as hilo (was my smoke script sending a NOTIFICATION through a call-response
helper: JSON-RPC notifications get no response by design, server.rs:75).
agent destroyed + key removed + absence verified (bunker list grep = 0).
left behind: docs/dogfood/2026-09-25-run15-mcp-official-sdk.md, diagnostics.md Run 15 section,
skills/hilo-usage/SKILL.md run-15 section, rows, this entry.
2026-09-25 | run 16 | 🟡 PROMISING-BUT-ROUGH (Go FFI consumer; P0 found on the impact path) |
promise (angle by stale-surface rule — runs 7/8/11 touched mount/backends/Python-FFI; the Go
consumer had never been driven end to end): a Go developer can follow hilo-ffi/README.md to
generate Go bindings and embed libhilo_ffi.so in a real Go program that answers structural
questions about a repo without reading files. reality: generation works, cgo links the .so,
stats/related/listdir/metadata all match the CLI, xattr written via Go FFI reads back via
`hilo meta` (cross-surface proof) — BUT vfs_graph_impact is CWD-DEPENDENT and SILENTLY WRONG:
pkg-node expansion (PkgResolver::pkg_node → python_module_for_file, resolution.rs:359) walks
the filesystem from process CWD, not the handle root, so dependents reachable only via pkg:
nodes vanish (rc=0, 0 dependents) from any other directory. Same query: 3 dependents from
repo root, silent 0 from elsewhere (FFI and CLI both; CLI additionally builds a WRONG node
name pkg:terminal_jail.interruptor.parser missing the plugin. prefix from a subdir). Proven
not the binding layer: explicit pkg: start node through the same Go bindings returns the
correct result; raw-byte cgo probe + Python FFI cross-check isolate it to the resolver.
P0 row DF-WARPFS-55; P2s DF-WARPFS-56 (README's Go claim inverted: cgo DOES link the .so;
no link/rpath docs; vendored-openssl feature not on hilo_ffi) + DF-WARPFS-57 (namespace
vfs_get/set_metadata take raw CWD-relative paths while handle methods join root — two path
dialects in one UDL). time-to-first-success: ~15 min (documented block is incomplete; the
missing link/rpath step is DF-WARPFS-56). friction count: 3 (1 behavioral-P0, 2 docs).
rows: DF-WARPFS-55..57 appended + read-back verified (board 219 → 222, 0 dups, trailing-
newline check before append), committed 6258f4d, pushed e42c833..6258f4d (guard PASS).
perf (hyperfine, release, 57-file real graph, warm): impact d3 17.5ms ±1.1 (n=20); search
219ms ±10 (n=20); warm incremental 15.6ms ±0.5, cold 467ms ±15 (n=10); no PERF row (nothing
a user waits on; matches runs 12-15).
install leg: PASS on las-03 fresh agent df1379ab (public clone e42c833 == origin HEAD,
rustup minimal 1.98.1, cargo build --release -p hilo_ffi RC=0 in 1308s; bindgen smoke ran
after). Go smoke NOT run in the bunker (bare Debian user, no Go toolchain, no sudo —
installing one would have exceeded the documented install path; local Go consumer is the
behavioral proof). agent destroyed + key removed after collect.
left behind: docs/dogfood/2026-09-25-run16-go-ffi-consumer.md, diagnostics.md Run 16
section, skills/hilo-usage/SKILL.md run-16 section, rows, this entry.
2026-09-25 | run 17 | 🟡 PROMISING-BUT-ROUGH (plugin/extensibility surface; core read path ✅) |
promise (angle by stale-surface rule — 16 runs never executed the full plugin workflow; permissions
re-test after DF-31's doc-only fix): a user can extend Hilo with a WASM plugin — declare hooks in
.vfs/manifest.yaml per spec §4, load the module, have the runtime pick it up. HEAD 47f153b (== origin).
reality: core workflow green and fast on a fresh scratch project (init/warm/stats/impact/classify/meta,
mount + xattr + read-only + clean unmount); rule-check is a working undocumented gem. But `hilo plugin
load` on a file INSIDE .vfs/plugins (the location the manifest examples use) TRUNCATES the plugin to
0 bytes while printing success (fs::copy onto itself, O_TRUNC before read; DF-WARPFS-58, deterministic
repro: self-load 8→0 bytes rc=0, outside-load 8→8 OK). Manifest plugins: block parses and is read by
nothing — declared hooks can never dispatch (fuse/triggers have zero plugin references; DF-WARPFS-59).
permissions.rules re-probe: mode 0600 on src/** has no effect, spec §4 still implies enforcement, the
honest note lives only in docs/hilo-permissions.md (DF-WARPFS-60, P3 residual of DF-31).
time-to-first-success: <1 min core; NEVER for the documented plugin loop. friction count: 3.
perf (hyperfine, release 47f153b, 1-file scratch graph, warm n=20): stats 20.1ms ±2.0, plugin list
4.2ms ±0.4, rule-check 13.8ms ±0.7. NO PERF ROW — nothing a user waits on.
install leg: PASS — bunker-las-03 fresh agent 38c5218d, public clone 47f153b (== HEAD), rustup minimal
1.98.1 (clean RUSTUP_HOME, scratch in $HOME), cargo build --release -p hilo-cli --features vendored-openssl
RC=0 in 1519s; smoke on a fresh cargo corpus: init+warm 55ms, stats/impact/related correct (0-edge
warmup on empty lib.rs was the fingerprint cache, populated probe matched local). agent destroyed +
key removed.
rows: DF-WARPFS-58..60 appended + read-back verified (board 222 → 225, 0 bad lines, each id exactly
once, trailing newline checked before append).
left behind: docs/dogfood/2026-09-25-run17-plugins-manifest-retest.md, diagnostics.md Run 17 section,
skills/hilo-usage/SKILL.md run-17 section, rows, this entry.
