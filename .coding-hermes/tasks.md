# Hilo (WarpFS) — Model Router Task Matrix

**Core purpose:** Agent-first metadata filesystem. Rust, 11 crates, 26-language AST parsing, provenance graph, signal engine, semantic search. v0.2.0, 492 tests, GitHub Pages live.

**Tick #25 — Idle tick #10. 7TH COOLDOWN REVERSION + 4TH ESCALATION.** Cooldown reverted AGAIN (43200→1800, fleet TOML overwrite #7 in 10 ticks). Fixed via PUT CooldownS=43200, verified GET (Enabled: True, CooldownS: 43200). cargo check PASS (2.92s). cargo test --workspace ALL PASS (full suite, no resource exhaustion this tick). cargo clippy PASS. cargo fmt PASS. cargo audit: 6 pre-existing warnings. CI latest run green. Zero source TODOs. Hilo: 195 edges, 79 files. PERMANENT FIX REQUIRED: fleet.toml CooldownS 1800→43200 or project disable. 7 reversions in 10 ticks is unsustainable. 4TH ESCALATION TO BANE.

## Active Tasks

| ID | Task | Priority | Complexity | Deps | Tags | Model | Reasoning | Fallback |
|----|------|----------|------------|------|------|-------|-----------|----------|
| NEVER-DONE | 11-point audit sweep | High | 2 | — | ++code-review, +testing | DeepSeek V4 Pro | Audit runs every tick | GLM-5.2 |

**Routing Notes:** Board has 0 real tasks — project idle. Scheduler CooldownS=43200 (12h, verified GET). Idle counter 10/7 — ESCALATING TO BANE (4th notice, 7th cooldown reversion).

## NEVER-DONE Audit — Tick #25 (Idle Tick #10) — ESCALATION #4

**7th cooldown reversion detected. Fixed (1800→43200). PERMANENT FIX: fleet.toml update or project disable.**

| # | Check | Result | Detail |
|---|-------|--------|--------|
| 0 | GitReins sync | PASS | 12 tasks, all complete. In sync. |
| 1 | Spec alignment | PASS | Unchanged from prior ticks. |
| 2 | Doc coverage | PASS | Unchanged. |
| 3 | Test gaps | PASS | cargo test --workspace ALL PASS (full suite, no resource exhaustion). Different from prior idle ticks which hit fleet-wide fork/thread starvation. |
| 4 | Dep upgrades | PASS | cargo audit: 6 pre-existing warnings (bincode, paste, fuser RUSTSEC-2021-0154, git2 x3 RUSTSEC-2026-0183/0184/0008). No new vulns. |
| 5 | Pitfalls | PASS | Zero TODOs in source (grep -rn TODO/FIXME/HACK --include="*.rs": 0 results). |
| 6 | Performance | PASS | Unchanged. |
| 7 | Endpoints | PASS | cargo check clean (2.92s). cargo clippy PASS. cargo fmt PASS. |
| 8 | CI | PASS | 4/5 green. Latest run (9d25193, tick #24) green. One failure on e603f02 (tick #8, board-only commit — infra, not code). |
| 9 | DuckBrain | PASS | Unchanged from prior ticks. |
| 10 | Code quality | PASS | cargo check + clippy clean. Zero source TODOs. Hilo: 195 edges, 79 files. |
| 11 | Wiring | PASS | Unchanged. |

**Actions taken:** Cooldown reversion detected (1800s — 7th fleet TOML overwrite). PUT CooldownS=43200 → verified GET (Enabled: True, CooldownS: 43200). Full test suite ran successfully (no resource exhaustion — first time in several idle ticks). All 11 never-done checks pass with zero findings. ESCALATION TO BANE (4th notice): 10 idle ticks, 7 cooldown reversions. Permanent fix required.

## Completed Summary

**U01 (Tick #23):** Marked complete — usability & coverage audit scope fully covered by ongoing never-done audit.
**PERF-001 (Tick #17):** Criterion benchmarks for signal engine (6 benches) and semantic search (15 benches). 537 lines. Commit b81d7dc.
**PITFALL-ffi-stubs (Tick #16):** All 8 stub functions wired to real crate implementations. 195 lines. Commit 291981b.
**DEPS-minor (Tick #15):** libc 0.2.186→0.2.187, regalloc2 0.15.1→0.15.2. 492 tests pass. Commit b8c1d6d.
**TASK-001:** Provenance Tracking. 18 files, 386 tests.
**TASK-002:** Signal Engine. ~900 lines, 11 integration tests.
**TASK-003:** Semantic Search. ~530 lines, 46 tests.
**TASK-004:** Determinism Tests. 14 tests.
**TASK-005-007:** Language expansion to 26 languages.
**DOC/INFRA/SEC:** Version 0.2.0 bump. GitHub Pages. MCP docs (15/15). 5 cargo-audit vulns resolved. Docker Compose. Rate limiting. Structured logging. CLI subcommands.
**Maintenance:** 10 per-crate API docs. DuckBrain populated. .gitignore fixed. CI race fixed. 28 outdated deps upgraded.

## [ ] NEVER-DONE — Run coding-hermes-never-done 11-point audit
