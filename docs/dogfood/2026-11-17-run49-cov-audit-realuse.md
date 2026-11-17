# Run 49 — a real agent using Hilo on a foreign Go repo (2026-11-17)

Tick `warpfs-dogfood-2026-11-17-12-14-37`. Read-only lane: nothing in this run
changed product code; the findings are rows `DF-WARPFS-174`, `DF-WARPFS-43-R2`
and `PERF-016`.

Evidence for every number below: `.coding-hermes/evidence/warpfs-dogfood-2026-11-17-12-14-37/`
(`INDEX.md` maps each figure to the file that produced it, `run-notes.md`
carries the exact command list).

## The promise (null hypothesis)

README: *"Give your AI coding agent a pre-built map of every codebase it
touches — dependencies, entrypoints, test coverage, blast radius — without
burning context window on file reads."* The workflow the docs promise is
`hilo init` → `hilo graph warm` → ask structural questions (`stats`, `impact`,
`related`, `understand`, `audit`), optionally through MCP.

## What was actually used

Artifact: the **installed, certified** binary — `hilo 0.4.1`,
`build: v0.4.0-50-g4216c7f`, built 2026-11-12, 129 MB. `4216c7f` is an
ancestor of `origin/master`, so this is a real user's artifact rather than a
local build. The tree at `origin/master` is 20 commits further; 3 of those
touch product code (`f9efdd7` mount xattrs, `1d90760` lint reap, `84c0267`
Go module-local imports) and **are not graded here** — a fresh-HEAD release
build was started in an isolated worktree with its own target dir and had not
finished (~35 min, still inside the DuckDB C++ dependency build) at close.

Corpus: a scratch **copy** of `coding-hermes-tools` @ `6e702d1` — a real Go
service, 243 `.go` files (81 non-test / 162 test) plus a handful of Python
scripts; 256 discovered files, 3 languages. Copy, not the live checkout, so
`hilo init`'s git hooks land in a throwaway tree.

## Time to first real answer (the thing a user feels)

| step | wall | note |
|---|---|---|
| `hilo init` | 0.046 s | installs `post-commit` + `post-merge` hooks |
| `graph clean` + cold `graph warm` | **23.41 s** (run 1), **26.74 s** (run 2) | 20.2 s / 20.7 s of **CPU**; 402 MB RSS |
| first answer: `graph stats` | 0.09 s | |
| `graph warm` again (cached) | 0.84 s | `parse cache: 256/256 files skipped` |
| `graph impact <file>` | 0.23 s | |
| `graph understand "<task>"` | 0.82 s | 470 lines of map/signatures/detail |
| `graph audit` | 0.74 s | |

So the steady state is excellent — every query is sub-second — and the whole
first-run cost is the one cold warm. **Time-to-first-answer ≈ 23.5 s**, and
~20 s of it is CPU, not the shared host (the host was at load 31–51 on 16
cores during the window; three independent cold runs burned 19.5–20.7 s of
user CPU each).

## What held up (worth saying plainly)

- **`graph impact` is honest and correct on a real repo.** For
  `internal/patch/patch.go` it returned 9 rows; the 5 crate-scoped ones are
  exactly the 5 files whose `import` of that package `grep` finds, and the
  output explains that crate-scoped rows come from the package node, not the
  file, and that family expansion can exceed the direct importer set. That is
  the rare case of a tool whose caveat is truer than its headline.
- **`graph understand` produces what an agent needs** for a natural-language
  task: for *"where is the patch apply path and what tests cover it"* it
  returned the patch package, its test files with symbol lists, the CLI
  signatures, and a DETAIL tier — 470 lines, in 0.82 s, with no file reads.
- **The refusal is honest.** `graph surfaces`, `coverage-links`,
  `test-classes` and `rollup` refuse on a foreign tree with rc=1 and a
  one-sentence reason (`scope: foreign`). That is documented
  (`docs/cli-reference.md` §`graph surfaces`) and already owned by
  `DF-WARPFS-159`; this run re-confirmed it and filed nothing.
- **Cached warm is a real optimisation.** 0.84 s vs 23 s on the same tree.

## What did not hold up

### 1. `graph audit`'s buckets grade non-surface symbols as public surface — `DF-WARPFS-174`

`graph audit` reported **199 public symbols** with buckets
**5 unreachable / 38 unlinked / 156 ok**. Classifying every bucket entry
against its own definition line
(`audit-receiver-classify.tsv`, produced by the committed
`audit-receiver-classify.sh`):

- **31 of the 199** "public functions" are methods on **unexported receiver
  types** — e.g. `(*connModeConn).Read`, `(connModeAddr).String`. Per
  `docs/cli-reference.md` the rule is "Go `func`/method whose name is exported
  (leading uppercase)", so their inclusion is *documented*; the consequence is
  that ~16 % of the product's "public surface" cannot be called from another
  package at all.
- **4 of the 5 `unreachable` entries are the `net.Addr`/`net.Conn`
  implementations on those unexported types** — `Network` (`serve.go:299`),
  `RemoteAddr` (`:327`), `SetReadDeadline` (`:329`), `SetWriteDeadline`
  (`:330`). They are live: `connModeAddr` is returned *as* `net.Addr` from
  `LocalAddr`/`RemoteAddr`, so the compiler guarantees interface satisfaction
  and the stdlib calls them by dispatch. The audit's rule is lexical ("no file
  reachable from an entrypoint **names** the symbol"), so interface dispatch
  is invisible to it.
- The text output (and the `--json` rule string) describes this as *"no caller
  path from a declared entrypoint"* — a reachability claim — while the docs'
  note calls the metric "conservative, no-false-accusation". On this corpus the
  smallest bucket is 80 % interface implementations, i.e. the one bucket an
  agent will read as "dead code" is the least trustworthy.

Fix direction in the row: name the class (an `interface_impl` bucket, or at
minimum an annotation on entries whose receiver type is unexported or whose
name+signature satisfies a stdlib interface) and keep the `public_symbols`
headline honest by counting only symbols reachable from outside the package.

### 2. `graph stats` diagnoses a fresh graph as stale, and the remedy never clears it — `DF-WARPFS-43-R2` (recurrence of `DF-WARPFS-43`)

On a graph warmed **seconds earlier** (three times in a row, cold and cached):

```
Discovered 2370 edges across 242 files (3 languages)      <- graph warm
Coverage: 256 files = 242 contribute edges + 0 package facades + 14 no imports + ...
Total edges: 2321 distinct / 2321 raw (edges.jsonl)       <- graph stats
Total files: 245
File census: 245 files with edges (distinct edge sources) — warm last counted 256
  discovered files = 242 contribute + 14 zero-edge ...; 245 > 242 means the graph
  predates the last warm — `hilo graph warm` prunes sources whose file is gone
```

`edges.jsonl` on disk holds **2321** rows and **2321** distinct
`(from,to,rel,provenance)` identities, with **248** distinct `from` values of
which 3 are `std:` pseudo-nodes — i.e. **245 project sources**, exactly what
`stats` prints. `coverage.json` carries `unique_edge_sources: 242`.

So the census compares two different populations (245 = every distinct edge
source, including test files that only emit `tested_by`/`tests` rows; 242 =
files that contributed import-like edges). The comparison is structurally true
on a healthy graph, and the sentence drawn from it — *"the graph predates the
last warm"* — is false. Worse, the remedy it names is the thing that was just
run: `graph warm` three times changes nothing, so an agent that acts on the
advice loops. A second number in the same family: warm says 2370 edges,
`edges.jsonl` stores 2321 — a 49-row gap with no line anywhere saying
"deduplicated 49".

### 3. Cold map build: ~4.5 ms per edge inside DuckDB — `PERF-016`

The product's own instrumentation (`HILO_WARM_TIMING=1`) splits the 20 s of
cold warm:

```
[timing] test associations: 446.868µs
[timing] service calls: 29.789957ms
[timing] conformance edges: 1.003139455s
[timing] service contracts: 1.02986668s
[timing] jsonl dedup write: 11.96103ms
[timing] GraphDB::open (incl reconcile): 9.53783785s
[timing] insert_edges: 10.4669685s
```

95 % of the cost is DuckDB: opening + reconciling an **empty** cache (9.54 s)
and then inserting the same 2321 edges (10.47 s) — ~4.1 and ~4.5 ms per row.
The insert path is a prepared `INSERT OR IGNORE` executed row-by-row inside
2048-row transactions (`hilo-graph/src/graph.rs:334` `insert_edge_chunk`,
reached from `hilo-cli/src/commands/graph.rs:1462`); DuckDB can bulk-load
2321 rows from JSONL/CSV in tens of milliseconds, so the fix direction is a
bulk-load path for the cold case, not another chunk-size sweep
(`PERF-007`/`PERF-008` already settled chunk size).

**Profile honesty:** no flamegraph. `perf_event_paranoid=4` on this host and
`perf record` answers `Failure to open any events for recording`; the phase
split above is the product's own measurement, and the hot-path file:line above
is read from source, not inferred from a graph. A user *does* feel this one
(23 s before the first answer) so it is filed; the cached path is already fine.

**Not filed (explicitly):** a HYPOTHESIS that `GraphDB::open`'s 9.5 s is a
*fixed* cost paid even by a tiny repo. The probe that would have settled it
was refused by the command gate and was not re-run by another route, so it
appears in `run-notes.md` and here as a hypothesis only — no board row asserts
it.

## Verdict

**🟡 PROMISING-BUT-ROUGH.** The value is real and demo-able: a 0.8 s
`understand`, a correct 0.23 s `impact` with an honest caveat, sub-second
queries over a 256-file repo, and an honest refusal where the coverage model
does not apply. What blocks a recommendation is trust in the numbers: the
smallest and most actionable `audit` bucket is 80 % live interface
implementations, `stats` tells you a freshly-warmed graph is stale and gives a
remedy that cannot clear it, and the first map build burns 20 s of CPU
inserting 2321 rows one at a time.

## Install leg (this tick)

`bunker-qa.sh launch` succeeded against `bunker-mvp` (bunkerd active, docker
yes, **act no**, disk 75 % — 111.9/149.9 GB, 3/50 agents, 20 orphan keys) and
started agent `c3a0807b`, evidence
`/tmp/bunker-qa-evidence-20261117T124552Z-3029473.jsonl`. A Rust workspace
build on that host was measured at **50 m 01 s** by the 2026-10-23 fire, so the
battery was still RUNNING at close: **PARTIAL, not a pass**. The ci-pass cell
cannot pass on this host as configured (`act=no`) — that is harness evidence,
not a product red.

## Left behind

- this report, `docs/dogfood/2026-11-17-run49-cov-audit-realuse.md`
- `.coding-hermes/evidence/warpfs-dogfood-2026-11-17-12-14-37/` (all raw outputs,
  the re-runnable bucket classifier, `INDEX.md`, `run-notes.md`)
- `docs/dogfood/diagnostics.md` (how the map is built, why cold warm costs what
  it costs, and the query-time traps this run hit)
- `skills/hilo-usage/SKILL.md` — run 49 section (what to trust in `audit`, how
  to read the stats census line, how to time a cold warm)
- board rows `DF-WARPFS-174`, `DF-WARPFS-43-R2`, `PERF-016`
