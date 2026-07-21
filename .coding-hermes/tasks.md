# Hilo (WarpFS) — Model Router Task Matrix

**Core purpose:** Agent-first metadata filesystem. Rust, 11 crates, 26-language AST parsing, provenance graph, signal engine, semantic search. v0.2.0, 492 tests, GitHub Pages live.

**Tick #17 — PERF-001 completed. Board has 0 real tasks.**

## Active Tasks

| ID | Task | Priority | Complexity | Deps | Tags | Model | Reasoning | Fallback |
|----|------|----------|------------|------|------|-------|-----------|----------|
| NEVER-DONE | 11-point audit sweep | High | 2 | — | ++code-review, +testing | DeepSeek V4 Pro | Audit runs every tick | GLM-5.2 |

**Routing Notes:** Board has 0 real tasks — discovery sweep needed next tick. Only NEVER-DONE audit remains.

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
