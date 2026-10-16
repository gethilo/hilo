//! `hilo sandbox check` — verify the manifest bubblewrap sandbox end-to-end.
//!
//! DF-WARPFS-77: the manifest `sandbox:` block is honored at mount time, and
//! this command is the verification surface. It builds a
//! `hilo_core::sandbox::BubblewrapExecutor` from the parsed manifest config
//! and executes a trivial command through `BubblewrapExecutor::run` (bwrap),
//! proving the sandbox works end-to-end without mounting.

use std::path::Path;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};

use hilo_core::sandbox::BubblewrapExecutor;

use super::mount::load_sandbox;

#[derive(Subcommand)]
pub enum SandboxCommand {
    /// Verify the manifest bubblewrap sandbox by executing a trivial command
    /// through `BubblewrapExecutor::run` (bwrap) end-to-end.
    ///
    /// This is the documented entry point for the §14.3 sandbox executor: it
    /// builds a `BubblewrapExecutor` from the parsed `sandbox:` block and runs
    /// `sh -c "echo ok"` inside it. Exit 0 means the sandboxed command
    /// succeeded (bwrap is present and functional); a non-zero exit reports
    /// what failed. A missing or disabled block is a no-op ("nothing to
    /// check") — the mount itself proceeds unsandboxed in that case.
    Check(SandboxCheckArgs),
    /// Execute a command inside the manifest's bubblewrap sandbox (§14.1).
    ///
    /// Builds a `BubblewrapExecutor` from the parsed `sandbox:` block and runs
    /// the given command through `BubblewrapExecutor::run_inherit` — stdio is
    /// inherited, so the sandboxed process's output streams through and its
    /// exit status becomes this command's exit status. The command runs with
    /// the sandbox's manifest settings: read-only root, network isolation
    /// (`isolate_network: true` blocks all sockets), PID isolation, and
    /// writable paths. This is the production launch surface for sandboxed
    /// agent shells on a mounted workspace.
    Exec(SandboxExecArgs),
}

#[derive(Args)]
pub struct SandboxCheckArgs {
    /// Backing source directory whose manifest is checked (default: the
    /// current directory).
    pub(crate) source: Option<String>,
}

#[derive(Args)]
pub struct SandboxExecArgs {
    /// Command (and arguments) to run inside the sandbox.
    #[arg(required = true, trailing_var_arg = false)]
    pub(crate) command: Vec<String>,

    /// Backing source directory whose manifest defines the sandbox
    /// (default: the current directory). The workspace bind inside the
    /// sandbox is this directory.
    #[arg(long)]
    pub(crate) source: Option<String>,
}

/// Run `hilo sandbox exec -- <COMMAND>...`.
pub fn run_sandbox_exec(args: &SandboxExecArgs) -> Result<()> {
    if args.command.is_empty() {
        anyhow::bail!("sandbox exec: no command given");
    }

    let dir = match &args.source {
        None => std::env::current_dir().context("failed to determine the current directory")?,
        Some(src) => Path::new(src)
            .canonicalize()
            .with_context(|| format!("source directory {src} does not exist"))?,
    };

    let cfg = match load_sandbox(&dir) {
        Some(cfg) if cfg.enabled => cfg,
        _ => {
            anyhow::bail!(
                "no enabled sandbox block in the manifest for {} — nothing to exec inside",
                dir.display()
            );
        }
    };

    if !BubblewrapExecutor::is_available() {
        eprintln!(
            "warning: manifest sandbox block present but bwrap not found — cannot exec inside the sandbox (install: apt install bubblewrap)"
        );
        anyhow::bail!("bwrap not found");
    }

    let workspace = dir.to_string_lossy().to_string();
    let executor = BubblewrapExecutor::new(cfg, &workspace);
    let status = executor
        .run_inherit(&args.command)
        .map_err(|e| anyhow::anyhow!("sandbox exec failed: {e}"))?;

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    Ok(())
}

/// Run `hilo sandbox check [SOURCE]`.
pub fn run_sandbox_check(source: Option<&str>) -> Result<()> {
    let dir = match source {
        None => std::env::current_dir().context("failed to determine the current directory")?,
        Some(src) => Path::new(src)
            .canonicalize()
            .with_context(|| format!("source directory {src} does not exist"))?,
    };

    let cfg = match load_sandbox(&dir) {
        Some(cfg) if cfg.enabled => cfg,
        _ => {
            println!(
                "no enabled sandbox block in the manifest — nothing to check (mount runs unsandboxed)"
            );
            return Ok(());
        }
    };

    if !BubblewrapExecutor::is_available() {
        eprintln!(
            "warning: manifest sandbox block present but bwrap not found — cannot run the sandbox check (install: apt install bubblewrap)"
        );
        anyhow::bail!("bwrap not found");
    }

    let workspace = dir.to_string_lossy().to_string();
    let executor = BubblewrapExecutor::new(cfg.clone(), &workspace);
    let output = executor
        .run(&["sh".into(), "-c".into(), "echo ok".into()])
        .map_err(|e| anyhow::anyhow!("sandbox check failed: {e}"))?;

    if !output.status.success() {
        anyhow::bail!("sandbox check failed: bwrap exited with {}", output.status);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    println!("sandbox check passed: {}", stdout.trim());
    Ok(())
}
