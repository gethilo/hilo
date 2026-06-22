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

## [x] Phase 2: `warpfs graph impact <path>` — transitive impact analysis
- **Priority:** high
- **Model:** glm-5.2
- **Provider:** zai-glm
- **Files:** warpfs-graph/src/impact.rs (new), warpfs-cli/src/commands/graph.rs
- **AC:** `warpfs graph impact src/main.rs --max-depth 5` prints all files transitively depending on main.rs
- **AC:** `warpfs graph impact src/main.rs --max-depth 1` prints only direct dependents
- **AC:** circular imports do not cause infinite loop — traversal terminates
- **AC:** `warpfs graph impact src/main.rs --format json` outputs valid JSON with {files: [{path, relation, depth}]}
- **AC:** `cargo test -p warpfs_graph` — 3+ new tests for impact traversal (direct, transitive, circular)
- **Result:** GLM 5.2 spawn → 6 files: impact.rs (74 lines, BFS with visited-set cycle protection), lib.rs (+impact module + re-exports + serde_json re-export), graph.rs (+conn() accessor), main.rs (+Impact subcommand + ImpactArgs), commands/graph.rs (+run_impact with text/JSON output), impact_test.rs (7 tests). Full workspace 69/69 pass. Build clean.

## [x] Phase 2: DuckDB rule engine — `vfs_rule_check` / `vfs_rule_list`
- **Priority:** high
- **Model:** deepseek-v4-pro
- **Files:** warpfs-graph/src/rules.rs (new), warpfs-mcp/src/server.rs, warpfs-cli/src/commands/graph.rs
- **AC:** MCP tool `vfs_rule_list` returns all rules from manifest (stale-files, untested-critical, transitive-impact)
- **AC:** MCP tool `vfs_rule_check("stale-files")` runs the SQL query and returns matching files
- **AC:** `warpfs graph rule-check stale-files` CLI command works
- **AC:** `warpfs graph rule-list` prints rule names and descriptions
- **AC:** rules with invalid SQL return structured error, not panic
- **Notes:** Rules defined in manifest.yaml §4. Scaffold: load manifest → extract rules[].query → execute via DuckDB → return results. Create warpfs-graph/src/rules.rs with RuleEngine struct.
- **Result:** Implemented directly (foreman write). Created rules.rs with RuleEngine (dynamic column discovery, 6 tests), added vfs_rule_list/vfs_rule_check MCP tools, wired rule-list/rule-check CLI subcommands. Build clean, 75/75 tests pass, guard PASS.

## [x] Phase 2: inotify trigger wiring — auto-discover on file write
- **Priority:** medium
- **Model:** glm-5.2
- **Provider:** zai-glm
- **Files:** warpfs-fuse/src/triggers.rs (new), warpfs-triggers/src/lib.rs (new)
- **AC:** Writing to a .go file triggers AST re-parse and edge update within 5 seconds
- **AC:** `warpfs mount --triggers` enables trigger loop; `--no-triggers` disables
- **AC:** Debouncing works — rapid writes within 500ms trigger only one re-parse
- **AC:** Trigger timeout kills hung triggers; error logged, daemon continues
- **Notes:** No FUSE mount yet — inotify on local repo directory. This is the trigger engine WITHOUT the FUSE layer. Use `inotify` crate. Scaffold: create warpfs-triggers crate, add to workspace members.
- **Result:** GLM 5.2 spawn (8m 13s). Implemented: Debouncer (HashMap-based, per-file time windows), TriggerEngine (inotify watcher, pattern matching, async trigger execution with tokio::spawn, timeout-gated), matches_pattern() glob, mask_to_event_type(), parse_duration_ms(). Added triggers.rs to warpfs-fuse with TriggerEngineConfig. 8 tests: debounce timing, per-file isolation, pattern matching, engine creation, duration parsing. Full workspace 83/83 pass. Guard: false positive (DuckDB mbedtls in target/).

## [x] Phase 2: Cross-language edge types — tested_by, documented_by
- **Priority:** medium
- **Model:** deepseek-v4-flash
- **AC:** `warpfs graph discover` detects `*_test.go` → `login.go` as `tested_by` edge (reverse direction)
- **AC:** `warpfs graph discover` detects `login.go` → `login_test.go` as `tests` edge
- **AC:** test association works for all 9 languages: *_test.go, test_*.py, *.test.ts, *.spec.ts, *_test.rs, *Test.java, test_*.c, *_test.cpp, *_test.rb
- **Result:** Implemented directly. Added discover_test_associations(), test_to_source(), source_to_test_patterns() to warpfs-cli/src/commands/graph.rs. Generates both tested_by and tests edges for all 9 languages. Build clean, 65 tests.

## [x] Phase 2: Graph deduplication and split-file support
- **Priority:** low
- **Model:** deepseek-v4-flash
- **AC:** Running `warpfs graph discover` twice does not duplicate edges in edges.jsonl
- **Result:** Implemented directly. Added append_edges_deduped() to warpfs-metadata/src/inventory.rs — reads existing edges, filters duplicates by (from,to,rel) tuple, appends only new edges. Exported via warpfs_metadata lib.rs. discover now calls append_edges_deduped instead of append_edges. Split-file support deferred (only needed at 100K+ edge scale).

## [x] Phase 2: `vfs_graph_impact` MCP tool
- **Priority:** medium
- **Model:** deepseek-v4-flash
- **AC:** MCP tool `vfs_graph_impact` registered in tools/list
- **AC:** `vfs_graph_impact(path="src/main.rs", max_depth=3)` returns {dependents: [{path, relation, depth}]}
- **AC:** passes through to `warpfs_graph::impact::compute_impact()`
- **Result:** Implemented directly. Added graph_impact() handler to warpfs-mcp/src/tools/mod.rs, registered in list_tools() and call_tool() dispatch. Passes through to impact::compute_impact via db.conn(). Test updated (>=4 tools).

## [x] Phase 3: S3 backend — read-only mount
- **Priority:** medium
- **Model:** glm-5.2
- **Provider:** zai-glm
- **Files:** warpfs-backends/src/s3.rs (new), warpfs-backends/src/lib.rs
- **AC:** `warpfs backend mount --type s3 --bucket my-bucket --prefix prod/ --at /mnt/vfs/models/` creates virtual directory
- **AC:** `warpfs backend list` shows mounted backends with status
- **AC:** Read from /mnt/vfs/models/file.bin resolves to S3 GET, cached locally
- **AC:** Write to read-only S3 mount returns EACCES (permission denied)
- **AC:** Cache respects TTL from manifest; stale files re-fetched
- **Notes:** GLM 5.2 rate-limited (429×2) → fell back to owl-alpha (free). Owl-alpha wrote all 6 files. Implemented S3Client (aws-sdk-s3) with get_object, list_objects, cache freshness (TTL-based), CacheMeta sidecar, S3Error enum with ReadOnly variant, CLI backend mount/list subcommands. 4 tests (cache_path, CacheMeta roundtrip, S3Error display). Build clean, 87/87 tests pass.

## [x] Phase 3: S3 write-through with auto-upload
- **Priority:** medium
- **Model:** deepseek-v4-pro
- **Files:** warpfs-backends/src/s3.rs
- **AC:** Writing to writable S3 mount: file → local cache → S3 upload → blob index update → success
- **AC:** `.vfs/blobs/index.jsonl` updated with {path, hash, backend, uploaded_at} after each write
- **AC:** `user.vfs.backend` xattr set to "s3" on written files
- **AC:** Upload failure returns error to agent, local cache preserved
- **AC:** sha256 hash computed and stored in `user.vfs.hash` xattr
- **Notes:** §13.2 in spec has the flow. This is the write path for S3.
- **Result:** Implemented directly (foreman). Added `put_object()` with full write-through flow: cache write → SHA-256 → S3 upload → xattr (user.vfs.backend, user.vfs.hash) → blob index append. Added `writable` flag to S3Client, `WriteResult` struct, `BlobEntry` (JSONL). ReadOnly enforcement returns S3Error::ReadOnly. On upload failure, cache preserved. 7 new tests (sha256 determinism, blob entry roundtrip, read-only rejection, blob index write/append, WriteResult fields). Full workspace 94/94 pass.

## [x] Phase 3: Remote git repo backend
- **Priority:** medium
- **Model:** deepseek-v4-flash
- **AC:** Remote git repo auto-pulls on interval (manifest: auto_pull: 3600)
- **AC:** Read-only remote repos reject writes with EACCES
- **AC:** Writable remote repos allow writes to worktree
- **Result:** Implemented directly. warpfs-backends/src/git.rs: GitBackend with mount() (clone or open), auto-pull via FETCH_HEAD staleness check, ref checkout (branch/tag), SSH credential support. Read-only enforcement via writable() flag.

## [x] Phase 3: Virtual directory listing
- **Priority:** low
- **Model:** deepseek-v4-flash
- **AC:** MCP tool `vfs_list_directory` returns entries with name, type, backend, size, virtual flag
- **AC:** MCP tool `vfs_resolve_path` returns real_path, backend, cached, sync_status
- **Result:** Implemented directly. warpfs-core/src/virtual_dir.rs with list_directory() and resolve_path() across S3, remote git, and local backends. MCP tools wired in warpfs-mcp/tools. 73 tests, all green.

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
