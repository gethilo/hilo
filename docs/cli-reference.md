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
hilo meta src/main.rs

# Set a specific attribute
hilo meta --set src/main.rs user.vfs.role entrypoint

# Read a specific attribute
hilo meta --read src/main.rs user.vfs.role
```

## `hilo graph`

### `discover`

Walk the directory tree, parse all source files with tree-sitter, and
build the dependency graph. Writes to `.vfs/graph/edges.jsonl` and
`.vfs/graph/graph.db`.

```bash
hilo graph discover

# With cross-repo workspace edges
hilo graph discover --workspace
```

Supported languages: Go, Python, TypeScript, Rust, JavaScript, Java,
C, C++, Ruby. Directories skipped: `target/`, `node_modules/`, `vendor/`,
`__pycache__/`, `.venv/`.

### `stats`

Aggregate statistics about the dependency graph.

```bash
hilo graph stats

# Output:
# Total edges: 2252
# Unique source files: 716
# Unique dependencies: 531
# Top dependencies:
#   sys:gtest/gtest.h: 349
#   sys:metacall/metacall.h: 175
```

### `related`

Find files related to a given path through the dependency graph.

```bash
# Forward: what does this file import?
hilo graph related src/main.rs

# Filter by relation type
hilo graph related src/main.rs --relation imports

# Reverse: what imports this file?
hilo graph related sys:some-header.h --direction reverse

# Reverse with relation filter
hilo graph related src/login.go --direction reverse --relation tested_by
```

### `impact`

Find all files that depend on a given file, directly or transitively.

```bash
# Direct dependents only
hilo graph impact sys:metacall/metacall.h --max-depth 1

# Full transitive closure (up to 5 by default)
hilo graph impact sys:gtest/gtest.h --max-depth 5

# JSON output
hilo graph impact sys:metacall/metacall.h --format json
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
```

Roles detected: `entrypoint`, `library`, `test`, `script`, `example`,
`config`, `unknown`.

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
```

## `hilo serve`

Start the MCP server for agent integration.

```bash
# Stdio transport (for Claude Desktop, Hermes)
hilo serve --mcp
```
