# Evidence — warpfs-dogfood-2026-11-20-15-00-01 (run 50: real use on the Nov-17 binary; tip arms staged)

## Provenance

- Tick `warpfs-dogfood-2026-11-20-15-00-01`, first delivery of this label (gateway.log count = 1,
  2026-11-20 10:00:06 -0500). This fire is effectively the 4th delivery of the warpfs dogfood line
  this week: the 2026-11-17 chain (run 49 + delta2 + third delivery) left the HEAD-binary leg open.
- Filing tree: worktree `/home/kara/warpfs-wt-df49` at `origin/master` tip `08267f3` (the shared
  checkout `/home/kara/warpfs` is parked on side branch `qa-wtick-20261113-165414`, c7924af, 54
  commits behind). Product delta tip vs the 2026-11-17 records commit `e518262`: 6 files /
  +1104 −62; vs the battery's tree c7924af: 13 files / +1634 −96.
- Binary used for real use: `/mnt/ssd/caches/hermes-target/release/hilo` v0.4.1, build
  `v0.4.1-40-g838b487-dirty` (mtime Nov 17 10:30) — the 2026-11-17 fire's build, which finished
  ~2h after that delivery closed. Harvested per the skill's finished-ledger rule; NOT the tip.
- Consumer: real foreign monorepo clone `/tmp/df49/heading` (heading @ 28e6c248, 374 files warm
  discovered / 328 with edges). Scratch; self-prunes.

## What is here

| File | What it is | Notes |
|------|------------|-------|
| `walkbook-verify-TIP.sh` | Corrected, staged-unrun arms for a TIP binary: REV-WARPFS-9 `--port` AC, DF-WARPFS-70 on its ACTUAL fix surface (`hilo workspace mount`, per f9efdd7) + single-mount halves, DF-WARPFS-156/176 kill/self-heal AC, PERF-011 clean-window cold-mount×5. Fixes four v1 probe bugs (recorded in the run report): repo-relative fixture paths, no pipes around exit codes, mount window excludes sleep, DF-70 on the workspace variant. | UNRUN — needs a tip binary. |
| `run-use-v1.log` | First real-use pass (happy path + GAP-121 + COV gate + mount + serve + perf) on the Nov-17 binary. Contains the v1 probe bugs; product outputs readable; impact/related errors are the probe's wrong paths, not product. | RUN, partially void arms |
| `run2-focused-actual.sh` + `run-use-v2.log` | Focused corrected pass: COV exit codes without pipes (both rc=1 ✓), DF-70 arm with a real file, serve-on-8766 attribution, clean mount window. | RUN — the load-bearing numbers |
| `r1-surfaces.err`, `r3-serve.log` | Raw stderr of the COV gate refusals; the serve bind-failure log ("Address already in use"). | verbatim |
| `bunker-ledger-pulled-1610Z.jsonl` | Bunker battery ledger (9 rows), agent `241bec2a` @ bunker-mvp, pulled by hand via `bunker exec` after two collect() probe failures. | verbatim; jq-verified 9 lines |

## Probe-error disclosure (v1 walkbook, so the next fire does not re-diagnose)

1. `impact src/main.ts` / `related src/main.ts`: fixture path not repo-relative (monorepo) — the
   product's "not in the graph" error is CORRECT and names accepted id forms; not a finding.
2. `surfaces rc=0` / `coverage-links rc=0` in v1: read through a pipe (`| head`), so `$?` was
   head's. Clean re-run: rc=1 both. The gate is correct.
3. The DF-70 arm first ran with an empty path (`find src` on a monorepo) — void; redone in v2.
4. `mount=2.0s` in the first perf arm included a `sleep 2` inside the window — clean window:
   mount-start 13–25ms.
5. Earlier "cargo wedged 3×" during the tip build was probe blindness (rustc-only greps miss
   duckdb's cc1plus grandchildren); builds were alive. Never filed; do not re-diagnose.

## Key measurements (tree 838b487+v40, host load 27–34 on 16 cores — wall numbers host-inflated)

init 0.15s · cold warm 28.4s (CPU 24.9s, 374 files, 18 loud parse-recoveries, 3 parse-miss) ·
stats 70ms (2451 edges, 963 tested_by, honest orphan census) · untested --granularity function
1.6s (60 files / 202 uncovered functions, file:line each) · COV gate foreign-repo refusal rc=1
with precise scope message · mount-start 13–25ms · first stats after mount 46–100ms (8 arms;
PERF-011's 2.7–8.0s stall does NOT reproduce) · impact 247ms ±61ms · hyperfine stats 96.8ms ±29ms
(vs run 48's 47.5ms at low load — host delta).

## Bunker leg (agent 241bec2a, battery ALIVE at close — leave intact)

- Launch 15:07Z `bunker-qa.sh launch /home/kara/warpfs --keep` (bunkerd active, 125.0/149.9 GB,
  preflight passed). Battery tree c7924af — does NOT cover tip fixes.
- Ledger: toolchain-bootstrap 5×OK; `fresh-install [SKIP]` at the 3600s budget cap mid-build
  (class owned by QA-WARPFS-043); `lint [FAIL]` go-vet-on-Rust (class owned by QA-WARPFS-035).
- `bunker-qa.sh collect` failed twice ("ssh probe returned no state") while `bunker exec` answered
  immediately — filed as **QA-WARPFS-045** (the tick's one new row). Agent left intact for the
  collector; harvest or destroy next fire (grep `241bec2a` on board + this dir).
