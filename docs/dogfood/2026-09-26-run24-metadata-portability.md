# Run 24 — Metadata portability across a fresh clone (the two-machine collaboration promise)

**Date:** 2026-09-26 · **Head under test:** 6d443df · **Binary:** hilo 0.3.1-dev (v0.3.0-14-g2b26d3f, local release build)
**Corpus:** gin-gonic/gin @ master (99 Go files → 879 edges, 94 files contributing)
**Verdict:** 🟡 PROMISING-BUT-ROUGH — the map travels; the annotations do not.

## The promise under test

README: *"Give your AI coding agent a pre-built map of every codebase it touches —
dependencies, entrypoints, test coverage, blast radius."* The obvious collaboration
loop: agent A enriches a repo (annotations + warmed graph), pushes; agent B clones
and immediately has the map AND the knowledge. Runs 1–23 never tested the clone
boundary — this run is machine A → machine B end to end, plus the crash story of
the artifacts that cross it.

## Setup (what a real two-agent team does)

```bash
# machine A
git clone https://github.com/gin-gonic/gin && cd gin
hilo init                                   # installs post-commit/post-merge hooks
hilo meta gin.go --set user.vfs.role --value "core-router: ..."
hilo meta gin.go --set user.vfs.feature --value "FND-101\nentrypoint"
hilo meta tree.go --set user.vfs.role --value "route-tree: radix tree, hot path"
hilo meta context.go --set user.vfs.role --value "request-context: public API surface"
hilo graph warm                             # 2.1s cold, 879 edges
git add -A && git commit -m "gin + hilo metadata" && git push

# machine B (fresh clone of A's repo)
git clone <remote> && cd gin
hilo meta gin.go          # ? (see finding DF-WARPFS-95)
hilo graph stats          # ? (see what worked)
```

## What worked (real positives)

- **The dependency graph crosses the clone boundary perfectly.** B reads A's
  committed `.vfs/graph/edges.jsonl` + `graph.db` with **zero re-warm**:
  `graph stats` (879 distinct edges, tested_by/tests breakdown, orphans list),
  `graph impact tree.go` (3 dependents with confidence labels) — **0.02s cold**.
  This is the flagship promise delivering: B's agent has blast radius instantly.
- **`hilo meta --set` round-trips locally** including a literal `\n` multiline
  value (the `--value` help is honest about the escape).
- **The installed git hooks are harmless and honest** (`hilo graph warm
  --changed 2>/dev/null || true`) — they never corrupt a commit.
- **Xattr and CLI views agree**: `getfattr -d -m - gin.go` shows exactly what
  `hilo meta` prints.
- **Crash does not touch metadata**: a SIGKILL during warm leaves annotations
  readable; the damage is confined to the graph cache (see DF-WARPFS-94).

## The break (findings → rows)

### DF-WARPFS-95 (P1) — annotations die at the push

Machine B, fresh clone: `hilo meta gin.go` → **"No Hilo metadata"**. Root cause
mechanism, verified at each hop:

1. `meta --set` writes **only** filesystem xattrs (`user.vfs.*`). `.vfs/features/`
   — which init creates — stays **empty** after 4 annotations.
2. Git cannot carry xattrs, and the commit carries no annotation data in any
   other form (the 6 committed `.vfs` files are manifest + graph artifacts only).
3. No escape hatch exists: `hilo --help` has no export/import/backup subcommand;
   the manifest's `metadata.namespaces: []` declared store is unwired.

Net: the knowledge layer a human or agent curates is invisible to every other
machine. The graph (derived, rebuildable) travels; the annotations (source of
truth for intent) do not — the portability is exactly inverted from the design
rules, where DuckDB is the rebuildable artifact.

### DF-WARPFS-96 (P2) — init doesn't gitignore the derived DuckDB

`git check-ignore .vfs/graph/graph.db` → not ignored. The `git add -A` a real
user runs ships a **1.0MB binary** DuckDB next to the 100KB JSONL it is derived
from. Harmless but wrong-size; fix is a `.gitignore` append in `init` + a README
sentence naming which `.vfs` files belong in git.

### DF-WARPFS-94 (P1) — a killed warm poisons every later query until `graph clean`

Killed `hilo graph warm` 0.9s into its 2.1s parse (SIGKILL). State after:
`edges.jsonl` intact (879 lines), `.parse_cache.json` present, `graph.db`
freshly created and **empty** (12KB + wal). Then the trap:

```
$ hilo graph warm
  parse cache: 99/99 files skipped (unchanged)
  Discovered 879 edges ... [all cached, graph unchanged]     # ← rebuilds nothing
$ hilo graph stats
Total edges: 5 distinct / 879 raw (edges.jsonl)              # ← silent wrong answer
$ hilo graph impact tree.go
No dependents found for 'tree.go'                            # ← silent wrong answer
```

Every query reads the empty DuckDB and confidently reports nonsense. Only
`hilo graph clean` (undiscoverable from any of the messages) + re-warm recovers.
Mechanism: warm's short-circuit checks parse-cache freshness, never graph.db
population. Root cause submitted to the pre-solve lab: `duckdb-graph-cache-stale-after-crash-warm-says-cached` (sub_b51845).

## The workflow that works TODAY (be honest with B-side agents)

- Commit `.vfs/manifest.yaml` + `.vfs/graph/edges.jsonl` (skip `graph.db`, see
  DF-WARPFS-96 — B re-derives it or reads JSONL).
- Expect the graph on B to be immediately usable; re-warm only after pulls
  (the post-merge hook does this — though note DF-WARPFS-83: the hook's dirty
  flag writer is dead code, so after a pull run `hilo graph warm` by hand).
- **Re-declare annotations on B** (or script a `meta --set` manifest you keep
  in the repo) — nothing else carries them yet.
- After ANY killed/interrupted warm: `hilo graph clean && hilo graph warm`
  before trusting stats/impact numbers.

## Numbers (Step 2b)

| Operation | Time | Notes |
|---|---|---|
| `hilo init` | <0.1s | + hooks install |
| `hilo graph warm` (cold, 99 files) | 2.1s | full tree-sitter parse |
| `hilo graph warm` (cached) | <0.9s | parse-cache short-circuit |
| B: `graph stats` (shipped graph, cold) | 0.02s | no re-warm needed |
| B: `graph impact tree.go` (cold) | 0.02s | 3 dependents, conf labels |
| commit with 1MB graph.db | n/a | DF-WARPFS-96: should not ship |

No PERF row: nothing here makes a user wait beyond the already-filed DF-76
search family; the clone-boundary numbers are the point and they're fast.
