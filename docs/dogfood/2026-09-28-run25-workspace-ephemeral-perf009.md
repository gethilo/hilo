# Dogfood Run 25 — 2026-09-28 — Workspace Ephemeral + PERF-009 Verification Attempt

**Verdict:** PROMISING-BUT-ROUGH (workspace tools solid; PERF-009 unverifiable due to stale binary)

**Promise tested:** Two surfaces: (1) `hilo workspace ephemeral` / `hilo workspace wipe` — the
undocumented tools from DOC-5 that manage rebuildable/redownloadable files; (2) PERF-009's
symbol-index persistence — does `.symbols_cache.json` actually get created and reused across
CLI invocations?

**Reality:**

1. **Workspace ephemeral/wipe: WORKS AS DOCUMENTED.**
   - `hilo workspace ephemeral` lists ephemeral files with size and glob pattern:
     `target/`, `.vfs/graph/`, `__pycache__/`, `.gitreins/logs/`, `tests/__pycache__/`.
   - `hilo workspace wipe --ephemeral` shows a dry-run plan ("would remove ...").
   - `hilo workspace wipe --ephemeral --apply` deletes the files and reports bytes freed
     ("freed 2087575 bytes across 31 file(s)").
   - After wipe, `ls target/debug/test.bin` correctly returns "No such file or directory".
   - No defects found. The tools are honest, safe (dry-run by default), and useful.

2. **PERF-009 symbol cache: CANNOT VERIFY.**
   - The installed binary at `~/.cargo/bin/hilo` is stale: `hilo 0.3.1-dev, build
     v0.3.0-14-g2b26d3f, built 2026-09-22T23:19:30Z`. PERF-009 landed in commit 75fdcdc
     on 2026-09-28. The installed binary predates the fix by 6 days.
   - Attempted local release build (`cargo build -p hilo-cli --release`) — timed out after
     10+ minutes (foreground cap ~180s, then background with 600s timeout, still building).
   - Attempted bunker build on las-03 (agent 63217649, fresh Debian, Rust 1.98.1 installed
     via rustup, repo cloned from GitHub) — also still building after 10+ minutes.
   - Cannot verify whether `.symbols_cache.json` is created, whether cache invalidation
     works on file changes, or whether the 23x-96x speedup holds.
   - **Finding:** binary freshness is a blocker for verifying recent perf work. The installed
     binary is 6 days stale while the repo has moved forward. A dogfood run that needs to
     verify a perf fix needs either (a) a pre-built binary that matches HEAD, or (b) a build
     that completes within the tick's time budget.

**Time-to-first-success:** ~2 min (workspace ephemeral/wipe worked immediately on the stale
binary; the PERF-009 verification was blocked, not slow).

**Friction count:** 2 defects (DF-WARPFS-99 stale binary, DF-WARPFS-100 build timeout).
0 usability notes (help text for workspace ephemeral/wipe is honest and complete).

**Perf (Step 2b):** Not measured — could not get a fresh binary to measure. The stale binary's
search times (0.55-0.58s warm on 111 files) are consistent with run 23's numbers, suggesting
no regression in the graph/search path itself, but the symbol-cache question is unanswerable
without the new binary.

**Install leg:** SKIPPED (DF-WARPFS-100, 10th infra incident). Spawned agent 63217649 on
bunker-las-03 (ssh reachable, bunkerd active, Docker 26.1.5). Installed Rust 1.98.1 via
rustup. Cloned gethilo/hilo from GitHub. Started `cargo build --release` in background.
After 10+ minutes, build still running (no target/release/hilo yet). Same result locally.
Root cause: full workspace release builds on fresh agents exceed the tick's time budget.
The README's "Standard source build" path is documented but not time-bounded — a fresh user
on a slow machine could wait 20-30 minutes. No defect in the install instructions themselves,
but the time cost is a usability friction that has now blocked 10 consecutive dogfood runs.

**Rows filed:** DF-WARPFS-98 (workspace ephemeral/wipe verified), DF-WARPFS-99 (PERF-009
unverifiable), DF-WARPFS-100 (SKIPPED-install-bunker). Appended via board_append.py, census
284 -> 287, 0 dups.

**Left behind:** this file, diagnostics.md Run 25 section, skills/hilo-usage/SKILL.md run-25
field notes, rows, this entry.

**Bunker cleanup:** agent 63217649 destroyed (TTL 2h, but explicit destroy is the contract).

</