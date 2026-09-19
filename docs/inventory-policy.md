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

A `hilo graph warm` may change it in exactly one way:

- **Append, never rewrite.** Warm parses the tree and appends only edges that
  are not already present. Deduplication is by the tuple
  `(from, to, rel, provenance)` (`hilo-metadata/src/inventory.rs`,
  `append_edges_deduped`), so pre-existing lines survive verbatim and no line
  is ever written twice.
- **Intentional unchanged case.** When every discovered file hits the parse
  cache and all derived `service_call` / `service_contract` edges are already
  present, warm takes its fast path and does not write edges.jsonl, graph.db,
  or `.last_warm` at all. The output line `[all cached, graph unchanged]`
  marks this; a re-warm of an untouched tree is expected to produce a clean
  `git status` for `.vfs/graph/edges.jsonl`.
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
| `graph.db` | DuckDB query cache, loaded from edges.jsonl at open time; reloaded/reconciled against the JSONL whenever the stamp (mtime+size) says the JSONL moved. A missing graph.db is not an error — commands rebuild it. |
| `.parse_cache.json` | Per-file parse cache (content hash + mtime), the PERF-002 incremental-warm state. |
| `.last_warm` | mtime marker that scopes `warm --changed`. |
| `.last_reconcile` | PERF-001 fingerprint stamp (mtime+size of edges.jsonl) letting read-only opens skip cache re-validation. |

None of these are ever committed. If a `warm` (or any command) shows changes
in these files in `git status`, they are noise from a stale gitignore, not
inventory drift.

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

`graph clean` is the only command that deletes the tracked inventory: it
removes `edges.jsonl` together with the cache artifacts above so the next
warm re-parses from scratch (the reset path for stale edges after renames or
moves — e.g. the historical `warpfs-*` entries). That deletion is
intentional; only run it when you intend to recommit a full refresh of the
inventory. There is no merge step: the next warm appends, and the dedup tuple
`(from, to, rel, provenance)` guarantees no duplicate lines — so an
operator-level "detect stale inventory" check is a plain git diff after a
warm: additions are legitimate refresh, modifications/deletions of existing
lines cannot come from warm at all (append-only) and mean the file was edited
outside the tool.

## Coverage

`hilo-cli/src/commands/graph.rs` pins this contract in
`warm_refreshes_tracked_edges_jsonl_append_only_and_rewarm_adds_no_duplicates`:
first warm persists discovered edges into the inventory; a fully-cached warm
leaves it untouched; a cache-miss warm re-appends next to a surviving
hand-written sentinel line exactly once; and a repeated cache-miss warm adds
no duplicates. It runs through the real `run_warm_in` path (the production
entry `run_warm` is a thin wrapper over it).
