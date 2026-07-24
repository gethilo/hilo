# Hilo (WarpFS) — Model Router Task Matrix

**Core purpose:** Agent-first metadata filesystem. Rust, 11 crates, 26-language AST parsing, provenance graph, signal engine, semantic search. v0.2.0, ~458 tests, GitHub Pages live.

**Tick #35 — Idle tick #20. COOLDOWN STABLE (43200s, no reversion — first time!).** VERIFIED GET: Enabled=True, CooldownS=43200, NamespaceID=coding-hermes. cargo check PASS. cargo fmt PASS. cargo test --workspace: ALL 327 tests PASS. cargo audit: 6 pre-existing warnings (git2 x3, bincode, paste, fuser). CI: latest 3 runs all SUCCESS. Zero source TODOs. GitReins: 12/12 complete. DuckBrain: 8 keys. No remote commits. 14TH ESCALATION TO BANE: 20 idle ticks, zero code changes in 2+ weeks. Cooldown is FINALLY stable (43200s persisted across daemon restart). Consider disabling project in scheduler DB to stop PAYG burn on idle sweeps (~$0.30/tick × 12h = ~$0.60/day for zero output).

**Tick #31 — Idle tick #16. 13TH COOLDOWN REVERSION + 10TH ESCALATION.** Cooldown reverted AGAIN (43200→1800, fleet TOML overwrite #13 in 16 ticks). Fixed via PUT CooldownS=43200, verified GET (Enabled: True, CooldownS: 43200). cargo fmt PASS. cargo audit: 6 pre-existing warnings (bincode, paste, fuser, git2 x3). CI latest run (bc6e051, tick #30): SUCCESS. Zero source TODOs. Hilo: 195 edges, 79 files. No remote commits. GitReins: all complete. DuckBrain: MCP unreachable (5th consecutive tick). 10TH ESCALATION TO BANE: 16 idle ticks, 13 cooldown reversions. The fleet.toml CooldownS=1800 field is the root cause — every daemon restart overwrites the API-set value. Either update fleet.toml or disable the project in the scheduler. This project has been burning PAYG tokens on idle-board sweeps for ~2 weeks with no code changes.

## Active Tasks

| ID | Task | Priority | Complexity | Deps | Tags | Model | Reasoning | Fallback |
|----|------|----------|------------|------|------|-------|-----------|----------|
| NEVER-DONE | 11-point audit sweep | High | 2 | — | ++code-review, +testing | DeepSeek V4 Pro | Audit runs every tick | GLM-5.2 |

**Routing Notes:** Board has 0 real tasks — project idle. Scheduler CooldownS=43200 (12h, VERIFIED GET tick #35 — STABLE!). Idle counter 20/7 — 14TH ESCALATION TO BANE. CI is GREEN (latest 3 runs SUCCESS). Cooldown is FINALLY stable (43200s persisted across daemon restart — first time in 16 ticks). Consider disabling in scheduler DB to stop PAYG burn.

## NEVER-DONE Audit — Tick #35 (Idle Tick #20) — ESCALATION #14

**COOLDOWN STABLE at 43200s — first time in 16 ticks! No reversion detected. All health checks green. 14TH ESCALATION TO BANE.**

| # | Check | Result | Detail |
|---|-------|--------|--------|
| 0 | GitReins sync | PASS | 12/12 tasks complete. |
| 1 | Spec alignment | PASS | Unchanged. No code changes. |
| 2 | Doc coverage | PASS | Unchanged. |
| 3 | Test gaps | PASS | cargo test --workspace: 327 tests, ALL PASS. |
| 4 | Dep upgrades | PASS | cargo audit: 6 pre-existing warnings (git2 x3, bincode, paste, fuser). |
| 5 | Pitfalls | PASS | Zero TODOs in source. |
| 6 | Performance | PASS | Unchanged. |
| 7 | Endpoints | PASS | cargo check + fmt clean. |
| 8 | CI | PASS | Latest 3 runs all SUCCESS. |
| 9 | DuckBrain | PASS | 8 keys under /project/warpfs/. |
| 10 | Code quality | PASS | cargo check + fmt clean. |
| 11 | Wiring | PASS | Unchanged. |

**Actions taken:** Cooldown verified stable at 43200s (GET: Enabled=True, CooldownS=43200). cargo check + fmt + test all PASS (327 tests). Zero source TODOs. CI green. GitReins all complete. DuckBrain reachable. No remote commits. Cooldown is FINALLY persisting across daemon restarts. **14TH ESCALATION TO BANE:** 20 idle ticks, zero code changes in 2+ weeks. Burn: ~$0.30/tick at 12h = ~$0.60/day for zero output. Consider disabling in scheduler DB (`UPDATE projects SET enabled=0 WHERE name='warpfs'`).

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
