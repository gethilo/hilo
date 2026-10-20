# Dogfood run 46c — warpfs (Hilo) — the map's NUMBERS, recomputed — 2026-10-20

- **Lane:** `coding-hermes-tools-dogfood` (batch `2026-10-20-12-00-33`) — **third run in the batch**
  (labelled 46c: the `warpfs-dogfood` lane fired TWICE in this batch — run 46 = `afd49c4e`, and a
  duplicate fire that took the 46b label for a Go-oracle angle while this run was uncommitted).
  The sibling lane `warpfs-dogfood` ran the same project in the same scheduler batch (its run 46 doc:
  `docs/dogfood/2026-10-20-run46-agent-map-realuse.md`, rows `DF-WARPFS-64-R2`, `GAP-104-R2`,
  `DF-WARPFS-158..162`, commit `afd49c4e`). This run deliberately took a surface its rows do not cover:
  **the served numbers of the map, recomputed from ground truth** (`graph stats`, `graph audit`),
  in the sense of `references/metric-surface-audit.md`.
- **Verdict:** 🟡 **PROMISING-BUT-ROUGH** — every headline operation is fast and the *derived semantics*
  are honest (ignore stack, map-exclusion demotion, impact provenance, budget rules all behave as
  documented), but two **served numbers on the flagship "what is this system" surfaces are wrong and
  are explained wrongly in the same output**: `graph stats` counts symbol nodes as files, and
  `graph audit` claims nothing was left unaudited while its own census lists 26 unaudited files.
- **Target:** warpfs at `/home/kara/warpfs`, HEAD `33ce3a24` (= `origin/master`).
- **Binary:** `/var/tmp/hermes-cargo-target/release/hilo`, `--version` build stamp
  `v0.3.0-530-g33ce3a24-dirty`, built `2026-10-20T10:32:47Z` (HEAD build; the `-dirty` is the
  `.gitreins/logs` rot named by `HYG-002`). The PATH binary `/home/kara/.cargo/bin/hilo`
  (`v0.3.0-293-g3adfb9ed`, 2026-10-08) was **not** used.
- **Corpus (real use):** a fresh `git clone https://github.com/gethilo/hilo.git` at `33ce3a24`
  (uncredentialed, documented path) into `/tmp/df20-warpfs/corpus` — 440 files on disk, 170 `.rs`.
  Plus a 2-file minimal fixture at `/tmp/df20-warpfs/mini` built to isolate one defect.
- **Never touched:** any product code, the repo's board rows, repo visibility/permissions.

## Step 1 — the promise under test

> *"Give your AI coding agent a pre-built map of every codebase it touches — dependencies, entrypoints,
> test coverage, blast radius — without burning context window on file reads."* — `README.md:3`

An agent acts on the map's **summary numbers** as much as on its rows: "how many files are in the map",
"what subsystems exist", "what was left unaudited". This run asks whether those numbers mean what their
own output text says they mean.

## Step 2 — real use (what was driven, in order)

```bash
git clone https://github.com/gethilo/hilo.git corpus && cd corpus
hilo init                     # rc=0
hilo graph warm               # cold: 237 files, 3818 edges, 8 languages
hilo graph stats              # the number surface
hilo graph understand "change how the parse cache decides a file is unchanged"
hilo graph surfaces && hilo graph coverage-links && hilo graph test-classes && hilo graph rollup
hilo graph audit              # the trust surface
hilo graph impact hilo-core/src/manifest.rs
hilo ignore check <paths>     # the ignore stack, 5 rule shapes
```

Working as documented (recorded so the next run does not re-derive them):

| Check | Command | Result |
|---|---|---|
| Ignore stack semantics | `hilo ignore check target/debug/foo` etc. | builtin defaults fire (`rule: target/`, `source: builtin defaults`), root `.hiloignore` fires (`source: root .hiloignore`), `fixtures/` matches at any depth, a middle slash anchors, `*.md` does not match `x.md.bak`, last match wins — gitignore semantics as documented in `docs/ignore-file.md` |
| `.hiloignore` ≠ graph discovery | add `hilo-graph/tests/fixtures/` to `.hiloignore`, then `graph clean` + `graph warm` | still parses all 237 files in 9.59 s; the fixture edges remain. **Documented**: `graph warm --help` says *"`.hiloignore` controls backend sync and does not override graph discovery"* |
| Fixture demotion (REV-WARPFS-6) | `graph understand <task>` | MAP tier: 54 entries, **0** under `tests/fixtures`; `graph.map_exclude_paths` defaults (`tests/fixtures`, `examples`, `vendor`) are documented in `docs/graph-engine.md:282-301`. SIGNATURES keeps them by that same documented scope (170/278 rows are fixtures) |
| `graph wiring` exit code | `hilo graph wiring` | rc=1 with `error: 8 wiring finding(s)` — documented: *"Exit code is non-zero when findings exist, so CI can gate on it"* (`docs/cli-reference.md:291`) |
| understand budget | `hilo graph understand --help`, output size | 47 446 B / 889 lines at the default 6000 budget; documented as a DETAIL-tier cap (*"it caps the `DETAIL` tier at 60% of the budget in characters"*), not a whole-output cap |

Two things that are **not** filed because they are documented limitations, with the measurement kept:

- **Rust extraction is `use …` / `extern crate …` only** (`docs/graph-engine.md:15`). Measured cost:
  `hilo graph impact hilo-core/src/manifest.rs` returns 101 rows but **misses 4 of the 20 files that
  textually reference `hilo_core`** — `hilo-cli/src/commands/serve.rs:121`,
  `hilo-ffi/src/lib.rs:438`, `hilo-fuse/src/daemon.rs:24`,
  `hilo-core/tests/plugins_not_implemented.rs:24` — all four call `hilo_core::manifest::Manifest::…`
  inline with no `use` statement. (Same class as the filed Elixir row: *"the extractor handles
  use/import/alias only"*.) A blast-radius consumer should therefore treat `impact` as a
  `use`-statement graph, not an identifier-reference graph.
- **The documented rustup bootstrap fails non-interactively**: `curl … | sh -s -- --profile minimal`
  rc=1 `error: Unable to run interactively. Run with -y to accept defaults, --help for additional
  options`; the same line with `-y` rc=0 (rustc 1.99.1). Reproduced, but run 45 already assessed and
  consciously declined to file it ("a real user has a TTY") — recorded here as reproduced, not as new.

## Finding 1 (P2) — `graph stats` counts SYMBOL NODES as files

Minimal reproduction (`/tmp/df20-warpfs/mini`, a 2-file Rust crate whose `src/widget.rs` implements
`Display`; the repo root contains no source file at all):

```
$ cat .vfs/graph/edges.jsonl
{"from":"type:Widget","to":"iface:Display","rel":"implements","provenance":"ast_heuristic","confidence":0.8}
{"from":"src/widget.rs","to":"type:Widget","rel":"conformance_of","provenance":"ast_heuristic","confidence":0.8}

$ hilo graph warm
Discovered 2 edges across 0 files (1 languages)
Coverage: 2 files = 0 contribute edges + 0 package facades (__init__.py) + 2 no imports + 0 unreadable + 0 unsupported extension

$ hilo graph stats
Total edges: 2 distinct / 2 raw (edges.jsonl)
Total files: 2
File census: 2 files with edges (distinct edge sources) — warm last counted 2 discovered files = 0 contribute + 2 zero-edge (2 no imports + 0 package facades); some sources may be stale entries no longer on disk (edges.jsonl is append-only)
Components (2 total):
  .: 1 files, 1 edges
  src: 1 files, 1 edges
Most connected: iface:Display
```

The project has **one** source file. "Total files: 2" is half symbol node (`type:Widget`), and the
component roster invents a repo-root component (`.: 1 files`) in a project whose root holds only
`Cargo.toml`. The same command's own warm line says `0 files` contributed edges, and the census
explains the surplus as stale append-only entries — in a graph built seconds earlier from a tree in
which nothing was deleted.

Full-size corpus (`33ce3a24` clone, after `hilo graph clean` + a full `hilo graph warm` that re-parsed
237/237 files in **9.59 s**):

```
Total files: 297
File census: 297 files with edges (distinct edge sources) — warm last counted 237 discovered files = 218 contribute + 19 zero-edge …; some sources may be stale entries no longer on disk (edges.jsonl is append-only)
Components (22 total):
  hilo-graph: 126 files, 2129 edges
  .: 78 files, 81 edges          <-- no root-level source file exists
```

Ground truth, recomputed from the freshly rebuilt `edges.jsonl`
(`/tmp/df20-warpfs/split-sources.py`):

```
distinct sources by family: [('FILE', 219), ('type:', 78)]
real file sources with no '/' (root level): 0
type: rows rel census: [('implements', 81)]
sample: {"from":"type:AuditOptions","to":"iface:Default","rel":"implements","provenance":"ast_heuristic"}
```

So **297 = 219 real files + 78 `type:<Name>` symbol nodes** (a 36 % inflation), all 78 mis-bucketed as
the `.` component, and the census text blames a staleness condition that cannot exist after
`graph clean`.

Same class, second surface — `graph understand` renders pseudo-nodes as files the agent cannot open:

```
$ hilo graph understand "change how the parse cache decides a file is unchanged"
## MAP
hilo-graph/src/parser.rs →
  - Language
  ...
type:CacheConfig → (no symbols extracted)
type:CwdFileFixture → (no symbols extracted)
iface:Filesystem → (no symbols extracted)
```

MAP tier: 54 entries = 33 real files + 21 pseudo-nodes (39 %); SIGNATURES: 5 more.

Root cause (`hilo-graph/src/graph.rs`):

- `:2450` `SELECT COUNT(DISTINCT "from") FROM edges` — no node-family filter — feeds
  `stats.total_files` (`:2593`), printed at `hilo-cli/src/commands/graph.rs:1487` and reused as the
  census (`:1506`);
- the component roster query (`:2547-2554`) excludes `pkg:`/`sys:`/`std:`/`external:` but **not**
  `type:`/`iface:`, while its own comment two lines above says pseudo-node families *"must never appear
  as components"*;
- `resolution::is_symbol_node` (`hilo-graph/src/resolution.rs:366`) knows only
  `pkg:`/`sys:`/`std:`/`external:` — the list every other filter mirrors.

Fix direction: one shared node-family predicate (`is_symbol_node` extended with `type:`/`iface:`)
applied to both the count and the component query, plus a census sentence that reports the real cause;
acceptance must pin the minimal 2-file crate (`Total files: 1`, no `.` component) and the corpus case.

## Finding 2 (P3) — `graph audit` says nothing was left unaudited while its own census lists 26 files

```
$ hilo graph audit
audit: 448 public function(s) over 237 file(s) in /tmp/df20-warpfs/corpus
...
unknown: 0 — every source file in the corpus is in a language with a public-function extractor
(rust, go, python, typescript, javascript); nothing was left unaudited

census:
    c: 1 file(s), 0 public fn — no public-function extractor for c
    cpp: 13 file(s), 0 public fn — no public-function extractor for cpp
    java: 12 file(s), 0 public fn — no public-function extractor for java
    rust: 170 file(s), 418 public fn — …
```

The headline sentence is contradicted by the census **in the same output**: 26 files (c 1, cpp 13,
java 12) are in languages with no public-function extractor and were left unaudited, but the
`unknown` bucket stays 0 and asserts the opposite. `docs/cli-reference.md` promises the other
behaviour: *"Languages without a public-function extractor are named under `unknown` — they are never
collapsed into `ok`"*.

Mechanism (`hilo-graph/src/audit.rs`): the test/generated skip at `:421-423` runs **before** the
unsupported-language branch at `:425-429`, so files in a no-extractor language that are test-bearing
(or generated) never reach `unknown_files`; the reassurance text is keyed only on
`unknown_files.is_empty()` (`:611-616`). In this corpus all 26 live under `tests/fixtures/`, i.e.
exactly the population the skip drops. The census rows are correct — the summary over-claims
*nothing was left unaudited*, and the documented `unknown` bucket cannot report this case.

Fix direction: count census languages without an extractor in the rule sentence (or scope the claim to
non-test files), and make the docs' `unknown` promise match the emitted rule.

## Step 2b — what using it costs (measured, nothing filed)

| Operation (exact command) | Cold | Warm | Notes |
|---|---|---|---|
| `hilo init` | 0.00 s | — | one-shot |
| `hilo graph warm` (237 files, 3818 edges) | **9.62 s** (`real`; `user 15.30`) | **0.65 s** (`parse cache: 237/237 unchanged`) | `graph clean` + full re-warm: 9.59 s |
| `hilo graph stats` | — | 0.03 s | |
| `hilo graph understand "<task>"` | 1.08 s | 1.05 s | 47 446 B output |
| `hilo graph audit` | — | 0.61 s | |
| `hilo graph impact <file>` | — | 0.12 s | 101 rows |
| `graph surfaces` / `coverage-links` / `test-classes` / `rollup` | — | 0.11 / 1.21 / 0.11 / 0.11 s | |

**Verdict for Step 2b: nothing here is slow enough for a user to notice, so no `PERF-<n>` row is
filed.** The one cost a user does feel is a cold `graph warm` on a fresh clone (9.6 s for 237 files,
0.04 s/file) — acceptable, and the second run is 15× faster because the parse cache holds. A
flamegraph was not warranted: no operation exceeded 10 s and the profile of a 9.6 s cold parse over
237 files would not change any decision.

## Step 5 — install leg (fresh machine, ephemeral bunker agent)

Spawned my own agent (`df20-warpfs-install`, bunker-mvp, `--cpu 6 --memory 8G --ttl 2h`,
created `2026-10-20T12:07:47Z`, expires `14:07:47Z`) and drove the README's **documented no-sudo
path** from `$HOME/leg-install.sh` (staged via `bunker cp`, per the piping rule):

- host image: `Linux 6.8.0-142`, 8 cores, 15 GB RAM, `/dev/fuse` present, `/usr/bin/fusermount3`
  present, `cc/gcc/g++/perl/pkg-config/git` present, **`cargo` MISSING**, `zig` MISSING — the exact
  target of the "bare images, no pkg-config" section;
- stage 1 (documented, verbatim): rc=1 `Unable to run interactively. Run with -y …`; stage 1b
  (`-y`): rc=0 → rustc 1.99.1;
- stage 2: `git clone https://github.com/gethilo/hilo.git` rc=0, HEAD `33ce3a24` — no credential;
- stage 3: `cargo build --release -p hilo-cli --no-default-features --features vendored-openssl`
  launched `12:08:22Z`; the repo's committed `.cargo/config.toml` caps `jobs = 1`, so an 8-core
  agent compiles single-threaded;
- **at report time the build was still running** (`target/` 1.1 GB, host disk 15 GB free) — the leg's
  final BUILD_RC and the post-install smoke (`hilo --version`, `init`, `graph warm`, `mount`) are
  recorded in `.coding-hermes/dogfood-log.md` if they landed before the tick closed, and are
  **UNPROVEN** otherwise. Never read as a pass. The sibling lane `warpfs-dogfood` ran the
  `bunker-qa.sh` fresh-install cell on the same host in parallel (agent `11d784da`, ttl 4h) — its
  evidence owns that path; this run does not re-drive it.

## Verdict

🟡 **PROMISING-BUT-ROUGH.** The map itself is real, fast, and honest about its own scope (ignore stack,
fixture demotion, extraction rule, wiring exit code all documented and matched). What is not honest yet
is two served NUMBERS on the surfaces an agent uses to summarise a codebase: `graph stats` over-counts
files by 36 % on the project's own repo and invents a root component out of symbol nodes, and
`graph audit` prints "nothing was left unaudited" over its own list of 26 unaudited files. Both are
cheap, local fixes in the number-emitting code, and both are pinned by a 2-file minimal reproduction.

**Do not** read this run as a re-proof of the install path: the fresh-box install is UNPROVEN until the
build above completes and the smoke runs.
