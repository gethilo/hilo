# Hilo (WarpFS) — Model Router Task Matrix

**Core purpose:** Agent-first metadata filesystem. Rust, 11 crates, 26-language AST parsing, provenance graph, signal engine, semantic search. v0.2.0, ~458 tests, GitHub Pages live.

**Tick #34 — Idle tick #19. 16TH COOLDOWN REVERSION + 13TH ESCALATION.** Cooldown reverted AGAIN (43200→1800, fleet TOML overwrite #16). Fixed via PUT CooldownS=43200, VERIFIED GET (Enabled: True, CooldownS: 43200). cargo check PASS. cargo fmt PASS. cargo test --workspace: ALL PASS. cargo audit: 6 pre-existing allowed warnings (git2 x3, bincode, paste, fuser). CI: latest 3 runs all SUCCESS. Zero source TODOs. Hilo: 195 edges, 79 files (unchanged). GitReins: 12/12 complete. DuckBrain: reachable (7 keys). No remote commits. 13TH ESCALATION TO BANE: 19 idle ticks, 16 reversions. PERMANENT FIX URGENT — fleet.toml CooldownS 1800→43200 or disable project. Burn: ~$6.00+ wasted on 19 empty-board PAYG sweeps with zero code changes in 2+ weeks.

**Tick #31 — Idle tick #16. 13TH COOLDOWN REVERSION + 10TH ESCALATION.** Cooldown reverted AGAIN (43200→1800, fleet TOML overwrite #13 in 16 ticks). Fixed via PUT CooldownS=43200, verified GET (Enabled: True, CooldownS: 43200). cargo fmt PASS. cargo audit: 6 pre-existing warnings (bincode, paste, fuser, git2 x3). CI latest run (bc6e051, tick #30): SUCCESS. Zero source TODOs. Hilo: 195 edges, 79 files. No remote commits. GitReins: all complete. DuckBrain: MCP unreachable (5th consecutive tick). 10TH ESCALATION TO BANE: 16 idle ticks, 13 cooldown reversions. The fleet.toml CooldownS=1800 field is the root cause — every daemon restart overwrites the API-set value. Either update fleet.toml or disable the project in the scheduler. This project has been burning PAYG tokens on idle-board sweeps for ~2 weeks with no code changes.

## Active Tasks

| ID | Task | Priority | Complexity | Deps | Tags | Model | Reasoning | Fallback |
|----|------|----------|------------|------|------|-------|-----------|----------|
| NEVER-DONE | 11-point audit sweep | High | 2 | — | ++code-review, +testing | DeepSeek V4 Pro | Audit runs every tick | GLM-5.2 |

**Routing Notes:** Board has 0 real tasks — project idle. Scheduler CooldownS=43200 (12h, VERIFIED GET tick #34). Idle counter 19/7 — 13TH ESCALATION TO BANE. CI is GREEN (latest 3 runs SUCCESS). PERMANENT FIX URGENT: fleet.toml CooldownS 1800→43200 or project disable. 16 reversions in 19 ticks is unsustainable. Burn: ~$6.00+ wasted.

## NEVER-DONE Audit — Tick #34 (Idle Tick #19) — ESCALATION #13

**16th cooldown reversion detected. Fixed (1800→43200). VERIFIED GET: Enabled=True, CooldownS=43200. PERMANENT FIX URGENT: fleet.toml update or project disable. 13TH ESCALATION TO BANE.**

| # | Check | Result | Detail |
|---|-------|--------|--------|
| 0 | GitReins sync | PASS | 12/12 tasks complete. |
| 1 | Spec alignment | PASS | Unchanged. No code changes. |
| 2 | Doc coverage | PASS | Unchanged. |
| 3 | Test gaps | PASS | cargo test --workspace: ALL PASS. No code changes. |
| 4 | Dep upgrades | PASS | cargo audit: 6 allowed warnings (git2 x3, bincode, paste, fuser — pre-existing). |
| 5 | Pitfalls | PASS | Zero TODOs in source (0 grep results). |
| 6 | Performance | PASS | Unchanged. |
| 7 | Endpoints | PASS | cargo check + fmt clean. |
| 8 | CI | PASS | Latest 3 runs all SUCCESS (tick #33, #31, #30). |
| 9 | DuckBrain | PASS | Reachable. 7 keys under /project/warpfs/. |
| 10 | Code quality | PASS | cargo check + fmt clean. Hilo: 195 edges, 79 files (unchanged). |
| 11 | Wiring | PASS | Unchanged. |

**Actions taken:** Cooldown reversion #16 fixed (PUT 43200s → VERIFIED GET with Python script showing Enabled: True, CooldownS: 43200). cargo check + fmt + test all PASS. Zero source TODOs. CI green. GitReins all complete. DuckBrain reachable. No remote commits. All never-done checks PASS. **13TH ESCALATION TO BANE:** 19 idle ticks, 16 reversions. Permanent fix required — fleet.toml CooldownS 1800→43200 or disable project. Burn: ~$6.00+ wasted on 19 empty-board sweeps spanning 2+ weeks with zero code changes.

## NEVER-DONE Audit — Tick #32 (Idle Tick #17) — ESCALATION #11

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
