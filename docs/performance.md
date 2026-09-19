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

## Real-world scale: containerd (2026-09-11)

The battery above is a controlled measurement on three fixed Rust corpora.
This is the opposite case: the full [containerd](https://github.com/containerd/containerd)
Go repository (depth-1 clone), as a user actually meets it — a committed
`vendor/` tree of 4122 files is excluded by policy (PERF-005), leaving
**1367 project files** in the graph (14070 edges across 1287 files).

| Phase | Command | Result |
|---|---|---:|
| Graph construction (one-time) | `hilo graph warm` | **~80 s** for 1367 files |
| Incremental rebuild | `hilo graph warm --changed` | **~1 s** for 1 changed file |
| Query (graph already built) | `hilo graph stats` | **~0.03 s** (26 ms measured) |
| Query (graph already built) | `hilo graph impact 'pkg:github.com/containerd/containerd/v2/core/mount' --max-depth 1` | **~0.1 s** (108 ms measured), 126 dependents |

**The warm and query numbers measure different phases, so they are not
competing claims.** The ~80 s is *graph construction*: the one-time AST
parse and edge build paid once per repository (or per changed-file delta
with `--changed`). The ~0.03 s figure is *query latency*: what a cheap
query costs on every invocation once that graph exists, and it does not
grow with the 80 s. In short, at containerd scale the queries ran in
~0.03 s–0.1 s (tens of milliseconds), never the 12–15 s cache-revalidation
tax the 0.2.x era paid — which is exactly the PERF-001 claim above,
re-verified on Go code.

The measured numbers, the corpus shape, and the per-hub blast-radius
cross-checks (client 110, plugins 98, `pkg/namespaces` 92, `core/content`
88 — all matching grep import counts) live in
[the run-3 dogfood report](dogfood/2026-09-11-integration.md).

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

### Which files a warm may change (INV-001)

`edges.jsonl` is the single source of truth, but it is **append-only from
warm's side**: a warm with any parse-cache miss appends the newly discovered,
deduplicated edges to the *tracked* `.vfs/graph/edges.jsonl`, and that diff
is intentional inventory refresh — commit it with the change that produced
it. A warm that fully hits the parse cache (and finds no missing derived
edges) takes its fast path and writes nothing at all; a fully-cached re-warm
of an untouched tree therefore leaves `git status` clean. Rebuildable cache
artifacts warm also touches on the slow path — `graph.db`, the
`.parse_cache.json` parse cache, and the `.last_warm` marker — are gitignored
and never committed. The full contract, the cache-artifact table, and the
operator recipe for a deliberate full refresh (`graph clean` + `warm` +
recommit) live in [inventory-policy.md](inventory-policy.md); the contract is
pinned by
`warm_refreshes_tracked_edges_jsonl_append_only_and_rewarm_adds_no_duplicates`
in `hilo-cli/src/commands/graph.rs`.

## Memory & binary size

| Metric | Value |
|---|---|
| Query peak RSS | ~80 MB (tokio-scale graph) |
| Warm peak RSS | 132 MB (tokio), 90 MB (ripgrep) |
| Release binary | 118 MB (embeds DuckDB) |
| Debug binary | 1.22 GB (full symtabs — dev-profile tuning under evaluation) |

No leaks observed across the battery; memory scales with graph size and
is released on exit.

## Resource-constrained environments

`cargo test --workspace` fails under a 3 GB virtual-memory cap
(`ulimit -v 3145728`): the `-p hilo_graph --lib` suite aborts with
`error: test failed, to rerun pass '-p hilo_graph --lib'`. This is the
known duckdb/arrow address-space class — both reserve large virtual
address ranges up front regardless of resident memory — not a memory
leak, and the same suite is green uncapped (CI Test job and the native
suite both PASS). Take the reservations into account before gating CI on
memory-capped runners. Evidence: QA battery cell `chaos-resource`,
2026-09-07 (board row QA-WARPFS-4 carries the cell output verbatim).

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
