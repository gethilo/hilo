# Dogfood Run 10 — Triggers (Living Map) + Permissions Surface (2026-09-23)

**Tick:** coding-hermes-tools-dogfood-2026-09-23-07-09-38 ·
**HEAD tested:** v0.3.0-20-g8832da0 (local release build `hilo 0.3.1-dev`, built 2026-09-23T02:38Z) ·
**Verdict: 🔴 DOES-NOT-DELIVER for the triggers surface; ⚪ UNKNOWN-VALUE for the permissions surface** (CLI/graph/MCP/FUSE read surfaces remain shippable per runs 1–8, re-verified healthy this run)

## The promise tested this run

Two crates ship no CLI door of their own; their promises live in docs and the
manifest:

1. **hilo-triggers** (`docs/hilo-triggers.md`): "Every file save triggers
   parse-and-diff (tree-sitter AST → edge updates)" — the *living map*: an
   agent edits code and the graph stays current without a manual
   `hilo graph warm`. Documented user door: `hilo mount /mnt/vfs --triggers`.
2. **hilo-permissions** (`docs/hilo-permissions.md`): glob rules with mode
   bits, "Used by FUSE for kernel-level enforcement and by the MCP server for
   agent access control", configured via the manifest `permissions.rules`.

Neither surface was touched by runs 1–9 (CLI/CAG, FUSE, backends, FFI,
plugins, git-backend). Run 10 used both the way a real user would: a fresh
corpus (fd @ 9e8927e — repo never used before), documented quickstart, then
the triggers mount with real edits, then the permission paths.

## Trigger engine: what a user actually gets

```console
$ hilo init && hilo graph warm        # 268 edges / 23 files, <1s — healthy
$ hilo mount /tmp/mnt --triggers --daemon
[trigger-engine] loaded 0 triggers for mount /tmp/mnt   # ← the flag did nothing
Hilo mounted at /tmp/mnt (triggers enabled)             # ← printed anyway
```

**DF-WARPFS-28 (P1): with the stock `hilo init` manifest, `--triggers` loads
ZERO triggers.** `load_triggers()` (hilo-cli/src/commands/mount.rs:342)
prefers the manifest and `hilo init` writes `triggers: []`;
`parse_manifest_triggers` returns `Some(vec![])` for the empty list, so the
9-source-extension default set (mount.rs:446) never loads. The mount prints
"(triggers enabled)" with nothing enabled. Deleting the `triggers:` key from
the manifest (undocumented workaround) loads the defaults.

With defaults force-loaded, the deeper defect:

**DF-WARPFS-29 (P0): parse-and-diff never fires for files in subdirectories
— which is every real project.** Discriminator experiment, same mount,
defaults active ("9 triggers active" on the stderr banner):

- `echo … >> src/cli.rs` (nested path, with imports): 30 s watch —
  edges.jsonl mtimes unchanged, edge count unchanged (268), engine silent.
  Repeated across three independent mounts (also with a concurrent
  graph.db lock, and clean): identical result.
- `printf 'use std::…; use crate::config::Config; …' > probe_root.rs`
  (file AT the project root, imports included): **edges appended 268 → 270
  in 0.51 s.** `grep -c probe_root edges.jsonl` → 2.

Root cause (read, not fixed — dogfood files, the foreman works): the event
loop builds `FileEvent.path` as `PathBuf::from(&name)` — the bare inotify
filename, relative to whichever directory was watched
(hilo-triggers/src/engine.rs:195). The correct reconstruction
(`self.watches` reverse-lookup → `dir.join(&name)`, engine.rs:203-209) is
computed only for the backend sync hook, never assigned to the FileEvent.
`parse_and_diff_sync` then does `std::fs::read_to_string("cli.rs")` →
ENOENT for anything not in the project root. Every failure path inside it
logs via `info!()` / `tracing` — and **hilo-cli installs no log subscriber
anywhere** (grep: no env_logger/tracing_subscriber/log::set in
hilo-cli/src), so the miss is INVISIBLE. A user sees "9 triggers active",
edits `src/main.rs`, and the map silently goes stale.

**DF-WARPFS-30 (P1): every trigger-bearing mount takes a conflicting DuckDB
lock on graph.db** (run-9-era lock defect, live at HEAD, now with two
product-level symptoms):

```console
$ hilo graph stats            # while a mount is up
error: failed to open DuckDB graph database: DuckDB error: IO Error:
Could not set lock on file ".vfs/graph/graph.db": Conflicting lock is held
in …/hilo (PID 3003225) by user kara.
```

The trigger engine opens `.vfs/graph/graph.db` itself at startup
(mount.rs:271-284) and holds it. Two mounts, or a mount plus any CLI graph
command, fight over the lock; in one run the ENGINE lost ("[trigger-engine]
cannot open graph.db … Conflicting lock") and continued with `db_conn: None`
— impact computation silently disabled. A user running the documented mount
plus the documented `graph stats` workflow gets a hard error with no hint
that their own mount is the holder.

Mount read-path itself is healthy: `ro` fuse mount up in 0.28 s warm
(`--daemon` returns in 11 ms), tree + xattr passthrough correct, EROFS on
write is by design (`read_only: true` hardcoded in mount.rs — consistent
with the §8 note that only stream-mode backend mounts go rw).

## Permissions surface: unfalsifiable as shipped

- **The mount's rule set is hardcoded.** `Hilo::new`
  (hilo-fuse/src/ops.rs:113) builds the engine from
  `default_protections()` (.vfs/**, .git/** → 0o444) only. `grep -rn
  manifest hilo-fuse/src/daemon.rs hilo-cli/src/commands/mount.rs` → zero
  hits: the manifest `permissions.rules` block is parsed by hilo-core
  (`PermissionConfig`) and wired NOWHERE in the mount path.
- **Kernel enforcement is unobservable.** The mount is hardcoded read-only
  (EROFS from the kernel before the engine matters), so no manifest rule —
  even if wired — could deny a write a user could attempt. The only
  default-protection behavior a user could observe (open on .vfs/**)
  is unreachable because the ignore stack hides .vfs from the mount.
- **The MCP claim is false.** `grep -rni permission hilo-mcp/src/` → 0
  matches; `docs/hilo-permissions.md` says "Used by … the MCP server for
  agent access control".

Net: there is NO user-observable path in which manifest permission rules
affect any surface. That is DF-WARPFS-31 (P1, docs/honesty + missing
wiring). The run-9 doctrine applies: "the docs describe an aspiration;
the binary doesn't ship it."

## What held up

- CLI quickstart on a brand-new corpus: init 12 ms, warm <1 s (268 edges /
  23 files, 100% coverage), stats warm 23.5 ms ± 2.5 (n=20, hyperfine).
- Mount read path: 0.28 s to live mount, xattr passthrough intact, EROFS
  semantics clean, clean unmount; `--daemon` correct.
- Run-9's fixes re-verified: `backend list` reads mounts.yaml; plugin load
  rejects non-WASM (headers checked in skills/hilo-usage notes).
- Perf (Step 2b, coding-hermes-perf law): headline ops are FAST — stats
  23.5 ms ± 2.5 warm / ~30 ms cold; mount-to-available 0.28 s warm cycle;
  parse-and-diff itself, when it fires, appends in 0.51 s. **No PERF row:**
  nothing a user would call slow; the run's waits were broken
  functionality (silent no-op triggers, DuckDB lock errors), not latency.

## Time-to-first-success & friction

- Triggers surface: NEVER for the documented workflow (stock manifest →
  0 triggers; workaround → subdirectory writes still no-op). The only
  firing configuration (defaults + root-level file) is not something a
  user can discover.
- Friction count: 4 findings (DF-WARPFS-28..31), plus one usability note
  folded into DF-30 (the lock error names the holder PID — good — but
  nothing tells the user that holder is their own mount).

## Verdict reasoning

The trigger engine's dispatch chain is real (pattern match → event filter →
debounce → parse → append, proven by the root-file probe) but it is wired
off by default and blind to subdirectories — the "every file save" promise
fails for the canonical layout of every real repo. The permission engine
has genuinely no user surface. That is DOES-NOT-DELIVER for the surfaces
this run set out to test, with the core graph/mount/CLI still healthy.
