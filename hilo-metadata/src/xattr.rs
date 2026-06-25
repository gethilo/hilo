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
}

/// Set `user.vfs.<name>` on the file at `path` to `value`.
pub fn set_vfs_xattr(path: &Path, name: &str, value: &str) -> Result<(), MetadataError> {
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
