# Hilo Documentation

Hilo is an agent-first virtual filesystem. It pre-computes a dependency
graph across your codebase and exposes it through standard filesystem
tools and an MCP server — so AI agents can answer structural questions
without reading files.

## Guides

- **[Getting Started](getting-started.md)** — install, init, first discover
- **[Architecture](architecture.md)** — FUSE, metadata engine, backends
- **[CLI Reference](cli-reference.md)** — every command and flag
- **[MCP Tools](mcp-tools.md)** — agent integration via JSON-RPC
- **[Graph Engine](graph-engine.md)** — AST parsing, DuckDB, impact analysis

## Crate API Reference

- **[hilo-graph](hilo-graph.md)** — graph engine: AST parsing, DuckDB queries, impact, signal, semantic search
- **[hilo-mcp](hilo-mcp.md)** — MCP server: 17 tools over stdio JSON-RPC
- **[hilo-core](hilo-core.md)** — manifest, config, workspace, sandbox
- **[hilo-metadata](hilo-metadata.md)** — xattr I/O, JSONL inventory, shared `Edge` type
- **[hilo-backends](hilo-backends.md)** — S3, Git, and local storage backends
- **[hilo-fuse](hilo-fuse.md)** — kernel-level FUSE virtual filesystem
- **[hilo-triggers](hilo-triggers.md)** — inotify event engine: parse-and-diff, upload-to-backend
- **[hilo-plugins](hilo-plugins.md)** — Extism WASM plugin runtime
- **[hilo-permissions](hilo-permissions.md)** — glob-based permission engine with mode bits
- **[hilo-cli](hilo-cli.md)** — command-line interface: all subcommands and flags
- **[hilo-ffi](hilo-ffi.md)** — UniFFI language bindings for Go, Python, Kotlin, Swift

## Quick Links

- [GitHub Repository](https://github.com/gethilo/hilo)
- [Hilo Docs](https://gethilo.github.io/hilo/) — rendered documentation site

## Supported Languages

Go, Python, TypeScript, Rust, JavaScript,
Java, C, C++, Ruby, C#, Kotlin, PHP,
Swift, Elixir, Haskell, Erlang, Scala,
Zig, Lua, Dart, Clojure, OCaml, R, Julia, Elm, Nim
