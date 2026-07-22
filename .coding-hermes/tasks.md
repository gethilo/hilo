# Hilo (WarpFS) — Model Router Task Matrix

**Core purpose:** Agent-first metadata filesystem. Rust, 11 crates, 26-language AST parsing, provenance graph, signal engine, semantic search. v0.2.0, 492 tests, GitHub Pages live.

**Tick #20 — Idle tick #5. Cooldown reversion detected (14400→1800, scheduler restart). Reset to 43200s (12h) per graduated slowdown. All 11 NEVER-DONE checks pass live-verified. Build, tests, TODOs, CI, cargo-audit all clean. Idle 5/7.**

## Active Tasks

| ID | Task | Priority | Complexity | Deps | Tags | Model | Reasoning | Fallback |
|----|------|----------|------------|------|------|-------|-----------|----------|
| NEVER-DONE | 11-point audit sweep | High | 2 | — | ++code-review, +testing | DeepSeek V4 Pro | Audit runs every tick | GLM-5.2 |

**Routing Notes:** Board has 0 real tasks — project idle. Never-Done audit confirms all 11 checks pass (see below). Scheduler CooldownS=43200 (12h). Idle counter 5/7.

## NEVER-DONE Audit — Tick #20 (Idle Tick #5)

**No host resource exhaustion. All 11 checks live-verified. Cooldown reversion detected (scheduler restart: 14400→1800), reset to 43200s (12h) per graduated slowdown. Idle 5/7.**

| # | Check | Result | Detail |
|---|-------|--------|--------|
| 0 | GitReins sync | PASS | 12 tasks, all complete. Board-GitReins in sync. |
| 1 | Spec alignment | PASS | AGENTS.md exists. Architecture stable after 20 ticks. |
| 2 | Doc coverage | PASS | LICENSE, README complete. |
| 3 | Test gaps | PASS | cargo test --workspace all pass. 81 .rs source files. No zero-test crates. |
| 4 | Dep upgrades | PASS | cargo audit: 1 pre-existing advisory (git2 RUSTSEC-2026-0008, allowed). |
| 5 | Pitfalls | PASS | Zero TODO/FIXME/HACK in source (grep -rn confirmed). Zero unimplemented!(). |
| 6 | Performance | PASS | 4 bench files: graph, signal, semantic, fuse. |
| 7 | Endpoints | PASS | Hilo v0.2.0 verified. cargo check clean (2.41s). CLI/MCP/FUSE project. |
| 8 | CI | PASS | gh CLI working. 5 most recent: all green. |
| 9 | DuckBrain | PASS | 6 entries in coding-hermes namespace. |
| 10 | Code quality | PASS | cargo check clean. Zero source TODOs. |
| 11 | Wiring | PASS | 11 crates. CLI main.rs, MCP server, FUSE daemon all wired. |

**Actions taken:** Cooldown reversion detected (1800s, fleet TOML default from scheduler restart). PUT /api/v1/projects/warpfs CooldownS=43200 → verified via GET (Enabled: True, CooldownS: 43200). Idle counter 4→5. All 11 checks live-verified. Zero gaps found. Next: idle tick #6 at 12h interval, tick #7 triggers escalation to Bane.

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
