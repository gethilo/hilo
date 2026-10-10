//! Regression tests for DF-WARPFS-48 — nested path traversal in the
//! workspace FUSE mount.
//!
//! Pre-fix behavior: `lookup` of any parent below a mount re-populated the
//! backing repo ROOT (`populate_mount_children` re-read `backing_path` and
//! re-inserted its top-level entries), so a directory like `demo/src` never
//! got children and a file two levels deep (`demo/src/commands/run.rs`)
//! never received an inode — every nested read answered ENOENT. `readdir`
//! never populated anything at all.
//!
//! These tests walk the mount exactly as the VFS does: populate a level,
//! look up the child inode, descend. The seams they use are read-only
//! accessors over the inode table (no behavior of their own), so the
//! traversal they observe is the mount's.

use std::fs;
use std::path::PathBuf;

use hilo_core::workspace::MountEntry;
use hilo_fuse::permissions::PermissionEngine;
use hilo_fuse::workspace_mount::{
    ws_backing_file_for_test, ws_child_names_for_test, ws_create_for_test,
    ws_inode_for_path_for_test, ws_populate_dir_for_test, ws_root_ino_for_test,
    ws_setattr_size_for_test, ws_write_for_test, WorkspaceMount,
};
use hilo_fuse::FuseConfig;

/// A backing repo with a file at depth 0, 1 and 2, mounted as `demo`.
fn fixture() -> (tempfile::TempDir, WorkspaceMount) {
    let backing = tempfile::tempdir().unwrap();
    fs::create_dir_all(backing.path().join("src/commands")).unwrap();
    fs::write(backing.path().join("README.md"), "top-level readme").unwrap();
    fs::write(backing.path().join("src/lib.rs"), "nested file").unwrap();
    fs::write(
        backing.path().join("src/commands/run.rs"),
        "two levels deep",
    )
    .unwrap();

    let mounts = vec![MountEntry {
        name: "demo".to_string(),
        backing_path: backing.path().to_path_buf(),
        at: "/tmp/vfs/demo".to_string(),
        writable: false,
    }];
    let config = FuseConfig {
        mount_point: PathBuf::from("/tmp/hilo-ws48-test"),
        allow_other: false,
        direct_io: false,
        auto_unmount: false,
        read_only: true,
        attr_timeout: 1.0,
        entry_timeout: 1.0,
        max_read: 131_072,
        max_write: 131_072,
        sandbox: None,
    };
    let fs = WorkspaceMount::new(mounts, config, PermissionEngine::from_rules(Vec::new()));
    (backing, fs)
}

#[test]
fn test_nested_file_two_levels_deep_resolves_and_reads() {
    let (backing, fs) = fixture();
    let root = ws_root_ino_for_test();

    // The kernel's walk: root -> demo -> src -> commands -> run.rs.
    // One populate per directory level, exactly as lookup/readdir fire them.
    assert!(ws_populate_dir_for_test(&fs, root), "root populates");
    let demo = ws_inode_for_path_for_test(&fs, "demo").expect("top-level mount dir resolves");
    assert!(ws_populate_dir_for_test(&fs, demo), "mount dir populates");
    let src = ws_inode_for_path_for_test(&fs, "demo/src")
        .expect("nested directory `demo/src` must resolve (pre-fix this ENOENTed)");
    assert!(ws_populate_dir_for_test(&fs, src), "nested dir populates");
    let commands = ws_inode_for_path_for_test(&fs, "demo/src/commands")
        .expect("second nested directory `demo/src/commands` must resolve");
    assert!(
        ws_populate_dir_for_test(&fs, commands),
        "commands populates"
    );
    let run = ws_inode_for_path_for_test(&fs, "demo/src/commands/run.rs")
        .expect("a file two directory levels below the mount must get an inode");
    assert_ne!(run, 0);

    // The inode maps back to the real backing file and its content reads.
    let real = ws_backing_file_for_test(&fs, "demo/src/commands/run.rs")
        .expect("nested file maps to a backing path");
    assert_eq!(real, backing.path().join("src/commands/run.rs"));
    assert_eq!(fs::read(&real).unwrap(), b"two levels deep");
}

#[test]
fn test_nested_directory_lists_real_children_not_repo_root() {
    let (_backing, fs) = fixture();
    let root = ws_root_ino_for_test();

    assert!(ws_populate_dir_for_test(&fs, root));
    let demo = ws_inode_for_path_for_test(&fs, "demo").unwrap();
    assert!(ws_populate_dir_for_test(&fs, demo));
    let src = ws_inode_for_path_for_test(&fs, "demo/src")
        .expect("nested directory must resolve before its children can be listed");
    assert!(ws_populate_dir_for_test(&fs, src));

    // The pre-fix bug re-listed the repo root under every nested parent, so
    // `demo/src` would have shown README.md instead of its own entries.
    let src_children = ws_child_names_for_test(&fs, src);
    assert_eq!(
        src_children,
        vec!["commands".to_string(), "lib.rs".to_string()],
        "nested readdir must list the backing directory's own one level"
    );
    assert!(
        !src_children.contains(&"README.md".to_string()),
        "repo-root entries must not leak into a nested directory listing"
    );
}

#[test]
fn test_top_level_entries_still_resolve() {
    // Guard the pre-existing behavior the traversal fix must not regress.
    let (_backing, fs) = fixture();
    let root = ws_root_ino_for_test();

    assert!(ws_populate_dir_for_test(&fs, root));
    assert!(
        ws_inode_for_path_for_test(&fs, "demo").is_some(),
        "top-level mount directory still resolves"
    );
    assert!(
        ws_inode_for_path_for_test(&fs, "no-such-mount").is_none(),
        "unknown root entries still answer ENOENT"
    );

    let demo = ws_inode_for_path_for_test(&fs, "demo").unwrap();
    assert!(ws_populate_dir_for_test(&fs, demo));
    let readme = ws_inode_for_path_for_test(&fs, "demo/README.md")
        .expect("top-level backing file still resolves");
    let real = ws_backing_file_for_test(&fs, "demo/README.md").expect("backing mapping");
    assert_eq!(real, _backing.path().join("README.md"));
    assert_eq!(fs::read(&real).unwrap(), b"top-level readme");
    assert_ne!(readme, 0);
}

#[test]
fn test_deep_sibling_files_are_distinct_inodes() {
    // Two files under different nested directories must not collide on path.
    let (_backing, fs) = fixture();
    let root = ws_root_ino_for_test();

    assert!(ws_populate_dir_for_test(&fs, root));
    let demo = ws_inode_for_path_for_test(&fs, "demo").unwrap();
    assert!(ws_populate_dir_for_test(&fs, demo));
    let src = ws_inode_for_path_for_test(&fs, "demo/src").unwrap();
    assert!(ws_populate_dir_for_test(&fs, src));
    let commands = ws_inode_for_path_for_test(&fs, "demo/src/commands").unwrap();
    assert!(ws_populate_dir_for_test(&fs, commands));
    assert!(
        ws_inode_for_path_for_test(&fs, "demo/src/commands/run.rs").is_some(),
        "nested file resolves"
    );
    assert!(
        ws_inode_for_path_for_test(&fs, "demo/README.md").is_some(),
        "top-level file resolves after nested traversal"
    );
    assert_ne!(
        ws_inode_for_path_for_test(&fs, "demo/src/commands/run.rs"),
        ws_inode_for_path_for_test(&fs, "demo/README.md"),
        "distinct paths must map to distinct inodes"
    );
}

// ============================================================
// DF-WARPFS-138 — workspace-mount write path (write/create/setattr)
// through the backing path. Same in-process harness as above: no
// kernel mount, write-through verified by reading the REAL backing
// directory back from disk.
// ============================================================

/// A backing repo with one file, mounted writable or read-only.
fn writable_fixture(writable: bool) -> (tempfile::TempDir, WorkspaceMount) {
    let backing = tempfile::tempdir().unwrap();
    fs::write(backing.path().join("README.md"), "0123456789").unwrap();

    let mounts = vec![MountEntry {
        name: "demo".to_string(),
        backing_path: backing.path().to_path_buf(),
        at: "/tmp/vfs/demo".to_string(),
        writable,
    }];
    let config = FuseConfig {
        mount_point: PathBuf::from("/tmp/hilo-ws138-test"),
        allow_other: false,
        direct_io: false,
        auto_unmount: false,
        read_only: !writable,
        attr_timeout: 1.0,
        entry_timeout: 1.0,
        max_read: 131_072,
        max_write: 131_072,
        sandbox: None,
    };
    let fs = WorkspaceMount::new(mounts, config, PermissionEngine::from_rules(Vec::new()));
    (backing, fs)
}

/// Walk root -> demo -> README.md exactly as the kernel's lookup does.
fn readme_ino(fs: &WorkspaceMount) -> u64 {
    let root = ws_root_ino_for_test();
    assert!(ws_populate_dir_for_test(fs, root), "root populates");
    let demo = ws_inode_for_path_for_test(fs, "demo").expect("mount dir resolves");
    assert!(ws_populate_dir_for_test(fs, demo), "mount dir populates");
    ws_inode_for_path_for_test(fs, "demo/README.md").expect("README.md resolves")
}

#[test]
fn ws138_write_to_writable_mount_lands_bytes_on_backing_disk() {
    let (backing, fs) = writable_fixture(true);
    let ino = readme_ino(&fs);

    let n = ws_write_for_test(&fs, ino, 4, b"ABCD").expect("writable mount must accept writes");
    assert_eq!(n, 4, "write replies the byte count written");

    // Ground truth: the bytes landed in the REAL backing directory at the
    // requested offset, leaving the rest of the file intact.
    let on_disk = fs::read(backing.path().join("README.md")).unwrap();
    assert_eq!(
        on_disk, b"0123ABCD89",
        "bytes must land at offset 4 on disk"
    );

    // The same bytes read back through the mount's own read path.
    let real = ws_backing_file_for_test(&fs, "demo/README.md").expect("backing mapping");
    assert_eq!(fs::read(&real).unwrap(), b"0123ABCD89");
}

#[test]
fn ws138_write_to_read_only_mount_is_refused_with_eacces() {
    let (backing, fs) = writable_fixture(false);
    let ino = readme_ino(&fs);

    let err = ws_write_for_test(&fs, ino, 0, b"nope")
        .expect_err("a writable:false mount must refuse writes");
    assert_eq!(
        err,
        libc::EACCES,
        "refusal must be EACCES (denied), not ENOSYS/EIO"
    );

    assert_eq!(
        fs::read(backing.path().join("README.md")).unwrap(),
        b"0123456789",
        "a refused write must not touch the backing file"
    );
}

#[test]
fn ws138_create_materializes_backing_file_and_lookup_sees_it() {
    let (backing, fs) = writable_fixture(true);
    let root = ws_root_ino_for_test();
    assert!(ws_populate_dir_for_test(&fs, root));
    let demo = ws_inode_for_path_for_test(&fs, "demo").unwrap();

    let ino = ws_create_for_test(&fs, demo, "brand-new.txt").expect("create on writable mount");
    assert_ne!(ino, 0);

    // The file exists on the real backing disk.
    let backing_file = backing.path().join("brand-new.txt");
    assert!(
        backing_file.exists(),
        "create must materialize the backing file"
    );
    assert_eq!(fs::read(&backing_file).unwrap(), Vec::<u8>::new());

    // The new node is visible via lookup (kernel retry path) and via readdir.
    assert_eq!(
        ws_inode_for_path_for_test(&fs, "demo/brand-new.txt"),
        Some(ino),
        "lookup must see the created node"
    );
    assert!(
        ws_child_names_for_test(&fs, demo).contains(&"brand-new.txt".to_string()),
        "the created file must appear in the parent's listing"
    );

    // And writes through the new inode land on the same backing file.
    let n = ws_write_for_test(&fs, ino, 0, b"hi").expect("write to created file");
    assert_eq!(n, 2);
    assert_eq!(fs::read(&backing_file).unwrap(), b"hi");
}

#[test]
fn ws138_create_on_read_only_mount_is_refused() {
    let (backing, fs) = writable_fixture(false);
    let root = ws_root_ino_for_test();
    assert!(ws_populate_dir_for_test(&fs, root));
    let demo = ws_inode_for_path_for_test(&fs, "demo").unwrap();

    let err =
        ws_create_for_test(&fs, demo, "blocked.txt").expect_err("read-only mount must refuse");
    assert_eq!(err, libc::EACCES);
    assert!(
        !backing.path().join("blocked.txt").exists(),
        "a refused create must not materialize anything"
    );
}

#[test]
fn ws138_setattr_size_shrinks_and_extends_backing_file() {
    let (backing, fs) = writable_fixture(true);
    let ino = readme_ino(&fs);

    ws_setattr_size_for_test(&fs, ino, 3).expect("setattr size on writable mount");

    assert_eq!(
        fs::read(backing.path().join("README.md")).unwrap(),
        b"012",
        "setattr(size) must shrink the REAL backing file"
    );

    // Extending back must zero-fill like truncate(2).
    ws_setattr_size_for_test(&fs, ino, 6).expect("setattr extend");
    assert_eq!(
        fs::read(backing.path().join("README.md")).unwrap(),
        b"012\0\0\0"
    );
}

#[test]
fn ws138_setattr_size_on_read_only_mount_is_refused() {
    let (backing, fs) = writable_fixture(false);
    let ino = readme_ino(&fs);

    let err = ws_setattr_size_for_test(&fs, ino, 2).expect_err("read-only must refuse setattr");
    assert_eq!(err, libc::EACCES);
    assert_eq!(
        fs::read(backing.path().join("README.md")).unwrap(),
        b"0123456789",
        "refused setattr must leave the backing file untouched"
    );
}
