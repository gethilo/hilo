# Dogfood Run 12 — MCP Server Surface (hermes-canopy target)

**Date:** 2026-09-23 · **Build:** hilo 0.3.1-dev (v0.3.0-20-g8832da0, target/release, Sep 22)
**Angle:** the MCP server (`hilo serve --mcp`) — the agent-facing surface. Run 1 (0.2.x era)
drove only 6 of 15 tools; 17 tools ship now and no run had ever connected a real client to
the 0.3.x server. Runs 7-11 covered FUSE, backends, FFI, git-backend, plugins, triggers,
permissions, Java concurrency — MCP stdio was the untouched surface.

## Promise under test

"A real MCP client (the shape Claude Code / Hermes use: NDJSON JSON-RPC 2.0 on stdio) can
connect `hilo serve --mcp` to a project and answer structural questions about a codebase it
has never seen, through the 17 vfs_* tools, without reading files."

## Real use performed

Target: a scratch copy of hermes-canopy (Go + TS repo, 666-file graph after warm) at
`/tmp/dogfood-warpfs-run12/scratch-canopy-target` — a repo this machine's agents genuinely
work on, so answers were independently checkable.

Workflow a real consumer runs, driven from Python over `subprocess` (the only transport an
MCP stdio client has):

1. `initialize` → `serverInfo {name: hilo-mcp, version: 0.3.1-dev}` — **version is now
   honest** (run-4's "serverInfo lies: reports 0.2.0" defect is fixed in 0.3.x).
2. `tools/list` → **exactly 17 tools**, schemas present and descriptive (`path` vs `task`
   required args stated per tool).
3. Structural battery, 8 tools exercised with real questions:
   - `vfs_graph_stats` — 6845 distinct edges (4511 imports / 2213 tested_by / 121 tests),
     most-connected `std:context`, orphans listed. Matches CLI `graph stats` byte-for-byte
     in content.
   - `vfs_graph_impact` `internal/card/service.go` depth 3 — **total=27, all 27 returned**,
     sample cross-checked against the CLI's own impact output: identical file set.
   - `vfs_graph_search` "context compiler" — ranked lexical hits, top = the real compiler
     file (`internal/context/compiler.go`), provenance marked `lexical`.
   - `vfs_graph_understand` "card storage and sync" budget 3000 — anchored on 8 real card
     files, emitted tiered file excerpts. The flagship agent-tool works and is fast.
   - `vfs_get_metadata` / `vfs_set_metadata` — wrote `user.vfs.feature=dogfood-run12` via
     MCP, read it back through a **fresh server process** → xattr persisted (cross-restart
     proof, same guarantee the CLI leg proved in run 1).
   - `vfs_resolve_path` — honest backend report: `local-only, cached:true` for a
     non-managed path.
   - `vfs_rule_list` — `{rules:[],total:0}` (no rules file in target: correct empty, and
     arguably should distinguish "no manifest" from "no rules").
4. Error-quality probes (a user hits these first):
   - Nonexistent file → `-32603` with the **good** message: accepted id forms listed
     (bare path / `sys:` / `pkg:`) — this is the fix from DF-WARPFS-2 working end-to-end.
   - Unknown tool → clean `-32603 Unknown tool`.
   - Wrong arg name (`file` instead of `path`) → `missing 'path' argument` — actionable,
     though naming the expected schema key + a did-you-mean would be kinder.
5. **The failure:** `vfs_list_directory` returned `{"entries":[],"total":0}` for 7/7 real,
   populated paths (4 relative shapes, an absolute path, `.` — and even a file path, which
   should be a type error, also silent-empty). Cross-check: `vfs_workspace_ephemeral` on the
   same tree enumerated `frontend/dist/*` files correctly, so path resolution and file
   enumeration work elsewhere in the same server. Filed **DF-WARPFS-41**.
6. Restart persistence, stdout hygiene, and rate/reconcile stderr lines: all clean —
   0 non-JSON stdout lines across 6 client sessions (GAP-050 hygiene held; but the
   diagnostics doc still describes stdout INFO as a live trap — filed DF-WARPFS-42).

## The headline numbers (warm/cold)

| operation | warm | cold | notes |
|---|---|---|---|
| server handshake + 8-tool battery (one process) | 0.97–2.02 s | 2.02 s | includes process spawn + graph open; per-call amortized ≈ nothing |
| `tools/list` round (init+list) | 0.01 s | — | |
| CLI `graph stats` (hyperfine, n=20) | 19.1 ms ± 0.7 | — | release build |
| CLI `graph impact` depth 3 (hyperfine, n=20) | 39.4 ms ± 2.7 | — | release build |
| `graph warm` (cold cache) | — | 9.06 s | 6593 edges / 659 files, 4 languages |
| `graph warm` (all cached) | 0.09 s | — | |
| `classify` | 0.80 s | — | |

No perf row filed: nothing here is slow enough that a user would notice. The MCP round
trip is dominated by process spawn (~1 s) — a persistent client connection amortizes it;
in-server answers are in the tens-of-ms band. That matches the README's promise (~0.02 s
stats; impact ~0.5 s) — actually beaten (39 ms vs ~500 ms claimed).

## Install leg (ephemeral bunker, bunker-las-02)

Agent 87de5bfd, launch 14:10Z, collect + destroy same session (verified gone from
`bunker list`). toolchain-bootstrap cells all OK (rust stable, zig cc wrappers, GNU make,
compose + buildx plugins). **fresh-install: ENV-BLOCKED (4th consecutive run):** the bare
agent image lacks `pkg-config` and has no sudo; the build died in a `*-sys` build script.
The README's own documented install path (`sudo apt install ...`) cannot run on this image.
Filed DF-WARPFS-44 (recurrence of DF-WARPFS-25's root cause, with the fix options).

## Verdict

🟡 **PROMISING-BUT-ROUGH** — for the MCP surface specifically: the graph tools (impact /
search / understand / stats / metadata) are genuinely excellent — fast, honest errors,
persisted state, real answers about a repo the server had never seen. But the one tool an
agent needs for orientation (`vfs_list_directory`) silently returns empty on everything,
and a client that trusts it believes the project has no files. One broken tool in a 17-tool
surface, but it's the front door.

## Left behind in the repo

- This report: `docs/dogfood/2026-09-23-run12-mcp-server.md`
- Diagnostics trail: `docs/dogfood/diagnostics.md` (Run 12 section)
- Agent skill: `skills/hilo-usage/SKILL.md` (Run 12 section: MCP battery + list_directory warning)
- Board rows: DF-WARPFS-41..44 on `.coding-hermes/board/tasks.jsonl` (append-verified)
- Log: `.coding-hermes/dogfood-log.md`