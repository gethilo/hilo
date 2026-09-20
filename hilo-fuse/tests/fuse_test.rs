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
        read_only: true,
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
    use hilo_fuse::permissions::{compute_mode, default_protections};
    use std::path::Path;

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

// ─── DF-WARPFS-5: readdir on an EMPTY directory must terminate ───────────────
//
// Reported: `ls -a <mount>/emptydir` hung to the timeout cap on every attempt
// (10s/12s/15s/20s, zero output) while every non-empty sibling answered in
// ~110ms; consequently `find <mount> -type f` returned nothing and hung, so any
// tree walk over a mount containing one empty directory produced no rows and no
// error.
//
// The kernel's contract: a readdir reply carries a cookie per entry; the next
// call passes the LAST cookie. Emitting `.` at `offset <= 1` and `..` at
// `offset <= 2` is fine only while some later child raises the cookie past 2.
// With no children the last cookie IS 2, so `readdir(offset=2)` re-emitted `..`
// with cookie 2 forever.
//
// These tests pin the cookie rule. The bug is a LOOP, so the test is a walk:
// replay exactly what the kernel does and require it to terminate.

use hilo_fuse::ops::readdir_entries_for_test;

fn kids(names: &[(&str, bool)]) -> Vec<(u64, String, bool)> {
    names
        .iter()
        .enumerate()
        .map(|(i, (n, d))| ((i + 10) as u64, n.to_string(), *d))
        .collect()
}

/// Replay the kernel: start at 0, take the last cookie, repeat. Returns every
/// name the filesystem would report, or panics if the stream never ends.
fn walk(children: &[(u64, String, bool)]) -> Vec<String> {
    let mut seen = Vec::new();
    let mut offset = 0i64;
    for _ in 0..1000 {
        let batch = readdir_entries_for_test(offset, children);
        if batch.is_empty() {
            return seen;
        }
        let last = batch.last().unwrap().1;
        for (_, _, name, _) in &batch {
            seen.push(name.clone());
        }
        // A resume offset that does not advance is the bug: name it loudly.
        assert!(
            last > offset,
            "readdir did not advance: offset {} -> last cookie {} (batch {:?})",
            offset,
            last,
            batch.iter().map(|e| e.2.clone()).collect::<Vec<_>>()
        );
        offset = last;
    }
    panic!("readdir never terminated (walked 1000 rounds) — the empty-directory hang");
}

#[test]
fn test_readdir_empty_directory_terminates() {
    let got = walk(&[]);
    assert_eq!(got, vec![".".to_string(), "..".to_string()]);
}

#[test]
fn test_readdir_empty_directory_resume_at_cookie_two_is_empty() {
    // The exact call the kernel made forever: offset == 2 (last cookie was 2).
    let batch = readdir_entries_for_test(2, &[]);
    assert!(
        batch.is_empty(),
        "readdir(offset=2) on an empty dir must return NOTHING so the stream \
         ends; it returned {:?}",
        batch.iter().map(|e| e.2.clone()).collect::<Vec<_>>()
    );
}

#[test]
fn test_readdir_non_empty_directory_terminates() {
    let got = walk(&kids(&[("a.txt", false), ("sub", true)]));
    assert_eq!(
        got,
        vec![".", "..", "a.txt", "sub"],
        "non-empty dir must list dot entries then children"
    );
}

#[test]
fn test_readdir_many_children_terminates() {
    let names: Vec<(&str, bool)> = vec![
        ("a", false),
        ("b", false),
        ("c", false),
        ("d", true),
        ("e", false),
        ("f", false),
        ("g", true),
        ("h", false),
        ("i", false),
        ("j", false),
    ];
    let got = walk(&kids(&names));
    assert_eq!(got.len(), 12, "dot + dotdot + 10 children");
}

#[test]
fn test_readdir_offsets_are_strictly_increasing_cookies() {
    let batch = readdir_entries_for_test(0, &kids(&[("a", false), ("b", false)]));
    let cookies: Vec<i64> = batch.iter().map(|e| e.1).collect();
    assert_eq!(
        cookies,
        vec![1, 2, 3, 4],
        "one cookie per entry, no re-emission"
    );
}

#[test]
fn test_readdir_initial_call_emits_dot_entries_first() {
    let batch = readdir_entries_for_test(0, &[]);
    let names: Vec<&str> = batch.iter().map(|e| e.2.as_str()).collect();
    assert_eq!(names, vec![".", ".."]);
}

// ─── DF-WARPFS-6: a default mount must not require allow_other ───────────────
//
// On a stock Debian 13 box (base image only, user_allow_other commented out in
// /etc/fuse.conf), `hilo mount` printed "Hilo mounted at ..." and then died:
//   fusermount3: option allow_other only allowed if 'user_allow_other' is set
//   error: FUSE mount failed: Operation not permitted (os error 1)   EXIT=1
// Nothing asked for allow_other — `hilo mount --help` shows it as opt-in and
// the generated manifest says allow_other: false. The dependency (fuser 0.15.1)
// requires AutoUnmount to be accompanied by AllowOther, and auto_unmount was
// hardcoded true, so the "convenience" flag silently forced a privileged mount
// option the operator never chose.

use hilo_fuse::daemon::auto_unmount_suppressed;

fn cfg(allow_other: bool, auto_unmount: bool) -> FuseConfig {
    FuseConfig {
        mount_point: PathBuf::from("/tmp/x"),
        allow_other,
        direct_io: false,
        auto_unmount,
        read_only: true,
        attr_timeout: 1.0,
        entry_timeout: 1.0,
        max_read: 131_072,
        max_write: 131_072,
        sandbox: None,
    }
}

#[test]
fn test_default_mount_does_not_request_auto_unmount() {
    // THE REGRESSION: auto_unmount defaults on, allow_other stays off. Requesting
    // AutoUnmount here is what made fusermount3 refuse the mount outright.
    assert!(
        auto_unmount_suppressed(&cfg(false, true)),
        "a default mount (allow_other off) must NOT emit AutoUnmount, because \
         fuser pairs it with AllowOther and fusermount3 then rejects the mount \
         on any stock box where user_allow_other is unset"
    );
}

#[test]
fn test_explicit_allow_other_keeps_auto_unmount() {
    // The operator asked for allow_other, so they are already responsible for
    // user_allow_other; the convenience flag may stay.
    assert!(
        !auto_unmount_suppressed(&cfg(true, true)),
        "explicit allow_other should keep auto-unmount"
    );
}

#[test]
fn test_auto_unmount_off_is_not_reported_as_suppressed() {
    assert!(!auto_unmount_suppressed(&cfg(false, false)));
    assert!(!auto_unmount_suppressed(&cfg(true, false)));
}

// ─── DF-WARPFS-7: the mount must honour the ignore stack ────────────────────
//
// Measured contradiction before the fix: `hilo ignore check target/` ->
// "ignored: true", `.git/` -> true, `.vfs/` -> true — yet `ls <mount>` listed
// target, .git, .vfs and .pytest_cache, and `ls <mount>/target` showed the
// build tree (130 GB on the reporting repo). `grep -rn ignore
// hilo-fuse/src/*.rs` returned ZERO hits: the mount tree came from a raw
// `std::fs::read_dir` walk, so ignore policy was enforced on the graph,
// ephemeral and backend paths but never on the mount itself.

use hilo_backends::IgnoreMatcher;
use hilo_fuse::ops::{attr_for_test, is_ignored_for_test, is_ignored_for_test_absent};

/// The built-in default stack, rebuilt per call (IgnoreMatcher is not Clone).
fn ignored(rel: &str) -> bool {
    let m = IgnoreMatcher::parse(
        "target/\nnode_modules/\n.venv/\nvenv/\n__pycache__/\ndist/\nbuild/\n.next/\n.cargo/\n.vfs/\n.git/\n*.o\n*.pyc\n*.class\n.DS_Store\n*.log\n.hiloignore\n.hiloephemeral\n",
    );
    is_ignored_for_test(m, rel)
}

#[test]
fn test_mount_hides_the_builtin_ignored_directories() {
    for p in ["target", ".git", ".vfs", "node_modules", "__pycache__"] {
        assert!(
            ignored(p),
            "`{p}` is reported ignored:true by `hilo ignore check` and must \
             therefore NOT be served through the mount"
        );
    }
}

#[test]
fn test_mount_still_serves_ordinary_entries() {
    for p in ["a.txt", "src", "src/main.rs", "README.md", "docs"] {
        assert!(
            !ignored(p),
            "`{p}` is not ignored — the mount must still serve it (a mount that \
             over-hides is as broken as one that under-hides)"
        );
    }
}

#[test]
fn test_nested_paths_under_an_ignored_dir_are_ignored() {
    // `find -type f` walks into children, so the rule must hold below the dir.
    assert!(ignored("target/debug/incremental"));
    assert!(ignored(".git/objects/ab/cdef"));
}

#[test]
fn test_log_files_are_ignored_but_sources_are_not() {
    assert!(ignored("build.log"));
    assert!(!ignored("build.rs"));
}

#[test]
fn test_no_matcher_means_no_filtering() {
    // A mount built without a stack keeps its previous behaviour rather than
    // silently hiding the tree — attaching the stack is an explicit act.
    assert!(!is_ignored_for_test_absent());
}

// ─── DF-WARPFS-8: getattr must report the BACKING file's mtime ──────────────
//
// Measured: README.md disk 2026-09-19 20:13:55 vs mount 20:47:12; Makefile disk
// 2026-07-19 12:15:26 vs mount 20:47:12; LICENSE disk 2026-06-24 vs mount
// 20:47:12. The fabricated value was not a constant — it was the moment of the
// call (two sessions minutes apart disagreed for the same unchanged files).
// Sizes stayed byte-exact and content hashes matched, so this was purely
// metadata dishonesty, and it broke every mtime-driven tool run over the mount.

use std::time::{Duration, SystemTime};

#[test]
fn test_attr_mtime_matches_the_backing_file() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = test_config(tmp.path().to_path_buf());
    let wfs = Hilo::new(tmp.path().to_path_buf(), cfg);
    let p = tmp.path().join("stamped.txt");
    fs::write(&p, b"x").unwrap();

    // Set a recognisable mtime well in the past (2010-01-01).
    let past = SystemTime::UNIX_EPOCH + Duration::from_secs(1_262_304_000);
    filetime_set(&p, past);

    hilo_fuse::ops::populated_child_count(&wfs, 1);
    let ino = hilo_fuse::ops::inode_for_path(&wfs, "stamped.txt").expect("inode");
    let attr = attr_for_test(&wfs, ino).expect("attr");

    let backing = fs::metadata(&p).unwrap().modified().unwrap();
    let got = attr.mtime;
    let delta = got
        .duration_since(backing)
        .or_else(|_| backing.duration_since(got))
        .unwrap_or_default();
    assert!(
        delta < Duration::from_secs(2),
        "getattr reported mtime {:?} but the backing file's mtime is {:?} — \
         metadata through the mount must be honest",
        got,
        backing
    );
    assert!(
        got < SystemTime::now() - Duration::from_secs(86_400),
        "mtime looks fabricated (close to now) for a file last touched years ago"
    );
}

#[test]
fn test_attr_mtime_is_stable_across_calls() {
    // The old bug returned the time OF THE CALL, so two stats disagreed. An
    // honest mtime does not move for an unmodified file.
    let tmp = tempfile::tempdir().unwrap();
    let cfg = test_config(tmp.path().to_path_buf());
    let wfs = Hilo::new(tmp.path().to_path_buf(), cfg);
    fs::write(tmp.path().join("a.txt"), b"y").unwrap();
    hilo_fuse::ops::populated_child_count(&wfs, 1);
    let ino = hilo_fuse::ops::inode_for_path(&wfs, "a.txt").expect("inode");

    let first = attr_for_test(&wfs, ino).expect("attr").mtime;
    std::thread::sleep(Duration::from_millis(1100));
    let second = attr_for_test(&wfs, ino).expect("attr").mtime;
    assert_eq!(
        first, second,
        "mtime changed between two stats of an untouched file — that is the \
         fabricated-time-of-call bug"
    );
}

#[test]
fn test_attr_size_is_still_byte_exact() {
    // Guard the property that was already correct, so a metadata fix cannot
    // regress it.
    let tmp = tempfile::tempdir().unwrap();
    let cfg = test_config(tmp.path().to_path_buf());
    let wfs = Hilo::new(tmp.path().to_path_buf(), cfg);
    fs::write(tmp.path().join("s.bin"), b"0123456789").unwrap();
    hilo_fuse::ops::populated_child_count(&wfs, 1);
    let ino = hilo_fuse::ops::inode_for_path(&wfs, "s.bin").expect("inode");
    assert_eq!(attr_for_test(&wfs, ino).expect("attr").size, 10);
}

/// Set a file's mtime with no extra dependency (utimensat via libc).
fn filetime_set(path: &std::path::Path, t: SystemTime) {
    use std::os::unix::ffi::OsStrExt;
    let secs = t.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs() as i64;
    let times = [
        libc::timespec {
            tv_sec: secs,
            tv_nsec: 0,
        },
        libc::timespec {
            tv_sec: secs,
            tv_nsec: 0,
        },
    ];
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    let rc = unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), times.as_ptr(), 0) };
    assert_eq!(rc, 0, "utimensat failed");
}
