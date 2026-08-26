//! Spec §7.1 backend sync hook — inotify watcher loop gains a sync hook.
//!
//! Event flow (spec §7.1):
//!
//! ```text
//! inotify event → debounce (HILO_DEBOUNCE_MS, default 250)
//!   → resolve changed path relative to workspace root
//!   → IgnoreMatcher::matches? → skip (local-only), done
//!   → EphemeralMatcher::classify == Ephemeral && sync != upstream? → skip, done
//!   → mark dirty; batch dirty keys; when batch settles (no events for 500ms)
//!   → execute_sync(Push) for the batch via the mount's backend
//! ```
//!
//! Mount polling for Pull: every `poll_secs` (default 60), `plan_sync(Pull)` +
//! `execute_sync`. Polling is the v1 pull mechanism (tool-agnostic; no reliance
//! on per-tool watch modes).
//!
//! The hook is shared with the flush/poll background tasks via
//! `Arc<Mutex<SyncHook>>`: the engine's event loop calls [`SyncHook::record_event`]
//! under a short lock, and the spawned tasks lock only to flush/poll (all sync
//! work is synchronous — no await while holding the lock).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hilo_backends::{
    planner::{self, SyncDirection, SyncError, SyncStats},
    BackendError, BackendRegistry, EphemeralClass, EphemeralMatcher, IgnoreMatcher, MountEntry,
};

/// Spec §7.1 debounce default (configurable via HILO_DEBOUNCE_MS).
pub const DEFAULT_DEBOUNCE_MS: u64 = 250;
/// Spec §7.1 batch-settle window: no events for 500ms → flush.
pub const DEFAULT_SETTLE_MS: u64 = 500;
/// Spec §7.1 pull poll default.
pub const DEFAULT_POLL_SECS: u64 = 60;

/// Configuration for a sync hook attached to one workspace.
#[derive(Debug, Clone)]
pub struct SyncHookConfig {
    /// Workspace root; changed paths are resolved relative to it.
    pub workspace_root: PathBuf,
    /// `.vfs/backends/mounts.yaml` (spec §11.3) — the mounted backends.
    pub mounts_yaml: PathBuf,
    /// Per-path debounce window (HILO_DEBOUNCE_MS, default 250).
    pub debounce_ms: u64,
    /// Quiet window before the dirty batch is pushed (default 500).
    pub settle_ms: u64,
    /// Default pull poll interval in seconds (default 60); per-mount
    /// `poll_secs` from mounts.yaml wins when present.
    pub poll_secs: u64,
}

/// One mount's pre-built matchers (name lives on the parallel `MountEntry`).
struct MountMatcher {
    ignore: IgnoreMatcher,
    ephemeral: EphemeralMatcher,
    poll_secs: u64,
}

/// The spec §7.1 sync hook state. Not thread-safe by itself — share via
/// `Arc<Mutex<SyncHook>>` between the engine loop and the spawned tasks.
pub struct SyncHook {
    cfg: SyncHookConfig,
    registry: BackendRegistry,
    mounts: Vec<MountEntry>,
    matchers: Vec<MountMatcher>,
    /// Relative paths (unix separators) with pending changes.
    dirty: HashSet<String>,
    /// Per-path last-accepted instant (debounce window).
    last_recorded: HashMap<String, Instant>,
    /// When the most recent accepted event arrived.
    last_event: Option<Instant>,
}

impl SyncHook {
    /// Load mounts.yaml, build the per-mount ignore/ephemeral matchers, and
    /// construct the hook. Errors are configuration errors (InvalidConfig).
    pub fn new(cfg: SyncHookConfig) -> Result<Self, BackendError> {
        let registry = BackendRegistry::load_mounts(&cfg.mounts_yaml)?;
        let text = std::fs::read_to_string(&cfg.mounts_yaml).map_err(|e| {
            BackendError::InvalidConfig(format!("cannot read {}: {e}", cfg.mounts_yaml.display()))
        })?;
        let mounts: Vec<MountEntry> = serde_yaml::from_str(&text)
            .map_err(|e| BackendError::InvalidConfig(format!("bad mounts.yaml: {e}")))?;
        if mounts.is_empty() {
            return Err(BackendError::InvalidConfig(
                "no mounts in mounts.yaml — run 'hilo backend mount' first".into(),
            ));
        }
        let mut matchers = Vec::new();
        for entry in &mounts {
            let ignore = IgnoreMatcher::load(
                &cfg.workspace_root,
                entry.ignore_file.as_deref().map(Path::new),
                entry.no_default_ignores.unwrap_or(false),
            )
            .map_err(|e| {
                BackendError::InvalidConfig(format!(
                    "ignore rules for {} (ignore_file={:?}): {e}",
                    entry.name, entry.ignore_file
                ))
            })?;
            let ephemeral = EphemeralMatcher::load(&cfg.workspace_root, None).map_err(|e| {
                BackendError::InvalidConfig(format!("ephemeral rules for {}: {e}", entry.name))
            })?;
            matchers.push(MountMatcher {
                ignore,
                ephemeral,
                poll_secs: entry.poll_secs.unwrap_or(cfg.poll_secs),
            });
        }
        Ok(Self {
            cfg,
            registry,
            mounts,
            matchers,
            dirty: HashSet::new(),
            last_recorded: HashMap::new(),
            last_event: None,
        })
    }

    /// Number of mounted backends the hook syncs to.
    pub fn mount_count(&self) -> usize {
        self.mounts.len()
    }

    /// Number of dirty (pending push) paths.
    pub fn dirty_len(&self) -> usize {
        self.dirty.len()
    }

    /// True when at least one path awaits a settle flush.
    pub fn has_dirty(&self) -> bool {
        !self.dirty.is_empty()
    }

    /// Record one changed path (spec §7.1 flow).
    ///
    /// Steps: resolve relative to the workspace root (events outside the root
    /// are ignored), debounce per path, skip ignored paths (git-exact
    /// matching), skip ephemeral paths unless `user.vfs.sync=upstream`, then
    /// mark the path dirty and re-arm the settle timer.
    pub fn record_event(&mut self, path: &Path) {
        let rel = match path.strip_prefix(&self.cfg.workspace_root) {
            Ok(r) => r,
            Err(_) => return, // outside the workspace
        };
        if rel.as_os_str().is_empty() {
            return;
        }
        let rel_str = rel.to_string_lossy().replace('\\', "/");

        // Debounce: the same path within the window does not re-arm the batch.
        if let Some(last) = self.last_recorded.get(&rel_str) {
            if last.elapsed() < Duration::from_millis(self.cfg.debounce_ms) {
                return;
            }
        }

        let is_dir = rel.is_dir();

        // Ignore check: a file matched by ANY mount's ignore rules is
        // local-only — it is never transferred (spec §4/§7.1).
        if self
            .matchers
            .iter()
            .any(|m| m.ignore.matches(&rel_str, is_dir))
        {
            return;
        }

        // Ephemeral check: skipped unless the file opts into upstream sync.
        let full = self.cfg.workspace_root.join(rel);
        for m in &self.matchers {
            let eph = m
                .ephemeral
                .classify(rel, is_dir, planner::xattr_ephemeral_bool(&full))
                == EphemeralClass::Ephemeral;
            if eph && !planner::is_upstream_override(&full) {
                return;
            }
        }

        self.last_recorded.insert(rel_str.clone(), Instant::now());
        self.dirty.insert(rel_str);
        self.last_event = Some(Instant::now());
    }

    /// True when the batch has been quiet for at least `settle_ms`.
    pub fn settle_elapsed(&self) -> bool {
        match self.last_event {
            Some(t) => t.elapsed() >= Duration::from_millis(self.cfg.settle_ms),
            None => false,
        }
    }

    /// Push the dirty batch to every mount (spec §7.1 settle flush).
    ///
    /// The plan is filtered to the batch's keys; deletes are NOT propagated
    /// by the hook (remote-only keys only leave on a full
    /// `hilo backend sync --push`, spec §13.9). The dirty set is drained
    /// first so a failing backend cannot loop the flush forever — events that
    /// arrive during the flush re-arm it for the next settle.
    pub fn flush_dirty(&mut self) -> Result<Vec<SyncStats>, SyncError> {
        let batch: Vec<String> = self.dirty.drain().collect();
        if batch.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for (entry, m) in self.mounts.iter().zip(&self.matchers) {
            let backend = self.registry.get(&entry.name).ok_or_else(|| {
                SyncError::Backend(BackendError::InvalidConfig(format!(
                    "backend {} missing from registry",
                    entry.name
                )))
            })?;
            let mut plan = planner::plan_sync(
                backend.as_ref(),
                &self.cfg.workspace_root,
                &m.ignore,
                &m.ephemeral,
                SyncDirection::Push,
            )?;
            plan.to_transfer
                .retain(|item| batch.iter().any(|k| k == &item.key));
            plan.to_delete.clear();
            if plan.to_transfer.is_empty() {
                continue;
            }
            out.push(planner::execute_sync(
                &plan,
                backend.as_ref(),
                &self.cfg.workspace_root,
            )?);
        }
        Ok(out)
    }

    /// Spec §7.1 mount polling: `plan_sync(Pull)` + `execute_sync` for every
    /// mount. Pull treats remote-only keys as downloads (convergent — an
    /// agent that just uploaded must not see its file deleted).
    pub fn poll_pull(&mut self) -> Result<Vec<SyncStats>, SyncError> {
        let mut out = Vec::new();
        for (entry, m) in self.mounts.iter().zip(&self.matchers) {
            let backend = self.registry.get(&entry.name).ok_or_else(|| {
                SyncError::Backend(BackendError::InvalidConfig(format!(
                    "backend {} missing from registry",
                    entry.name
                )))
            })?;
            let plan = planner::plan_sync(
                backend.as_ref(),
                &self.cfg.workspace_root,
                &m.ignore,
                &m.ephemeral,
                SyncDirection::Pull,
            )?;
            if plan.to_transfer.is_empty() {
                continue;
            }
            out.push(planner::execute_sync(
                &plan,
                backend.as_ref(),
                &self.cfg.workspace_root,
            )?);
        }
        Ok(out)
    }
}

/// Spawn the settle-flush loop: polls every 100ms and flushes the dirty batch
/// once it has been quiet for `settle_ms`.
pub fn spawn_flush_task(hook: Arc<Mutex<SyncHook>>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let mut h = match hook.lock() {
                Ok(h) => h,
                Err(_) => continue, // poisoned — keep polling
            };
            if h.has_dirty() && h.settle_elapsed() {
                match h.flush_dirty() {
                    Ok(stats) => {
                        for s in &stats {
                            tracing::info!(
                                "[sync-hook] pushed batch: {} transferred ({} bytes), {} conflicts",
                                s.transferred,
                                s.bytes,
                                s.conflicts.len()
                            );
                        }
                    }
                    Err(e) => tracing::info!("[sync-hook] flush failed: {e}"),
                }
            }
        }
    })
}

/// Spawn one pull-poll loop per mount, each at its own `poll_secs` interval.
pub fn spawn_poll_tasks(hook: Arc<Mutex<SyncHook>>) -> Vec<tokio::task::JoinHandle<()>> {
    let intervals: Vec<u64> = match hook.lock() {
        Ok(h) => h.matchers.iter().map(|m| m.poll_secs).collect(),
        Err(_) => return Vec::new(),
    };
    intervals
        .into_iter()
        .map(|secs| {
            let hook = hook.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(secs.max(1))).await;
                    let mut h = match hook.lock() {
                        Ok(h) => h,
                        Err(_) => continue,
                    };
                    match h.poll_pull() {
                        Ok(stats) => {
                            for s in &stats {
                                tracing::info!(
                                    "[sync-hook] pull: {} transferred ({} bytes), {} conflicts",
                                    s.transferred,
                                    s.bytes,
                                    s.conflicts.len()
                                );
                            }
                        }
                        Err(e) => tracing::info!("[sync-hook] poll pull failed: {e}"),
                    }
                }
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a temp workspace with a `.hiloignore` (ignore *.tmp), a
    /// `.hiloephemeral` (cache/ is ephemeral) and one local backend mount
    /// whose root is a second temp dir. Returns (workspace, backend_root).
    fn test_workspace() -> (tempfile::TempDir, tempfile::TempDir) {
        let ws = tempfile::TempDir::new().unwrap();
        let backend = tempfile::TempDir::new().unwrap();
        std::fs::write(ws.path().join(".hiloignore"), "*.tmp\n").unwrap();
        std::fs::write(ws.path().join(".hiloephemeral"), "cache/\n").unwrap();
        let entry = MountEntry {
            name: "loc".into(),
            kind: "local".into(),
            bucket: None,
            prefix: Some(backend.path().to_string_lossy().into_owned()),
            region: None,
            remote: None,
            tool: None,
            mode: None,
            ignore_file: None,
            poll_secs: Some(60),
            no_default_ignores: None,
            at: None,
        };
        let yaml = serde_yaml::to_string(&vec![entry]).unwrap();
        std::fs::create_dir_all(ws.path().join(".vfs/backends")).unwrap();
        std::fs::write(ws.path().join(".vfs/backends/mounts.yaml"), yaml).unwrap();
        (ws, backend)
    }

    fn test_hook(ws: &Path) -> SyncHook {
        SyncHook::new(SyncHookConfig {
            workspace_root: ws.to_path_buf(),
            mounts_yaml: ws.join(".vfs/backends/mounts.yaml"),
            debounce_ms: DEFAULT_DEBOUNCE_MS,
            settle_ms: DEFAULT_SETTLE_MS,
            poll_secs: DEFAULT_POLL_SECS,
        })
        .unwrap()
    }

    #[test]
    fn record_event_marks_dirty_and_skips_ignored() {
        let (ws, _backend) = test_workspace();
        let mut hook = test_hook(ws.path());

        std::fs::write(ws.path().join("doc.md"), "hi").unwrap();
        hook.record_event(&ws.path().join("doc.md"));
        assert_eq!(hook.dirty_len(), 1);
        assert!(hook.dirty.contains("doc.md"));

        // Ignored file: never marked dirty.
        std::fs::write(ws.path().join("build.tmp"), "x").unwrap();
        hook.record_event(&ws.path().join("build.tmp"));
        assert_eq!(hook.dirty_len(), 1);

        // Path outside the workspace: ignored.
        hook.record_event(Path::new("/tmp/unrelated"));
        assert_eq!(hook.dirty_len(), 1);
    }

    #[test]
    fn record_event_skips_ephemeral_unless_upstream() {
        let (ws, _backend) = test_workspace();
        let mut hook = test_hook(ws.path());

        std::fs::create_dir_all(ws.path().join("cache")).unwrap();
        std::fs::write(ws.path().join("cache/artifact.bin"), "x").unwrap();
        hook.record_event(&ws.path().join("cache/artifact.bin"));
        assert_eq!(
            hook.dirty_len(),
            0,
            "ephemeral without upstream override skips"
        );

        // user.vfs.sync=upstream opts the file in.
        xattr::set(
            ws.path().join("cache/artifact.bin"),
            "user.vfs.sync",
            b"upstream",
        )
        .unwrap();
        hook.record_event(&ws.path().join("cache/artifact.bin"));
        assert_eq!(hook.dirty_len(), 1);
    }

    #[test]
    fn debounce_collapses_rapid_repeats() {
        let (ws, _backend) = test_workspace();
        let mut hook = test_hook(ws.path());

        std::fs::write(ws.path().join("doc.md"), "a").unwrap();
        hook.record_event(&ws.path().join("doc.md"));
        hook.record_event(&ws.path().join("doc.md")); // within window
        assert_eq!(hook.dirty_len(), 1);
    }

    #[test]
    fn flush_dirty_pushes_only_the_batch_ignore_aware() {
        let (ws, backend) = test_workspace();
        let mut hook = test_hook(ws.path());

        std::fs::write(ws.path().join("doc.md"), "hello backend").unwrap();
        hook.record_event(&ws.path().join("doc.md"));
        std::fs::write(ws.path().join("local.tmp"), "local-only").unwrap();
        hook.record_event(&ws.path().join("local.tmp")); // ignored → not dirty

        let stats = hook.flush_dirty().unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].transferred, 1);

        let pushed = backend.path().join("doc.md");
        assert_eq!(std::fs::read_to_string(&pushed).unwrap(), "hello backend");
        assert!(
            !backend.path().join("local.tmp").exists(),
            "ignored file never syncs"
        );
        assert_eq!(hook.dirty_len(), 0);
    }

    #[test]
    fn flush_dirty_never_deletes_remote_only_keys() {
        let (ws, backend) = test_workspace();
        let mut hook = test_hook(ws.path());

        // A remote-only key (present on the backend, absent locally) must
        // survive an incremental batch push.
        std::fs::write(backend.path().join("remote_only.md"), "keep me").unwrap();
        std::fs::write(ws.path().join("doc.md"), "hi").unwrap();
        hook.record_event(&ws.path().join("doc.md"));
        hook.flush_dirty().unwrap();

        assert!(backend.path().join("remote_only.md").exists());
    }

    #[test]
    fn poll_pull_downloads_remote_only_keys() {
        let (ws, backend) = test_workspace();
        let mut hook = test_hook(ws.path());

        std::fs::write(backend.path().join("from_remote.md"), "pulled").unwrap();
        let stats = hook.poll_pull().unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].transferred, 1);

        let pulled = ws.path().join("from_remote.md");
        assert_eq!(std::fs::read_to_string(&pulled).unwrap(), "pulled");
    }

    #[test]
    fn settle_elapsed_is_false_right_after_an_event() {
        let (ws, _backend) = test_workspace();
        let mut hook = test_hook(ws.path());
        assert!(!hook.settle_elapsed(), "no events yet");

        std::fs::write(ws.path().join("doc.md"), "hi").unwrap();
        hook.record_event(&ws.path().join("doc.md"));
        assert!(!hook.settle_elapsed(), "settle window has not elapsed");
    }
}
