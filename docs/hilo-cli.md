# hilo-cli — Command-Line Interface

The `hilo` binary — entrypoint for all Hilo operations. Built with clap.

**Crate:** `hilo-cli`  
**Binary:** `hilo`

## Commands

| Command | Description |
|---------|-------------|
| `hilo graph warm <flags>` | Walk the tree, parse sources, build the graph. Every completed warm stamps `.vfs/graph/state.json` with the git HEAD it saw; when a later warm finds HEAD moved (typically after a pull), it warns the graph is out of date and the same warm rebuilds and re-stamps |
| `hilo init [--no-hooks]` | Initialize a Hilo project — creates `.vfs/` and appends a `### HILO` block to `.git/hooks/post-commit` + `post-merge` (existing hook content preserved); `--no-hooks` skips hook installation entirely |
| `hilo meta <path> [--set <attr> --value <value>]` | Read (all) or set xattr metadata on a file — no per-key read/list flags |
| `hilo graph related <path> [--direction reverse] [--relation imports]` | Query forward/reverse dependency edges |
| `hilo graph impact <path>` | Review set — the file, its blast radius, and the linked helpers/tests/build targets |
| `hilo graph stats` | Aggregate graph statistics |
| `hilo graph warm [--language <lang>] [--changed]` | Pre-parse all files into DuckDB cache |
| `hilo graph module <name>` | All edges for a module |
| `hilo graph untested` | Files with no test coverage edges |
| `hilo graph understand <TASK> [--budget <N>]` | Multi-resolution harmonic context output for a natural-language task |
| `hilo graph search <QUERY> [--limit <N>]` | Deterministic semantic code search (TF-IDF + BM25) |
| `hilo graph rule-list` | List all rules defined in the manifest |
| `hilo graph rule-check <NAME>` | Execute a named rule query against the dependency graph |
| `hilo graph clean` | Delete the cached dependency graph (edges.jsonl + DuckDB cache) so the next `graph warm` re-parses from scratch |
| `hilo graph test-classes [--json]` | Test-class taxonomy + per-surface class mix (COV-3) — totals per class and, per surface, the classes that reach it, flagging single-class surfaces as `class_gap` |
| `hilo graph audit [--json] [PATH]` | Symbol-level connection + test audit (COV-5) — per public function, is it reachable from a declared entrypoint and does it carry a test link; the NAMED list bucketed into `unreachable` / `unlinked` / `ok`, each with a count and the rule that produced it |
| `hilo serve --mcp` | Start MCP server (stdio) — `--mcp` required (only implemented server mode); rate limit read from manifest `performance.rate_limit_rps` |
| `hilo backend mount --type s3\|gdrive\|onedrive\|dropbox\|external --bucket <BUCKET> --at <PATH> [--prefix <PREFIX>] [--region <REGION>]` | Mount a virtual backend at a virtual path (see the next row for the backend-backed workspace form) |
| `hilo backend mount --type s3|gdrive|onedrive|dropbox|external ... --at <PATH> [--endpoint <URL>] [--tool <T>] [--mode stream\|mirror] [--poll-secs <N>]` | Mount a backend-backed workspace (spec §9); writes `.vfs/backends/mounts.yaml` |
| `hilo backend sync [--push\|--pull\|--both] [PATH...]` | Sync mounted backends against the workspace (default two-way, ignore-aware) |
| `hilo backend setup [--type s3\|gdrive\|onedrive\|dropbox\|external]` | Detect sync tools/credentials, print next steps (writes nothing) |
| `hilo backend list` | List all mounted backends |
| `hilo mount <mount-point> [--triggers] [--allow-other] [--daemon] [--log-file <PATH>]` | Mount FUSE filesystem (--daemon detaches into a background process; daemon stderr — trigger engine + tracing logs — appends to `<mount-point>/.vfs/logs/mount-daemon.log` by default, or `--log-file`) |
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
