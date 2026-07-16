# Contributing to Hilo

## Development Setup

```bash
git clone https://github.com/totalwindupflightsystems/hilo.git
cd hilo

# Build
cargo build

# Run tests
cargo test --workspace

# Format + lint
cargo fmt --check
cargo clippy --workspace -- -D warnings
```

Requirements: Rust 1.80+, `libfuse3-dev`, `attr`.

## Project Structure

```
hilo/
├── hilo-core/        # Manifest, workspace, sandbox, virtual dirs
├── hilo-metadata/    # xattr read/write, JSONL inventory
├── hilo-graph/       # Tree-sitter parsers, DuckDB queries, classify
├── hilo-mcp/         # JSON-RPC MCP server (15 tools)
├── hilo-cli/         # CLI (init, meta, graph, classify, mount, serve)
├── hilo-fuse/        # FUSE daemon, ops, permissions
├── hilo-backends/    # S3, Git, local storage backends
├── hilo-triggers/    # inotify watchers
├── hilo-plugins/     # Extism WASM plugin runtime
├── hilo-permissions/ # Mode-bit enforcement engine
└── specs/              # Design documents
```

## Commit Convention

```
feat(<crate>): <brief description>

Co-authored-by: wojons <wojonstech@gmail.com>
```

Crate name matches Cargo.toml `name` field: `hilo_core`, `hilo_graph`,
`hilo_metadata`, `hilo_cli`, `hilo_mcp`, `hilo_backends`,
`hilo_triggers`, `hilo_fuse`, `hilo_plugins`, `hilo_permissions`.

## Pre-commit Checks

Hilo uses [GitReins](https://github.com/totalwindupflightsystems/gitreins)
for pre-commit enforcement:

- **Tier 1** (blocks commit): secrets scan, `cargo test`, LSP diagnostics,
  static analysis
- **Tier 2** (post-commit): LLM-based semantic evaluation

All commits must pass Tier 1 guards. Install the hook:

```bash
pip install gitreins
gitreins init
```

## Pull Requests

1. Run `cargo fmt` and `cargo clippy` before pushing
2. Add tests for new functionality
3. Update CHANGELOG.md under `[Unreleased]`
4. PR title follows commit convention above

## Adding a Language

1. Add the `tree-sitter-<lang>` crate to `hilo-graph/Cargo.toml`
2. Add the variant to `Language` enum in `hilo-graph/src/parser.rs`
3. Add extension → language mapping in `from_extension()`
4. Implement import extraction in `parse_imports()`
5. Add entrypoint + test patterns to `hilo-graph/src/classify.rs`
6. Add test files for verification
