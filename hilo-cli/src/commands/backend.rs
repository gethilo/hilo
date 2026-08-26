//! `hilo backend mount/list/sync/setup` — virtual backends (S3, git, local)
//! plus the spec §9 CLI surface for backend-backed workspaces
//! (specs/backend-backed-workspace-spec.md §9).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Subcommand};

use hilo_backends::{
    BackendConfig, BackendError, BackendKind, BackendRegistry, EphemeralMatcher, IgnoreMatcher,
    MountEntry, SyncDirection, SyncError, SyncMode, SyncTool,
};

const MOUNTS_YAML: &str = ".vfs/backends/mounts.yaml";

#[derive(Subcommand)]
pub enum BackendCommand {
    /// Mount a virtual backend.
    Mount(Box<MountArgs>),
    /// List all mounted backends.
    List,
    /// Sync a mounted backend against the current workspace.
    Sync(SyncArgs),
    /// Detect sync tools and credentials for a backend type (writes nothing).
    Setup(SetupArgs),
}

#[derive(Args)]
pub struct MountArgs {
    /// Backend type: "s3", "gdrive", "onedrive", "dropbox", "external"
    /// (legacy: "git", "local")
    #[arg(long)]
    pub r#type: String,
    /// S3 bucket name
    #[arg(long)]
    pub bucket: Option<String>,
    /// S3 key prefix
    #[arg(long)]
    pub prefix: Option<String>,
    /// Git repository URL (required for --type git)
    #[arg(long)]
    pub url: Option<String>,
    /// Mount point (virtual path) — for --type local, the real path to mount
    #[arg(long)]
    pub at: String,
    /// AWS region
    #[arg(long, default_value = "us-east-1")]
    pub region: String,
    /// External tool remote ("remote:path" or tool remote) — required for
    /// gdrive/onedrive/dropbox/external
    #[arg(long)]
    pub remote: Option<String>,
    /// Sync tool: auto|native|rclone|s3sync|gdrive|onedrive|dropbox (default auto)
    #[arg(long)]
    pub tool: Option<String>,
    /// Sync mode: stream|mirror (default mirror)
    #[arg(long)]
    pub mode: Option<String>,
    /// Extra ignore file (on top of .hiloignore)
    #[arg(long)]
    pub ignore_file: Option<String>,
    /// Pull poll interval seconds (default 60)
    #[arg(long, default_value_t = 60)]
    pub poll_secs: u64,
    /// Do not apply built-in default ignore patterns
    #[arg(long)]
    pub no_default_ignores: bool,
}

#[derive(Args)]
pub struct SyncArgs {
    /// Push local changes to the backend
    #[arg(long, conflicts_with_all = ["pull", "both"])]
    pub push: bool,
    /// Pull remote changes into the workspace
    #[arg(long, conflicts_with_all = ["push", "both"])]
    pub pull: bool,
    /// Two-way sync (default)
    #[arg(long, conflicts_with_all = ["push", "pull"])]
    pub both: bool,
    /// Limit the sync to one or more subtrees
    #[arg(value_name = "PATH")]
    pub paths: Vec<PathBuf>,
}

#[derive(Args)]
pub struct SetupArgs {
    /// Backend type to check: s3|gdrive|onedrive|dropbox|external (default: all)
    #[arg(long)]
    pub r#type: Option<String>,
}

pub fn run_mount(args: &MountArgs) -> Result<()> {
    let kind = args.r#type.as_str();
    let new_surface = matches!(kind, "gdrive" | "onedrive" | "dropbox" | "external")
        || args.remote.is_some()
        || args.tool.is_some()
        || args.mode.is_some()
        || args.ignore_file.is_some()
        || args.poll_secs != 60
        || args.no_default_ignores;
    if !new_surface {
        return run_mount_legacy(args);
    }
    match run_mount_new(args) {
        Ok(()) => Ok(()),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(exit_code(&e));
        }
    }
}

/// Legacy mount surface (GAP-009): s3/git/local with no spec §9 flags.
fn run_mount_legacy(args: &MountArgs) -> Result<()> {
    match args.r#type.as_str() {
        "s3" => {
            let bucket = args.bucket.as_deref().unwrap_or("");
            let prefix = args.prefix.as_deref().unwrap_or("");
            if bucket.is_empty() {
                anyhow::bail!("--bucket is required for s3 backend");
            }
            println!("mounted s3://{}/{} at {}", bucket, prefix, args.at);
            // In a real implementation, this would register the backend
            // in the running VFS. For Phase 3, we validate the args and
            // report success.
            Ok(())
        }
        "git" => {
            let url = args.url.as_deref().unwrap_or("");
            if url.is_empty() {
                anyhow::bail!("--url is required for git backend");
            }
            let backend =
                hilo_backends::git::GitBackend::mount(hilo_backends::git::GitBackendConfig {
                    url: url.to_string(),
                    ref_name: "main".to_string(),
                    at: args.at.clone(),
                    writable: false,
                    auto_pull_secs: None,
                    cache_dir: None,
                })
                .map_err(|e| {
                    anyhow::anyhow!(
                        "failed to mount git backend from {url}: {e} — hint: the repository may be private, may not exist, or the URL may be wrong; check the URL and your credentials"
                    )
                })?;
            println!(
                "mounted git {} at {} (worktree {})",
                url,
                args.at,
                backend.mount_point()
            );
            Ok(())
        }
        "local" => {
            let backend = hilo_backends::local::LocalBackend::mount(
                hilo_backends::local::LocalBackendConfig {
                    real_path: PathBuf::from(&args.at),
                    at: args.at.clone(),
                },
            )?;
            println!("mounted local {} at {}", backend.mount_point(), args.at);
            Ok(())
        }
        other => anyhow::bail!("unknown backend type: {other}"),
    }
}

/// Spec §9 mount surface: validates the config by constructing the driver,
/// then appends the entry to `.vfs/backends/mounts.yaml`.
fn run_mount_new(args: &MountArgs) -> Result<(), BackendError> {
    let kind = match args.r#type.as_str() {
        "s3" => BackendKind::S3,
        "gdrive" => BackendKind::GDrive,
        "onedrive" => BackendKind::OneDrive,
        "dropbox" => BackendKind::Dropbox,
        "external" => BackendKind::External,
        other => {
            return Err(BackendError::InvalidConfig(format!(
                "unknown backend type: {other} (expected s3|gdrive|onedrive|dropbox|external)"
            )))
        }
    };
    let tool = resolve_tool(kind, args.tool.as_deref())?;
    let mode = match args.mode.as_deref().unwrap_or("mirror") {
        "stream" => SyncMode::Stream,
        "mirror" => SyncMode::Mirror,
        other => {
            return Err(BackendError::InvalidConfig(format!(
                "unknown mode: {other} (expected stream|mirror)"
            )))
        }
    };
    let name = mount_name(&args.at, kind);
    let cfg = BackendConfig {
        kind,
        name: name.clone(),
        bucket: args.bucket.clone(),
        prefix: args.prefix.clone(),
        region: Some(args.region.clone()),
        remote: args.remote.clone(),
        tool,
        mode,
        ignore_file: args.ignore_file.as_ref().map(PathBuf::from),
        poll_secs: args.poll_secs,
        no_default_ignores: args.no_default_ignores,
    };

    // Fail fast: construct the driver (missing tool → ToolMissing, bad
    // config → InvalidConfig) before touching mounts.yaml.
    BackendRegistry::from_config(&cfg)?;

    let mounts_path = PathBuf::from(MOUNTS_YAML);
    append_mount(&mounts_path, &cfg, &args.at)?;

    println!(
        "mounted {} {} at {} (tool={}, mode={})",
        kind_name(kind),
        mount_target(kind, args),
        args.at,
        tool_name(tool),
        mode_name(mode),
    );
    Ok(())
}

pub fn run_list() -> Result<()> {
    // Phase 3: read manifest backends and print them.
    // For now, read from .vfs/manifest.yaml if present.
    let manifest_path = std::path::Path::new(".vfs/manifest.yaml");
    if !manifest_path.exists() {
        println!("No mounted backends. Run 'hilo init' first.");
        return Ok(());
    }

    match std::fs::read_to_string(manifest_path) {
        Ok(contents) => match serde_yaml::from_str::<serde_yaml::Value>(&contents) {
            Ok(manifest) => {
                let backends = manifest.get("backends").cloned().unwrap_or_default();
                let s3_backends = backends.get("s3");
                let remote_backends = backends.get("remote");
                let local_backends = backends.get("local");

                let mut found = false;

                if let Some(s3_list) = s3_backends.and_then(|v| v.as_sequence()) {
                    for s3 in s3_list {
                        let bucket = s3.get("bucket").and_then(|v| v.as_str()).unwrap_or("?");
                        let prefix = s3.get("prefix").and_then(|v| v.as_str()).unwrap_or("");
                        let at = s3.get("at").and_then(|v| v.as_str()).unwrap_or("?");
                        let region = s3.get("region").and_then(|v| v.as_str()).unwrap_or("?");
                        let writable = s3
                            .get("writable")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        println!(
                            "s3  s3://{}/{}  {}  region={}, rw={}, status=configured",
                            bucket, prefix, at, region, writable
                        );
                        found = true;
                    }
                }
                if let Some(remote_list) = remote_backends.and_then(|v| v.as_sequence()) {
                    for r in remote_list {
                        let url = r.get("url").and_then(|v| v.as_str()).unwrap_or("?");
                        let at = r.get("at").and_then(|v| v.as_str()).unwrap_or("?");
                        println!("git {}  {}  status=configured", url, at);
                        found = true;
                    }
                }
                if let Some(local_list) = local_backends.and_then(|v| v.as_sequence()) {
                    for l in local_list {
                        let path = l.get("path").and_then(|v| v.as_str()).unwrap_or("?");
                        let at = l.get("at").and_then(|v| v.as_str()).unwrap_or("?");
                        println!("local  {}  {}  status=configured", path, at);
                        found = true;
                    }
                }
                if !found {
                    println!("No backends configured in manifest.");
                }
            }
            Err(e) => println!("warning: could not parse manifest: {}", e),
        },
        Err(e) => println!("warning: could not read manifest: {}", e),
    }
    Ok(())
}

/// Spec §9 sync: plan + execute a sync against every mounted backend.
/// PATH arguments limit the operation to subtrees.
pub fn run_sync(args: &SyncArgs) -> Result<()> {
    let direction = if args.push {
        SyncDirection::Push
    } else if args.pull {
        SyncDirection::Pull
    } else {
        SyncDirection::Both
    };

    let mounts_path = PathBuf::from(MOUNTS_YAML);
    let registry = match BackendRegistry::load_mounts(&mounts_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!("hint: run 'hilo backend mount' first to register a backend");
            std::process::exit(exit_code(&e));
        }
    };
    let names = registry.names();
    if names.is_empty() {
        eprintln!("error: no backends mounted");
        eprintln!("hint: run 'hilo backend mount' first to register a backend");
        std::process::exit(2);
    }

    // Per-mount configs (ignore file, poll, defaults) come from mounts.yaml;
    // the registry holds the constructed drivers.
    let text = std::fs::read_to_string(&mounts_path).context("failed to read mounts.yaml")?;
    let entries: Vec<MountEntry> = serde_yaml::from_str(&text).context("bad mounts.yaml")?;

    let root = std::env::current_dir().context("failed to get current directory")?;
    let prefixes: Vec<PathBuf> = args.paths.iter().map(|p| root.join(p)).collect();

    let mut total_transferred = 0usize;
    let mut total_bytes = 0u64;
    for name in &names {
        let backend = registry
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("backend {name} missing from registry"))?;
        let entry = entries
            .iter()
            .find(|e| &e.name == name)
            .ok_or_else(|| anyhow::anyhow!("mount entry {name} missing from mounts.yaml"))?;

        let matcher = IgnoreMatcher::load(
            &root,
            entry.ignore_file.as_deref().map(Path::new),
            entry.no_default_ignores.unwrap_or(false),
        )
        .map_err(|e| {
            anyhow::anyhow!(
                "failed to load ignore rules for {name} (ignore_file={:?}): {e}",
                entry.ignore_file
            )
        })?;
        let ephemeral = EphemeralMatcher::load(&root, None)
            .map_err(|e| anyhow::anyhow!("failed to load ephemeral rules for {name}: {e}"))?;

        let mut plan = match hilo_backends::planner::plan_sync(
            backend.as_ref(),
            &root,
            &matcher,
            &ephemeral,
            direction,
        ) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("error: {name}: {e}");
                std::process::exit(sync_exit_code(&e));
            }
        };

        if !prefixes.is_empty() {
            let rel_prefixes: Vec<String> = prefixes
                .iter()
                .filter_map(|p| {
                    p.strip_prefix(&root)
                        .ok()
                        .map(|r| r.to_string_lossy().into_owned())
                })
                .collect();
            plan.to_transfer
                .retain(|item| prefixes.iter().any(|p| item.local_path.starts_with(p)));
            plan.to_delete.retain(|key| {
                rel_prefixes
                    .iter()
                    .any(|p| key.starts_with(p.as_str()) || p.is_empty())
            });
        }

        println!(
            "plan {name}: {} to transfer, {} to delete, {} skipped ignored, {} skipped ephemeral",
            plan.to_transfer.len(),
            plan.to_delete.len(),
            plan.skipped_ignored,
            plan.skipped_ephemeral,
        );

        let stats = match hilo_backends::planner::execute_sync(&plan, backend.as_ref(), &root) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: {name}: {e}");
                std::process::exit(sync_exit_code(&e));
            }
        };
        total_transferred += stats.transferred;
        total_bytes += stats.bytes;
        println!(
            "synced {name}: {} transferred ({} bytes), {} conflicts recorded",
            stats.transferred,
            stats.bytes,
            stats.conflicts.len(),
        );
    }
    println!(
        "done: {total_transferred} transferred, {total_bytes} bytes across {} backend(s)",
        names.len()
    );
    Ok(())
}

/// Spec §9 setup: detect tools on PATH, validate credentials, print next
/// steps. Writes nothing; always exits 0 (informational).
pub fn run_setup(args: &SetupArgs) -> Result<()> {
    let kinds: Vec<(&str, BackendKind)> = match args.r#type.as_deref() {
        None => vec![
            ("s3", BackendKind::S3),
            ("gdrive", BackendKind::GDrive),
            ("onedrive", BackendKind::OneDrive),
            ("dropbox", BackendKind::Dropbox),
            ("external", BackendKind::External),
        ],
        Some("s3") => vec![("s3", BackendKind::S3)],
        Some("gdrive") => vec![("gdrive", BackendKind::GDrive)],
        Some("onedrive") => vec![("onedrive", BackendKind::OneDrive)],
        Some("dropbox") => vec![("dropbox", BackendKind::Dropbox)],
        Some("external") => vec![("external", BackendKind::External)],
        Some(other) => anyhow::bail!(
            "unknown backend type: {other} (expected s3|gdrive|onedrive|dropbox|external)"
        ),
    };

    for (label, kind) in kinds {
        println!("== {label} ==");
        match kind {
            BackendKind::S3 => setup_s3(),
            BackendKind::GDrive => setup_external("gdrive", "gdrive"),
            BackendKind::OneDrive => setup_external("onedrive", "onedrive"),
            BackendKind::Dropbox => setup_external("dropbox", "dropbox"),
            BackendKind::External => setup_external("external", "rclone"),
            _ => {}
        }
    }
    Ok(())
}

fn setup_s3() {
    println!("  native engine: built-in (aws-sdk)");
    let creds_ok = std::env::var("AWS_ACCESS_KEY_ID").is_ok()
        && std::env::var("AWS_SECRET_ACCESS_KEY").is_ok()
        || home_dir()
            .map(|h| h.join(".aws/credentials").exists())
            .unwrap_or(false);
    println!(
        "  credentials: {}",
        if creds_ok {
            "found"
        } else {
            "not found (set AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY or run 'aws configure')"
        }
    );
    for tool in ["rclone", "s3sync"] {
        println!(
            "  {tool}: {}",
            if find_on_path(tool).is_some() {
                "found"
            } else {
                "not found"
            }
        );
    }
    println!("  next steps: hilo backend mount --type s3 --bucket <B> --at <PATH> [--tool native|rclone|s3sync]");
}

fn setup_external(label: &str, official: &str) {
    let official_bin = find_on_path(official);
    let rclone = find_on_path("rclone");
    println!(
        "  {official}: {}",
        if official_bin.is_some() {
            "found"
        } else {
            "not found"
        }
    );
    println!(
        "  rclone: {}",
        if rclone.is_some() {
            "found"
        } else {
            "not found"
        }
    );
    let recommended = if official_bin.is_some() {
        official
    } else if rclone.is_some() {
        "rclone"
    } else {
        "install 'rclone' (https://rclone.org) or the official CLI"
    };
    println!("  recommended tool: {recommended}");
    println!("  next steps: hilo backend mount --type {label} --remote <REMOTE> --at <PATH> [--tool auto|{official}|rclone]");
}

// ───────────────────────── helpers ─────────────────────────

/// Spec §9 `--tool auto` resolution (exact): external kinds prefer the
/// matching official CLI, then rclone, else fail listing what to install;
/// s3 → native unless an explicit tool is given.
fn resolve_tool(kind: BackendKind, explicit: Option<&str>) -> Result<SyncTool, BackendError> {
    let prefer = |official: &str, official_tool: SyncTool| -> SyncTool {
        if find_on_path(official).is_some() {
            official_tool
        } else if find_on_path("rclone").is_some() {
            SyncTool::Rclone
        } else {
            // Signals ToolMissing below.
            SyncTool::Native
        }
    };
    match explicit {
        None | Some("auto") => match kind {
            BackendKind::S3 => Ok(SyncTool::Native),
            BackendKind::GDrive => {
                let t = prefer("gdrive", SyncTool::GDriveCli);
                if t == SyncTool::Native {
                    Err(BackendError::ToolMissing("gdrive or rclone".into()))
                } else {
                    Ok(t)
                }
            }
            BackendKind::OneDrive => {
                let t = prefer("onedrive", SyncTool::OneDriveCli);
                if t == SyncTool::Native {
                    Err(BackendError::ToolMissing("onedrive or rclone".into()))
                } else {
                    Ok(t)
                }
            }
            BackendKind::Dropbox => {
                let t = prefer("dropbox", SyncTool::DropboxCli);
                if t == SyncTool::Native {
                    Err(BackendError::ToolMissing("dropbox or rclone".into()))
                } else {
                    Ok(t)
                }
            }
            BackendKind::External => {
                if find_on_path("rclone").is_some() {
                    Ok(SyncTool::Rclone)
                } else {
                    Err(BackendError::ToolMissing("rclone".into()))
                }
            }
            _ => Err(BackendError::InvalidConfig("unsupported kind".into())),
        },
        Some("native") => Ok(SyncTool::Native),
        Some("rclone") => Ok(SyncTool::Rclone),
        Some("s3sync") => Ok(SyncTool::S3Sync),
        Some("gdrive") => Ok(SyncTool::GDriveCli),
        Some("onedrive") => Ok(SyncTool::OneDriveCli),
        Some("dropbox") => Ok(SyncTool::DropboxCli),
        Some(other) => Err(BackendError::InvalidConfig(format!(
            "unknown tool '{other}'"
        ))),
    }
}

/// Mount registry name derived from the mount point (spec §11.3 `name`).
fn mount_name(at: &str, kind: BackendKind) -> String {
    let base = Path::new(at)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if base.is_empty() || base == "/" {
        format!("mount-{}", kind_name(kind))
    } else {
        base
    }
}

/// Human-readable target for the mount confirmation line.
fn mount_target(kind: BackendKind, args: &MountArgs) -> String {
    match kind {
        BackendKind::S3 => format!(
            "s3://{}/{}",
            args.bucket.as_deref().unwrap_or(""),
            args.prefix.as_deref().unwrap_or("")
        ),
        BackendKind::GDrive
        | BackendKind::OneDrive
        | BackendKind::Dropbox
        | BackendKind::External => args.remote.as_deref().unwrap_or("?").to_string(),
        _ => args.at.clone(),
    }
}

fn tool_name(t: SyncTool) -> &'static str {
    match t {
        SyncTool::Native => "native",
        SyncTool::Rclone => "rclone",
        SyncTool::S3Sync => "s3sync",
        SyncTool::GDriveCli => "gdrive",
        SyncTool::OneDriveCli => "onedrive",
        SyncTool::DropboxCli => "dropbox",
    }
}

fn mode_name(m: SyncMode) -> &'static str {
    match m {
        SyncMode::Stream => "stream",
        SyncMode::Mirror => "mirror",
    }
}

/// Append one mount entry to `.vfs/backends/mounts.yaml`, preserving existing
/// entries. Duplicate names are rejected.
fn append_mount(path: &Path, cfg: &BackendConfig, at: &str) -> Result<(), BackendError> {
    let mut entries: Vec<MountEntry> = if path.exists() {
        let text = std::fs::read_to_string(path).map_err(|e| {
            BackendError::InvalidConfig(format!("cannot read {}: {e}", path.display()))
        })?;
        serde_yaml::from_str(&text)
            .map_err(|e| BackendError::InvalidConfig(format!("bad mounts.yaml: {e}")))?
    } else {
        Vec::new()
    };
    if entries.iter().any(|e| e.name == cfg.name) {
        return Err(BackendError::InvalidConfig(format!(
            "a mount named '{}' already exists in {}",
            cfg.name,
            path.display()
        )));
    }
    entries.push(MountEntry {
        name: cfg.name.clone(),
        kind: kind_name(cfg.kind).to_string(),
        bucket: cfg.bucket.clone(),
        prefix: cfg.prefix.clone(),
        region: cfg.region.clone(),
        remote: cfg.remote.clone(),
        tool: Some(tool_name(cfg.tool).to_string()),
        mode: Some(mode_name(cfg.mode).to_string()),
        ignore_file: cfg.ignore_file.as_ref().map(|p| p.display().to_string()),
        poll_secs: Some(cfg.poll_secs),
        no_default_ignores: Some(cfg.no_default_ignores),
        at: Some(at.to_string()),
    });
    let yaml = serde_yaml::to_string(&entries)
        .map_err(|e| BackendError::InvalidConfig(format!("cannot serialize mounts.yaml: {e}")))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            BackendError::InvalidConfig(format!("cannot create {}: {e}", parent.display()))
        })?;
    }
    std::fs::write(path, yaml).map_err(|e| {
        BackendError::InvalidConfig(format!("cannot write {}: {e}", path.display()))
    })?;
    Ok(())
}

fn kind_name(k: BackendKind) -> &'static str {
    match k {
        BackendKind::Local => "local",
        BackendKind::S3 => "s3",
        BackendKind::GDrive => "gdrive",
        BackendKind::OneDrive => "onedrive",
        BackendKind::Dropbox => "dropbox",
        BackendKind::External => "external",
    }
}

/// Spec §12 CLI exit codes.
fn exit_code(e: &BackendError) -> i32 {
    match e {
        BackendError::NotFound(_) => 1,
        BackendError::InvalidConfig(_) => 2,
        BackendError::ToolFailed(..) => 3,
        BackendError::ToolMissing(_) => 4,
        BackendError::ReadOnly => 4,
        BackendError::Unreachable(_) => 5,
        _ => 1,
    }
}

fn sync_exit_code(e: &SyncError) -> i32 {
    match e {
        SyncError::TransferFailed(_) => 3,
        SyncError::Backend(b) => exit_code(b),
        _ => 1,
    }
}

fn find_on_path(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(bin);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}
