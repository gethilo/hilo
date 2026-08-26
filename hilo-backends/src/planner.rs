//! Spec §7 — trait-based sync planner (backend-agnostic).
//!
//! This module implements the backend-agnostic sync engine from
//! `specs/backend-backed-workspace-spec.md` §7: [`plan_sync`] walks the local
//! workspace and the remote backend and produces a [`SyncPlan`], and
//! [`execute_sync`] runs that plan against any [`Backend`] implementation.
//!
//! The legacy S3-native [`SyncEngine`](crate::sync::SyncEngine) (tick 149,
//! "native fallback") stays in `sync.rs` and keeps its own `SyncPlan` type;
//! the §7 planner lives here so both can coexist (the spec's `SyncPlan`
//! fields differ from the legacy struct's, so the new one is reachable as
//! `planner::SyncPlan` while `sync::SyncPlan` stays the legacy type).
//!
//! ## Semantics (spec §7 exact + documented deviations)
//!
//! - LWW: remote `modified` vs local mtime (unix seconds). Remote newer →
//!   remote wins (pull). Local newer → local wins (push). Equal mtimes →
//!   remote wins on `Pull`, local wins on `Push`. On `Both`, equal mtimes
//!   transfer nothing (prevents the push/pull ping-pong when both sides are
//!   aligned); this is the documented `Both` tie-break.
//! - A direction-blocked resolution (e.g. remote-newer file during a
//!   `Push`-only run) produces no transfer item; it resolves on the next
//!   two-way run.
//! - Delete propagation (spec §13.9): `Push` deletes remote keys whose local
//!   file is gone and which are not ignored. `Pull` never deletes local files
//!   (no `--prune` in v1). `Both` treats remote-only keys as downloads
//!   (convergent — an agent that just uploaded must not see its file
//!   deleted); explicit deletes happen only on `Push` runs.
//! - Conflicts: every executed LWW resolution (a transferred item whose
//!   counterpart exists on the other side) appends a [`ConflictRecord`] to
//!   `.vfs/sync/conflicts.jsonl` — even when the winner is the current
//!   direction, so agents see churn.
//! - After each transfer the local mtime is aligned to the remote `modified`
//!   (push: local file mtime = remote modified after put; pull: dest mtime =
//!   remote modified), so a repeated sync is a no-op instead of a ping-pong.
//! - Partial failure: `execute_sync` stops at the first failing item and
//!   returns [`SyncError::TransferFailed`] with the item key; already
//!   transferred items are NOT rolled back (an idempotent re-run resumes).
//! - Ephemeral: files classified ephemeral are skipped unless the
//!   `user.vfs.sync=upstream` xattr is set (spec §7); symlinks are never
//!   transferred (spec §13.5).
//!
//! ## Spec-signature deviation
//!
//! The spec declares `execute_sync(plan, backend)` but also states
//! `record_conflict(workspace_root, rec)` is "called by execute_sync on every
//! LWW resolution" — impossible without the workspace root. `execute_sync`
//! therefore takes a third `workspace_root: &Path` parameter.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::backend::{Backend, BackendEntry, BackendError};
use crate::ephemeral::{EphemeralClass, EphemeralMatcher};
use crate::sync::{is_never_synced, IgnoreMatcher};

/// Which way a sync run moves data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncDirection {
    /// Upload-only. Local files may be pushed; remote deletes propagate.
    Push,
    /// Download-only. Remote files may be pulled; local files are never
    /// deleted and new local files are never uploaded.
    Pull,
    /// Two-way: push local-newer, pull remote-newer, propagate deletes.
    Both,
}

/// One planned transfer between the workspace and the backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferItem {
    /// Backend key (relative to the backend root).
    pub key: String,
    /// Local path (absolute, inside the workspace root).
    pub local_path: PathBuf,
    pub direction: SyncDirection,
}

/// The transfer plan produced by [`plan_sync`].
///
/// Note: the spec (§7) defines `to_transfer`/`skipped_ignored`/
/// `skipped_ephemeral`; `to_delete` is a minimal extension required by the
/// §7 delete-propagation semantics and §13.9.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncPlan {
    pub to_transfer: Vec<TransferItem>,
    /// Remote keys to delete (local file gone + not ignored).
    pub to_delete: Vec<String>,
    /// Files counted but not transferred because an ignore rule matched.
    pub skipped_ignored: usize,
    /// Files counted but not transferred because they are ephemeral.
    pub skipped_ephemeral: usize,
}

/// Which side won a last-writer-wins resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResolvedBy {
    LocalWins,
    RemoteWins,
}

/// One LWW resolution, persisted to `.vfs/sync/conflicts.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictRecord {
    pub key: String,
    /// Local mtime at resolution time, unix seconds.
    pub local_mtime: i64,
    /// Remote modified at resolution time, unix seconds.
    pub remote_mtime: i64,
    pub resolved: ResolvedBy,
}

/// Result of [`execute_sync`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncStats {
    /// Number of transferred items (pushes + pulls; deletes not counted).
    pub transferred: usize,
    /// Bytes transferred (source-side sizes).
    pub bytes: u64,
    /// LWW resolutions recorded this run (also appended to conflicts.jsonl).
    pub conflicts: Vec<ConflictRecord>,
}

/// Sync engine errors (spec §12: `SyncError::TransferFailed`).
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("transfer failed: {0}")]
    TransferFailed(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("backend error: {0}")]
    Backend(#[from] BackendError),
    #[error("conflict record serialization failed: {0}")]
    Serialize(String),
    #[error("workspace walk failed: {0}")]
    Walk(String),
}

/// A local file discovered during the workspace walk.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalEntry {
    /// POSIX-style path relative to the workspace root.
    rel_path: String,
    size: u64,
    /// mtime as UNIX epoch seconds.
    mtime_unix: i64,
}

/// Build the transfer plan for one workspace against one backend.
///
/// Walk order is deterministic (sorted file names / keys). Ignore and
/// ephemeral checks are applied per file (no directory pruning, so
/// re-included children under an excluded dir — spec §13.1 — stay correct).
pub fn plan_sync(
    backend: &dyn Backend,
    workspace_root: &Path,
    matcher: &IgnoreMatcher,
    ephemeral: &EphemeralMatcher,
    direction: SyncDirection,
) -> Result<SyncPlan, SyncError> {
    let mut plan = SyncPlan::default();

    let local = walk_local(workspace_root)?;
    let remote = backend.walk("")?;
    let remote_map: HashMap<&str, &BackendEntry> = remote
        .iter()
        .filter(|e| !e.is_dir)
        .map(|e| (e.key.as_str(), e))
        .collect();
    let local_set: HashSet<&str> = local.iter().map(|l| l.rel_path.as_str()).collect();

    for lf in &local {
        if is_never_synced(&lf.rel_path) {
            continue;
        }
        if matcher.is_ignored(&lf.rel_path) {
            plan.skipped_ignored += 1;
            continue;
        }
        let full_path = workspace_root.join(&lf.rel_path);
        let is_eph = ephemeral.classify(
            Path::new(&lf.rel_path),
            false,
            xattr_ephemeral_bool(&full_path),
        ) == EphemeralClass::Ephemeral;
        if is_eph && !is_upstream_override(&full_path) {
            plan.skipped_ephemeral += 1;
            continue;
        }

        let item = TransferItem {
            key: lf.rel_path.clone(),
            local_path: full_path,
            direction: SyncDirection::Push,
        };
        match remote_map.get(lf.rel_path.as_str()) {
            None => {
                if matches!(direction, SyncDirection::Push | SyncDirection::Both) {
                    plan.to_transfer.push(item);
                }
            }
            Some(ro) => {
                let remote_mtime = ro.modified.unwrap_or(0);
                let local_mtime = lf.mtime_unix;
                if local_mtime > remote_mtime {
                    if matches!(direction, SyncDirection::Push | SyncDirection::Both) {
                        plan.to_transfer.push(item);
                    }
                    // Pull-only: local newer is direction-blocked.
                } else if remote_mtime > local_mtime {
                    if matches!(direction, SyncDirection::Pull | SyncDirection::Both) {
                        let pull_item = TransferItem {
                            direction: SyncDirection::Pull,
                            ..item
                        };
                        plan.to_transfer.push(pull_item);
                    }
                    // Push-only: remote newer is direction-blocked.
                } else if matches!(direction, SyncDirection::Push) {
                    // Equal: local wins on Push (spec §7 tie-break).
                    plan.to_transfer.push(item);
                } else if matches!(direction, SyncDirection::Pull) {
                    // Equal: remote wins on Pull (spec §7 tie-break).
                    let pull_item = TransferItem {
                        direction: SyncDirection::Pull,
                        ..item
                    };
                    plan.to_transfer.push(pull_item);
                }
                // Both + equal: no transfer (documented no-ping-pong tie-break).
            }
        }
    }

    {
        let mut remote_keys: Vec<&BackendEntry> = remote
            .iter()
            .filter(|e| !e.is_dir && !local_set.contains(e.key.as_str()))
            .collect();
        remote_keys.sort_by(|a, b| a.key.cmp(&b.key));
        for ro in remote_keys {
            if is_never_synced(&ro.key) {
                continue;
            }
            if matcher.is_ignored(&ro.key) {
                // Ignored remote files are neither transferred nor deleted.
                plan.skipped_ignored += 1;
                continue;
            }
            match direction {
                // Push: local state is truth — remote-only non-ignored keys
                // are deleted (spec §7 / §13.9).
                SyncDirection::Push => plan.to_delete.push(ro.key.clone()),
                // Pull / Both: remote-only keys are downloads (convergent —
                // an agent that just uploaded must not see its file deleted).
                SyncDirection::Pull | SyncDirection::Both => {
                    plan.to_transfer.push(TransferItem {
                        key: ro.key.clone(),
                        local_path: workspace_root.join(&ro.key),
                        direction: SyncDirection::Pull,
                    });
                }
            }
        }
    }

    Ok(plan)
}

/// Execute a plan: transfers, then deletes. Stops at the first failure with
/// [`SyncError::TransferFailed`] carrying the item key; already-transferred
/// items are not rolled back.
///
/// Every executed LWW resolution (a transferred item whose counterpart
/// exists on the other side) appends a [`ConflictRecord`] to
/// `.vfs/sync/conflicts.jsonl` via [`record_conflict`], and local mtimes are
/// aligned to the remote `modified` after each transfer so a repeated sync
/// is a no-op.
pub fn execute_sync(
    plan: &SyncPlan,
    backend: &dyn Backend,
    workspace_root: &Path,
) -> Result<SyncStats, SyncError> {
    let mut stats = SyncStats::default();

    for item in &plan.to_transfer {
        match item.direction {
            SyncDirection::Push | SyncDirection::Pull => {
                execute_transfer(item, backend, &mut stats)?;
            }
            SyncDirection::Both => {
                // plan_sync never emits `Both` items (it resolves each file to
                // Push or Pull), but a caller may reuse a plan with a
                // hand-built item; resolve by LWW at execution time.
                let remote_modified = match backend.stat(&item.key) {
                    Ok(e) => Some(e.modified.unwrap_or(0)),
                    Err(BackendError::NotFound(_)) => None,
                    Err(e) => return Err(SyncError::Backend(e)),
                };
                let local_mtime = local_mtime_unix(&item.local_path).unwrap_or(0);
                let pull = match remote_modified {
                    Some(rm) => rm > local_mtime,
                    None => false, // no remote counterpart: upload
                };
                let resolved = TransferItem {
                    direction: if pull {
                        SyncDirection::Pull
                    } else {
                        SyncDirection::Push
                    },
                    ..item.clone()
                };
                execute_transfer(&resolved, backend, &mut stats)?;
            }
        }
    }

    for key in &plan.to_delete {
        backend
            .delete(key)
            .map_err(|_| SyncError::TransferFailed(key.clone()))?;
    }

    for rec in &stats.conflicts {
        record_conflict(workspace_root, rec)?;
    }

    Ok(stats)
}

/// Execute one transfer item (push or pull): record the LWW conflict when a
/// counterpart exists, transfer, count bytes, and align local mtime to the
/// remote `modified` so a repeated sync is a no-op.
fn execute_transfer(
    item: &TransferItem,
    backend: &dyn Backend,
    stats: &mut SyncStats,
) -> Result<(), SyncError> {
    match item.direction {
        SyncDirection::Push => {
            let counterpart = match backend.stat(&item.key) {
                Ok(e) => Some(e),
                Err(BackendError::NotFound(_)) => None,
                Err(e) => return Err(SyncError::Backend(e)),
            };
            if let Some(ro) = counterpart {
                stats.conflicts.push(ConflictRecord {
                    key: item.key.clone(),
                    local_mtime: local_mtime_unix(&item.local_path).unwrap_or(0),
                    remote_mtime: ro.modified.unwrap_or(0),
                    resolved: ResolvedBy::LocalWins,
                });
            }
            backend
                .put(&item.local_path, &item.key)
                .map_err(|_| SyncError::TransferFailed(item.key.clone()))?;
            stats.bytes += std::fs::metadata(&item.local_path)?.len();
            stats.transferred += 1;
            // Align local mtime to the remote modified (no ping-pong).
            if let Ok(ro) = backend.stat(&item.key) {
                if let Some(modified) = ro.modified {
                    let _ = set_mtime(&item.local_path, modified);
                }
            }
        }
        SyncDirection::Pull => {
            let counterpart = if item.local_path.exists() {
                Some(backend.stat(&item.key).unwrap_or(BackendEntry {
                    key: item.key.clone(),
                    size: -1,
                    modified: None,
                    etag: None,
                    is_dir: false,
                }))
            } else {
                None
            };
            if let Some(ro) = counterpart {
                stats.conflicts.push(ConflictRecord {
                    key: item.key.clone(),
                    local_mtime: local_mtime_unix(&item.local_path).unwrap_or(0),
                    remote_mtime: ro.modified.unwrap_or(0),
                    resolved: ResolvedBy::RemoteWins,
                });
            }
            if let Some(parent) = item.local_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // Partial downloads land in `<dest>.part`, renamed on success
            // (spec §13.7).
            let mut part_name = item.local_path.as_os_str().to_owned();
            part_name.push(".part");
            let part = PathBuf::from(part_name);
            backend
                .get(&item.key, &part)
                .map_err(|_| SyncError::TransferFailed(item.key.clone()))?;
            std::fs::rename(&part, &item.local_path)?;
            stats.bytes += std::fs::metadata(&item.local_path)?.len();
            stats.transferred += 1;
            if let Ok(ro) = backend.stat(&item.key) {
                if let Some(modified) = ro.modified {
                    let _ = set_mtime(&item.local_path, modified);
                }
            }
        }
        SyncDirection::Both => unreachable!("Both is resolved before execute_transfer"),
    }
    Ok(())
}

/// Append one LWW resolution to `.vfs/sync/conflicts.jsonl` (JSONL, one
/// record per line), creating the directory as needed.
pub fn record_conflict(workspace_root: &Path, rec: &ConflictRecord) -> Result<(), SyncError> {
    use std::io::Write;

    let dir = workspace_root.join(".vfs").join("sync");
    std::fs::create_dir_all(&dir)?;
    let line = serde_json::to_string(rec).map_err(|e| SyncError::Serialize(e.to_string()))?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("conflicts.jsonl"))?;
    writeln!(f, "{line}")?;
    Ok(())
}

/// Recursively walk the workspace root; symlinks and non-files are skipped
/// (spec §13.5). Sorted by file name for deterministic plans.
fn walk_local(root: &Path) -> Result<Vec<LocalEntry>, SyncError> {
    let mut entries = Vec::new();
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
    {
        let entry = entry.map_err(|e| SyncError::Walk(e.to_string()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = match entry.path().strip_prefix(root) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let rel_path = rel.to_string_lossy().replace('\\', "/");
        let meta = entry
            .metadata()
            .map_err(|e| SyncError::Walk(e.to_string()))?;
        entries.push(LocalEntry {
            rel_path,
            size: meta.len(),
            mtime_unix: local_mtime_unix(entry.path()).unwrap_or(0),
        });
    }
    Ok(entries)
}

/// mtime of a path as UNIX epoch seconds (0 when unavailable).
fn local_mtime_unix(path: &Path) -> Option<i64> {
    let meta = std::fs::metadata(path).ok()?;
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
}

/// Set a file's mtime to a UNIX epoch timestamp.
fn set_mtime(path: &Path, unix_secs: i64) -> std::io::Result<()> {
    let file = std::fs::OpenOptions::new().write(true).open(path)?;
    let secs = u64::try_from(unix_secs).unwrap_or(0);
    let times =
        std::fs::FileTimes::new().set_modified(UNIX_EPOCH + std::time::Duration::from_secs(secs));
    file.set_times(times)
}

/// Read one `user.vfs.<name>` xattr as a UTF-8 string (None when absent or
/// unreadable — xattr probing must never fail a sync).
fn vfs_xattr(path: &Path, name: &str) -> Option<String> {
    xattr::get(path, format!("user.vfs.{name}"))
        .ok()
        .flatten()
        .and_then(|v| String::from_utf8(v).ok())
}

/// `user.vfs.sync == "upstream"` forces an ephemeral file into the plan
/// (spec §7: skipped_ephemeral "unless user.vfs.sync=upstream").
pub fn is_upstream_override(path: &Path) -> bool {
    vfs_xattr(path, "sync").as_deref() == Some("upstream")
}

/// The `user.vfs.ephemeral` xattr as an explicit classify override.
pub fn xattr_ephemeral_bool(path: &Path) -> Option<bool> {
    match vfs_xattr(path, "ephemeral").as_deref() {
        Some("true") => Some(true),
        Some("false") => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::BackendKind;
    use std::collections::HashMap;
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    /// In-memory backend test double (HashMap-backed).
    struct MockBackend {
        objects: Arc<Mutex<HashMap<String, MockObject>>>,
        fail_keys: Arc<Mutex<HashSet<String>>>,
    }

    #[derive(Clone)]
    struct MockObject {
        bytes: Vec<u8>,
        modified: i64,
    }

    impl MockBackend {
        fn new() -> Self {
            Self {
                objects: Arc::new(Mutex::new(HashMap::new())),
                fail_keys: Arc::new(Mutex::new(HashSet::new())),
            }
        }

        fn seed(&self, key: &str, bytes: &[u8], modified: i64) {
            self.objects.lock().unwrap().insert(
                key.to_string(),
                MockObject {
                    bytes: bytes.to_vec(),
                    modified,
                },
            );
        }

        fn fail(&self, key: &str) {
            self.fail_keys.lock().unwrap().insert(key.to_string());
        }

        fn get_bytes(&self, key: &str) -> Option<Vec<u8>> {
            self.objects
                .lock()
                .unwrap()
                .get(key)
                .map(|o| o.bytes.clone())
        }

        fn fails(&self, key: &str) -> bool {
            self.fail_keys.lock().unwrap().contains(key)
        }
    }

    impl Backend for MockBackend {
        fn kind(&self) -> BackendKind {
            BackendKind::Local
        }

        fn name(&self) -> &str {
            "mock"
        }

        fn list(&self, prefix: &str) -> Result<Vec<BackendEntry>, BackendError> {
            let mut out = Vec::new();
            for (key, obj) in self.objects.lock().unwrap().iter() {
                if prefix.is_empty() || key.starts_with(&format!("{prefix}/")) {
                    out.push(BackendEntry {
                        key: key.clone(),
                        size: obj.bytes.len() as i64,
                        modified: Some(obj.modified),
                        etag: None,
                        is_dir: false,
                    });
                }
            }
            out.sort_by(|a, b| a.key.cmp(&b.key));
            Ok(out)
        }

        fn stat(&self, key: &str) -> Result<BackendEntry, BackendError> {
            let objs = self.objects.lock().unwrap();
            match objs.get(key) {
                Some(o) => Ok(BackendEntry {
                    key: key.to_string(),
                    size: o.bytes.len() as i64,
                    modified: Some(o.modified),
                    etag: None,
                    is_dir: false,
                }),
                None => Err(BackendError::NotFound(key.to_string())),
            }
        }

        fn get(&self, key: &str, dest: &Path) -> Result<(), BackendError> {
            if self.fails(key) {
                return Err(BackendError::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "mock failure",
                )));
            }
            let objs = self.objects.lock().unwrap();
            let obj = objs
                .get(key)
                .ok_or_else(|| BackendError::NotFound(key.to_string()))?;
            let mut f = std::fs::File::create(dest)?;
            f.write_all(&obj.bytes)?;
            Ok(())
        }

        fn put(&self, local: &Path, key: &str) -> Result<crate::WriteResult, BackendError> {
            if self.fails(key) {
                return Err(BackendError::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "mock failure",
                )));
            }
            let bytes = std::fs::read(local)?;
            let modified = local_mtime_unix(local).unwrap_or(0);
            self.objects
                .lock()
                .unwrap()
                .insert(key.to_string(), MockObject { bytes, modified });
            Ok(crate::WriteResult {
                cache_path: local.to_path_buf(),
                sha256: String::new(),
                etag: None,
            })
        }

        fn delete(&self, key: &str) -> Result<(), BackendError> {
            if self.fails(key) {
                return Err(BackendError::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "mock failure",
                )));
            }
            let mut objs = self.objects.lock().unwrap();
            objs.remove(key)
                .map(|_| ())
                .ok_or_else(|| BackendError::NotFound(key.to_string()))
        }

        fn walk(&self, prefix: &str) -> Result<Vec<BackendEntry>, BackendError> {
            self.list(prefix)
        }
    }

    fn write_file(root: &Path, rel: &str, bytes: &[u8], mtime: i64) -> PathBuf {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(bytes).unwrap();
        set_mtime(&p, mtime).unwrap();
        p
    }

    fn read_plain(p: &Path) -> String {
        std::fs::read_to_string(p).unwrap()
    }

    fn empty_ignore() -> IgnoreMatcher {
        IgnoreMatcher::empty()
    }

    fn empty_ephemeral(root: &Path) -> EphemeralMatcher {
        EphemeralMatcher::load(root, None).unwrap()
    }

    #[test]
    fn plan_excludes_ignored_and_ephemeral_with_counts() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "src/main.rs", b"fn main() {}", 1000);
        write_file(root, "target/artifact.bin", b"binary", 1000);
        write_file(root, "node_modules/pkg/index.js", b"js", 1000);
        let matcher = IgnoreMatcher::parse("target/\n*.bin\n");
        let ephemeral = empty_ephemeral(root);
        let backend = MockBackend::new();

        let plan = plan_sync(&backend, root, &matcher, &ephemeral, SyncDirection::Push).unwrap();

        let keys: Vec<&str> = plan.to_transfer.iter().map(|i| i.key.as_str()).collect();
        assert_eq!(keys, vec!["src/main.rs"]);
        assert_eq!(plan.skipped_ignored, 1, "target/artifact.bin");
        assert_eq!(plan.skipped_ephemeral, 1, "node_modules/pkg/index.js");
        assert!(plan.to_delete.is_empty());
    }

    #[test]
    fn push_transfers_new_and_changed_only() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "a.txt", b"a", 5000);
        write_file(root, "b.txt", b"b", 6000);
        let backend = MockBackend::new();
        backend.seed("a.txt", b"a", 5000); // equal mtimes -> unchanged on Both
        backend.seed("c.txt", b"c", 7000); // remote-only

        let plan = plan_sync(
            &backend,
            root,
            &empty_ignore(),
            &empty_ephemeral(root),
            SyncDirection::Both,
        )
        .unwrap();

        let keys: Vec<(&str, SyncDirection)> = plan
            .to_transfer
            .iter()
            .map(|i| (i.key.as_str(), i.direction))
            .collect();
        // b.txt: new local -> Push; c.txt: remote-only -> Pull (convergent).
        assert_eq!(
            keys,
            vec![
                ("b.txt", SyncDirection::Push),
                ("c.txt", SyncDirection::Pull)
            ]
        );
        assert!(plan.to_delete.is_empty(), "Both never deletes remote-only");
    }

    #[test]
    fn delete_propagation_respects_ignores_and_direction() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let backend = MockBackend::new();
        backend.seed("gone.txt", b"gone", 1000);
        backend.seed("dead.bin", b"dead", 1000);
        let matcher = IgnoreMatcher::parse("*.bin\n");

        let push = plan_sync(
            &backend,
            root,
            &matcher,
            &empty_ephemeral(root),
            SyncDirection::Push,
        )
        .unwrap();
        assert_eq!(push.to_delete, vec!["gone.txt".to_string()]);
        assert_eq!(push.skipped_ignored, 1, "dead.bin counted, not deleted");

        let pull = plan_sync(
            &backend,
            root,
            &matcher,
            &empty_ephemeral(root),
            SyncDirection::Pull,
        )
        .unwrap();
        assert!(pull.to_delete.is_empty(), "pull never deletes");
        let pull_keys: Vec<&str> = pull.to_transfer.iter().map(|i| i.key.as_str()).collect();
        assert_eq!(pull_keys, vec!["gone.txt"], "remote-only downloads on Pull");

        let both = plan_sync(
            &backend,
            root,
            &matcher,
            &empty_ephemeral(root),
            SyncDirection::Both,
        )
        .unwrap();
        assert!(both.to_delete.is_empty(), "Both never deletes remote-only");
        let both_keys: Vec<&str> = both.to_transfer.iter().map(|i| i.key.as_str()).collect();
        assert_eq!(both_keys, vec!["gone.txt"]);
    }

    #[test]
    fn lww_resolutions_follow_direction_and_tiebreaks() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "remote_newer.txt", b"r", 1000);
        write_file(root, "local_newer.txt", b"l", 3000);
        write_file(root, "equal.txt", b"e", 5000);
        let backend = MockBackend::new();
        backend.seed("remote_newer.txt", b"r", 2000);
        backend.seed("local_newer.txt", b"l", 1000);
        backend.seed("equal.txt", b"e", 5000);
        let eph = empty_ephemeral(root);

        let both = plan_sync(&backend, root, &empty_ignore(), &eph, SyncDirection::Both).unwrap();
        let both_dir: HashMap<&str, SyncDirection> = both
            .to_transfer
            .iter()
            .map(|i| (i.key.as_str(), i.direction))
            .collect();
        assert_eq!(both_dir.get("remote_newer.txt"), Some(&SyncDirection::Pull));
        assert_eq!(both_dir.get("local_newer.txt"), Some(&SyncDirection::Push));
        assert!(
            !both_dir.contains_key("equal.txt"),
            "Both + equal transfers nothing (no ping-pong)"
        );

        // Push-only: local-newer pushes, equal pushes (local wins), remote-newer blocked.
        let push = plan_sync(&backend, root, &empty_ignore(), &eph, SyncDirection::Push).unwrap();
        let push_keys: Vec<&str> = push.to_transfer.iter().map(|i| i.key.as_str()).collect();
        assert_eq!(push_keys, vec!["equal.txt", "local_newer.txt"]);

        // Pull-only: remote-newer pulls, equal pulls (remote wins), local-newer blocked.
        let pull = plan_sync(&backend, root, &empty_ignore(), &eph, SyncDirection::Pull).unwrap();
        let pull_keys: Vec<&str> = pull.to_transfer.iter().map(|i| i.key.as_str()).collect();
        assert_eq!(pull_keys, vec!["equal.txt", "remote_newer.txt"]);
    }

    #[test]
    fn execute_records_conflicts_and_aligns_mtimes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "remote_newer.txt", b"local-old", 1000);
        write_file(root, "local_newer.txt", b"local-new", 3000);
        write_file(root, "brand_new.txt", b"fresh", 4000);
        let backend = MockBackend::new();
        backend.seed("remote_newer.txt", b"remote-new", 2000);
        backend.seed("local_newer.txt", b"remote-old", 1000);

        let plan = plan_sync(
            &backend,
            root,
            &empty_ignore(),
            &empty_ephemeral(root),
            SyncDirection::Both,
        )
        .unwrap();
        let stats = execute_sync(&plan, &backend, root).unwrap();

        assert_eq!(stats.transferred, 3);
        // pull counts dest bytes ("remote-new" = 10); pushes count source
        // bytes ("local-new" = 9, "fresh" = 5).
        assert_eq!(stats.bytes, 10 + 9 + 5);
        // LWW resolutions: remote_newer (RemoteWins), local_newer (LocalWins);
        // brand_new has no counterpart -> no conflict.
        assert_eq!(stats.conflicts.len(), 2);
        let rn = stats
            .conflicts
            .iter()
            .find(|c| c.key == "remote_newer.txt")
            .unwrap();
        assert_eq!(rn.resolved, ResolvedBy::RemoteWins);
        assert_eq!((rn.local_mtime, rn.remote_mtime), (1000, 2000));
        let ln = stats
            .conflicts
            .iter()
            .find(|c| c.key == "local_newer.txt")
            .unwrap();
        assert_eq!(ln.resolved, ResolvedBy::LocalWins);
        assert_eq!((ln.local_mtime, ln.remote_mtime), (3000, 1000));

        // Conflicts persisted to .vfs/sync/conflicts.jsonl (JSONL, one per line).
        let conflicts_path = root.join(".vfs/sync/conflicts.jsonl");
        assert!(conflicts_path.exists());
        let lines: Vec<String> = read_plain(&conflicts_path)
            .lines()
            .map(|l| l.to_string())
            .collect();
        assert_eq!(lines.len(), 2);
        assert!(lines
            .iter()
            .any(|l| l.contains("remote_newer.txt") && l.contains("RemoteWins")));
        assert!(lines
            .iter()
            .any(|l| l.contains("local_newer.txt") && l.contains("LocalWins")));

        // Content + mtime alignment: pull landed remote content with remote mtime;
        // push landed local content with remote mtime aligned (2000? no: local_newer
        // pushed -> remote modified becomes local 3000 -> local aligned to 3000).
        assert_eq!(read_plain(&root.join("remote_newer.txt")), "remote-new");
        assert_eq!(local_mtime_unix(&root.join("remote_newer.txt")), Some(2000));
        assert_eq!(backend.get_bytes("local_newer.txt").unwrap(), b"local-new");
        assert_eq!(local_mtime_unix(&root.join("local_newer.txt")), Some(3000));
        assert_eq!(backend.get_bytes("brand_new.txt").unwrap(), b"fresh");

        // A re-run of the same state plans nothing (idempotent).
        let plan2 = plan_sync(
            &backend,
            root,
            &empty_ignore(),
            &empty_ephemeral(root),
            SyncDirection::Both,
        )
        .unwrap();
        assert!(plan2.to_transfer.is_empty(), "aligned state is a no-op");
    }

    #[test]
    fn partial_failure_stops_with_transfer_failed_and_rerun_completes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Sorted walk order: a_ok.txt first, z_blocked.txt second.
        write_file(root, "a_ok.txt", b"ok", 1000);
        write_file(root, "z_blocked.txt", b"blocked", 1000);
        let backend = MockBackend::new();
        backend.fail("z_blocked.txt");

        let plan = plan_sync(
            &backend,
            root,
            &empty_ignore(),
            &empty_ephemeral(root),
            SyncDirection::Push,
        )
        .unwrap();
        let err = execute_sync(&plan, &backend, root).unwrap_err();
        assert!(
            matches!(&err, SyncError::TransferFailed(k) if k == "z_blocked.txt"),
            "unexpected error: {err:?}"
        );
        // The earlier item was NOT rolled back.
        assert_eq!(backend.get_bytes("a_ok.txt").unwrap(), b"ok");
        assert!(backend.get_bytes("z_blocked.txt").is_none());

        // Idempotent re-run after the failure is cleared completes.
        backend.fail_keys.lock().unwrap().clear();
        let stats = execute_sync(&plan, &backend, root).unwrap();
        assert_eq!(stats.transferred, 2);
        assert_eq!(backend.get_bytes("z_blocked.txt").unwrap(), b"blocked");
    }

    #[test]
    fn delete_failure_also_stops_the_run() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let backend = MockBackend::new();
        backend.seed("gone.txt", b"gone", 1000);
        backend.fail("gone.txt");

        let plan = plan_sync(
            &backend,
            root,
            &empty_ignore(),
            &empty_ephemeral(root),
            SyncDirection::Push,
        )
        .unwrap();
        let err = execute_sync(&plan, &backend, root).unwrap_err();
        assert!(matches!(&err, SyncError::TransferFailed(k) if k == "gone.txt"));
    }

    #[test]
    fn pull_never_deletes_local_and_skips_new_local() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "local_only.txt", b"local", 1000);
        let backend = MockBackend::new();
        backend.seed("remote_only.txt", b"remote", 2000);

        let plan = plan_sync(
            &backend,
            root,
            &empty_ignore(),
            &empty_ephemeral(root),
            SyncDirection::Pull,
        )
        .unwrap();
        assert!(plan.to_delete.is_empty());
        let keys: Vec<&str> = plan.to_transfer.iter().map(|i| i.key.as_str()).collect();
        assert_eq!(keys, vec!["remote_only.txt"]);
        assert!(root.join("local_only.txt").exists());

        execute_sync(&plan, &backend, root).unwrap();
        assert!(root.join("local_only.txt").exists(), "pull never deletes");
        assert_eq!(read_plain(&root.join("remote_only.txt")), "remote");
    }

    #[test]
    fn unicode_and_emoji_keys_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "héllo wörld/emoji 🎉.txt", b"u", 1000);
        let backend = MockBackend::new();
        let plan = plan_sync(
            &backend,
            root,
            &empty_ignore(),
            &empty_ephemeral(root),
            SyncDirection::Push,
        )
        .unwrap();
        assert_eq!(plan.to_transfer.len(), 1);
        assert_eq!(plan.to_transfer[0].key, "héllo wörld/emoji 🎉.txt");
        execute_sync(&plan, &backend, root).unwrap();
        assert_eq!(backend.get_bytes("héllo wörld/emoji 🎉.txt").unwrap(), b"u");
    }

    #[test]
    fn upstream_xattr_override_moves_ephemeral_into_plan() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let p = write_file(root, "node_modules/pkg/index.js", b"js", 1000);
        let backend = MockBackend::new();

        // Without the xattr: ephemeral, skipped.
        let plan = plan_sync(
            &backend,
            root,
            &empty_ignore(),
            &empty_ephemeral(root),
            SyncDirection::Push,
        )
        .unwrap();
        assert!(plan.to_transfer.is_empty());
        assert_eq!(plan.skipped_ephemeral, 1);

        // With user.vfs.sync=upstream: transferred.
        xattr::set(p, "user.vfs.sync", b"upstream").unwrap();
        let plan2 = plan_sync(
            &backend,
            root,
            &empty_ignore(),
            &empty_ephemeral(root),
            SyncDirection::Push,
        )
        .unwrap();
        assert_eq!(plan2.to_transfer.len(), 1);
        assert_eq!(plan2.skipped_ephemeral, 0);
    }

    #[test]
    fn record_conflict_appends_jsonl_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let rec = ConflictRecord {
            key: "a/b.txt".into(),
            local_mtime: 100,
            remote_mtime: 200,
            resolved: ResolvedBy::RemoteWins,
        };
        record_conflict(root, &rec).unwrap();
        record_conflict(root, &rec).unwrap();
        let lines: Vec<String> = read_plain(&root.join(".vfs/sync/conflicts.jsonl"))
            .lines()
            .map(|l| l.to_string())
            .collect();
        assert_eq!(lines.len(), 2);
        let parsed: ConflictRecord = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(parsed, rec);
    }
}
