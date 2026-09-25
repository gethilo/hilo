# Dogfood Run 15 — MCP server via the official SDK (remaining 9 tools + protocol layer)

**Date:** 2026-09-25 · **Build:** hilo 0.3.1-dev (v0.3.0-55-g192e797-dirty, target/release, Sep 24) · HEAD fed7ae2
**Angle:** run 12 (2026-09-23) drove 8 of the 17 tools through a hand-rolled `subprocess` NDJSON
client. Run 15 covers what run 12 did not: the other 9 tools, a REAL MCP client (official
`mcp` Python SDK, `StdioServerParameters` + `ClientSession` — the exact stack a Claude Code /
Hermes integration uses), protocol-level behavior (initialize/capabilities, tool schemas as
the client sees them), and re-verification of the DF-WARPFS-41 `vfs_list_directory` fix.

## Promise under test

"A user can `hilo init` a real repo, warm the graph, point any MCP client at
`hilo serve --mcp` and get 17 working `vfs_*` tools that answer graph/metadata questions."

## Target

Scratch copy of the hilo repo itself (110 files, 1132 edges / 103 files after `graph warm`,
4 languages) at `/tmp/dogfood-warpfs-mcp/repo`. Answers independently checkable — we are the
repo.

## Real use performed (official SDK client, 3 sessions)

Setup cost, measured: `hilo init` 12 ms (installs post-commit/post-merge hooks),
`graph warm` 2.24 s (110 files, cold), `classify` 0.15 s. Server handshake + `initialize`
0.01 s; `tools/list` → exactly 17 tools, every one with a properties-bearing JSON schema.

Tools exercised this run (run 12 already covered get/set_metadata, impact, search,
understand, resolve_path, stats, rule_list):

- `vfs_graph_related` — returns a flat LIST of `{from, to, relation, confidence, provenance,
  scope}` edge objects (8 for `hilo-mcp/src/tools/mod.rs` @depth 2), not the spec's
  `{files: [...]}` wrapper. Content correct against the repo's own imports.
- `vfs_graph_module` — `module_name: "hilo-fuse"` → 9 files, 118 edges;
  `hilo-graph` → 270 edges. `test_coverage_pct: 0.0` for Rust is BY DESIGN
  (graph.rs:1905 documents the `.rs` exclusion — one crate-root `tested_by` edge would
  otherwise mark a whole `src/` covered). The exclusion is invisible at the MCP layer;
  a Go repo shows real percentages (run 12 era data agrees).
- `vfs_graph_understand` — 4 natural-language tasks: "mount the fuse daemon",
  "trigger debounce", "parse edges duckdb" all anchored correctly (8/7/5 files, real
  doc-comments in excerpts, 176–203 ms warm). The literal word "workspace" anchored only 3
  files and 3 of 3 were the right ones — short ambiguous queries just return fewer anchors
  with honest guidance text, they do not fail.
- `vfs_list_directory` — **DF-WARPFS-41 FIX VERIFIED**: entries returned for `.`, with
  type/size/backend per entry. 7/7 silent-empty in run 12; working now.
- `vfs_workspace_ephemeral` / `vfs_workspace_wipe` — listed junk files with reasons
  (`__pycache__/` etc.), wipe freed 2,078,036 bytes, removed list matches listed set. The
  run-14 CLI workspace rot (DF-WARPFS-48/49) does NOT apply to these two tools — they operate
  on the local tree, not the workspace FUSE mount.
- `vfs_backend_status` / `vfs_sync_backend` — honest `local-only, cached:true` report;
  sync correctly refuses an unmanaged local path, naming the CLI escape hatch.
- `vfs_rule_check` — clean `Rule '<name>' not found in manifest. Available:` error for a
  bogus rule (manifest has no rules; empty list is correct).
- Error paths: nonexistent file → actionable message listing accepted id forms
  (`bare path | sys: | pkg:`); unknown rule → not-found + availability list. All clean.

## Protocol layer (what run 12's raw client could not see)

- `initialize` → `protocolVersion: 2024-11-05`, `serverInfo hilo-mcp 0.3.1-dev` (honest,
  DF-WARPFS-17 fix holding), `capabilities.tools` declared.
- Tool schemas served to the client are ACCURATE: `vfs_graph_module.module_name`,
  `vfs_rule_check.name`, `vfs_graph_understand.task` (required) + `budget`/`resolution`
  (optional). A real integration reads these and never guesses wrong.
- stdout hygiene: zero non-JSON bytes on stdout across all SDK sessions (GAP-050 holding);
  server INFO logs go to stderr.
- No-project behavior: `hilo serve --mcp` outside a Hilo project exits 1 with
  `error: no Hilo project found … run 'hilo init' first` — exactly the documented contract.
- `hilo serve` without `--mcp` is a clap error (exit 2): `--mcp` is the only mode. Spec §11
  still says "stdio or SSE" and the manifest example carries `transport: sse, port: 8766` —
  SSE does not exist (filed).

## Findings (rows filed this run)

- **DF-WARPFS-52 [P2] spec §11 drift**: 3 tool signatures + 2 response shapes in
  `specs/warpfs-spec.md` §11 disagree with the shipped server (module→module_name,
  rule_name→name, `understand` needs `task`; related returns a bare list, module omits
  coverage metadata). Wire schemas are right; the spec doc is what lies. Fix: regenerate
  §11 from `tools/list`, plus drop/mark the SSE transport + manifest `mcp.port` block.
- **DF-WARPFS-53 [P2] understand anchor-recall on short queries**: single-generic-word
  tasks return few anchors with no hint what WOULD anchor (worked examples in help text
  are the cheap fix). Honest-but-blunt; P2 polish, not a functional break.

## Numbers

| operation | warm | cold/first | note |
|---|---|---|---|
| initialize handshake | 0.01 s | — | SDK, spawn included in session setup |
| `vfs_graph_understand` | 186 ms ± 8 (n=12) | 187 ms first-ever | 110-file repo; harmonic, default budget |
| fresh session setup (spawn+init) | — | ~7 ms + server boot | amortized by persistent connection |
| `graph warm` | — | 2.24 s | 110 files (scratch copy, page-warm) |

No PERF row: nothing here makes a user wait. The 186 ms understand call is the slowest
tool and it is an interactive burst, not a loop; run 12 already established the
tens-of-ms band for the graph reads.

## Verdict

✅ **SHIPPABLE (MCP surface)** — all 17 tools now exercised across runs 12+15; the official-SDK
integration path works end-to-end with accurate schemas, honest errors, clean stdout, and
sub-200 ms answers. Usability friction is documentation drift (spec §11), not behavior.
