//! Stream-mode FUSE wiring (spec §8.2-§8.5).
//!
//! [`StreamState`] carries the backend + placeholder plan for a stream
//! mount. The `Hilo` filesystem consults it in three places:
//!
//! - `getattr` reports the remote size of an unmaterialized placeholder
//!   (§8.2) so agents see real file sizes before any bytes are fetched.
//! - `open` materializes on demand: `backend.get` into the file itself,
//!   then `user.vfs.materialized` flips to `"true"` while the
//!   `user.vfs.remote` marker is kept so the placeholder stays resolvable
//!   for re-sync (§8.3). Metadata-only operations (getxattr/listxattr)
//!   never materialize (§13.14).
//! - Writes to a placeholder materialize first (at open-for-write), then
//!   apply through the FUSE `write` handler into the backing file; the
//!   regular dirty→push flow (inotify + sync hook) picks the change up
//!   (§8.4).
//!
//! §8.5: an unmaterialized placeholder has no local truth — it is
//! local-only by definition and must never be pushed (the sync planner
//! skips these files; see `hilo_backends::planner::is_unmaterialized_placeholder`).

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hilo_backends::backend::Backend;
use hilo_backends::stream::{Placeholder, StreamPlacer};

/// Stream-mode mount state: the backend to materialize from and the
/// placeholder plan keyed by POSIX-relative path (spec §8).
pub struct StreamState {
    backend: Arc<dyn Backend>,
    placeholders: HashMap<PathBuf, Placeholder>,
}

impl StreamState {
    /// Build the mount state from a backend and its planned placeholders.
    /// The caller (the mount command) has already created the placeholder
    /// files on disk via [`StreamPlacer::create_placeholders`].
    pub fn new(backend: Arc<dyn Backend>, plan: Vec<Placeholder>) -> Self {
        let placeholders = plan
            .into_iter()
            .map(|ph| (ph.rel_path.clone(), ph))
            .collect();
        StreamState {
            backend,
            placeholders,
        }
    }

    /// The placeholder for a POSIX-relative path, if any.
    pub fn placeholder(&self, rel: &Path) -> Option<&Placeholder> {
        self.placeholders.get(rel)
    }

    /// Whether `rel` is a stream placeholder (regardless of materialized
    /// state).
    pub fn is_placeholder(&self, rel: &Path) -> bool {
        self.placeholders.contains_key(rel)
    }

    /// §8.2: the size `getattr` should report for `rel` — the remote size
    /// while the placeholder is still unmaterialized; `None` once bytes are
    /// local (the on-disk size is then authoritative).
    pub fn remote_size(&self, rel: &Path, full: &Path) -> Option<u64> {
        let ph = self.placeholder(rel)?;
        if StreamPlacer::is_materialized(full) {
            return None;
        }
        Some(ph.size)
    }

    /// §8.3/§8.4: materialize `rel` — fetch the remote bytes into the file
    /// itself (the caller's mapped local path), then flip
    /// `user.vfs.materialized` to `"true"`. The `user.vfs.remote` marker is
    /// kept so the placeholder stays resolvable for re-sync. Returns `true`
    /// when a fetch actually happened (`false` when already materialized or
    /// not a placeholder).
    pub fn materialize(&self, rel: &Path, full: &Path) -> io::Result<bool> {
        let Some(ph) = self.placeholder(rel) else {
            return Ok(false);
        };
        if StreamPlacer::is_materialized(full) {
            return Ok(false);
        }
        StreamPlacer::materialize(self.backend.as_ref(), ph, full)
            .map_err(|e| io::Error::other(format!("materialize {}: {e}", full.display())))?;
        Ok(true)
    }

    /// §8.5: whether `rel` has no local truth — it is a placeholder and not
    /// yet materialized. Such files are local-only by definition and must
    /// never be pushed.
    pub fn is_local_only(&self, rel: &Path, full: &Path) -> bool {
        self.is_placeholder(rel) && !StreamPlacer::is_materialized(full)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hilo_backends::MockBackend;
    use std::sync::RwLock;

    /// A tiny in-memory backend for stream wiring tests.
    fn mock_backend(objects: &[(&str, &[u8])]) -> Arc<MockBackend> {
        let map: HashMap<String, Vec<u8>> = objects
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_vec()))
            .collect();
        Arc::new(MockBackend {
            objects: RwLock::new(map),
        })
    }

    fn plan(entries: &[(&str, u64)]) -> Vec<Placeholder> {
        entries
            .iter()
            .map(|(rel, size)| Placeholder {
                rel_path: PathBuf::from(rel),
                key: (*rel).to_string(),
                size: *size,
            })
            .collect()
    }

    #[test]
    fn remote_size_reports_walk_size_until_materialized() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let backend = mock_backend(&[("dir/a.txt", b"hello world")]);
        let plan = plan(&[("dir/a.txt", 11)]);
        StreamPlacer::create_placeholders(root, &plan).unwrap();

        let state = StreamState::new(backend, plan);
        let rel = Path::new("dir/a.txt");
        let full = root.join(rel);

        // §8.2: unmaterialized → remote size (not the on-disk 0).
        assert_eq!(state.remote_size(rel, &full), Some(11));
        assert!(state.is_placeholder(rel));
        assert!(state.is_local_only(rel, &full));

        // §8.3: open materializes → bytes land in the file, xattr flips.
        assert!(state.materialize(rel, &full).unwrap());
        assert_eq!(std::fs::read(&full).unwrap(), b"hello world");
        assert_eq!(
            hilo_metadata::xattr::get_vfs_xattr(&full, "materialized")
                .unwrap()
                .as_deref(),
            Some("true")
        );
        // Remote marker is kept for re-sync.
        assert!(hilo_metadata::xattr::get_vfs_xattr(&full, "remote")
            .unwrap()
            .is_some());

        // After materialization the on-disk size is authoritative.
        assert_eq!(state.remote_size(rel, &full), None);
        assert!(!state.is_local_only(rel, &full));

        // Idempotent: a second materialize does nothing.
        assert!(!state.materialize(rel, &full).unwrap());
    }

    #[test]
    fn non_placeholder_paths_are_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let backend = mock_backend(&[("a.txt", b"hi")]);
        let state = StreamState::new(backend, plan(&[("a.txt", 2)]));

        let rel = Path::new("other.txt");
        let full = root.join(rel);
        assert!(!state.is_placeholder(rel));
        assert_eq!(state.remote_size(rel, &full), None);
        assert!(!state.materialize(rel, &full).unwrap());
        assert!(!state.is_local_only(rel, &full));
    }

    #[test]
    fn materialize_failure_propagates() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Backend has no objects → get() fails with NotFound.
        let backend = mock_backend(&[]);
        let plan = plan(&[("missing.bin", 7)]);
        StreamPlacer::create_placeholders(root, &plan).unwrap();

        let state = StreamState::new(backend, plan);
        let full = root.join("missing.bin");
        assert!(state.materialize(Path::new("missing.bin"), &full).is_err());
        // Still unmaterialized → still local-only, still remote-size.
        assert!(state.is_local_only(Path::new("missing.bin"), &full));
        assert_eq!(state.remote_size(Path::new("missing.bin"), &full), Some(7));
    }
}
