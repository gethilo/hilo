# SPEC — Backend-Backed Workspaces & Ephemeral Classification

**Status:** Spec Phase (implementation-ready)
**Date:** 2026-08-26
**Board:** GAP-055 (P1, c3) — backend-backed working space · GAP-056 (P2, c2, depends GAP-055) — ephemeral classification
**Repo:** [gethilo/hilo](https://github.com/gethilo/hilo) · **Language:** Pure Rust
**Requested by:** Bane (2026-08-26, product direction incl. design update)

---

## 0. Terminology

| Term | Meaning |
|---|---|
| **upstream** | The remote source of truth: an S3 bucket, Google Drive, OneDrive, Dropbox, or an external-tool remote (rclone `remote:path`) |
| **local-only** | A file that exists on the machine but is never transferred upstream |
| **ignore rule** | A gitignore-style pattern in `.hiloignore`; a match = excluded from upstream |
| **ephemeral** | A file that is rebuildable or redownloadable; safe to delete |
| **materialized** | A stream-mode file whose bytes are present in the local cache (vs. a placeholder) |
| **overlay** | Hilo's existing FUSE mount: the agent sees local files; Hilo routes/caches/transfers |

## 1. Purpose

**GAP-055:** Let the working space itself be backed by an S3 bucket, Google Drive, OneDrive, or Dropbox — Hilo mounts its overlay where needed, its inotify engine sees file changes and decides per-file whether to sync (ignore-aware), so the agent gets what feels like unlimited storage while build artifacts and binaries stay local-only. Reuse the sync tool the user already has (s3sync, rclone, official clients) instead of reimplementing sync; fall back to Hilo's native S3 engine when no external tool exists. Multiple agents can share the same working view through the shared backend without uploading huge files. A feature & compatibility matrix is a required deliverable.

**GAP-056:** Classify what is ephemeral in the workspace via clear patterns (reusing the GAP-055 ignore engine): noise that wastes space can be listed and wiped safely, knowing it can be rebuilt/redownloaded.

## 2. Non-Goals (explicit scope cut)

1. No reimplementation of rsync/rclone transfer engines. Hilo decides **what** to sync (ignore-aware); the driver or external tool decides **how**.
2. No rename detection optimization (rename = delete+create in v1).
3. No remote delete propagation on pull (`--prune` excluded in v1; local files absent remotely are never deleted by pull).
4. No permission/ownership/xattr syncing — content and size/mtime only.
5. No sync locks between agents; conflicts are recorded, resolved last-write-wins (LWW).
6. No UniFFI/FFI bindings for the new surfaces in v1 (CLI + MCP only).
7. `.git/` is never in the ephemeral wipe set; `.vfs/manifest.yaml` and `.vfs/backends/mounts.yaml` are never ephemeral (they are workspace truth, not rebuildable).

## 3. Architecture

```
┌──────────────────────────────────────────────────────────────┐
│  Agent / workspace view (FUSE overlay, hilo-fuse)             │
│  local files + stream placeholders (user.vfs.remote)          │
├──────────────────────────────────────────────────────────────┤
│  Decision layer (hilo-core)                                   │
│  ignore engine  → is this file local-only?                    │
│  ephemeral      → is this file rebuildable?                   │
│  sync planner   → plan/execute push·pull·both                 │
├──────────────────────────────────────────────────────────────┤
│  Change detection (hilo-triggers)                             │
│  inotify → debounce → ignore check → schedule sync            │
├──────────────────────────────────────────────────────────────┤
│  Drivers (hilo-backends)                                      │
│  S3Driver (native, extends S3Client) │ ExternalToolDriver     │
│  (rclone / s3sync / gdrive / onedrive / dropbox CLIs)         │
│  LocalDriver (reference impl, test double)                    │
└──────────────────────────────────────────────────────────────┘
```

The sync decision is always: **path matches ignore rules → local-only, never transferred.** Everything else is eligible. Ephemeral files are by default also local-only (unless explicitly overridden).

## 4. Ignore Engine — `hilo_core::ignore` (new module, hilo-core/src/ignore.rs)

No new crate: hilo-core is the shared crate consumed by hilo-backends, hilo-fuse, hilo-triggers, hilo-cli, hilo-mcp.

```rust
pub enum IgnoreSource { Builtin, RootFile, NestedFile(PathBuf) }

pub struct IgnoreRule {
    pub pattern: String,      // raw pattern text after comment stripping
    pub negated: bool,        // '!' prefix
    pub dir_only: bool,       // trailing '/'
    pub anchored: bool,       // leading '/'
    pub source: IgnoreSource,
}

pub struct IgnoreDecision {
    pub excluded: bool,       // true = local-only (excluded from upstream)
    pub rule: Option<String>, // the pattern that decided, None when default-included
    pub source: Option<IgnoreSource>,
}

pub struct IgnoreMatcher {
    root: PathBuf,
    rules: Vec<IgnoreRule>,   // ordered; LAST match wins (git semantics)
    nested: Vec<(PathBuf, Vec<IgnoreRule>)>, // per-directory .hiloignore files
    no_defaults: bool,
}

impl IgnoreMatcher {
    /// Loads rules: built-in defaults (unless no_defaults), root .hiloignore,
    /// optional extra file (--ignore-file), then discovers nested .hiloignore
    /// files under root (walk, depth-first).
    pub fn load(root: &Path, extra_file: Option<&Path>, no_defaults: bool)
        -> Result<Self, IgnoreError>;

    /// rel_path is relative to root. A directory excluded ⇒ whole subtree
    /// excluded unless a deeper negation re-includes it.
    pub fn matches(&self, rel_path: &Path, is_dir: bool) -> bool;

    /// Debug surface for `hilo ignore check <PATH>`.
    pub fn explain(&self, rel_path: &Path, is_dir: bool) -> IgnoreDecision;
}
```

### 4.1 Pattern syntax (gitignore-compatible subset — exact)

| Rule | Meaning |
|---|---|
| `# comment`, blank line | no-op |
| `!pattern` | negation (re-include) |
| `pattern/` | directory-only match |
| `/pattern` | anchored to the ignore file's directory |
| `pattern` (no slash) | matches basename at any depth |
| `a/b` (contains slash, not trailing) | relative to the ignore file's directory |
| `**` | globstar (any depth); `*` one segment; `?` one char |
| `\#` `\!` `\\` | escaped literals |

Last matching rule wins. A parent dir match excludes the subtree unless a nested negation re-includes a path (git semantics). Rules from nested `.hiloignore` files apply relative to their own directory; the root file applies everywhere; nearest file wins on equal specificity.

### 4.2 Built-in defaults (compile-time, overridable by any user rule)

```
target/ node_modules/ .venv/ venv/ __pycache__/ dist/ build/ .next/ .cargo/
.vfs/ .git/ *.o *.pyc *.class .DS_Store *.log
.hiloignore .hiloephemeral          # ignore files themselves are never uploaded
```

`.vfs/` is local-only by default (graph edges, caches, mounts.yaml are per-machine state; manifest.yaml IS in `.vfs/` — exclude note: `.vfs/manifest.yaml` must be **explicitly re-included** by a `!.vfs/manifest.yaml` rule in the generated root `.hiloignore` when `hilo workspace init --backend` writes it — the generator writes this line for you).

### 4.3 Errors

```rust
pub enum IgnoreError {
    InvalidPattern { line: usize, text: String }, // unparseable token (e.g. lone '!')
    MissingFile(PathBuf),      // --ignore-file path does not exist
    Io(std::io::Error),
}
```

## 5. Ephemeral Engine — `hilo_core::ephemeral` (new module, hilo-core/src/ephemeral.rs)

```rust
pub enum EphemeralClass { Ephemeral, Persistent }

pub struct EphemeralEntry { pub path: PathBuf, pub size: u64, pub reason: String }

pub struct EphemeralMatcher {
    matcher: IgnoreMatcher,     // .hiloephemeral uses the SAME pattern engine
    overrides: HashMap<PathBuf, bool>, // xattr user.vfs.ephemeral cache
}

impl EphemeralMatcher {
    /// Loads .hiloephemeral (same syntax as .hiloignore) if present, plus the
    /// built-in ephemeral catalog. extra_file param mirrors IgnoreMatcher.
    pub fn load(root: &Path, extra_file: Option<&Path>) -> Result<Self, IgnoreError>;

    /// xattr_ephemeral (user.vfs.ephemeral) is the HIGHEST precedence:
    /// Some(true) ⇒ Ephemeral even if no pattern matches; Some(false) ⇒
    /// Persistent even if a pattern matches. None ⇒ pattern-based.
    pub fn classify(&self, rel_path: &Path, is_dir: bool, xattr_ephemeral: Option<bool>)
        -> EphemeralClass;

    /// Walks root, classifies every file, returns entries with byte sizes
    /// (symlinks excluded, never ephemeral). O(n) stat walk.
    pub fn scan(&self, root: &Path) -> Result<Vec<EphemeralEntry>, EphemeralError>;
}

pub enum EphemeralError { Io(std::io::Error), WalkFailed(PathBuf, std::io::Error) }
```

### 5.1 Built-in ephemeral catalog (exact)

```
target/ node_modules/ .venv/ venv/ __pycache__/ dist/ build/ .next/
*.o *.pyc *.class .DS_Store *.log
.vfs/graph/            # edges.jsonl + graph.db: hilo graph clean+warm rebuilds
.vfs/sync/conflicts.jsonl
```

NOT in the catalog: `.git/`, `.vfs/manifest.yaml`, `.vfs/backends/mounts.yaml`, `.hiloignore`, `.hiloephemeral`. Any pattern in `.hiloephemeral` adds to the catalog; `!` negation removes.

### 5.2 Interaction rules (exact)

1. Ephemeral ⇒ local-only by default: the sync planner treats ephemeral paths as ignored **unless** the file carries `user.vfs.sync = upstream`.
2. `user.vfs.ephemeral = false` is the only wipe protector.
3. `hilo workspace wipe --ephemeral` never touches Persistent files, never follows symlinks, never crosses the workspace root.

## 6. Backend Abstraction — `hilo-backends` (extend hilo-backends/src/lib.rs + s3.rs, new external.rs)

```rust
pub enum BackendKind { S3, GDrive, OneDrive, Dropbox, External, Local }
pub enum SyncMode { Stream, Mirror }
pub enum SyncTool { Native, Rclone, S3Sync, GDriveCli, OneDriveCli, DropboxCli }

pub struct BackendEntry {
    pub key: String,            // remote key, relative to backend prefix/path
    pub size: i64,
    pub modified: Option<i64>,  // unix seconds
    pub etag: Option<String>,
    pub is_dir: bool,
}

pub struct BackendConfig {
    pub kind: BackendKind,
    pub name: String,                    // registry key + mounts.yaml name
    pub bucket: Option<String>,          // S3
    pub prefix: Option<String>,          // S3 key prefix
    pub region: Option<String>,          // S3
    pub remote: Option<String>,          // external tool remote ("remote:path")
    pub tool: SyncTool,                  // Native | Rclone | S3Sync | ... CLIs
    pub mode: SyncMode,                  // Stream | Mirror
    pub ignore_file: Option<PathBuf>,
    pub poll_secs: u64,                  // default 60
    pub no_default_ignores: bool,        // default false
}

pub trait Backend: Send + Sync {
    fn kind(&self) -> BackendKind;
    fn name(&self) -> &str;
    /// Non-recursive listing of prefix. Keys are relative to the backend root.
    fn list(&self, prefix: &str) -> Result<Vec<BackendEntry>, BackendError>;
    fn stat(&self, key: &str) -> Result<BackendEntry, BackendError>;
    /// Download key bytes to dest (dest is a full file path; parent exists).
    fn get(&self, key: &str, dest: &Path) -> Result<(), BackendError>;
    /// Upload local file to key. Returns the existing WriteResult shape.
    fn put(&self, local: &Path, key: &str) -> Result<WriteResult, BackendError>;
    fn delete(&self, key: &str) -> Result<(), BackendError>;
    /// Recursive listing (list + descend); used by plan_sync and stream mount.
    fn walk(&self, prefix: &str) -> Result<Vec<BackendEntry>, BackendError>;
}

pub struct S3Driver { client: S3Client, bucket: String, prefix: String, mode: SyncMode }
impl S3Driver {
    /// S3Client is the existing read/write-thru client; S3Driver extends it
    /// with ListObjectsV2 (list/walk), GetObject→cache (get), PutObject (put,
    /// reuses WriteResult), DeleteObject (delete). Multipart is handled by
    /// aws_sdk_s3; no custom chunking.
    pub fn new(cfg: &BackendConfig) -> Result<Self, BackendError>;
}

pub struct ExternalToolDriver { tool: SyncTool, remote: String, path: String, mode: SyncMode }
impl ExternalToolDriver {
    pub fn new(cfg: &BackendConfig) -> Result<Self, BackendError>; // ToolMissing if binary absent
}

pub struct LocalDriver { root: PathBuf, mode: SyncMode } // reference impl + test double; key = relative path under root

pub struct BackendRegistry { backends: HashMap<String, Arc<dyn Backend>> }
impl BackendRegistry {
    pub fn register(&mut self, name: String, b: Arc<dyn Backend>);
    pub fn get(&self, name: &str) -> Option<Arc<dyn Backend>>;
    pub fn from_config(cfg: &BackendConfig) -> Result<Arc<dyn Backend>, BackendError>;
    pub fn load_mounts(mounts_yaml: &Path) -> Result<Self, BackendError>; // reads .vfs/backends/mounts.yaml
}
```

### 6.1 ExternalToolDriver command table (exact — v1)

| Tool | list | get | put | delete | stat |
|---|---|---|---|---|---|
| `rclone` | `rclone lsf --json {remote}:{path}/{prefix}` | `rclone copyto {remote}:{path}/{key} {dest}` | `rclone copyto {local} {remote}:{path}/{key}` | `rclone deletefile {remote}:{path}/{key}` | `rclone lsl {remote}:{path}/{key}` |
| `s3sync` | `s3sync list {bucket}/{prefix}` | `s3sync pull {bucket}/{key} {dest}` | `s3sync push {local} {bucket}/{key}` | `s3sync rm {bucket}/{key}` | `s3sync stat {bucket}/{key}` |
| `gdrive` | `gdrive files list --query "'{folder}' in parents"` | `gdrive files download {id} --dest {dest}` | `gdrive files upload {local} --parent {folder}` | `gdrive files delete {id}` | `gdrive files info {id}` |

OneDrive/Dropbox official CLIs: same pattern; exact flags live in the driver's per-tool `Command` builders (one function per tool). Every external call runs with `Command::new(tool).args(...)` under a 30s default timeout (configurable `HILO_TOOL_TIMEOUT_SECS`), stderr captured into `BackendError::ToolFailed`. Missing binary → `BackendError::ToolMissing` before any call.

### 6.2 Auth (exact, v1)

| Backend | Credential source |
|---|---|
| S3 native | Standard AWS chain (`AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`/`AWS_REGION`, profile, IMDS) — unchanged |
| rclone | Existing `rclone config` (user's own remotes) — Hilo never writes rclone config |
| s3sync | `S3_*` env / existing config |
| gdrive / onedrive / dropbox CLIs | The tool's own auth state (OAuth token files) |

Credentials NEVER appear in `.vfs/` yaml or `.hiloignore`. `hilo backend setup` only detects/validates, never stores secrets.

## 7. Sync Engine — `hilo_core::sync` (new module, hilo-core/src/sync.rs)

```rust
pub enum SyncDirection { Push, Pull, Both }

pub struct TransferItem { pub key: String, pub local_path: PathBuf, pub direction: SyncDirection }
pub struct SyncPlan {
    pub to_transfer: Vec<TransferItem>,
    pub skipped_ignored: usize,     // counted, not transferred
    pub skipped_ephemeral: usize,   // counted, not transferred (unless user.vfs.sync=upstream)
}
pub enum ResolvedBy { LocalWins, RemoteWins }
pub struct ConflictRecord { pub key: String, pub local_mtime: i64, pub remote_mtime: i64, pub resolved: ResolvedBy }
pub struct SyncStats { pub transferred: usize, pub bytes: u64, pub conflicts: Vec<ConflictRecord> }

/// Push: walk local tree, for each file: ignore check (skip if excluded),
/// ephemeral check (skip unless explicit upstream), compare remote stat
/// (mtime, size); transfer when different/newer. Deletes: local file gone +
/// remote exists + not ignored → remote delete.
/// Pull: walk backend (walk()), for each remote entry: ignore check (skip),
/// exists locally? (yes → mtime compare; no → download).
pub fn plan_sync(backend: &dyn Backend, workspace_root: &Path,
                 matcher: &IgnoreMatcher, ephemeral: &EphemeralMatcher,
                 direction: SyncDirection) -> Result<SyncPlan, SyncError>;

/// Executes the plan sequentially; partial failure stops the run and returns
/// SyncError::TransferFailed with the item key; already-transferred items
/// are NOT rolled back (idempotent re-run resumes).
pub fn execute_sync(plan: &SyncPlan, backend: &dyn Backend) -> Result<SyncStats, SyncError>;

/// Writes/updates .vfs/sync/conflicts.jsonl (JSONL, one ConflictRecord per
/// line). Called by execute_sync on every LWW resolution.
pub fn record_conflict(workspace_root: &Path, rec: &ConflictRecord) -> Result<(), SyncError>;
```

LWW rule (exact): compare remote `modified` vs local mtime (unix seconds). Remote newer → remote wins (pull content over local). Local newer → local wins (push). Equal mtimes → remote wins on Pull, local wins on Push. Every resolution appends to `.vfs/sync/conflicts.jsonl`. A conflict is recorded even when the winner is the current direction (so agents see churn).

### 7.1 hilo-triggers integration (hilo-triggers/src, existing watcher loop)

The existing inotify watcher + debounce loop gains a sync hook:

```
inotify event → debounce (250ms default, configurable HILO_DEBOUNCE_MS)
  → resolve changed path relative to workspace root
  → IgnoreMatcher::matches? → skip (local-only), done
  → EphemeralMatcher::classify == Ephemeral && xattr sync != upstream? → skip, done
  → mark dirty; batch dirty keys; when batch settles (no events for 500ms)
  → execute_sync(Push) for the batch via the mount's backend
```

Mount polling for Pull: every `poll_secs` (default 60), `plan_sync(Pull)` + `execute_sync`. Polling is the v1 pull mechanism (tool-agnostic; no reliance on per-tool watch modes).

## 8. Stream Mode — `hilo-fuse` (extend hilo-fuse/src)

`--mode stream` mount semantics:

1. On mount: `backend.walk("")` → for each remote entry, create a **placeholder**: zero-byte regular file at the mapped local path with xattrs `user.vfs.remote = <key>` and `user.vfs.materialized = "false"`. Placeholders are never created for ignored/ephemeral keys.
2. `getattr` on a placeholder reports the **remote size** (from the walk listing; BackendEntry.size) so agents see real file sizes.
3. FUSE `open`/`read` on a placeholder triggers `backend.get(key, cache_path)` into the workspace cache (hilo-backends' existing cache layout), then serves bytes from the cache file, then sets `user.vfs.materialized = "true"` (and removes the `user.vfs.remote` marker only when the file is also marked dirty — placeholder stays resolvable for re-sync).
4. Writes to a placeholder (open for write) materialize first, then apply the write, then enter the normal dirty→push flow.
5. `user.vfs.materialized = "false"` files are **local-only** by definition (they have no local truth) — they are never pushed; a pull that sees them dirty refreshes from remote.

Mirror mode: no placeholders; full pull on mount (ignore-aware).

## 9. CLI Surface — `hilo-cli` (extend hilo-cli/src/commands/{backend,workspace}.rs, new commands/ignore.rs)

```text
hilo backend mount --type s3|gdrive|onedrive|dropbox|external
       --bucket <B>            (s3)
       --prefix <P>            (s3, optional)
       --remote <R>            (external/gdrive/onedrive/dropbox: "remote:path" or tool remote)
       --at <PATH>             (mount point, required)
       [--tool auto|native|rclone|s3sync|gdrive|onedrive|dropbox]   (default auto)
       [--mode stream|mirror]  (default mirror)
       [--ignore-file <PATH>]  (extra ignore file, optional)
       [--poll-secs <N>]       (default 60)
       [--no-default-ignores]

hilo backend sync [--push|--pull|--both] [PATH...]   (default --both; PATH limits to subtree)
hilo backend setup [--type s3|gdrive|onedrive|dropbox|external]   (detect tools on PATH,
       validate creds, print next steps; writes nothing)

hilo workspace ephemeral [PATH...]     (default workspace root; TSV: path<TAB>size<TAB>reason)
hilo workspace wipe --ephemeral [--apply]   (default: dry-run plan; --apply deletes; prints freed bytes)

hilo ignore check <PATH>               (prints the IgnoreDecision: excluded? rule? source?)
```

`--tool auto` resolution (exact): `external`/`gdrive`/`onedrive`/`dropbox` types → prefer the matching official CLI, else `rclone`, else fail with `BackendError::ToolMissing` listing what to install. `s3` → `native` unless `--tool rclone|s3sync` is given. `hilo backend mount` extends the existing command (GAP-009's documented `--type s3 --bucket --at [--prefix] [--region]` surface is unchanged; new flags are additive).

## 10. MCP Surface — `hilo-mcp` (extend hilo-mcp/src/tools/mod.rs)

Tools: 15 → **17**. Registry names, handlers, `input_schema`, and docs must all list both new tools.

| Tool | Input | Output |
|---|---|---|
| `vfs_workspace_ephemeral` | `path?` (default workspace root) | `{entries: [{path, size, reason}], total_bytes}` |
| `vfs_workspace_wipe` | `path?`, `dry_run: bool = true` | `{removed: [{path, bytes}], freed_bytes}` (dry_run: planned only) |

Semantic change (documented): `vfs_sync_backend(path)` becomes ignore-aware — ignored/ephemeral paths report `{synced_files: 0, errors: [], skipped_ignored: N}` instead of transferring.

## 11. Config & Data Model

### 11.1 `.hiloignore` (workspace root) + nested `.hiloignore` files
Format = §4.1. `hilo workspace init --backend ...` generates a root `.hiloignore` containing the built-in defaults materialized as comments + the `!.vfs/manifest.yaml` re-include line, so users can edit it explicitly.

### 11.2 `.hiloephemeral` (workspace root)
Same syntax as `.hiloignore`; defaults from §5.1 always active; file adds/removes patterns.

### 11.3 `.vfs/backends/mounts.yaml` — extended entry (exact)
```yaml
- name: prod-bucket
  type: s3
  bucket: my-bucket
  prefix: workspace/
  at: /mnt/vfs/ws
  tool: native          # auto|native|rclone|s3sync|gdrive|onedrive|dropbox
  mode: mirror          # stream|mirror
  ignore_file: .hiloignore
  poll_secs: 60
  no_default_ignores: false
```
Existing entries without the new keys keep current behavior (`tool: native` implied for s3, `mode: mirror`, defaults for the rest).

### 11.4 `.vfs/manifest.yaml` — optional `backend_defaults:` block
```yaml
backend_defaults:
  tool: auto
  mode: mirror
  poll_secs: 60
```

### 11.5 xattrs (extend the `user.vfs.*` namespace)
| xattr | values | meaning |
|---|---|---|
| `user.vfs.sync` | `inherit` (default) / `local` / `upstream` | per-file override of the ignore decision |
| `user.vfs.ephemeral` | `true` / `false` | highest-precedence ephemeral override; `false` = wipe protector |
| `user.vfs.remote` | key string | stream placeholder → remote key |
| `user.vfs.materialized` | `true` / `false` | stream placeholder state |

## 12. Error Catalog (exact)

| Error | Condition | CLI exit | MCP error |
|---|---|---|---|
| `BackendError::NotFound(key)` | get/stat/delete on missing key | 1 | -32602 |
| `BackendError::BucketError(String)` | bucket ops fail | 1 | -32603 |
| `BackendError::ReadOnly` | put/delete on read-only mount | 4 | -32603 |
| `BackendError::Io(io)` | local fs failure | 1 | -32603 |
| `BackendError::Aws(String)` | aws_sdk_s3 failure | 1 | -32603 |
| `BackendError::ToolMissing(tool)` | required binary not on PATH | 4 | -32602 |
| `BackendError::ToolFailed(tool, exit, stderr)` | external tool nonzero | 3 | -32603 |
| `BackendError::Unreachable(endpoint)` | connection/auth probe failed | 5 | -32603 |
| `BackendError::InvalidConfig(field)` | bad mounts.yaml entry | 2 | -32602 |
| `IgnoreError::InvalidPattern{line,text}` | unparseable pattern | 2 | -32602 |
| `IgnoreError::MissingFile(path)` | --ignore-file absent | 2 | -32602 |
| `SyncError::TransferFailed(key)` | execute_sync partial failure | 3 | -32603 |
| `EphemeralError::WalkFailed(path, io)` | scan I/O error | 1 | -32603 |

## 13. Edge Cases (numbered; resolution is the spec)

1. **Ignored dir, re-included child** (`target/` + `!target/keep.txt`) — git semantics via last-match-wins; matcher checks parent dirs: a re-included path under an excluded dir is included (ancestor chain must not end in exclusion for the final rule).
2. **Empty bucket / brand-new workspace** — pull plans nothing, push creates keys; mount succeeds.
3. **Files > 5 GiB** — native S3: aws_sdk_s3 multipart (no custom code). External tools: their own handling. No Hilo-side chunking.
4. **Unicode / emoji filenames** — keys are raw strings end-to-end (never normalized); unit tests cover NFC/NFD and emoji.
5. **Symlinks** — never transferred, never ephemeral, skipped by walkers; documented in matrix as unsupported.
6. **Concurrent agents on one backend** — LWW (§7) + `.vfs/sync/conflicts.jsonl`; no locks; deterministic resolution makes both agents converge.
7. **Network drop mid-transfer** — partial local downloads written to `<dest>.part` then renamed on success; re-run resumes; external tools' own retries apply.
8. **Read-only backend** — put/delete → `ReadOnly`; stream mode still works (get only).
9. **Local delete propagation** — push deletes remote only when the local path is gone AND not ignored; pull never deletes local files (no `--prune` in v1).
10. **Rename** — delete+create in v1 (two transfer items); documented limitation.
11. **File changes during sync** — debounce + dirty-batch; a file modified mid-plan is re-planned next cycle (execute_sync re-stats).
12. **No `.hiloignore`** — defaults only; `hilo ignore check` explains with `source: Builtin`.
13. **Ignore files themselves** — `.hiloignore`/`.hiloephemeral` are always local-only (built-in rule, not removable).
14. **Metadata-only ops on placeholders** — getattr/setxattr/listxattr do NOT materialize; only open/read/write do.
15. **Wipe of in-use files** — Linux allows unlink of open files; reported as freed regardless; the rebuild path is what matters.
16. **Ephemeral + explicit sync** — `user.vfs.sync=upstream` uploads an ephemeral file but does NOT protect it from wipe; `user.vfs.ephemeral=false` is the only protector.
17. **`.hiloignore` inside an ignored dir** — its rules still load (git behavior); the dir's files stay excluded unless re-included.

## 14. Feature & Compatibility Matrix (deliverable — docs/backend-compatibility-matrix.md)

Source of truth for the deliverable doc; the implementer copies this table into `docs/backend-compatibility-matrix.md` as part of GAP-055.

| Capability | S3 native | S3 rclone | S3 s3sync | GDrive rclone | GDrive gdrive CLI | OneDrive rclone | OneDrive CLI | Dropbox rclone | Dropbox CLI |
|---|---|---|---|---|---|---|---|---|---|
| Stream mode (lazy fetch) | ✅ | ✅ | ❌ | ✅ | ✅ | ✅ | ⚠️ | ✅ | ⚠️ |
| Mirror mode | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Selective sync (ignore-aware) | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Multi-agent shared view | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Incremental (listing diff) | ✅ | ✅ | ✅ | ✅ | ⚠️ | ✅ | ⚠️ | ✅ | ⚠️ |
| Checksum verify | ✅ etag | ✅ | ✅ | ⚠️ md5 | ⚠️ md5 | ⚠️ | ❌ | ⚠️ content hash | ❌ |
| Rename tracking | ❌ v1 | ✅ --track-renames | ❌ | ✅ | ❌ | ✅ | ❌ | ✅ | ❌ |
| Auth | AWS env chain | rclone config | S3_* env | rclone config | OAuth | rclone config | OAuth | rclone config | OAuth |
| Windows/macOS | ✅ | ✅ | ⚠️ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |

✅ = supported · ⚠️ = partial/limitations (note in doc) · ❌ = not in v1. Any cell marked ❌ must have its limitation sentence written in the doc next to the table.

## 15. Testing (exact scenarios)

**Ignore matcher (hilo-core, table-driven corpus, ~35 cases):** anchored `/build` only matches root build/; trailing `/` dir-only; `!` negation re-includes; globstar `a/**/b`; `*.o` basename at any depth; nested `.hiloignore` nearest-wins; unicode/emoji; escaped `\#`; parent-dir exclusion blocks child negation order; last-match-wins ordering; default built-ins apply when no file; `--no-default-ignores` disables them.

**Ephemeral (hilo-core):** built-in catalog classifies `target/`, `node_modules/` as Ephemeral and `src/` as Persistent; `.hiloephemeral` adds/removes; xattr `Some(true)` overrides pattern-negative; `Some(false)` protects from wipe; `.git/` and `manifest.yaml` never Ephemeral; `scan` sizes sum correctly (fixture tree with known byte counts).

**Backend trait (hilo-backends):** `MockBackend` (HashMap in-memory) + `LocalDriver` reference impl pass an identical contract test suite (list/stat/get/put/delete/walk round-trips). `ExternalToolDriver` unit tests use a fake `rclone` bash shim on PATH (captures argv, returns canned `lsf --json`) — assert exact command construction for list/get/put/delete/stat. S3 native: live integration test gated on `AWS_ACCESS_KEY_ID` presence (skip otherwise).

**Sync engine (hilo-core):** plan excludes ignored + ephemeral (counts in `skipped_*`); push transfers new/changed only; delete propagation when local gone + not ignored; pull never deletes local; LWW: remote-newer → remote wins, local-newer → local wins, equal → direction default; conflicts.jsonl appended per resolution; partial failure stops with `TransferFailed(key)` and idempotent re-run completes.

**Stream placeholders (hilo-fuse):** mount with MockBackend → placeholders created only for non-ignored keys; getattr reports remote size; read materializes + sets `user.vfs.materialized=true`; write-to-placeholder materializes then writes; metadata ops do not materialize.

**CLI (hilo-cli):** clap parse tests for every new flag; `--tool auto` resolution matrix; exit codes per §12 (ToolMissing=4, ToolFailed=3, usage=2, auth=5); `wipe --ephemeral` dry-run lists and `--apply` deletes only ephemeral; `ignore check` output.

**MCP (hilo-mcp):** tools/list = 17; both new tools callable; `vfs_sync_backend` on ignored path returns `skipped_ignored: N, synced_files: 0`.

**E2E (integration, LocalDriver as backend):** workspace init → write `src/main.rs` + `target/artifact.bin` → `hilo backend sync --push` → assert only main.rs upstream → fresh mount pull → artifact.bin absent locally, main.rs present → `wipe --ephemeral` dry-run lists target/ only → `--apply` frees bytes → rebuild regenerates artifact → `hilo graph clean && hilo graph warm` after wipe of `.vfs/graph/` still works (rebuild path verified).

## 16. Hilo Impact & Wiring Checklist

| Crate | File(s) | Change |
|---|---|---|
| hilo-core | src/ignore.rs, src/ephemeral.rs, src/sync.rs (new) | ignore/ephemeral engines + sync planner; exports in lib.rs |
| hilo-backends | src/lib.rs, src/s3.rs, src/external.rs (new), src/local.rs (new) | Backend trait, registry, S3Driver extension, ExternalToolDriver, LocalDriver |
| hilo-triggers | src/* (watcher loop) | sync hook after debounce; poll timer for pull |
| hilo-fuse | src/* (open/read/getattr) | placeholder materialization |
| hilo-cli | src/commands/backend.rs, workspace.rs, ignore.rs (new) | 4 new command surfaces + extended mount |
| hilo-mcp | src/tools/mod.rs, src/lib.rs | 2 new tools + registry + input_schema; SKILL.md MCP table sync (15→17) |
| docs | docs/backend-compatibility-matrix.md (new), docs/cli-reference.md, SKILL.md | matrix deliverable + CLI/MCP doc sync |

Depends on: existing S3Client (read/write-thru), existing inotify watcher/debounce, existing trigger action `upload-to-backend`, existing mounts.yaml loader. Nothing else depends on the new modules (pure additions; existing `hilo backend mount` semantics for old fields unchanged).

## 17. Board AC Mapping

| Board task | Pass criteria (board) | Spec section |
|---|---|---|
| GAP-055 | mount against S3/GDrive/OneDrive/Dropbox orchestrating existing tool or native engine | §6, §9, §11.3 |
| GAP-055 | ignore-aware overlay + inotify (matched files local-only, never transferred) | §4, §7.1 |
| GAP-055 | feature & compatibility matrix document exists | §14 |
| GAP-056 | ephemeral detection by clear patterns | §5, §15 |
| GAP-056 | wipe removes only ephemeral, reports, regenerable | §5.2, §9, §15 |
