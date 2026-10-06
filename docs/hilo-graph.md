# hilo-graph — Graph Engine

The core dependency-graph engine. Builds a knowledge graph from source code using tree-sitter AST parsing, stores edges in DuckDB, and provides queries for related files, impact analysis, and semantic search.

**Crate:** `hilo-graph`  
**Public modules:** 11  
**Key dependency:** `hilo-metadata` (shared `Edge` type)

## Public API Surface

### Types

| Type | Description |
|------|-------------|
| `GraphDB` | DuckDB-backed graph database — open, initialize schema, insert edges, query |
| `Parser` | Tree-sitter AST parser for 26 languages — extracts import edges |
| `Edge` | Re-export from `hilo_metadata::inventory` — `{from, to, rel, provenance, confidence}` |
| `Language` | Enum of 26 supported languages (Go, Rust, Python, TS, C#, Kotlin, PHP, Swift, Elixir, Haskell, Erlang, Scala, Zig, Lua, Dart, Clojure, OCaml, R, Julia, Elm, Nim, C, C++, Java, Ruby, ...) |
| `Provenance` | Edge provenance level: `AstExact`(1.0), `AstInferred`(0.8), `Heuristic`(0.5), `Lexical`(0.3), `Latent`(0.3), `Unresolved`(0.0) |
| `ImpactResult` | Impact analysis result — `{subject, total, files: Vec<ImpactFile>}` |
| `ImpactFile` | A file in the impact chain — `{path, relation, depth, scope, via?, provenance?, confidence?}`; `scope` is `self` / `file` / `crate` / `dependency` / `link` (`GAP-117`) |
| `SignalOpts` | Signal engine config — `{token_budget, seed_limit, depth, max_nodes, resolution}` |
| `SignalResult` | Compressed codebase context — `{text, files, tokens_estimate, anchors}` |
| `SignalFile` | File in signal output — `{path, symbols, tier, provenance, signal_score, detail}` |
| `SymbolSignature` | Symbol with location — `{name, line_number, signature}` |
| `Tier` | Signal tier — `Map`, `Signature`, `Detail` |
| `Resolution` | Output resolution — `Harmonic` (3-tier) or `Flat` |
| `SearchResult` | Semantic search hit — `{file_path, symbols, score, provenance}` |
| `SearchOpts` | Search config — `{limit}` |
| `TfIdfIndex` | In-memory TF-IDF index over graph nodes |
| `ModuleStats` | Per-module edge counts — `{module, edge_count, files_count}` |
| `Direction` | Query direction — `Forward` or `Reverse` |
| `Classification` | File classification — `{role, status, feature}` |
| `TestClass` | Test class (COV-3) — `unit`, `integration`, `e2e_process`, `conformance_golden`, `property_fuzz`, `chaos_fault`, `bench`, `doc_smoke` |
| `TestClassReport` | Per-class totals + per-surface class mix (`class_gap`, `missing_classes`) joined through the COV-2 links |
| `SurfaceClassMix` | One surface's reachable test-class set, its `class_gap` flag and named `missing_classes` |
| `Rule` | A DuckDB rule — `{id, name, query}` |
| `RuleEngine` | Rule execution engine over the graph |
| `RuleCheckResult` | Per-rule result — `{rule, passed, details}` |
| `GraphError` | Error type for graph operations |

### Functions

| Function | Description |
|----------|-------------|
| `Parser::new()` | Create a parser for all 26 languages |
| `Parser::parse(&self, path, source, lang) -> Vec<Edge>` | Parse source, extract import edges |
| `GraphDB::open(path) -> Result<Self>` | Open/create DuckDB graph database |
| `GraphDB::init_schema(&self)` | Create edges table with provenance columns |
| `GraphDB::insert_edges(&self, edges)` | Insert edges (deduped by from+to+rel+provenance) |
| `GraphDB::related(&self, path, direction) -> Vec<Edge>` | Get forward/reverse edges for a file |
| `compute_impact(db, path) -> ImpactResult` | Transitive BFS — all files depending on path |
| `compute_review_set(conn, path, max_depth) -> Vec<ImpactFile>` | `GAP-117`: the subject row, the dependents, and the bounded forward/reverse neighbourhood (helpers, tests, fixtures, build targets) |
| `expand_review_set(conn, path, dependents) -> Vec<ImpactFile>` | `GAP-117`: the same expansion over an already-computed dependent set (used by `GraphDB::review_or_parse`) |
| `compute_impact_with_external(db, path, ext) -> ImpactResult` | Impact with external dependency map |
| `classify_file(path, source, lang) -> Classification` | Classify a single file (role/status/feature) |
| `classify_test_file(path, source) -> Option<TestClass>` | Classify a test-bearing file into one class (COV-3); `None` for a non-test file |
| `classify_test_functions(path, source) -> Vec<TestFunctionClass>` | Discover test functions (Rust/Go/Python) and their classes |
| `test_classes::enumerate(root) -> Vec<TestFileClass>` | Walk a repo, classifying every test-bearing source file |
| `test_classes::derive(surfaces, links, files) -> TestClassReport` | Join the class map onto the COV-2 coverage links |
| `infer_feature(path) -> String` | Infer feature name from file path |
| `understand(opts) -> SignalResult` | Build harmonic multi-resolution context |
| `understand_with_source(opts, files) -> SignalResult` | Signal engine with pre-loaded source |
| `search(query, graph) -> Vec<SearchResult>` | Semantic search via TF-IDF + BM25 + RRF |
| `search_with_symbols(query, graph, symbols) -> Vec<SearchResult>` | Search with explicit symbol extraction |

## Usage Example

```rust
use hilo_graph::{GraphDB, Direction, Parser, compute_impact, Provenance};

// Open graph database
let db = GraphDB::open(".vfs/graph/graph.db")?;
db.init_schema()?;

// Parse a Go file
let parser = Parser::new();
let edges = parser.parse("main.go", &source, Language::Go)?;
// edges[0].provenance == Provenance::AstExact, confidence == 1.0

// Insert edges
db.insert_edges(&edges)?;

// Query forward dependencies
let deps = db.related("main.go", Direction::Forward)?;

// Impact analysis — what depends on handler.go?
let impact = compute_impact(&db, "handler.go")?;
for file in impact.files {
    println!("{} (depth {}, provenance {:?})", file.path, file.depth, file.provenance);
}
```
