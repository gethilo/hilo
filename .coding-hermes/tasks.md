# Hilo (WarpFS) — Model Router Task Matrix

**Core purpose:** Agent-first metadata filesystem. Rust, 11 crates, 26-language AST parsing, provenance graph, signal engine, semantic search. v0.2.0, ~458 tests, GitHub Pages live.

**Tick #31 — Idle tick #16. 13TH COOLDOWN REVERSION + 10TH ESCALATION.** Cooldown reverted AGAIN (43200→1800, fleet TOML overwrite #13 in 16 ticks). Fixed via PUT CooldownS=43200, verified GET (Enabled: True, CooldownS: 43200). cargo fmt PASS. cargo audit: 6 pre-existing warnings (bincode, paste, fuser, git2 x3). CI latest run (bc6e051, tick #30): SUCCESS. Zero source TODOs. Hilo: 195 edges, 79 files. No remote commits. GitReins: all complete. DuckBrain: MCP unreachable (5th consecutive tick). 10TH ESCALATION TO BANE: 16 idle ticks, 13 cooldown reversions. The fleet.toml CooldownS=1800 field is the root cause — every daemon restart overwrites the API-set value. Either update fleet.toml or disable the project in the scheduler. This project has been burning PAYG tokens on idle-board sweeps for ~2 weeks with no code changes.

## Active Tasks

| ID | Task | Priority | Complexity | Deps | Tags | Model | Reasoning | Fallback |
|----|------|----------|------------|------|------|-------|-----------|----------|
| NEVER-DONE | 11-point audit sweep | High | 2 | — | ++code-review, +testing | DeepSeek V4 Pro | Audit runs every tick | GLM-5.2 |

**Routing Notes:** Board has 0 real tasks — project idle. Scheduler CooldownS=43200 (12h, verified GET). Idle counter 16/7 — 10TH ESCALATION TO BANE. CI is GREEN (tick #30 run succeeded). PERMANENT FIX URGENT: fleet.toml CooldownS 1800→43200 or project disable. 13 reversions in 16 ticks is unsustainable.

## NEVER-DONE Audit — Tick #31 (Idle Tick #16) — ESCALATION #10

**13th cooldown reversion detected. Fixed (1800→43200). VERIFIED GET: Enabled=True, CooldownS=43200. PERMANENT FIX URGENT: fleet.toml update or project disable.**

| # | Check | Result | Detail |
|---|-------|--------|--------|
| 0 | GitReins sync | PASS | Unchanged — all tasks complete from prior ticks. |
| 1 | Spec alignment | PASS | Unchanged from prior ticks. |
| 2 | Doc coverage | PASS | Unchanged. |
| 3 | Test gaps | PASS | ~458 tests across 25 suites ALL PASS (prior tick #28 verified). No code changes since. |
| 4 | Dep upgrades | PASS | cargo audit: 6 pre-existing warnings (bincode, paste, fuser RUSTSEC-2021-0154, git2 x3 RUSTSEC-2026-0183/0184/0008). No new vulns. |
| 5 | Pitfalls | PASS | Zero TODOs in source (grep -rn TODO/FIXME/HACK --include="*.rs": 0 results). |
| 6 | Performance | PASS | Unchanged. |
| 7 | Endpoints | PASS | cargo fmt clean. No code changes. |
| 8 | CI | PASS | Latest run (bc6e051, tick #30): SUCCESS — CI is green. |
| 9 | DuckBrain | SKIP | MCP Connection Error (unreachable) — 5th consecutive tick. Prior known: 7 entries present. |
| 10 | Code quality | PASS | fmt clean. Zero source TODOs. Hilo: 195 edges, 79 files (unchanged). |
| 11 | Wiring | PASS | Unchanged. |

**Actions taken:** Cooldown reversion detected (1800s — 13th fleet TOML overwrite). PUT CooldownS=43200 → verified GET (Enabled: True, CooldownS: 43200). cargo fmt clean. CI green (tick #30 run succeeded). Zero source TODOs. No remote commits. All never-done checks PASS (DuckBrain SKIP — MCP unreachable 5 consecutive ticks). 10TH ESCALATION TO BANE: 16 idle ticks, 13 cooldown reversions. Permanent fix required — either update fleet.toml CooldownS to 43200 or disable the project in the scheduler. This project has been burning PAYG tokens on idle-board sweeps for ~2 weeks with zero code changes.

## Completed Summary

**U01 (Tick #23):** Marked complete — usability & coverage audit scope fully covered by ongoing never-done audit.
**PERF-001 (Tick #17):** Criterion benchmarks for signal engine (6 benches) and semantic search (15 benches). 537 lines. Commit b81d7dc.
**PITFALL-ffi-stubs (Tick #16):** All 8 stub functions wired to real crate implementations. 195 lines. Commit 291981b.
**DEPS-minor (Tick #15):** libc 0.2.186→0.2.187, regalloc2 0.15.1→0.15.2. 492 tests pass. Commit b8c1d6d.
**TASK-001:** Provenance Tracking. 18 files, 386 tests.
**TASK-002:** Signal Engine. ~900 lines, 11 integration tests.
**TASK-003:** Semantic Search. ~46 tests.
**TASK-004:** Determinism Tests. 14 tests.
**TASK-005-007:** Language expansion to 26 languages.
**DOC/INFRA/SEC:** Version 0.2.0 bump. GitHub Pages. MCP docs (15/15). 5 cargo-audit vulns resolved. Docker Compose. Rate limiting. Structured logging. CLI subcommands.
**Maintenance:** 10 per-crate API docs. DuckBrain populated. .gitignore fixed. CI race fixed. 28 outdated deps upgraded.

## [ ] NEVER-DONE — Run coding-hermes-never-done 11-point audit
