//! `hilo mount <mount_point>` — mount a Hilo virtual filesystem via FUSE.

use std::path::PathBuf;

use anyhow::{Context, Result};
use hilo_fuse::{daemon, FuseConfig, Hilo};

/// Mount a Hilo read-only FUSE filesystem.
///
/// Reads the current directory as the backing root, builds a `FuseConfig`
/// from the CLI arguments, and blocks in `daemon::mount` until unmounted.
///
/// On `SIGINT` / `SIGTERM` the mount is cleaned up via `daemon::unmount`.
pub fn run_mount(mount_point: &str, triggers: bool, allow_other: bool) -> Result<()> {
    let current_dir =
        std::env::current_dir().context("failed to determine the current directory")?;

    let config = FuseConfig {
        mount_point: PathBuf::from(mount_point),
        allow_other,
        direct_io: false,
        auto_unmount: true,
        attr_timeout: 1.0,
        entry_timeout: 1.0,
        max_read: 131_072,
        max_write: 131_072,
        sandbox: None,
    };

    let fs = Hilo::new(current_dir, config.clone());

    println!(
        "Hilo mounted at {}{}",
        mount_point,
        if triggers { " (triggers enabled)" } else { "" }
    );

    daemon::mount(fs, &config).context("FUSE mount failed")?;
    Ok(())
}
