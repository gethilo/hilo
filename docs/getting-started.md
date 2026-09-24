# Getting Started

## Install

```bash
git clone https://github.com/gethilo/hilo.git
cd hilo
cargo build --release
cp target/release/hilo ~/.cargo/bin/hilo   # put hilo on PATH
hilo --help
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

> On non-root / restricted hosts (no sudo, no `pkg-config`/`libssl-dev`),
> build with `cargo install --path hilo-cli --features vendored-openssl` —
> see the README's "Non-root / restricted hosts" section.

> ⚠️ **Build time:** the first build compiles `duckdb-sys`/`arrow` from
> source — expect 15-20 min and a C/C++ toolchain (`g++`, e.g. from
> `build-essential`). `clang` and `CMake` are **not** required — see the
> README's build-time note (verified on a bare Debian 13 fresh install,
> 2026-09-20). Subsequent builds are incremental and fast.

## First Run

```bash
# 1. Initialize Hilo in your project
cd my-project
hilo init

# 2. Build the dependency graph
hilo graph warm

# 3. Auto-classify every file
hilo classify

# 4. Explore
hilo graph stats
hilo graph impact sys:some-header.h --max-depth 3
hilo graph related src/main.rs --relation imports
```

## Using with AI Agents

### Via MCP (Claude Desktop, Hermes, Continue)

The MCP server needs an initialized project — run `hilo init` in the
project directory first (it creates `manifest.yaml` / `.vfs/manifest.yaml`).

```bash
hilo serve --mcp
```

Add to your MCP client configuration:

```json
{
  "mcpServers": {
    "hilo": {
      "command": "/path/to/hilo",
      "args": ["serve", "--mcp"],
      "cwd": "/path/to/your/project"
    }
  }
}
```

### Via FUSE Mount

```bash
mkdir /mnt/vfs
# NOTE: hilo mount runs in the FOREGROUND and blocks this terminal until
# unmounted (Ctrl-C to stop). Run it in a separate terminal, with `&`, or
# use `hilo mount /mnt/vfs --daemon` to detach it into a background process.
hilo mount /mnt/vfs

# Standard tools work through the mount
ls /mnt/vfs/
cat /mnt/vfs/src/main.rs
getfattr -n user.vfs.role /mnt/vfs/src/main.rs
```

### More commands

Hilo also ships three more command families: `hilo backend` (virtual
S3/git/local backends), `hilo workspace` (multi-repo mounts) and
`hilo plugin` (WASM plugin runtime). See `hilo backend --help`,
`hilo workspace --help` and `hilo plugin --help` for usage.
