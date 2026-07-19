# Getting Started

## Install

```bash
git clone https://github.com/gethilo/hilo.git
cd hilo
cargo build --release
./target/release/hilo-cli --help
```

### Requirements

- Rust 1.80+
- `libfuse3-dev` (for FUSE mount)
- `attr` package (for `getfattr` / `setfattr`)

```bash
# Ubuntu/Debian
sudo apt install libfuse3-dev attr

# macOS (FUSE not supported; CLI + MCP still work)
# No additional deps needed for CLI-only use
```

## First Run

```bash
# 1. Initialize Hilo in your project
cd my-project
hilo init

# 2. Build the dependency graph
hilo graph discover

# 3. Auto-classify every file
hilo classify

# 4. Explore
hilo graph stats
hilo graph impact sys:some-header.h --max-depth 3
hilo graph related src/main.rs --relation imports
```

## Using with AI Agents

### Via MCP (Claude Desktop, Hermes, Continue)

```bash
hilo serve --mcp
```

Add to your MCP client configuration:

```json
{
  "mcpServers": {
    "hilo": {
      "command": "/path/to/hilo-cli",
      "args": ["serve", "--mcp"],
      "cwd": "/path/to/your/project"
    }
  }
}
```

### Via FUSE Mount

```bash
mkdir /mnt/vfs
hilo mount /mnt/vfs

# Standard tools work through the mount
ls /mnt/vfs/
cat /mnt/vfs/src/main.rs
getfattr -n user.vfs.role /mnt/vfs/src/main.rs
```
