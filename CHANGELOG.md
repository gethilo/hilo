# Changelog

All notable changes to Hilo are documented in this file.

## [0.2.0] — 2026-07-16

### Added

- **Provenance tracking** — every graph edge tagged with `provenance`
  (AstExact, AstInferred, Heuristic, Lexical, Latent, Unresolved) and
  `confidence` (0.0–1.0). DuckDB schema auto-migrates from v0.1 format.
  `vfs_graph_related` returns provenance + confidence per edge.
- **Signal engine** — harmonic multi-resolution context compression
  (MAP → SIGNATURES → DETAIL), 60% smaller than raw dump, position-ordered.
  New MCP tool `vfs_graph_understand`.
- **Semantic code search** — pure-Rust TF-IDF + Okapi BM25 + Reciprocal
  Rank Fusion. Zero API calls, zero embeddings, fully deterministic.
  New MCP tool `vfs_graph_search`.
- **Determinism tests** — 14 tests proving byte-identical graph/signal/search
  output across runs. Immutable fixture corpus with 6 files (Go, Python, TS).
- **Language expansion** — from 9 to 26 languages: C#, Kotlin, PHP, Swift,
  Elixir, Haskell, Erlang, Scala, Zig, Lua, Dart, Clojure, OCaml, R, Julia,
  Elm, Nim. Tiers 1–3.
- **MCP server** — expanded from 8 to 15 tools: `vfs_graph_understand`,
  `vfs_graph_search`, `vfs_set_metadata`, `vfs_graph_module`,
  `vfs_graph_untested`, `vfs_backend_status`, `vfs_sync_backend`.
- **JIT/lazy query architecture** — `hilo graph related` works on fresh
  `hilo init` without pre-warming. Files auto-parse on first access.
- **GitHub Pages** — documentation site at `https://gethilo.github.io/hilo/`
  with landing page + 5 doc pages.

### Changed

- `hilo graph discover` renamed to `hilo graph warm` (Discover kept as alias)
- Edge struct extended with provenance + confidence fields
- DuckDB unique index includes provenance column
- Signal engine integrated into semantic search anchor discovery

## [0.1.0] — 2026-06-24

### Added

- **26-language AST parsing** — Go, Python, TypeScript, Rust, JavaScript,
  Java, C, C++, Ruby, C#, Kotlin, PHP, Swift, Elixir, Haskell, Erlang,
  Scala, Zig, Lua, Dart, Clojure, OCaml, R, Julia, Elm, Nim via tree-sitter
- **Auto-classification** — `hilo classify` detects entrypoints, test
  files, library roles, stability status across all 26 languages
- **Parallel graph discovery** — rayon-powered, progress output every
  100 files
- **Dependency graph** — DuckDB-backed with forward/reverse queries,
  transitive impact analysis (BFS)
- **Cross-language edges** — `tested_by`, `tests`, `imported_by`
- **MCP server** — 8 tools: `vfs_get_metadata`, `vfs_graph_related`,
  `vfs_graph_stats`, `vfs_graph_impact`, `vfs_rule_list`,
  `vfs_rule_check`, `vfs_list_directory`, `vfs_resolve_path`
- **FUSE mount** — kernel-level virtual filesystem with xattr passthrough,
  permission enforcement, read-only mode
- **CLI** — `init`, `meta --set/--read`, `graph discover/stats/related/impact`,
  `classify`, `mount`, `serve --mcp`
- **Storage backends** — S3 (read-only + write-through), remote Git (auto-pull),
  local disk passthrough
- **Bubblewrap sandboxing** — agent process isolation via bwrap
- **Plugin system** — Extism WASM runtime with host functions
- **Permission engine** — glob-based mode bit enforcement
- **inotify triggers** — debounced file-watch re-parsing
- **Virtual directories** — S3 buckets as local paths with auto-upload tracking
- **Workspace mounts** — multi-repo unified FUSE tree

### Infrastructure

- 10 crates, 31 test suites, zero warnings
- GitReins pre-commit: secrets + tests + LSP + static analysis
- Coding Hermes foreman for autonomous development
