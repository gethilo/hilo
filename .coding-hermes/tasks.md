# Hilo v0.2 — The Rinnegan-Upgrade Batch

WORKING DIRECTORY: /home/kara/warpfs. DO NOT work elsewhere.

You are upgrading Hilo with four features that Rinnegan (a competing provenance-graph code-knowledge engine) does better today. Each task is self-contained but shares the same edge schema migration. Read AGENTS.md first.

---

## [x] TASK-001: Provenance Tracking on Every Edge

### Why
Hilo edges are `{ from, to, rel }`. The agent can't distinguish "this import is definitely called" from "this was pattern-matched by a heuristic." Rinnegan tags every edge `ast_exact | ast_inferred | heuristic | lexical | latent | unresolved` with a confidence weight (1.0 → 0).

### What
Add `provenance` and `confidence` fields to the Edge struct and DuckDB schema.

### Implementation

1. **Extend `Edge` struct** in `hilo-metadata/src/inventory.rs`:
   ```rust
   pub struct Edge {
       pub from: String,
       pub to: String,
       pub rel: String,
       pub provenance: Provenance,    // NEW
       pub confidence: f64,           // NEW: 0.0 – 1.0
   }
   ```

2. **Add `Provenance` enum** in a new `hilo-graph/src/provenance.rs`:
   ```rust
   #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
   pub enum Provenance {
       AstExact,      // directly from the AST — the only ground truth (weight 1.0)
       AstInferred,   // AST-derived but required inference (weight 0.8)
       Heuristic,     // pattern-synthesized (weight 0.5)
       Lexical,       // discovered by BM25/text search (weight 0.3)
       Latent,        // discovered by LSA/semantic search (weight 0.3)
       Unresolved,    // static path ends here — dynamic dispatch (weight 0.0)
   }

   impl Provenance {
       pub fn trust_weight(&self) -> f64 { /* map as above */ }
       pub fn is_ground_truth(&self) -> bool { matches!(self, AstExact) }
   }
   ```

3. **Update DuckDB schema** — version the table to `edges_v2` or add columns with ALTER:
   ```sql
   CREATE TABLE edges_v2 (
       "from" TEXT NOT NULL,
       "to" TEXT NOT NULL,
       rel TEXT NOT NULL,
       provenance TEXT NOT NULL DEFAULT 'ast_exact',
       confidence REAL NOT NULL DEFAULT 1.0
   );
   ```
   - Maintain backward compat: auto-migrate existing `edges` table, or reject old format with a clear error.
   - Update the unique index to include provenance.

4. **Update every parser** to tag edges. For v0.2, start simple:
   - All tree-sitter import edges → `AstExact` (they came from the AST)
   - Test-relationship edges (filename heuristic) → `Heuristic` (confidence 0.8)
   - Leave room for `Lexical`/`Latent` in TASK-003

5. **Update every query** — `vfs_graph_related`, `vfs_graph_stats`, `vfs_graph_impact`, `vfs_graph_module`, `vfs_graph_untested`, and all rule queries — to handle the new columns. Survival rule: every query that returns edges must also return provenance + confidence.

6. **Update MCP tools** output to include provenance + confidence in responses.

### Files touched
- `hilo-metadata/src/inventory.rs` — Edge struct
- `hilo-graph/src/provenance.rs` — NEW file
- `hilo-graph/src/graph.rs` — schema, insert_edges
- `hilo-graph/src/parser.rs` — tag edges at extraction
- `hilo-graph/src/impact.rs` — return provenance in ImpactFile
- `hilo-graph/src/lib.rs` — re-export Provenance
- `hilo-mcp/src/tools/mod.rs` — include provenance in tool responses
- `hilo-triggers/src/engine.rs` — update test schemas
- `hilo-graph/src/rules.rs` — update test schemas
- `docs/graph-engine.md` — document new columns

### Acceptance criteria
- [ ] `cargo check --workspace` passes
- [ ] `cargo test --workspace` passes (31 suites)
- [ ] `cargo clippy --workspace -- -D warnings` clean
- [ ] `cargo fmt --all` clean
- [ ] Every edge in `edges.jsonl` has `provenance` and `confidence` fields
- [ ] Old `edges.jsonl` (no provenance) is either auto-migrated or rejected with a clear error
- [ ] `vfs_graph_related` returns `provenance` + `confidence` per edge
- [ ] Edge struct roundtrips through JSONL serialize → deserialize correctly

### Result
**Status: COMPLETE — 2026-07-03**

Implemented provenance tracking across the entire Hilo workspace:

- **New `Provenance` enum** (`hilo-graph/src/provenance.rs`): 6 levels (AstExact, AstInferred, Heuristic, Lexical, Latent, Unresolved) with trust_weight(), is_ground_truth(), parse(), serde snake_case serialization. 7 unit tests.
- **Extended `Edge` struct** (`hilo-metadata/src/inventory.rs`): added `provenance: String` and `confidence: f64` fields with `#[serde(default)]` for backward compatibility. Added `Edge::new()` and `Edge::with_provenance()` constructors. Updated `append_edges_deduped` to include provenance in dedup key.
- **DuckDB schema migration** (`hilo-graph/src/graph.rs`): `init_schema` creates 5-column `edges` table. `migrate_schema` auto-migrates old 3-column tables via `ALTER TABLE ADD COLUMN` (nullable + DEFAULT, with backfill). Unique index includes provenance.
- **All queries updated**: `related()`, `compute_impact()`, `compute_impact_with_external()` select and return provenance + confidence. `ImpactFile` struct has `provenance: Option<String>` and `confidence: Option<f64>`.
- **Parser tags edges**: tree-sitter import edges → `ast_exact` (conf 1.0). Test-association edges → `heuristic` (conf 0.8). Extension edges → `heuristic` (conf 0.5).
- **MCP tool output**: `vfs_graph_related` includes `provenance` + `confidence` per edge.
- **CLI output**: `hilo graph related` and `hilo graph impact` display `[provenance conf=X.XX]`.
- **FFI bindings**: `GraphEdge` and `GraphImpactEntry` include optional `provenance` + `confidence`. UDL updated.
- **Backward compat**: old JSONL without provenance deserializes with defaults (ast_exact, 1.0). Old DuckDB databases auto-migrate.

**Files touched (18 files):**
- `hilo-graph/src/provenance.rs` — NEW (187 lines)
- `hilo-graph/src/lib.rs` — re-export Provenance
- `hilo-graph/src/graph.rs` — schema, migration, insert, related
- `hilo-graph/src/impact.rs` — ImpactFile + BFS queries
- `hilo-graph/src/parser.rs` — tag edges with ast_exact
- `hilo-graph/src/rules.rs` — test schema updated
- `hilo-metadata/src/inventory.rs` — Edge struct + constructors + dedup
- `hilo-cli/src/commands/graph.rs` — edge construction + CLI output
- `hilo-mcp/src/tools/mod.rs` — MCP response includes provenance
- `hilo-ffi/src/lib.rs` — FFI structs updated
- `hilo-ffi/src/hilo.udl` — UDL dictionary updated
- `hilo-triggers/src/engine.rs` — test schemas updated
- `hilo-graph/tests/graph_test.rs` — helper + new tests
- `hilo-graph/tests/edges_test.rs` — helper updated
- `hilo-graph/tests/impact_test.rs` — helper + ImpactFile updated
- `hilo-metadata/tests/inventory_test.rs` — all Edge literals + backward compat tests
- `hilo-mcp/tests/mcp_test.rs` — all Edge literals updated

**Verification:**
- `cargo check --workspace` — PASS
- `cargo test --workspace` — 386 tests, 0 failures
- `cargo clippy --workspace -- -D warnings` — clean
- `cargo fmt --all` — applied
- Binary rebuilt + installed: `hilo graph related` shows `[ast_exact conf=1.00]`
- `hilo graph warm` succeeds with new schema
- Old JSONL deserializes with default provenance

---

## TASK-002: Signal Engine — Harmonic Multi-Resolution Context

### Why
`vfs_graph_related` returns ALL related files. An agent gets 50 files when it needs 5. Rinnegan's `understand()` returns ~3000 tokens of MAP → SIGNATURES → DETAIL, position-ordered, 85% smaller than a raw dump. The model gets the shape first, exact lines last.

### What
A new `hilo-graph/src/signal.rs` module that produces budgeted, tiered output from the dependency graph. Exposed as a new MCP tool `vfs_graph_understand`.

### Implementation

1. **New module: `hilo-graph/src/signal.rs`** with three tiers:

   ```rust
   pub struct SignalOpts {
       pub token_budget: usize,      // default 6000
       pub seed_limit: usize,        // default 8 — max anchor files
       pub depth: usize,             // default 2 — graph traversal depth
       pub max_nodes: usize,         // default 60
       pub resolution: Resolution,   // Harmonic (MAP→SIG→DETAIL) or Flat
   }

   pub enum Resolution { Harmonic, Flat }

   pub struct SignalResult {
       pub text: String,             // formatted MAP + SIGNATURES + DETAIL
       pub files: Vec<SignalFile>,   // machine-readable
       pub tokens_estimate: usize,
       pub anchors: Vec<String>,     // seed file paths
   }

   pub struct SignalFile {
       pub path: String,
       pub symbols: Vec<String>,     // key symbols in this file
       pub tier: Tier,               // map | signature | detail
       pub provenance: Provenance,
       pub signal_score: f64,
       pub detail: Option<String>,   // whitespace-minified source for "detail" tier
   }
   ```

2. **Harmonic budget split**: 15% MAP (orientation), 25% SIGNATURES (spine), 60% DETAIL (source)
   - MAP: `{ file_path: [symbol1, symbol2, ...] }` — which files are in play
   - SIGNATURES: `file:line   func AuthMiddleware(next http.Handler)` — one line per symbol
   - DETAIL: whitespace-minified source blocks with provenance tag

3. **Position ordering**: highest-signal facts at the edges of the output (first and last), lower-signal in the middle. This beats "lost in the middle" for attention-limited models.

4. **Whitespace minimization**: uniform dedent, elide blank lines (line-number gaps signal the elision).

5. **New MCP tool `vfs_graph_understand`**:
   - Input: `{ task: string, budget?: number, resolution?: "harmonic" | "flat" }`
   - Output: `{ text: string, files: SignalFile[], tokens_estimate: number }`
   - If graph isn't built, auto-build it (lazy init).

6. **Reuse existing graph queries** — `vfs_graph_related` + `vfs_graph_impact` — don't rebuild traversal. The signal engine is a VIEW layer on top of the existing graph, not a replacement.

### Files touched
- `hilo-graph/src/signal.rs` — NEW file (~200-400 lines)
- `hilo-graph/src/lib.rs` — re-export
- `hilo-mcp/src/tools/mod.rs` — add `vfs_graph_understand` tool
- `hilo-graph/tests/signal_test.rs` — NEW test file
- `docs/graph-engine.md` — document the tool

### Acceptance criteria
- [ ] `cargo check --workspace` passes
- [ ] `cargo test --workspace` passes
- [ ] `vfs_graph_understand { task: "rate limiter" }` returns 3-tier output
- [ ] Output respects token budget (60K chars ≈ 6K tokens)
- [ ] MAP tier groups by file with ≤8 symbols per file
- [ ] DETAIL tier output is whitespace-minified (no blank lines, uniform indent)
- [ ] Output is deterministic: same task + same graph → same text (no model, no randomness)
- [ ] Files are position-ordered (highest-signal at edges)

---

## TASK-003: Semantic Code Search — Deterministic, No Embeddings

### Why
Hilo search is SQL + FTS on text. You can't search for "authentication middleware" and get files that use the `AuthMiddleware` pattern if they don't contain the literal words. Rinnegan uses classical NLP (TF-IDF + truncated SVD + BM25) to find code by meaning — deterministically, zero API calls, fully local.

### What
A `hilo-graph/src/semantic.rs` module with a pure-Rust TF-IDF + BM25 implementation. No neural embeddings, no external API. Exposed via a new MCP tool `vfs_graph_search` and integrated into the signal engine for anchor discovery.

### Implementation

1. **New module: `hilo-graph/src/semantic.rs`** containing:
   - **Tokenization**: split symbols on camelCase/snake_case boundaries, lowercase, deduplicate
   - **TF-IDF**: term frequency × inverse document frequency, computed over all graph nodes
   - **BM25**: Okapi BM25 ranking function for relevance scoring
   - **Fuse**: combine TF-IDF + BM25 results via Reciprocal Rank Fusion (RRF)

2. **Index build**: create semantic index over all graph nodes (file-level: qualifiedName + signature + docstring). Store in-memory — no external database needed.

3. **New MCP tool `vfs_graph_search`**:
   - Input: `{ query: string, limit?: number }`
   - Output: `{ results: [{ file_path, symbols, score }] }`
   - Deterministic: same query + same graph → same results, byte-identical

4. **Integrate into signal engine**: TASK-002's `understand()` should use semantic search for anchor discovery when FTS returns empty/broad results.

5. **Integration test**: on a Go project with multiple "Search" backends, `vfs_graph_search "vector search"` returns the correct implementation files — not just files containing the word "search."

### Files touched
- `hilo-graph/src/semantic.rs` — NEW file (~300-500 lines)
- `hilo-graph/src/lib.rs` — re-export
- `hilo-graph/src/signal.rs` — use semantic.rs for anchor discovery
- `hilo-mcp/src/tools/mod.rs` — add `vfs_graph_search` tool
- `hilo-graph/tests/semantic_test.rs` — NEW test file
- `Cargo.toml` (workspace) — no new deps (pure Rust, stdlib + already-imported crates)

### Acceptance criteria
- [ ] `cargo check --workspace` passes
- [ ] `cargo test --workspace` passes
- [ ] `vfs_graph_search "authentication"` returns `AuthMiddleware` symbols (semantic, not literal)
- [ ] Same query + same graph → same results (determinism test)
- [ ] No external dependencies — pure Rust
- [ ] Index build: <500ms for 10K symbols
- [ ] Query: <50ms for 1000-symbol corpus
- [ ] Edge provenance for semantic results is `Lexical` (BM25) or `Latent` (future semantic expansion)

---

## TASK-004: Determinism Tests — Byte-Identical Graph Output

### Why
None of Hilo's tests guarantee byte-identical output across runs. For a system that feeds AI agents, reproducibility is a safety property. Rinnegan has determinism tests that prove the index and `understand()` output are byte-identical between runs.

### What
A test suite that builds the graph from a controlled corpus, dumps it, rebuilds it, and asserts byte-identical output. Also tests signal engine determinism (TASK-002) and semantic search determinism (TASK-003).

### Implementation

1. **Controlled corpus** in `hilo-graph/tests/fixtures/`:
   - 3-5 small Go/Python/TypeScript files with imports between them
   - Never changes — committed to git
   - Covers: imports, circular imports, test files, entrypoints

2. **Determinism test** in `hilo-graph/tests/determinism_test.rs`:
   ```rust
   #[test]
   fn graph_is_deterministic() {
       let run1 = build_and_dump(&fixtures_dir);
       let run2 = build_and_dump(&fixtures_dir);
       assert_eq!(run1, run2); // byte-identical JSON dump
   }
   ```
   - `build_and_dump()`: create fresh DuckDB in-memory, scan corpus, extract edges, dump to sorted JSON
   - Sort edges before comparing (insertion order may vary but content must be identical)
   - Test must pass with `--release` and `--debug`

3. **Signal engine determinism** (if TASK-002 is done):
   ```rust
   #[test]
   fn signal_engine_is_deterministic() {
       let run1 = signal_understand(&db, "rate limiter");
       let run2 = signal_understand(&db, "rate limiter");
       assert_eq!(run1.text, run2.text);
   }
   ```

4. **Semantic search determinism** (if TASK-003 is done):
   ```rust
   #[test]
   fn semantic_search_is_deterministic() {
       let db = build_graph(&fixtures_dir);
       let run1 = semantic_search(&db, "import helper");
       let run2 = semantic_search(&db, "import helper");
       assert_eq!(run1, run2);
   }
   ```

5. **Edge provenance determinism**: after TASK-001, same source should produce same provenance tags every time.

### Files touched
- `hilo-graph/tests/fixtures/` — NEW directory with controlled source files
- `hilo-graph/tests/determinism_test.rs` — NEW test file
- `hilo-graph/tests/signal_test.rs` — add determinism test (if TASK-002 done)
- `hilo-graph/tests/semantic_test.rs` — add determinism test (if TASK-003 done)

### Acceptance criteria
- [ ] `cargo test --workspace` passes
- [ ] `graph_is_deterministic` passes 10 consecutive runs
- [ ] Test corpus is committed and never changed (immutable fixtures)
- [ ] Tests use in-memory DuckDB (no filesystem pollution)
- [ ] If TASK-002 done: `signal_engine_is_deterministic` passes
- [ ] If TASK-003 done: `semantic_search_is_deterministic` passes

---

## Implementation Order

```
TASK-001 (provenance) → should be done FIRST — all other tasks depend on the new Edge schema
TASK-004 (determinism) → can be done in parallel with 002/003 — fixtures don't need provenance
TASK-002 (signal engine) + TASK-003 (semantic search) → can be done in parallel after 001
```

## Key Design Rules (from AGENTS.md)

1. **Metadata, not injection.** Never modify file content. Metadata lives in xattrs + JSONL inventory.
2. **JSONL for edges.** Append-only, git-friendly, streamable. Source of truth.
3. **DuckDB for queries.** Rebuildable from JSONL. Not source of truth.
4. **Inventory as truth.** `.vfs/manifest.yaml`, `.vfs/graph/edges.jsonl`, `.vfs/backends/mounts.yaml`
5. **MCP as fallback.** When agent tools don't expose xattrs, MCP server provides the tools.

## After Each Task

1. `cargo check --workspace` — must pass
2. `cargo test --workspace` — must pass (31 suites)
3. `cargo fmt --all` — apply
4. `cargo clippy --workspace -- -D warnings` — must pass
5. Commit with `gitreins commit -m "message"` — guards run before commit
