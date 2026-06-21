# WarpFS Coding Tasks

## [x] `warpfs meta --set` — xattr write CLI
- **Priority:** high
- **Model:** deepseek-v4-flash
- **Files:** warpfs-cli/src/commands/meta.rs, warpfs-metadata/src/lib.rs (add set_xattr)
- **AC:** `warpfs meta --set login.go user.vfs.feature auth-module` succeeds, then `getfattr -n user.vfs.feature login.go` returns "auth-module"
- **AC:** `warpfs meta --set --value "multi\nline\nvalue"` round-trips correctly through getfattr
- **AC:** setting on nonexistent file exits with clear error code and message, no panic
- **Notes:** metadata crate has `set_xattr` function; wire it to CLI with clap args `--set` and `--value`

## [x] `warpfs graph related <path>` — query graph edges for a file
- **Priority:** high
- **Model:** deepseek-v4-flash
- **Files:** warpfs-cli/src/commands/graph.rs, warpfs-graph/src/graph.rs
- **AC:** `warpfs graph related src/main.rs` prints edges where from=src/main.rs (imports, imported_by, etc.)
- **AC:** `warpfs graph related --relation imports src/main.rs` filters to only 'imports' edges
- **AC:** `warpfs graph related nonexistent.rs` exits 1 with "not found in graph"
- **Notes:** DuckDB query: `SELECT * FROM edges WHERE from = ?`; add subcommand with --relation filter
- **Result:** Implemented directly. Added `GraphDB::related()` and `GraphDB::file_in_graph()` to warpfs-graph, wired `Related` subcommand with `--relation` filter to warpfs-cli. Build clean, 62/62 tests pass.

## [ ] Phase 2: `warpfs graph impact <path>` — transitive impact analysis
- **Priority:** high
- **Model:** glm-5.2
- **Provider:** zai-glm
- **Files:** warpfs-graph/src/impact.rs (new), warpfs-cli/src/commands/graph.rs
- **AC:** `warpfs graph impact src/main.rs --max-depth 5` prints all files transitively depending on main.rs
- **AC:** `warpfs graph impact src/main.rs --max-depth 1` prints only direct dependents
- **AC:** circular imports do not cause infinite loop — traversal terminates
- **AC:** `warpfs graph impact src/main.rs --format json` outputs valid JSON with {files: [{path, relation, depth}]}
- **AC:** `cargo test -p warpfs_graph` — 3+ new tests for impact traversal (direct, transitive, circular)
- **Notes:** Uses petgraph from edges.jsonl → DuckDB query or in-memory BFS/DFS with depth limit. Scaffold: create warpfs-graph/src/impact.rs with `ImpactResult` struct, `fn compute_impact(path, max_depth) -> Vec<ImpactFile>`

## [ ] Phase 2: DuckDB rule engine — `vfs_rule_check` / `vfs_rule_list`
- **Priority:** high
- **Model:** deepseek-v4-pro
- **Files:** warpfs-graph/src/rules.rs (new), warpfs-mcp/src/server.rs, warpfs-cli/src/commands/graph.rs
- **AC:** MCP tool `vfs_rule_list` returns all rules from manifest (stale-files, untested-critical, transitive-impact)
- **AC:** MCP tool `vfs_rule_check("stale-files")` runs the SQL query and returns matching files
- **AC:** `warpfs graph rule-check stale-files` CLI command works
- **AC:** `warpfs graph rule-list` prints rule names and descriptions
- **AC:** rules with invalid SQL return structured error, not panic
- **Notes:** Rules defined in manifest.yaml §4. Scaffold: load manifest → extract rules[].query → execute via DuckDB → return results. Create warpfs-graph/src/rules.rs with RuleEngine struct.

## [ ] Phase 2: inotify trigger wiring — auto-discover on file write
- **Priority:** medium
- **Model:** glm-5.2
- **Provider:** zai-glm
- **Files:** warpfs-fuse/src/triggers.rs (new), warpfs-triggers/src/lib.rs (new)
- **AC:** Writing to a .go file triggers AST re-parse and edge update within 5 seconds
- **AC:** `warpfs mount --triggers` enables trigger loop; `--no-triggers` disables
- **AC:** Debouncing works — rapid writes within 500ms trigger only one re-parse
- **AC:** Trigger timeout kills hung triggers; error logged, daemon continues
- **Notes:** No FUSE mount yet — inotify on local repo directory. This is the trigger engine WITHOUT the FUSE layer. Use `inotify` crate. Scaffold: create warpfs-triggers crate, add to workspace members.

## [ ] Phase 2: Cross-language edge types — tested_by, documented_by
- **Priority:** medium
- **Model:** deepseek-v4-flash
- **Files:** warpfs-graph/src/parser.rs, warpfs-graph/src/edges.rs
- **AC:** `warpfs graph discover` detects `*_test.go` → `login.go` as `tested_by` edge (reverse direction)
- **AC:** `warpfs graph discover` detects `login.go` → `login_test.go` as `tests` edge
- **AC:** test association works for all 9 languages: *_test.go, test_*.py, *.test.ts, *.spec.ts, *_test.rs, *Test.java, test_*.c, *_test.cpp, *_test.rb
- **AC:** `cargo test -p warpfs_graph` — 9 new tests (one per language pattern)
- **Notes:** Discovery section §7 in manifest has test_association patterns. Extend parser to emit tested_by/tests edges based on filename patterns.

## [ ] Phase 2: Graph deduplication and split-file support
- **Priority:** low
- **Model:** deepseek-v4-flash
- **Files:** warpfs-graph/src/graph.rs, warpfs-metadata/src/inventory.rs
- **AC:** Running `warpfs graph discover` twice does not duplicate edges in edges.jsonl
- **AC:** When edges.jsonl exceeds max_edges_per_file (from manifest), new edges go to edges-001.jsonl, edges-002.jsonl
- **AC:** DuckDB queries span all split files transparently
- **AC:** `warpfs graph stats` reports correct counts across split files
- **Notes:** Append-only but dedup: check if edge already exists before appending. Split support: use glob `edges*.jsonl` in DuckDB queries.

## [ ] Phase 2: `vfs_graph_impact` MCP tool
- **Priority:** medium
- **Model:** deepseek-v4-flash
- **Files:** warpfs-mcp/src/server.rs
- **AC:** MCP tool `vfs_graph_impact` registered in tools/list
- **AC:** `vfs_graph_impact(path="src/main.rs", max_depth=3)` returns {dependents: [{path, relation, depth}]}
- **AC:** passes through to `warpfs_graph::impact::compute_impact()`
- **Notes:** Wire existing impact computation to MCP. Thin adapter layer.

## [ ] Phase 3: S3 backend — read-only mount
- **Priority:** medium
- **Model:** glm-5.2
- **Provider:** zai-glm
- **Files:** warpfs-backends/src/s3.rs (new), warpfs-backends/src/lib.rs
- **AC:** `warpfs backend mount --type s3 --bucket my-bucket --prefix prod/ --at /mnt/vfs/models/` creates virtual directory
- **AC:** `warpfs backend list` shows mounted backends with status
- **AC:** Read from /mnt/vfs/models/file.bin resolves to S3 GET, cached locally
- **AC:** Write to read-only S3 mount returns EACCES (permission denied)
- **AC:** Cache respects TTL from manifest; stale files re-fetched
- **Notes:** No FUSE yet — this is the backend plumbing that provides file content resolution. Use rusoto or aws-sdk-rust. Scaffold: create warpfs-backends crate, add to workspace members.

## [ ] Phase 3: S3 write-through with auto-upload
- **Priority:** medium
- **Model:** deepseek-v4-pro
- **Files:** warpfs-backends/src/s3.rs
- **AC:** Writing to writable S3 mount: file → local cache → S3 upload → blob index update → success
- **AC:** `.vfs/blobs/index.jsonl` updated with {path, hash, backend, uploaded_at} after each write
- **AC:** `user.vfs.backend` xattr set to "s3" on written files
- **AC:** Upload failure returns error to agent, local cache preserved
- **AC:** sha256 hash computed and stored in `user.vfs.hash` xattr
- **Notes:** §13.2 in spec has the flow. This is the write path for S3.

## [ ] Phase 3: Remote git repo backend
- **Priority:** medium
- **Model:** deepseek-v4-flash
- **Files:** warpfs-backends/src/git.rs
- **AC:** `warpfs backend mount --type git --url git@github.com:org/repo.git --ref main --at /mnt/vfs/vendor/repo/` clones repo
- **AC:** Remote git repo auto-pulls on interval (manifest: auto_pull: 3600)
- **AC:** Read-only remote repos reject writes with EACCES
- **AC:** Writable remote repos allow writes to worktree
- **Notes:** Use git2 crate. Git worktree management: clone to ~/.warpfs/worktrees/<name>/.

## [ ] Phase 3: Virtual directory listing
- **Priority:** low
- **Model:** deepseek-v4-flash
- **Files:** warpfs-mcp/src/server.rs, warpfs-core/src/virtual_dir.rs (new)
- **AC:** MCP tool `vfs_list_directory("/project/models/")` returns entries with name, type, backend, size, virtual flag
- **AC:** MCP tool `vfs_resolve_path("/project/models/checkpoint.pt")` returns real_path, backend, cached, sync_status
- **AC:** `warpfs ls /project/models/` CLI command shows virtual directory contents
- **Notes:** Virtual directories are NOT real directories — entries come from backend mounts table. This is a metadata operation, not FUSE.

## Models Reference

| Model | Use | Provider | Fallback |
|-------|-----|----------|----------|
| glm-5.2 | Large new-crate features (3+ new files, complex logic) | zai-glm | openrouter/owl-alpha |
| deepseek-v4-pro | Complex graph/algorithm work, evaluation | deepseek | openrouter/owl-alpha |
| deepseek-v4-flash | Simple CLI wiring, thin adapters, 1-2 file changes | deepseek | — |
| openrouter/owl-alpha | Fallback for any spawn — 1M ctx, 262K output, $0/M token, agentic-optimized | openrouter | — |

**Fallback rule:** If `glm-5.2` rate-limits (429) or `deepseek-v4-pro` hits context limits, retry with `openrouter/owl-alpha` via `--provider openrouter`.

## Verification (Rust — every task)

```bash
cd /home/kara/warpfs
sudo chown -R kara:kara . 2>/dev/null
cargo build 2>&1
cargo test 2>&1
PATH="/home/kara/.cargo/bin:/home/kara/go/bin:$PATH" bash .git/hooks/pre-commit 2>&1
```

## Commit Convention

```
feat(<crate>): <brief description>

Co-authored-by: wojons <wojonstech@gmail.com>
```

Crate name matches Cargo.toml `name` field (underscores): warpfs_core, warpfs_graph, warpfs_metadata, warpfs_cli, warpfs_mcp, warpfs_backends, warpfs_triggers
