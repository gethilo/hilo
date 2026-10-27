# Tracked inventory: `.vfs/graph/edges.jsonl` (INV-001)

This is the policy that resolves how `.vfs/graph/edges.jsonl` relates to the
other files under `.vfs/graph/` — what a `hilo graph warm` may change in it,
when it is intentionally unchanged, and what to do when you want a fresh one.

## The contract

`.vfs/graph/edges.jsonl` is **inventory truth**, not a cache. It is tracked in
git (see `.gitignore`, which explicitly keeps `.vfs/manifest.yaml` and
`edges.jsonl` tracked while ignoring the rebuildable state around them), and
every graph query is answered from it (or from the DuckDB cache reconciled
against it).

A `hilo graph warm` may change it in exactly two ways:

- **Append, never rewrite (discovery).** Warm parses the tree and appends only
  edges that are not already present. Deduplication is by the tuple
  `(from, to, rel, provenance)` (`hilo-metadata/src/inventory.rs`,
  `append_edges_deduped`), so pre-existing lines survive verbatim and no line
  is ever written twice.
- **Prune rows the tree no longer supports (reconciliation, DF-WARPFS-170).**
  An append-only file never forgets, so before this pass a dependency the
  agent *removed* kept answering `impact`, `related`, `stats`, and `untested`
  as a live dependent — forever. Warm now drops two classes of stale row and
  atomically rewrites the file (temp file + rename, under the same advisory
  lock every writer takes; malformed lines are preserved verbatim):
  1. **a file endpoint that no longer exists** under the project root — the
     `from` or `to` of a row names a path, and that path is gone (deleted or
     moved). Applies to every warm, including `--changed` and `--language`.
  2. **a row its source file no longer emits** — the file is still on disk but
     the import was deleted from it. Applied only on a *complete* derivation
     (no `--changed`, no `--language`), because a partial pass cannot tell
     "removed" from "not looked at". Graph node endpoints (`pkg:`, `std:`,
     `sys:`, `local:`, `iface:`, `type:`, `external:`) are never treated as
     paths, and cross-repo `external:` rows are never pruned by rule 2 (they
     are only derivable with `--workspace`).

  The pass is idempotent: a reconciled inventory is not rewritten at all, so a
  re-warm of an untouched tree still leaves `git status` clean for it. Warm
  prints `Pruned N stale edge(s) from edges.jsonl: …` when it removes rows, and
  mirrors the removal into the DuckDB cache (delete + re-stamp), so queries see
  the pruned graph immediately rather than waiting for someone to delete
  `graph.db`.
- **Intentional unchanged case.** When every discovered file hits the parse
  cache, all derived `service_call` / `service_contract` edges are already
  present, and the prune pass removes nothing, warm takes its fast path and
  does not write edges.jsonl, graph.db, or `.last_warm` at all. The output line
  `[all cached, graph unchanged]` marks this; a re-warm of an untouched tree is
  expected to produce a clean `git status` for `.vfs/graph/edges.jsonl`.
- **Refresh on cache miss is intentional.** When the parse cache misses
  (new files, changed files, or a missing/removed cache), warm re-parses and
  appends newly discovered edges to the tracked inventory. The refresh may be
  a commit's worth of lines (the post-commit hook runs `hilo graph warm
  --changed`), or a large append the first time a new edge pass lands — e.g.
  the `service_call` / `service_contract` passes appended hundreds of lines
  to corpora whose parse cache was still complete, exactly as designed
  (GAP-081 fast-path trap). This is the intended contract, not accidental
  drift; commit the appended lines together with the change that produced
  them.

## What is NOT the inventory (rebuildable cache artifacts)

Everything else under `.vfs/graph/` is a rebuildable cache artifact and is
gitignored:

| File | Role |
|------|------|
| `graph.db` | DuckDB query cache, loaded from edges.jsonl at open time; reloaded/reconciled against the JSONL whenever the stamp (mtime+size) says the JSONL moved, and since GAP-094 resumed from the checkpoint offset rather than replayed from byte 0. A missing graph.db is not an error — commands rebuild it. |
| `.parse_cache.json` | Per-file parse cache (mtime + size), the PERF-002 incremental-warm state. Each entry also carries the file's search-index vocabulary — definition names (`symbols`) and comment/docstring tokens (`doc`) — so a query reads it instead of re-parsing the corpus (DF-WARPFS-76); an entry from an older warm simply lacks those keys and is re-derived (and re-recorded by the next warm). |
| `.last_warm` | mtime marker that scopes `warm --changed`. |
| `.last_reconcile` | PERF-001 fingerprint stamp (mtime+size of edges.jsonl) letting read-only opens skip cache re-validation; since GAP-094 also a checkpoint — consumed byte offset, a digest of that prefix, and whether the ingest reached EOF (`v2:` line). A stopped-at-budget checkpoint is a resume point, never a claim that the cache is fresh. |

None of these are ever committed. If a `warm` (or any command) shows changes
in these files in `git status`, they are noise from a stale gitignore, not
inventory drift.

## The ignore contract is installed by `hilo init`

`hilo init` appends a managed block to `.gitignore` (DF-WARPFS-38) covering
the rebuildable cache files above — `graph.db`, `graph.db.wal`, `graph.duckdb`
(+ wal), `.last_warm`, `.parse_cache.json`, `.last_reconcile` — so the first
ordinary commit in a fresh project does not add a multi-megabyte `graph.db`
or parse caches. The block is marked with a
"# --- hilo managed: rebuildable .vfs cache state (installed by hilo init)"
comment line and is idempotent: re-init never duplicates it, and any
pre-existing `.gitignore` content is preserved verbatim. `.vfs/manifest.yaml` and
`.vfs/graph/edges.jsonl` are inventory truth and are deliberately **not**
ignored. Projects initialized before this change: add the managed block by
hand or re-run `hilo init` (idempotent).

## Deliberate inventory refresh (operator recipe)

The inventory can legitimately lag the tree: hand-edited or committed-before-
discovery lines that no longer parse to edges, or edges for files/moves that
only a full re-parse would notice. To produce a fresh, complete inventory:

```bash
hilo graph clean    # deletes edges.jsonl, graph.db, .parse_cache.json, .last_warm
hilo graph warm     # full re-parse; writes a fresh edges.jsonl
git add .vfs/graph/edges.jsonl
git commit
```

`graph clean` is no longer the only command that removes rows: `graph warm`
prunes stale ones (above). `graph clean` is still the only command that
deletes the tracked inventory *wholesale*: it removes `edges.jsonl` together
with the cache artifacts above so the next warm re-parses from scratch (the
reset path for a corrupted inventory or a parser/edge-model change you want
re-derived from nothing). That deletion is intentional; only run it when you
intend to recommit a full refresh of the inventory. There is no merge step:
the next warm appends, and the dedup tuple `(from, to, rel, provenance)`
guarantees no duplicate lines — so an operator-level "detect stale inventory"
check is a plain git diff after a warm: **additions** are legitimate refresh,
**deletions** are the prune pass reconciling removed files/imports (review them
against the change you made — a deletion with no corresponding removal in your
diff is worth a second look), and **modifications** of surviving lines cannot
come from warm at all (rows are kept verbatim) and mean the file was edited
outside the tool.

## Coverage

`hilo-cli/src/commands/graph.rs` pins this contract in
`warm_refreshes_tracked_edges_jsonl_append_only_and_rewarm_adds_no_duplicates`:
first warm persists discovered edges into the inventory; a fully-cached warm
leaves it untouched; a cache-miss warm re-appends next to a surviving
hand-written sentinel line exactly once; and a repeated cache-miss warm adds
no duplicates. It runs through the real `run_warm_in` path (the production
entry `run_warm` is a thin wrapper over it).

The prune half is pinned in three places:

- `warm_prunes_edges_for_deleted_files` (hilo-cli) — delete a file from a warmed
  fixture, re-warm, and assert no row mentioning it survives in
  `edges.jsonl`, that the DuckDB cache answers `stats`/`related` from the
  pruned graph, and that a second warm is a no-op.
- `warm_prunes_removed_imports_on_rederive` (hilo-cli) — delete the `use` line
  from a parsed file, re-warm, and assert the phantom `imports` row is gone
  (the case an append-only inventory could never reconcile).
- `hilo_graph::prune::tests` (hilo-graph, run by the guard) — the two rules,
  the node-endpoint exemption, `--changed`/`--language` scoping, idempotence
  (a reconciled file is never rewritten), malformed-row preservation, and the
  cache mirror + checkpoint re-stamp.
