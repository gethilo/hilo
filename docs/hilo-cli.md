# hilo-cli — Command-Line Interface

The `hilo` binary — entrypoint for all Hilo operations. Built with clap.

**Crate:** `hilo-cli`  
**Binary:** `hilo`

## Commands

| Command | Description |
|---------|-------------|
| `hilo graph warm <flags>` | Walk the tree, parse sources, build the graph. Every warm also prunes stale rows — deleted files and imports the re-parse no longer emits (DF-WARPFS-170) — from `edges.jsonl` and the DuckDB cache, so a removed dependency stops being reported as a dependent. Every completed warm stamps `.vfs/graph/state.json` with the git HEAD it saw; when a later warm finds HEAD moved (typically after a pull), it warns the graph is out of date and the same warm rebuilds and re-stamps |
| `hilo init [--no-hooks]` | Initialize a Hilo project — creates `.vfs/` and appends a `### HILO` block to `.git/hooks/post-commit` + `post-merge` (existing hook content preserved); `--no-hooks` skips hook installation entirely |
| `hilo meta <path> [--set <attr> --value <value>]` | Read (all) or set xattr metadata on a file — no per-key read/list flags |
| `hilo graph related <path> [--direction reverse] [--relation imports]` | Query forward/reverse dependency edges |
| `hilo graph impact <path>` | Review set — the file, its blast radius, and the linked helpers/tests/build targets |
| `hilo graph stats` | Aggregate graph statistics |
| `hilo graph warm [--language <lang>] [--changed]` | Pre-parse all files into DuckDB cache |
| `hilo graph module <name>` | All edges for a module |
| `hilo graph untested [--granularity file\|function] [--json]` | Files with no test coverage edges (`file`, the default) or public FUNCTIONS with no test reference, named individually (`function`, GAP-121) |
| `hilo graph reachable --from <entrypoint> [--json]` | Public functions with no static call path from the named entrypoint — the SYMBOL answer file reachability cannot give (GAP-121) |
| `hilo graph understand <TASK> [--budget <N>]` | Multi-resolution harmonic context output for a natural-language task |
| `hilo graph search <QUERY> [--limit <N>]` | Deterministic semantic code search (TF-IDF + BM25) |
| `hilo graph rule-list` | List all rules defined in the manifest |
| `hilo graph rule-check <NAME>` | Execute a named rule query against the dependency graph |
| `hilo graph clean` | Delete the cached dependency graph (edges.jsonl + DuckDB cache) so the next `graph warm` re-parses from scratch |
| `hilo graph gc` | Remove exact-duplicate rows from `.vfs/graph/edges.jsonl` (REV-WARPFS-8) — identity is `(from, to, rel, provenance)`, the same one dedup-on-append uses, and the FIRST occurrence of each is kept in row order; the rewrite is atomic (temp file + rename) under the shared edges lock, so it is safe against a running `hilo mount --triggers` append or another `graph warm`. Idempotent, takes no flags. Run `hilo graph warm` afterwards so the DuckDB cache (no unique constraint) is rebuilt from the compacted file |
| `hilo graph surfaces [--json] [--kind <KIND>]` | Enumerate the codebase's externally-visible contract surfaces — CLI verbs/flags, MCP tools, FFI exports, FUSE ops, config keys, and public API items — into `.vfs/graph/surfaces.jsonl` (COV-1); `--json` prints the inventory in its locked shape, `--kind` restricts enumeration to one kind (`cli_verb`, `cli_flag`, `mcp_tool`, `ffi_export`, `fuse_op`, `config_key`, `public_api_item`); without `--kind` all kinds are enumerated |
| `hilo graph coverage-links [--json] [--surface <SURFACE>] [--unlinked]` | Derive evidenced test→surface coverage links (COV-2) — one row per (test_file, target) with evidence kind, confidence, and direction, written to `.vfs/graph/coverage_links.jsonl`; `--surface` restricts output to one surface (by surface_id or name) and answers "what tests cover this surface", `--unlinked` shows only surfaces with no link at all |
| `hilo graph test-classes [--json]` | Test-class taxonomy + per-surface class mix (COV-3) — totals per class and, per surface, the classes that reach it, flagging single-class surfaces as `class_gap` |
| `hilo graph rollup [--by <DIMENSION>] [--json] [--group <NAME>]` | Group surfaces into features/components and roll coverage + class mix up to the group level (COV-4) — `--by` picks the grouping dimension (`feature`, the default: explicit `user.vfs.feature`/`user.vfs.component` xattr, then the crate boundary, then the module path prefix; or `crate`; or `module`), and the chosen source is recorded on each surface's row so an annotated group is distinguishable from a structural fallback; `--group` answers for one group with its own gap list (uncovered surfaces + class-gap surfaces), an unknown name being a loud error. Requires `.vfs/graph/surfaces.jsonl` (`graph surfaces`), reads `.vfs/graph/coverage_links.jsonl` for the counts and class mix, and names that absence rather than reporting an empty report |
| `hilo graph wiring [PATH] [--json] [--baseline <PATH>] [--write-baseline <PATH>]` | Wiring report (GAP-112/GAP-113) — detects silent-fallback wiring (interfaces consumed from non-test code whose only satisfiers are test-role types, the TRBL-084 shape) as `pass`/`finding`/`unsupported`, and emits the aggregate per-module report: public surfaces, inbound/outbound connection counts, deltas against a stored baseline, untested surfaces, and a module classification (entrypoint/service/lib/test/dead); `PATH` is an optional project root (defaults to the current directory), `--baseline` compares against a stored baseline file, `--write-baseline` writes the current module snapshots for a later comparison, and `--json` emits the versioned `hilo.graph.wiring/2` schema |
| `hilo graph audit [--json] [PATH]` | Symbol-level connection + test audit (COV-5) — per public function, is it reachable from a declared entrypoint and does it carry a test link; the NAMED list bucketed into `unreachable` / `unlinked` / `ok`, each with a count and the rule that produced it |
| `hilo serve --mcp` | Start MCP server (stdio) — `--mcp` required (only implemented server mode); rate limit read from manifest `performance.rate_limit_rps` |
| `hilo backend mount --type s3\|gdrive\|onedrive\|dropbox\|external --bucket <BUCKET> --at <PATH> [--prefix <PREFIX>] [--region <REGION>]` | Mount a virtual backend at a virtual path (see the next row for the backend-backed workspace form) |
| `hilo backend mount --type s3|gdrive|onedrive|dropbox|external ... --at <PATH> [--endpoint <URL>] [--tool <T>] [--mode stream\|mirror] [--poll-secs <N>]` | Mount a backend-backed workspace (spec §9); writes `.vfs/backends/mounts.yaml` |
| `hilo backend sync [--push\|--pull\|--both] [PATH...]` | Sync mounted backends against the workspace (default two-way, ignore-aware) |
| `hilo backend setup [--type s3\|gdrive\|onedrive\|dropbox\|external]` | Detect sync tools/credentials, print next steps (writes nothing) |
| `hilo backend list` | List all mounted backends |
| `hilo mount <mount-point> [--triggers] [--allow-other] [--daemon] [--log-file <PATH>]` | Mount FUSE filesystem (--daemon detaches into a background process; daemon stderr — trigger engine + tracing logs — appends to `$HOME/.vfs/logs/mount-daemon.log` by default, outside the mounted tree (DF-WARPFS-63), or `--log-file`) |
| `hilo sandbox check [SOURCE]` | Verify the manifest bubblewrap sandbox (spec §14.3) end-to-end — builds a `BubblewrapExecutor` from the parsed `sandbox:` block and runs a trivial command through `BubblewrapExecutor::run` (bwrap) |
| `hilo sandbox exec [--source DIR] -- CMD...` | Run a command inside the manifest sandbox (spec §14.1 agent-shell model) — streams stdio through, exits with the child's status; `isolate_network: true` blocks network access |
| `hilo workspace mount [MOUNT_POINT] [--manifest <PATH>] [--allow-other] [--daemon]` | Mount every repo declared in the manifest — accepts both the project manifest `hilo init` writes (`repos[].at`, default `.vfs/manifest.yaml`) and the workspace schema (`repos[]`/`backends[]`/`mounts[]`); omitting MOUNT_POINT uses `interfaces.fuse.mount_point`; `--allow-other` is opt-in for multi-user mounts and a default mount needs no `user_allow_other` in `/etc/fuse.conf` (see [workspace.md](workspace.md)) |
| `hilo workspace unmount <MOUNT_POINT>` | Unmount a workspace |
| `hilo workspace sync --bucket <BUCKET> --at <DIR> [--prefix <PREFIX>] [--ignore <FILE>] [--dry-run]` | Two-way sync a local directory against an S3 prefix — non-ignored files mirrored both ways, ignored files (`.hiloignore`, git-ignore style) stay local-only |
| `hilo workspace ephemeral [PATH...]` | List ephemeral (rebuildable/redownloadable) files in the workspace — TSV output `path<TAB>size<TAB>reason`; PATH arguments limit the listing to subtrees |
| `hilo workspace wipe --ephemeral [--apply]` | Wipe ephemeral files — `--ephemeral` required (only wipe mode); default is a dry-run plan ("would remove ..."), `--apply` deletes and reports freed bytes |
| `hilo ignore check <PATH> [--ignore-file <FILE>] [--no-default-ignores]` | Check whether a path is ignored by the workspace ignore stack, and which rule decided it |
| `hilo classify [--dry-run]` | Auto-classify all files (role/status/feature metadata) |
| `hilo plugin list` | List loaded WASM plugins |
| `hilo plugin load <path>` | Load a plugin |

## Usage Examples

```bash
# Initialize
hilo init

# Query graph
hilo graph related src/main.go
hilo graph impact src/auth/mod.rs
hilo graph warm --language go --language rust
hilo graph stats
hilo graph clean

# Metadata
hilo meta --set user.vfs.feature --value auth-module src/auth.rs
# Read (all xattrs): hilo meta src/auth.rs — no per-key --read/--list flags

# Classify
hilo classify --dry-run
hilo classify

# Backends
hilo backend mount --type s3 --bucket my-bucket --region us-east-1 --at /s3
# S3-compatible endpoint (MinIO); without --endpoint, AWS_ENDPOINT_URL is
# read at sync time and disclosed on every plan line
hilo backend mount --type s3 --bucket my-bucket --at /s3 --endpoint http://minio.internal:9000
hilo backend list

# Workspaces
hilo workspace mount /mnt/hilo
hilo workspace unmount /mnt/hilo
# S3-backed workspace: two-way sync, ignored files stay local-only
# (see docs/ignore-file.md for the .hiloignore format)
hilo workspace sync --bucket my-bucket --prefix my-project --at ./workspace
hilo workspace sync --bucket my-bucket --prefix my-project --at ./workspace --dry-run

# Mount
hilo mount /mnt/hilo --triggers

# Plugins
hilo plugin load ./my-plugin.wasm
hilo plugin list
```

## `graph warm` reconciles removals too

`graph warm` is not append-only any more. Before it decides whether the graph
changed it prunes rows the tree no longer supports: a file endpoint that no
longer exists (deleted or moved) and — on a complete derivation — a row its
source file no longer emits (the import was deleted from a file that is still
there). Removals are mirrored into the DuckDB cache, so `graph impact`,
`graph related`, `graph stats`, and `graph untested` stop reporting a
dependency you removed. One line reports what went:

```
$ hilo graph warm
Pruned 2 stale edge(s) from edges.jsonl: 1 missing path, 1 superseded (388 rows -> 386)
  cache prune: 2 stale row(s) removed from graph.db
```

`--changed` and `--language` warms still prune deleted files, but do not apply
the "superseded row" rule: they derive only the files they re-parse, where a
row missing from the derived set cannot be told apart from a file that was not
looked at. The pass is idempotent — see
[inventory-policy.md](inventory-policy.md) for the full contract.
