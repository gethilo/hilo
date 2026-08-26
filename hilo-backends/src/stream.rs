//! Stream-mode placeholder engine (spec §8).
//!
//! On a stream mount every remote entry becomes a **placeholder**: a
//! zero-byte regular file at the mapped local path carrying
//! `user.vfs.remote = <key>` and `user.vfs.materialized = "false"`.
//! Placeholders are never created for ignored or ephemeral keys (§8.1).
//! `getattr` reports the remote size recorded in the plan (from the walk
//! listing, §8.2). Reading a placeholder materializes it: `backend.get` into
//! the caller's cache path, then `user.vfs.materialized` flips to `"true"`
//! while the `user.vfs.remote` marker is kept so the placeholder stays
//! resolvable for re-sync (§8.3).

use std::io;
use std::path::{Path, PathBuf};

use hilo_metadata::xattr::{get_vfs_xattr, set_vfs_xattr};

use crate::backend::{Backend, BackendError};
use crate::ephemeral::{EphemeralClass, EphemeralMatcher};
use crate::sync::IgnoreMatcher;

/// xattr carrying the remote key of a stream placeholder (spec §11.5).
pub const XATTR_REMOTE: &str = "user.vfs.remote";
/// xattr carrying the materialized state of a stream placeholder (§11.5).
pub const XATTR_MATERIALIZED: &str = "user.vfs.materialized";

/// One planned stream placeholder (spec §8.1/§8.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placeholder {
    /// POSIX-relative path under the mount root (backend key, prefix-stripped
    /// by the backend's own key-relative contract).
    pub rel_path: PathBuf,
    /// Remote key as returned by `Backend::walk`.
    pub key: String,
    /// Remote size in bytes — what `getattr` reports before materialization.
    pub size: u64,
}

/// Stream-mode placeholder planning and lifecycle (§8).
pub struct StreamPlacer;

impl StreamPlacer {
    /// §8.1 planning: walk the backend and map every remote file to a
    /// placeholder. Directories never become placeholders; ignored keys
    /// (`.hiloignore`, built-ins) and ephemeral keys (`.hiloephemeral`,
    /// built-in catalog) are skipped — placeholders are only created for
    /// files that would participate in a real sync.
    pub fn plan_placeholders(
        backend: &dyn Backend,
        ignore: &IgnoreMatcher,
        ephemeral: &EphemeralMatcher,
    ) -> Result<Vec<Placeholder>, BackendError> {
        let mut out = Vec::new();
        for entry in backend.walk("")? {
            if entry.is_dir {
                continue;
            }
            let rel_path = PathBuf::from(entry.key.replace('\\', "/"));
            let rel_str = rel_path.to_string_lossy();
            // Rule files are workspace config, never upstream content — they
            // stay excluded even when the built-in catalog is disabled.
            if rel_path
                .file_name()
                .is_some_and(|n| n == ".hiloignore" || n == ".hiloephemeral")
            {
                continue;
            }
            if ignore.is_ignored(&rel_str) {
                continue;
            }
            if ephemeral.classify(&rel_path, false, None) == EphemeralClass::Ephemeral {
                continue;
            }
            out.push(Placeholder {
                rel_path,
                key: entry.key,
                size: entry.size.max(0) as u64,
            });
        }
        out.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
        Ok(out)
    }

    /// §8.1 creation: materialize a placeholder *plan* on disk as zero-byte
    /// regular files under `root`, each carrying `user.vfs.remote = <key>`
    /// and `user.vfs.materialized = "false"`. Returns the number of files
    /// created.
    pub fn create_placeholders(root: &Path, plan: &[Placeholder]) -> io::Result<usize> {
        let mut created = 0;
        for ph in plan {
            let dest = root.join(&ph.rel_path);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&dest, [])?;
            set_vfs_xattr(&dest, XATTR_REMOTE, &ph.key).map_err(xattr_err)?;
            set_vfs_xattr(&dest, XATTR_MATERIALIZED, "false").map_err(xattr_err)?;
            created += 1;
        }
        Ok(created)
    }

    /// §8.3 materialization: fetch the placeholder's bytes into `dest` (the
    /// caller's cache path), then flip `user.vfs.materialized` to `"true"`.
    /// The `user.vfs.remote` marker is kept so the placeholder stays
    /// resolvable for re-sync. Returns the local path that now holds the
    /// bytes.
    pub fn materialize(
        backend: &dyn Backend,
        ph: &Placeholder,
        dest: &Path,
    ) -> Result<PathBuf, BackendError> {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        backend.get(&ph.key, dest)?;
        set_vfs_xattr(dest, XATTR_MATERIALIZED, "true").map_err(xattr_err)?;
        Ok(dest.to_path_buf())
    }

    /// Whether `path` currently carries `user.vfs.materialized = "true"`.
    /// A missing attribute is treated as not-materialized.
    pub fn is_materialized(path: &Path) -> bool {
        get_vfs_xattr(path, "materialized")
            .ok()
            .flatten()
            .as_deref()
            == Some("true")
    }
}

fn xattr_err(e: hilo_metadata::MetadataError) -> io::Error {
    io::Error::other(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::SyncMode;
    use crate::ephemeral::EphemeralMatcher;
    use crate::sync::IgnoreMatcher;

    /// Workspace-shaped fixture: backend root doubles as the workspace root
    /// (the local test recipe), holding ignore/ephemeral rule files plus a
    /// mix of keep/skip files.
    fn fixture(root: &Path) {
        std::fs::write(root.join(".hiloignore"), "secret/\n").unwrap();
        std::fs::write(root.join(".hiloephemeral"), "tmp-cache/\n").unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), b"fn main() {}").unwrap(); // 11 bytes
        std::fs::create_dir_all(root.join("notes")).unwrap();
        std::fs::write(root.join("notes/readme.md"), b"hi there").unwrap(); // 8 bytes
        std::fs::create_dir_all(root.join("secret")).unwrap();
        std::fs::write(root.join("secret/keys.txt"), b"k").unwrap();
        std::fs::create_dir_all(root.join("tmp-cache")).unwrap();
        std::fs::write(root.join("tmp-cache/blob.bin"), b"b").unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("target/artifact.bin"), b"a").unwrap();
        std::fs::create_dir_all(root.join("build")).unwrap();
        std::fs::write(root.join("build/gen.o"), b"g").unwrap();
        std::fs::create_dir_all(root.join(".cargo/registry")).unwrap();
        std::fs::write(root.join(".cargo/registry/cache.bin"), b"c").unwrap();
    }

    #[test]
    fn plan_skips_ignored_and_ephemeral_keeps_rest() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let backend = crate::backend::LocalDriver::new(dir.path().to_path_buf(), SyncMode::Stream);
        let ignore = IgnoreMatcher::load(dir.path(), None, false).unwrap();
        let ephemeral = EphemeralMatcher::load(dir.path(), None).unwrap();

        let plan = StreamPlacer::plan_placeholders(&backend, &ignore, &ephemeral).unwrap();

        let paths: Vec<&Path> = plan.iter().map(|p| p.rel_path.as_path()).collect();
        assert_eq!(
            paths,
            vec![Path::new("notes/readme.md"), Path::new("src/main.rs")]
        );
        // Sizes come from the walk listing (§8.2 getattr source).
        let main = plan
            .iter()
            .find(|p| p.rel_path == Path::new("src/main.rs"))
            .unwrap();
        assert_eq!(main.size, 12);
        assert_eq!(main.key, "src/main.rs");
        // Ignored + ephemeral + both-rule files never become placeholders.
        assert!(!plan
            .iter()
            .any(|p| p.rel_path == Path::new("secret/keys.txt")));
        assert!(!plan
            .iter()
            .any(|p| p.rel_path == Path::new("tmp-cache/blob.bin")));
        assert!(!plan
            .iter()
            .any(|p| p.rel_path == Path::new("target/artifact.bin")));
        assert!(!plan.iter().any(|p| p.rel_path == Path::new("build/gen.o")));
        assert!(!plan
            .iter()
            .any(|p| p.rel_path == Path::new(".cargo/registry/cache.bin")));
        // Rule files themselves are ignored by the built-in catalog.
        assert!(!plan.iter().any(|p| p.rel_path == Path::new(".hiloignore")));
        assert!(!plan
            .iter()
            .any(|p| p.rel_path == Path::new(".hiloephemeral")));
    }

    #[test]
    fn plan_without_defaults_keeps_ignore_builtin_files() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let backend = crate::backend::LocalDriver::new(dir.path().to_path_buf(), SyncMode::Stream);
        // no_defaults disables the built-in catalog: .cargo/ (ignore-only
        // builtin) is no longer ignored and stays in the plan, but target/
        // stays excluded by the ephemeral built-ins and secret/ by the root
        // .hiloignore user rule.
        let ignore = IgnoreMatcher::load(dir.path(), None, true).unwrap();
        let ephemeral = EphemeralMatcher::load(dir.path(), None).unwrap();

        let plan = StreamPlacer::plan_placeholders(&backend, &ignore, &ephemeral).unwrap();

        let paths: Vec<&Path> = plan.iter().map(|p| p.rel_path.as_path()).collect();
        assert_eq!(
            paths,
            vec![
                Path::new(".cargo/registry/cache.bin"),
                Path::new("notes/readme.md"),
                Path::new("src/main.rs"),
            ]
        );
        assert!(!plan
            .iter()
            .any(|p| p.rel_path == Path::new("target/artifact.bin")));
        assert!(!plan
            .iter()
            .any(|p| p.rel_path == Path::new("secret/keys.txt")));
        assert!(!plan.iter().any(|p| p.rel_path == Path::new("build/gen.o")));
    }

    #[test]
    fn plan_never_creates_placeholders_for_directories() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("dir/with")).unwrap();
        std::fs::write(dir.path().join("dir/with/file.txt"), b"f").unwrap();
        let backend = crate::backend::LocalDriver::new(dir.path().to_path_buf(), SyncMode::Stream);
        let ignore = IgnoreMatcher::load(dir.path(), None, false).unwrap();
        let ephemeral = EphemeralMatcher::load(dir.path(), None).unwrap();

        let plan = StreamPlacer::plan_placeholders(&backend, &ignore, &ephemeral).unwrap();

        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].rel_path, Path::new("dir/with/file.txt"));
    }

    #[test]
    fn plan_empty_backend_yields_empty_plan() {
        let dir = tempfile::tempdir().unwrap();
        let backend = crate::backend::LocalDriver::new(dir.path().to_path_buf(), SyncMode::Stream);
        let ignore = IgnoreMatcher::load(dir.path(), None, false).unwrap();
        let ephemeral = EphemeralMatcher::load(dir.path(), None).unwrap();

        let plan = StreamPlacer::plan_placeholders(&backend, &ignore, &ephemeral).unwrap();

        assert!(plan.is_empty());
    }

    #[test]
    fn create_placeholders_writes_zero_byte_files_with_xattrs() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let backend = crate::backend::LocalDriver::new(dir.path().to_path_buf(), SyncMode::Stream);
        let ignore = IgnoreMatcher::load(dir.path(), None, false).unwrap();
        let ephemeral = EphemeralMatcher::load(dir.path(), None).unwrap();
        let plan = StreamPlacer::plan_placeholders(&backend, &ignore, &ephemeral).unwrap();

        let mount = tempfile::tempdir().unwrap();
        let created = StreamPlacer::create_placeholders(mount.path(), &plan).unwrap();

        assert_eq!(created, plan.len());
        let main = mount.path().join("src/main.rs");
        assert_eq!(std::fs::metadata(&main).unwrap().len(), 0); // zero-byte
        assert_eq!(
            get_vfs_xattr(&main, "remote").unwrap().as_deref(),
            Some("src/main.rs")
        );
        assert_eq!(
            get_vfs_xattr(&main, "materialized").unwrap().as_deref(),
            Some("false")
        );
        // Skipped paths are absent from the mount.
        assert!(!mount.path().join("secret/keys.txt").exists());
        assert!(!mount.path().join("target/artifact.bin").exists());
        assert!(!mount.path().join("tmp-cache/blob.bin").exists());
        // Nested parent directories were created.
        assert!(mount.path().join("notes/readme.md").exists());
    }

    #[test]
    fn materialize_fetches_bytes_and_flips_state() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let backend = crate::backend::LocalDriver::new(dir.path().to_path_buf(), SyncMode::Stream);
        let ignore = IgnoreMatcher::load(dir.path(), None, false).unwrap();
        let ephemeral = EphemeralMatcher::load(dir.path(), None).unwrap();
        let plan = StreamPlacer::plan_placeholders(&backend, &ignore, &ephemeral).unwrap();
        let main = plan
            .iter()
            .find(|p| p.rel_path == Path::new("src/main.rs"))
            .unwrap()
            .clone();

        let mount = tempfile::tempdir().unwrap();
        StreamPlacer::create_placeholders(mount.path(), &[main.clone()]).unwrap();
        let dest = mount.path().join("src/main.rs");
        assert!(!StreamPlacer::is_materialized(&dest));

        let local = StreamPlacer::materialize(&backend, &main, &dest).unwrap();

        assert_eq!(local, dest);
        assert_eq!(std::fs::read(&dest).unwrap(), b"fn main() {}");
        assert_eq!(
            get_vfs_xattr(&dest, "materialized").unwrap().as_deref(),
            Some("true")
        );
        // The remote marker is kept so the placeholder stays resolvable.
        assert_eq!(
            get_vfs_xattr(&dest, "remote").unwrap().as_deref(),
            Some("src/main.rs")
        );
        assert!(StreamPlacer::is_materialized(&dest));
    }
}
