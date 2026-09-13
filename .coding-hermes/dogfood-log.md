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
