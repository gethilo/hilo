# Dogfood Run 19 — backend sync suite (native S3 engine, real store)

**Date:** 2026-09-25 · **Head:** 2ce90ba · **Binary:** hilo 0.3.1-dev (v0.3.0-99-g2ce90ba, local release build)
**Surface:** the GAP-55 backend-sync suite — `backend setup/mount/list/sync`, the §7.1 trigger
sync hook, the LWW planner and its conflicts ledger. Runs 15-18 covered MCP, FFI, plugins and
trigger cold-start; this suite had never been dogfooded end-to-end against a real remote.
**Store:** Hetzner Object Storage hel1 (real S3-compatible store, dedicated `df19-warpfs/`
prefix — backups untouched). Workspace: /tmp/df19-warpfs/repo (scratch, disposable).

## What a real user does, and what happened

| Step | Command | Result |
|---|---|---|
| init | `hilo init` | OK (warns about missing .git hooks — honest) |
| setup | `backend setup --type s3` / `--type gdrive` | Good output: engine/credentials/tool table + real next-step lines |
| mount | `backend mount --type s3 --bucket … --prefix df19-warpfs --at s3docs --endpoint … --tool native --mode mirror` | OK; `backend list` truthful; mounts.yaml matches spec §11.3 |
| push | `backend sync --push` (AWS_PROFILE auth) | **FAIL — opaque `InvalidArgument (400): service error` (DF-WARPFS-65)** |
| push | same, static env creds | OK: 3 transferred, 281 B, 6.3 s cold |
| incremental | edit 1 file, push | OK, 2.5 s — but **3 phantom conflicts recorded (DF-WARPFS-67)** |
| no-op | `backend sync --both` | OK: 0 transferred, 0.66 s warm — no ping-pong (§13.9 tie-break holds) |
| pull | remote-only file added via aws cli, `--pull` | File landed; **but 4 transfers for 1 remote file (DF-WARPFS-68)** |
| §7.1 hook | `hilo mount --triggers mnt` | **PANIC at s3.rs:1125 (runtime-in-runtime); sync hook silently absent (DF-WARPFS-66)**; mount itself comes up |
| mount writes | `echo > mnt/…` | Read-only (default FuseConfig.read_only=true; undocumented surprise) |

Cross-check probe: boto3 against the same endpoint + same profile creds lists and uploads
fine — the 400 is hilo's client, not the store.

## Timings (headline operation = push, warm store)

- cold first push (3 files, 281 B): **6.3 s** wall (0.05 s user) — network-bound, fine
- incremental push (1 changed file): **2.5 s**
- no-op `--both`: **0.66 s** — comfortably fast
- Nothing here justifies a PERF row; the time sinks are network round-trips, not code.

## Verdict

🟡 **PROMISING-BUT-ROUGH.** The sync engine's core loop is genuinely good: real Hetzner push /
pull / idempotent re-sync all work, the plan line names bucket+endpoint+region (DF-WARPFS-12's
disclosure holding up), default ignores skip `target/`, and the no-op tie-break prevents
ping-pong. But the auth story breaks on the *most common* AWS auth method (profile file →
opaque 400), the §7.1 auto-sync hook panics and silently vanishes, and the conflicts ledger
records routine resolutions until the word "conflict" stops meaning anything. None of the four
defects require redesign — all four are contained fixes (rows 65-68).

## Rows filed

- DF-WARPFS-65 (P1) — profile-file credentials never reach the endpoint client; opaque 400
- DF-WARPFS-66 (P0) — `mount --triggers` panics in SyncHook::new; §7.1 sync silently absent
- DF-WARPFS-67 (P2) — LWW resolutions logged as conflicts; ledger meaning erosion
- DF-WARPFS-68 (P2) — `--pull` transfers local-only files too; summary lacks direction split

## What was NOT exercised (honest gaps)

- rclone/external-tool arms (no rclone on host) and gdrive/onedrive/dropbox (no CLIs) — `setup`
  output for them verified shape only. The external-tool driver has unit tests but no live run.
- `stream` mode against S3 (placeholder materialize + write-through) — separate surface, needs
  its own run.
- True conflict (both sides changed since last sync) — the planner never saw one in this
  session; row 67's fix will need a real divergence test.
