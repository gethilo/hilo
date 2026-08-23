# Hilo Integration Report — 2026-08-23

Second real-use dogfood run (first: 2026-08-13, 🟡 PROMISING-BUT-ROUGH with
P0 GAP-034). Corpus this time: a fresh `serde-rs/serde` clone (208 Rust
files, 749 edges, 185 files with edges) — deliberately different from the
ripgrep corpus of run 1. Binary: built from master per README
(`cargo build --release -p hilo-cli`, 5m36s with warm target cache).
Verdict: 🟡 **PROMISING-BUT-ROUGH** — materially improved, flagship query
still under-reports.

## What was verified FIXED (all previously-broken paths now work)

| Claim (from run 1 / board) | Result on serde |
|---|---|
| GAP-034 P0: file-level impact structurally empty | ✅ `hilo graph impact serde/src/lib.rs` returns real file dependents (was "No dependents found" ALWAYS) |
| GAP-034: related reverse empty | ✅ `related serde/src/lib.rs --direction reverse` returns 6 importer files |
| GAP-035: `pkg:{` brace-group garbage | ✅ 0 occurrences in edges.jsonl (was 27/256 on ripgrep) |
| GAP-036: classify misses tests/ dirs | ✅ 151/208 files classified test; `graph untested` (38) no longer lists test files |
| GAP-043: intra-crate file→file edges | ✅ 177 file→file edges (e.g. serde_derive/src/ser.rs → serde_derive/src/de.rs) |
| GAP-044: `understand` symbols for symbol-rich files | ✅ serde_core/src/de/mod.rs MAP shows IgnoredAny, Unexpected, fmt::Display, ... |
| GAP-045: bare `pkg:*` in related output | ✅ labeled `[external package]` |
| Speed claims | ✅ impact 0.84s (release), warm 35s debug, init 30ms, classify 1.2s |
| Determinism | ✅ `graph clean` → `graph warm` → identical 598 distinct / 749 raw |
| Git hooks | ✅ post-commit runs `hilo graph warm --changed` (incremental, 1 file) |
| Multi-language | ✅ 6 files / 6 languages (py, go, ts, c, h, rs) parsed in one warm |
| Push parity (trust) | ✅ local master == github.com/gethilo/hilo mirror (b69ee76) |

## The working recipe (still valid, now with file-level queries)

```bash
hilo init && hilo graph warm && hilo classify      # ~40s on 208 files
hilo graph impact serde/src/lib.rs                 # file-level blast radius WORKS now
hilo graph related serde/src/de/mod.rs             # forward deps
hilo graph understand serde_core/src/de/mod.rs     # symbols (messy but real)
hilo graph search "Deserialize derive" --limit 5   # lexical search
hilo meta --set feature --value demo <path>        # xattr write; read via getfattr
hilo serve --mcp                                   # 15 tools over stdio
hilo mount /mnt --daemon                           # FUSE; fusermount -u /mnt to stop
```

## What still breaks / frictions (new findings)

1. **GAP-048 (P0)** — blast radius under-counts by ~96%: `impact serde/src/lib.rs`
   → **6 dependents** but **148 files** import serde. Root cause: the GAP-034
   resolution layer matches only exact `pkg:serde` targets (7 edges); the 53+
   brace-expanded `pkg:serde::<member>` edges (from `use serde::{...}` — the
   common form) never resolve to the file. Same under-count for `'pkg:serde'`
   and MCP `vfs_graph_impact`.
2. **GAP-049 (P1)** — classify role metadata is wrong on core files:
   serde/src/lib.rs (crate root, 148 importers) → role `unknown`; only 8 files
   get `library`; `entrypoint` = 4 build.rs scripts (serde has no binaries).
3. **GAP-050 (P2)** — MCP server logs an INFO event to **stdout** at startup;
   naive JSON-RPC clients misparse it as the initialize response. My first
   client broke on this (run 1 reported "zero protocol friction" — that client
   must have skipped lines).
4. **GAP-052 (P2)** — zero `tested_by` edges ever emitted; `graph untested`
   still means "all non-test files", not "no test coverage".
5. **GAP-051 (P2)** — SKILL.md line 25 `cargo build --release -p hilo_cli`
   fails; the crate is `hilo-cli` (hyphen — only hyphenated crate in the
   workspace). GAP-047 fixed the `hilo_graph` test lines but missed this one.

## Errors hit and their fixes (this run)

| Error | Cause | Fix / workaround |
|---|---|---|
| `cargo build --release -p hilo_cli` → "package ID specification did not match" | crate is `hilo-cli` | use `-p hilo-cli` (task GAP-051) |
| MCP client `KeyError: result` on initialize | startup INFO log line on stdout consumed as response | skip non-JSON-RPC lines; server-side fix GAP-050 |
| `getfattr -n user.vfs.role <crate-root>` → `unknown` | classify role heuristics miss crate roots | task GAP-049 |

## What a NEW user should know

- Build: `cargo build --release -p hilo-cli` (NOT `hilo_cli`). Binary is `hilo`.
- **Blast-radius results are lower bounds, not truth** — verify with
  `pkg:<crate>` exact form and cross-check counts against `graph stats`.
  GAP-048 is open; treat small impact counts with suspicion on brace-heavy code.
- `graph untested` is not a coverage tool yet (GAP-052).
- MCP clients should tolerate a leading INFO line on stdout (GAP-050).
- Everything else from run 1's recipe stands: pkg: form for external deps,
  meta --set attr first then --value then path, mount --daemon + fusermount -u.
