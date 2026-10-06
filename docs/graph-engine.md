# Graph Engine

## Overview

The graph engine parses source code with tree-sitter, extracts import
relationships, and stores them in DuckDB for querying.

## Supported Languages (26)

| Language | Parser | Import Detection |
|----------|--------|-----------------|
| Go | `tree-sitter-go` | `import "..."` |
| Python | `tree-sitter-python` | `import X`, `from X import Y` |
| TypeScript | `tree-sitter-typescript` | `import ... from "..."`, `require(...)` |
| Rust | `tree-sitter-rust` | `use ...`, `extern crate ...` |
| JavaScript | `tree-sitter-javascript` | `import ... from "..."`, `require(...)` |
| Java | `tree-sitter-java` | `import ...` |
| C | `tree-sitter-c` | `#include "..."`, `#include <...>` |
| C++ | `tree-sitter-cpp` | `#include "..."`, `#include <...>` |
| Ruby | `tree-sitter-ruby` | `require "..."`, `require_relative "..."` |
| C# | `tree-sitter-c-sharp` | `using ...` |
| Kotlin | `tree-sitter-kotlin-ng` | `import ...` |
| PHP | `tree-sitter-php` | `use ...`, `use function ...`, `use const ...` |
| Swift | `tree-sitter-swift` | `import ...` |
| Elixir | `tree-sitter-elixir` | `alias/import/require/use` |
| Haskell | `tree-sitter-haskell` | `import ...`, `import qualified ...` |
| Erlang | `tree-sitter-erlang` | `-include(...)`, `-include_lib(...)` |
| Scala | `tree-sitter-scala` | `import ...` |
| Zig | `tree-sitter-zig` | `@import(...)` |
| Lua | `tree-sitter-lua` | `require(...)` |
| Dart | `tree-sitter-dart` | `import ...`, `export ...` |
| Clojure | `tree-sitter-clojure` | `:require`, `import` |
| OCaml | `tree-sitter-ocaml` | `open ...`, `include ...` |
| R | `tree-sitter-r` | `library(...)`, `require(...)` |
| Julia | `tree-sitter-julia` | `import ...`, `using ...` |
| Elm | `tree-sitter-elm` | `import ...` |
| Nim | `tree-sitter-nim` | `import ...`, `include ...` |

## Edge Types

| Relation | Direction | Meaning |
|----------|-----------|---------|
| `imports` | A → B | File A imports dependency B |
| `imported_by` | A ← B | File A is imported by file B (reverse of imports) |
| `tests` | A → B | Test file A tests source file B |
| `tested_by` | A ← B | Source file A is tested by test file B |

## How Discovery Works

1. **Walk** — collect all source files, skip `target/`, `node_modules/`, etc.
2. **Parse** — parallel parse with rayon, one tree-sitter parser per file
3. **Extract** — walk AST for import statements, resolve to canonical paths
4. **Deduplicate** — `INSERT OR IGNORE` with unique constraint on `(from, to, rel)`
5. **Persist** — append to `edges.jsonl` (source of truth), insert into DuckDB (query cache)

## DuckDB Schema

```sql
CREATE TABLE IF NOT EXISTS edges (
    "from" TEXT NOT NULL,
    "to" TEXT NOT NULL,
    rel TEXT NOT NULL DEFAULT 'imports',
    provenance TEXT NOT NULL DEFAULT 'ast_exact',
    confidence REAL NOT NULL DEFAULT 1.0
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_edges_unique
    ON edges("from", "to", rel, provenance);
```

## Impact Analysis

BFS traversal from a starting node outward through `imports` edges:

1. Start with file X
2. Find all files that import X (`WHERE "to" = ?` in DuckDB)
3. For each, find files that import *them*
4. Repeat up to `max_depth` (default 5)
5. Visited set prevents infinite loops from circular imports

## Classification

Separate from graph discovery. `hilo classify` uses tree-sitter AST
queries to detect:

- **Entrypoints** — `fn main()`, `if __name__`, `public static void main`
- **Test files** — filename patterns (`*_test.go`, `test_*.py`, `*.spec.ts`)
- **Libraries** — public API surface (exported functions, classes)
- **Stability** — path heuristics (`src/` vs `examples/`), public API ratio

Results stored as xattrs: `user.vfs.role`, `user.vfs.status`.

## Performance

- **Parallel parsing** — rayon thread pool, one parser per file
- **Deduplication** — unique index prevents re-discover from doubling edges
- **Incremental** — `append_edges_deduped()` only adds new edges to JSONL
- **Progress** — output every 100 files during discovery
- **Vendor skip** — `target/`, `node_modules/`, `vendor/` excluded by default

## Signal Engine — `vfs_graph_understand`

The signal engine produces budgeted, tiered context from the dependency graph
so agents get the *shape* of the code first, then exact lines last.

### Tiers

| Tier | Budget | Content |
|------|--------|---------|
| MAP | 15% | `{ file: [symbols…] }` — orientation |
| SIGNATURES | 25% | `file:line  fn foo(x: i32) -> bool` — spine |
| DETAIL | 60% | whitespace-minified source blocks — exact lines |

### Position Ordering

Highest-signal files are placed at the **edges** of the output (first and last),
lower-signal files in the **middle**. This exploits the empirical finding that
attention-limited models attend more to the beginning and end of context
windows ("lost in the middle" effect).

### Determinism

The engine is fully deterministic: same task + same graph → byte-identical
text output. No randomness, no model calls, no external API.

### MCP Tool

```json
{
  "name": "vfs_graph_understand",
  "arguments": {
    "task": "rate limiter middleware",
    "budget": 6000,
    "resolution": "harmonic"
  }
}
```

- `task` (required): natural-language description of what the agent needs
- `budget` (optional): token budget, default 6000
- `resolution` (optional): `"harmonic"` (3-tier, default) or `"flat"` (single-tier)

### Anchor Discovery

The engine tokenizes the task string (lowercase, split on non-alphanumeric,
filter tokens ≥ 3 chars) and matches tokens against file paths in the graph.
Files with the most token matches become anchor/seed files. The engine then
traverses the graph from anchors (BFS, depth 2 by default) to collect related
files, scoring each by provenance weight × depth factor.

When literal matching returns no anchors, the engine falls back to
semantic search (TF-IDF + BM25 + Reciprocal Rank Fusion) to find files
by meaning rather than literal substring matching.

## Semantic Code Search — `vfs_graph_search`

The semantic search module provides deterministic, zero-API code search
using classical NLP techniques:

- **Tokenization**: splits symbols on camelCase/snake_case boundaries,
  lowercases, and deduplicates
- **TF-IDF**: term frequency × inverse document frequency over all graph nodes
- **BM25**: Okapi BM25 ranking function for relevance scoring
- **Reciprocal Rank Fusion (RRF)**: combines TF-IDF + BM25 results via RRF
  (k=60) to produce a single ranked list

### Determinism

Same query + same graph → byte-identical results. No randomness, no
external API calls, no model inference. Pure Rust, stdlib only.

### MCP Tool

```json
{
  "name": "vfs_graph_search",
  "arguments": {
    "query": "authentication middleware",
    "limit": 20
  }
}
```

- `query` (required): search query — tokenized on camelCase/snake_case
- `limit` (optional): max results, default 20

### Integration with Signal Engine

When the signal engine's literal anchor discovery returns no files,
it falls back to semantic search for anchor discovery. This enables
the signal engine to find relevant files even when the task description
doesn't contain literal path substrings.

## Surface Inventory — `hilo graph surfaces` (COV-1)

The graph engine also owns the **surface inventory**: one row per
externally-visible contract point of the codebase, derived from code rather
than hand-written. `hilo graph surfaces` enumerates seven kinds and persists
them to `.vfs/graph/surfaces.jsonl` (append-only, deduplicated by a
content-derived `surface_id`, the same identity discipline `edges.jsonl`
follows).

| kind | provider |
|------|----------|
| `cli_verb` | clap `CommandFactory` introspection of `hilo-cli/src/cli.rs` |
| `cli_flag` | the same — the `--long` / `-s` args each subcommand registers |
| `mcp_tool` | `hilo_mcp::tools::list_tools()` — the server's own registry |
| `ffi_export` | `vfs_*` declarations in `hilo-ffi/src/hilo.udl` |
| `fuse_op` | `fn` methods of `impl Filesystem for Hilo` in `hilo-fuse/src/ops.rs` |
| `config_key` | top-level `pub` fields of `hilo_core::manifest::Manifest` |
| `public_api_item` | `pub use` re-exports of `hilo-graph/src/lib.rs` |

### JSON shape

```json
{
  "schema": 1,
  "scope": "self",
  "surfaces": [
    { "surface_id": "…", "kind": "mcp_tool",
      "owner_file": "hilo-mcp/src/tools/mod.rs",
      "owner_symbol": "vfs_get_metadata", "name": "vfs_get_metadata",
      "public": true }
  ],
  "census": [
    { "kind": "mcp_tool", "detected": 17, "expected": 17,
      "rule": "hilo_mcp::tools::list_tools() registry (17 tools)" }
  ]
}
```

Every kind reports a census row: `detected` vs `expected` plus the rule that
produced it, so a parser that finds nothing reads as a **gap**
(`detected == 0`) rather than a silent empty success.

### Scope contract (GAP-112)

The `cli_verb` / `cli_flag` / `mcp_tool` providers run **unconditionally** —
they describe the `hilo` binary that is executing, not the tree it is pointed
at — and the source-derived kinds parse Hilo's own files. On a repository
that is not Hilo, presenting that self-data as the target's surfaces is a
confident wrong answer, so `graph surfaces` classifies the tree first and
declares the scope it answered:

- **`scope: "self"`** — the current directory (or an ancestor) is a Hilo
  workspace: it carries all three marker files `hilo-cli/src/cli.rs`,
  `hilo-mcp/src/tools/mod.rs`, and `hilo-graph/src/lib.rs`. The full
  self-inventory is returned and appended to `.vfs/graph/surfaces.jsonl`.
  This is the unchanged COV-1 behaviour on the Hilo repo itself.
- **`scope: "foreign"`** — the tree is not Hilo. **No Hilo-owned row is
  presented as the target's result**: `surfaces` and `census` are empty, the
  `scope` marker is the entire answer, nothing is written to the tree, and
  the command exits non-zero (a foreign run is a refusal, never a
  success-shaped empty report). A foreign `--json` run still prints the
  marker so a machine caller can branch on `scope` instead of inferring
  "zero surfaces".

The classification walks **up** from the current directory the way `cargo`
finds `Cargo.toml`, so a run inside a Hilo subdirectory still reads as
`self`. It stops at the first repository root (a `.git` entry) that is not
Hilo, so a foreign checkout nested under a Hilo tree classifies as `foreign`
rather than inheriting the ancestor's identity.

`--kind <kind>` restricts both the rows and the census to one kind; an
unknown kind is refused with the valid set named.
