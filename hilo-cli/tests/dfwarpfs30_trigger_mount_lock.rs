//! DF-WARPFS-30 regression: a trigger mount must not hold the graph.db
//! DuckDB lock for its lifetime.
//!
//! Proven cross-process (in-process second opens hit duckdb-rs's same-file
//! instance cache and prove nothing about the FILE lock — the lesson from
//! the DF-WARPFS-33 tests):
//!
//! 1. `hilo graph stats` (a child process) must succeed while a trigger
//!    mount is up — with the old eager connection the engine's read-write
//!    handle took DuckDB's exclusive file lock and stats failed with
//!    `Conflicting lock is held`.
//! 2. A SECOND trigger mount on the same workspace must come up — the old
//!    eager open failed there with `cannot open graph DB` and the mount
//!    silently continued with impact computation disabled.
//! 3. The trigger engine still works live: a file written while the mounts
//!    are up is parsed and appended to edges.jsonl.
//!
//! These tests need FUSE (`/dev/fuse` + fusermount3). They skip cleanly
//! where FUSE is unavailable, and fail (not skip) when FUSE exists but the
//! lock behavior regresses.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_hilo");

fn fuse_available() -> bool {
    std::path::Path::new("/dev/fuse").exists() && Command::new("fusermount3").output().is_ok()
}

fn hilo_in(dir: &std::path::Path) -> Command {
    let mut cmd = Command::new(BIN);
    cmd.current_dir(dir);
    cmd
}

/// A live trigger mount that cleans itself up even when an assertion panics.
/// A leaked FUSE mount keeps the test binary alive after the failure and has
/// to be reaped by hand.
struct TriggerMount {
    mnt: std::path::PathBuf,
    child: Child,
    reaped: bool,
}

impl TriggerMount {
    /// Spawn `hilo mount <mnt> --triggers` as a foreground process with
    /// stderr captured (daemon mode discards stderr, so we spawn directly).
    fn spawn(project: &std::path::Path, mnt: &std::path::Path) -> Self {
        let child = Command::new(BIN)
            .arg("mount")
            .arg(mnt)
            .arg("--triggers")
            .current_dir(project)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn hilo mount --triggers");
        Self {
            mnt: mnt.to_path_buf(),
            child,
            reaped: false,
        }
    }

    /// Wait until the FUSE mount registers (the mirrored file appears).
    fn wait_ready(&self, probe_file: &str, timeout: Duration) {
        let target = self.mnt.join(probe_file);
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if target.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!(
            "mount at {} did not become ready within {timeout:?} ({} not visible)",
            self.mnt.display(),
            probe_file
        );
    }

    /// Unmount, reap, and return whatever the mount wrote to stderr.
    fn shutdown(mut self) -> String {
        let _ = Command::new("fusermount3")
            .arg("-uz")
            .arg(&self.mnt)
            .output();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if self.child.try_wait().expect("try_wait failed").is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.reaped = true;
        let mut buf = String::new();
        if let Some(stderr) = self.child.stderr.as_mut() {
            let _ = stderr.read_to_string(&mut buf);
        }
        buf
    }
}

impl Drop for TriggerMount {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        let _ = Command::new("fusermount3")
            .arg("-uz")
            .arg(&self.mnt)
            .output();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Full DF-WARPFS-30 acceptance walk: stats + second mount + live trigger
/// append while a trigger mount is up.
#[test]
fn test_trigger_mount_does_not_block_graph_readers_or_second_mount() {
    if !fuse_available() {
        eprintln!("skipping: FUSE (/dev/fuse + fusermount3) not available");
        return;
    }
    // Mountpoints must live OUTSIDE the project tree. A mountpoint created
    // inside the watched tree makes the engine's own directory walk descend
    // into the FUSE mount, which mirrors the parent — the walk recurses into
    // itself and the whole tree wedges.
    let root = tempfile::TempDir::new().unwrap();
    let project = root.path().join("corpus");
    let mnt1 = root.path().join("mnt1");
    let mnt2 = root.path().join("mnt2");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&mnt1).unwrap();
    std::fs::create_dir_all(&mnt2).unwrap();
    let project = project.as_path();

    std::fs::write(project.join("main.go"), "package main\nimport \"fmt\"\n").unwrap();
    // Warm corpus: init + graph warm so graph.db exists and is hot.
    let out = hilo_in(project).arg("init").output().unwrap();
    assert!(out.status.success(), "hilo init failed: {:?}", out.stderr);
    let out = hilo_in(project).arg("graph").arg("warm").output().unwrap();
    assert!(out.status.success(), "graph warm failed: {:?}", out.stderr);
    assert!(
        project.join(".vfs/graph/graph.db").exists(),
        "warm must materialize graph.db for this test"
    );

    // Mount #1 with triggers — the OLD code opened graph.db READ-WRITE right
    // here and held it for the process lifetime.
    let m1 = TriggerMount::spawn(project, &mnt1);
    m1.wait_ready("main.go", Duration::from_secs(30));

    // AC1: a normal graph reader in a SEPARATE process must succeed while the
    // trigger mount is up. Old code: exclusive DuckDB lock -> "Conflicting
    // lock is held".
    let out = hilo_in(project).arg("graph").arg("stats").output().unwrap();
    let stats_err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        out.status.success(),
        "graph stats must succeed while a trigger mount is up (stderr: {stats_err})"
    );
    assert!(
        !stats_err.contains("Conflicting lock"),
        "stats hit a conflicting lock despite the lazy-DB fix: {stats_err}"
    );

    // AC2: a second trigger mount on the same workspace must come up. Old
    // code: `cannot open graph DB` then impact computation silently disabled.
    let m2 = TriggerMount::spawn(project, &mnt2);
    m2.wait_ready("main.go", Duration::from_secs(30));

    // AC3: the trigger engine still works live while both mounts are up — a
    // new source file must be parsed and appended to edges.jsonl.
    std::fs::write(
        project.join("extra.go"),
        "package extra\nimport \"strings\"\n",
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let edges_path = project.join(".vfs/graph/edges.jsonl");
    let mut found = false;
    while Instant::now() < deadline {
        let content = std::fs::read_to_string(&edges_path).unwrap_or_default();
        if content.contains("extra.go") {
            found = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    assert!(
        found,
        "trigger engine must append the extra.go edge to edges.jsonl"
    );

    // A graph reader must still work with both mounts up (no lock regression
    // introduced by the per-event write-through connection). The per-event
    // connection is held only for the duration of one trigger event, so a
    // reader that fires mid-event can still hit DuckDB's single-writer lock
    // — that transient is expected and bounded; retry briefly and require
    // the reader to succeed (the RUN-LIFETIME lock is what regressed here,
    // and that is what AC1 above proves is gone).
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut last_err = String::new();
    let mut ok = false;
    while Instant::now() < deadline {
        let out = hilo_in(project).arg("graph").arg("stats").output().unwrap();
        last_err = String::from_utf8_lossy(&out.stderr).to_string();
        if out.status.success() {
            ok = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    assert!(
        ok,
        "graph stats must succeed with two trigger mounts up once the event's \
         per-event connection is released (last stderr: {last_err})"
    );

    // Neither mount may have reported a graph-DB open failure: mount #1 is
    // the control, mount #2 is the AC2 case (silently-degraded in the old
    // code).
    let err2 = m2.shutdown();
    let err1 = m1.shutdown();
    assert!(
        !err1.contains("cannot open graph DB"),
        "mount #1 must not report a graph-DB open failure: {err1}"
    );
    assert!(
        !err2.contains("cannot open graph DB"),
        "second trigger mount must not silently continue with impact disabled; \
         it must not report an open failure at all under the lazy design: {err2}"
    );
}
