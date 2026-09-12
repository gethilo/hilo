# Hilo Dogfood Run 4 — Python corpus (fastapi), 2026-09-12

**Binary:** master b780fb8 (0.3.0, built 2026-09-11 05:23) — post GAP-057
(Go resolution), GAP-059 (loud not-found), CI-002 (clippy).

**Corpus:** `github.com/fastapi/fastapi` @ 50113da — 1138 `.py` files across
`fastapi/` (library), `tests/` (450+ test files), `docs_src/` (240+ tutorial
apps), `scripts/`. A deliberately hostile corpus for a graph tool: same
import name (`fastapi.routing`) reached through two alias styles, thousands
of small self-contained tutorial apps, and package `__init__` chains.

**Verdict: ✅ SHIPPABLE for Rust and Go, 🟡 for Python.** The Python gap is
one family: the query-time file→package resolution (GAP-057, fixed for Go in
9431cac) was not generalized. Everything else re-verified clean.

## The real-use loop (all commands as a user would run them)

```bash
git clone --depth 1 https://github.com/fastapi/fastapi.git && cd fastapi
hilo init          # 0.016 s  (installs git hooks post-commit/post-merge)
hilo graph warm    # 15.3 s   5782 edges across 936 files (2 languages)
hilo classify      # 0.13 s   389 library / 121 unknown / 27 entrypoint
hilo graph stats   # 0.028 s  byte-identical across runs (determinism held)
```

## What worked (re-verified in real use)

- **pkg-form blast radius is EXACT on Python.** Ground truth via grep:
  10 files under `fastapi/` + `tests/` import `fastapi.routing`;
  `hilo graph impact 'pkg:fastapi.routing' --max-depth 2` returned all 10
  plus 7 `docs_src/` importers my grep scope didn't even cover — every edge
  `ast_exact conf=1.0`, zero false positives.
- **Queries are instant at this scale:** stats 28 ms, impact 20 ms,
  search 23 ms, untested 19 ms, understand 158 ms. README performance
  section no longer overstates (GAP-060 verified from the good direction).
- **Loud failure modes landed (GAP-059/062 CLI side).** Unknown paths now
  error with exit 1 on both `impact` and `related`:
  `'no/such/file.py' is not in the graph (no such file and no matching
  graph node)`. Non-indexable files fail fast and clearly:
  `'README.md' is not an indexable source file (26 AST languages)` —
  PERF-004's instant guard, re-confirmed.
- **SIGPIPE is quiet (GAP-063 fixed).** `hilo graph untested | head -1` and
  `graph stats | head -1` → exit 0, no panic output. Re-piped commands are
  safe in scripts now.
- **MCP: 17 tools, pure stdout, NDJSON framing.** Drove `initialize` →
  `tools/list` → 5 `tools/call` with a hand-rolled JSON-RPC client. Every
  response well-formed; tracing INFO goes to stderr only (GAP-050 still
  fixed). Note for client authors: the server speaks **line-delimited
  JSON-RPC** (per the MCP stdio transport spec), NOT LSP-style
  `Content-Length` framing — a header-framed initialize gets
  `-32700 Parse error`.
- **FUSE end-to-end:** `hilo mount <dir> --daemon` → instant return,
  `ls`/`cat` serve real content, xattr passthrough works
  (`getfattr -n user.vfs.role` → `library`), `fusermount3 -u` clean.
- **meta round-trips** when you use the documented syntax
  (`--set <attr> --value <val> <path>`) — read back through CLI, getfattr,
  and MCP consistently.
- **tested_by edges are now emitted on Python** (2225 on this corpus —
  run-1's "never emitted" is long fixed). Edge shape is sound: a test file
  emits BOTH `imports` and `tested_by` to the same `pkg:` node.

## What failed (each = a board row)

1. **GAP-064 (P1) — Python file-form impact/related silently empty.**
   `hilo graph impact fastapi/routing.py --max-depth 3` → "No dependents
   found" (rc 0). Grep truth: 10 importers. The edge data is perfect; the
   file→`pkg:fastapi.routing` lookup is missing (Go-only fix in 9431cac).
   MCP `vfs_graph_impact` file-form → `{"dependents":[],"total":0}`.
2. **GAP-065 (P2) — warm's silent exclusion, now quantified on Python.**
   Warm parsed 936 of 1138 `.py` files and said nothing. 203 missing =
   183 `__init__.py` (only 4/187 indexed — plausibly policy, definitely
   undocumented) + 20 real parse misses (e.g.
   `docs_src/python_types/tutorial001..008_py310.py`) with zero warnings.
3. **GAP-066 (P2) — coverage collected but not consumed.**
   `tests/test_sse.py → tested_by → pkg:fastapi.routing` exists (8 such
   edges for routing), yet `graph untested` lists `fastapi/routing.py` AND
   `fastapi/applications.py`, and `graph module fastapi` reports
   `Tests: 0.0%`. The untested/module consumers don't resolve file→pkg.
4. **GAP-067 (P2) — `meta --set` accepts malformed syntax silently.**
   `hilo meta f.py --set role=core-router` (docs form is
   `--set <attr> --value <val> <path>`) exits 0 and creates an xattr
   *named* `user.vfs.role=core-router` with an empty value. Found by
   natural typo; verified via getfattr + MCP.
5. **GAP-068 (P3) — README requirements over-specify.** `libfuse3-dev` is
   only needed for `hilo mount`; clang/CMake are not needed at all
   (second consecutive clean-room build confirms).
6. **GAP-062 (P2, existing — re-confirmed via MCP).**
   `vfs_graph_related` on a ghost path returns `[]` with `isError=false`
   while the CLI now errors loudly. The 488ee3d guard never reached MCP.

## Workaround that works today (Python repos)

Resolve the file's package yourself and query the `pkg:` node:

```bash
# fastapi/routing.py → pkg:fastapi.routing
hilo graph impact 'pkg:fastapi.routing' --max-depth 3
# coverage, until GAP-066 lands:
grep '"tested_by"' .vfs/graph/edges.jsonl | grep 'pkg:fastapi.routing' | wc -l
```

## Time-to-first-success & friction

- init → warm → first exact blast-radius answer: **~16 s** (warm included).
- Friction count: 6 (one per board row above). Zero blockers for Rust/Go
  repos; Python users must know the pkg-form workaround.

## Bunker install leg (las-bunker-03, agent 5aec2a70)

- Clone of the public repo inside the bunker: OK (b780fb8).
- rustup minimal profile: not present by default (docs finding, benign).
- `cargo build --release`: first attempt hit a 590 s ssh timeout mid
  duckdb-sys (~7.5 min in); relaunched and the incremental build resumed
  from the warm target/ — see `.coding-hermes/dogfood-log.md` for the
  final numbers and smoke result.

## Judge's answers

1. **Does it work?** Yes for Rust/Go; Python loses file-form queries only.
2. **Is it useful?** Yes — 20 ms exact blast radius is the whole pitch,
   and it delivered it via pkg-form on a third language family.
3. **Is it usable?** The pkg: form is still not discoverable from an empty
   result (no "try pkg:<pkg> instead" hint on file-form empties).
4. **Is it trustworthy?** Determinism, loud errors, and pure MCP stdout all
   held; the silent warm exclusion (GAP-065) is the one trust dent.
