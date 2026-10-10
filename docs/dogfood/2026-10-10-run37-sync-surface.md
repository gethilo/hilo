# Dogfood run 37 (sync surface) — 2026-10-10 — backend sync at tip, native S3 + rclone external

Lane: coding-hermes-tools-dogfood (tick coding-hermes-tools-dogfood-2026-10-10-04-57-05).
Angle: the sync surface — never dogfooded in runs 1-36 (all CLI/graph/MCP/mount/install).
Trigger: DF-WARPFS-69 fix (621fd5c8, "repeat sync = no-op") landed 10-09 22:30; the installed
`hilo` binary (10-08) predates it, so a fresh scoped debug build was made (3a62d63b, 04:43).
A sibling tick the same morning took the triggers surface (rows 119/120) and re-proved the
install premise (runs 35/36 already did twice on bunker-las-02) — this run took sync.

## Method (real use, not tests)

1. Scratch workspace /tmp/dogfood-warpfs-s3 + /tmp/dogfood-warpfs-sync, `hilo init`, sample files.
2. Real S3 endpoint without cloud credentials: `rclone serve s3 /tmp/dogfood-remote --addr
   127.0.0.1:19018 --auth-key <k>,<s>` as a local S3-compatible store.
3. `hilo backend mount --type s3 --bucket dogfood --at s3work --endpoint http://127.0.0.1:19018`
   + AWS env keys; then the documented sync verbs: `backend sync --push/--pull/--both`, repeat
   runs, LWW conflict edits on both sides, `.vfs/sync/conflicts.jsonl` inspection.
4. External backend: `backend mount --type external --remote <dir> --tool rclone`.

## What worked (native S3 path)

- init → mount → push: 3 files/430B transferred, RC=0, 117ms. Plan line discloses endpoint
  (DF-WARPFS-12 contract holds).
- DF-WARPFS-69 verified LIVE: repeat push, repeat pull, and --both over an aligned set are all
  full no-ops (0 transferred, 0 conflicts). A genuinely changed file transfers exactly once and
  logs exactly one conflict row ({"key":"docs/guide.md",...,"resolved":"RemoteWins"}); local
  mtime aligns to the remote `modified` after transfer. Confirmed across 7 edit/pull cycles:
  6 changes → 6 transfers → 6 rows; steady-state pulls clean.
- Perf: warm 130.2ms ±10.8 (hyperfine, 20 runs), cold ~117ms first-run. Nothing a user notices;
  no PERF row (per the perf law: a win nobody can feel is not a finding).

## What broke (filed as rows 121-125)

- DF-WARPFS-121 (P0): external backends are DEAD at HEAD — `rclone lsf --json` (external.rs:170)
  is not a legal invocation; the verb is `rclone lsjson`. First push RC=3 before any transfer.
  GDrive/OneDrive/Dropbox/external all ride this lister.
- DF-WARPFS-122 (P1): no per-backend selector; one broken mount poisons every sync (positional
  args are subtree PATHs; `sync --push s3work` still ran the broken mount first and exited 3).
- DF-WARPFS-123 (P2): transfer failures discard the BackendError cause (planner.rs:335/370/410
  `map_err(|_| TransferFailed(key))`) — "transfer failed: docs/guide.md" with zero diagnosis.
- DF-WARPFS-124 (P2): plans ride a stale LIST (rclone serve 5m dir-cache): immediate pull after
  an external edit silently no-ops; one conflict row carried post-transfer aligned mtimes
  (local_mtime == remote_mtime) — decision mtimes should be captured at plan time.
- DF-WARPFS-125 (P3): LocalDriver unreachable from `backend mount` (CLI accepts only
  s3|gdrive|onedrive|dropbox|external) — the zero-credential try-it path is library-only.

## Install leg

SKIPPED-install-bunker — premise already re-proven twice today (run 35 fresh-machine no-sudo
PASS 26m32s; run 36 second-agent re-proof 24m37s) and the sibling tick filed the explicit skip
row DF-WARPFS-119 the same morning with identical evidence. This tick's unique value (the sync
surface) required a fresh HEAD build regardless; nothing the bunker leg could add to the
install premise.

## Verdict

🟡 PROMISING-BUT-ROUGH — the native S3 sync engine (the DF-WARPFS-69 fix) does exactly what its
commit claims, with a clean no-op steady state and an honest one-row-per-change ledger; the
backend family around it is unreached by any integration surface (external lister dead, no
per-mount isolation, cause-swallowing errors), which is invisible to the green unit suite.
