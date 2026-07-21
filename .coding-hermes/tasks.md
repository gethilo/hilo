# Hilo (WarpFS) — Model Router Task Matrix

**Core purpose:** Agent-first metadata filesystem. Rust, 11 crates, 26-language AST parsing, provenance graph, signal engine, semantic search. v0.2.0, 492 tests, GitHub Pages live.

**Tick #14 — Audit found 3 gaps. Idle streak BROKEN (was 5 ticks). Reset to active mode.**

## Active Tasks

| ID | Task | Priority | Complexity | Deps | Tags | Model | Reasoning | Fallback |
|----|------|----------|------------|------|------|-------|-----------|----------|
| PITFALL-ffi-stubs | Wire hilo-ffi stub functions to real crate implementations | High | 4 | hilo-graph, hilo-metadata, hilo-backends | +pitfall, +ffi, +rust | DeepSeek V4 Pro | 8 stub functions return empty/zero — needs real wiring to graph/metadata/backend crates | DeepSeek V4 Flash |
| PERF-001 | Add benchmarks for hot paths (graph parsing, signal engine, semantic search) | Medium | 3 | — | +perf, +rust | DeepSeek V4 Pro | Zero #[bench] functions across workspace — no performance baselines exist | GLM-5.2 |
| DEPS-minor | Upgrade libc 0.2.186→0.2.187, regalloc2 0.15.1→0.15.2 | Low | 1 | — | +deps, +rust, +chore | DeepSeek V4 Flash | 2 minor crate updates from cargo update --dry-run | — |
| NEVER-DONE | 11-point audit sweep | High | 2 | — | ++code-review, +testing | DeepSeek V4 Pro | Audit runs every tick | GLM-5.2 |

**Routing Notes:** PITFALL-ffi-stubs requires understanding of 3 crates (graph, metadata, backends) to wire stub functions → DeepSeek V4 Pro. PERF-001 needs benchmark expertise in Rust → DeepSeek V4 Pro. DEPS-minor is mechanical → DeepSeek V4 Flash. Board has 3 real tasks — active mode.

**Execution Order:** DEPS-minor (1) → PITFALL-ffi-stubs (2) → PERF-001 (3) → NEVER-DONE audit (recurring).

**Escalation Conditions:** PITFALL-ffi-stubs may reveal architectural question about FFI design intent. If hilo-ffi stubs are by design (not yet wired, pending language SDK integration), mark as BLOCKED and escalate to Bane for roadmap decision.

## Completed Summary

**TASK-001 (Provenance Tracking):** 6-level Provenance enum (AstExact→Unresolved) with confidence weights. Edge struct extended. DuckDB schema migrated. All queries updated. 18 files touched, 386 tests.
**TASK-002 (Signal Engine):** Harmonic 3-tier output (MAP 15%, SIGNATURES 25%, DETAIL 60%). Position ordering. Deterministic. MCP tool `vfs_graph_understand`. ~900 lines, 11 integration tests.
**TASK-003 (Semantic Search):** TF-IDF + BM25 + RRF. Pure Rust, zero API calls. Deterministic. MCP tool `vfs_graph_search`. ~530 lines, 26 unit + 20 integration tests.
**TASK-004 (Determinism Tests):** 14 determinism tests over 6 fixture files proving byte-identical graph/signal/semantic output across runs.
**TASK-005-007 (Language Expansion):** 26 languages total — C#, Kotlin, PHP, Swift, Elixir, Haskell, Erlang, Scala, Zig, Lua, Dart, Clojure, OCaml, R, Julia, Elm, Nim.
**DOC/INFRA/SEC:** Version 0.2.0 bump (17 files). GitHub Pages enabled (gethilo.github.io/hilo). MCP tool docs completed (15/15). 5 cargo-audit vulns resolved. Docker Compose for integration tests. Rate limiting (token bucket). Structured logging (tracing). CLI subcommands for understand/search/module/untested.
**Maintenance:** 10 per-crate API docs. DuckBrain populated (46+ entries). .gitignore fixed. CI race condition fixed (tokio async flush). 28 outdated deps upgraded. 2 minor dep bumps. 10 test files for hilo-permissions.

## [ ] NEVER-DONE — Run coding-hermes-never-done 11-point audit
