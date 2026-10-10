---
name: hilo-usage
description: How to install and use Hilo (warpfs) — the agent-first metadata filesystem CLI. Use when a fresh machine needs Hilo installed, or when driving hilo meta/graph/classify/mount.
---

# Hilo (warpfs) usage — for agents landing in this repo

## What it is
Rust CLI + FUSE filesystem that attaches `user.vfs.*` xattr metadata and a
dependency graph (26-language AST parse) to a codebase without touching file
content. Truth = `.vfs/manifest.yaml` + `.vfs/graph/edges.jsonl`; DuckDB is a
rebuildable cache.

## Fresh-machine install (documented no-sudo path, proven 2026-10-10)
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- --profile minimal
source "$HOME/.cargo/env"
git clone https://github.com/gethilo/hilo.git && cd hilo
cargo build --release -p hilo-cli --no-default-features --features vendored-openssl
cp target/release/hilo ~/.cargo/bin/hilo
```
- Cold build ≈ 26 min (DuckDB + OpenSSL from source). No pkg-config / libfuse3-dev / sudo needed.
- Binary has NO libfuse3 link (pure-Rust fuser path); `fusermount3` (Debian pkg `fuse3`) is present-by-default on Debian 13.

## Pitfalls (proven, do not relearn)
- `hilo meta` syntax is `hilo meta --set KEY --value VAL <path>` (get = `hilo meta <path>`). There is no `meta set <path> k=v` form — that errors.
- `hilo mount <dir>` fails with `No such file or directory` AFTER printing "Hilo mounted at <dir>" when <dir> does not exist. mkdir the mount point first (finding DF-WARPFS-115).
- `hilo graph search add` can return "No results found" for a real symbol on a tiny corpus; search matches package/file names (lexical) — warm the graph first (DF-WARPFS-116).
- `hilo init` outside a git repo skips hook install with a warning — expected, not an error.
- On shared build hosts, never write build logs to /tmp — collide with sibling tenants; use $HOME.
- Local builds on the control host go to `CARGO_TARGET_DIR=/var/tmp/hermes-cargo-target`; a `target/debug/hilo` inside the repo may be a stale artifact of an old invocation (bit runs 30-33). Check `hilo --version` build stamp.

## Quick verify a build works
```bash
hilo --version                      # expect a version + git stamp
mkdir -p ~/smoke/src && printf 'def add(a,b):\n    return a+b\n' > ~/smoke/src/math.py
printf 'from src.math import add\n' > ~/smoke/src/main.py && cd ~/smoke
hilo init && hilo graph warm && hilo graph stats && hilo classify
mkdir ~/mnt && hilo mount ~/mnt &   # then ls ~/mnt/src and cat files through it
```

## Full run records
`docs/dogfood/` — integration reports per dogfood run, plus `diagnostics.md`.
