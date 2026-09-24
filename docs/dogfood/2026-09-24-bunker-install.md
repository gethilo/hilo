# Dogfood Run 13 — Fresh-Install Bunker Evidence (2026-09-24)

**Leg:** ephemeral-bunker fresh install, per the dogfood skill's manual
procedure §0–5. This run's qa battery could NOT run on bunker-las-02
(bunkerd stuck in `activating`; `bunker spawn` → `dial tcp 100.116.99.35:10001:
connection refused` ×2 at 15:33Z). The install leg was executed instead on
**bunker-las-03** (100.69.3.13), the skill's designated build host — explicit
here so the run is never mistaken for a silent install skip.

## Timeline (all 2026-09-24, UTC-5)

| Step | Result |
|---|---|
| Host probe | `HOST_OK`, bunkerd `active`, Docker 26.1.5 |
| Spawn | agent **4f10f656**, ttl 2h |
| Fresh box | Debian 13.7, x86_64, 6 cores, 32 GB, pkg-config + gcc + g++ + curl + git present |
| Clone | `git clone https://github.com/gethilo/hilo.git ~/app` → **ef4748d** (== local HEAD) — public clone, no credential minted, no visibility change |
| rustup minimal | 1.98.1 (first attempt hit rustup os-error-39 rename races; fixed with clean RUSTUP_HOME/CARGO_HOME) |
| `cargo build --release` | **RC=0 in 1610s (26m50s)**, first full build on the box |
| Binary | `hilo 0.3.1-dev`, build stamp `v0.3.0-57-gef4748d` |
| Smoke (documented quickstart) | `init` → OK (hooks skipped with a legible warning, no .git); `graph warm` on a 1-file corpus → correct 0-edge coverage line; `graph stats` → legible empty-graph message. **SMOKE_OK** |
| Destroy | `bunker destroy 4f10f656 --server bunker-las-03` → destroyed, local key removed, agent absent from `bunker list` |

## Findings carried by this leg

1. **Build time over-claim persists (run 8, still true):** README says
   "expect 15-20 min" for the first build; measured **26m50s** on 6 cores
   with a warm-OS cold-cargo. The number is in the right ballpark but the
   claim under-promises reality on small boxes.
2. **rustup os-error-39 race (fresh wrinkle):** the standard
   `sh rustup-init.sh -y --profile minimal` failed twice on this box with
   `could not rename 'component' file … Directory not empty (os error 39)`
   before succeeding with a clean `RUSTUP_HOME`. Not reproducible enough to
   file as a product defect — it is an environment quirk — but noted here
   so a future run that sees the same error knows it has been seen.
3. **No P0/P1 install blockers:** the documented path (`git clone` →
   `cargo build --release` → `hilo --help`-equivalent smoke) worked
   end-to-end on a bare box with zero undocumented dependencies. The
   las-02 ENV-BLOCK class (DF-WARPFS-25/44: pkg-config absent, no sudo)
   did NOT reproduce on las-03, which ships pkg-config by default —
   consistent with run 8's history (las-03 passed then too).

## Comparison table (all fresh-install legs to date)

| Run | Host | Result | Build time |
|---|---|---|---|
| 09-20 (run 7 era) | las-03 | PASS | 19m06s |
| 09-20 (run 8) | las-03 | PASS | 33m45s |
| 09-22..23 (runs 9-12) | las-02 | ENV-BLOCKED ×4 (no pkg-config/sudo) | — |
| **this run (13)** | las-03 | **PASS** | **26m50s** |
