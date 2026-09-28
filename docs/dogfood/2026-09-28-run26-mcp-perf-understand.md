# Dogfood Run 26 — MCP Server, Graph Query Perf, Understand Regression

**Date:** 2026-09-28 21:44 UTC
**Binary:** v0.3.0-144-g01a9634-dirty (debug, built 2026-09-28 15:51)
**Corpus:** warpfs repo itself (83 files, 432 edges, 10 components)

## Promise

Hilo claims:
- MCP server exposes 17 tools for agent integration
- Graph queries answer in ~0.02s on a 793-file corpus
- `graph understand` returns harmonic multi-resolution context for a task

## What Worked

### MCP Server E2E

```bash
$ echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"dogfood","version":"0.1"}}}' | ./target/debug/hilo serve --mcp
{"id":1,"jsonrpc":"2.0","result":{"capabilities":{"tools":{}},"protocolVersion":"2024-11-05","serverInfo":{"name":"hilo-mcp","version":"0.3.1-dev"}}}

$ echo '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' | ./target/debug/hilo serve --mcp
# Returns all 17 tools: vfs_get_metadata, vfs_set_metadata, vfs_graph_related,
# vfs_graph_stats, vfs_graph_untested, vfs_graph_module, vfs_graph_impact,
# vfs_graph_understand, vfs_graph_search, vfs_rule_list, vfs_rule_check,
# vfs_list_directory, vfs_resolve_path, vfs_backend_status, vfs_sync_backend,
# vfs_workspace_ephemeral, vfs_workspace_wipe

$ echo '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"vfs_graph_stats","arguments":{}}}' | ./target/debug/hilo serve --mcp
# Returns: total_edges=432, total_files=83, most_connected=pkg:std, 12 orphans
```

MCP initialize, tools/list, and tools/call all work. Server starts in <1s, responds to JSON-RPC over stdio correctly.

### CLI Graph Queries (debug build)

```bash
$ time ./target/debug/hilo graph stats
# real 0.44s, user 0.20s, sys 0.04s
# 432 distinct edges, 83 files, 10 components

$ time ./target/debug/hilo graph impact 'pkg:hilo_graph' --max-depth 3
# real 0.28s
# 5 files impacted (hilo-graph/tests/parser_test.rs, hilo-cli/src/commands/classify.rs, etc.)
```

Stats and impact are fast even on debug build.

## What Didn't

### DF-WARPFS-101: graph search is 200x slower than claimed

```bash
$ time ./target/debug/hilo graph search "fn main" --limit 5
# real 4.39s, user 3.28s, sys 0.04s
```

README claims ~0.02s for search on a 793-file corpus. On an 83-file corpus (10x smaller), debug build takes 4.39s — 200x slower.

**Hypothesis:** debug build is unoptimized. Release build might hit the claimed numbers. But the gap is large enough that either (a) debug builds are unusable for interactive search, or (b) there's a real regression.

**Needs:** release-build measurement to distinguish.

### DF-WARPFS-102: graph understand returns wrong content

```bash
$ time ./target/debug/hilo graph understand "rate limiter middleware"
# real 2.77s
# Output: Go AuthMiddleware snippet from hilo-graph/tests/fixtures/middleware.go
```

Asked for "rate limiter middleware", got a Go auth middleware snippet from a test fixture file. The tool's description says it returns "harmonic multi-resolution context for a task" but the output is a literal code snippet from a test fixture, unrelated to rate limiters.

**Likely cause:** search/index is returning fixture files, or understand tool is not filtering test fixtures from its context window.

**User impact:** asks for rate-limiter context, gets Go auth middleware from a test file. The tool is not doing what it promises.

### DF-WARPFS-103: SKIPPED-install-bunker (11th consecutive run)

Same wall as runs 16-25: full workspace release builds exceed the tick time budget on both local and bunker-las-03. README says "expect 15-20 min" for first build (duckdb-sys + arrow from source), but the build hangs past 10 min even with incremental caching.

**Root cause:** no prebuilt binaries published. Every install path builds from source, and the first build is slow.

**Fix direction:** prebuilt binaries, or a scoped build path that skips duckdb-sys for non-graph commands.

## Performance Summary (debug build, 83 files)

| Command | Time | README claim |
|---------|------|--------------|
| graph stats | 0.44s | ~0.02s (on 793 files) |
| graph impact | 0.28s | ~0.5s |
| graph search | 4.39s | ~0.02s |
| graph understand | 2.77s | ~1.5s |

Stats and impact are in the right ballpark (debug is slower than release, but not 200x). Search and understand are way off — either debug-build penalty is massive, or there's a regression.

## Verdict

**PROMISING-BUT-ROUGH**

- MCP server works end-to-end (initialize, tools/list, tools/call)
- CLI graph stats and impact are fast
- Graph search is 200x slower than claimed (needs release measurement)
- Graph understand returns wrong content (test fixtures leaking into context)
- Install leg still blocked (11th consecutive run)

## Rows Filed

- DF-WARPFS-101: search 4.4s on debug build (needs release measurement)
- DF-WARPFS-102: understand returns hardcoded fixture, not task context
- DF-WARPFS-103: SKIPPED-install-bunker (11th consecutive run, release build timeout)

## Commit

`2fbea5e` — dogfood: warpfs run 26 — MCP server, graph query perf, understand tool regression

</ARG>