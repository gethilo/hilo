# Dogfood Run 13 — Public Docs Site + Fix Re-verification (2026-09-24)

**Tick:** coding-hermes-tools-dogfood-2026-09-24-15-28-40
**HEAD tested:** ef4748d (local release build `hilo 0.3.1-dev`, stamp v0.3.0-55-g192e797)
**Angle (stale-surface rule):** runs 1–12 covered CLI/graph, FUSE, backends, FFI,
plugins, triggers, permissions, Java, concurrency, MCP. No run had ever
**visited the public documentation site as a first-touch user** — the surface
every real new user hits before anything else. Second leg: re-verify, by real
use, the four fixes that closed on 2026-09-23/24 (DF-WARPFS-28/29/30/32/33/41)
plus the DF-38 .gitignore contract.

## Leg A — the public docs site (first-touch user)

The site is live at https://gethilo.github.io/hilo/ (HTTP 200, redeployed
2026-09-24 13:23). What a first-touch user actually experiences:

1. **Every "Guides" link on the landing page is a 404.** `docs/index.html`
   and `docs/index.md` link to `getting-started`, `graph-engine`, `mcp-tools`,
   `cli-reference`, `architecture` — all without a `.md`/`.html` suffix. The
   Pages site publishes raw repo docs with no Jekyll conversion to help (no
   `.nojekyll` needed either way): `getting-started` → 404 while
   `getting-started.md` → 200. All five links are dead. This is the front
   door of the project and it is broken.
2. **`dashboard.html` is a stale fabricated snapshot.** Live on Pages,
   footer "Generated 2026-07-12" (v0.2 era): claims "MCP Server — 15 tools"
   (real: 17), "10 crates" (real: 11), "57 Rust source files" / "~500 tests"
   (stale), "Quality Gates: static_analysis · lsp PASS" (AGENTS.md says those
   legs are disabled as fake-green), and Recent Commits frozen at
   `34dbd05 … v0.2 Rinnegan batch` — ~70 commits ago. Nothing generates it;
   it is a static HTML file that silently lies.
3. **MCP tools docs count: 17 documented, 17 served — match (verified by
   tools/list).** The count drift that run 8 hit (0.2.0 vs 0.3.0) is gone.
4. **README install claims are accurate now**: quickstart, build-time warning
   (15–20 min), and the verified-fresh-install note all match the bunker leg
   (see below). getting-started.md still claims `clang` and `CMake` are
   required — README explicitly says they are NOT; minor internal
   contradiction (P2, docs).

## Leg B — fix re-verification by real use (canopy corpus, 709 files, 6661 edges)

| Fix | Re-test | Result |
|---|---|---|
| DF-41 `vfs_list_directory` silent-empty | real dirs over stdio MCP | ✅ FIXED — `frontend/src` → 15 entries, `internal/card` → 11; bad path → explicit -32603 error |
| DF-33 concurrent read failures | 2 MCP servers same repo, simultaneous barrier; 8× CLI xargs -P8 | ✅ FIXED — both servers answered impact+stats; 8/8 CLI rc=0 |
| DF-32 trigger-mount leak on unmount | 3 mount/unmount cycles, pid-liveness watch | ✅ FIXED — daemon self-exits ~1s after `fusermount3 -u`; no manual kill; graph.db unlocked (stats ran fine during mount, DF-30 also holds) |
| DF-28 empty `triggers: []` loads defaults | stock manifest → mount --triggers | ✅ FIXED — triggers load from the init-written manifest |
| DF-29 nested-path events | file writes under `frontend/src/` | ✅ FIXED for new files — `frontend/src/trigger_canary13.ts` + `canary13.rs` (root) append edges in ~0.5s |
| DF-38 .gitignore managed block | `hilo init` on canopy (pre-existing .gitignore) | ✅ block appended, pre-existing content preserved |

## NEW DEFECT (this run): trigger engine never fires for `.tsx`/`.jsx`

Method: controlled writes into the mounted tree (mount --triggers --daemon),
each followed by a 15–30 s wait past the 500 ms debounce, checked against
`.vfs/graph/edges.jsonl` tail.

- Fired (edges appended): new root `.rs`, new nested `.ts`, **edits to existing
  `.rs` and `.ts` files** (`canary13.rs` +HashMap; `dayjs_canary13.ts` +luxon).
- Never fired (0/8 writes, incl. 30 s waits): **every `.tsx` write** — 3 edits
  to existing `frontend/src/App.tsx` (zod, yup, dayjs imports), a new
  `frontend/src/tsx_canary13.tsx`, a copy of App.tsx (`appcopy13.tsx`) and an
  edit to that copy (rxjs). Also the earlier App.tsx edits.

Root cause (read, not changed — file:line for the fix):
`hilo-cli/src/commands/mount.rs:502-513` `default_triggers()` watches only 9
globs: `*.go *.rs *.py *.ts *.js *.java *.c *.cpp *.rb` — no `*.tsx`, no
`*.jsx`. The engine itself handles both (`hilo-graph/src/parser.rs:59`
`"ts" | "tsx" => Some(Language::TypeScript)`), so this is purely the default
trigger-set's watch list. React/TSX repos — the most common TS shape — get a
"living map" that silently ignores every component edit. Same silent-empty
class the board has filed twice before (DF-41 list tool, run-10 triggers).

Filed as DF-WARPFS-45 (P1).

## Performance (Step 2b)

Release build, warm cache, hyperfine n=20, canopy corpus (6661 edges):

- `hilo graph stats`: 33.1 ms ± 3.2
- `hilo graph impact <file> --max-depth 3`: 26.8 ms ± 4.5
- cold `graph warm` (fresh .vfs): 17.2 s for 709 files (rayon, ~30s user CPU)

**No PERF row.** Nothing is slow enough for a user to feel; the numbers sit
squarely in the README's promised ranges and match runs 9–12.

## Install leg (bunker-las-03, agent 4f10f656)

bunker-las-02 was DOWN this run (bunkerd `activating`, spawn → connection
refused ×2 at 15:33Z) — the qa battery could not run there. Per the skill's
explicit-skip rule this is recorded, and the manual skill procedure (§0–5)
was executed on bunker-las-03 instead, which the skill designates as the
build host. See `2026-09-24-bunker-install.md` for the evidence.

## Verdict

🟡 **PROMISING-BUT-ROUGH** — the product core keeps getting better (4 real
defects fixed and verified by use; perf comfortably in spec), but the
project's public front door (docs site) is 404-broken and its flagship
"living map" silently ignores .tsx/.jsx edits. Both are cheap fixes the
foreman can land fast.
