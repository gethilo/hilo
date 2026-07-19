# Hilo — Implementation Specification

**Status:** Spec Phase  
**Date:** June 2026  
**Repo:** [gethilo/hilo](https://github.com/gethilo/hilo)  
**Language:** Pure Rust  
**Design Reference:** [Hermes VFS Design v2](https://gethilo.github.io/hilo/hermes-vfs-design.html)

---

## 1. Mission

Hilo is a virtual filesystem for AI coding agents. Clone repos → mount with Hilo → point agent workspace at the mount. Files stay pristine. Metadata — relationships, graph edges, backend routing, feature tags — lives in Linux extended attributes (xattrs) and inventory files. Agents query metadata through native tools (`getfattr`, `stat`) or via an MCP server. File content is never modified.

---

## 2. Architecture — Three Layers

```
┌─────────────────────────────────────────────────────────────┐
│  LAYER 1: Interface                                        │
│  How agents interact with the VFS                          │
│  FUSE | MCP server | CLI shim | UniFFI bindings            │
├─────────────────────────────────────────────────────────────┤
│  LAYER 2: Metadata Engine                                  │
│  Graph edges, inventory files, backend routing, xattrs      │
│  xattrs | Inventory files (.vfs/*) | DuckDB query engine   │
├─────────────────────────────────────────────────────────────┤
│  LAYER 3: Backend Storage                                  │
│  Where bytes actually live                                  │
│  Git | S3 | Local disk | Remote repos                      │
└─────────────────────────────────────────────────────────────┘
```

**Key insight:** Metadata lives ALONGSIDE files in xattrs — never injected into file content. Files read by the agent are byte-for-byte identical to what's on disk. Metadata is queried separately, explicitly, through tools the agent already has or through an MCP server.

---

## 3. Language & Distribution

**Pure Rust.** Single binary. Single repo.

```bash
cargo install hilo
hilo mount /mnt/vfs/workspace    # FUSE daemon
hilo serve --mcp                 # MCP server (same binary)
hilo meta login.go               # CLI shim (same binary)
hilo graph impact login.go       # Graph queries (same binary)
hilo plugin load ./scanner.wasm  # Plugin loader (same binary)
```

### 3.1 Key Crates

| Crate | Version | Role |
|---|---|---|
| `fuser` | 0.16.0 | FUSE daemon — pure Rust, no libfuse required |
| `petgraph` | 0.8.2 | Dependency graph — used in Rust's compiler |
| `tree-sitter` | latest | AST parsing — official Rust bindings, zero-copy |
| `xattr` | latest | Extended attributes — pure Rust |
| `inotify` | latest | File event watching — idiomatic kernel wrapper |
| `duckdb` | 0.10+ | Analytical query engine — official DuckDB bindings |
| `tokio` | 1.x | Async runtime for concurrent FUSE request handling |
| `rayon` | 1.x | Parallel graph traversal |
| `extism` | latest | Wasm plugin runtime — sandboxed, multi-language PDK |
| `uniffi` | 0.28+ | FFI bindings generator — Go, Python, Kotlin, Swift |
| `rmcp` | latest | MCP protocol implementation |
| `clap` | 4.x | CLI argument parsing |
| `serde` | 1.x | Serialization (manifest, JSONL, MCP responses) |
| `git2` | latest | Git backend operations |
| `rusoto` / `aws-sdk` | latest | S3 backend operations |

### 3.2 Extensibility Surfaces

| Surface | Technology | For |
|---|---|---|
| **MCP protocol** | stdio/SSE JSON-RPC | Agents talk to the VFS |
| **Wasm plugins** | extism + wasmtime | Custom hooks, new edge types, new triggers |
| **UniFFI bindings** | Generated from `.udl` | Go/Python/Kotlin programs calling VFS functions |

---

## 4. The Manifest — Single Source of Truth

All configuration lives in `.vfs/manifest.yaml`. One file. Everything the VFS knows, watches, computes, and exposes is declared here.

### 4.1 Complete Schema

```yaml
# .vfs/manifest.yaml
version: 2

# ── Project identity ──
project:
  name: "my-project"
  description: "Description surfaced to agents"

# ── Interfaces ──
interfaces:
  fuse:
    enabled: true
    mount_point: /mnt/vfs/project
    allow_other: false
    direct_io: false
    auto_unmount: true
  ninep:
    enabled: false
    listen: "0.0.0.0:5640"
  cli:
    enabled: true
  mcp:
    enabled: true
    transport: stdio          # or: sse
    port: 8766
  golib:
    enabled: false

# ── Multi-repo workspace ──
repos:
  - name: auth-service
    url: git@github.com:org/auth-service.git
    ref: main
    at: /mnt/vfs/workspace/auth-service/
    writable: true
    auto_pull: true
    manifest: .vfs/repo-manifests/auth.yaml   # per-repo overrides (optional)
  - name: shared-lib
    url: git@github.com:org/shared-lib.git
    ref: v2.1.0
    at: /mnt/vfs/workspace/shared-lib/
    writable: false            # read-only dependency
    auto_pull: true

# ── Backends ──
backends:
  s3:
    - bucket: my-project-models
      prefix: prod/
      at: /project/models/
      region: us-east-1
      cache: /tmp/vfs-cache/models/
      ttl: 3600
      writable: false
      lazy_load: true
  remote:
    - url: git@github.com:org/shared-lib.git
      at: /project/vendor/shared-lib/
      ref: main
      cache: /tmp/vfs-cache/shared-lib/
      auto_pull: 3600
  local:
    - path: /data/datasets/
      at: /project/data/
      writable: true

# ── Metadata namespaces (set as xattrs on matching files) ──
metadata:
  namespaces:
    - feature         # user.vfs.feature → "auth-module"
    - purpose         # user.vfs.purpose → human description
    - backend         # user.vfs.backend → "git", "s3", "local"
    - hash            # user.vfs.hash → "sha256:abc123..."
    - origin          # user.vfs.origin → git ref, S3 key
    - relations       # user.vfs.relations → "imports:a.go|tested_by:a_test.go"
    - impact          # user.vfs.impact → pre-computed blast radius
    - complexity      # user.vfs.complexity → "cyclomatic=14,loc=142"
    - risk            # user.vfs.risk → "critical-path", "low", "generated"
    - last_tested     # user.vfs.last_tested → ISO timestamp
    - last_modified   # user.vfs.last_modified → ISO timestamp
    - cache_status    # user.vfs.cache_status → "hit", "miss", "stale"
    - review_status   # user.vfs.review_status → "needs-review", "approved"
  auto:
    hash: true
    last_modified: true
    complexity: true

# ── Graph & Impact ──
graph:
  edges: .vfs/graph/edges.jsonl
  blob_index: .vfs/blobs/index.jsonl
  duckdb_cache: .vfs/graph/graph.duckdb
  auto_discover:
    enabled: true
    languages: [go, python, typescript, rust, javascript, java, c, cpp, ruby]
    on_first_read: true
    on_write: true
    on_mount: false
  impact:
    enabled: true
    max_depth: 5
    parallel: true            # rayon parallel traversal
  cross_repo: true            # parse imports across repo boundaries
  max_edges_per_file: 100000
  deduplicate: true
  extensions:                 # manually declared edges
    - name: docs
      pattern: "docs/**/*.md → src/**/*.go"
      relation: "documented_by"

# ── Permissions ──
permissions:
  rules:
    - paths: [".vfs/**"]
      mode: 0444
    - paths: [".git/**", ".gitignore", ".gitattributes", ".gitmodules"]
      mode: 0444
    - paths: ["**/go.sum", "**/Cargo.lock", "**/package-lock.json",
              "**/yarn.lock", "**/pnpm-lock.yaml", "**/Gemfile.lock"]
      mode: 0444
    - paths: ["**/*.pb.go", "**/*.gen.go", "**/*.generated.*",
              "**/generated/**", "**/__pycache__/**"]
      mode: 0444
    - paths: ["**/vendor/**", "**/node_modules/**"]
      mode: 0444
    - paths: [".github/workflows/**", ".gitlab-ci.yml", "Dockerfile",
              "docker-compose*.yml"]
      mode: 0444
    - paths: ["docs/**", "*.md", "README.md"]
      mode: 0644
    - paths: ["src/**", "lib/**", "pkg/**", "cmd/**", "internal/**"]
      mode: 0644
    - paths: ["*.toml", "*.yaml", "*.yml", "*.json"]
      mode: 0644
      allow_delete: false
  default_mode: 0644
  backends:
    - name: shared-lib
      mode: 0444              # entire mounted dependency read-only

# ── Triggers (inotify-wired) ──
triggers:
  - name: update-graph
    when: "*"
    on: [write, delete]
    run: parse-and-diff       # built-in: tree-sitter parse → diff AST → update edges
    async: true
    timeout: 5s
  - name: lint-go
    when: "*.go"
    on: write
    run: "golangci-lint run {{ .FilePath }}"
    async: true
    timeout: 30s
    on_failure: warn
  - name: auto-test
    when: "src/**/*.go"
    on: write
    run: "go test ./{{ .ModulePath }}/..."
    async: true
    timeout: 60s
    debounce: 2s
    on_success:
      set_xattr: "user.vfs.last_tested={{ .Timestamp }}"
  - name: sync-s3
    when: "/project/models/*"
    on: write
    run: upload-to-backend    # built-in: upload to S3, update blob index
    async: true
    timeout: 120s

# ── Query Rules ──
rules:
  - name: stale-files
    description: "Files changed since last test pass"
    query: |
      SELECT path, last_modified, last_tested
      FROM file_registry
      WHERE last_modified > last_tested OR last_tested IS NULL
      ORDER BY last_modified DESC
  - name: untested-critical
    description: "Critical path files with no tests"
    query: |
      SELECT e.to AS file, COUNT(*) AS dependents
      FROM read_json_auto('.vfs/graph/edges.jsonl') e
      WHERE e.rel = 'imports'
        AND e.to NOT IN (
          SELECT from FROM read_json_auto('.vfs/graph/edges.jsonl')
          WHERE rel = 'tested_by'
        )
      GROUP BY e.to HAVING COUNT(*) > 5
      ORDER BY dependents DESC
  - name: transitive-impact
    description: "Full dependency chain for a file"
    query: |
      WITH RECURSIVE chain AS (
        SELECT to, rel, 1 AS depth
        FROM read_json_auto('.vfs/graph/edges.jsonl')
        WHERE from = $PATH
        UNION ALL
        SELECT e.to, e.rel, c.depth + 1
        FROM read_json_auto('.vfs/graph/edges.jsonl') e
        JOIN chain c ON e.from = c.to
        WHERE c.depth < $MAX_DEPTH
      )
      SELECT DISTINCT to, MIN(depth) AS depth FROM chain
      ORDER BY depth, to

# ── Plugins ──
plugins:
  - name: sql-scanner
    wasm: .vfs/plugins/sql-scanner.wasm
    hooks:
      - on: file_write
        languages: [go]
        priority: 10
    provides:
      edge_types: [security_audit, compliance_check]
      metadata_namespaces: [security.sql_risk, security.owasp_category]

# ── Auto-Discovery ──
discovery:
  feature_inference:
    enabled: true
    strategy: directory       # src/auth/ → feature=auth-module
    override_file: .vfs/features/tags.yaml
  test_association:
    enabled: true
    patterns:
      - "*_test.go"
      - "test_*.py"
      - "*.test.ts"
      - "*.spec.ts"
  generated_detection:
    enabled: true
    markers:
      - header: "DO NOT EDIT"
      - header: "auto-generated"
      - header: "Code generated by"
      - path: "*.pb.go"
      - path: "*.gen.go"
      - path: "*.generated.*"

# ── Sandbox (optional) ──
sandbox:
  enabled: false
  engine: bubblewrap
  isolate_network: true
  isolate_pid: true
  read_only_root: true
  writable_paths:
    - /tmp
    - /workspace

# ── Performance ──
performance:
  cache:
    path: .vfs/cache/
    max_size: 1GB
  fuse:
    attr_timeout: 1.0
    entry_timeout: 1.0
    max_read: 131072
    max_write: 131072
  duckdb:
    threads: 4
    memory_limit: 512MB
  triggers:
    debounce_default: 500ms
    max_concurrent: 8
```

---

## 5. Permission Model — FUSE-Enforced

Permissions are NOT advisory. The VFS sets FUSE mode bits that the Linux kernel enforces. When the agent tries to write to a read-only file, the kernel returns `EACCES`. The model sees a real filesystem error — no injected warnings, no special tool calls.

### 5.1 Enforcement Path

```
AGENT: echo "change" > /mnt/vfs/workspace/.gitignore
KERNEL: checks FUSE mode bits → file is 0444
KERNEL: returns -EACCES (Permission denied)
AGENT: "Permission denied" — learns .gitignore is protected
```

### 5.2 Default Protections

| Path | Mode | Rationale |
|---|---|---|
| `.vfs/**` | 0444 | VFS internals |
| `.git/**`, `.gitignore` | 0444 | Git config — use git commands |
| `**/*.sum`, `**/*.lock` | 0444 | Dependency pinning — use package manager |
| `**/*.pb.go`, `**/*.gen.go` | 0444 | Generated code — edit the source |
| `**/vendor/**`, `**/node_modules/**` | 0444 | Package manager territory |
| `.github/workflows/**` | 0444 | CI config — change via PR |
| `src/**`, `lib/**`, `cmd/**` | 0644 | Agent's workspace — read-write |
| Mounted dependency repos | 0444 | External deps pinned to tags |

---

## 6. Multi-Repo Workspace

A top-level VFS mount that is NOT itself a git repo. Multiple repos, S3 buckets, and local paths mounted as peers in a unified tree.

```
/mnt/vfs/workspace/          ← top-level VFS mount (not a git repo)
├── auth-service/            ← git@github.com:org/auth-service.git (main)
├── payment-service/         ← git@github.com:org/payment-service.git (develop)
├── shared-lib/              ← git@github.com:org/shared-lib.git (v2.1.0, read-only)
├── models/                  ← S3 bucket (not a repo)
├── docs/                    ← git@github.com:org/docs.git (main)
├── .vfs/                    ← TOP-LEVEL inventory (cross-repo graph)
│   ├── manifest.yaml        ← declares all repos & backends
│   ├── graph/
│   │   └── edges.jsonl      ← CROSS-REPO edges
│   ├── backends/
│   │   └── mounts.yaml
│   ├── graph.duckdb          ← DuckDB persisted indexes
│   └── plugins/
└── datasets/                ← Local path mount
```

### 6.1 Cross-Repo Graph Edges

Edges that span repo boundaries are flagged as `external:`:

```bash
$ getfattr -n user.vfs.relations auth-service/src/handler.go
user.vfs.relations="imports:auth-service/src/types.go|external:shared-lib/pkg/utils.go"

$ getfattr -n user.vfs.impact shared-lib/pkg/utils.go
user.vfs.impact="direct:shared-lib/pkg/auth.go|external:auth-service/src/handler.go,payment-service/src/checkout.rs"
```

### 6.2 Mount Ordering

Repos with dependencies must be mounted first so their graphs are available when dependents parse. The VFS can topologically sort repos by their import graph when `auto_dependency_order: true`.

### 6.3 Git Worktree Model

Each repo gets its own git worktree under `~/.hilo/worktrees/<name>/`. The VFS manages these — clone, pull, checkout ref. The agent never touches the worktree directly.

---

## 7. Trigger System

inotify-wired. The core loop that keeps all metadata current. Every file event fires triggers.

### 7.1 Trigger Loop

```
FILE WRITTEN → inotify event →
  1. tree-sitter parse AST of changed file
  2. Diff against cached AST → changed edges detected
  3. petgraph traversal → find impacted files (transitive, up to max_depth)
  4. Parallel setxattr (user.vfs.impact) on all impacted files
  5. Append to edges.jsonl
  6. Set user.vfs.last_modified → now
  7. Fire priority-ordered plugin hooks
  8. Fire user-defined triggers (lint, test, sync)
```

### 7.2 Built-in Trigger Types

| Trigger | Name | Priority | Description |
|---|---|---|---|
| `parse-and-diff` | update-graph | 0 | Always runs. Re-parses AST, computes impact |
| Command template | user-defined | 10-30 | External commands with `{{ .FilePath }}`, `{{ .ModulePath }}` |
| `upload-to-backend` | sync-s3 | 20 | Uploads written file to S3 backend |

### 7.3 Trigger Features

- **Debouncing:** `debounce: 2s` — wait after last event before firing
- **Async execution:** `async: true` — don't block the write response
- **Timeouts:** `timeout: 60s` — kill if hung
- **Success/failure actions:** `on_success: { set_xattr: "..." }`, `on_failure: warn`
- **Concurrency limiting:** `max_concurrent: 8` — cap simultaneous trigger execution

---

## 8. Plugin System (extism wasm)

Plugins are `.wasm` modules loaded from `.vfs/plugins/`. Written in any language with an extism PDK (Rust, Go, Python, JS, C, Zig). The daemon hot-loads plugins on manifest change.

### 8.1 Host Functions (exposed to plugins)

```rust
fn get_file_content(path: String) -> String;
fn get_xattr(path: String, key: String) -> String;
fn set_xattr(path: String, key: String, value: String);
fn add_edge(from: String, to: String, relation: String);
fn query_graph(sql: String) -> String;
fn emit_warning(path: String, message: String);
```

Plugins CANNOT modify file content (FUSE-enforced) or access the host (wasm sandbox).

### 8.2 Plugin Lifecycle

```
1. Plugin compiled to .wasm (any extism-supported language)
2. .wasm placed in .vfs/plugins/
3. Declared in manifest (name, hooks, provided features)
4. Daemon detects manifest change → hot-loads
5. Hooks fire on matching file events
```

### 8.3 Example: SQL Injection Scanner

```rust
#[plugin_fn]
pub fn on_file_write(path: String, ast_json: String) -> FnResult<Vec<HookResult>> {
    let ast: AST = serde_json::from_str(&ast_json)?;
    let mut results = Vec::new();
    for node in ast.walk() {
        if node.kind() == "call_expression" && node.contains("Query") {
            results.push(HookResult::Edge {
                from: path.clone(),
                to: format!("audit/sql-{}.md", sanitize(&path)),
                relation: "security_audit".into(),
            });
            results.push(HookResult::SetXattr {
                path: path.clone(),
                key: "security.sql_risk".into(),
                value: "high".into(),
            });
        }
    }
    Ok(results)
}
```

---

## 9. AST & Graph Engine

### 9.1 tree-sitter — AST Parsing

Pure Rust tree-sitter bindings. Zero-copy AST traversal — nodes are `&[u8]` slices into the source. Languages loaded on demand:

| Language | Grammar | Status |
|---|---|---|
| Go | tree-sitter-go | Production |
| Python | tree-sitter-python | Production |
| TypeScript | tree-sitter-typescript | Production |
| Rust | tree-sitter-rust | Production |
| JavaScript | tree-sitter-javascript | Production |
| Java | tree-sitter-java | Production |
| C | tree-sitter-c | Production |
| C++ | tree-sitter-cpp | Production |
| Ruby | tree-sitter-ruby | Production |

### 9.2 petgraph — Dependency Graph

In-memory directed graph. Node = file path, edge = relationship type. Parallel traversal via rayon for impact computation across independent subtrees.

### 9.3 Edge Types

```
Built-in: imports, imported_by, tested_by, tests, documented_by, documents
Plugin-extensible: references_db_table, security_audit, compliance_check, threat_model
```

### 9.4 Inventory Storage

Edges stored in append-only JSONL (`.vfs/graph/edges.jsonl`). One JSON object per line. Git-friendly (diffs cleanly). Can be split across multiple files (`edges-001.jsonl`, `edges-002.jsonl`) when exceeding `max_edges_per_file`.

---

## 10. DuckDB Query Engine

DuckDB reads JSONL inventory files directly — no import step required. The `.vfs/graph/graph.duckdb` file persists indexes and compiled query plans.

### 10.1 Direct JSONL Queries

```sql
-- No import needed. Just query the inventory file in-place.
SELECT e.to, e.rel, COUNT(*) AS weight
FROM read_json_auto('.vfs/graph/edges.jsonl') e
WHERE e.from = 'src/auth/login.go'
GROUP BY e.to, e.rel
ORDER BY weight DESC;
```

### 10.2 Cross-Inventory Joins

```sql
SELECT e.from, e.to, e.rel, b.backend, b.cache
FROM read_json_auto('.vfs/graph/edges.jsonl') e
JOIN read_json_auto('.vfs/blobs/index.jsonl') b
  ON e.to = b.path;
```

### 10.3 Named Rules

Rules defined in the manifest as SQL queries. Invoked via MCP as `vfs_rule_check("rule-name")`. Results returned as structured data.

---

## 11. MCP Server

Served by the same binary. `hilo serve --mcp` starts the MCP server (stdio or SSE transport).

### 11.1 Complete Tool Set

```
File Metadata:
  vfs_get_metadata(path, keys?)         → {path, xattrs, size, mtime, hash, backend}
  vfs_set_metadata(path, key, value)    → {success, previous_value?}

Graph Queries:
  vfs_graph_related(path, relations?, max_depth?) → {files: [{path, relation, depth}]}
  vfs_graph_impact(path, max_depth?)    → {dependents: [{path, relation, depth}]}
  vfs_graph_module(module)              → {files, edges_count, test_coverage_pct}
  vfs_graph_stats()                     → {total_files, total_edges, most_connected, orphans}
  vfs_graph_untested()                  → {files: [path]}

Virtual Directory:
  vfs_list_directory(path)              → {entries: [{name, type, backend?, size?, virtual}]}
  vfs_resolve_path(path)                → {real_path, backend, cached, sync_status}

Backend Operations:
  vfs_backend_status(path)              → {backend, cache_hit, cache_path, remote_url, last_synced}
  vfs_sync_backend(path)                → {synced_files, errors}

Rules:
  vfs_rule_check(rule_name)             → [{result}]
  vfs_rule_list()                       → [{name, description}]
```

---

## 12. FFI (UniFFI)

Language bindings generated from a `.udl` interface definition. Bindings are OUTPUT (not source) — generated during build.

### 12.1 Exported Functions

```idl
namespace hilo {
    Metadata vfs_get_metadata(string path);
    void vfs_set_metadata(string path, string key, string value);
    sequence<GraphEdge> vfs_graph_related(string path, sequence<string>? relations, u32 max_depth);
    sequence<ImpactFile> vfs_graph_impact(string path, u32 max_depth);
    GraphStats vfs_graph_stats();
    BackendInfo vfs_resolve_backend(string path);
    sequence<RuleResult> vfs_rule_check(string rule_name);
    sequence<DirEntry> vfs_list_directory(string path);
}
```

### 12.2 Generated Targets

| Language | Output | Usage |
|---|---|---|
| Go | `hilo-go/vfs/` | `import "github.com/gethilo/hilo-go/vfs"` |
| Python | `hilo/` wheel | `import hilo` |
| Kotlin | `hilo-kotlin/` | JVM/Swift interop |
| Swift | `Hilo/` | Native Apple platforms |

---

## 13. Virtual Backends

Paths in the mount that resolve to external storage. The agent sees a local file. The VFS handles routing, caching, and upload tracking transparently.

### 13.1 Backend Types

| Type | Storage | Write Behavior | Cache |
|---|---|---|---|
| **Git** | Local git repo | Normal file write → staged in worktree | N/A |
| **S3 (read-only)** | S3 bucket | Writes rejected (0444) | Local cache, TTL expiry |
| **S3 (write-through)** | S3 bucket | Write → local cache → upload to S3 → update blob index | Local cache + remote |
| **Remote git** | Remote git repo | If writable: write to worktree; if read-only: rejected | Local worktree |
| **Local path** | Host filesystem path | Direct write | N/A |

### 13.2 Auto-Upload Flow (S3 write-through)

```
1. Agent writes to /project/models/new-model.bin
2. VFS intercepts:
   ├── Write to local cache
   ├── Compute hash: sha256
   ├── Upload to S3
   ├── Set xattrs: backend, hash, cache_status
   ├── Append to .vfs/blobs/index.jsonl
   └── Return success to agent
3. Agent sees: file written successfully
```

---

## 14. Bubblewrap Sandboxing (Optional)

Bubblewrap wraps the agent's SHELL in a mount namespace. The daemon runs on the host. The agent's process sees only the VFS mount.

### 14.1 Isolation Architecture

```
Host:               hilo-daemon (FUSE, full access)
                        │
                /mnt/vfs/workspace  ← mounted by daemon
                        │
Sandbox:           /workspace  ← bind-mounted read-write
                   / (root)    ← bind-mounted READ-ONLY
                   /tmp        ← isolated tmpfs
                   /proc       ← isolated process space
                        │
                   agent's shell (cd /workspace)
```

### 14.2 Security Properties

| Threat | Without Bubblewrap | With Bubblewrap |
|---|---|---|
| Agent reads /etc/shadow | Possible | Kernel-blocked (file doesn't exist) |
| Agent writes to ~/.ssh | Possible | Kernel-blocked (read-only root) |
| Agent accesses host processes | Possible via /proc | Kernel-blocked (isolated PID namespace) |
| Agent calls home over network | Possible | Blocked if isolate_network: true |

### 14.3 Configuration

```yaml
sandbox:
  enabled: false              # default: off
  engine: bubblewrap
  isolate_network: true
  isolate_pid: true
  read_only_root: true
  writable_paths: [/tmp, /workspace]
```

### 14.4 Availability

| System | Package | User Namespaces |
|---|---|---|
| Ubuntu 24.04+ | `apt install bubblewrap` (0.11.1) | Enabled by default |
| Debian 12+ | `apt install bubblewrap` | Enabled by default |
| Fedora 40+ | Pre-installed (Flatpak dep) | Enabled |
| Arch | `pacman -S bubblewrap` | Enabled |
| Docker/CI | Requires `--privileged` | Often disabled |

---

## 15. Crate Structure

```
hilo/                         ← single repo, pure Rust
├── hilo-core/                # Manifest parsing, config types, shared state
├── hilo-fuse/                # FUSE daemon (fuser 0.16.0), inotify wiring
├── hilo-metadata/            # xattr read/write, inventory file I/O
├── hilo-graph/               # tree-sitter AST parsing, petgraph traversal, impact
├── hilo-backends/            # Git (git2), S3 (rusoto/aws-sdk), remote, local
├── hilo-triggers/            # Trigger engine, debouncing, async execution
├── hilo-permissions/         # Mode bit enforcement, FUSE permission callbacks
├── hilo-plugins/             # extism wasm runtime, host function registry
├── hilo-cli/                 # CLI shim (hilo mount|meta|graph|plugin|serve)
├── hilo-mcp/                 # MCP server (rmcp), tool implementations
└── hilo-ffi/                 # UniFFI .udl interface, generates bindings
```

---

## 16. Inventory Files

All stored in `.vfs/` at the mount root (workspace-level for multi-repo, repo-level for single-repo).

| File | Format | Purpose |
|---|---|---|
| `.vfs/manifest.yaml` | YAML | Master configuration |
| `.vfs/graph/edges.jsonl` | JSONL | Graph edges — append-only, streamable |
| `.vfs/graph/graph.duckdb` | DuckDB | Persisted indexes and query plans |
| `.vfs/backends/mounts.yaml` | YAML | Virtual mount table |
| `.vfs/features/tags.yaml` | YAML | Feature/module groupings |
| `.vfs/blobs/index.jsonl` | JSONL | Content-addressed blob index |
| `.vfs/plugins/` | Directory | Wasm plugin modules |
| `.vfs/cache/` | Directory | Local file cache |

---

## 17. Agent Experience

```bash
# Agent opens workspace
$ cd /mnt/vfs/workspace

# Sees unified tree (multiple repos + S3 + local paths)
$ ls
auth-service/  payment-service/  shared-lib/  models/  docs/  datasets/

# Reads a file normally — pristine bytes
$ cat auth-service/src/handler.go
(func Handler(...) — exact git blob content)

# Asks: "what's connected?"
$ getfattr -n user.vfs.relations auth-service/src/handler.go
user.vfs.relations="imports:types.go,middleware.go|external:shared-lib/pkg/utils.go"

# Asks: "what breaks if I change this?"
$ getfattr -n user.vfs.impact shared-lib/pkg/utils.go
user.vfs.impact="direct:pkg/auth.go|external:auth-service/src/handler.go,payment-service/src/checkout.rs"

# Tries to edit .gitignore → kernel-enforced denial
$ echo "*.log" > .gitignore
bash: .gitignore: Permission denied

# Checks risk before editing
$ getfattr -n user.vfs.risk auth-service/src/middleware.go
user.vfs.risk="critical-path"

# Checks freshness
$ getfattr -n user.vfs.last_tested auth-service/src/login.go
user.vfs.last_tested="2026-06-14T09:33:14"
# Model compares with last_modified → STALE. Needs re-test.

# Invokes a rule through MCP
# vfs_rule_check("untested-critical")
# → Returns: "3 critical path files have no tests: ..."
```

---

## 18. Phase 1 Prototype

**Target:** 2-3 days  
**Deliverable:** CLI shim + xattr read/write + JSONL graph inventory + MCP server

### 18.1 Scope

- `hilo init` — create .vfs/ directory structure and default manifest
- `hilo meta <path>` — read/write xattrs on files
- `hilo graph discover` — tree-sitter parse project, generate edges.jsonl
- `hilo graph related <path>` — query graph edges for a file
- `hilo graph stats` — graph-wide statistics (DuckDB)
- `hilo serve --mcp` — MCP server with stdio transport
- MCP tools: `vfs_get_metadata`, `vfs_graph_related`, `vfs_graph_stats`
- Manifest parsing (YAML → typed config)
- DuckDB graph.db initialization from edges.jsonl
- **No FUSE mount yet** (Phase 4)
- **No plugins yet** (Phase 3+)
- **No multi-repo yet** (Phase 3)
- **Git backend only** (local repo)

### 18.2 Cargo Features (Phase 1)

```toml
[features]
default = ["cli", "mcp", "graph"]
cli = ["clap", "serde_yaml"]
mcp = ["rmcp", "tokio"]
graph = ["tree-sitter", "petgraph", "duckdb", "xattr"]
```

---

## 19. Implementation Phases

| Phase | Milestone | Interface | Metadata | Backend | Time |
|---|---|---|---|---|---|
| **1. Prototype** | CLI + MCP + xattr + graph inventory | CLI shim + MCP | xattrs + JSONL | Git (local) | 2-3 days |
| **2. Graph Engine** | Import parsing, edge discovery, impact queries | CLI + MCP | xattrs + SQLite/DuckDB | Git | 3-5 days |
| **3. Virtual Backends** | S3 mounts, remote repos, auto-upload | CLI + MCP | xattrs + blob index | Git + S3 | 3-5 days |
| **4. FUSE Mount** | Real FUSE filesystem, agents use cat/ls | FUSE + MCP | xattrs + DuckDB | Git + S3 | 2-4 days |
| **5. Plugins** | extism wasm runtime, host functions, hot-loading | FUSE + MCP | xattrs + DuckDB | Git + S3 | 3-5 days |
| **6. Multi-Repo** | Workspace mounts, cross-repo graph, git worktrees | FUSE + MCP | xattrs + DuckDB | Multi-repo | 2-3 days |
| **7. Production** | Scale, benchmarks, security, bubblewrap, permissions | All | All | All | 1-2 weeks |

---

## 20. Open Questions

1. **xattr namespace strategy?** `user.*` is portable but per-UID. `trusted.*` requires CAP_SYS_ADMIN but is system-wide. Multi-UID agents need MCP proxy or trusted namespace.
2. **Metadata size limits?** ext4 caps xattrs at 4KB per file. Highly-connected files may exceed this. Split across multiple xattrs (`user.vfs.relations.0`, `user.vfs.relations.1`) or store heavy data in sidecar files.
3. **xattr sync across git clones?** `git clone` does not preserve xattrs. Store in git notes or a sidecar file that `vfs mount` restores on mount.
4. **Cross-filesystem xattr behavior?** Moving files from ext4 to tmpfs = complete metadata loss. Detect and warn on mount if any configured paths lack xattr support.
5. **Graph write amplification?** Every file write triggers import re-parsing + edge updates + xattr writes + JSONL append. Measure overhead under realistic load (files per second, edges per write).
6. **Bubblewrap in Docker/CI?** User namespaces are frequently disabled in containerized environments. Degrade gracefully — VFS permissions still apply.
7. **Concurrent write safety?** Multiple agents writing simultaneously. JSONL append is atomic at the OS level for writes under PIPE_BUF (4KB). Edge writes are individual lines — safe. DuckDB handles concurrent reads but single-writer for mutations.
8. **Plugin sandboxing depth?** extism provides wasmtime-level isolation. Plugins have no filesystem access except through host functions. CPU/memory limits via wasmtime config.

---

## 21. MCP Tool Signatures — Complete API Reference

### 21.1 vfs_get_metadata

```
Request:
{"method":"tools/call","params":{"name":"vfs_get_metadata","arguments":{"path":"src/login.go","keys":["feature","risk"]}}}

Response (success):
{"content":[{"type":"text","text":"{\"path\":\"src/login.go\",\"size\":1234,\"mtime\":\"2026-06-14T09:33:14Z\",\"backend\":\"git\",\"hash\":\"sha256:abc123...\",\"xattrs\":{\"user.vfs.feature\":\"auth-module\",\"user.vfs.risk\":\"critical-path\"}}"}]}

Response (not found):
{"content":[{"type":"text","text":"{\"error\":\"file not found\",\"path\":\"src/nonexistent.go\"}"}],"isError":true}
```

### 21.2 vfs_set_metadata

```
Request:
{"method":"tools/call","params":{"name":"vfs_set_metadata","arguments":{"path":"src/login.go","key":"feature","value":"auth-module"}}}

Response:
{"content":[{"type":"text","text":"{\"success\":true,\"path\":\"src/login.go\",\"key\":\"user.vfs.feature\",\"previous_value\":null}"}]}
```

### 21.3 vfs_graph_related

```
Request:
{"method":"tools/call","params":{"name":"vfs_graph_related","arguments":{"path":"src/login.go","relations":["imports"],"max_depth":3}}}

Response:
{"content":[{"type":"text","text":"{\"files\":[{\"path\":\"src/types.go\",\"relation\":\"imports\",\"depth\":1},{\"path\":\"pkg/utils.go\",\"relation\":\"imports\",\"depth\":1}],\"total\":2}"}]}
```

### 21.4 vfs_graph_impact

```
Request:
{"method":"tools/call","params":{"name":"vfs_graph_impact","arguments":{"path":"pkg/utils.go","max_depth":5}}}

Response:
{"content":[{"type":"text","text":"{\"dependents\":[{\"path\":\"src/login.go\",\"relation\":\"imported_by\",\"depth\":1},{\"path\":\"src/handler.go\",\"relation\":\"imported_by\",\"depth\":2}],\"total\":2,\"max_depth_reached\":false}"}]}
```

### 21.5 vfs_graph_stats

```
Response:
{"content":[{"type":"text","text":"{\"total_files\":142,\"total_edges\":891,\"most_connected\":\"pkg/utils.go\",\"orphans\":[\"scripts/migrate.sh\"],\"edge_types\":{\"imports\":520,\"imported_by\":520,\"tested_by\":73,\"tests\":73,\"documented_by\":5,\"documents\":5}}"}]}
```

### 21.6 vfs_graph_untested

```
Response:
{"content":[{"type":"text","text":"{\"files\":[\"src/middleware.go\",\"src/config.go\"],\"total\":2}"}]}
```

### 21.7 vfs_list_directory

```
Request:
{"method":"tools/call","params":{"name":"vfs_list_directory","arguments":{"path":"/project/models/"}}}

Response:
{"content":[{"type":"text","text":"{\"entries\":[{\"name\":\"checkpoint.pt\",\"type\":\"file\",\"backend\":\"s3\",\"size\":524288000,\"virtual\":true}]}"}]}
```

### 21.8 vfs_resolve_path

```
Response:
{"content":[{"type":"text","text":"{\"real_path\":\"/tmp/vfs-cache/models/checkpoint.pt\",\"backend\":\"s3\",\"cached\":true,\"cache_path\":\"/tmp/vfs-cache/models/checkpoint.pt\",\"remote_url\":\"s3://my-bucket/prod/checkpoint.pt\",\"sync_status\":\"synced\"}"}]}
```

### 21.9 vfs_rule_check

```
Request:
{"method":"tools/call","params":{"name":"vfs_rule_check","arguments":{"rule_name":"stale-files"}}}

Response:
{"content":[{"type":"text","text":"{\"rule\":\"stale-files\",\"results\":[{\"path\":\"src/login.go\",\"last_modified\":\"2026-06-14T09:33:14Z\",\"last_tested\":\"2026-06-10T14:22:00Z\"}],\"total\":1}"}]}
```

### 21.10 vfs_rule_list

```
Response:
{"content":[{"type":"text","text":"{\"rules\":[{\"name\":\"stale-files\",\"description\":\"Files changed since last test pass\"},{\"name\":\"untested-critical\",\"description\":\"Critical path files with no tests\"},{\"name\":\"transitive-impact\",\"description\":\"Full dependency chain for a file\"}]}"}]}
```

### 21.11 vfs_graph_module

```
Response:
{"content":[{"type":"text","text":"{\"module\":\"auth-service\",\"files\":42,\"edges_count\":156,\"test_coverage_pct\":78.3}"}]}
```

---

## 22. Per-Crate Test Requirements

| Crate | Test Files | Min Tests | Coverage Target | Notes |
|---|---|---|---|---|
| hilo-core | tests/manifest_test.rs | 10 | 95% on manifest.rs | Every manifest field + error path |
| hilo-metadata | tests/xattr_test.rs, tests/inventory_test.rs | 19 | 90% | Round-trip tests for all xattr ops |
| hilo-graph | tests/graph_test.rs, tests/parser_test.rs, tests/impact_test.rs | 19+ | 85% | One test per language parser |
| hilo-cli | tests/cli.rs | 6 | 80% | Integration: each subcommand |
| hilo-mcp | tests/mcp_test.rs | 8 | 85% | JSON-RPC protocol correctness |
| hilo-backends | tests/s3_test.rs, tests/git_test.rs | 10+ | 80% | Mock S3 server + temp git repo |
| hilo-triggers | tests/trigger_test.rs | 8+ | 80% | Temp files + inotify + timing |
| hilo-fuse | tests/fuse_test.rs | 5+ | 70% | FUSE: mount + read/write + permissions |
| hilo-plugins | tests/plugin_test.rs | 5+ | 75% | extism host function mocking |
| hilo-ffi | N/A (generated) | N/A | N/A | Verify: uniffi-bindgen generate |

### 22.1 Test Patterns

**xattr round-trip:**
```rust
let tmp = TempFile::new();
set_xattr(&tmp.path, "user.vfs.feature", "auth-module").unwrap();
assert_eq!(get_xattr(&tmp.path, "user.vfs.feature").unwrap(), Some("auth-module".into()));
```

**Graph edge insertion + query:**
```rust
let db = GraphDb::in_memory();
db.insert_edge("a.go", "b.go", "imports").unwrap();
assert_eq!(db.edge_count(), 1);
```

**MCP message round-trip:**
```rust
let msg = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#;
let resp = handle_message(msg);
let v: Value = serde_json::from_str(&resp).unwrap();
assert!(v["result"]["tools"].as_array().unwrap().len() >= 3);
```

**CLI integration:**
```rust
let tmp = TempDir::new();
let output = run_hilo(&["init"], tmp.path());
assert!(output.status.success());
assert!(tmp.path().join(".vfs/manifest.yaml").exists());
```

### 22.2 Pre-Commit Guard Expectations

- Secrets: 0 findings (gitleaks)
- Build: `cargo build` must pass
- Tests: `cargo test` all must pass
- Lint: `cargo clippy -- -D warnings` — no warnings (Phase 3+)
- Format: `cargo fmt --check` — no diffs (Phase 3+)

---

*Hilo — Implementation Specification v2 — June 2026*  
*gethilo/hilo*
