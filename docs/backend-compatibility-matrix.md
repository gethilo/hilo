# Backend Compatibility Matrix

Feature & compatibility matrix for Hilo backend-backed workspaces
(spec: `specs/backend-backed-workspace-spec.md` §14, GAP-055).

A backend-backed workspace mounts a remote store (S3, Google Drive, OneDrive,
Dropbox) as an overlay filesystem: Hilo's inotify engine sees file changes and
syncs them through an existing sync tool when present (rclone, official CLIs,
s3sync) or through Hilo's native engine, which is ignore-aware so build
artifacts and other transient files stay local-only.

## Capability matrix

| Capability | S3 native | S3 rclone | S3 s3sync | GDrive rclone | GDrive gdrive CLI | OneDrive rclone | OneDrive CLI | Dropbox rclone | Dropbox CLI |
|---|---|---|---|---|---|---|---|---|---|
| Stream mode (lazy fetch) | ✅ | ✅ | ❌ | ✅ | ✅ | ✅ | ⚠️ | ✅ | ⚠️ |
| Mirror mode | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Selective sync (ignore-aware) | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Multi-agent shared view | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Incremental (listing diff) | ✅ | ✅ | ✅ | ✅ | ⚠️ | ✅ | ⚠️ | ✅ | ⚠️ |
| Checksum verify | ✅ etag | ✅ | ✅ | ⚠️ md5 | ⚠️ md5 | ⚠️ | ❌ | ⚠️ content hash | ❌ |
| Rename tracking | ❌ v1 | ✅ --track-renames | ❌ | ✅ | ❌ | ✅ | ❌ | ✅ | ❌ |
| Auth | AWS env chain | rclone config | S3_* env | rclone config | OAuth | rclone config | OAuth | rclone config | OAuth |
| Windows/macOS | ✅ | ✅ | ⚠️ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |

✅ = supported · ⚠️ = partial/limitations (see notes) · ❌ = not in v1

## Limitation notes (❌ and ⚠️ cells)

- **S3 native — Stream mode**: supported (native engine). **Rename tracking:
  not in v1** — a rename is seen as delete+upload (the S3 API has no rename
  primitive; use the `--track-renames` rclone mode if rename history matters).
- **S3 s3sync — Stream mode: not supported** (s3sync is a mirror-only
  tool). **Rename tracking: not supported** (no rename detection).
- **GDrive gdrive CLI — Incremental: partial** (the official `gdrive` CLI
  lacks a stable listing-diff primitive; fall back to rclone for large
  trees). **Checksum verify: partial (md5)** — Drive stores md5 checksums
  but the CLI does not expose per-file verification reliably.
- **OneDrive CLI — Stream mode: partial** (the OneDrive sync client streams
  on demand but Hilo cannot control the lazy-fetch boundary). **Incremental:
  partial** (the client uses its own change feed; Hilo's listing diff falls
  back to full-list). **Checksum verify: not supported** — the OneDrive
  client does not expose file checksums for verification.
- **Dropbox CLI — Stream mode: partial** (Dropbox smart-sync streams on
  demand; Hilo cannot control the boundary). **Incremental: partial** (the
  Dropbox client's delta API is not exposed to the CLI). **Checksum verify:
  not supported** — the Dropbox CLI does not expose content hashes.

## Implementation status (2026-08-26)

| Slice | Status |
|---|---|
| Native S3 two-way sync engine (`hilo_backends::sync`, 20+ tests) | ✅ landed (GAP-055 tick 149) |
| Ignore engine (gitignore subset, `.hiloignore`) + `docs/ignore-file.md` | ✅ landed (GAP-055 tick 149) |
| `hilo ignore check <PATH>` diagnostic CLI | ✅ landed (GAP-055 tick 150) |
| `hilo workspace sync --bucket/--prefix/--at` CLI | ✅ landed (GAP-055 tick 149) |
| External-tool drivers (rclone / gdrive / onedrive / dropbox) + `--tool` resolution | ✅ landed (GAP-055 tick 152: `ExternalToolDriver`, `BackendRegistry`) |
| `hilo backend mount --type gdrive/onedrive/dropbox/external` (writes mounts.yaml), `backend sync` (planner-driven, ignore-aware), `backend setup` (detect/creds/next steps) | ✅ landed (GAP-055 tick 155) |
| `hilo mount --triggers` backend sync hook (spec §7.1): inotify → debounce (HILO_DEBOUNCE_MS) → ignore/ephemeral check → dirty batch → settle push; poll pull every `poll_secs` | ✅ landed (GAP-055 tick 157: `hilo_triggers::sync_hook`) |
| Stream mode placeholders (`hilo-fuse`) | ⏳ spec §8 — pending |
| MCP tools 15 → 17 (`vfs_workspace_ephemeral`, `vfs_workspace_wipe`) | ✅ landed (GAP-055 tick 156) |
| Ephemeral classification + `workspace ephemeral` / `wipe --ephemeral` | ✅ landed (GAP-055/056 tick 151: `hilo_backends::ephemeral` + CLIs) |

Status legend: ✅ landed · ⏳ spec'd, not yet implemented. Cells above mark
the v1 capability plan, not current runtime support for every tool — check
the per-slice status table for what is live today.
