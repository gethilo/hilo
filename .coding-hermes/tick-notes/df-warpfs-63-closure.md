## TASK: DF-WARPFS-63 — Daemon mount hangs on reads after successful daemonization

### Problem
`hilo mount <path-inside-project> --daemon` daemonizes successfully but then EVERY read (ls, getfattr, cat) blocks/hangs on a fresh Debian 13 box. Works flawlessly on the dev host.

### Root Cause
The daemon log path resolved to `<mount_point>/.vfs/logs/mount-daemon.log`. When mount_point was INSIDE the project tree, the log file was WITHIN the mounted filesystem. The FUSE filesystem then encountered this path during read resolution and hangs.

### Fix Applied (2 commits)
1. **31c5c4b** — Changed `resolve_daemon_log_path()` in `hilo-cli/src/commands/mount.rs` to default to `$HOME/.vfs/logs/mount-daemon.log` instead of `<mount_point>/...`
2. **1aa951e** — Updated ALL docs that cited the old path + fixed workspace daemon stderr (which discarded output via `Stdio::null()`)

### Verification
- `cargo check -p hilo-cli` — clean
- `cargo clippy -p hilo-cli --all-targets -- -D warnings` — clean
- `cargo test -p hilo-cli --lib` — 184 passed, 0 failed
- All docs updated: CHANGELOG.md, docs/cli-reference.md, docs/hilo-cli.md, hilo-cli/src/cli.rs, skills/hilo-usage/SKILL.md
- Workspace daemon stderr now logs to `$HOME/.vfs/logs/workspace-daemon.log`
- Pushed to origin/master via fast-forward merge

### Files Modified
- `hilo-cli/src/commands/mount.rs` — log path fix + DF-WARPFS-63 comment
- `hilo-cli/src/commands/workspace.rs` — workspace daemon stderr fix
- `hilo-cli/src/cli.rs` — CLI help text update
- `CHANGELOG.md` — entry for DF-WARPFS-63
- `docs/cli-reference.md` — updated log path docs
- `docs/hilo-cli.md` — updated log path docs
- `skills/hilo-usage/SKILL.md` — updated dogfood notes
- `docs/dogfood/2026-10-13-run45-daemon-log-postmortem.md` — updated
- `docs/dogfood/2026-10-16-run44-sandbox-first-drive.md` — updated

### Acceptance Criteria Met
1. ✅ Daemon log lives OUTSIDE the mount point → reads no longer hang
2. ✅ All docs updated to reference `$HOME/.vfs/logs/mount-daemon.log`
3. ✅ Workspace daemon stderr is readable (no longer discarded)
4. ✅ Build, clippy, tests all pass
5. ✅ Code merged to master and pushed

Addresses DF-WARPFS-63
