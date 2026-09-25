# Dogfood Run 21 — 2026-09-25 (task-router-dogfood: C/C++ + Ruby languages, §14 sandbox, DF-61/64 fix re-verification)

## Angle (stale-surface rule)

Runs 1–20 covered CLI/graph (1–6, 18), FUSE (7), backends + FFI ABI (8),
git-backend + plugins (9), triggers + permissions (10), Java + concurrency +
Python FFI (11), MCP server (12), docs site + fix re-verification (13),
workspace (14), MCP official SDK (15), Go FFI (16), plugin surface (17),
cross-surface session (18), backend sync vs Hetzner (19), Python FFI × live
backend (20). What NO run ever did:

1. **Warm a C/C++ corpus** — the flagship C/C++ `sys:` header form was never
   exercised on a real C repo (20 runs, and the README's headline example is a
   C header query). Also **Ruby** — bringing languages exercised to 7 of 26.
2. **Enable the CHANGELOG's "Bubblewrap sandboxing"** as a user (the claim has
   stood since the 0.2.x era; spec §14 documents it in detail).
3. **Re-verify the DF-WARPFS-61/64 graph self-heal fix** (2af3c0d, closed by
   foreman tick 232 two hours before this run) in real use.

HEAD tested: f13a95d (origin/master == local master). Local release build
`hilo 0.3.1-dev, build: v0.3.0-104-gce9de75-dirty`. Corpora:
`/tmp/dogfood-warpfs-r21/sidekiq` (25d4350, 177 files) and
`/tmp/dogfood-warpfs-r21/redis` (788 C/H files, 562 warmed).

## Promise under test

*"An agent can answer structural questions about any codebase — dependencies,
entrypoints, test coverage, blast radius — by querying a pre-computed metadata
graph via CLI / MCP / FUSE — in <1s, without reading files" — in any of 26
languages, with agent processes isolated per spec §14.*

## What worked (real-use verified)

- **Ruby pkg-form blast radius is exact**: `pkg:sidekiq/api` → 21/21 ==
  `grep -rl "require.*sidekiq/api"` ground truth (18 test + 3 lib,
  `ast_exact conf=1.00`); `pkg:sidekiq/manager` → 2/2. Warm 0.96s / 392
  edges / 103 files.
- **C .c-file blast radius is exact**: `impact local:server.h` returned all
  72 `.c` files that include it directly.
- **C warm is fast and honest about failures**: 11.2s cold / 3235 edges / 539
  files; the 2 non-UTF-8 files (deps/tre, deps/lua) are named, not swallowed.
- **Meta round-trips on C** (`meta --set user.vfs.role --value core-list` →
  `getfattr` → `hilo meta` byte-consistent) and classify stays sane
  (entrypoint/library/unknown on redis).
- **stats/impact stay in the millisecond class on 539 files** (32.6ms /
  44.9ms depth-3) — the perf claims hold everywhere except search (below).
- **DF-WARPFS-61/64 self-heal, headline case VERIFIED in real use**:
  `graph.db` deleted → next `graph impact` transparently rebuilt from
  edges.jsonl and returned correct results. No ritual.

## What broke (rows filed)

1. **DF-WARPFS-75 (P1) — C headers are never edge sources.** Zero
   header→header include edges exist (jq census), so every header's impact
   under-counts by exactly its header-includer count (rax.h 1/4, sds.h 10/18,
   adlist.h 7/9 …) and transitive C blast radius ("who breaks if I change
   server.h") cannot be answered at all.
2. **DF-WARPFS-76 (P0, PERF) — `graph search` re-parses every file per
   invocation.** 4.204s on redis (539 files) vs 33ms with `--no-symbols`
   (129x), 243ms on sidekiq (103 files) — superlinear; `--limit` doesn't
   help. The default symbol extractor (semantic.rs:44) re-reads + tree-sitter
   parses every document per process; the persistent parse cache warm
   maintains is ignored.
3. **DF-WARPFS-77 (P0) — §14 sandboxing is not wired.** Manifest `sandbox:`
   block (spec §14.3 example, verbatim) + bwrap 0.10.0 installed → mount
   RC=0, tree served, `pgrep -c bwrap` = 0. mount.rs:99 hardcodes
   `sandbox: None`; the daemon's bwrap-missing refusal branch is unreachable;
   `BubblewrapExecutor::run` has no production caller. The shipped feature is
   arg construction + unit tests.
4. **DF-WARPFS-78 (P1) — Ruby file-form queries silently empty** while
   pkg-form is exact — the per-language file→package gap family on its 5th
   language (Go, Python, TS/JS, Java before this). New face: `stats` lists
   `lib/sidekiq.rb` and `lib/sidekiq/api.rb` as **orphans** despite 21+
   incoming edges.
5. **DF-WARPFS-79 (P1) — the DF-61/64 self-heal misses the zero-BYTE db.**
   `graph.db` emptied in place (disk-full / killed copy — the run-18
   cold-start face) → opaque DuckDB IO error on every command AND on the
   documented `graph warm` recovery. The probe-open the fix added is where
   DuckDB rejects the 0-byte file, before any heal logic. Deleting db →
   heals fine. Workaround: `rm graph.db` (undocumented).
6. **DF-WARPFS-80 (P2) — the C header id form is `local:adlist.h`** (bare
   basename) while the CLI's accepted-forms error advertises `sys:<header>`;
   `sys:adlist.h` and `sys:src/adlist.h` both fail on C corpora. A user
   following the error text verbatim can never query a C header.
7. **DF-WARPFS-81 (P2) — SKIPPED-install-bunker.** All three bunker hosts
   unavailable this tick: las-03 and las-01 ssh connect-timeout; las-02
   reachable but bunkerd crash-looping (restart counter 25158) — refusing to
   bind plaintext :10002/:10001 without `tls.enabled: true`, plus an audit
   chain-head `record hash mismatch (tampered)` warning needing operator
   review. `bunker-qa.sh` was NOT launched. Both are host-owner decisions, out
   of a dogfood lane's scope.

## Verdict

🟡 **PROMISING-BUT-ROUGH** — for the surfaces this run visited.

- The core graph engine keeps its record: exactness wherever an edge exists,
  millisecond queries, honest failure reporting on warm, deterministic output.
- But a real C developer gets under-counted blast radius (P1) through an
  id form the tool itself misdocuments (P2); a real Ruby developer gets a
  confident empty answer (P1); and **search — the one command a new user
  tries on a big repo — costs 4.2 seconds of pure CPU per keystroke-query**
  (P0) because it silently re-parses the repo.
- The §14 sandbox result is the run-17 plugin story repeating: infrastructure
  built, unit-tested, advertised — and not reachable from any user surface
  (P0). Premature completion, again, at the wiring layer.
- The DF-61/64 fix re-verification is the honest split foremen should see:
  the headline case works in real use; the adjacent damage mode (0-byte file)
  strands the user with an opaque error and defeats the documented recovery.

Time-to-first-success: ~30s on sidekiq (init 16ms + warm 0.96s + stats), ~1
min on redis. Friction count: 7 (5 P0/P1 behavioral, 2 P2).

## Install leg

SKIPPED — see DF-WARPFS-81. Fifth infra incident in the install-leg record
(runs 9/10/11 ENV-BLOCKED image, run 13 las-02 down, run 15 launcher crash).

## Reproduction

```bash
# corpora
git clone --depth 1 https://github.com/redis/redis.git
git clone --depth 1 https://github.com/sidekiq/sidekiq.git
cd redis && hilo init && hilo graph warm          # 11.2s, 3235 edges
# C header under-count
hilo graph impact local:rax.h --max-depth 1       # 1 result
grep -rl '#include "rax.h"' src deps | wc -l      # 4
# search cost
hyperfine --warmup 3 --runs 10 'hilo graph search "append list value"'
# vs: 'hilo graph search "append list value" --no-symbols'  -> 33ms
# sandbox no-op
printf '\nsandbox:\n  enabled: true\n  engine: bubblewrap\n' >> .vfs/manifest.yaml
hilo mount /tmp/mnt --daemon && pgrep -c bwrap    # 0
```
