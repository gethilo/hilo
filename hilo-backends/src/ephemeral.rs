//! Ephemeral classification — distinguish transient, rebuildable noise from
//! irreplaceable files (spec: backend-backed-workspace-spec.md §5).
//!
//! The built-in catalog classifies common build/artifact/cache paths as
//! ephemeral; a workspace `.hiloephemeral` file (same git-ignore-style
//! syntax as `.hiloignore`) adds or (`!` negation) removes patterns. An
//! `user.vfs.ephemeral` xattr overrides everything: `false` is the only wipe
//! protector, `true` forces ephemeral even when no pattern matches.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::sync::IgnoreMatcher;

/// Built-in ephemeral catalog (spec §5.1) — exact pattern list.
///
/// NOT in the catalog (never ephemeral): `.git/`, `.vfs/manifest.yaml`,
/// `.vfs/backends/mounts.yaml`, `.hiloignore`, `.hiloephemeral`.
pub const BUILTIN_EPHEMERAL_CATALOG: &str = "\
target/
node_modules/
.venv/
venv/
__pycache__/
dist/
build/
.next/
*.o
*.pyc
*.class
.DS_Store
*.log
.vfs/graph/
.vfs/sync/conflicts.jsonl
";

/// Whether a path is transient (rebuildable/redownloadable) or persistent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EphemeralClass {
    Ephemeral,
    Persistent,
}

/// One ephemeral file discovered by a scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EphemeralEntry {
    /// POSIX-style path relative to the scanned root.
    pub path: PathBuf,
    /// Byte size of the file.
    pub size: u64,
    /// Human-readable reason: the deciding rule (or xattr) that classified it.
    pub reason: String,
}

/// Classifies files against the built-in ephemeral catalog plus an optional
/// `.hiloephemeral` file, with `user.vfs.ephemeral` xattr overrides.
pub struct EphemeralMatcher {
    /// Built-in catalog patterns (spec §5.1).
    builtin: IgnoreMatcher,
    /// User `.hiloephemeral` patterns — matched FIRST, so user rules
    /// (including `!` negations) override the built-in catalog.
    extra: IgnoreMatcher,
    /// Cached xattr overrides (path → user.vfs.ephemeral), highest precedence.
    overrides: HashMap<PathBuf, bool>,
}

impl EphemeralMatcher {
    /// Load the built-in catalog plus the workspace `.hiloephemeral` file
    /// (or `extra_file` when given). A missing extra file yields an empty
    /// user-rule set, mirroring `IgnoreMatcher::from_file`.
    pub fn load(root: &Path, extra_file: Option<&Path>) -> std::io::Result<Self> {
        let builtin = IgnoreMatcher::parse(BUILTIN_EPHEMERAL_CATALOG);
        let extra = match extra_file {
            Some(f) => IgnoreMatcher::from_file(f)?,
            None => IgnoreMatcher::from_file(&root.join(".hiloephemeral"))?,
        };
        Ok(Self {
            builtin,
            extra,
            overrides: HashMap::new(),
        })
    }

    /// Set a cached override for `rel_path` (highest precedence — above both
    /// the caller-supplied xattr value and the pattern catalog).
    pub fn set_override(&mut self, rel_path: PathBuf, ephemeral: bool) {
        self.overrides.insert(rel_path, ephemeral);
    }

    /// Classify a path relative to the workspace root.
    ///
    /// Precedence (spec §5): cached override, then the caller-supplied xattr
    /// value (`user.vfs.ephemeral`), then patterns. `Some(true)` ⇒ Ephemeral
    /// even if no pattern matches; `Some(false)` ⇒ Persistent even if a
    /// pattern matches (the only wipe protector). `None` ⇒ pattern-based.
    /// User `.hiloephemeral` rules win over the built-in catalog.
    pub fn classify(
        &self,
        rel_path: &Path,
        _is_dir: bool,
        xattr_ephemeral: Option<bool>,
    ) -> EphemeralClass {
        if let Some(ephemeral) = self.overrides.get(rel_path) {
            return if *ephemeral {
                EphemeralClass::Ephemeral
            } else {
                EphemeralClass::Persistent
            };
        }
        if let Some(ephemeral) = xattr_ephemeral {
            return if ephemeral {
                EphemeralClass::Ephemeral
            } else {
                EphemeralClass::Persistent
            };
        }
        let rel = rel_path.to_string_lossy().replace('\\', "/");
        // A user rule that matches anything (including a `!` negation)
        // decides; the built-in catalog only applies when no user rule
        // matches — mirroring last-match-wins with user rules after built-ins.
        if self.extra.decision(&rel).rule.is_some() {
            return if self.extra.is_ignored(&rel) {
                EphemeralClass::Ephemeral
            } else {
                EphemeralClass::Persistent
            };
        }
        if self.builtin.is_ignored(&rel) {
            EphemeralClass::Ephemeral
        } else {
            EphemeralClass::Persistent
        }
    }

    /// The deciding rule for `rel_path` (raw `.hiloephemeral` or built-in
    /// catalog line), or `None` when no pattern matches.
    pub fn reason(&self, rel_path: &Path) -> Option<String> {
        let rel = rel_path.to_string_lossy().replace('\\', "/");
        if let Some(rule) = self.extra.decision(&rel).rule {
            return Some(rule);
        }
        self.builtin.decision(&rel).rule
    }

    /// Walk `root`, classify every regular file, and return ephemeral
    /// entries with byte sizes. Symlinks are excluded (never ephemeral) and
    /// the walk never follows links, so it never crosses the workspace root.
    /// Paths in the returned entries are POSIX-relative to `root`.
    pub fn scan(&self, root: &Path) -> Result<Vec<EphemeralEntry>, EphemeralError> {
        let mut out = Vec::new();
        for entry in WalkDir::new(root).follow_links(false) {
            let entry = entry.map_err(|e| {
                let failed_path = e.path().map(|p| p.to_path_buf()).unwrap_or_default();
                let io = e
                    .into_io_error()
                    .unwrap_or_else(|| std::io::Error::other("walkdir error"));
                EphemeralError::WalkFailed(failed_path, io)
            })?;
            if !entry.file_type().is_file() {
                continue;
            }
            let rel = match entry.path().strip_prefix(root) {
                Ok(r) if !r.as_os_str().is_empty() => r.to_path_buf(),
                _ => continue,
            };
            if self.classify(&rel, false, None) != EphemeralClass::Ephemeral {
                continue;
            }
            let size = entry
                .metadata()
                .map_err(|e| EphemeralError::Io(std::io::Error::from(e)))?
                .len();
            let reason = self.reason(&rel).unwrap_or_else(|| "ephemeral".to_string());
            out.push(EphemeralEntry {
                path: rel,
                size,
                reason,
            });
        }
        Ok(out)
    }
}

/// Errors from ephemeral scanning.
#[derive(Debug, thiserror::Error)]
pub enum EphemeralError {
    /// I/O error reading file metadata.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    /// A directory entry could not be read during the walk.
    #[error("failed to read directory {0}: {1}")]
    WalkFailed(PathBuf, std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn matcher() -> EphemeralMatcher {
        EphemeralMatcher {
            builtin: IgnoreMatcher::parse(BUILTIN_EPHEMERAL_CATALOG),
            extra: IgnoreMatcher::empty(),
            overrides: HashMap::new(),
        }
    }

    fn matcher_with_extra(extra: &str) -> EphemeralMatcher {
        EphemeralMatcher {
            builtin: IgnoreMatcher::parse(BUILTIN_EPHEMERAL_CATALOG),
            extra: IgnoreMatcher::parse(extra),
            overrides: HashMap::new(),
        }
    }

    #[test]
    fn builtin_catalog_classifies_build_artifacts_ephemeral() {
        let m = matcher();
        assert_eq!(
            m.classify(Path::new("target/debug/hilo"), false, None),
            EphemeralClass::Ephemeral
        );
        assert_eq!(
            m.classify(Path::new("node_modules/pkg/index.js"), false, None),
            EphemeralClass::Ephemeral
        );
        assert_eq!(
            m.classify(Path::new("src/main.rs"), false, None),
            EphemeralClass::Persistent
        );
        assert_eq!(
            m.classify(Path::new("README.md"), false, None),
            EphemeralClass::Persistent
        );
    }

    #[test]
    fn protected_paths_never_ephemeral() {
        let m = matcher();
        assert_eq!(
            m.classify(Path::new(".git/config"), false, None),
            EphemeralClass::Persistent
        );
        assert_eq!(
            m.classify(Path::new(".vfs/manifest.yaml"), false, None),
            EphemeralClass::Persistent
        );
        assert_eq!(
            m.classify(Path::new(".vfs/backends/mounts.yaml"), false, None),
            EphemeralClass::Persistent
        );
        assert_eq!(
            m.classify(Path::new(".hiloignore"), false, None),
            EphemeralClass::Persistent
        );
        assert_eq!(
            m.classify(Path::new(".hiloephemeral"), false, None),
            EphemeralClass::Persistent
        );
    }

    #[test]
    fn hiloephemeral_file_adds_patterns() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join(".hiloephemeral"), "*.tmp\n").expect("write");
        let m = EphemeralMatcher::load(dir.path(), None).expect("load");
        assert_eq!(
            m.classify(Path::new("scratch.tmp"), false, None),
            EphemeralClass::Ephemeral
        );
        assert_eq!(
            m.classify(Path::new("src/scratch.tmp"), false, None),
            EphemeralClass::Ephemeral
        );
        assert_eq!(
            m.classify(Path::new("src/main.rs"), false, None),
            EphemeralClass::Persistent
        );
    }

    #[test]
    fn hiloephemeral_negation_reincludes_builtin_match() {
        let m = matcher_with_extra("!target/keep.bin\n");
        assert_eq!(
            m.classify(Path::new("target/keep.bin"), false, None),
            EphemeralClass::Persistent
        );
        assert_eq!(
            m.classify(Path::new("target/drop.bin"), false, None),
            EphemeralClass::Ephemeral
        );
    }

    #[test]
    fn xattr_overrides_patterns() {
        let m = matcher();
        // Some(true) forces ephemeral even when no pattern matches.
        assert_eq!(
            m.classify(Path::new("src/main.rs"), false, Some(true)),
            EphemeralClass::Ephemeral
        );
        // Some(false) protects from wipe even when a pattern matches.
        assert_eq!(
            m.classify(Path::new("target/x.bin"), false, Some(false)),
            EphemeralClass::Persistent
        );
        assert_eq!(
            m.classify(Path::new("target/x.bin"), false, None),
            EphemeralClass::Ephemeral
        );
    }

    #[test]
    fn override_cache_takes_highest_precedence() {
        let mut m = matcher();
        m.set_override(PathBuf::from("src/cached.rs"), true);
        assert_eq!(
            m.classify(Path::new("src/cached.rs"), false, Some(false)),
            EphemeralClass::Ephemeral
        );
    }

    #[test]
    fn scan_reports_sizes_and_skips_symlinks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("target")).expect("mkdir");
        fs::create_dir_all(root.join("node_modules/pkg")).expect("mkdir");
        fs::create_dir_all(root.join("src")).expect("mkdir");
        fs::write(root.join("target/a.bin"), vec![0u8; 20]).expect("write");
        fs::write(root.join("node_modules/pkg/index.js"), vec![0u8; 30]).expect("write");
        fs::write(root.join("src/main.rs"), vec![0u8; 10]).expect("write");

        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("src/main.rs"), root.join("target/link-to-src"))
            .expect("symlink");

        let m = matcher();
        let entries = m.scan(root).expect("scan");
        let mut paths: Vec<PathBuf> = entries.iter().map(|e| e.path.clone()).collect();
        paths.sort();
        assert_eq!(
            paths,
            vec![
                PathBuf::from("node_modules/pkg/index.js"),
                PathBuf::from("target/a.bin"),
            ]
        );
        let total: u64 = entries.iter().map(|e| e.size).sum();
        assert_eq!(total, 50);
        // Reasons name the deciding rule.
        assert!(entries
            .iter()
            .all(|e| e.reason.contains("node_modules") || e.reason.contains("target")));
    }

    #[test]
    fn scan_never_crosses_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("sub")).expect("mkdir");
        // A symlink pointing OUTSIDE the root must not be followed (and is
        // not ephemeral anyway — it is excluded as a symlink).
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.parent().unwrap(), root.join("sub/escape"))
            .expect("symlink");
        let m = matcher();
        let entries = m.scan(root).expect("scan");
        assert!(entries.iter().all(|e| !e.path.starts_with("sub/escape")));
    }
}
