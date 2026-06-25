# Graph Engine

## Overview

The graph engine parses source code with tree-sitter, extracts import
relationships, and stores them in DuckDB for querying.

## Supported Languages

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
    rel TEXT NOT NULL DEFAULT 'imports'
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_edges_unique
    ON edges("from", "to", rel);
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
