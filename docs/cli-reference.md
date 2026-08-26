# CLI Reference

## `hilo init`

Initialize Hilo in the current directory. Creates `.vfs/` with inventory
files and a default manifest.

```bash
hilo init
```

## `hilo meta`

Read and write extended attributes on files.

```bash
# Read all Hilo xattrs
hilo meta hilo-cli/src/main.rs

# Set a specific attribute
hilo meta --set user.vfs.role --value entrypoint hilo-cli/src/main.rs

# Read (prints all xattrs — no per-key read flag)
hilo meta hilo-cli/src/main.rs
```

## `hilo graph`

### `warm`

Walk the directory tree, parse all source files with tree-sitter, and
build the dependency graph. Writes to `.vfs/graph/edges.jsonl` and
`.vfs/graph/graph.db`.

```bash
hilo graph warm

# With cross-repo workspace edges
hilo graph warm --workspace

# Only parse files of a specific language
hilo graph warm --language rust

# Only parse files changed since the last warm (used by the post-commit hook)
hilo graph warm --changed
```

Supported languages (26): Go, Python, TypeScript, Rust, JavaScript,
Java, C, C++, Ruby, C#, Kotlin, PHP, Swift, Elixir, Haskell, Erlang,
Scala, Zig, Lua, Dart, Clojure, OCaml, R, Julia, Elm, Nim.
Directories skipped: `target/`, `node_modules/`, `vendor/`,
`__pycache__/`, `.venv/`.

### `stats`

Aggregate statistics about the dependency graph.

```bash
hilo graph stats

# Output:
# Total edges: 202 distinct / 292 raw (edges.jsonl)
# Total files: 81
# Most connected: pkg:std
# Edge types:
#   imports: 200
#   tested_by: 1
#   tests: 1
```

### `related`

Find files related to a given path through the dependency graph.

```bash
# Forward: what does this file import?
hilo graph related hilo-cli/src/main.rs

# Filter by relation type
hilo graph related hilo-cli/src/main.rs --relation imports

# Reverse: what imports this file?
hilo graph related hilo-graph/src/lib.rs --direction reverse

# Reverse with relation filter
hilo graph related hilo-graph/tests/fixtures/handler.go --direction reverse --relation tested_by
```

### `impact`

Find all files that depend on a given file, directly or transitively.

```bash
# Direct dependents only
hilo graph impact hilo-graph/src/lib.rs --max-depth 1

# Full transitive closure (default: 10)
hilo graph impact hilo-graph/src/lib.rs --max-depth 10

# JSON output
hilo graph impact hilo-graph/src/lib.rs --format json

# Include external cross-repo edges in the traversal
hilo graph impact hilo-graph/src/lib.rs --external
```

### `understand`

Multi-resolution harmonic context output for a natural-language task.

```bash
hilo graph understand "how does plugin execution get sandboxed"
```

Token budget override (default: 6000):

```bash
hilo graph understand "how does plugin execution get sandboxed" --budget 12000
```

### `search`

Deterministic semantic code search (TF-IDF + BM25).

```bash
# Top 20 matches (default)
hilo graph search "rate limiter"

# Custom result limit
hilo graph search "rate limiter" --limit 50
```

### `module`

Per-module statistics and test coverage.

```bash
hilo graph module hilo-graph/src
```

### `untested`

List source files with no test coverage.

```bash
hilo graph untested
```

### `rule-list`

List all rules defined in the manifest.

```bash
hilo graph rule-list
```

### `rule-check`

Execute a named rule query against the dependency graph. Rules are defined
in the project manifest; this repo currently defines none (see
`hilo graph rule-list`).

```bash
# List rules defined in the manifest
hilo graph rule-list

# Check a named rule (fails with "Rule not found" unless defined in .vfs/manifest.yaml)
hilo graph rule-check <RULE_NAME>
```

### `clean`

Delete the cached dependency graph (`edges.jsonl` + DuckDB cache) so the
next `graph warm` re-parses every source file from scratch. The reset path
for a corrupted or stale graph database.

```bash
hilo graph clean
```

## `hilo classify`

Auto-tag every source file with `user.vfs.role` and `user.vfs.status`
using tree-sitter AST queries. No LLM required.

```bash
# Dry run — show what would be tagged
hilo classify --dry-run

# Apply tags
hilo classify

# Verbose output (per-file)
hilo classify --verbose

# Enable feature inference (sets user.vfs.feature xattrs from directory structure)
hilo classify --features
```

Roles detected: `entrypoint`, `library`, `test`, `script`, `example`,
`config`, `build`, `generated`, `unknown`.

Statuses detected: `stable`, `beta`, `unstable`, `deprecated`, `unknown`.

## `hilo mount`

Mount the current directory as a FUSE filesystem with xattr passthrough.

```bash
mkdir /mnt/vfs
hilo mount /mnt/vfs

# With triggers (auto-reparse on file changes)
hilo mount /mnt/vfs --triggers

# Allow other users to access
hilo mount /mnt/vfs --allow-other

# Run in the background (detached daemon — returns immediately)
hilo mount /mnt/vfs --daemon
```

**Note:** `hilo mount` runs in the foreground and blocks the terminal
until unmounted. Run it in a separate terminal, background it with `&`,
or pass `--daemon` to detach it into a background process that keeps the
mount alive until `fusermount -u /mnt/vfs` unmounts it.

## `hilo serve`

Start the MCP server for agent integration.

```bash
# Stdio transport (for Claude Desktop, Hermes)
hilo serve --mcp
```

## `hilo backend`

Manage virtual backends (S3, git, local).

### `mount`

Mount a virtual backend.

```bash
hilo backend mount --type s3 --bucket my-bucket --prefix data --at /s3

# Explicit region (default: us-east-1)
hilo backend mount --type s3 --bucket my-bucket --at /s3 --region eu-west-1
```

### `list`

List all mounted backends.

```bash
hilo backend list
```

## `hilo workspace`

Manage multi-repo workspace mounts.

### `mount`

Mount all repos and backends from the manifest.

```bash
hilo workspace mount /mnt/hilo
```

### `unmount`

Unmount a workspace.

```bash
hilo workspace unmount /mnt/hilo
```

### `sync`

Two-way sync a local directory against a remote backend prefix (S3 today).
Non-ignored files are mirrored in both directions (newer side wins); files
matched by the ignore file stay local-only and are never transferred.
See docs/ignore-file.md for the ignore format.

```bash
hilo workspace sync --bucket my-bucket --prefix data --at ./ws --dry-run
```

### `ephemeral`

List ephemeral (rebuildable/redownloadable) files in the workspace — the
built-in catalog covers common build/artifact/cache paths (`target/`,
`node_modules/`, `.venv/`, `*.o`, `.vfs/graph/`, ...); a `.hiloephemeral`
file (same git-ignore-style syntax as `.hiloignore`) adds or (`!`) removes
patterns. Output is TSV: `path<TAB>size<TAB>reason`. PATH arguments limit
the listing to subtrees.

```bash
hilo workspace ephemeral
# target/artifact.bin	64	target/
# node_modules/pkg/index.js	32	node_modules/
```

### `wipe`

Plan or apply a wipe of ephemeral files. Default is a dry-run plan; pass
`--apply` to delete. Only ephemeral files are removed, `user.vfs.ephemeral
= false` is the only wipe protector (set it with `hilo meta --set
ephemeral --value false <file>`), and symlinks are never touched.

```bash
hilo workspace wipe --ephemeral
# would remove	target/artifact.bin
# would free 64 bytes across 1 file(s) (dry-run; pass --apply to delete)
hilo workspace wipe --ephemeral --apply
# removed	target/artifact.bin
# freed 64 bytes across 1 file(s)
```

## `hilo ignore`

Inspect ignore decisions (the git-ignore-style `.hiloignore` file, with
`.vfsignore` accepted as a legacy alias).

### `check`

Report whether a path would be ignored, and which rule decided it.

```bash
hilo ignore check build/out.o
# path: build/out.o
# ignored: true
# rule: build/
# source: /path/to/workspace/.hiloignore
```

## `hilo plugin`

Load and manage wasm plugins.

### `load`

Load a .wasm plugin and register it in the runtime.

```bash
hilo plugin load ./my-plugin.wasm
```

### `list`

List plugins discovered in `.vfs/plugins/`.

```bash
hilo plugin list
```
