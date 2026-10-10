//! Extended attribute (xattr) read/write for Hilo.
//!
//! All Hilo xattrs live under the `user.vfs.*` namespace so they are
//! visible to standard tools like `getfattr -n user.vfs.relations file`.
//!
//! The `xattr` crate functions take the *full* attribute name (including the
//! `user.` prefix), so we build the full name with `format!("user.vfs.{}", name)`.

use std::path::Path;

use crate::MetadataError;

/// Build the full attribute name: `user.vfs.<name>`.
///
/// Idempotent — if `name` already has a `user.vfs.` prefix, it is
/// stripped first. This prevents double- or triple-prefixing when the
/// caller passes a fully-qualified name.
fn full_name(name: &str) -> String {
    let stripped = name.strip_prefix("user.vfs.").unwrap_or(name);
    format!("user.vfs.{}", stripped)
}

/// Validate a logical attribute name (after any `user.vfs.` prefix strip).
///
/// Rejects empty names, names containing `=` (the classic `--set role=value`
/// CLI typo that would otherwise create a garbage xattr literally named
/// `user.vfs.role=value`), and names containing whitespace or control
/// characters. This is the single write-path chokepoint for the CLI, MCP,
/// and FFI callers.
fn validate_name(name: &str) -> Result<(), MetadataError> {
    if name.is_empty()
        || name.contains('=')
        || name.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(MetadataError::InvalidName(name.to_string()));
    }
    Ok(())
}

/// Set `user.vfs.<name>` on the file at `path` to `value`.
pub fn set_vfs_xattr(path: &Path, name: &str, value: &str) -> Result<(), MetadataError> {
    validate_name(name.strip_prefix("user.vfs.").unwrap_or(name))?;
    let attr = full_name(name);
    xattr::set(path, &attr, value.as_bytes()).map_err(|e| MetadataError::Xattr(e.to_string()))
}

/// Get `user.vfs.<name>` from the file at `path`.
///
/// Returns `Ok(None)` when the attribute does not exist (either the xattr
/// crate reports `None`, or the underlying syscall returns `ENODATA`).
pub fn get_vfs_xattr(path: &Path, name: &str) -> Result<Option<String>, MetadataError> {
    let attr = full_name(name);
    match xattr::get(path, &attr) {
        Ok(Some(bytes)) => Ok(Some(String::from_utf8(bytes)?)),
        Ok(None) => Ok(None),
        Err(e) => {
            // ENODATA / ENOATTR — attribute simply not set yet.
            if e.raw_os_error() == Some(libc_enodata()) {
                Ok(None)
            } else {
                Err(MetadataError::Xattr(e.to_string()))
            }
        }
    }
}

/// List all `user.vfs.*` xattrs on the file at `path`.
///
/// Returns the full attribute names (including the `user.vfs.` prefix).
pub fn list_vfs_xattrs(path: &Path) -> Result<Vec<String>, MetadataError> {
    let prefix = "user.vfs.";
    let mut result = Vec::new();
    for entry in xattr::list(path).map_err(|e| MetadataError::Xattr(e.to_string()))? {
        let name = entry.to_string_lossy().into_owned();
        if name.starts_with(prefix) {
            result.push(name);
        }
    }
    Ok(result)
}

/// Remove `user.vfs.<name>` from the file at `path`.
pub fn remove_vfs_xattr(path: &Path, name: &str) -> Result<(), MetadataError> {
    let attr = full_name(name);
    xattr::remove(path, &attr).map_err(|e| MetadataError::Xattr(e.to_string()))
}

/// COV-4: the explicit grouping annotations carried by one file.
///
/// The two xattrs are the human's own words for *which feature this file
/// belongs to* — the first, most-authoritative level of the surface-grouping
/// precedence (`user.vfs.feature` is coarser than `user.vfs.component`, so
/// callers should prefer `feature` when both are set).
///
/// Both are `Option<String>` because "not annotated" is a real state: the
/// grouping falls back to the structural boundary (crate, then module path)
/// rather than inventing a group.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupAnnotations {
    /// `user.vfs.feature`, when set and non-blank.
    pub feature: Option<String>,
    /// `user.vfs.component`, when set and non-blank.
    pub component: Option<String>,
}

impl GroupAnnotations {
    /// True when neither annotation is present.
    pub fn is_empty(&self) -> bool {
        self.feature.is_none() && self.component.is_none()
    }
}

/// Read the COV-4 grouping annotations from `path`'s `user.vfs.*` xattrs.
///
/// **Best-effort by design.** A file whose xattrs cannot be read (the
/// filesystem carries no xattr support, the file vanished, permissions) is
/// reported as *un-annotated* rather than failing: grouping has a structural
/// fallback, so an unreadable annotation must degrade to "no explicit group",
/// never abort a rollup that could still answer with crate/module groups.
/// The one thing it must never do is invent a group.
pub fn group_annotations(path: &Path) -> GroupAnnotations {
    GroupAnnotations {
        feature: read_annotation(path, "feature"),
        component: read_annotation(path, "component"),
    }
}

/// One annotation, trimmed, with a blank/whitespace-only value treated as
/// absent (an empty xattr is not a group name).
fn read_annotation(path: &Path, name: &str) -> Option<String> {
    let value = get_vfs_xattr(path, name).ok().flatten()?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Return the platform-specific errno value for "no data / attribute not found".
///
/// On Linux this is `ENODATA` (61). On macOS the xattr crate maps missing
/// attributes to `None` directly, so the value here is irrelevant.
#[cfg(target_os = "linux")]
fn libc_enodata() -> i32 {
    61 // ENODATA
}

#[cfg(not(target_os = "linux"))]
fn libc_enodata() -> i32 {
    -1 // sentinel — won't match any real errno
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    // ── full_name unit tests ──────────────────────────────────────────

    #[test]
    fn full_name_no_prefix() {
        assert_eq!(full_name("feature"), "user.vfs.feature");
    }

    #[test]
    fn full_name_with_prefix_is_idempotent() {
        assert_eq!(full_name("user.vfs.feature"), "user.vfs.feature");
    }

    #[test]
    fn full_name_empty_name() {
        assert_eq!(full_name(""), "user.vfs.");
    }

    #[test]
    fn full_name_nested_prefix() {
        // Only strips ONE level — "user.vfs.user.vfs.foo" → "user.vfs.user.vfs.foo"
        // This is correct: the stripped part is "user.vfs." leaving "user.vfs.foo".
        assert_eq!(full_name("user.vfs.user.vfs.foo"), "user.vfs.user.vfs.foo");
    }

    // ── REGRESSION: prefix doubling roundtrip tests ────────────────────
    // These prevent the bug where hilo meta --set user.vfs.feature
    // stored as user.vfs.user.vfs.feature (doubled prefix).

    fn temp_file() -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.txt");
        fs::write(&path, "content").unwrap();
        (dir, path)
    }

    #[test]
    fn regression_set_without_prefix_get_without_prefix() {
        let (_dir, path) = temp_file();
        set_vfs_xattr(&path, "feature", "auth-module").unwrap();
        let val = get_vfs_xattr(&path, "feature").unwrap();
        assert_eq!(val, Some("auth-module".into()));
    }

    #[test]
    fn regression_set_with_prefix_get_with_prefix() {
        let (_dir, path) = temp_file();
        // Setting with prefix must be idempotent — no doubling
        set_vfs_xattr(&path, "user.vfs.feature", "entrypoint").unwrap();
        let val = get_vfs_xattr(&path, "user.vfs.feature").unwrap();
        assert_eq!(val, Some("entrypoint".into()));
    }

    #[test]
    fn regression_set_with_prefix_get_without_prefix() {
        let (_dir, path) = temp_file();
        // If we set with prefix, reading without prefix MUST still work.
        // The stored name must be user.vfs.feature, not user.vfs.user.vfs.feature.
        set_vfs_xattr(&path, "user.vfs.feature", "entrypoint").unwrap();
        let val = get_vfs_xattr(&path, "feature").unwrap();
        assert_eq!(val, Some("entrypoint".into()));
    }

    #[test]
    fn regression_set_without_prefix_get_with_prefix() {
        let (_dir, path) = temp_file();
        set_vfs_xattr(&path, "feature", "auth-module").unwrap();
        let val = get_vfs_xattr(&path, "user.vfs.feature").unwrap();
        assert_eq!(val, Some("auth-module".into()));
    }

    #[test]
    fn regression_stored_name_is_user_vfs_dot_name_not_doubled() {
        let (_dir, path) = temp_file();
        // This was the bug: --set user.vfs.feature stored as user.vfs.user.vfs.feature
        set_vfs_xattr(&path, "user.vfs.feature", "value").unwrap();
        let attrs = list_vfs_xattrs(&path).unwrap();
        // Must contain exactly "user.vfs.feature", NOT "user.vfs.user.vfs.feature"
        assert!(attrs.contains(&"user.vfs.feature".to_string()));
        assert!(!attrs.contains(&"user.vfs.user.vfs.feature".to_string()));
    }

    #[test]
    fn regression_list_after_set_with_prefix_returns_one_attr() {
        let (_dir, path) = temp_file();
        set_vfs_xattr(&path, "user.vfs.feature", "val").unwrap();
        let attrs = list_vfs_xattrs(&path).unwrap();
        assert_eq!(attrs.len(), 1, "should have exactly 1 xattr, not doubled");
        assert_eq!(attrs[0], "user.vfs.feature");
    }

    #[test]
    fn regression_multiple_set_with_mixed_prefixes() {
        let (_dir, path) = temp_file();
        set_vfs_xattr(&path, "feature", "no-prefix").unwrap();
        set_vfs_xattr(&path, "user.vfs.other", "with-prefix").unwrap();
        let attrs = list_vfs_xattrs(&path).unwrap();
        assert_eq!(attrs.len(), 2);
        assert!(attrs.contains(&"user.vfs.feature".to_string()));
        assert!(attrs.contains(&"user.vfs.other".to_string()));
        assert_eq!(
            get_vfs_xattr(&path, "feature").unwrap(),
            Some("no-prefix".into())
        );
        assert_eq!(
            get_vfs_xattr(&path, "other").unwrap(),
            Some("with-prefix".into())
        );
    }

    #[test]
    fn regression_get_nonexistent_attr_returns_none() {
        let (_dir, path) = temp_file();
        let val = get_vfs_xattr(&path, "nonexistent").unwrap();
        assert_eq!(val, None);
    }

    #[test]
    fn regression_remove_after_set() {
        let (_dir, path) = temp_file();
        set_vfs_xattr(&path, "feature", "temp").unwrap();
        remove_vfs_xattr(&path, "feature").unwrap();
        assert_eq!(get_vfs_xattr(&path, "feature").unwrap(), None);
    }

    // ── COV-4 grouping annotations ────────────────────────────────────

    #[test]
    fn group_annotations_reads_feature_and_component() {
        let (_dir, path) = temp_file();
        set_vfs_xattr(&path, "user.vfs.feature", "workspace-mount").unwrap();
        // The prefix is stripped exactly once, and the value is trimmed.
        set_vfs_xattr(&path, "component", "  fuse  ").unwrap();
        assert_eq!(
            group_annotations(&path),
            GroupAnnotations {
                feature: Some("workspace-mount".into()),
                component: Some("fuse".into()),
            }
        );
        assert!(!group_annotations(&path).is_empty());
    }

    #[test]
    fn group_annotations_absent_is_unannotated_not_invented() {
        let (_dir, path) = temp_file();
        let ann = group_annotations(&path);
        assert_eq!(ann, GroupAnnotations::default());
        assert!(ann.is_empty());
        assert_eq!(ann.feature, None);
        assert_eq!(ann.component, None);
    }

    #[test]
    fn group_annotations_blank_value_is_absent() {
        let (_dir, path) = temp_file();
        set_vfs_xattr(&path, "feature", "   ").unwrap();
        let ann = group_annotations(&path);
        assert_eq!(ann.feature, None, "a blank xattr is not a group name");
        assert!(ann.is_empty());
    }

    #[test]
    fn group_annotations_missing_file_is_unannotated() {
        // An unreadable path must degrade to "no explicit group" (the
        // structural fallback covers it), never error out.
        let ann = group_annotations(std::path::Path::new("/nonexistent/cov4/file.rs"));
        assert_eq!(ann, GroupAnnotations::default());
    }

    #[test]
    fn group_annotations_reads_exactly_the_two_grouping_keys() {
        let (_dir, path) = temp_file();
        set_vfs_xattr(&path, "role", "entrypoint").unwrap();
        set_vfs_xattr(&path, "component", "backends").unwrap();
        let ann = group_annotations(&path);
        assert_eq!(ann.component, Some("backends".into()));
        // An unrelated user.vfs.* xattr is not a grouping annotation.
        assert_eq!(ann.feature, None);
    }
}
