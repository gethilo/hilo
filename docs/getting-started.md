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

- Rust 1.80+ — `cargo` is required by every install path (no prebuilt
  binaries are published yet)
- `libfuse3-dev` and `pkg-config` — needed to **build** (the FUSE bindings);
  a prebuilt binary needs only the `libfuse3-4` runtime library
- `attr` package — *optional*, only for the `getfattr` / `setfattr`
  inspection commands (Hilo itself uses xattr syscalls)

```bash
# Ubuntu/Debian
sudo apt install build-essential pkg-config libssl-dev libfuse3-dev attr

# macOS (FUSE not supported; CLI + MCP still work)
# No additional deps needed for CLI-only use
```

### No sudo? Install without root

A bare image or sandbox with no `sudo` can still build the CLI — nothing in
this path needs privileges:

```bash
# 1. Rust into ~/.cargo (no root)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- --profile minimal
source "$HOME/.cargo/env"

# 2. FUSE 3 dev files under $HOME (apt-get download needs no privileges)
mkdir -p "$HOME/.local/hilo-deps" && cd "$HOME/.local/hilo-deps"
apt-get download libfuse3-dev libfuse3-4 pkgconf pkgconf-bin
for d in *.deb; do dpkg-deb -x "$d" root/; done
sed -i "s|^prefix=/usr|prefix=$PWD/root/usr|" root/usr/lib/x86_64-linux-gnu/pkgconfig/fuse3.pc

# 3. Build: vendored OpenSSL + the unpacked FUSE dev files
git clone https://github.com/gethilo/hilo.git && cd hilo
env PKG_CONFIG="$HOME/.local/hilo-deps/root/usr/bin/pkg-config" \
    PKG_CONFIG_PATH="$HOME/.local/hilo-deps/root/usr/lib/x86_64-linux-gnu/pkgconfig" \
    cargo build --release -p hilo-cli --features vendored-openssl
cp target/release/hilo ~/.cargo/bin/hilo
```

`--features vendored-openssl` removes the OpenSSL / `libssl-dev` requirement,
and unpacking the two FUSE `.deb`s removes the `pkg-config` / `libfuse3-dev`
one — the vendored feature alone is **not** enough, because `fuser`'s build
script probes `pkg-config` for `fuse3.pc`. Full rationale, caveats and the
Docker alternative: README →
[Source build without sudo](../README.md#source-build-without-sudo).

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
