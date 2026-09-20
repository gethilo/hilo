# Hilo Dogfood — 2026-09-20 (warpfs project, hilo product)

Real-use run by the warpfs-dogfood lane at workdir
`/home/kara/.hermes/stand-in/dogfood/warpfs` (board symlinked to the repo
board). Verdict: SHIPPABLE with two P2 usability findings.

## Promise exercised

"A user can give an AI agent a pre-built map of a codebase (dependencies,
entrypoints, test coverage, blast radius) without burning context on file
reads." Exercised as a real consumer: install from source, init, warm,
stats, impact, related, search, module, untested, classify, xattr
metadata, MCP server over stdio — on a scratch clone of hilo itself at
HEAD a8a401d, and again on a fresh bunker machine from a public clone.

## What worked (real-use evidence)

- `hilo init` — 9 ms. Manifest + git hooks.
- `hilo graph warm` (cold) — 6.2 s over 103 files, 968 edges across 97
  files. Incremental warm after touching one file: 0.13 s, 102/103
  parse-cache hits, 2 delta edges (skip path + delta insert both real).
- `hilo graph stats` — 770 distinct / 958 raw edges, edge-type breakdown,
  orphan list. ~30 ms.
- `hilo graph impact <bare path>` and `pkg:hilo_graph` — correct
  transitive results with depth/scope/confidence annotations.
- `hilo graph related <bare path>` — exact edge list, external-package
  edges labeled.
- `hilo graph search` — returned lexical results; edge-aware paths present.
- `hilo graph module`, `untested` (68 files listed), `classify`.
- `hilo graph clean` → `warm` → `stats` — full rebuild cycle healthy
  (GAP-070 regression check passed; the previous "clean strands the user"
  behavior is gone).
- xattr: `setfattr -n user.vfs.feature -v dogfood-test <file>` then
  `hilo meta <file>` shows it. Metadata, not injection — held true.
- MCP: `hilo serve --mcp` over stdio — initialize OK, tools/list = 17
  tools (README says 17: accurate).
- Bunker fresh-install (Debian 13 trixie, nothing preinstalled): public
  clone at a8a401d → rustup minimal → `cargo build --release` RC=0 in
  1146s → `--version`, init, warm, stats all pass. Agent destroyed after.

## Findings (board rows)

- DF-WARPFS-3 (P1) — bunker spawn infra: get.docker.com throughput
  collapse kills fresh spawns mid rootless-docker install (server-side
  budget, not CLI); `rootlessInstallerCacheDir` seam exists but unset.
- DF-WARPFS-1 (P2) — README hard-requirement wording for libfuse3-4 vs
  reality on a fresh Debian box.
- DF-WARPFS-2 (P2) — id-form UX on impact/related: the `file:` prefix
  form rejected valid paths a bare path accepted; error strings don't
  teach the three id shapes.
- DF-WARPFS-4 (P2) — the value case re-proven at HEAD; record only.

 See `.coding-hermes/board/tasks.jsonl` rows DF-WARPFS-1..4.

## The right way (for the next agent)

1. Build: `cargo build -p hilo-cli --release` (19 s incremental locally;
   cold ~19–20 min on a bare box due to duckdb-sys; clang/CMake NOT needed).
2. Run the CLI from the repo root after `hilo init`.
3. Address graph nodes as bare repo-relative paths (works); `sys:` for C/C++
   headers, `pkg:` for external packages. `file:` prefix is NOT an accepted
   input form on impact/related today.
4. MCP probing: newline-delimited JSON-RPC over stdio (initialize →
   notifications/initialized → tools/list).
5. Bunker legs: use `bunker exec` only for short probes; long installs via
   direct ssh with a HOME-absolute prompt.
