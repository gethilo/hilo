# Evidence — warpfs-dogfood-2026-11-17-12-14-37 (run 49)

Read-only lane: this directory is the raw evidence for the report in
`docs/dogfood/2026-11-17-run49-cov-audit-realuse.md`. Every number in the
report and on the board rows resolves to a file here.

## What was measured

Target: **hilo 0.4.1**, installed binary `~/.cargo/bin/hilo`
(`build: v0.4.0-50-g4216c7f`, `built: 2026-11-12T18:40:55Z`, 129,192,560 bytes).
Commit `4216c7f` is an ancestor of `origin/master`; the binary is the
`v0.4.1`-certified artifact (RELEASE-021).

Corpus: a **copy** of `coding-hermes-tools` at `6e702d1`
(81 non-test `.go` + 162 `_test.go` + a handful of Python scripts,
256 discovered files, 3 languages). Copied to scratch so `hilo init`
could install its git hooks without touching the real checkout.

## Files

| file | what it shows |
|------|----------------|
| `warm-cold-1.out/.err` | cold `graph warm` after `graph clean`: discovered 2370 edges / 242 files; coverage 256 files |
| `warm-cold-2.out/.err` | identical second cold run (repeatability of the same three numbers) |
| `warm-timing.out/.err` | cold `graph warm` with `HILO_WARM_TIMING=1`: the phase split (open 9.54 s, insert_edges 10.47 s, conformance 1.00 s) |
| `graph-warm.out` | cached `graph warm` (256/256 parse-cache skips) |
| `graph-stats.out` | `graph stats`: `Total edges: 2321`, `Total files: 245`, and the false *"the graph predates the last warm"* census sentence |
| `audit-text.out` / `audit.json` | `graph audit` text and `--json`: 199 public symbols, buckets 5 unreachable / 38 unlinked / 156 ok |
| `audit-receiver-classify.sh` | the classifier that grades every bucket entry against its definition line (re-runnable: `bash audit-receiver-classify.sh audit.json <corpus>`) |
| `audit-receiver-classify.tsv` | its output: 31 method-unexported-type, 61 method-exported-type, 56 func-exported, 48 py-def, 3 UNMATCHED |
| `surfaces-refusal.out/.err`, `coverage-links-refusal.err`, `test-classes-refusal.err`, `rollup-refusal.err` | the COV chain's documented `scope: foreign` refusal, rc=1 (by design; row DF-WARPFS-159) |
| `understand.out` | `graph understand "where is the patch apply path and what tests cover it"`: 88 MAP / 53 SIGNATURE / 329 DETAIL lines |

## Provenance and elision

Absolute host-home paths in the captured outputs were rewritten to `~`
(`/home/kara` → `~`) before committing. Nothing else was changed: every row
count, exit status and timing in these files is byte-identical to what the
commands emitted. The corpus itself is not committed (it is a copy of another
repository).

## Numbers, and where each came from

| measurement | value | source |
|---|---|---|
| `hilo init` | 0.046 s | this run's console (not captured to a file; re-runnable in one command) |
| cold `graph warm` (run 1) | wall 23.41 s, user 20.19 s, sys 1.14 s, RSS 401.6 MB | `warm-cold-1.err` |
| cold `graph warm` (run 2) | wall 26.74 s, user 20.65 s, sys 1.15 s, RSS 404.0 MB | `warm-cold-2.err` |
| cold `graph warm` with timing | wall 24.15 s, user 19.53 s | `warm-timing.err` |
| phase: `GraphDB::open (incl reconcile)` | 9.53783785 s | `warm-timing.err` |
| phase: `insert_edges` | 10.4669685 s | `warm-timing.err` |
| phase: `conformance edges` | 1.003139455 s | `warm-timing.err` |
| phase: `service contracts` | 1.02986668 s | `warm-timing.err` |
| cached `graph warm` | 0.84 s | console (`parse cache: 256/256 files skipped`) |
| `graph stats` | 0.09 s | console |
| `graph impact internal/patch/patch.go` | 0.23 s, 9 rows, 5 of them the 5 real importers | console + grep cross-check |
| `graph understand <task>` | 0.82 s | console |
| `graph audit` | 0.74 s | console |
| edges on disk | 2321 lines, 2321 distinct `(from,to,rel,provenance)`, 248 distinct `from` (3 are `std:` pseudo-nodes) | `jq` census over `.vfs/graph/edges.jsonl` in the scratch corpus |
| public symbols | 199 (11 entrypoints; 5 / 38 / 156 buckets) | `audit.json` |
| public symbols defined on UNexported receivers | 31 | `audit-receiver-classify.tsv` |

## Host load during the runs (matters for the wall times)

`/proc/loadavg` on the 16-core host read `31.08` → `44.95` → `48.52` → `51.02`
across the measurement window (fleet worker units on the same box). Wall times
are therefore host numbers; **user+sys CPU (19.5–20.7 s, three runs) is the
product's own cost.** No flamegraph was possible: `perf_event_paranoid=4` and
`perf record` fails with `Failure to open any events for recording`, so the
profile evidence is the product's own `HILO_WARM_TIMING` phase instrumentation
plus the source path below.

## Install leg

`bunker-qa.sh launch` → agent `c3a0807b` @ `bunker-mvp`, evidence
`/tmp/bunker-qa-evidence-20261117T124552Z-3029473.jsonl`. Host preflight at
launch: bunkerd active, docker yes, **act no**, disk 111.9 GB / 149.9 GB used
(75 %), 3/50 agents, 20 orphan keys. A Rust workspace build on that host was
measured at 50 m 01 s by the 2026-10-23 fire, so this leg cannot finish inside
one tick; its state at close is reported as RUNNING/PARTIAL, never a pass.
