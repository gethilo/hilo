# hilo-cli — Command-Line Interface

The `hilo` binary — entrypoint for all Hilo operations. Built with clap.

**Crate:** `hilo-cli`  
**Binary:** `hilo`

## Commands

| Command | Description |
|---------|-------------|
| `hilo init` | Initialize a Hilo project — creates `.vfs/`, installs git hooks |
| `hilo meta <path> [--set key=value] [--read key] [--list]` | Read/write/list xattr metadata on files |
| `hilo graph related <path> [--direction reverse] [--relation imports]` | Query forward/reverse dependency edges |
| `hilo graph impact <path>` | Transitive blast-radius analysis |
| `hilo graph stats` | Aggregate graph statistics |
| `hilo graph warm [--language <lang>] [--changed]` | Pre-parse all files into DuckDB cache |
| `hilo graph module <name>` | All edges for a module |
| `hilo graph untested` | Files with no test coverage edges |
| `hilo serve [--mcp] [--rate-limit <rps>]` | Start MCP server (stdio) |
| `hilo backend add <id> --type s3 --bucket ... --region ...` | Add a storage backend |
| `hilo backend list` | List configured backends |
| `hilo backend remove <id>` | Remove a backend |
| `hilo backend sync <id>` | Sync a backend |
| `hilo mount <mount-point> [--triggers] [--allow-other]` | Mount FUSE filesystem |
| `hilo workspace init` | Initialize multi-repo workspace |
| `hilo workspace mount` | Mount all workspace repos |
| `hilo workspace list` | List workspace repos |
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

# Metadata
hilo meta --set user.vfs.feature auth-module src/auth.rs
hilo meta --read user.vfs.feature src/auth.rs
hilo meta --list src/auth.rs

# Classify
hilo classify --dry-run
hilo classify

# Backends
hilo backend add my-s3 --type s3 --bucket my-bucket --region us-east-1
hilo backend sync my-s3

# Mount
hilo mount /mnt/hilo --triggers

# Plugins
hilo plugin load ./my-plugin.wasm
hilo plugin list
```
