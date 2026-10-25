//! Kernel-level regression coverage for DF-WARPFS-52.
//!
//! `hilo workspace mount` requests `auto_unmount`, and fuser 0.15.1 pairs that
//! with `allow_other` on its own (src/session.rs:93: "adding allow_other, with
//! userspace permission handling"). `fusermount3` then refuses the mount unless
//! `/etc/fuse.conf` has `user_allow_other` — which stock Debian/Ubuntu images
//! ship commented out — so every workspace mount died on a fresh box with
//!
//! ```text
//! fusermount3: option allow_other only allowed if 'user_allow_other' is set
//! error: workspace FUSE mount failed: Operation not permitted (os error 1)
//! ```
//!
//! The option-vector unit tests in `fuse_test.rs` pin the decision; this test
//! pins the OUTCOME at the kernel level: a real mount with the CLI's default
//! config (`allow_other: false`, `auto_unmount: true`) must appear in
//! /proc/self/mountinfo WITHOUT `allow_other`. On a host that has
//! `user_allow_other` set (like most dev boxes) the buggy build still mounted —
//! it just silently asked for a privileged option — so mount success alone
//! would not have caught this; the mountinfo options are the oracle. On a stock
//! box the same absence is what lets the mount succeed at all.
//!
//! Two arms, so the oracle cannot pass vacuously:
//!
//! * **control** — `auto_unmount: false`, `allow_other: false`: if the host
//!   refuses even this, FUSE mounting is unavailable here and the test skips
//!   loudly instead of "passing" on a failure it never exercised.
//! * **subject** — the config `hilo workspace mount` actually builds.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use hilo_core::workspace::MountEntry;
use hilo_fuse::permissions::PermissionEngine;
use hilo_fuse::workspace_mount::{mount, WorkspaceMount};
use hilo_fuse::FuseConfig;

struct MountedWorkspace {
    mount_point: PathBuf,
    mount_thread: Option<JoinHandle<anyhow::Result<()>>>,
    active: bool,
}

impl Drop for MountedWorkspace {
    fn drop(&mut self) {
        if self.active {
            let _ = Command::new("fusermount3")
                .args(["-u", "-z"])
                .arg(&self.mount_point)
                .status();
        }
        if let Some(thread) = self.mount_thread.take() {
            let _ = thread.join();
        }
    }
}

fn is_mounted(path: &Path) -> bool {
    mountinfo_line(path).is_some()
}

/// The whole mountinfo line for `path` (mount options + super options), so the
/// assertion sees the same option string the kernel and fusermount3 agreed on.
fn mountinfo_line(path: &Path) -> Option<String> {
    let mountinfo = fs::read_to_string("/proc/self/mountinfo").ok()?;
    mountinfo
        .lines()
        .find(|line| {
            line.split_whitespace()
                .nth(4)
                .is_some_and(|mountpoint| mountpoint == path.to_string_lossy())
        })
        .map(|line| line.to_string())
}

fn fuse_tooling_present() -> bool {
    use std::os::unix::fs::FileTypeExt;
    fs::metadata("/dev/fuse")
        .map(|metadata| metadata.file_type().is_char_device())
        .unwrap_or(false)
        && Command::new("fusermount3")
            .arg("--version")
            .output()
            .is_ok()
}

/// A scratch directory on a filesystem that accepts FUSE mounts.
///
/// `std::env::temp_dir()` is not always one: this host's TMPDIR
/// (/var/tmp/hermes-scratch) answers EPERM for *every* FUSE mount, allow_other
/// or not, so the test would skip on a machine that mounts fine in /tmp.
fn mountable_scratchdir() -> tempfile::TempDir {
    for candidate in ["/tmp", "/home"] {
        if Path::new(candidate).is_dir() {
            if let Ok(dir) = tempfile::Builder::new()
                .prefix("hilo-df52-")
                .tempdir_in(candidate)
            {
                return dir;
            }
        }
    }
    tempfile::tempdir().expect("a scratch directory")
}

fn backing_repo() -> tempfile::TempDir {
    let dir = mountable_scratchdir();
    fs::write(dir.path().join("README.md"), "workspace payload").unwrap();
    dir
}

/// Mount the workspace and wait for it to appear; returns the live mount.
fn mount_workspace(
    backing: &Path,
    base: &Path,
    allow_other: bool,
    auto_unmount: bool,
) -> Result<MountedWorkspace, io::Error> {
    let mount_point = base.join(format!("ws-{}-{}", allow_other as u8, auto_unmount as u8));
    fs::create_dir_all(&mount_point).unwrap();

    let entries = vec![MountEntry {
        name: "repo".to_string(),
        backing_path: backing.to_path_buf(),
        at: "/workspace/repo".to_string(),
        writable: false,
    }];
    let config = FuseConfig {
        mount_point: mount_point.clone(),
        allow_other,
        direct_io: false,
        auto_unmount,
        read_only: true,
        attr_timeout: 0.0,
        entry_timeout: 0.0,
        max_read: 131_072,
        max_write: 131_072,
        sandbox: None,
    };
    let filesystem = WorkspaceMount::new(
        entries,
        config.clone(),
        PermissionEngine::from_rules(Vec::new()),
    );
    let mount_config = config;
    let thread = thread::spawn(move || mount(filesystem, &mount_config));
    let mut mounted = MountedWorkspace {
        mount_point: mount_point.clone(),
        mount_thread: Some(thread),
        active: false,
    };

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if is_mounted(&mount_point) {
            mounted.active = true;
            return Ok(mounted);
        }
        if mounted
            .mount_thread
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
        {
            let result = mounted.mount_thread.take().unwrap().join();
            return match result {
                Ok(Ok(())) => Err(io::Error::other("mount returned without staying mounted")),
                Ok(Err(error)) => Err(error
                    .downcast_ref::<io::Error>()
                    .map(|e| io::Error::new(e.kind(), e.to_string()))
                    .unwrap_or_else(|| io::Error::other(error.to_string()))),
                Err(panic) => Err(io::Error::other(format!(
                    "mount thread panicked: {panic:?}"
                ))),
            };
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other(
                "workspace mount did not appear in /proc/self/mountinfo within 10s",
            ));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn real_workspace_mount_never_requests_allow_other_by_default() {
    if !fuse_tooling_present() {
        eprintln!("skipping real FUSE mount test: /dev/fuse or fusermount3 is unavailable");
        return;
    }

    let backing = backing_repo();
    let base = mountable_scratchdir();

    // CONTROL ARM: the same mount with the convenience flag OFF requests
    // nothing privileged. If this is refused, the host cannot FUSE-mount at all
    // and the subject arm below would prove nothing.
    let control = match mount_workspace(backing.path(), base.path(), false, false) {
        Ok(control) => control,
        Err(error) => {
            eprintln!(
                "skipping real FUSE mount test: this host refuses even a plain FUSE mount \
                 ({error})"
            );
            return;
        }
    };
    drop(control);

    // SUBJECT ARM: exactly what `hilo workspace mount` builds (read-only plan,
    // allow_other off, auto_unmount on).
    let subject = match mount_workspace(backing.path(), base.path(), false, true) {
        Ok(subject) => subject,
        // THE REGRESSION: pre-fix, fusermount3 refused the mount here because
        // fuser had added allow_other behind the config's back.
        Err(error) => panic!(
            "a default workspace mount (allow_other off, auto_unmount on) must mount \
             without requesting allow_other: {error}"
        ),
    };

    let line =
        mountinfo_line(&subject.mount_point).expect("mount is live, so mountinfo has a line");
    assert!(
        !line.contains("allow_other"),
        "a default workspace mount must not carry allow_other (fuser adds it when \
         auto_unmount is requested alone, and fusermount3 then refuses the mount on a \
         stock box): {line}"
    );

    // The mount must actually serve files, not merely exist.
    let served = fs::read_to_string(subject.mount_point.join("repo/README.md"))
        .expect("mounted workspace serves the backing file");
    assert_eq!(served, "workspace payload");

    let mount_point = subject.mount_point.clone();
    drop(subject);
    assert!(
        !is_mounted(&mount_point),
        "workspace mount must be gone after teardown"
    );
}
