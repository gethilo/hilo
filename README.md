# Hilo

**An agent-first virtual filesystem.** Give your AI coding agent a
pre-built map of every codebase it touches — dependencies, entrypoints,
test coverage, blast radius — without burning context window on file reads.

[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.80%2B-orange.svg)](https://rust-lang.org)

## Install

```bash
git clone https://github.com/totalwindupflightsystems/hilo.git
cd hilo && cargo build --release
./target/release/hilo-cli --help
```

Requirements: Rust 1.80+, `libfuse3-dev` (for FUSE mount), `attr` (for xattrs).

## The Problem

Your AI agent reads files to answer questions about code. "What depends on
this header?" → read 50 files. "Is this function tested?" → grep for
`_test` patterns. "What would break if I change this?" → read the whole
damn repo.

Every file read burns tokens. Context windows fill up. The agent forgets
what it was doing halfway through. And the next time it asks the same
question, it reads the same files again.

## What Hilo Does

Hilo pre-computes a dependency graph and enriches every file with
metadata — **before** the agent asks. The agent queries the graph through
standard tools (`getfattr`, `ls`, `cat`) or an MCP server. Zero file reads
for structural questions.

```
$ hilo init              # Create .vfs/ metadata directory
$ hilo graph warm        # Parse AST, build graph (26 languages)
  parsing 100/817 files...
  parsing 817/817 files...
  Discovered 2315 edges across 716 files (26 languages)

$ hilo graph impact 'sys:gtest/gtest.h' --max-depth 5
  349 files impacted — 347 C++, 2 headers

$ hilo classify          # Auto-tag every file with role + stability
  1064 files: 423 library, 405 test, 39 entrypoint
```

## Architecture

```
┌──────────────────────────────────────────┐
│  Agent (Claude / Hermes / Codex)         │
│    │  MCP tools  │  getfattr  │  cat     │
├────┼─────────────┼────────────┼──────────┤
│  Hilo                                    │
│  ┌──────────┐  ┌──────────┐  ┌────────┐  │
│  │  FUSE    │  │   MCP    │  │  CLI   │  │
│  │  mount   │  │  server  │  │  shim  │  │
│  └────┬─────┘  └────┬─────┘  └───┬────┘  │
│       │             │            │       │
│  ┌────┴─────────────┴────────────┴─────┐ │
│  │          Metadata Engine            │ │
│  │  xattrs │ JSONL edges │ DuckDB      │ │
│  └────────────────┬────────────────────┘ │
│                   │                      │
│  ┌────────────────┴────────────────────┐ │
│  │      Backend Storage                │ │
│  │  Git repos │ S3 │ local disk        │ │
│  └─────────────────────────────────────┘ │
└──────────────────────────────────────────┘
```

## Features

- **26-language AST parsing** — Go, Python, TypeScript, Rust, JavaScript,
  Java, C, C++, Ruby, C#, Kotlin, PHP, Swift, Elixir, Haskell, Erlang,
  Scala, Zig, Lua, Dart, Clojure, OCaml, R, Julia, Elm, Nim
- **Auto-classification** — detects entrypoints, test files, libraries
  without manual tagging
- **Cross-language impact** — "what C headers does the Python loader
  depend on?" answered in <1s
- **Metadata-first** — xattrs + JSONL inventory. File content is
  never modified
- **Parallel parsing** — rayon-powered, 800+ files in seconds
- **MCP server** — 8 tools (`vfs_graph_impact`, `vfs_graph_related`,
  `vfs_resolve_path`, etc.) for direct agent integration
- **FUSE mount** — standard `ls`, `cat`, `getfattr` through kernel
  filesystem

## Quickstart

```bash
# Initialize Hilo in any repo
hilo init

# Build the dependency graph
hilo graph discover

# Classify every file
hilo classify

# Query through CLI
hilo graph stats
hilo graph impact <file> --max-depth 3
hilo graph related <file> --relation imports

# Or mount as a filesystem
mkdir /mnt/vfs
hilo mount /mnt/vfs
ls /mnt/vfs/
getfattr -n user.vfs.role /mnt/vfs/src/main.rs

# Or serve MCP for agents
hilo serve --mcp
```

## License

MIT — see [LICENSE](LICENSE).
