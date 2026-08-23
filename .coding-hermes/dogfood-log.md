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
