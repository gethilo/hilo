//! `hilo workspace mount/unmount/sync` — unified FUSE tree + S3 two-way sync.

use std::path::PathBuf;

use anyhow::{Context, Result};
use hilo_backends::{IgnoreMatcher, S3Client, SyncEngine};
use hilo_core::workspace::WorkspaceManifest;
use hilo_fuse::permissions::PermissionEngine;
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

    let permissions = PermissionEngine::from_rules(hilo_fuse::permissions::default_protections());
    let fs = WorkspaceMount::new(plan, config.clone(), permissions);

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

/// Two-way sync a local directory against an S3 prefix.
///
/// Non-ignored files are mirrored in both directions (last writer wins by
/// mtime vs LastModified); files matched by the ignore file (git-ignore
/// style, defaults to `<at>/.hiloignore`) stay local-only and are never
/// transferred. `.vfs/` metadata and the ignore files themselves are never
/// transferred either way.
pub fn run_workspace_sync(
    bucket: &str,
    prefix: &str,
    at: &str,
    ignore_file: Option<&str>,
    region: &str,
    dry_run: bool,
) -> Result<()> {
    if bucket.is_empty() {
        anyhow::bail!("--bucket is required for s3 workspace sync");
    }
    let local_dir = PathBuf::from(at);
    if !local_dir.is_dir() {
        anyhow::bail!("--at must be an existing directory: {at}");
    }

    // Ignore file: explicit path, else <at>/.hiloignore (missing = no ignores).
    let ignore_path = match ignore_file {
        Some(f) => PathBuf::from(f),
        None => local_dir.join(".hiloignore"),
    };
    let ignore = IgnoreMatcher::from_file(&ignore_path)
        .with_context(|| format!("failed to read ignore file {}", ignore_path.display()))?;

    let cache_dir = local_dir.join(".vfs").join("cache");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to create tokio runtime")?;

    rt.block_on(async move {
        let client = S3Client::new(region, &cache_dir, 0, true)
            .await
            .map_err(|e| anyhow::anyhow!("s3 client init failed: {e}"))?;
        let engine = SyncEngine::new(
            client,
            bucket.to_string(),
            prefix.to_string(),
            local_dir.clone(),
            ignore,
        );

        let plan = if dry_run {
            println!(
                "dry-run: sync plan for s3://{}/{} <-> {}",
                bucket,
                prefix.trim_matches('/'),
                local_dir.display()
            );
            engine
                .plan()
                .await
                .map_err(|e| anyhow::anyhow!("sync plan failed: {e}"))?
        } else {
            println!(
                "syncing s3://{}/{} <-> {}",
                bucket,
                prefix.trim_matches('/'),
                local_dir.display()
            );
            engine
                .sync()
                .await
                .map_err(|e| anyhow::anyhow!("sync failed: {e}"))?
        };

        for lf in &plan.uploads {
            println!("  ↑ {}", lf.rel_path);
        }
        for ro in &plan.downloads {
            println!("  ↓ {}", ro.rel_path);
        }
        println!(
            "sync complete: {} uploaded, {} downloaded, {} unchanged, {} ignored local, {} ignored remote",
            plan.uploads.len(),
            plan.downloads.len(),
            plan.unchanged,
            plan.ignored_local,
            plan.ignored_remote,
        );
        Ok(())
    })
}
