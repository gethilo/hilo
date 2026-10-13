# Dogfood run 40 — warpfs MCP server surface — 2026-10-13

## Promise under test

"This project claims that an agent can manage and query a codebase's metadata
through the MCP server (`hilo serve --mcp`, 18 vfs_* tools per cli.rs:52,
README:364) exactly as it would through the CLI and FUSE surfaces."

## What was driven (real use, not tests)

- Fresh scratch repo /tmp/df40repo (app/main.py, helper.py, config.json, then
  invoice.py and consts.py added mid-run), `hilo init`, `graph warm` (2.7s cold).
- A hand-written JSON-RPC stdio MCP client (initialize → tools/list → tools/call)
  against HEAD binary hilo 0.4.0, build v0.3.0-439-g4d866c84-dirty.
- 13 tool calls across 10 distinct tools: vfs_graph_search, vfs_graph_related,
  vfs_graph_stats, vfs_graph_understand, vfs_get_metadata, vfs_set_metadata,
  vfs_list_directory, vfs_resolve_path, plus error-path calls (unknown tool,
  missing file, bad path form).
- Cross-surface parity: xattr set via CLI (`hilo meta --set`) read back via MCP
  vfs_get_metadata; xattr set via MCP vfs_set_metadata read back via CLI and
  getfattr(1). Both directions byte-identical.
- Error contract: unknown tool → JSON-RPC -32603 with clean message; nonexistent
  path → honest "no such file"; absolute path outside the workspace → silently
  served (see finding 1).

## Findings

### 1. P0 — MCP server has no workspace confinement (DF-WARPFS-139)

`vfs_get_metadata {path:"/etc/passwd"}` returned the file's metadata (size 3131)
and `vfs_list_directory {path:"/home/kara"}` returned 1129 home-directory
entries (the full output was captured; it includes private files). Root cause
read at hilo-mcp/src/tools/mod.rs:1029-1037 (`resolve_backend`): an absolute
path is used as-is, and only mount-owned paths get rewritten relative to cwd.
Relative traversal (`../`) is likewise accepted until the kernel stat fails.
The MCP tools are built FOR agents (an agent is the client), so this is the
product's primary trust boundary. Fix direction: confine every path argument to
the workspace root (canonicalize + prefix check) before any fs access, and test
it with an out-of-tree absolute + a `..` escape.

### 2. P1 — search is blind to any file with zero import edges (DF-WARPFS-140)

A source file with docstring/comment vocabulary but no imports is entirely
invisible to `hilo graph search`: added app/consts.py (constants + docstring
mentioning billing/plan price/monthly/discount) — every query returned "No
results". Adding a single `import json` line made the SAME docstring hit for
the SAME queries. The doc/symbol channels (GAP-116) ride the edge index, which
is populated per file with edges only ("1 no imports" in warm's coverage line).
A constants/config file is exactly what a user asks a code-search tool about.
Fix direction: index files with zero edges too (the warm pass already visits
and counts them), or document the blindness.

### 3. P2 — error paths are honest and useful (positive finding, no row)

Unknown tool, missing file, and missing-graph errors all arrive as proper
JSON-RPC error objects with actionable messages ("Accepted id forms: bare
repo-relative path (src/main.rs), sys:<header>, pkg:<crate>"). Exit-code N/A
(server), but the protocol contract is clean. Recorded here rather than a row
because nothing needs fixing.

## Value judgment

- Does it work? Yes — the 18-tool MCP surface is functional end to end.
- Is it useful? Yes for the in-workspace tools; the search blindness narrows it.
- Is it usable? High: stdio JSON-RPC, self-describing tools/list, good errors.
- Is it trustworthy? NO — finding 1 is a trust-boundary failure on the exact
  surface agents use.
- Fast enough? Yes: warm search 25.8ms ±1.8 (hyperfine, 20 runs), MCP 7-call
  session 322ms ±10 (dominated by process spawn), DB-rebuild cold 224ms,
  full re-init cold 50ms. Nothing a user would feel. No PERF row.

## Install leg

Ephemeral bunker agent `df40-install` on bunker-mvp (100.115.67.116, Ubuntu
24.04.4, 8 cores, no rustc preinstalled): cloned public origin, rustup into
$HOME, `cargo build -p hilo-cli` — results recorded in dogfood-log.md.

## Left behind

- This file: docs/dogfood/2026-10-13-run40-mcp-surface.md
- Rows DF-WARPFS-139 (P0), DF-WARPFS-140 (P1) on tasks.jsonl
  (created_by: dogfood-warpfs-run40)
- dogfood-log.md entry run 40
- skills/hilo-usage/SKILL.md: MCP-server section
