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
    ws_backing_file_for_test, ws_child_names_for_test, ws_inode_for_path_for_test,
    ws_populate_dir_for_test, ws_root_ino_for_test, WorkspaceMount,
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
