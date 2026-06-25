//! `hilo workspace mount/unmount` — unified FUSE tree from multi-repo manifest.

use std::path::PathBuf;

use anyhow::{Context, Result};
use hilo_core::workspace::WorkspaceManifest;
use hilo_fuse::{daemon, workspace_mount, workspace_mount::WorkspaceMount, FuseConfig};

/// Mount all repos and backends declared in the manifest.
pub fn run_workspace_mount(manifest_path: &str, mount_point: &str) -> Result<()> {
    let manifest =
        WorkspaceManifest::load(manifest_path).context("failed to load workspace manifest")?;
    let errors = manifest.validate();
    if !errors.is_empty() {
        eprintln!("manifest validation errors:");
        for e in &errors {
            eprintln!("  {}: {}", e.field, e.message);
        }
        anyhow::bail!("manifest validation failed with {} error(s)", errors.len());
    }

    let plan = manifest
        .build_mount_plan()
        .context("failed to build mount plan")?;

    if plan.is_empty() {
        anyhow::bail!("manifest has no mounts defined");
    }

    println!("Mounting {} source(s)...", plan.len());
    for entry in &plan {
        println!(
            "  {} -> {} ({})",
            entry.name,
            entry.at,
            if entry.writable { "rw" } else { "ro" }
        );
    }

    let config = FuseConfig {
        mount_point: PathBuf::from(mount_point),
        allow_other: false,
        direct_io: false,
        auto_unmount: true,
        attr_timeout: 1.0,
        entry_timeout: 1.0,
        max_read: 131_072,
        max_write: 131_072,
        sandbox: None,
    };

    let fs = WorkspaceMount::new(plan, config.clone());

    println!("Hilo workspace mounted at {}", mount_point);
    workspace_mount::mount(fs, &config).context("workspace FUSE mount failed")?;
    Ok(())
}

/// Unmount a workspace at the given mount point.
pub fn run_workspace_unmount(mount_point: &str) -> Result<()> {
    let path = PathBuf::from(mount_point);
    daemon::unmount(&path).context("workspace unmount failed")?;
    println!("Hilo workspace unmounted from {}", mount_point);
    Ok(())
}
