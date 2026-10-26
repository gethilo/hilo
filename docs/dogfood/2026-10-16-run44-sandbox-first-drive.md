# Dogfood Run 44 — 2026-10-16 — first drive of `hilo sandbox` (DF-WARPFS-77 fix surface) + MCP HTTP transport (REV-WARPFS-3/5) — 🟡 PROMISING-BUT-ROUGH

Lane: warpfs-dogfood (read-only contract: no code changes, no scheduler mutation).
Build: HEAD a8f01fc5, debug binary at /var/tmp/hermes-cargo-target/debug/hilo
(build stamp v0.3.0-484-ga8f01fc5-dirty — note: CARGO_TARGET_DIR on this host is
/var/tmp/hermes-cargo-target, NOT ./target; the ./target/debug/hilo still lags HEAD).

## Promise under test

1. DF-WARPFS-77 (closed today, commits 7070d1b8 + f7826092): "the manifest
   `sandbox:` block is honored at mount time; `hilo sandbox check` verifies
   end-to-end; `hilo sandbox exec` runs a command inside the bwrap sandbox
   (§14.1 agent-shell model) with network/PID/root isolation per §14.3."
2. REV-WARPFS-3 (c91c83ed): "`hilo serve --http` serves GET /health on the
   manifest port; bind conflict is a startup error on the main thread."
3. REV-WARPFS-5 (98b859b7 + eac4f7c9): "MCP rate limiter admitted through
   config (`performance.rate_limit_rps`), unparsable manifest = hard error,
   GET /metrics exposes the live limiter snapshot."

## What was driven (real use, not the test suite)

Scratch projects: /tmp/df44/scratch-proj and /home/kara/df44-ws, both `hilo init`
+ a src/main.rs, manifest sandbox block enabled with varying flags. All rows
below quote the live binary's own output.

### `hilo sandbox check` / `exec` — the promise HOLDS

- `sandbox check` with no block: "no enabled sandbox block in the manifest —
  nothing to check (mount runs unsandboxed)", exit 0 (documented no-op).
- With block enabled: "sandbox check passed: ok", exit 0.
- `exec -- sh -c 'echo hi'` streams through, exit 0; child exit propagates
  (`sh -c 'exit 7'` → CLI exit 7 — the documented contract holds).
- isolate_network:true: curl → rc=6 (could not resolve), 000 http; loopback
  :18777 also refused (rc=7). Network isolation real.
- isolate_pid:true: 4 pids in /proc, no host processes visible. Real.
- read_only_root:true: /usr write → "Read-only file system"; workspace still
  rw (bind-mounted over the RO root); /tmp is a fresh empty tmpfs per exec
  (writes there do NOT reach host /tmp — good isolation, surprising to a user
  who set writable_paths:[/tmp] expecting host-tmp visibility).
- Missing block + exec: "error: no enabled sandbox block in the manifest for
  <dir> — nothing to exec inside", exit 1. Fail-closed, good.
- bwrap removed from PATH (PATH=/emptybin): loud warning + "bwrap not found",
  exit 1 on both check and exec. Honest refusal, good.

### BUT — three real defects (DF-WARPFS-150/151/152)

1. **Exec cwd is the HOST cwd, not the workspace; outside the workspace you
   land on a read-only host root (DF-WARPFS-150, P1).** `sandbox exec` in a
   scratch dir under /tmp with --source elsewhere starts the child in the host
   CWD (e.g. /tmp/df44/scratch-proj or /), which under read_only_root:true is
   READ-ONLY — the first write attempt fails with "Read-only file system" and
   no hint that the workspace (the rw bind) is elsewhere. Spec §14.1 says the
   agent's shell should be at /workspace (here: the workspace path). When CWD
   happens to be inside the workspace everything works (cd + write verified).
   Fix direction: chdir the child to the workspace (bwrap --chdir) or error
   loudly when the resolved CWD is outside the sandbox workspace.

2. **The sandboxed process inherits the FULL host environment — including 48
   KEY/SECRET/TOKEN-shaped vars and real private keys — defying §14.2 (DF-WARPFS-151,
   P0).** `env | grep -c 'KEY\|SECRET\|TOKEN\|sk-'` = 48 inside the sandbox;
   `cat ~/.ssh/back-office.key` prints "-----BEGIN OPENSSH PRIVATE KEY-----";
   ~/.aws/credentials readable (353 bytes). §14.2 promises "Agent reads
   /etc/shadow → Kernel-blocked": /etc/shadow is indeed unreadable only
   because its 640 root:shadow perms survive the root bind (same as the host);
   every file the invoking user owns is trivially readable, secrets included.
   An "isolated" sandbox that exports OPENROUTER_MANAGEMENT_KEY et al. to the
   wrapped process is not isolation for the agent-shell threat model §14.1
   describes. Fix direction: env -i + a minimal allowlist (bwrap alone does
   not scrub env; the executor must), or a documented `env_passthrough:` key,
   plus /home bind-allowlisting instead of full-root bind.

3. **`--source DIR` resolves the manifest from DIR but leaves the child in the
   host CWD (DF-WARPFS-152, P2, same root as 150).** run_sandbox_exec
   (sandbox.rs:70) canonicalizes --source only for manifest lookup and the
   workspace bind; nothing ever chdirs. Documented examples
   (`hilo sandbox exec --source /path/to/project -- curl ...`) work only
   because curl doesn't care about cwd; any build/test command breaks.

### FUSE mount + sandbox block (the closed row's other half)

- `hilo mount /tmp/df44/mnt2 /home/kara/df44-ws --daemon` with sandbox
  enabled: mounts, serves the tree, banner + log path correct. Zero bwrap
  processes on the host — the mount itself is (by design, per daemon.rs
  comments) NOT bwrap-wrapped; the sandbox config is stored and only the
  loud fail-open warning path exists. A user reading spec §14.1 ("Bubblewrap
  wraps the agent's SHELL") will not know that from the mount alone. This is
  a docs/contract gap (DF-WARPFS-153, P3), not a regression: the wiring is
  real, the isolation surface is `sandbox exec`.
- Daemon log file created at the then-documented in-mount default
  (DF-WARPFS-134 era; DF-WARPFS-63 later moved the default outside the
  mount to `$HOME/.vfs/logs/mount-daemon.log`) as documented (empty
  without --triggers, fine).
- Unmount: docs name `fusermount -u` (§866 region) — plain `hilo mount` has
  no unmount verb (workspace does); fusermount3 -u failed "Device or
  resource busy" from inside the tree, succeeded from / after killing the
  daemon pid. Minor friction, pre-existing.

### `hilo serve --http` (REV-WARPFS-3) — works, port trap confirmed live

- REV-WARPFS-9's premise re-proven: with the fleet-default 8766 taken by
  off-by-one, first start died exactly as the row says: "HTTP transport
  failed to bind port 8766: I/O error: Address already in use (os error 98)",
  non-zero exit, main-thread error. The bind-readiness channel works as
  designed. Manifest port 18777 → `/health` →
  {"status":"ok","version":"0.4.0"} http=200; unknown route → 404 (tested).
- PENDING row REV-WARPFS-9 stands (no --port flag; only the manifest can
  move the port). Not re-filed; this is fresh evidence, recorded in the log.

### `hilo serve --mcp --http` + rate limiter (REV-WARPFS-5)

- Startup log names the config: "MCP server started (rate_limit_rps=5,
  reconcile_budget_ms=2000)" — the P3's "no signal that config applied" is
  addressed; rate_limit_rps=0/5 both parsed from the manifest.
- GET /metrics live: {"queue_depth":0,"rate":5.0,"rejected":0} — rate mirrors
  config. stdio bursts (8/30/120 concurrent one-shot sessions) all answered
  (120/120 tools/list responses, 7105 bytes each), metrics stayed
  rejected:0/queue_depth:0 — correct for separate processes (the limiter is
  per-process; cross-process bursts are out of its scope, as the original
  row already established).
- One real gap (DF-WARPFS-154, P2): a raw POST to /mcp (the natural guess for
  any MCP client author) and to / return 404 with no pointer to /health or
  the stdio transport. The HTTP surface advertises health + metrics only; a
  one-line 404 body ("MCP transport is stdio-only; HTTP serves /health and
  /metrics") would save every first-time integrator a source dive.

## Step 2b — perf numbers (nothing user-noticeable; no PERF rows)

- `hilo --version`: 0.02s. `sandbox exec -- true` (debug build):
  0.15-0.29s, hyperfine 40 runs mean 190.1ms ±52.7 vs raw bwrap 151.5ms
  ±44.8 (1.25x — the two bwrap --version probes + manifest parse). strace:
  3 bwrap execs per `exec` (2x is_available probe + 1 real), plain `true`
  child ~0 sys time. `sandbox check`: ~0.2s. `ignore check`: 0.12s.
- All comfortably under any user-noticeable threshold for a sandbox spawn;
  no profile justified. Cold = warm here (no caches involved beyond the
  binary).
- MCP stdio 120-session burst: 0.56s wall (each session full
  initialize+tools/list handshake) — no wait feedback issue observable.
- The one mildly interesting number: `hilo --version` 0.02s vs `ignore check`
  0.12s (manifest+config load dominates); both fine.

## Verdict

🟡 PROMISING-BUT-ROUGH. The DF-77 fix genuinely lands: check/exec/check's
failure ladders are honest, exit codes propagate, network+PID+RO-root
isolation is real and verified live. But the sandbox as shipped is a
filesystem namespace, not an environment boundary — it hands the wrapped
agent the full host env (live API keys, SSH private keys) and starts it in a
host CWD that is read-only under the default recommended config. For the
§14.1 "wrap the agent's shell" threat model these are P0/P1, not polish.
MCP HTTP transport + rate limiter work as the review rows promised; the
default-port trap (REV-WARPFS-9) is confirmed live on fleet hosts.

## Findings → rows

- DF-WARPFS-150 (P1) exec cwd = host cwd, read-only writes outside workspace
- DF-WARPFS-151 (P0) full host env (secrets/keys) inherited by sandboxed process
- DF-WARPFS-152 (P2) --source never chdirs (same root as 150, separate surface)
- DF-WARPFS-153 (P3) mount + sandbox block: no bwrap on mount path; docs don't
  say isolation is exec-only
- DF-WARPFS-154 (P2) HTTP transport 404s give no hint MCP is stdio-only

REV-WARPFS-9 re-proven (bind conflict, no --port flag) — event recorded, row
left pending for the foreman.

## Left behind

- This file.
- skills/hilo-usage/SKILL.md: run-44 section (sandbox reality + serve facts),
  run-21 "sandbox IGNORED" section corrected.
- docs/dogfood/diagnostics.md: run-44 entry.
- .coding-hermes/dogfood-log.md: run-44 entry.
- Board rows DF-WARPFS-150..154 + event for the REV-WARPFS-9 re-proof.

Scratch (not committed): /tmp/df44/*, /home/kara/df44-ws.

## Install leg (2026-10-16, same run) — PARTIAL, honest

bunker-mvp was the only reachable bunker host (las-01/02/03/04 offline). Spawned
ephemeral agent a04646a4 (2 cores, 15.6GB). The documented fresh-install path ran
for real: rustup (needs `-y` non-interactively — small docs gap), rustc 1.99.1,
`git clone https://github.com/gethilo/hilo.git` rc=0 at 91a9a2a,
`cargo build --release -p hilo-cli` — 467 crates compiled clean with the now-default
vendored OpenSSL, no sudo, no pkg-config. The build reached the hilo-cli/hilo_fuse
tier at ~35 min but the agent hit its TTL/reaper at ~50 min (`bunker info`:
not_found) before the artifact finished, so no binary smoke and no headline-feature
smoke on the fresh box. NOT a silent pass and not a full PASS: the install path is
re-proven through the compile phase; the run-35 lesson (install PASS ≠ feature works
on the fresh box) remains unreproved on 0.4.0. Next leg: TTL ≥ 2h or pre-warmed
target dir (2 cores + duckdb deps cannot finish inside 45m).
