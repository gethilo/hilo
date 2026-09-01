# Hilo Performance (2026-09-01 baseline)

Numbers from the 2026-09-01 performance sprint, measured with the release
binary on a 16-core / 59 GB Linux box (not loaded) against three real Rust
corpora: [ripgrep](https://github.com/BurntSushi/ripgrep) (110 files),
[clap](https://github.com/clap-rs/clap) (330 files), and
[tokio](https://github.com/tokio-rs/tokio) (793 files). Best of 3 runs,
`/usr/bin/time -f '%e %M'` wall-clock and max-RSS.

## Query latency (the day-to-day path)

Every CLI invocation opens the graph database. Before the PERF-001 fix,
each open re-validated the entire `edges.jsonl` cache at full price —
even for read-only one-shot queries. Now a fingerprint stamp
(`.vfs/graph/.last_reconcile`, mtime+size of `edges.jsonl`) lets open()
trust a fresh cache and skip straight to the query. Any writer that
appends edges (write-through triggers, `graph warm`, external processes)
changes the file and the stamp invalidates — correctness preserved.

| Command (tokio, 793 files) | Before | After | Speedup |
|---|---:|---:|---:|
| `hilo graph stats` | 12.70 s | **0.03 s** | 497x |
| `hilo graph related <file>` | 12.78 s | **0.01 s** | 880x |
| `hilo graph impact <file>` | 13.31 s | **0.48 s** | 28x |
| `hilo graph understand <task>` | 15.63 s | **1.49 s** | 10x |
| `hilo graph search <query>` | 13.26 s | **0.02 s** | 727x |
| `hilo graph untested` | 12.00 s | **0.02 s** | 692x |

Smaller repos were slow too, just less noticeably: `graph stats` was
1.08 s on ripgrep (now 0.02 s, 49x) and 2.29 s on clap (now 0.02 s,
103x). The old cost scaled linearly with edge count; the new one is
constant per invocation.

## The rest of the battery

| Operation | Result | Notes |
|---|---:|---|
| Cold JIT query (no cache at all) | **0.02 s** | One file parsed on demand — the fastest path in the binary |
| `hilo meta <file>` (xattrs only) | **0.00 s** | Never touches the graph |
| Full `graph warm` (ripgrep) | 5.0 s | 110 files, 90 MB peak RSS |
| Full `graph warm` (clap) | 11.2 s | 330 files |
| Full `graph warm` (tokio) | 41.9 s | 793 files, 132 MB peak RSS |
| `hilo mount --daemon` | background | FUSE detach added in 0.3.0 |

## Determinism

`hilo graph stats` output is now byte-identical across repeated runs
(verified 12/12 identical SHA-256). Two latent nondeterminism sources
were fixed as part of the sprint: HashMap iteration order leaked into
the edge-types section (fixed with a sorted print), and top-dependency
counts lacked a tiebreaker (fixed with `ORDER BY cnt DESC, "to" ASC`).
This matters for diffing, CI assertions, and before/after benchmarking.

## Cache coherence (JIT-001 / JIT-002 / PERF-001)

`edges.jsonl` is the single source of truth; `graph.db` is a query
cache. Three mechanisms keep them consistent:

1. **Write-through (JIT-001)** — graph-writing operations append to the
   JSONL and update the DuckDB cache in the same operation.
2. **Read-through reconcile (JIT-002)** — any open() that finds the
   cache behind the JSONL replays the missing edges (single prepared
   statement inside one transaction since PERF-001).
3. **Fingerprint stamp (PERF-001)** — a cache validated against the
   exact current `edges.jsonl` skips re-validation entirely; the stamp
   lives in `.vfs/graph/.last_reconcile` and is rebuildable.

A missing `graph.db` is no longer an error: with an `edges.jsonl`
present, commands rebuild the cache automatically (previously
`understand`, `search`, `module`, `untested`, and `rule-check` bailed).

## Memory & binary size

| Metric | Value |
|---|---|
| Query peak RSS | ~80 MB (tokio-scale graph) |
| Warm peak RSS | 132 MB (tokio), 90 MB (ripgrep) |
| Release binary | 118 MB (embeds DuckDB) |
| Debug binary | 1.22 GB (full symtabs — dev-profile tuning under evaluation) |

No leaks observed across the battery; memory scales with graph size and
is released on exit.

## Reproducing

The battery is simple to rerun on any box:

```bash
# clone a corpus, build a graph, time a query
git clone --depth 1 https://github.com/tokio-rs/tokio /tmp/tokio
cd /tmp/tokio && hilo init && hilo graph warm
/usr/bin/time -f '%e s | %M KB' hilo graph stats
```

## Open performance work

Tracked on the project board (`.coding-hermes/board/tasks.jsonl`):

- **PERF-002** — incremental `graph warm`: skip unchanged files via a
  content-hash/mtime parse cache (a no-change re-warm currently
  re-parses everything; target < 5 s on tokio)
- **PERF-003** — dev-profile binary-size tuning + keeping this document
  current as numbers change
