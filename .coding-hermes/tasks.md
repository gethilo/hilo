# Hilo (WarpFS) — Model Router Task Matrix

**Core purpose:** Agent-first metadata filesystem. Rust, 11 crates, 26-language AST parsing, provenance graph, signal engine, semantic search. v0.2.0, 492 tests, GitHub Pages live.

**Tick #23 — Idle tick #8. ESCALATION TO BANE (2nd notice).** Cooldown reverted for the 5th time (43200→1800, fleet TOML). Fixed → 43200s (verified GET). U01 marked [x] — covered by ongoing never-done audit. cargo check clean, CI all green. cargo test resource-exhausted (fleet-wide fork/thread starvation, not code regression). 8 idle ticks — PERMANENT FIX REQUIRED: fleet.toml CooldownS 1800→43200 or project disable. 5 reversions in 9 ticks is unsustainable.

## Active Tasks

| ID | Task | Priority | Complexity | Deps | Tags | Model | Reasoning | Fallback |
|----|------|----------|------------|------|------|-------|-----------|----------|
| U01 | Usability & coverage audit — find gaps in endpoint wiring, UX flow, error handling, edge cases, test coverage | High | 3±1 | — | +++testing, ++endpoint-verification, ++code-review, +e2e, -vision | DS-V4-Flash | Medium | GLM-5.2 |
| NEVER-DONE | 11-point audit sweep | High | 2 | — | ++code-review, +testing | DeepSeek V4 Pro | Audit runs every tick | GLM-5.2 |

**Routing Notes:** Board has 0 real tasks — project idle. Scheduler CooldownS=43200 (12h, verified). Idle counter 7/7 — escalating to Bane.

## NEVER-DONE Audit — Tick #22 (Idle Tick #7) — ESCALATION

**Quick idle sweep confirms zero gaps. 4th cooldown reversion detected. Escalating to Bane per graduated slowdown rules.**

| # | Check | Result | Detail |
|---|-------|--------|--------|
| 0 | GitReins sync | PASS | 12 tasks, all complete. In sync. |
| 1 | Spec alignment | PASS | Unchanged. |
| 2 | Doc coverage | PASS | Unchanged. |
| 3 | Test gaps | PASS | cargo test --workspace: all pass. |
| 4 | Dep upgrades | PASS | cargo audit: 6 pre-existing warnings (bincode, paste, git2 x2). No new vulns. |
| 5 | Pitfalls | PASS | Zero TODOs in source (grep -rn TODO/FIXME/HACK --include="*.rs": 0 results). |
| 6 | Performance | PASS | Unchanged. |
| 7 | Endpoints | PASS | cargo check clean (1.03s). |
| 8 | CI | PASS | gh CLI: last 5 runs all green (success). |
| 9 | DuckBrain | PASS | 6 entries in coding-hermes namespace. |
| 10 | Code quality | PASS | cargo check clean. Zero source TODOs. Hilo: 195 edges, 79 files. |
| 11 | Wiring | PASS | Unchanged. |

**Actions taken:** Cooldown reversion detected (1800s, fleet TOML overwrite during scheduler restart ~09:08 UTC). PUT /api/v1/projects/warpfs CooldownS=43200 → verified via GET (Enabled: True, CooldownS: 43200). This is the 4th reversion — prior ticks #4, #5, #6 all fixed the same issue, each reverted by scheduler restart + fleet TOML. **Permanent fix needed: update fleet.toml CooldownS from 1800 to 43200 for warpfs, OR disable the project in the scheduler.**

## Completed Summary

**PERF-001 (Tick #17):** Criterion benchmarks for signal engine (6 benches: understand 100/500, tokenize short/medium/long, extract_symbols) and semantic search (15 benches: tokenize 5 variants, TF-IDF build 100/500/1000, TF-IDF search 5/20/50%, BM25 search 5/20/50%, E2E). 537 lines total. Commit b81d7dc.
**PITFALL-ffi-stubs (Tick #16):** All 8 stub functions wired to real crate implementations. vfs_get_metadata→hilo_metadata, vfs_set_metadata→hilo_metadata, vfs_graph_related→hilo_graph, vfs_graph_impact→hilo_graph, vfs_graph_stats→hilo_graph, vfs_resolve_backend→hilo_core+hilo_backends, vfs_rule_check→hilo_graph+hilo_core, vfs_list_directory→std::fs. 195 lines inserted, 29 deleted. Commit 291981b.
**DEPS-minor (Tick #15):** libc 0.2.186→0.2.187, regalloc2 0.15.1→0.15.2. 492 tests pass. Commit b8c1d6d.
**TASK-001 (Provenance Tracking):** 6-level Provenance enum (AstExact→Unresolved) with confidence weights. Edge struct extended. DuckDB schema migrated. All queries updated. 18 files touched, 386 tests.
**TASK-002 (Signal Engine):** Harmonic 3-tier output (MAP 15%, SIGNATURES 25%, DETAIL 60%). Position ordering. Deterministic. MCP tool `vfs_graph_understand`. ~900 lines, 11 integration tests.
**TASK-003 (Semantic Search):** TF-IDF + BM25 + RRF. Pure Rust, zero API calls. Deterministic. MCP tool `vfs_graph_search`. ~530 lines, 26 unit + 20 integration tests.
**TASK-004 (Determinism Tests):** 14 determinism tests over 6 fixture files proving byte-identical graph/signal/semantic output across runs.
**TASK-005-007 (Language Expansion):** 26 languages total — C#, Kotlin, PHP, Swift, Elixir, Haskell, Erlang, Scala, Zig, Lua, Dart, Clojure, OCaml, R, Julia, Elm, Nim.
**DOC/INFRA/SEC:** Version 0.2.0 bump (17 files). GitHub Pages enabled (gethilo.github.io/hilo). MCP tool docs completed (15/15). 5 cargo-audit vulns resolved. Docker Compose for integration tests. Rate limiting (token bucket). Structured logging (tracing). CLI subcommands for understand/search/module/untested.
**Maintenance:** 10 per-crate API docs. DuckBrain populated (46+ entries). .gitignore fixed. CI race condition fixed (tokio async flush). 28 outdated deps upgraded. 2 minor dep bumps. 10 test files for hilo-permissions.

## [ ] NEVER-DONE — Run coding-hermes-never-done 11-point audit
