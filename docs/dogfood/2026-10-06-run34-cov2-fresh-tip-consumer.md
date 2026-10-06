# Dogfood run 34 — 2026-10-06 — COV-2 flagship consumer pass on CURRENT HEAD

ch:trace row=DF-WARPFS-113 spec=hilo-cli/src/commands/coverage_links.rs test=hilo-cli/tests/cli.rs evidence=docs/dogfood/2026-10-06-run34-cov2-fresh-tip-consumer.md witness=none:scratch-dir-ephemeral verdict=none:uncommitted

## What this run did (real use, not the test suite)

A real consumer integrating the COV-2 flagship `hilo graph coverage-links` from a fresh
scratch checkout at HEAD 3954db07, binary built with the scoped incremental path
(`cargo build -p hilo-cli`, 21.1s) landing in `CARGO_TARGET_DIR=/var/tmp/hermes-cargo-target`.

Prior dogfood runs (30-33) tested a 12-day-stale binary that lacked the subcommand
(DF-WARPFS-109) and concluded from source-reading that it "ships at d11a14b"
(DF-WARPFS-112). This run is the first USE of the flagship on the current tip.

## Working sequence (what a user types)

```bash
git clone https://github.com/gethilo/hilo.git && cd hilo
cargo build -p hilo-cli            # 21s incremental; use -p, NOT workspace (duckdb-sys wall)
target/debug/hilo init             # 12ms, creates .vfs/
target/debug/hilo graph warm       # 35.3s cold / 0.07s warm (parse cache)
target/debug/hilo graph coverage-links           # 1.5s -> 945 links + 28 unlinked
target/debug/hilo graph coverage-links --json    # locked schema: {schema:1, links[], unlinked[], cause_census[], rules[]}
target/debug/hilo graph coverage-links --unlinked
target/debug/hilo graph coverage-links --surface "graph warm"   # 5 links for that verb
```

Results on this repo: 945 links (import conf 0.7, symbol_name_match conf 0.5), 28
unlinked surfaces each carrying a `cause` string (e.g. "owner file not imported by any
test (declaration-only surface)"). Output artifact `.vfs/graph/coverage_links.jsonl`
wrote 81 new/updated rows on the second invocation — incremental write works.

## The one real defect found

DF-WARPFS-113 (P1): `--surface <typo>` returns rc=0, zero links, and the text
"UNLINKED: none — every surface has at least one evidenced link". A misspelled surface
name is indistinguishable from a real name with full coverage. Same confusion class
DF-WARPFS-110 fixed for `graph impact`; `--surface` was missed. Surface names are the
human-facing display strings from `graph surfaces` (e.g. "graph warm", "hilo mcp"), not
ids — but ids also pass silently when unknown.

## Performance (Step 2b numbers)

- init: 12ms
- graph warm: 35.3s cold (one-time, first run), 0.07s warm (parse cache)
- coverage-links: 1.5s (1.55/1.50/1.50 over three runs)
- graph search: ~30ms; graph stats: instant; graph impact: instant
Nothing filed as PERF — all within user-invisible territory except the one-time cold
warm, which PERF-010 (classify 18s) already covers as the class.

## Install leg

SKIPPED-install-bunker (DF-WARPFS-114, P3): bunker-las-03 (100.69.3.13) ssh timed out;
no fresh-machine install proven this run.

## Errors hit along the way (diagnostics)

1. `target/debug/hilo` inside the repo is stale (09-30) because the environment sets
   `CARGO_TARGET_DIR=/var/tmp/hermes-cargo-target` — the repo's target/ only updates
   when that variable is unset. Any run that "built" and then ran the repo-path binary
   was testing an old artifact. Check `--version` build string first.
2. `hilo init <path>` / `graph warm <path>` take no positional path — run from the
   project root (help text does not say this loudly; rc=2 usage dump is the only signal).
3. Workspace-wide builds hit the duckdb-sys wall locally (prior runs' 2000s+ hangs);
   the scoped `-p hilo-cli` path builds in seconds when the shared target dir is warm.
