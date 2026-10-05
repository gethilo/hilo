# Dogfood run 33 — 2026-10-04 — COV-2 coverage-links on a FRESH HEAD build (coding-hermes-tools-dogfood)

ch:trace row=DF-WARPFS-112 evidence=docs/dogfood/2026-10-04-run33-coverage-links-fresh-build.md witness=live:/tmp/df32-ws verdict=none:dogfood-no-verdicts

## What this run was

Run 32 (same day) found DF-WARPFS-109 ("COV-2 flagship `coverage-links` CLI missing
from hilo-cli") using the **installed** binary `v0.3.0-14-g2b26d3f` (2026-09-22, 12
days stale). This run re-tests that finding the honest way: a **fresh release build
from HEAD ef5cdd5** (101m47s cold, duckdb-sys from source — same cost class run 30
measured at 1648s on bunker-las-03), then a real-consumer pass of the COV-2 surface
with that binary.

**Result: DF-WARPFS-109 is a staleness artifact, not a product defect.** The
subcommand ships in the d11a14b commit itself (`git show d11a14b:hilo-cli/src/cli.rs`
contains `CoverageLinks` at line 165); the installed `~/.cargo/bin/hilo` predates it
by 249 commits. The install channel, not the feature, is broken — this is the same
root cause DF-WARPFS-100/104/108/111 already file (the release/refresh wall).

## Fresh-consumer pass of `hilo graph coverage-links` (HEAD binary, release)

Sequence actually run (angle, not setup):

1. `hilo graph warm` on the real repo — 37,023 distinct edges / 6,733 files.
2. `hilo graph coverage-links` — text report renders: three rule layers explained
   inline, cause census (32 declaration-only surfaces + 9 test files with no
   derived link), unlinked set listed with per-surface hashes.
3. `hilo graph coverage-links --json` — machine shape: `{links, unlinked,
   cause_census, rules, schema}`; **1,026 links across 189 unique targets**, 32
   unlinked surfaces; every link carries `evidence_kind` (import >
   symbol_name_match > runtime_trace), `confidence` (0.5 for symbol layer), and
   `direction`.
4. `--surface "graph coverage-links"` — answers "what covers this surface" for one
   surface; correct empty result on this repo (no tests import cli.rs).
5. `--unlinked` — 32 rows, each with a cause; the rules block explains why
   runtime_trace is dead (no runtime_trace.jsonl recorded anywhere).

**Independent re-derivation (checker law):** link_id
`af6d388f…` binds test file `hilo-backends/tests/s3_integration_test.rs` to surface
`325db5cf…` = `cli_verb "ignore"` (owner hilo-cli/src/cli.rs) with
`symbol_name_match`, conf 0.5. Re-derived by hand: the test file contains the word
`ignore` 11 times (word-boundary), and no import of hilo-cli (symbol layer only,
import pairs excluded) — exactly what the link claims. The link graph is honest on
a sampled row.

## Perf (Step 2b, hyperfine, release, warm, real repo, load-matched note: host loadavg 33-46)

- `graph search "rate limiter"`: **405.8 ms ± 17.1** (warm). Cold-first on scratch
  repo: 24.2 ms ± 1.5 (small corpus). vs run 31's 510 ms on the stale binary —
  slightly better, same order. Not a finding.
- `graph stats`: 182.9 ms ± 5.2. `coverage-links`: 0.24s warm. Not findings.
- `graph clean && graph warm` (re-parse, 6.8k files): **76.4s cold** — one-time
  cost, no user complaint (run 30 measured 2.3s on the 19GB tree's mount path).
- **`hilo classify`: 15.99s ± 0.28** (hyperfine, warm, 5 runs) — down from the
  17.9s stale-binary baseline (PERF-010 closed as 1.73x faster via interleaved
  A/B). 16s on 7.5k files is still the one number a user feels; PERF-010 already
  carries the fix direction (parallelize the scan), no new row.

**No new PERF rows.** Nothing crossed the "a user would notice and not already
filed" line.

## Install leg

SKIPPED — 16th consecutive run. The fresh-binary build itself IS this run's
install evidence (cold release build 101m47s local; bunker-las-03 unreachable —
`ssh bunker3` connection timed out, host down at tick time). The standing wall
(DF-WARPFS-100/104/108/111) is unchanged: the installed `~/.cargo/bin/hilo` is now
**13 days stale**, and run 32's P1 finding proves the cost — users of the installed
binary cannot see features merged the same week. No new SKIPPED row: DF-WARPFS-111
(3h old) covers this run too; this log is the cross-evidence.

## What a fresh user would need (docs gaps, minor)

- `hilo init` takes no path argument (cwd only) — README examples don't say so.
- `coverage-links` reports are much easier to read via `--json`; the text tail
  prints unlinked causes twice (rules block + CAUSES block). Polish only.

## Verdict

🟢 Per-surface: COV-2 coverage-links **SHIPPABLE** at HEAD (works, honest, fast,
evidence re-derived). CLI/graph surface: SHIPPABLE. Install/refresh channel:
DOES-NOT-DELIVER as shipped today — the wall rows are pending, not fixed.
