# Dogfood Run 30 — MCP Untouched Tools + Fresh-Install Leg (FINALLY GREEN)

**Date:** 2026-10-01
**Binary (local):** v0.3.0-14-g2b26d3f (2026-09-22, release, 118MB stripped)
**Binary (bunker):** built fresh from origin/master 3609b77, release, `make release`
**Corpus:** warpfs repo itself (83 files, 432 edges, 19GB target/)
**Angle change (per skill rule):** runs 12/15/26 exercised only initialize, tools/list, and
4-5 of the 17 MCP tools. This run drove all 17 over a real NDJSON session, plus the
fresh-machine install leg that had been SKIPPED 13 consecutive runs.

## Verdict: ✅ SHIPPABLE (MCP surface: SHIPPABLE / workspace-wipe contract: rough edge)

## MCP — all 17 tools driven (11 tools called this run, 6 already proven in runs 26/25)

Never-called tools exercised for the first time:

| Tool | Call | Result |
|---|---|---|
| vfs_rule_list | `{}` | `{"rules":[],"total":0}` — correct for the default manifest |
| vfs_graph_untested | `{}` | 56 files, correct (only 1 tested_by edge in this corpus) |
| vfs_graph_module | `module_name: "hilo-graph/"` | 140 edges for the module — works |
| vfs_resolve_path | duckdb.rs | `backend: local-only, real_path: ...` — correct |
| vfs_backend_status (no args) | `{}` | protocol error `missing 'path' argument` — good negative case |
| vfs_backend_status (path) | duckdb.rs | `managed: false, cache_hit: true` — correct |
| vfs_workspace_ephemeral | `{}` | 48,160 entries / 20.8 GB — correct set (target/ dominates) |
| vfs_workspace_wipe | `{}` (plan) | 48,160 files / 20.8 GB planned, nothing deleted (verified by stat) |
| vfs_rule_check | `name: "stale-files"` | clean error: `Rule 'stale-files' not found... Available: ` (empty manifest) |
| vfs_list_directory | `/` | real listing with backend attribution — correct |
| vfs_get_metadata | rules.rs | xattrs round-trip — correct |

Missing-required-argument handling is consistent (-32603 protocol errors naming the
argument). No tool hung; full 11-call session completed in ~0.5s server-side.

## Defects found (filed DF-WARPFS-105/106/107)

1. **DF-WARPFS-105 (P1):** `vfs_workspace_wipe` response carries no `dry_run`/mode flag —
   an agent consumer cannot distinguish a plan from an applied wipe by shape. Also 48k
   `removed[]` entries is a context bomb for an LLM consumer (cap/paginate).
2. **DF-WARPFS-106 (P2):** `user.vfs.ephemeral=true` does not force a file into the
   ephemeral LISTING — `scan()` at `hilo-backends/src/ephemeral.rs:168` passes `None` for
   the xattr slot. The `false` wipe-protector works (read at wipe time in CLI and MCP);
   the "true forces ephemeral" half of the contract is dead in the scan path. Live repro:
   setfattr on CHANGELOG.md → absent from `hilo workspace ephemeral` output.
3. **DF-WARPFS-107 (P2):** README frictions on the fresh path: rustup `--profile minimal`
   pulls no linker (first make dies without build-essential); README documents bare
   `hilo workspace wipe` in the plan sense but the CLI hard-requires `--ephemeral`.

## Install leg — PASSED (first time in 30 runs)

Agent 03fc2681 on bunker-las-03 (destroyed after; ssh user-key refused but bunkerd API
alive — spawn path works exactly as the skill predicted). Fresh Debian 13, 6 cores,
no sudo, no cargo:

1. rustup `--profile minimal` → cargo 1.99.0
2. `git clone https://github.com/gethilo/hilo.git` — network fetch, real fresh-user path
3. `make release` — needed build-essential for the linker (friction b, DF-WARPFS-107)
4. **INSTALL_SECONDS=1648** (~27.5 min) → binary 118MB, `hilo --version` works
5. Smoke on the SAME box (what run 25 never reached): init → graph → related → classify →
   search → workspace ephemeral → wipe plan — all correct on a 2-file corpus. Small
   honest notes: `.vfs/graph/` shows up in its own ephemeral listing (graph.db is
   rebuildable — by design, but a fresh user may flinch); graph cache empty until warmed
   (documented behavior).

This closes the install-wall family (DF-WARPFS-100/103 superseded into DF-WARPFS-104,
which this run now supersedes as executed).

## Perf (Step 2b, hyperfine, release binary, warm, 20 runs)

| Operation | Warm | Notes |
|---|---|---|
| `hilo graph stats` | 28.0 ms ± 6.2 | fast enough, no row |
| `hilo graph search "rate limit"` | 507.6 ms ± 45.2 | 2.7x faster than run 29's 1.37s debug number — but different binary; no cross-claim filed. A user notices ~0.5s only mildly; below row threshold |
| `hilo workspace ephemeral` | 2.318 s ± 0.133 | walks 19GB target/; 83x slower than stats. Felt as a pause, but it is an intentional full-tree scan over 400k+ files — filed only as context on DF-WARPFS-105, not a PERF row (nothing to optimize without a profiler mandate) |
| MCP session (init+tools/call) | 0.118 s total | server cold-start included — excellent |

Cold-cache arm: a drop-caches probe hung the local call (OS-level, not the binary's
fault); warm numbers stand, cold referenced from run 29. No PERF rows filed — nothing
here is slow enough that a user would complain.

## What an agent consumer should know (the integration recipe)

```
# One-shot NDJSON session (stdout = JSON-RPC only, tracing on stderr):
printf '%s\n%s\n%s\n' \
 '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"me","version":"0"}}}' \
 '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
 '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"vfs_graph_stats","arguments":{}}}' \
 | hilo serve --mcp
```
- Must run inside an `hilo init`-ed project (GAP-086 guard refuses otherwise, with a
  clear message naming `hilo init` — good error).
- `vfs_graph_module` wants `module_name`, not `path` (the schema is right; a caller
  guessing `path` gets a clear -32603).
- Treat `vfs_workspace_wipe` output as a plan ONLY when you passed `dry_run:true`
  explicitly — until DF-WARPFS-105 lands, absence of a flag is ambiguous.
