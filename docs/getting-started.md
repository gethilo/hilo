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

## Clone & rebuild (fresh clone of an already-initialized project)

Hilo's inventory is split between what git carries and what is rebuilt on
demand. If you clone a repository that is already Hilo-initialized (like
hilo itself), `.vfs/manifest.yaml` and `.vfs/graph/edges.jsonl` arrive with
the clone — but **`.vfs/graph/graph.db` does not**: it is a rebuildable
DuckDB query cache and is gitignored (the managed block `hilo init` writes
to `.gitignore`). That is by design, not data loss. What you have and what
you need to regenerate:

| Path | After `git clone` | What it is |
|------|-------------------|------------|
| `.vfs/manifest.yaml` | present (tracked) | project inventory truth |
| `.vfs/graph/edges.jsonl` | present (tracked) | append-only edge inventory truth |
| `.vfs/graph/graph.db` | **absent** (gitignored) | DuckDB query cache, rebuilt from `edges.jsonl` |

### The rebuild commands

From the clone root:

```bash
hilo init        # see note below — safe on a clone, but usually unnecessary
hilo graph warm  # parse all sources, write .vfs/graph/graph.db + .last_warm
hilo graph stats # verify: prints edge/file counts from the rebuilt cache
```

Expected `graph warm` output (hilo's own repo, v0.3.x): progress lines
(`parsing 111/111 files...`), then:

```
Discovered 1167 edges across 104 files (4 languages)
Coverage: 111 files = 104 contribute edges + 1 package facades (__init__.py) + 6 no imports + 0 unreadable + 0 unsupported extension
```

`graph warm` takes its time on a cold tree (roughly a minute for hilo-sized
repos; it also writes `.vfs/graph/.last_warm`, the cutoff future incremental
warms use). `graph stats` then reports e.g. `Total edges: 930 distinct / 555
raw (edges.jsonl)` — the distinct-vs-raw delta is expected (DuckDB dedupes
multi-provenance edges; see [inventory policy](inventory-policy.md)).

**`hilo init` on a fresh clone is a no-op for hilo itself** — a manifest
already exists and `init` never overwrites one. It only earns its keep when
you clone a repo that is *not* yet Hilo-initialized, or want its side
effects in a repo where the manifest was deleted: it creates `.vfs/` +
`manifest.yaml`, appends the rebuildable-cache block to `.gitignore`, and
installs the git hooks below (hooks are installed only when a fresh
manifest is created).

### Is `warm` actually required?

Strictly, no — queries self-heal. `hilo graph related|impact|stats` rebuild
the missing `graph.db` from the tracked `edges.jsonl` on first open
(read-through reconcile), so a cold clone answers queries after a short
rebuild pause. Running `hilo graph warm` up front is still the recommended
first action after a clone: it parses every source file eagerly (catching
drift between `edges.jsonl` and the working tree in one pass) instead of
paying a rebuild on each first query, and it refreshes `.last_warm` so the
post-commit hook's `--changed` incremental mode has a cutoff to work from.

### Why the hooks won't have done it for you

`hilo init` installs `post-commit` / `post-merge` hooks into the *clone
target's* `.git/hooks/` — never into the repo you cloned from, and git does
not transport hooks. The post-commit hook only runs `hilo graph warm
--changed` (incremental since the last warm marker), and the post-merge hook
runs a full warm only when a `.vfs/.dirty` marker exists — which nothing in
a fresh clone creates. So on a brand-new clone neither hook fires a full
warm: run `hilo graph warm` manually once, exactly as above. (Hook
mechanics: [README → Git hooks](../README.md#git-hooks-installed-by-hilo-init).)

### Fresh clone of a project you initialize yourself

```bash
git clone https://github.com/you/your-project.git
cd your-project
hilo init          # creates .vfs/manifest.yaml + .gitignore block + hooks
hilo graph warm    # first full parse; writes edges.jsonl + graph.db
git add .vfs/manifest.yaml .vfs/graph/edges.jsonl
git commit -m "chore: initialize Hilo inventory"
git push           # collaborators' clones now carry the inventory
```

Commit `manifest.yaml` and `edges.jsonl` (inventory truth — collaborators'
clones get them via git), never `graph.db` (rebuilt locally; `hilo init`
has already gitignored it for you). The full contract:
[inventory policy](inventory-policy.md).

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
