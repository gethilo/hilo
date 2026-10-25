//! `hilo workspace mount/unmount/sync/ephemeral/wipe` — unified FUSE tree +
//! S3 two-way sync + ephemeral classification.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use hilo_backends::{
    EphemeralMatcher, IgnoreMatcher, S3Client, S3Endpoint, SyncDirection, SyncEngine,
};
use hilo_core::workspace::{LoadedWorkspace, MountEntry, WorkspaceManifest};
use hilo_fuse::permissions::PermissionEngine;
use hilo_fuse::{daemon, workspace_mount, workspace_mount::WorkspaceMount, FuseConfig};

use super::mount::DAEMON_CHILD_ENV;
use crate::SyncDirectionArg;

/// Mount all repos and backends declared in the manifest.
///
/// The manifest is loaded from EITHER accepted schema (DF-WARPFS-50): the
/// project schema `hilo init` writes (`.vfs/manifest.yaml` — repos mount at
/// their `at` path) and the workspace schema (`repos[]`/`backends[]`/
/// `mounts[]`). A valid manifest that declares no mountable source is not an
/// error: it prints what to add and exits 0.
///
/// When `--daemon` is set, the workspace mount runs in a detached background
/// process (mirroring `hilo mount --daemon`, DF-WARPFS-130): the current
/// process re-executes itself with `HILO_MOUNT_DAEMONIZED=1`, `setsid()`s
/// the child, redirects its stdio to `/dev/null`, and returns immediately.
/// The child performs the actual `workspace_mount::mount` and keeps serving
/// until `hilo workspace unmount` tears it down.
pub fn run_workspace_mount(
    manifest_path: &str,
    mount_point: Option<&str>,
    daemon: bool,
) -> Result<()> {
    let (plan, resolved_mount_point) = match preflight(manifest_path, mount_point)? {
        MountPreflight::NothingToMount(loaded) => {
            println!("{}", nothing_to_mount_message(manifest_path, &loaded));
            return Ok(());
        }
        MountPreflight::Plan { plan, mount_point } => (plan, mount_point),
    };
    let mount_point = resolved_mount_point.display().to_string();

    // Foreground parent of a `--daemon` mount: spawn the detached child and
    // return. The child (see DAEMON_CHILD_ENV) skips this branch.
    if daemon && std::env::var_os(DAEMON_CHILD_ENV).is_none() {
        let mut cmd = build_workspace_daemon_command(manifest_path, &mount_point)?;
        let child = cmd
            .spawn()
            .context("failed to spawn background workspace mount")?;
        println!(
            "Hilo workspace mount daemonized (pid {}) — filesystem available at {mount_point}",
            child.id()
        );
        return Ok(());
    }

    let config = FuseConfig {
        mount_point: PathBuf::from(&mount_point),
        allow_other: false,
        direct_io: false,
        auto_unmount: true,
        read_only: !plan.iter().any(|entry| entry.writable),
        attr_timeout: 1.0,
        entry_timeout: 1.0,
        max_read: 131_072,
        max_write: 131_072,
        sandbox: None,
    };

    let permissions = PermissionEngine::from_rules(hilo_fuse::permissions::default_protections());
    let fs = WorkspaceMount::new(plan, config.clone(), permissions);

    println!("Hilo workspace mounted at {mount_point}");
    workspace_mount::mount(fs, &config).context("workspace FUSE mount failed")?;
    Ok(())
}

/// What [`run_workspace_mount`] learned from the manifest before touching
/// FUSE.
enum MountPreflight {
    /// Valid manifest, nothing declared to mount — exit 0.
    NothingToMount(LoadedWorkspace),
    /// Valid manifest with a mount plan and a resolved mount point.
    Plan {
        plan: Vec<MountEntry>,
        mount_point: PathBuf,
    },
}

/// Load + validate a manifest and build its mount plan.
///
/// The manifest may be in either accepted schema. Errors name the schema, the
/// field and what was expected (DF-WARPFS-50).
fn preflight(manifest_path: &str, explicit_mount_point: Option<&str>) -> Result<MountPreflight> {
    let loaded = WorkspaceManifest::load_with_dialect(manifest_path)
        .with_context(|| format!("failed to load manifest {manifest_path}"))?;

    println!(
        "manifest {manifest_path}: {} schema",
        loaded.dialect.schema_name()
    );
    if !loaded.inventory_backends.is_empty() {
        println!(
            "  note: `backends:` entries are inventory, not mount sources: {}",
            loaded.inventory_backends.join(", ")
        );
    }

    // `hilo init` writes a project manifest with no repos: mounting it is a
    // no-op, not a failure (DF-WARPFS-50 AC1).
    if loaded.declared_sources() == 0 {
        return Ok(MountPreflight::NothingToMount(loaded));
    }

    let errors = loaded.manifest.validate();
    if !errors.is_empty() {
        eprintln!(
            "manifest validation errors ({} schema):",
            loaded.dialect.schema_name()
        );
        for e in &errors {
            eprintln!("  {}: {}", e.field, e.message);
        }
        anyhow::bail!(
            "manifest validation failed with {} error(s) in the {} schema ({})",
            errors.len(),
            loaded.dialect.schema_name(),
            loaded.dialect.describe()
        );
    }

    let plan = loaded
        .manifest
        .build_mount_plan()
        .with_context(|| format!("failed to build mount plan for {manifest_path}"))?;

    if plan.is_empty() {
        anyhow::bail!("{}", empty_plan_message(manifest_path, &loaded));
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

    let mount_point = resolve_mount_point(explicit_mount_point, &loaded)?;
    Ok(MountPreflight::Plan { plan, mount_point })
}

/// Resolve the mount point: the positional argument wins, else the project
/// schema's `interfaces.fuse.mount_point`, else a named error.
fn resolve_mount_point(explicit: Option<&str>, loaded: &LoadedWorkspace) -> Result<PathBuf> {
    if let Some(mount_point) = explicit.map(str::trim).filter(|mp| !mp.is_empty()) {
        return Ok(PathBuf::from(mount_point));
    }
    if let Some(declared) = &loaded.fuse_mount_point {
        println!("mount point: {declared} (manifest interfaces.fuse.mount_point)");
        return Ok(PathBuf::from(declared));
    }
    anyhow::bail!(
        "no mount point: pass it as the positional argument \
         (`hilo workspace mount <MOUNT_POINT>`) or declare \
         `interfaces.fuse.mount_point` in the project schema"
    )
}

/// Explain why a valid manifest mounts nothing, naming the schema and the
/// missing field.
fn empty_plan_message(manifest_path: &str, loaded: &LoadedWorkspace) -> String {
    format!(
        "manifest {manifest_path} ({} schema) declares {} source(s) but no `mounts:` — \
         repos and backends alone mount nothing; expected a `mounts:` entry per source \
         (`- source: <repo or backend name>` + `at: /<path>`), or `repos[].at` in the \
         project schema. See docs/workspace.md",
        loaded.dialect.schema_name(),
        loaded.declared_sources()
    )
}

/// Explain that an initialized-but-empty project manifest has nothing to
/// mount, and show the smallest working addition.
fn nothing_to_mount_message(manifest_path: &str, loaded: &LoadedWorkspace) -> String {
    let schema = loaded.dialect.schema_name();
    let inventory = if loaded.inventory_backends.is_empty() {
        String::new()
    } else {
        format!(
            "\n  `backends:` inventory is declared ({}), but backend inventory is not a \
             mount source — register it with `hilo backend mount` or add a repo.",
            loaded.inventory_backends.join(", ")
        )
    };
    format!(
        "nothing to mount: manifest {manifest_path} ({schema} schema) declares no repos.\n  \
         Add a repo with a mount path:\n    \
         repos:\n      - name: shared-lib\n        url: git@github.com:org/shared-lib.git\n        ref: main\n        at: /shared-lib\n  \
         then re-run: hilo workspace mount --manifest {manifest_path} <MOUNT_POINT>{inventory}\n  \
         see docs/workspace.md"
    )
}

/// Build the detached child command for `hilo workspace mount --daemon`.
///
/// Re-executes the current binary as `hilo workspace mount --manifest
/// <manifest> <mount_point> --daemon` with `HILO_MOUNT_DAEMONIZED=1`, makes
/// the child a session leader (`setsid`) and detaches its stdio so the parent
/// can return immediately. Mirrors `mount::build_daemon_command`
/// (DF-WARPFS-130).
pub fn build_workspace_daemon_command(manifest_path: &str, mount_point: &str) -> Result<Command> {
    let exe = std::env::current_exe().context("failed to locate the hilo binary")?;
    let mut cmd = Command::new(exe);
    cmd.arg("workspace").arg("mount");
    cmd.arg("--manifest").arg(manifest_path);
    cmd.arg(mount_point);
    cmd.arg("--daemon");
    cmd.env(DAEMON_CHILD_ENV, "1");
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Safety: setsid() is async-signal-safe and does not allocate.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    Ok(cmd)
}

/// Unmount a workspace at the given mount point.
pub fn run_workspace_unmount(mount_point: &str) -> Result<()> {
    let path = PathBuf::from(mount_point);
    daemon::unmount(&path).context("workspace unmount failed")?;
    println!("Hilo workspace unmounted from {}", mount_point);
    Ok(())
}

/// Sync a local directory against an S3 prefix (DF-WARPFS-12).
///
/// Non-ignored files are mirrored between local and remote; files matched by
/// the ignore file (git-ignore style, defaults to `<at>/.hiloignore`) stay
/// local-only and are never transferred. `.vfs/` metadata and the ignore
/// files themselves are never transferred either way.
///
/// The run header always names the resolved endpoint, bucket and direction:
/// `--endpoint` (explicit, beats the environment) or the `AWS_ENDPOINT_URL`
/// environment variable selects an S3-compatible endpoint; with neither,
/// the client uses the ambient AWS config chain — and says so.
#[allow(clippy::too_many_arguments)]
pub fn run_workspace_sync(
    bucket: &str,
    prefix: &str,
    at: &str,
    ignore_file: Option<&str>,
    region: &str,
    endpoint: Option<&str>,
    direction: SyncDirectionArg,
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
        // DF-WARPFS-12: an explicit --endpoint beats AWS_ENDPOINT_URL;
        // otherwise resolve from the environment. Either way the resolved
        // endpoint is DISCLOSED on the header — never a silent default.
        let resolved = match endpoint {
            Some(ep) if !ep.is_empty() => S3Endpoint::Url(ep.to_string()),
            _ => S3Endpoint::from_env(),
        };
        let client = S3Client::with_endpoint(&resolved, region, &cache_dir, 0, true)
            .await
            .map_err(|e| anyhow::anyhow!("s3 client init failed: {e}"))?;
        println!(
            "sync {} s3://{}/{} <-> {} [bucket={} endpoint={} region={}]",
            direction.label(),
            bucket,
            prefix.trim_matches('/'),
            local_dir.display(),
            bucket,
            client.endpoint.display(),
            region,
        );
        // DF-WARPFS-126: the direction is ENFORCED, not just labeled —
        // --push never downloads, --pull never uploads (planner parity).
        let engine = SyncEngine::new(
            client,
            bucket.to_string(),
            prefix.to_string(),
            local_dir.clone(),
            ignore,
            match direction {
                SyncDirectionArg::Push => SyncDirection::Push,
                SyncDirectionArg::Pull => SyncDirection::Pull,
                SyncDirectionArg::Both => SyncDirection::Both,
            },
        );

        let plan = if dry_run {
            println!("dry-run: sync plan follows");
            engine
                .plan()
                .await
                .map_err(|e| anyhow::anyhow!("sync plan failed: {e}"))?
        } else {
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
            "sync complete: {} uploaded, {} downloaded, {} unchanged, {} ignored local, {} ignored remote{}",
            plan.uploads.len(),
            plan.downloads.len(),
            plan.unchanged,
            plan.ignored_local,
            plan.ignored_remote,
            if plan.unused_local + plan.unused_remote > 0 {
                format!(
                    ", {} skipped by direction ({} local, {} remote)",
                    plan.unused_local + plan.unused_remote,
                    plan.unused_local,
                    plan.unused_remote
                )
            } else {
                String::new()
            },
        );
        Ok(())
    })
}

/// `hilo workspace ephemeral [PATH...]` — list ephemeral (rebuildable /
/// redownloadable) files as TSV: `path<TAB>size<TAB>reason`.
///
/// Classification uses the built-in ephemeral catalog (spec §5.1) plus the
/// workspace `.hiloephemeral` file when present. Defaults to the whole
/// workspace root; PATH arguments limit the listing to those subtrees.
pub fn run_workspace_ephemeral(paths: &[PathBuf]) -> Result<()> {
    let root = std::env::current_dir().context("failed to get current directory")?;
    let matcher =
        EphemeralMatcher::load(&root, None).context("failed to load ephemeral catalog")?;
    let entries = matcher.scan(&root).context("failed to scan workspace")?;
    for e in entries
        .iter()
        .filter(|e| paths.is_empty() || paths.iter().any(|p| e.path.starts_with(p)))
    {
        println!("{}\t{}\t{}", e.path.display(), e.size, e.reason);
    }
    Ok(())
}

/// `hilo workspace wipe --ephemeral [--apply]` — plan or apply a wipe of
/// ephemeral files.
///
/// Default is a dry-run plan (lists what would be removed and the bytes it
/// would free). With `--apply`, only ephemeral files are deleted and the
/// freed bytes are reported. `user.vfs.ephemeral = false` is the only wipe
/// protector (spec §5.2.2); symlinks are never touched and the walk never
/// crosses the workspace root.
pub fn run_workspace_wipe(apply: bool) -> Result<()> {
    let root = std::env::current_dir().context("failed to get current directory")?;
    let matcher =
        EphemeralMatcher::load(&root, None).context("failed to load ephemeral catalog")?;
    let entries = matcher.scan(&root).context("failed to scan workspace")?;

    let mut freed: u64 = 0;
    let mut wiped = 0usize;
    let mut protected = 0usize;
    for e in &entries {
        let full = root.join(&e.path);
        // user.vfs.ephemeral = false is the ONLY wipe protector.
        if let Some(v) = hilo_metadata::xattr::get_vfs_xattr(&full, "ephemeral")
            .context("failed to read user.vfs.ephemeral")?
        {
            if v == "false" || v == "0" {
                protected += 1;
                continue;
            }
        }
        if apply {
            std::fs::remove_file(&full)
                .with_context(|| format!("failed to remove {}", full.display()))?;
            println!("removed\t{}", e.path.display());
        } else {
            println!("would remove\t{}", e.path.display());
        }
        wiped += 1;
        freed += e.size;
    }

    if apply {
        println!("freed {freed} bytes across {wiped} file(s)");
    } else {
        println!(
            "would free {freed} bytes across {wiped} file(s) (dry-run; pass --apply to delete)"
        );
    }
    if protected > 0 {
        println!("skipped {protected} wipe-protected file(s) (user.vfs.ephemeral=false)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    // ───────── DF-WARPFS-130: workspace mount --daemon command shape ─────────

    #[test]
    fn test_build_workspace_daemon_command_args() {
        let cmd = build_workspace_daemon_command(".vfs/manifest.yaml", "/mnt/hilo").unwrap();
        let args: Vec<OsString> = cmd.get_args().map(OsString::from).collect();
        assert_eq!(
            args,
            vec![
                OsString::from("workspace"),
                OsString::from("mount"),
                OsString::from("--manifest"),
                OsString::from(".vfs/manifest.yaml"),
                OsString::from("/mnt/hilo"),
                OsString::from("--daemon"),
            ]
        );
    }

    #[test]
    fn test_build_workspace_daemon_command_sets_marker_env() {
        let cmd = build_workspace_daemon_command(".vfs/manifest.yaml", "/mnt/hilo").unwrap();
        let envs: Vec<(OsString, Option<OsString>)> = cmd
            .get_envs()
            .map(|(k, v)| (k.to_os_string(), v.map(OsString::from)))
            .collect();
        assert!(
            envs.contains(&(
                OsString::from("HILO_MOUNT_DAEMONIZED"),
                Some(OsString::from("1"))
            )),
            "daemon child marker env must be set: {envs:?}"
        );
    }

    #[test]
    fn test_build_workspace_daemon_command_program_is_current_exe() {
        let cmd = build_workspace_daemon_command(".vfs/manifest.yaml", "/mnt/hilo").unwrap();
        assert_eq!(
            cmd.get_program(),
            std::env::current_exe().expect("current exe resolves")
        );
    }
}
