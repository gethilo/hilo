## TASK: DF-WARPFS-64-R2 — no `graph warm` ⇒ graph consumers answered confidently over a one-file on-demand graph (RECURRENCE of DF-WARPFS-64 / GAP-039)

Tick: `warpfs-foreman-2026-10-27-21-44-08` (verification-only closeout; no worker dispatched, no source change).

### Status at pick time
The implementation was ALREADY LANDED and an ancestor of HEAD:

| fact | value |
|---|---|
| implementation commit | `179308e90ca221b0a47a244864f33a6368c6c097` (merge of `wt/DF-WARPFS-64-R2`) |
| gate location | `hilo-cli/src/commands/graph.rs:1743` `require_completed_warm()` (bails with `No complete graph warm found. Run \`hilo graph warm\` first.`) |
| call sites | `graph.rs:1446` (impact), `:1791` (stats), `:3320` (untested), `:3410` (module) |
| regression test | `hilo-cli/tests/cli.rs:363` `df_warpfs_64_r2_fresh_project_requires_completed_warm` |
| ancestor check | `git merge-base --is-ancestor 179308e9 HEAD` → exit 0 |

Because the implementation was reachable from `origin/main`, this row is verification-only:
no worker, no duplicate code.

### Live acceptance probe (the load-bearing witness)
Binary: `target/debug/hilo` — build stamp `hilo 0.4.0 / v0.3.0-647-g1bdb99e1 / built 2026-10-27T08:21:01Z`,
repo-built from `1bdb99e195cc347ddd1c797709eb6994afe34c12` which HAS `179308e9` as an ancestor
(verified with `git merge-base --is-ancestor 179308e9 1bdb99e1`). The cargo-installed
`~/.cargo/bin/hilo` (v0.3.0-293-g3adfb9ed, 2026-10-08) is STALE and was NOT used.

Scratch: `/var/tmp/dfw64r2-1793138713` (fresh `hilo init`, 3-file corpus, deliberately NO `graph warm`).
Script: `/tmp/dfw64r2-probe.sh`; raw transcript `/tmp/dfw64r2-probe.out`.

```
===== BEFORE WARM =====
--- stats    rc=1  error: No complete graph warm found. Run `hilo graph warm` first.
--- untested rc=1  error: No complete graph warm found. Run `hilo graph warm` first.
--- module   rc=1  error: No complete graph warm found. Run `hilo graph warm` first.
--- impact   rc=1  error: No complete graph warm found. Run `hilo graph warm` first.
--- search   rc=1  error: No graph data. Run `hilo graph warm` first.
===== AFTER WARM =====
--- stats    rc=0  Total edges: 2 distinct / 2 raw (edges.jsonl) ; Total files: 2 ;
                   warm last counted 3 discovered files = 2 contribute + 1 zero-edge
--- untested rc=0  2 untested file(s): pkg/top.py, pkg/user.py
--- module   rc=0  Module: pkg  Files: 2  Edges: 2  Tests: 0.0%
--- impact   rc=0  pkg/user.py ← imports (depth 2, via pkg:helper) ; pkg/top.py ← imports (depth 4)
```

Both arms of the acceptance criterion hold: every previously fail-open consumer refuses
non-zero and names `hilo graph warm`, and after a warm the same commands report the real
corpus. No command reports "no dependents" / "N untested file(s)" for a corpus it never
traversed.

### Exact-head CI (terminal, bound to the published tip)
`gh api repos/gethilo/hilo/actions/runs/40584029035/jobs` →
`Build & Test (stable)`: **success**, every step `completed/success`
(Check formatting, Build, Clippy, **Test**, Post Cache cargo).
Head SHA `c77d5daf37cb99b7af4f9e8b1f4e037e6e3832c7` = current `origin/master` = the commit that
contains `179308e9`. The CI Test step is `cargo test --workspace` under `RUSTFLAGS: "-D warnings"`
(`.github/workflows/ci.yml`), so the regression test `df_warpfs_64_r2_fresh_project_requires_completed_warm`
ran inside that green battery.

### Evidence layering (honest state)
- implementation commit: `179308e9`
- local live probe: PASS (both arms), this tick
- exact-head CI at tip: **GREEN** (run `40584029035`)
- GitReins Tier-2: task `DF-WARPFS-64-R2` created + started in the control root this tick and
  `task complete` issued; the evaluator (`pid 2337875`) was still `Evaluating...` at closeout
  (the tier-2 judge itself shells out to `cargo test -p hilo-cli --test cli` on a shared
  `/var/tmp/hermes-cargo-target` where `libduckdb-sys` is doing a cold from-source C++ compile).
  **No verdict is claimed.** The earlier verdict `78038fab` (passed=true) is retained as history
  only — it evaluated `8d8cc596`, a later bookkeeping commit, so it was never the implementation
  witness.

### Verdict
Row CLOSED verification-only on the terminal exact-head CI run + the live dual-arm probe.
No source change, no worker, no duplicate work. Off-by-one discovery was consulted first
(`discover php-same-namespace-reference-resolution` → `not_found`; corpus grep hit
`data/answers/1059-file-query-pkg-resolution-gap.json`, which documents the query-time
resolution-layer mechanism — the same mechanism this row's gate family uses).

Addresses DF-WARPFS-64-R2
