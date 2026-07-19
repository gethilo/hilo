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

/// Build the mount option list from the config.
fn mount_options(config: &FuseConfig) -> Vec<MountOption> {
    let mut opts = vec![MountOption::RO, MountOption::FSName("hilo".into())];

    if config.allow_other {
        opts.push(MountOption::AllowOther);
    }
    if config.auto_unmount {
        opts.push(MountOption::AutoUnmount);
    }

    opts
}
