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
//!   NO-OP in every direction: because every transfer aligns the local
//!   mtime to the remote `modified`, equal mtimes mean neither side changed
//!   since the last sync, and transferring them re-logged a LWW conflict
//!   per file on every repeat run (DF-WARPFS-69). This is a documented
//!   deviation from the spec §7 per-direction tie-break.
//! - A direction-blocked resolution (e.g. remote-newer file during a
//!   `Push`-only run) produces no transfer item; it resolves on the next
//!   two-way run.
//! - Delete propagation (spec §13.9): `Push` deletes remote keys whose local
//!   file is gone and which are not ignored. `Pull` never deletes local files
//!   (no `--prune` in v1). `Both` treats remote-only keys as downloads
//!   (convergent — an agent that just uploaded must not see its file
//!   deleted); explicit deletes happen only on `Push` runs.
//! - Conflicts (DF-WARPFS-67): the ledger records GENUINE divergence only.
//!   Before this fix every executed LWW resolution with a counterpart
//!   appended a [`ConflictRecord`] (even one-sided edits on a clean
//!   incremental push), so the ledger filled with phantom "conflicts" and
//!   stopped meaning conflict. The planner keeps no last-sync baseline
//!   (spec §7 defines none), so a both-sides edit that predates the plan is
//!   provably indistinguishable from a one-sided edit; the genuine class
//!   this module DOES record is the one observable at execution: the
//!   losing side re-stats strictly NEWER than the winning side (spec
//!   §13.11 file-changes-during-sync), i.e. the LWW overwrite clobbers
//!   data that moved on both sides since the last alignment. Routine
//!   resolutions (a one-sided-newer transfer whose counterpart exists)
//!   count in [`SyncStats::resolved`] and never touch
//!   `.vfs/sync/conflicts.jsonl`; new-key copies count only as transfers.
//! - After each transfer the local mtime is aligned to the remote `modified`
//!   (push: local file mtime = remote modified after put; pull: dest mtime =
//!   remote modified), so a repeated sync is a no-op instead of a ping-pong.
//! - Partial failure: `execute_sync` stops at the first failing item and
//!   returns [`SyncError::TransferFailed`] carrying both the item key and
//!   the underlying [`BackendError`] (as its `#[source]`); already
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

impl SyncDirection {
    /// Whether this direction allows uploading local files (Push, Both).
    pub fn allows_upload(self) -> bool {
        matches!(self, SyncDirection::Push | SyncDirection::Both)
    }

    /// Whether this direction allows downloading remote files (Pull, Both).
    pub fn allows_download(self) -> bool {
        matches!(self, SyncDirection::Pull | SyncDirection::Both)
    }
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
    /// Files skipped because they are unmaterialized stream placeholders
    /// (`user.vfs.materialized = "false"`) — no local truth, never pushed (§8.5).
    pub skipped_placeholders: usize,
}

/// Which side won a last-writer-wins resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResolvedBy {
    LocalWins,
    RemoteWins,
}

/// What class of event a ledger row describes (DF-WARPFS-67).
///
/// Before the fix every LWW resolution was written as an undifferentiated
/// "conflict". The planner keeps no last-sync baseline (spec §7 defines
/// none), so the only GENUINE both-sides divergence observable at execution
/// is a transfer whose losing counterpart re-stats strictly newer than the
/// winner (`DivergentOverwrite`) — the overwrite-of-newer-data case the
/// DF-WARPFS-67 finding names. Rows written by this version always carry
/// `DivergentOverwrite`; `LwwResolution` is the serde default so pre-fix
/// rows in an existing `conflicts.jsonl` still decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ConflictKind {
    /// Legacy class: every LWW resolution (including one-sided edits) was
    /// recorded. Only appears in rows written before DF-WARPFS-67.
    #[serde(rename = "lww_resolution")]
    #[default]
    LwwResolution,
    /// Genuine divergence: the transfer overwrote a counterpart that was
    /// strictly newer than the incoming side at execution time (both sides
    /// changed since the last alignment; the LWW winner clobbers newer data).
    #[serde(rename = "divergent_overwrite")]
    DivergentOverwrite,
}

/// One genuine conflict, persisted to `.vfs/sync/conflicts.jsonl`.
///
/// DF-WARPFS-67: routine LWW resolutions (one-sided newer edits, new keys)
/// are NO LONGER written here — only `DivergentOverwrite` events are.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictRecord {
    pub key: String,
    /// Local mtime at resolution time, unix seconds (pre-transfer).
    pub local_mtime: i64,
    /// Remote modified at resolution time, unix seconds (pre-transfer).
    pub remote_mtime: i64,
    pub resolved: ResolvedBy,
    /// DF-WARPFS-67 classification. Absent on pre-fix rows, where it
    /// deserializes as `LwwResolution`.
    #[serde(default)]
    pub kind: ConflictKind,
}

/// Result of [`execute_sync`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncStats {
    /// Number of transferred items (pushes + pulls; deletes not counted).
    pub transferred: usize,
    /// Bytes transferred (source-side sizes).
    pub bytes: u64,
    /// Genuine divergent overwrites recorded this run (DF-WARPFS-67; the
    /// only rows appended to `.vfs/sync/conflicts.jsonl`).
    pub conflicts: Vec<ConflictRecord>,
    /// Routine LWW resolutions this run — transfers that had a counterpart
    /// which did NOT change past the winner (one-sided edits; DF-WARPFS-67).
    /// Never conflicts, never persisted to the ledger. New-key transfers
    /// (no counterpart) count only in `transferred`.
    pub resolved: usize,
}

/// Sync engine errors (spec §12: `SyncError::TransferFailed`).
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("transfer failed: {key}: {source}")]
    TransferFailed {
        /// Key of the item whose transfer/delete failed.
        key: String,
        /// Underlying backend failure (retained for diagnosis).
        #[source]
        source: BackendError,
    },
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
        // §8.5: unmaterialized stream placeholders have no local truth —
        // never pushed (a zero-byte placeholder must not clobber the remote
        // object it stands in for).
        if is_unmaterialized_placeholder(&full_path) {
            plan.skipped_placeholders += 1;
            continue;
        }
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
                } else if remote_mtime > local_mtime
                    && matches!(direction, SyncDirection::Pull | SyncDirection::Both)
                {
                    let pull_item = TransferItem {
                        direction: SyncDirection::Pull,
                        ..item
                    };
                    plan.to_transfer.push(pull_item);
                    // Push-only: remote newer is direction-blocked.
                }
                // Equal mtimes (any direction): no transfer. This is a
                // documented deviation from the spec §7 per-direction
                // tie-break ("Equal: local wins on Push / remote wins on
                // Pull"). Because every transfer aligns the local mtime to
                // the remote `modified`, equal mtimes are the steady state
                // after a first sync — transferring them re-resolves the
                // full set and re-logs a LWW conflict per file on EVERY
                // repeat run (DF-WARPFS-69). The post-transfer alignment
                // means the equal case carries no information: neither side
                // changed since the last sync.
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
/// [`SyncError::TransferFailed`] carrying the item key and the underlying
/// [`BackendError`] as its `#[source]`; already-transferred
/// items are not rolled back.
///
/// Only genuine divergent overwrites (a transfer whose counterpart re-stats
/// strictly newer than the incoming side — see [`execute_transfer`],
/// DF-WARPFS-67) append a [`ConflictRecord`] to `.vfs/sync/conflicts.jsonl`
/// via [`record_conflict`]. Routine one-sided resolutions increment
/// [`SyncStats::resolved`] without touching the ledger. Local mtimes are
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
        backend.delete(key).map_err(|e| SyncError::TransferFailed {
            key: key.clone(),
            source: e,
        })?;
    }

    for rec in &stats.conflicts {
        record_conflict(workspace_root, rec)?;
    }

    Ok(stats)
}

/// Execute one transfer item (push or pull): classify the outcome (DF-WARPFS-67),
/// transfer, count bytes, and align local mtime to the remote `modified` so a
/// repeated sync is a no-op.
///
/// Classification against the item's counterpart, observed at execution time
/// (spec §13.11 — either side may have moved between plan and execute):
/// - counterpart absent (new key) or not changed past the winner → the
///   transfer is a ROUTINE resolution: `stats.resolved += 1` when a
///   counterpart existed, and NO ledger row.
/// - counterpart strictly newer than the incoming side (a push over a
///   remote that has since moved ahead, or a pull over a local file that
///   has since been edited) → the LWW winner clobbers data that moved on
///   BOTH sides since the last alignment: a genuine
///   [`ConflictKind::DivergentOverwrite`], recorded with the pre-transfer
///   mtimes.
///
/// LIMITATION (documented honestly, DF-WARPFS-67): the spec (§7) defines no
/// last-sync baseline, so a both-sides divergence whose LWW winner is the
/// side that is still strictly newer (e.g. remote edited twice, local once)
/// is mtime-indistinguishable from a one-sided edit at plan time and is
/// classified as routine. Only divergence observable at execution — the
/// overwrite of a newer counterpart — is recorded; nothing is fabricated.
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
            let local_mtime = local_mtime_unix(&item.local_path).unwrap_or(0);
            if let Some(ro) = counterpart {
                let remote_mtime = ro.modified.unwrap_or(0);
                if remote_mtime > local_mtime {
                    // Divergent overwrite: the push clobbers a remote that
                    // is newer than the local side being pushed (moved after
                    // the plan chose Push — both sides changed).
                    stats.conflicts.push(ConflictRecord {
                        key: item.key.clone(),
                        local_mtime,
                        remote_mtime,
                        resolved: ResolvedBy::LocalWins,
                        kind: ConflictKind::DivergentOverwrite,
                    });
                } else {
                    // Routine: local is the newer side, remote unchanged past it.
                    stats.resolved += 1;
                }
            }
            backend
                .put(&item.local_path, &item.key)
                .map_err(|e| SyncError::TransferFailed {
                    key: item.key.clone(),
                    source: e,
                })?;
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
                let local_mtime = local_mtime_unix(&item.local_path).unwrap_or(0);
                match ro.modified {
                    Some(remote_mtime) if local_mtime > remote_mtime => {
                        // Divergent overwrite: the pull clobbers a local file
                        // newer than the remote side being pulled (edited
                        // after the plan chose Pull — both sides changed).
                        stats.conflicts.push(ConflictRecord {
                            key: item.key.clone(),
                            local_mtime,
                            remote_mtime,
                            resolved: ResolvedBy::RemoteWins,
                            kind: ConflictKind::DivergentOverwrite,
                        });
                    }
                    // Routine: remote is the newer (or equal) side, or the
                    // stat gave no timestamp to dispute — nothing is
                    // recorded without an observable divergence.
                    _ => stats.resolved += 1,
                }
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
                .map_err(|e| SyncError::TransferFailed {
                    key: item.key.clone(),
                    source: e,
                })?;
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

/// Append one genuine conflict (DF-WARPFS-67: only divergent overwrites are
/// recorded by `execute_sync`) to `.vfs/sync/conflicts.jsonl` (JSONL, one
/// record per line), creating the directory as needed. Routine LWW
/// resolutions never reach this ledger.
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

/// Whether `path` is an unmaterialized stream placeholder: it carries a
/// `user.vfs.remote` marker and `user.vfs.materialized != "true"` (§8.5).
/// Such files have no local truth and are local-only by definition — they
/// are never pushed (a zero-byte placeholder must never clobber the remote
/// object it stands in for).
pub fn is_unmaterialized_placeholder(path: &Path) -> bool {
    vfs_xattr(path, "remote").is_some()
        && vfs_xattr(path, "materialized").as_deref() != Some("true")
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

        /// Concrete cause asserted by the TransferFailed tests (extracted so
        /// get/put/delete agree with the expected message byte-for-byte).
        fn fail_cause(key: &str) -> BackendError {
            BackendError::Unreachable(format!("backend refused transfer of {key}"))
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
                return Err(Self::fail_cause(key));
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
                return Err(Self::fail_cause(key));
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
                return Err(Self::fail_cause(key));
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

        // Push-only: local-newer pushes; equal and remote-newer are no-ops/blocked.
        let push = plan_sync(&backend, root, &empty_ignore(), &eph, SyncDirection::Push).unwrap();
        let push_keys: Vec<&str> = push.to_transfer.iter().map(|i| i.key.as_str()).collect();
        assert_eq!(push_keys, vec!["local_newer.txt"]);

        // Pull-only: remote-newer pulls; equal and local-newer are no-ops/blocked.
        let pull = plan_sync(&backend, root, &empty_ignore(), &eph, SyncDirection::Pull).unwrap();
        let pull_keys: Vec<&str> = pull.to_transfer.iter().map(|i| i.key.as_str()).collect();
        assert_eq!(pull_keys, vec!["remote_newer.txt"]);
    }

    /// DF-WARPFS-67 regression (the traceability test named in the board
    /// row): a clean sync whose transfers are ROUTINE one-sided LWW
    /// resolutions (local-newer push, remote-newer pull, new key) records
    /// ZERO conflicts and appends ZERO ledger rows, while alignment and
    /// transfer counts keep working. A genuine divergence — the losing
    /// side moved past the winner between plan and execute (spec §13.11) —
    /// is still classified, recorded with pre-transfer mtimes, and
    /// persisted as `kind: "divergent_overwrite"`.
    ///
    /// ch:trace row=DF-WARPFS-67 spec=specs/backend-backed-workspace-spec.md#7 wave=none:serial \
    ///   test=hilo-backends/src/planner.rs::routine_resolution_is_not_conflict_and_genuine_divergence_is_classified \
    ///   doc=hilo-cli/src/commands/backend.rs evidence=none:worker-commit-only-until-foreman-verification \
    ///   witness=none:worker-branch verdict=none:pending-foreman-judge commit=none:self-referential-hash \
    ///   memory=none:pending
    ///
    /// LIMITATION (proven honestly, not fabricated): with no last-sync
    /// baseline in the planner state (spec §7 defines none), a both-sides
    /// edit where the LWW winner remains the newer side is
    /// mtime-indistinguishable from a one-sided edit at plan time — see
    /// `pre_plan_both_sides_divergence_is_not_fabricated` for the contract
    /// test pinning that this is NOT logged as a conflict. The genuine
    /// class recorded here is the observable overwrite-of-newer-counterpart.
    #[test]
    fn routine_resolution_is_not_conflict_and_genuine_divergence_is_classified() {
        // Part 1: routine resolutions — one-sided edits + a new key.
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
        // DF-WARPFS-67: remote_newer (RemoteWins) and local_newer
        // (LocalWins) are one-sided — routine resolutions, counted in
        // `resolved`, NOT conflicts. brand_new has no counterpart: only a
        // transfer. The clean sync must print "0 conflicts".
        assert_eq!(
            stats.conflicts.len(),
            0,
            "routine resolutions are not conflicts"
        );
        assert_eq!(
            stats.resolved, 2,
            "both one-sided transfers count as resolved"
        );

        // Zero ledger rows: conflicts.jsonl is never created by a clean run.
        assert!(
            !root.join(".vfs/sync/conflicts.jsonl").exists(),
            "a clean incremental sync must append ZERO rows to conflicts.jsonl"
        );

        // Content + mtime alignment still happen (DF-WARPFS-69 contract).
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

        // Part 2a: genuine divergence, push side — the plan chose Push
        // (local newer), then the REMOTE moved ahead before execution
        // (spec §13.11: file changes during sync; two agents editing).
        // The push clobbers a strictly-newer remote: both sides changed —
        // must be recorded.
        let tmp_push = tempfile::tempdir().unwrap();
        let root_push = tmp_push.path();
        write_file(root_push, "shared.txt", b"local-v2", 3000);
        let backend_push = MockBackend::new();
        backend_push.seed("shared.txt", b"remote-v1", 1000);
        let plan_push = plan_sync(
            &backend_push,
            root_push,
            &empty_ignore(),
            &empty_ephemeral(root_push),
            SyncDirection::Both,
        )
        .unwrap();
        assert!(
            matches!(plan_push.to_transfer[0].direction, SyncDirection::Push),
            "local-newer plans Push"
        );
        // Remote edits DURING the run window: modified jumps past local.
        backend_push.seed("shared.txt", b"remote-v2", 9000);
        let stats_push = execute_sync(&plan_push, &backend_push, root_push).unwrap();
        assert_eq!(
            stats_push.conflicts.len(),
            1,
            "divergent overwrite is recorded"
        );
        assert_eq!(stats_push.resolved, 0);
        let rec = &stats_push.conflicts[0];
        assert_eq!(rec.key, "shared.txt");
        assert_eq!(rec.resolved, ResolvedBy::LocalWins);
        assert_eq!(rec.kind, ConflictKind::DivergentOverwrite);
        // Pre-transfer mtimes: local 3000 vs the newer remote 9000.
        assert_eq!((rec.local_mtime, rec.remote_mtime), (3000, 9000));
        let lines_push = read_plain(&root_push.join(".vfs/sync/conflicts.jsonl"));
        assert_eq!(lines_push.lines().count(), 1);
        assert!(lines_push.contains("divergent_overwrite"), "{lines_push}");

        // Part 2b: genuine divergence, pull side — the plan chose Pull
        // (remote newer), then LOCAL was edited past the remote before
        // execution. The pull clobbers a strictly-newer local file.
        let tmp_pull = tempfile::tempdir().unwrap();
        let root_pull = tmp_pull.path();
        write_file(root_pull, "shared.txt", b"local-v1", 1000);
        let backend_pull = MockBackend::new();
        backend_pull.seed("shared.txt", b"remote-v2", 3000);
        let plan_pull = plan_sync(
            &backend_pull,
            root_pull,
            &empty_ignore(),
            &empty_ephemeral(root_pull),
            SyncDirection::Both,
        )
        .unwrap();
        assert!(
            matches!(plan_pull.to_transfer[0].direction, SyncDirection::Pull),
            "remote-newer plans Pull"
        );
        // Local edits DURING the run window: mtime jumps past remote.
        write_file(root_pull, "shared.txt", b"local-v2", 9000);
        let stats_pull = execute_sync(&plan_pull, &backend_pull, root_pull).unwrap();
        assert_eq!(
            stats_pull.conflicts.len(),
            1,
            "divergent overwrite is recorded"
        );
        assert_eq!(stats_pull.resolved, 0);
        let rec = &stats_pull.conflicts[0];
        assert_eq!(rec.key, "shared.txt");
        assert_eq!(rec.resolved, ResolvedBy::RemoteWins);
        assert_eq!(rec.kind, ConflictKind::DivergentOverwrite);
        // Pre-transfer mtimes: the newer local 9000 vs remote 3000.
        assert_eq!((rec.local_mtime, rec.remote_mtime), (9000, 3000));
        let lines_pull = read_plain(&root_pull.join(".vfs/sync/conflicts.jsonl"));
        assert_eq!(lines_pull.lines().count(), 1);
        assert!(lines_pull.contains("divergent_overwrite"), "{lines_pull}");
    }

    /// DF-WARPFS-67 limitation contract (proves the boundary rather than
    /// fabricating detection): when BOTH sides changed BEFORE the plan runs
    /// and the LWW winner is still the strictly-newer side, the planner has
    /// no persistent last-sync baseline (spec §7 defines none), so this
    /// state is mtime-indistinguishable from a one-sided edit. The honest
    /// contract: it transfers (LWW), counts as routine, and does NOT
    /// write a phantom ledger row. A false "both sides changed" claim is
    /// worse than none — the ledger's meaning is the deliverable.
    #[test]
    fn pre_plan_both_sides_divergence_is_not_fabricated() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // local edited (1000 -> 2000), remote edited more (1000 -> 5000):
        // both sides changed since some past point; only remote's number
        // survives in the current state.
        write_file(root, "shared.txt", b"local-v2", 2000);
        let backend = MockBackend::new();
        backend.seed("shared.txt", b"remote-v2", 5000);

        let plan = plan_sync(
            &backend,
            root,
            &empty_ignore(),
            &empty_ephemeral(root),
            SyncDirection::Both,
        )
        .unwrap();
        assert_eq!(plan.to_transfer.len(), 1);
        let stats = execute_sync(&plan, &backend, root).unwrap();

        // Observable state says "remote newer": routine pull, no row.
        assert_eq!(stats.conflicts.len(), 0);
        assert_eq!(stats.resolved, 1);
        assert!(!root.join(".vfs/sync/conflicts.jsonl").exists());
    }

    /// DF-WARPFS-69 regression: a repeat sync over an aligned (equal-mtime)
    /// set must plan ZERO transfers and record ZERO new conflicts, in every
    /// direction. Before the fix, `Pull`/`Push` runs re-transferred the whole
    /// equal set (per-direction tie-break) and appended a LWW conflict row
    /// per file on every run.
    #[test]
    fn repeat_sync_on_aligned_set_is_a_full_no_op() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "a.txt", b"same", 2000);
        write_file(root, "b.txt", b"same", 2000);
        let backend = MockBackend::new();
        backend.seed("a.txt", b"same", 2000);
        backend.seed("b.txt", b"same", 2000);
        let eph = empty_ephemeral(root);

        for direction in [
            SyncDirection::Push,
            SyncDirection::Pull,
            SyncDirection::Both,
        ] {
            let plan = plan_sync(&backend, root, &empty_ignore(), &eph, direction).unwrap();
            assert!(
                plan.to_transfer.is_empty(),
                "{direction:?}: aligned set must plan no transfers"
            );
            assert!(plan.to_delete.is_empty());
            let conflicts_before = root.join(".vfs/sync/conflicts.jsonl");
            let n_before = if conflicts_before.exists() {
                read_plain(&conflicts_before).lines().count()
            } else {
                0
            };
            let stats = execute_sync(&plan, &backend, root).unwrap();
            assert_eq!(stats.transferred, 0, "{direction:?}: zero transfers");
            assert!(stats.conflicts.is_empty(), "{direction:?}: zero conflicts");
            let n_after = if conflicts_before.exists() {
                read_plain(&conflicts_before).lines().count()
            } else {
                0
            };
            assert_eq!(
                n_before, n_after,
                "{direction:?}: conflicts.jsonl unchanged"
            );
        }
    }

    // A genuinely changed file (mtime differs) still transfers — one-sided
    // so it is a ROUTINE resolution, not a conflict row (DF-WARPFS-67).
    #[test]
    fn changed_file_still_transfers_without_a_conflict_row() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "changed.txt", b"local-old", 1000);
        let backend = MockBackend::new();
        backend.seed("changed.txt", b"remote-new", 5000);
        let eph = empty_ephemeral(root);

        let plan = plan_sync(&backend, root, &empty_ignore(), &eph, SyncDirection::Pull).unwrap();
        assert_eq!(plan.to_transfer.len(), 1);
        let stats = execute_sync(&plan, &backend, root).unwrap();
        assert_eq!(stats.transferred, 1);
        assert_eq!(
            stats.conflicts.len(),
            0,
            "one-sided edit is routine (DF-WARPFS-67)"
        );
        assert_eq!(stats.resolved, 1);

        // Second pull: mtime aligned to 5000 -> full no-op (AC1 shape).
        let plan2 = plan_sync(&backend, root, &empty_ignore(), &eph, SyncDirection::Pull).unwrap();
        assert!(plan2.to_transfer.is_empty());
        // The ledger was never written: zero rows on a clean one-sided sync.
        assert!(!root.join(".vfs/sync/conflicts.jsonl").exists());
        let stats2 = execute_sync(&plan2, &backend, root).unwrap();
        assert_eq!(stats2.transferred, 0);
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
        let SyncError::TransferFailed { key, source } = &err else {
            panic!("unexpected error: {err:?}");
        };
        assert_eq!(key, "z_blocked.txt");
        assert!(
            source
                .to_string()
                .contains("backend refused transfer of z_blocked.txt"),
            "cause must carry the concrete backend failure, got {source:?}"
        );
        // Rendered error includes both the key and the underlying cause.
        let rendered = err.to_string();
        assert!(
            rendered.contains("z_blocked.txt") && rendered.contains("backend refused transfer"),
            "rendered error must include key + cause, got: {rendered}"
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
        let SyncError::TransferFailed { key, source } = &err else {
            panic!("unexpected error: {err:?}");
        };
        assert_eq!(key, "gone.txt");
        assert!(
            source
                .to_string()
                .contains("backend refused transfer of gone.txt"),
            "cause must carry the concrete backend failure, got {source:?}"
        );
        let rendered = err.to_string();
        assert!(
            rendered.contains("gone.txt") && rendered.contains("backend refused transfer"),
            "rendered error must include key + cause, got: {rendered}"
        );
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
    fn unmaterialized_placeholder_is_never_pushed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // A real local file (should transfer) and a stream placeholder
        // (zero-byte, user.vfs.remote + materialized=false — no local truth).
        write_file(root, "src/main.rs", b"fn main() {}", 1000);
        let ph = write_file(root, "assets/data.bin", b"", 1000);
        xattr::set(&ph, "user.vfs.remote", b"assets/data.bin").unwrap();
        xattr::set(&ph, "user.vfs.materialized", b"false").unwrap();

        let backend = MockBackend::new();
        let plan = plan_sync(
            &backend,
            root,
            &empty_ignore(),
            &empty_ephemeral(root),
            SyncDirection::Push,
        )
        .unwrap();

        // §8.5: the placeholder is counted as skipped_placeholders and never
        // transferred — a zero-byte placeholder must not clobber the remote
        // object it stands in for.
        assert_eq!(plan.skipped_placeholders, 1);
        assert!(
            !plan.to_transfer.iter().any(|i| i.key == "assets/data.bin"),
            "placeholder must not be in the transfer plan"
        );
        assert_eq!(plan.to_transfer.len(), 1);
        assert_eq!(plan.to_transfer[0].key, "src/main.rs");

        // A materialized placeholder (same markers, materialized=true) is a
        // real local file again and transfers normally.
        xattr::set(&ph, "user.vfs.materialized", b"true").unwrap();
        let plan2 = plan_sync(
            &backend,
            root,
            &empty_ignore(),
            &empty_ephemeral(root),
            SyncDirection::Push,
        )
        .unwrap();
        assert_eq!(plan2.skipped_placeholders, 0);
        assert_eq!(plan2.to_transfer.len(), 2);
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
            kind: ConflictKind::DivergentOverwrite,
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

    /// DF-WARPFS-67 ledger compat: rows written before the fix (no `kind`
    /// field) decode as `LwwResolution`; rows written now carry the
    /// explicit `divergent_overwrite` tag so consumers can tell the classes
    /// apart in one file.
    #[test]
    fn legacy_conflict_rows_decode_as_lww_resolution() {
        let legacy = r#"{"key":"a.txt","local_mtime":1,"remote_mtime":2,"resolved":"RemoteWins"}"#;
        let parsed: ConflictRecord = serde_json::from_str(legacy).unwrap();
        assert_eq!(parsed.kind, ConflictKind::LwwResolution);

        let line = serde_json::to_string(&ConflictRecord {
            key: "a.txt".into(),
            local_mtime: 1,
            remote_mtime: 2,
            resolved: ResolvedBy::RemoteWins,
            kind: ConflictKind::DivergentOverwrite,
        })
        .unwrap();
        assert!(line.contains("\"kind\":\"divergent_overwrite\""), "{line}");
        let round: ConflictRecord = serde_json::from_str(&line).unwrap();
        assert_eq!(round.kind, ConflictKind::DivergentOverwrite);
    }
}
