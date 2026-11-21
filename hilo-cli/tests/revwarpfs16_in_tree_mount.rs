//! REV-WARPFS-016 regression: the mount front door refuses an in-tree mount
//! point instead of hanging.
//!
//! Measured defect (HEAD 242a792): from inside the repository,
//!
//!     mkdir mnt && hilo mount mnt
//!
//! mounted successfully, appeared in `/proc/mounts`, and then every access
//! hung — `ls mnt` was killed by `timeout 180` (exit 124) while the daemon sat
//! at 0.0% CPU in `request_wait_answer`, reading its own FUSE mount as its
//! backing tree. The same binary mounted to a directory OUTSIDE the source
//! served listings fine.
//!
//! These tests need NO working FUSE device: the guard runs before any session
//! or daemon child exists, so they pass on a host where FUSE is missing
//! entirely (there `/dev/fuse` is never opened). If the guard regresses, the
//! child never exits, the deadline below kills it, and the test fails with the
//! hang instead of wedging the suite.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_hilo");

/// Long enough for a debug-build CLI to start and print its refusal, far below
/// the 180s hang the defect produced — a hang here is a failure, not a wait.
const REFUSAL_DEADLINE: Duration = Duration::from_secs(10);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("hilo-rev016-front-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create scratch root");
        let root = root.canonicalize().expect("scratch root canonicalizes");
        Self { root }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

struct Outcome {
    success: bool,
    stderr: String,
}

/// Run `hilo mount <arg>` with `cwd`, capturing stdio into files (not pipes: a
/// killed child that left a grandchild holding the write end would block a
/// pipe read forever) and poll with a deadline instead of blocking in `wait()`.
fn mount_in(cwd: &Path, arg: &str) -> Outcome {
    let tag = std::process::id();
    let out_path = std::env::temp_dir().join(format!("hilo-rev016-stdout-{tag}.log"));
    let err_path = std::env::temp_dir().join(format!("hilo-rev016-stderr-{tag}.log"));
    let stdout = std::fs::File::create(&out_path).expect("create stdout capture");
    let stderr = std::fs::File::create(&err_path).expect("create stderr capture");

    let mut child = Command::new(BIN)
        .arg("mount")
        .arg(arg)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .expect("failed to spawn hilo mount");

    let deadline = Instant::now() + REFUSAL_DEADLINE;
    let status = loop {
        match child.try_wait().expect("try_wait on hilo mount") {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                // The guard must fire before any FUSE session exists; if the
                // process is still alive it is the DF-WARPFS-016 deadlock.
                let _ = child.kill();
                let _ = child.wait();
                let captured = std::fs::read_to_string(&err_path).unwrap_or_default();
                let _ = std::fs::remove_file(&out_path);
                let _ = std::fs::remove_file(&err_path);
                panic!(
                    "`hilo mount {arg}` in {} did not refuse within {REFUSAL_DEADLINE:?} — \
                     the containment guard must run before any FUSE session (stderr: {captured:?})",
                    cwd.display()
                );
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };

    let stderr = std::fs::read_to_string(&err_path).unwrap_or_default();
    let _ = std::fs::remove_file(&out_path);
    let _ = std::fs::remove_file(&err_path);
    Outcome {
        success: status.success(),
        stderr,
    }
}

/// AC4: no FUSE endpoint is left behind for `mount_point`.
fn assert_not_mounted(mount_point: &Path) {
    let mounts = std::fs::read_to_string("/proc/self/mounts").unwrap_or_default();
    let target = mount_point.display().to_string();
    let leaked: Vec<&str> = mounts
        .lines()
        .filter(|line| {
            let mut fields = line.split(' ');
            let _dev = fields.next();
            fields.next() == Some(target.as_str())
        })
        .collect();
    assert!(
        leaked.is_empty(),
        "a refused mount must leave no /proc/mounts entry for {target}: {leaked:?}"
    );
}

/// AC1: equal, nested and ancestor mount points all refuse promptly, before
/// FUSE, with both paths named.
#[test]
fn in_tree_mount_points_are_refused_promptly() {
    let scratch = Scratch::new("front-door");

    // (a) nested: the reported deadlock — `mkdir mnt && hilo mount mnt` in the
    // repository root, source defaulting to the CWD.
    let nested_root = scratch.root.join("nested");
    let nested_mnt = nested_root.join("mnt");
    std::fs::create_dir_all(&nested_mnt).unwrap();
    let out = mount_in(&nested_root, "mnt");
    assert!(
        !out.success,
        "an in-tree mount point must exit non-zero, not mount"
    );
    assert!(
        out.stderr.contains("mount refused"),
        "the refusal must say `mount refused`: {:?}",
        out.stderr
    );
    assert!(
        out.stderr.contains(&nested_mnt.display().to_string()),
        "the refusal must name the mount point: {:?}",
        out.stderr
    );
    assert!(
        out.stderr.contains(&nested_root.display().to_string()),
        "the refusal must name the served source: {:?}",
        out.stderr
    );
    assert_not_mounted(&nested_mnt);

    // (b) equal: mounting the source over itself.
    let equal_root = scratch.root.join("equal");
    std::fs::create_dir_all(&equal_root).unwrap();
    let out = mount_in(&equal_root, ".");
    assert!(
        !out.success,
        "mounting the source over itself must be refused"
    );
    assert!(out.stderr.contains("mount refused"), "{:?}", out.stderr);
    assert!(
        out.stderr.contains(&equal_root.display().to_string()),
        "the refusal must name the path: {:?}",
        out.stderr
    );
    assert_not_mounted(&equal_root);

    // (c) ancestor: the mount point is a parent of the served source.
    let ancestor_root = scratch.root.join("ancestor");
    let ancestor_source = ancestor_root.join("corpus");
    std::fs::create_dir_all(&ancestor_source).unwrap();
    let out = mount_in(&ancestor_source, "..");
    assert!(
        !out.success,
        "a mount point that is an ancestor of the source must be refused"
    );
    assert!(out.stderr.contains("mount refused"), "{:?}", out.stderr);
    assert!(
        out.stderr.contains(&ancestor_root.display().to_string()),
        "the refusal must name the mount point: {:?}",
        out.stderr
    );
    assert_not_mounted(&ancestor_root);
}
