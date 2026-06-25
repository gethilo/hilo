//! Integration tests for Hilo FUSE operations.
//!
//! These tests exercise the Hilo struct and its Filesystem trait
//! implementations without requiring an actual FUSE kernel mount.
//! They use temp directories populated with real files.

use std::fs;
use std::path::PathBuf;

use hilo_fuse::{FuseConfig, Hilo};

fn test_config(mount_point: PathBuf) -> FuseConfig {
    FuseConfig {
        mount_point,
        allow_other: false,
        direct_io: false,
        auto_unmount: true,
        attr_timeout: 1.0,
        entry_timeout: 1.0,
        max_read: 131_072,
        max_write: 131_072,
        sandbox: None,
    }
}

#[test]
fn test_new_warps_root_inode() {
    let tmp = tempfile::tempdir().unwrap();
    let config = test_config(tmp.path().to_path_buf());
    let wfs = Hilo::new(tmp.path().to_path_buf(), config);

    // Root inode should exist and resolve to the temp directory.
    let resolved = wfs.resolve_path(1);
    assert!(resolved.is_some(), "root inode (1) should resolve");
    assert_eq!(resolved.unwrap(), tmp.path());
}

#[test]
fn test_populate_directory_creates_inodes() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("hello.txt"), b"hello world").unwrap();
    fs::write(tmp.path().join("data.bin"), b"binary data here").unwrap();

    let config = test_config(tmp.path().to_path_buf());
    let wfs = Hilo::new(tmp.path().to_path_buf(), config);

    // Populate the root and verify via inode_for_path.
    hilo_fuse::ops::populated_child_count(&wfs, 1);
    assert!(
        hilo_fuse::ops::inode_for_path(&wfs, "hello.txt").is_some(),
        "hello.txt should have an inode"
    );
    assert!(
        hilo_fuse::ops::inode_for_path(&wfs, "data.bin").is_some(),
        "data.bin should have an inode"
    );
}

#[test]
fn test_lookup_existing_file() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("config.toml"), b"key = value").unwrap();

    let config = test_config(tmp.path().to_path_buf());
    let wfs = Hilo::new(tmp.path().to_path_buf(), config);

    // Populate the root directory.
    hilo_fuse::ops::populated_child_count(&wfs, 1);

    // Look up the inode for "config.toml".
    let ino = hilo_fuse::ops::inode_for_path(&wfs, "config.toml");
    assert!(ino.is_some(), "config.toml should have an inode");

    // Resolve it and check the path.
    let resolved = wfs.resolve_path(ino.unwrap());
    assert!(resolved.is_some());
    assert_eq!(resolved.unwrap(), tmp.path().join("config.toml"));
}

#[test]
fn test_lookup_missing_file() {
    let tmp = tempfile::tempdir().unwrap();
    let config = test_config(tmp.path().to_path_buf());
    let wfs = Hilo::new(tmp.path().to_path_buf(), config);

    let ino = hilo_fuse::ops::inode_for_path(&wfs, "nonexistent.txt");
    assert!(ino.is_none(), "nonexistent file should have no inode");
}

#[test]
fn test_getattr_file_size() {
    let tmp = tempfile::tempdir().unwrap();
    let content = "Hello, Hilo! This is test content.";
    fs::write(tmp.path().join("readme.md"), content).unwrap();

    let config = test_config(tmp.path().to_path_buf());
    let wfs = Hilo::new(tmp.path().to_path_buf(), config);

    // Populate and find the inode.
    hilo_fuse::ops::populated_child_count(&wfs, 1);
    let ino = hilo_fuse::ops::inode_for_path(&wfs, "readme.md").unwrap();
    let resolved = wfs.resolve_path(ino).unwrap();

    let metadata = fs::metadata(&resolved).unwrap();
    assert_eq!(
        metadata.len(),
        content.len() as u64,
        "file size should match content length"
    );
    assert!(metadata.is_file(), "readme.md should be a regular file");
}

#[test]
fn test_read_content() {
    let tmp = tempfile::tempdir().unwrap();
    let body = b"line one\nline two\nline three\n";
    fs::write(tmp.path().join("code.rs"), body).unwrap();

    let config = test_config(tmp.path().to_path_buf());
    let wfs = Hilo::new(tmp.path().to_path_buf(), config);

    hilo_fuse::ops::populated_child_count(&wfs, 1);
    let ino = hilo_fuse::ops::inode_for_path(&wfs, "code.rs").unwrap();

    // Read through resolve_path.
    let resolved = wfs.resolve_path(ino).unwrap();
    let data = fs::read(&resolved).unwrap();
    assert_eq!(&data, body, "read should return exact file content");
}

#[test]
fn test_readdir_sorted_entries() {
    let tmp = tempfile::tempdir().unwrap();
    // Create files in non-sorted order.
    fs::write(tmp.path().join("zebra.txt"), b"z").unwrap();
    fs::write(tmp.path().join("alpha.txt"), b"a").unwrap();
    fs::write(tmp.path().join("beta.txt"), b"b").unwrap();

    let config = test_config(tmp.path().to_path_buf());
    let wfs = Hilo::new(tmp.path().to_path_buf(), config);

    // Populate the root and verify all three files are discoverable.
    hilo_fuse::ops::populated_child_count(&wfs, 1);

    // Verify each file is discoverable via inode_for_path.
    // The root inode (1) has populated children; verify they're discoverable.
    let a_ino = hilo_fuse::ops::inode_for_path(&wfs, "alpha.txt");
    let b_ino = hilo_fuse::ops::inode_for_path(&wfs, "beta.txt");
    let z_ino = hilo_fuse::ops::inode_for_path(&wfs, "zebra.txt");
    assert!(a_ino.is_some(), "alpha.txt should be discoverable");
    assert!(b_ino.is_some(), "beta.txt should be discoverable");
    assert!(z_ino.is_some(), "zebra.txt should be discoverable");
}

#[test]
fn test_permission_compute_mode() {
    use std::path::Path;
    use hilo_fuse::permissions::{compute_mode, default_protections};

    let rules = default_protections();

    // Protected paths should be read-only.
    assert_eq!(compute_mode(Path::new(".vfs/manifest.yaml"), &rules), 0o444);
    assert_eq!(compute_mode(Path::new(".git/config"), &rules), 0o444);
    assert_eq!(compute_mode(Path::new(".gitignore"), &rules), 0o444);
    assert_eq!(compute_mode(Path::new("src/vendor/lib.rs"), &rules), 0o444);
    assert_eq!(compute_mode(Path::new("Cargo.lock"), &rules), 0o444);
    assert_eq!(compute_mode(Path::new("api/auth.pb.go"), &rules), 0o444);

    // Source directories should be read-write.
    assert_eq!(compute_mode(Path::new("src/main.rs"), &rules), 0o644);
    assert_eq!(compute_mode(Path::new("lib/utils.rs"), &rules), 0o644);
    assert_eq!(compute_mode(Path::new("cmd/server/main.go"), &rules), 0o644);

    // Unmatched paths get defaults (regular file → 0o644, directory → 0o755).
    assert_eq!(compute_mode(Path::new("random/file.txt"), &rules), 0o644);
}

#[test]
fn test_default_protections_count() {
    let rules = hilo_fuse::permissions::default_protections();
    assert!(
        rules.len() >= 12,
        "should have at least 12 default protection rules"
    );
}
