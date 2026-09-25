# Dogfood Run 17 — 2026-09-25 (coding-hermes-tools-dogfood, plugin surface + permissions/manifest re-test)

## What run 17 set out to test

Angle chosen by the stale-surface rule: across 16 runs the plugin system had
been touched only as an ABI honesty check (run 9 loaded a text file, run 9's
DF-WARPFS-22 fix added header validation + persistence). No run had ever
executed the full documented plugin workflow — build a `.wasm`, declare it in
the manifest like spec §4 shows, load it, list it — nor re-tested the
permissions surface after DF-WARPFS-31's doc-only fix, nor driven `hilo
classify`, `hilo graph rule-check`, and the mount xattr path on the same
fresh project in one session.

Promise under test: *"a user can extend Hilo with a WASM plugin — declare
hooks in `.vfs/manifest.yaml`, load the module, and have the runtime pick it
up"* (spec §3.2/§4, README feature list, docs/hilo-plugins.md), and
*"manifest permissions rules protect files"* (spec §4 "Single Source of
Truth").

HEAD tested: 47f153b (origin/master == local master, release binary rebuilt
from master, `hilo 0.3.1-dev` build 47f153b).

## What worked (real-use verified)

- **The full CLI workflow on a fresh scratch project** (`/tmp/dogfood-warpfs-run17/corpus`):
  `hilo init` → `hilo graph warm` (55 ms cold on bunker for 1 file) →
  `graph stats` → `graph impact` → `classify` (role=library, status=beta) →
  `hilo meta` round-trip. All green, all fast.
- **`hilo graph rule-check`** — an undocumented-but-real power feature. Added
  a `rules:` block to the manifest and it executed the DuckDB query
  (`Rule 'std-importers' — 1 match(es): src/lib.rs`, exit 0); wrong rule name
  gives a legible error listing available rules (exit 1). Better than most of
  the documented surface.
- **Plugin header validation is honest now.** A 13 KB text file renamed
  `.wasm` is rejected with a precise error (exit 1: "missing \0asm magic at
  byte 0 (file is 13413 bytes)"); a plain `.txt` is rejected before any wasm
  check. A real 8-byte wasm header loads with honest metadata (`0 hooks, 0
  edge types`).
- **FUSE mount round-trip on the scratch project** — mount, ls, stat
  (0644), read-through, xattr surface, read-only enforced at the kernel level
  (`Read-only file system` on write), clean `fusermount3 -u`, daemon self-exits.
- **Install leg PASS on las-bunker-03** (details in the log entry).

## What broke — findings DF-WARPFS-58/59/60

### DF-WARPFS-58 (P2): `hilo plugin load` destroys the plugin it loads

The documented loop is: drop your module in `.vfs/plugins/`, run `hilo plugin
load <path>`. If `<path>` is *inside* `.vfs/plugins` (where a user following
the manifest examples puts it), load **truncates the file to 0 bytes**: the
persist step (`std::fs::copy(path, dest)`, hilo-cli/src/commands/plugin.rs:65)
copies the file onto itself. `std::fs::copy` opens the destination with
O_TRUNC before reading, so the load prints `loaded plugin: …` / `persisted
to: …` / exit 0 and the "persisted" artifact is an empty file. The next
`hilo plugin list` silently drops it (0 bytes fails the wasm header check, as
designed). Loading the identical 8 bytes from *outside* `.vfs/plugins` works
perfectly — which is the only reason the bug stayed invisible.

Reproduction (deterministic, both arms): `/tmp/dogfood-warpfs-run17/repro_selfload.py` —
self-load: rc=0 before=8 after=0 TRUNCATED; outside-load: rc=0 persisted=8 OK.
Lesson: the run-22 fix ("persist so list sees it") introduced a second write
path nobody tested against the one location the manifest examples name.

### DF-WARPFS-59 (P2): two plugin systems, neither connected to hooks

Spec §4 shows `plugins:` in the manifest with `hooks: - on: file_write` and
`provides: edge_types`. The manifest parser accepts the whole block
(`deny_unknown_fields` passes — the schema exists). But nothing reads
`manifest.plugins` afterward, and `PluginRuntime::dispatch_hook` is reachable
only from `hilo plugin load`'s throwaway runtime and hilo-plugins' own tests:
`hilo-fuse` and `hilo-triggers` contain **zero** plugin references, so a
file_write event can never dispatch a plugin hook. Meanwhile `hilo plugin
load` builds its own runtime from the file argument and forgets it on exit.
Two islands: a declared-hook surface that does nothing, and a CLI loader
whose runtime dies immediately. docs/hilo-plugins.md honestly discloses
"execution is simulated", but neither README (which lists `hilo plugin load`
as a feature) nor getting-started nor the spec example says the declared
hooks never fire.

### DF-WARPFS-60 (P3): permissions honesty never reached the front door

DF-WARPFS-31 (run 10) was closed by *documenting* that manifest
permissions.rules are parsed but not consumed. The documentation landed in
one place: docs/hilo-permissions.md. README's feature list never mentions
permissions at all (so it neither promises nor disclaims), but spec §4 —
titled "The Manifest — Single Source of Truth" — still presents a
`permissions:` block with concrete `mode: 0444` examples as if it works.
Re-test this run: manifest with `mode: 0600` on `src/**`, mounted → file
serves at 0644, rule invisible. A fresh user reading the spec comes away
believing enforcement exists.

## Verdict

🟡 PROMISING-BUT-ROUGH for the plugin/extensibility surface (the load path
destroys its artifact in the canonical location; hooks declared but
unreachable); the core read path (init/warm/stats/impact/classify/rule-check/
mount) remains ✅ and is the fastest it has ever measured.

Time-to-first-success on the core workflow: <1 min. Time-to-first-success on
the plugin workflow: never (the only documented load location destroys the
plugin; the manifest surface is silent).

## The lesson for future dogfooders

Run 9 tested `hilo plugin load` with garbage inputs and found the honesty
bugs. Run 17 tested it with a *valid* plugin loaded from the *documented*
location and found the data-loss bug. The happy path was the untested path —
again. When a fix adds a write path, dogfood the fix at the location the
docs' own examples use, not just at the location the bug report used.
