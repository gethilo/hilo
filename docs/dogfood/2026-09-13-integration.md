# Hilo Dogfood Run 5 — Integration Report (vite TypeScript corpus, 2026-09-13)

**Binary:** master `6f0430b`, release build, hilo 0.3.0 (freshly rebuilt — the
prior binary predated the GAP-064/066/067/068 fixes).
**Corpus:** vitejs/vite @ main (tarball snapshot, 2026-09-13) — 575 scoped
`.ts` files, warm parsed 1530 candidates → 908 files / 4168 edges / 2
languages. Fifth real-use run, **first JavaScript/TypeScript language
tested** (prior: Rust ×2, Go, Python).

**Verdict: ✅ SHIPPABLE for Rust/Go/Python pkg-form workflows, 🟡 for
TypeScript/JS** — the file→module resolution gap that hit Go (run 3) and
Python (run 4) reappears one language later, plus a new `graph clean →
warm` trap that strands users who follow the tool's own instructions.

## The real-use workflow that works today (TS/JS)

```bash
hilo init                     # 7ms; warns loudly about missing .git/
hilo graph warm               # 13.3s on vite (1530 candidates); NOW reports
                              #   exclusions: "Excluded 14 ... (node_modules: 6,
                              #   hidden: 8)"  ← GAP-058 fix verified live
hilo graph stats              # 25ms; 4055 distinct / 4168 raw edges
hilo graph impact 'pkg:vitest' --max-depth 1   # exact, 18ms
hilo graph related 'local:./pluginContainer' --direction reverse
                              # ← THE TS/JS workaround: query the exact
                              #   relative specifier, not the file path
hilo graph module packages/vite/src/node/server
hilo graph search "plugin container rollup"    # 20ms
hilo classify                 # 390 test / 390 library — sane roles
hilo meta --set role --value core <path>       # GAP-067 fix verified:
hilo meta <path> --set role=core               #   now rc=1 + usage line
hilo mount /mnt/vfs --daemon  # instant; xattr passthrough + cat verified;
fusermount -u /mnt/vfs        #   clean unmount
hilo serve --mcp              # 17 tools, NDJSON, stdout pure JSON
```

## What does NOT work on TS/JS yet (new rows GAP-069..072)

1. **File-form blast radius is silently empty (GAP-069, P1).**
   `hilo graph impact packages/vite/src/node/server/pluginContainer.ts`
   → "No dependents found", rc 0. Ground truth: 24 incoming edges from 13
   importers (grep: 14 importer files). MCP returns
   `{"dependents":[],"total":0}` with no error flag. TS/JS edges target
   `local:<relative-specifier>` nodes; the Go/Python file→pkg resolvers
   never covered this dialect. Per-specifier queries work but `./x` and
   `../server/x` are separate queries — N queries to see one file's
   dependents.
2. **`graph clean` → `graph warm` leaves the graph empty (GAP-070, P1).**
   clean deletes edges.jsonl + graph.db but not `.parse_cache.json`; the
   promised warm run short-circuits ("all cached, graph unchanged") and
   stats reports "Graph cache is empty." Only manual deletion of the parse
   cache unblocks warm. No `--force` flag exists.
3. **Coverage still 0.0% on TS/JS (GAP-071, P2).** 231 `local:`-targeted
   tested_by edges exist (e.g. forwardConsole.spec.ts →
   `local:../forwardConsole`) but untested/module don't resolve them;
   `graph module packages/vite/src/shared` says Tests: 0.0% with 8 spec
   files in-module. The GAP-066 fix covered pkg-targeted edges only.
4. **Search shows raw node names; understand MAP is hollow on TS
   (GAP-072, P2).** Hits labeled `local:../pluginContainer` instead of
   repo paths; understand anchors an unrelated docs file with five empty
   symbol bullets.

## Verified clean this run (re-confirmed + newly fixed)

- **GAP-058 fix live:** warm exclusion report prints per-category counts.
- **GAP-067 fix live:** `--set role=core` → `error: invalid attribute name
  "role=core" — usage: ...`, rc 1; correct syntax echoes the canonical
  `user.vfs.role = core` pair.
- **GAP-059 loud errors:** unknown paths error on impact AND related
  ("no such file and no matching graph node") — the improved message was
  correct even when *my* path was wrong (it named both checks).
- Byte-determinism: stats byte-identical across a forced full rebuild
  (4055/4168/908 + full orphan list, 926 lines).
- Incremental warm: `touch` 1 file → `warm --changed` reparsed exactly
  1 file (24 edges), ~1s.
- FUSE `--daemon`: instant mount, `getfattr user.vfs.role` through the
  mount, `cat` works, clean unmount.
- MCP: 17 tools, 0 non-JSON stdout lines, NDJSON framing, provenance
  (`ast_exact conf=1.0`) exposed in CLI and MCP output.
- Performance at vite scale: stats 25ms, impact 18ms, search 20ms,
  understand 175ms.
- Sync lane (the `warpfs-sync` scheduler row): tick RC=0, "6/6 keys
  verified", namespace event `/project/warpfs/status/2026-09-13-tick172-audit`
  landed fresh — the context-sync pipeline works end to end.

## Bunker install leg (las-bunker-03, agent da91c36c)

Bare Debian: no cargo/rustc/clang/cmake (gcc/make present).
`git clone` of the public repo at exact HEAD 6f0430b ✓ → rustup minimal
(1.98.1) → `cargo build --release` RC=0 in 1142s (19m02s, clean build —
README's 15-20 min claim accurate) → smoke below. Third box confirming
clang/CMake are unnecessary for the core path (GAP-068).

## Time-to-first-success

init 7ms → warm 13.3s → first **exact** blast-radius answer (`pkg:` form)
≈14s. First file-form answer that *looks* like success but is wrong:
≈14s — which is exactly the GAP-069 hazard.

## Friction count: 5

(GAP-069 silent-empty file-form; GAP-070 clean→warm trap; GAP-071
coverage 0.0%; GAP-072 search/understand output forms; no `--force` for
warm once the cache/graph state diverges.)
