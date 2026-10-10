# Dogfood Run 38 — 2026-10-10 — backend/workspace sync: DF-WARPFS-69 fix verification + the true-conflict arm + direction flags (lane my-project-dogfood)

**Tick:** my-project-dogfood-2026-10-10-05-14-09 · **Head:** 3a62d63b · **Binary:** hilo 0.4.0 (v0.3.0-341-g3a62d63b-dirty, local debug build — workspace deps unchanged vs release 1195639a, so run 35/36's install premise stands)
**Angle:** runs 35/36 took fresh-install and foreign-repo graph surfaces on 10-10. Uncovered: the SYNC surface — commit 621fd5c8 (DF-WARPFS-69, equal-mtime no-op) landed hours before this tick and had never been dogfooded; run 19's named honest gap (the planner never saw a TRUE conflict) was still open. Store: local MinIO (`hilo-minio` container, :9000, bucket `df37`), scratch workspace /tmp/df37-warpfs (disposable).

## DF-WARPFS-69 fix — VERIFIED through real use

| Probe | Result |
|---|---|
| first push (4 files, 430 B) | OK 0.25s, 0 conflicts |
| repeat --push on aligned set | **0 transfers, 0 conflicts** (run 19 saw 3 phantom rows here) |
| repeat --pull on aligned set | 0 transfers, 0 conflicts |
| repeat --both on aligned set | 0 transfers, 0 conflicts |
| changed file push | 1 transfer, exactly **1** ledger row, repeat = 0 |
| delete propagation (rm local → --push) | remote object deleted, no phantom rows |
| remote-only file → --pull | landed, 1 transfer |
| **TRUE DIVERGENCE** (local v3 backdated, remote re-uploaded newer) | RemoteWins by mtime, **1 transfer + 1 ledger row** (`resolved:RemoteWins`) — run 19's gap closed |

The ledger itself is honest now: one row per genuine resolution, no rows during no-op runs.

## Finding 1 — DF-WARPFS-126 (P1; appended as DF-WARPFS-119, renumbered after a sibling tick's id collision): `workspace sync --push/--pull/--both` flags are INERT

`run_workspace_sync` (hilo-cli/src/commands/workspace.rs:86) never passes direction into
`SyncEngine::new` and calls `engine.sync()` unconditionally (line 154); the flags only color
the header label (line 131). Proven on real transfers: `--pull` UPLOADED a local-only file
(`↑ localonly.txt`), `--push` DOWNLOADED a remote-only file (`↓ pullme.md` under the
'sync push' header). Every run is a full two-way sync. The `backend sync` engine honors
directions correctly — the two sync surfaces have divergent direction semantics, and
one-way backup scripts (the most common sync use) get silent reverse transfers.
(The stale skill section claiming '--pull exits 2' described an even older binary — the
flags were added later but never wired.)

## Finding 2 — DF-WARPFS-127 (P2; renumbered as above): conflicts ledger path is undocumented/wrong in docs

`backend sync` prints '1 conflicts recorded' but no doc-tourist path holds the ledger:
README/§9 narrative points at the `.vfs/backends/` family (where mounts.yaml lives); the
actual file is `.vfs/sync/conflicts.jsonl` (planner.rs:436), and ONLY the spec names it
(specs/backend-backed-workspace-spec.md:178). `ls .vfs/backends/` after a conflict shows
mounts.yaml and nothing else.

## Step 2b — perf

Headline op = repeat no-op `backend sync --both` (what a user's cron does every minute):
**142.7 ms ± 13.7** warm (hyperfine 20 runs, 126-183 ms). Cold first push 0.25 s.
Nothing user-noticeable; the cost is network round-trips. No PERF row.

## Honest gaps

- rclone/s3sync tool arms: `setup` reports rclone present but the native path was exercised;
  external-tool driver not re-driven (covered by run 8's scope, unchanged surface).
- `stream` mode against S3: still unexercised (as run 19 noted).
- Install leg: SKIPPED-with-citation — runs 35 (26m32s) and 36 (24m37s) re-proved the
  documented build path on fresh bunker boxes within the last 24h; Cargo.toml/Cargo.lock
  show zero dep drift between the release build (1195639a) and this tick's HEAD (3a62d63b),
  so a third 25-minute install leg would have proven the same premise. The dogfood-log
  carries the explicit skip line.

## Verdict

🟢 **SHIPPABLE on the backend-sync surface** — the DF-69 fix does exactly what its commit
claims, the true-conflict arm resolves and logs honestly, delete propagation works, and
142 ms no-ops are free. But `workspace sync`'s direction flags are decorative (DF-WARPFS-126, P1):
a user who trusts the flag gets the opposite of what they asked for, silently.
