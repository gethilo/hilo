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

## Terraform / HCL (static subset)

`.tf` and `.hcl` files are recognized by the graph parser; `.tf.json` is
recognized by full-path consumers. Terraform support is static and local: Hilo
does not invoke Terraform, initialize providers, read cloud credentials, or
evaluate expressions. The scanner reports provider, module, resource, data,
variable, local, and output block declarations with source file and starting
line. It emits typed relations for module `source`, `depends_on`, and explicit
`var.`, `local.`, `module.`, and resource/data address references. Module
addresses are scoped as `module.<name>`; indexes in references are retained.

The implementation is deliberately a conservative block scanner, not a full
HCL grammar. Heredocs, multiline strings, complex dynamic blocks, template
control directives, automatic nested-module source loading, and `.tf.json`
expression decoding are not fully modeled. A resolved nested module can be
parsed with its source-derived `module.<name>` scope; paths and names alone are
not used to assign scopes. Unsupported or malformed blocks emit unresolved
`parse_coverage_gap` edges, and each Terraform file reports an unresolved
`deployment_link_unknown` edge until explicit evidence is supplied. The
`explicit_deployment_link` API requires caller-supplied evidence; name or
directory similarity is never evidence for an application-to-infrastructure
link.

## Edge Types

| Relation | Direction | Meaning |
|----------|-----------|---------|
| `imports` | A → B | File A imports dependency B |
| `imported_by` | A ← B | File A is imported by file B (reverse of imports) |
| `tests` | A → B | Test file A tests source file B |
| `tested_by` | A ← B | Source file A is tested by test file B |

`imports`/`tested_by` targets are usually file paths, but for several
languages the parser emits package/symbol nodes instead: Python emits
`pkg:<dotted module>` (GAP-064), Go `pkg:<import path>` (GAP-057), and Java
`pkg:<fully-qualified class name>` (DF-WARPFS-34); TypeScript/JavaScript emit
`local:<relative specifier>`. A Python file with no package ancestor is a
flat (top-level) module and maps to its bare stem (`util.py` → `pkg:util`,
GAP-104), matching the node the parser emits for `import util`. Coverage
consumers (`graph untested`,
`graph module` stats) resolve a file to these nodes when counting test
coverage, so a Java class covered via `tested_by -> pkg:com.example.Foo`
counts as tested even though no edge names the file directly.

## How Discovery Works

1. **Walk** — collect all source files, skip `target/`, `node_modules/`, etc.
2. **Parse** — parallel parse with rayon, one tree-sitter parser per file
3. **Extract** — walk AST for import statements, resolve to canonical paths
4. **Deduplicate** — identity is `(from, to, rel, provenance)`, enforced at the
   JSONL append boundary (`append_edges_deduped`, REV-WARPFS-8) and mirrored
   into DuckDB with `INSERT OR IGNORE`
5. **Persist** — append to `edges.jsonl` (source of truth), insert into DuckDB (query cache)

## DuckDB Schema

```sql
CREATE TABLE IF NOT EXISTS edges (
    "from" TEXT NOT NULL,
    "to" TEXT NOT NULL,
    rel TEXT NOT NULL,
    provenance TEXT NOT NULL DEFAULT 'ast_exact',
    confidence REAL NOT NULL DEFAULT 1.0
);

CREATE INDEX IF NOT EXISTS idx_edges_from_rel ON edges("from", rel);
```

There is deliberately NO unique index on `(from, to, rel, provenance)` in
DuckDB: the insert statement is `INSERT OR IGNORE`, which is a no-op for
duplicates without a unique constraint, so DuckDB-level dedup would not fire
anyway. Duplicate suppression is enforced where the source of truth is
written — the `edges.jsonl` append boundary (below) — and DuckDB is
rebuildable from that file at any time (`hilo graph warm` after `hilo graph
gc` compacts it).

## Edge Deduplication and `graph gc` (REV-WARPFS-8)

An edge's identity is the 4-tuple `(from, to, rel, provenance)` — **never**
`(from, to)` alone: the same pair legitimately carries multiple relations
(`imports` + `tests`, `service_call` + `service_contract`) and multiple
provenances (`ast_exact` vs `lexical`).

Suppression points, all sharing that identity:

- **Warm append** — `hilo graph warm` writes `edges.jsonl` through
  `append_edges_deduped`, which reads the existing file into a
  `HashSet<(from, to, rel, provenance)>`, appends only genuinely new edges,
  and grows the set with each accepted edge so duplicates *within* one batch
  collapse to their first occurrence too.
- **Trigger append** — the file watcher (`hilo mount --triggers`) appends
  through the same deduped writer; heuristic edges regenerate on every
  event, so a raw append here was the historical duplicate source.
- **`hilo graph gc`** — one-shot compaction for files written before this
  discipline (or by older versions). Removes exact-duplicate rows, keeping
  the FIRST occurrence of each identity in original row order; `confidence`
  travels with the kept row. The rewrite is atomic (temp file + rename) and
  runs under the advisory `edges.jsonl.lock` flock shared with the append
  paths, so it is safe to run while `hilo mount --triggers` or another
  `graph warm` is live. Idempotent: a second pass removes nothing and does
  not rewrite the file. Malformed (unparseable) lines are preserved
  verbatim — gc removes exact duplicates only, it is not a repair tool.

The acceptance probe mirrors what CI asserts: after repeated warm runs on
the same corpus, `sort .vfs/graph/edges.jsonl | uniq -d` finds nothing.

## Stale-edge pruning (DF-WARPFS-170)

Dedup keeps the inventory from growing *sideways*; it never lets it shrink.
That is the gap this pass closes: a dependency the agent REMOVED — a deleted
file, or an import deleted from a file that still exists — used to keep its
row and keep answering `impact`/`related`/`stats`/`untested` forever. The
DuckDB cache is insert-only on top of that, so the stale row was doubly
durable.

Every `hilo graph warm` now runs `hilo_graph::prune::prune_inventory` before
deciding whether the graph changed:

0. **Derivation filter** — `drop_dangling_edges` drops any derived edge whose
   file endpoint does not exist BEFORE it is appended. Import resolvers are
   structural (a Rust `mod lib;` still resolves to a deleted `src/lib.rs`), so
   without this the prune below would delete the row and the same warm would
   append it straight back.
1. **Missing file** — either endpoint that names a FILE (no `<scheme>:`
   prefix) must exist under the corpus root. Absolute endpoints are checked
   as written. This rule runs on every warm.
2. **Superseded row** — a row whose `from` is a source file this pass
   re-derived, whose identity the derivation no longer emits, and whose
   provenance is one warm writes (`ast_exact`, `ast_inferred`,
   `ast_heuristic`, `heuristic`, `lexical`, `latent`, `unresolved`,
   `grpc_ast`, `grpc_proto`). Hand-maintained rows (`manual`, unknown tags)
   are never judged. Applied only on a complete derivation — `--changed` and
   `--language` derive a subset and pass no source scope.

The rewrite is atomic (temp file + rename) under the same advisory flock as
`append_edges_deduped` and `graph gc`, and preserves malformed lines verbatim.
`prune_cache` then deletes the same identities from the `edges` table and
re-records the reconcile checkpoint, so the query layer agrees with the
inventory immediately and the next open still takes the fast path.

Idempotence is structural: when the predicate drops nothing, `prune_edges`
does not rewrite the file at all, so a re-warm of a reconciled corpus leaves
the bytes (and the checkpoint fingerprint) untouched.

## Impact Analysis

`compute_impact` walks the reverse edges from a starting node — every file that
depends on it, directly or transitively:

1. Start with file X
2. Find all files that import X (`WHERE "to" = ?` in DuckDB)
3. For each, find files that import *them*
4. Repeat up to `max_depth` (default 5)
5. Visited set prevents infinite loops from circular imports

### Review set (`GAP-117`)

A blast radius is not the same question as "what must I review": the changed
file itself, the helper/header/properties class it points AT, and the tests,
fixtures and build targets that reference those are all review targets the
reverse-only walk never reported. `compute_review_set` returns that superset:

1. the subject row (`scope: self`, `depth: 0`) — always first;
2. the reverse dependents from `compute_impact_at` (pkg-family and `local:`
   semantics unchanged);
3. a bounded both-direction expansion — outgoing edges from the subject and
   from each node the expansion reaches (`scope: dependency`), then incoming
   edges off those (`scope: link`), at most `REVIEW_FORWARD_DEPTH` (2) forward
   and `REVIEW_REVERSE_DEPTH` (2) reverse hops.

Real repo evidence for the shape: a C++ implementation's change reaches its
header at one forward hop, the test that includes the header at one reverse hop
from there, and the BUILD target that names the test at the next reverse hop.

`compute_impact` / `compute_impact_at` keep their reverse-only contract —
callers that need the historical dependent set are unaffected.

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
- **Deduplication** — identity `(from, to, rel, provenance)` enforced at the JSONL append boundary; `hilo graph gc` compacts legacy files (REV-WARPFS-8)
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

### MAP Ranking Heuristic (REV-WARPFS-6)

Anchor discovery ranks by path-token match, so symbol-dense sample corpora can
tie with — and alphabetically outrank — the implementation a task is about.
Measured on hilo's own repo: `hilo graph understand "rate limiter middleware"`
filled the MAP with 54 `hilo-graph/tests/fixtures/…` rows (a vendored ruff
fixture tree) ahead of `hilo-mcp/src/rate_limiter.rs`.

Two path-aware levers apply to the MAP tier only:

1. **Excluded path prefixes** — a file under one of the excluded prefixes
   (`tests/fixtures`, `examples`, `vendor` by default) does not render in the
   MAP tier **at all**. The MAP is the agent's orientation budget; sample
   corpora have high symbol density and low relevance, so they are filtered
   from it. The filter is tier-local: the file stays in the machine-readable
   `files` list, keeps its traversal-order tier assignment, and an
   anchor-band fixture still gets its full DETAIL source block. If every
   matched file is filtered, the MAP prints an explicit empty-tier marker.
2. **Path depth weighting** — the remaining files re-order by signal score
   minus a bounded depth demotion: a file more than 4 components below the
   repo root loses `0.02` per extra component, capped at `0.1`. Of two
   same-band files the shallower (closer to the repo root) leads. The cap
   stays far below the smallest gap between traversal bands (0.3 between
   1-hop 0.8 and 2-hop 0.5), so the weighting re-sorts ties *within* a band —
   it can never promote a 2-hop file above an anchor.

The engine's own `SignalOpts::default()` carries an **empty** exclusion list,
so library callers and every pinned regression keep the exact historical
order; the default list applies at the CLI and MCP surfaces.

**Configuration** — `graph.map_exclude_paths` in `.vfs/manifest.yaml`
(path-component-boundary prefixes, repo-root-relative):

```yaml
graph:
  map_exclude_paths:
    - tests/fixtures
    - examples
    - vendor
```

Defaults to `tests/fixtures`, `examples`, `vendor`. Set an explicit empty
list (`map_exclude_paths: []`) to disable the filtering. Matching is at
component boundaries and NOT anchored at the repo root — the prefix's
component sequence may appear anywhere in the path, so `tests/fixtures`
covers both `tests/fixtures/…` and (in a Cargo workspace)
`hilo-graph/tests/fixtures/…`, but never `tests/fixtures_go_here` or
`…/tests/fixtures.rs`. It also does **not** touch `tests/test_*.py` or
`test/…` trees — those keep ranking normally (the wave11 recall corpora
depend on this).

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
    { "kind": "mcp_tool", "detected": 18, "expected": 18,
      "rule": "hilo_mcp::tools::list_tools() registry (18 tools)" }
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
