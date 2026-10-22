//! Kernel-level regression coverage for DF-WARPFS-49.
//!
//! Unlike the in-process workspace mount tests, this test mounts a real FUSE
//! filesystem and exercises file creation through the mounted path.

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
        // This is also the panic-cleanup path, keeping the integration test
        // repeatable if one of its assertions fails.
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
    let Ok(mountinfo) = fs::read_to_string("/proc/self/mountinfo") else {
        return false;
    };
    mountinfo.lines().any(|line| {
        line.split_whitespace()
            .nth(4)
            .is_some_and(|mountpoint| mountpoint == path.to_string_lossy())
    })
}

fn fuse_available() -> bool {
    Path::new("/dev/fuse").is_char_device()
        && Command::new("fusermount3")
            .arg("--version")
            .output()
            .is_ok()
}

#[test]
fn real_workspace_mount_honors_writable_entries_for_touch() {
    if !fuse_available() {
        eprintln!("skipping real FUSE mount test: /dev/fuse or fusermount3 is unavailable");
        return;
    }

    let writable_backing = tempfile::tempdir().unwrap();
    let readonly_backing = tempfile::tempdir().unwrap();
    let mount_dir = tempfile::tempdir().unwrap();
    let mount_point = mount_dir.path().join("workspace");
    fs::create_dir(&mount_point).unwrap();

    let entries = vec![
        MountEntry {
            name: "writable-repo".to_string(),
            backing_path: writable_backing.path().to_path_buf(),
            at: "/workspace/writable-repo".to_string(),
            writable: true,
        },
        MountEntry {
            name: "readonly-repo".to_string(),
            backing_path: readonly_backing.path().to_path_buf(),
            at: "/workspace/readonly-repo".to_string(),
            writable: false,
        },
    ];
    let config = FuseConfig {
        mount_point: mount_point.clone(),
        allow_other: false,
        direct_io: false,
        auto_unmount: false,
        // Mixed workspaces need a kernel RW mount; the per-entry gate must
        // reject writes to the read-only repo inside the filesystem.
        read_only: false,
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
            break;
        }
        if mounted
            .mount_thread
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
        {
            let result = mounted.mount_thread.take().unwrap().join();
            match result {
                Ok(Err(error))
                    if matches!(
                        error
                            .downcast_ref::<io::Error>()
                            .and_then(io::Error::raw_os_error),
                        Some(libc::EPERM) | Some(libc::EACCES)
                    ) =>
                {
                    eprintln!(
                        "skipping real FUSE mount test: host denied mount permission: {error}"
                    );
                    return;
                }
                other => panic!("FUSE mount thread exited before mounting: {other:?}"),
            }
        }
        assert!(
            Instant::now() < deadline,
            "FUSE mount did not become visible at {}",
            mount_point.display()
        );
        thread::sleep(Duration::from_millis(50));
    }

    let writable_probe = mount_point.join("writable-repo/touch-probe.txt");
    fs::File::create(&writable_probe).expect("touch in writable:true mount entry should succeed");
    assert!(
        writable_backing.path().join("touch-probe.txt").is_file(),
        "the created file must be present in the writable entry's backing directory"
    );

    let readonly_probe = mount_point.join("readonly-repo/touch-probe.txt");
    let error = fs::File::create(&readonly_probe)
        .expect_err("touch in writable:false mount entry must be refused");
    assert!(
        matches!(error.raw_os_error(), Some(libc::EROFS) | Some(libc::EACCES)),
        "read-only entry should fail with EROFS or EACCES; got {error:?}"
    );
    assert!(
        !readonly_backing.path().join("touch-probe.txt").exists(),
        "a refused create must not reach the read-only backing directory"
    );

    // Unmount explicitly before the temp mount-point directory is dropped.
    let status = Command::new("fusermount3")
        .args(["-u"])
        .arg(&mount_point)
        .status()
        .expect("fusermount3 must be executable");
    assert!(status.success(), "fusermount3 -u failed: {status}");
    mounted.active = false;
    assert!(
        !is_mounted(&mount_point),
        "mount should be gone after unmount"
    );
    if let Some(thread) = mounted.mount_thread.take() {
        thread
            .join()
            .expect("mount thread panicked")
            .expect("mount failed");
    }
}

trait IsCharDevice {
    fn is_char_device(&self) -> bool;
}

impl IsCharDevice for Path {
    fn is_char_device(&self) -> bool {
        use std::os::unix::fs::FileTypeExt;
        fs::metadata(self)
            .map(|metadata| metadata.file_type().is_char_device())
            .unwrap_or(false)
    }
}
