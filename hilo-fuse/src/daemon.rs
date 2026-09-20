//! FUSE daemon — mount and unmount entry points.

use std::path::Path;

use fuser::MountOption;
use tracing::info;

use crate::ops::Hilo;
use crate::FuseConfig;

/// Mount a `Hilo` filesystem at `config.mount_point`.
///
/// This call blocks until the filesystem is unmounted (or an error occurs).
/// When `config.sandbox` is `Some(...)`, the mount itself runs sandboxed
/// via bubblewrap for agent isolation (§14.3). On success returns `Ok(())`.
pub fn mount(fs: Hilo, config: &FuseConfig) -> anyhow::Result<()> {
    // If a sandbox config is present, validate that bwrap is available.
    if let Some(ref sandbox_cfg) = config.sandbox {
        if sandbox_cfg.enabled && !hilo_core::sandbox::BubblewrapExecutor::is_available() {
            anyhow::bail!(
                "bubblewrap sandbox enabled but bwrap not found (install: apt install bubblewrap)"
            );
        }
    }

    info!("mounting Hilo at {}", config.mount_point.display());
    fuser::mount2(fs, &config.mount_point, &mount_options(config))?;
    info!("Hilo mounted successfully");
    Ok(())
}

/// Unmount the FUSE filesystem at `mount_point` if it is currently mounted.
///
/// On Linux this uses the `fusermount -u` helper. If the mount is not active
/// the call is a silent no-op.
pub fn unmount(mount_point: &Path) -> anyhow::Result<()> {
    info!("unmounting Hilo from {}", mount_point.display());
    // Try fusermount first (preferred), fall back to umount.
    let result = std::process::Command::new("fusermount")
        .arg("-u")
        .arg(mount_point)
        .output();

    match result {
        Ok(output) if output.status.success() => Ok(()),
        _ => {
            // Fall back to umount(2).
            let result2 = std::process::Command::new("umount")
                .arg(mount_point)
                .output();
            match result2 {
                Ok(o) if o.status.success() => Ok(()),
                Ok(o) => {
                    // If the error is "not mounted" that's fine.
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    if stderr.contains("not mounted")
                        || stderr.contains("No such file or directory")
                        || stderr.contains("no mount point")
                    {
                        Ok(())
                    } else {
                        Err(anyhow::anyhow!("umount failed: {stderr}"))
                    }
                }
                Err(e) => Err(anyhow::anyhow!("failed to run umount: {e}")),
            }
        }
    }
}

/// Build the kernel mount options (DF-WARPFS-6).
///
/// `AutoUnmount` and `AllowOther` are NOT independent: fuser requires
/// AutoUnmount to be paired with AllowOther (or AllowRoot), and `fusermount3`
/// then REFUSES the mount unless `/etc/fuse.conf` has `user_allow_other` —
/// which a stock Debian/Ubuntu image ships commented out. Since `auto_unmount`
/// is on by default and `allow_other` is opt-in, every default mount on a fresh
/// box died with:
///
/// ```text
/// fusermount3: option allow_other only allowed if 'user_allow_other' is set ...
/// error: FUSE mount failed: Operation not permitted (os error 1)
/// ```
///
/// even though nothing asked for `allow_other`. Auto-unmount is a convenience
/// (it releases the mount when the process dies); allow-other is a real access
/// decision. Honouring the access decision wins: AutoUnmount is emitted only
/// when the operator explicitly asked for AllowOther, i.e. only when they are
/// already responsible for enabling `user_allow_other`.
fn mount_options(config: &FuseConfig) -> Vec<MountOption> {
    let mut opts = vec![MountOption::FSName("hilo".into())];

    if config.read_only {
        opts.push(MountOption::RO);
    }
    if config.allow_other {
        opts.push(MountOption::AllowOther);
        // Only safe to request alongside the explicit allow-other decision.
        if config.auto_unmount {
            opts.push(MountOption::AutoUnmount);
        }
    }

    opts
}

/// Whether `AutoUnmount` was dropped because `allow_other` was not requested.
/// Exposed so the CLI can say so instead of silently changing behaviour.
pub fn auto_unmount_suppressed(config: &FuseConfig) -> bool {
    config.auto_unmount && !config.allow_other
}
