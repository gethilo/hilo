//! Tests for the xattr module.
//!
//! NOTE: Extended attributes only work on filesystems that support them
//! (ext4, xfs, btrfs). If the test temp directory lives on tmpfs, xattr
//! operations may fail with "Operation not supported". Tests that depend
//! on xattr support will skip gracefully in that case.

use std::fs;
use std::path::Path;

use hilo_metadata::xattr;
use tempfile::tempdir;

/// Returns true if the filesystem backing `dir` supports xattrs.
/// Writes a probe file and tries to set+get an xattr on it.
fn xattr_supported(dir: &Path) -> bool {
    let probe = dir.join(".xattr_probe");
    if fs::write(&probe, b"probe").is_err() {
        return false;
    }
    let ok = xattr::set_vfs_xattr(&probe, "probe", "test").is_ok()
        && xattr::get_vfs_xattr(&probe, "probe")
            .ok()
            .flatten()
            .as_deref()
            == Some("test");
    let _ = fs::remove_file(&probe);
    ok
}

#[test]
fn test_xattr_set_get_roundtrip() {
    let dir = tempdir().expect("create tempdir");
    if !xattr_supported(dir.path()) {
        eprintln!("SKIP: xattr not supported on this filesystem");
        return;
    }

    let file = dir.path().join("example.txt");
    fs::write(&file, b"hello").expect("write file");

    xattr::set_vfs_xattr(&file, "risk", "critical-path").expect("set xattr");

    let got = xattr::get_vfs_xattr(&file, "risk").expect("get xattr");
    assert_eq!(got.as_deref(), Some("critical-path"));
}

#[test]
fn test_xattr_get_missing_returns_none() {
    let dir = tempdir().expect("create tempdir");
    if !xattr_supported(dir.path()) {
        eprintln!("SKIP: xattr not supported on this filesystem");
        return;
    }

    let file = dir.path().join("noattrs.txt");
    fs::write(&file, b"no metadata").expect("write file");

    let got = xattr::get_vfs_xattr(&file, "nonexistent").expect("get missing xattr");
    assert!(
        got.is_none(),
        "expected None for missing xattr, got {:?}",
        got
    );
}

#[test]
fn test_xattr_list_multiple() {
    let dir = tempdir().expect("create tempdir");
    if !xattr_supported(dir.path()) {
        eprintln!("SKIP: xattr not supported on this filesystem");
        return;
    }

    let file = dir.path().join("multi.txt");
    fs::write(&file, b"data").expect("write file");

    xattr::set_vfs_xattr(&file, "risk", "high").expect("set risk");
    xattr::set_vfs_xattr(&file, "last_tested", "2026-06-14").expect("set last_tested");
    xattr::set_vfs_xattr(&file, "relations", "imports:types.go").expect("set relations");

    let attrs = xattr::list_vfs_xattrs(&file).expect("list xattrs");
    assert_eq!(attrs.len(), 3, "expected 3 vfs xattrs, got {:?}", attrs);

    // Verify all expected names are present.
    assert!(attrs.contains(&"user.vfs.risk".to_string()));
    assert!(attrs.contains(&"user.vfs.last_tested".to_string()));
    assert!(attrs.contains(&"user.vfs.relations".to_string()));
}

#[test]
fn test_xattr_remove() {
    let dir = tempdir().expect("create tempdir");
    if !xattr_supported(dir.path()) {
        eprintln!("SKIP: xattr not supported on this filesystem");
        return;
    }

    let file = dir.path().join("removable.txt");
    fs::write(&file, b"data").expect("write file");

    xattr::set_vfs_xattr(&file, "risk", "low").expect("set xattr");
    assert_eq!(
        xattr::get_vfs_xattr(&file, "risk").expect("get xattr"),
        Some("low".to_string())
    );

    xattr::remove_vfs_xattr(&file, "risk").expect("remove xattr");

    let got = xattr::get_vfs_xattr(&file, "risk").expect("get removed xattr");
    assert!(got.is_none(), "expected None after remove, got {:?}", got);

    // Verify it's no longer in the list.
    let attrs = xattr::list_vfs_xattrs(&file).expect("list xattrs");
    assert!(
        !attrs.iter().any(|a| a == "user.vfs.risk"),
        "removed attr should not appear in list: {:?}",
        attrs
    );
}

#[test]
fn test_xattr_get_on_nonexistent_file_errors() {
    let dir = tempdir().expect("create tempdir");
    let missing = dir.path().join("does_not_exist.txt");

    let result = xattr::get_vfs_xattr(&missing, "anything");
    assert!(
        result.is_err(),
        "expected error on nonexistent file, got {:?}",
        result
    );
}

#[test]
fn test_xattr_set_on_nonexistent_file_errors() {
    let dir = tempdir().expect("create tempdir");
    let missing = dir.path().join("does_not_exist.txt");

    let result = xattr::set_vfs_xattr(&missing, "anything", "value");
    assert!(
        result.is_err(),
        "expected error on nonexistent file, got {:?}",
        result
    );
}
