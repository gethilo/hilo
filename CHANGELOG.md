# Changelog

All notable changes to Hilo are documented in this file.

## [Unreleased]

### Added

- **Git-aware graph staleness detection** (DF-WARPFS-83). The post-merge
  hook used to wait for a `.vfs/.dirty` marker that nothing ever wrote, so a
  second machine that pulled never refreshed its graph. The marker protocol
  is replaced by a freshness ledger: every completed `hilo graph warm`
  stamps `.vfs/graph/state.json` with the git HEAD it observed, and a later
  warm that finds HEAD moved (typically after a pull, whose preserved file
  mtimes an incremental `--changed` warm cannot see through) warns
  `graph is out of date — ...` and heals by rebuilding and re-stamping in
  the same run. The post-merge hook now warms whenever a pull changed
  graph-relevant files (any supported source language, or the tracked
  `.vfs/manifest.yaml` / `.vfs/graph/edges.jsonl`); docs-only pulls skip
  the work. Undecidable states (no git repo, no git binary, unborn HEAD,
  corrupt ledger) stay silent and never block a warm. Hooks must be
  reinstalled (`hilo init`) to pick up the new post-merge body.

### Added

- **Search index vocabulary persisted, so a query stops re-reading the
  corpus** (DF-WARPFS-76). `hilo graph warm` now records each file's
  definition names (`symbols`) and comment/docstring tokens (`doc`) in its
  per-file parse cache, and the search surfaces consume them instead of
  re-deriving them per invocation:

  - `hilo_graph::ParseCacheTerms` serves the default extractor path
    (`search` + `index_symbols` with no caller-supplied extractor — MCP
    `vfs_graph_search` and `understand`'s semantic fallback) from warm's
    entry under the same `{m, s}` fingerprint warm already trusts. Only a
    missing/stale entry, or one written by an older warm, is read and
    extracted; a query never writes the cache.
  - `.symbols_cache.json` (version 3) records each file's
    `(mtime nanos, size)` as scanned, so an unchanged corpus is validated by
    `stat` alone — the previous version re-read and re-hashed every source
    file on every one-shot query. Any mismatch (or an older cache) still goes
    through the content scan, which is what proves the new bytes; a file that
    moves under the scan is recorded `Volatile` and can never satisfy the
    fast path.
  - Both surfaces now parse warm's cache keys ONCE per query
    (`parse_cache_keys` + `zero_edge_documents_for`), instead of parsing the
    same JSON twice (`IgnoredAny` skips the payloads).

  Measured on a warmed 935-file redis checkout, `hilo graph search "append
  list value"`: 97.7 ms → 86.5 ms mean (`--no-symbols` floor ~48 ms); the
  symbol channel's incremental cost over the path-only index fell from
  ~50 ms to ~14 ms across paired runs, and the top-20 result list is
  byte-identical. The search path opens no source file when the caches are
  current.

- **Edge deduplication for `edges.jsonl` + `hilo graph gc`**
  (REV-WARPFS-8): every writer now enforces the `(from, to, rel,
  provenance)` identity — the trigger/watcher append path went through
  raw `append_edges` and was the measured duplicate source (175
  duplicate groups / 537 redundant rows of 3120 on the review corpus),
  warm's deduped append now also collapses duplicates within one batch,
  and all writers serialize on an advisory `edges.jsonl.lock` flock. The
  new `hilo graph gc` compacts existing files (first occurrence kept,
  atomic temp+rename swap, idempotent; malformed lines preserved).
  `docs/graph-engine.md`'s "unique index on the DuckDB table" claim was
  drift — no such index exists in `ensure_schema`; dedup lives at the
  JSONL append boundary. Run `hilo graph gc` once per repo, then
  `hilo graph warm` to rebuild the DuckDB cache from the compacted file.

- **Annotations survive `git push` via the `.vfs/annotations.jsonl`**
  sidecar** (DF-WARPFS-95): xattrs do not travel through git, so
  `hilo meta --set` annotations died at the push. A successful set inside a
  Hilo workspace is now also mirrored into the git-tracked
  `.vfs/annotations.jsonl` (compact JSONL, one record per `rel_path` +
  `attr`, updated in place on re-annotation; outside a workspace the
  behavior stays xattr-only). List mode merges sidecar records that have no
  local xattr, marked `(sidecar)` — the exact gap a fresh clone has — and
  the new idempotent `hilo meta apply [--dry-run]` re-applies those records
  as xattrs without ever clobbering an existing local xattr.

### Fixed

- **Daemon logs moved OUT of the served filesystem; workspace daemon keeps
  its stderr** (DF-WARPFS-63): the DF-WARPFS-134 default placed the mount
  log INSIDE the mounted tree — on a fresh Debian 13 box the FUSE
  filesystem met its own log during read resolution and every read
  (ls/getfattr/cat) hung.
  `hilo mount --daemon` now defaults to
  `$HOME/.vfs/logs/mount-daemon.log` (`--log-file` still wins). The
  sibling `hilo workspace mount --daemon` no longer discards its stderr
  via `Stdio::null()`: it appends to
  `$HOME/.vfs/logs/workspace-daemon.log` (created on demand, append mode)
  and the banner prints the resolved path.
- **`hilo mount --daemon` keeps the child's stderr — persistent log file**
  (DF-WARPFS-134): the daemonized child's only logging channel (trigger
  engine eprintln!s + the `--triggers` tracing subscriber) was discarded
  via `Stdio::null()`, making `--triggers --daemon` observably dead. The
  parent now always hands the child a real log file: the new
  `--log-file <PATH>` flag when given, else
  `$HOME/.vfs/logs/mount-daemon.log` (DF-WARPFS-63 moved the default out
  of the mount point; dirs created on demand,
  append mode). The resolved path is printed in the "Hilo mount
  daemonized" banner; stdout stays detached.
- **`graph impact` reaches flat (top-level) Python modules in file form**
  (GAP-104): `app.py` doing `import util` emits the edge
  `{from: app.py, to: pkg:util}`, but `hilo graph impact util.py` answered
  "no dependents" (rc=0) because a flat module — no `__init__.py` package
  ancestor — resolved to no `pkg:` node. `python_module_for_file` now maps a
  file with no regular-package ancestor to its bare stem (`util.py` →
  `util`), the exact node the parser emits for `import util`; the mapping is
  stem-only, so host directory names never join the module. Packaged
  modules (GAP-064, full dotted paths) and the loud not-in-the-graph error
  (GAP-085) are unchanged and pinned by tests.
- **`graph search` reaches behaviour-owning files for intent prompts**
  (GAP-116): Wave 11 measured natural-language MRR 0.025 on six pinned
  repositories — the owning file appeared in only 2/6 outputs — while
  exact-symbol lookup scored 0.457. Search indexed a file's path and its
  defined symbol names, so a question ("a property derived from other model
  fields … serialization JSON Schema") could not reach `pydantic/fields.py`,
  whose path names none of it. The index now carries three weighted channels —
  path (1.0), defined symbols (2.0), and the file's own comment/docstring
  vocabulary (0.5) — and query text is filtered of question words and
  prepositions (`content_tokens`) before matching. Definitions therefore
  outrank mentions, and documentation lets intent vocabulary reach the owner.
  BM25 length accounting is unchanged (path + first-8-symbols), so pinned
  pre-existing rankings are preserved by construction. The CLI caches each
  file's documentation vocabulary in `.vfs/graph/.symbols_cache.json`
  (version 2) so a query never re-reads the corpus. Wave 11's cases are pinned
  as a regression harness (`hilo-graph/tests/wave11_search_test.rs` plus
  `tests/fixtures/wave11/`, the real upstream files at their pinned SHAs);
  on that corpus intent MRR rises 0.444 → 0.519 with the owning file inside
  the top 5 on 6/6 cases, and exact-symbol MRR is unchanged (0.917 ≥ the 0.457
  baseline). Full-repository re-measurement is GAP-118.
- **`graph understand` context packs omit the module the task is about**
  (GAP-101): the Wave 11 six-repository bake-off scored context packs by
  required-path recall and measured 19.3% — the pack for "add a new ORM field
  type" was 3.5x larger than a competitor's yet did not contain
  `django/db/models/fields/`, the one directory the task cannot be done
  without, because anchor discovery compares task tokens to the raw path
  string and drowned the object's own module in the token-flood. The signal
  engine now seeds the **domain-object module family**: files whose own name
  carries a task token as a module word (`type_adapter.py` for "TypeAdapter",
  `alarm.cc` for "alarm"), directories named after it (`fields/` for "field",
  `cli/` for "cli") and the files they directly hold, and the object's
  registration site (module roots such as `mod.rs`/`__init__.py`/`BUILD` plus
  the sub-packages declared beside it). Tokens matching more than 40 module
  sites — ordinary task prose like "source", "context", "tests" — and tokens
  whose only module files are test fixtures seed nothing, so the boost cannot
  re-introduce the flood it exists to cut through. Seeds are additive and
  ranked below every literal anchor, so the change adds reach at the cost of
  the lowest-ranked traversal context and never at the cost of an anchor.
  Measured on the committed Wave 11 fixtures (one per pinned repository, real
  prompt and required paths, reduced real neighbourhoods and real edges): mean
  required-path recall 2.8% → 61.8% on the same graphs, against a reported
  bake-off baseline of 19.3% on the full corpora; the strongest case (Deno,
  5/8) improves instead of regressing. The full-corpus re-measurement is
  GAP-118.

### Changed

- **Builds are no longer pinned to a single compile job** (PERF-012).
  `.cargo/config.toml` shipped `[build] jobs = 1` for every checkout, so a
  cold `cargo build --release -p hilo-cli` — the documented install path —
  compiled the whole dependency graph (`duckdb-sys`, `arrow`, `aws-sdk`) one
  crate at a time on machines that have 8–16 cores. The pin is gone, so cargo
  uses its default (one job per available core). The low-memory guidance it
  carried is unchanged in substance but is now an explicit opt-in for the
  hosts that need it: `CARGO_BUILD_JOBS=1 cargo build --release` on a ≤4GB
  box, documented in README → *Low-memory builders (≤4GB RAM)*.

## [0.4.0] — 2026-10-04

### Fixed

- **Manifest `plugins:` blocks are now loud about being NOT-IMPLEMENTED**
  (DF-WARPFS-59): the block was parsed but no code path ever connected a
  declared hook to the trigger engine or the FUSE daemon — a user declaring
  the spec's `sql-scanner` plugin got silent nothing. Loading a manifest
  that declares plugins now prints a warning naming each plugin and hook
  (both `hilo mount`'s subset parser and every `Manifest::parse` load),
  `hilo init` no longer writes a `plugins:` block, and the spec/docs/README
  plugin sections carry NOT-IMPLEMENTED notices. Existing manifests that
  declare plugins keep parsing (removal under `deny_unknown_fields` would
  hard-reject them); the block round-trips through serialize unchanged.

- **Java symbol packs no longer carry decorator noise** (DF-WARPFS-35): a Java
  symbol name was the last whitespace token of the declaration's first line.
  An annotation is part of a declaration's `modifiers`, so that first line IS
  the annotation (`@Override`, `@SuppressWarnings({`), and a header that wraps
  after a parameter — or ends in `;` — leaves a parameter behind: the dogfood
  run read `@Override`, `context)`, `object);` straight out of the pack. On the
  pinned Gson corpus (264 files) the pre-fix extractor produced 3757 unusable
  names out of 3838 symbols (98%: `""` for every declaration whose first line
  ends in `{`, `@Override`/`@Test` for every annotated one, `object);`/`f);`
  for wrapped headers). Names now come from the AST `name` field, guarded to be
  identifiers (a node without a name field yields no symbol), and the signature
  is the declaration's own header with annotations stripped and wraps joined —
  reported on the line that carries the name, not the annotation's line.
- **`graph understand` now surfaces the named symbols of a flow** (GAP-098):
  the `MAP`/`SIGNATURES` symbol list was a flat `take(8)` in source order, so
  the definition a question was about could not appear once a file had more
  than 8 of them — flask's `wsgi_app`/`full_dispatch_request`/
  `finalize_request` (app.py, 40th of 41) and gin's `ServeHTTP`/
  `handleHTTPRequest` (gin.go, 43rd/45th of 50) were unreachable no matter how
  exactly they were named. A file whose definitions name the task now lists
  them in source order up to 24 and appends any task-named definition past
  that; a file that does not name the task keeps the historical 8, byte for
  byte. Measured on the pinned bake-off corpora (flask/gin/express/vitest/bat/
  duckbrain flow questions): flask 1/4→4/4, gin 1/4→4/4, duckbrain 0/4→4/4,
  with express/vitest/bat unchanged at 3/3, 3/3, 4/4.
- **`graph understand` resolves `local:` import specifiers** (GAP-098, the
  GAP-072/079 family): `local:../../duckdb/connection` is an import *string*,
  and as a row it both answered nothing (`(no symbols extracted)`) and consumed
  one of the `max_nodes` slots a real file needed — on duckbrain 37 of 60 rows
  were specifier ghosts, which is why the duckdb/storage layers the question
  asked about never appeared at all. Specifiers are now resolved against the
  importing file's own directory (extension/`index` probing) to the file the
  graph stores, and a specifier naming nothing in the graph is dropped.
- **`--type git` / `--type local` backend mounts removed** (DF-WARPFS-19):
  they printed success while cloning into `~/.hilo/worktrees`, persisted
  nowhere `hilo backend list`/`sync` reads. Every unsupported type now fails
  with `unknown backend type` (exit 2, nothing written).
- **`hilo plugin load` validates the wasm header** (DF-WARPFS-22): a text
  file named `*.wasm` is rejected naming the missing `\0asm` magic instead of
  printing fabricated hooks; loaded modules persist to `.vfs/plugins/` and
  report honest `0 hooks, 0 edge types`; `plugin list` derives metadata the
  same honest way.

### Changed

- **`hilo --version` now includes build provenance** (git describe + UTC
  build time, baked by `hilo-cli/build.rs`). A rebuilt binary is no longer
  indistinguishable from an 11-day-old one.
- Workspace version bumped from `0.3.1-dev` to `0.4.0` for the release cut.

## [0.3.0] — 2026-09-21

### Added

- **`hilo ignore check <PATH>`** — diagnostic for ignore decisions: prints
  whether a path is excluded by the workspace `.hiloignore` file, the exact
  rule line that decided it, and the ignore file used. Backs `hilo workspace
  sync`'s ignore-aware transfer; see docs/ignore-file.md and
  docs/backend-compatibility-matrix.md.
- **`docs/backend-compatibility-matrix.md`** — feature & compatibility
  matrix for backend-backed workspaces (backend × sync tool × capabilities ×
  modes), per specs/backend-backed-workspace-spec.md §14.
- **`hilo workspace sync`** — two-way sync between a local directory and an
  S3 prefix (`--bucket`, `--prefix`, `--at`, `--ignore`, `--dry-run`).
  Non-ignored files are mirrored in both directions (newer side wins);
  files matched by the git-ignore-style `.hiloignore` file stay local-only
  and are never transferred (the ignore file itself included). `.vfs/`
  metadata is never transferred either way. See docs/ignore-file.md for
  the shared ignore format (S3 today; Google Drive / OneDrive / Dropbox
  backends reuse it).

- **`hilo graph clean`** — purge the cached dependency graph (edges.jsonl,
  graph.db, `.last_warm`) so the next `hilo graph warm` rebuilds from scratch;
  stale edges from renamed/moved files no longer accumulate
- **`hilo mount --daemon`** — background FUSE mounting: the CLI re-execs and
  detaches, returning immediately; unmount with `fusermount -u`
- **Git and local backend mount types** — `hilo backend mount --type git`
  (clone/pull a repository) and `--type local` (disk passthrough) are now
  wired end-to-end

### Changed

- **Tracked inventory policy documented (INV-001)** — docs/inventory-policy.md
  is now the contract for the tracked `.vfs/graph/edges.jsonl`: a warm with a
  parse-cache miss appends newly discovered, deduplicated edges to it
  (intentional refresh — commit the diff with your change), a fully-cached
  warm leaves it untouched, `graph.db` / `.parse_cache.json` / `.last_warm` /
  `.last_reconcile` are gitignored cache artifacts, and `graph clean` +
  re-warm is the operator recipe for a deliberate full inventory refresh.
  README (hooks table + quickstart note) and docs/performance.md point at
  it; the append-only/idempotency contract is pinned by a `hilo-cli` warm
  regression test.

- **Actionable git-backend mount errors** — bad repository URLs now produce a
  clear error message and a non-zero exit instead of a silent failure
- **MCP manifest prerequisite** — the no-manifest error now says
  "Run `hilo init` in the project directory first", and the MCP sections of
  the README and getting-started guide state the `hilo init` prerequisite
- **Backend help text** — `hilo --help` backend description lists only the
  supported types (S3, git, local); the non-existent `remote` type was dropped
- **Doc surface alignment (GAP-019..029)** — mount foreground note, SUPPORT
  repo URL, command-family docs, crate structure lists, MCP tool tables, and
  graph/classify flag lists aligned with the real CLI surface

## [0.2.0] — 2026-07-16

### Added

- **Provenance tracking** — every graph edge tagged with `provenance`
  (AstExact, AstInferred, Heuristic, Lexical, Latent, Unresolved) and
  `confidence` (0.0–1.0). DuckDB schema auto-migrates from v0.1 format.
  `vfs_graph_related` returns provenance + confidence per edge.
- **Signal engine** — harmonic multi-resolution context compression
  (MAP → SIGNATURES → DETAIL), 60% smaller than raw dump, position-ordered.
  New MCP tool `vfs_graph_understand`.
- **Semantic code search** — pure-Rust TF-IDF + Okapi BM25 + Reciprocal
  Rank Fusion. Zero API calls, zero embeddings, fully deterministic.
  New MCP tool `vfs_graph_search`.
- **Determinism tests** — 14 tests proving byte-identical graph/signal/search
  output across runs. Immutable fixture corpus with 6 files (Go, Python, TS).
- **Language expansion** — from 9 to 26 languages: C#, Kotlin, PHP, Swift,
  Elixir, Haskell, Erlang, Scala, Zig, Lua, Dart, Clojure, OCaml, R, Julia,
  Elm, Nim. Tiers 1–3.
- **MCP server** — expanded from 8 to 15 tools: `vfs_graph_understand`,
  `vfs_graph_search`, `vfs_set_metadata`, `vfs_graph_module`,
  `vfs_graph_untested`, `vfs_backend_status`, `vfs_sync_backend`.
- **JIT/lazy query architecture** — `hilo graph related` works on fresh
  `hilo init` without pre-warming. Files auto-parse on first access.
- **GitHub Pages** — documentation site at `https://gethilo.github.io/hilo/`
  with landing page + 5 doc pages.

### Changed

- `hilo graph warm` is the canonical batch graph build command (the former `discover` subcommand is kept as a hidden alias for backward compatibility)
- Edge struct extended with provenance + confidence fields
- DuckDB unique index includes provenance column
- Signal engine integrated into semantic search anchor discovery

## [0.1.0] — 2026-06-24

### Added

- **26-language AST parsing** — Go, Python, TypeScript, Rust, JavaScript,
  Java, C, C++, Ruby, C#, Kotlin, PHP, Swift, Elixir, Haskell, Erlang,
  Scala, Zig, Lua, Dart, Clojure, OCaml, R, Julia, Elm, Nim via tree-sitter
- **Auto-classification** — `hilo classify` detects entrypoints, test
  files, library roles, stability status across all 26 languages
- **Parallel graph discovery** — rayon-powered, progress output every
  100 files
- **Dependency graph** — DuckDB-backed with forward/reverse queries,
  transitive impact analysis (BFS)
- **Cross-language edges** — `tested_by`, `tests`, `imported_by`
- **MCP server** — 8 tools: `vfs_get_metadata`, `vfs_graph_related`,
  `vfs_graph_stats`, `vfs_graph_impact`, `vfs_rule_list`,
  `vfs_rule_check`, `vfs_list_directory`, `vfs_resolve_path`
- **FUSE mount** — kernel-level virtual filesystem with xattr passthrough,
  permission enforcement, read-only mode
- **CLI** — `init`, `meta --set/--read`, `graph discover/stats/related/impact`,
  `classify`, `mount`, `serve --mcp`
- **Storage backends** — S3 (read-only + write-through), remote Git (auto-pull),
  local disk passthrough
- **Bubblewrap sandboxing** — agent process isolation via bwrap
- **Plugin system** — Extism WASM runtime with host functions
- **Permission engine** — glob-based mode bit enforcement
- **inotify triggers** — debounced file-watch re-parsing
- **Virtual directories** — S3 buckets as local paths with auto-upload tracking
- **Workspace mounts** — multi-repo unified FUSE tree

### Infrastructure

- 10 crates, 31 test suites, zero warnings
- GitReins pre-commit: secrets + tests + LSP + static analysis
- Coding Hermes foreman for autonomous development
