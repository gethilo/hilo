# Dogfood Run 42 — 2026-10-13 — DF-WARPFS-134 re-proof on a live `mount --daemon --triggers` session

Tick: warpfs-dogfood-2026-10-13-17-25-05. Target board `.coding-hermes/board/tasks.jsonl`
read first; pending rows 134/136/137/139/140/141/142/143 reviewed. HEAD moved since run 41
(5deb8880 "fix(mount): --daemon keeps child stderr") — the discriminating re-proof this run
is DF-WARPFS-134 against that fresh fix. Read-only lane contract: no code changes, foreman
not woken (rows filed for it).

## Verdict: 🟢 DF-134 fix CONFIRMED live / 🟡 two NEW adjacent findings filed (DF-144 silent-CWD fallback, DF-145 triggers watch wrong dir)

## Re-proof (DF-WARPFS-134 — `--daemon` discards stderr → trigger engine observably dead)

Binary: `target/debug/hilo` built 13:36Z, tree reports `v0.3.0-445-...` at commit 5deb8880
(fix present). Drive: `hilo mount /tmp/df42-mnt --daemon --triggers` from /home/kara/warpfs.

- Banner: `Hilo mount daemonized (pid 166658) — filesystem available at /tmp/df42-mnt
  (log: /tmp/df42-mnt/.vfs/logs/mount-daemon.log)` — resolved path printed, as documented.
- Daemon log file EXISTED and was APPENDED with exactly the output the filed row said was
  lost: `[trigger-engine] loaded 13 triggers for mount /tmp/df42-mnt`, tracing INFO mount
  lines, `[trigger-engine] watching /home/kara/warpfs — 13 triggers active`.
- fd ground truth: `/proc/166658/fd/2 -> /tmp/df42-mnt/.vfs/logs/mount-daemon.log`
  (stderr IS the log file, no more Stdio::null).
- Tear-down: `fusermount3 -u /tmp/df42-mnt` → daemon exited (pgrep empty), final log lines
  `unmounting session` / `Hilo mounted successfully` appended. Clean lifecycle.
- Prior evidence extended: run 37 (filed 134) proved the OLD binary discarded stderr; this
  run proves the FIXED binary at HEAD keeps it. Row 134 premise no longer holds.

## NEW finding evidence

**Silent CWD fallback (DF-WARPFS-144, P1):** the mount under /tmp/df42-mnt served the CLI's
CURRENT WORKING DIRECTORY checkout, byte-identical to /home/kara/warpfs (AGENTS.md and
Cargo.toml diff-clean, same 36-entry top-level listing), NOT the path given on the command
line (/tmp/df42-mnt was an empty scratch with a src/main.rs probe file that never appeared
under the mount; writing through the mount failed). User-visible impact: `hilo mount
<other-dir> --daemon` silently mounts your checkout wherever you point it — a wrong-project
mount that looks like success. Confirmed before filing: docs never document any CWD
behavior; cli-reference.md:780 says "Mount the current directory" for the bare verb but the
path-argument semantics are nowhere.

**Triggers watch the wrong tree (DF-WARPFS-145, P1):** `--triggers` watches the daemon
process's CWD (`hilo-cli/src/commands/mount.rs:126 watch_dir = current_dir.clone()`), not
the mounted source. From /home/kara/warpfs, the engine watched /home/kara/warpfs. A user
who mounts a remote/other checkout gets a trigger engine pointed at their terminal session's
directory — reparse fires on the wrong repo, and the mount never sees changes to the source
it serves. Not documented anywhere (grep docs/hilo-triggers.md, cli-reference.md).

## Perf (Step 2b): nothing to file. The run's waits were: one 420s tool hang on an FUSE
underlying-write probe (my probe's fault, not the product's — filing devalues real rows),
and the mount itself daemonized instantly. No user-noticeable slowness in the exercised
surface.

## Install leg: SKIPPED-install-bunker, explicit — same premise as DF-WARPFS-136: runs 35+36
proved fresh-machine install PASS within 24h on fleet bunkers; this run's target was the
daemon-logging fix at HEAD, not an install path. No new install premise to prove.

## Left behind: this log entry + docs/dogfood/2026-10-13-run42-daemon-log-reproof.md (both
committed pathspec). Rows DF-WARPFS-144/145 on tasks.jsonl. Scratch mounts unmounted,
probes removed; daemon pid 166658 verified exited. Foreman woken: NO (read-only contract).
