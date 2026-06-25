# Changelog

All notable changes to Hilo are documented in this file.

## [0.1.0] — 2026-06-24

### Added

- **9-language AST parsing** — Go, Python, TypeScript, Rust, JavaScript,
  Java, C, C++, Ruby via tree-sitter
- **Auto-classification** — `hilo classify` detects entrypoints, test
  files, library roles, stability status across all 9 languages
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
