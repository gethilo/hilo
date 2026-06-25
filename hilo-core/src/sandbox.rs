//! Bubblewrap sandboxing — isolate agent processes with Linux namespaces.
//!
//! Uses `bwrap` (bubblewrap) to create unprivileged containers with
//! configurable network/PID/filesystem isolation. The manifest §14.3
//! sandbox block controls the configuration.
//!
//! When `bwrap` is not installed, all operations return
//! `SandboxError::BubblewrapNotFound` — fallback gracefully to
//! unsandboxed execution.

use crate::manifest::Sandbox;
use std::path::PathBuf;
use thiserror::Error;

// ── Error ──────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum SandboxError {
    #[error("bubblewrap binary not found (install with: apt install bubblewrap)")]
    BubblewrapNotFound,
    #[error("sandbox is not enabled in manifest")]
    NotEnabled,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

// ── Config ─────────────────────────────────────────────────────────

/// Runtime configuration derived from the manifest sandbox block.
#[derive(Debug, Clone, PartialEq)]
pub struct BubblewrapConfig {
    pub enabled: bool,
    pub isolate_network: bool,
    pub isolate_pid: bool,
    pub read_only_root: bool,
    pub writable_paths: Vec<String>,
}

impl BubblewrapConfig {
    /// Build config from the manifest sandbox block.
    pub fn from_manifest(sandbox: &Sandbox) -> Self {
        Self {
            enabled: sandbox.enabled,
            isolate_network: sandbox.isolate_network,
            isolate_pid: sandbox.isolate_pid,
            read_only_root: sandbox.read_only_root,
            writable_paths: sandbox.writable_paths.clone(),
        }
    }

    /// Return a fully-disabled config (no sandboxing).
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            isolate_network: false,
            isolate_pid: false,
            read_only_root: false,
            writable_paths: Vec::new(),
        }
    }
}

// ── Executor ───────────────────────────────────────────────────────

/// Builds and (optionally) executes `bwrap` invocations.
pub struct BubblewrapExecutor {
    config: BubblewrapConfig,
    workspace: PathBuf,
}

impl BubblewrapExecutor {
    pub fn new(config: BubblewrapConfig, workspace: &str) -> Self {
        Self {
            config,
            workspace: PathBuf::from(workspace),
        }
    }

    /// Check whether `bwrap` is installed on this system.
    pub fn is_available() -> bool {
        std::process::Command::new("bwrap")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
    }

    /// Build the `bwrap` argument vector for a command.
    ///
    /// Returns `(program, args)` where args *includes* program as
    /// argv[0] so the vector can be passed directly to
    /// `std::process::Command::new(&args[0]).args(&args[1..])`.
    pub fn build_args(&self, command: &[String]) -> Vec<String> {
        let mut args: Vec<String> = Vec::new();

        args.push("bwrap".into());

        // ── namespace isolation ────────────────────────────────
        args.push("--unshare-all".into());
        // NOTE: --unshare-all already includes net + pid; the
        // per-flag options are redundant but kept for clarity in
        // the arg vector (bwrap silently deduplicates).

        if self.config.isolate_network {
            args.push("--unshare-net".into());
        }
        if self.config.isolate_pid {
            args.push("--unshare-pid".into());
        }

        // ── root filesystem ────────────────────────────────────
        if self.config.read_only_root {
            args.push("--ro-bind".into());
            args.push("/".into());
            args.push("/".into());
        }

        // ── workspace bind-mount ───────────────────────────────
        let workspace_abs = self
            .workspace
            .canonicalize()
            .unwrap_or_else(|_| self.workspace.clone());

        args.push("--bind".into());
        args.push(workspace_abs.to_string_lossy().to_string());
        args.push("/workspace".into());

        // ── writable paths ─────────────────────────────────────
        for wp in &self.config.writable_paths {
            args.push("--bind".into());
            args.push(wp.clone());
            args.push(wp.clone());
        }

        // ── tmpfs for /tmp ─────────────────────────────────────
        args.push("--tmpfs".into());
        args.push("/tmp".into());

        // ── user's command ─────────────────────────────────────
        args.push("--".into());
        args.extend(command.iter().cloned());

        args
    }

    /// Execute `command` inside the bubblewrap sandbox.
    ///
    /// Returns `Err(SandboxError::NotEnabled)` when the config has
    /// `enabled=false`, and `Err(SandboxError::BubblewrapNotFound)`
    /// when `bwrap` is not installed.
    pub fn run(&self, command: &[String]) -> Result<std::process::Output, SandboxError> {
        if !self.config.enabled {
            return Err(SandboxError::NotEnabled);
        }

        if !Self::is_available() {
            return Err(SandboxError::BubblewrapNotFound);
        }

        let args = self.build_args(command);
        let output = std::process::Command::new(&args[0])
            .args(&args[1..])
            .output()?;

        Ok(output)
    }
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Sandbox as ManifestSandbox;

    // ── helpers ────────────────────────────────────────────────

    fn sandbox_enabled() -> ManifestSandbox {
        ManifestSandbox {
            enabled: true,
            engine: Some("bubblewrap".into()),
            isolate_network: true,
            isolate_pid: true,
            read_only_root: true,
            writable_paths: vec!["/tmp".into(), "/var/run".into()],
        }
    }

    fn sandbox_disabled() -> ManifestSandbox {
        ManifestSandbox {
            enabled: false,
            ..Default::default()
        }
    }

    // ── config parsing from YAML ───────────────────────────────

    #[test]
    fn test_from_manifest_enabled() {
        let cfg = BubblewrapConfig::from_manifest(&sandbox_enabled());
        assert!(cfg.enabled);
        assert!(cfg.isolate_network);
        assert!(cfg.isolate_pid);
        assert!(cfg.read_only_root);
        assert_eq!(cfg.writable_paths, vec!["/tmp", "/var/run"]);
    }

    #[test]
    fn test_from_manifest_disabled() {
        let cfg = BubblewrapConfig::from_manifest(&sandbox_disabled());
        assert!(!cfg.enabled);
        assert!(!cfg.isolate_network);
        assert!(!cfg.isolate_pid);
        assert!(!cfg.read_only_root);
        assert!(cfg.writable_paths.is_empty());
    }

    #[test]
    fn test_disabled_constructor() {
        let cfg = BubblewrapConfig::disabled();
        assert!(!cfg.enabled);
        assert!(!cfg.isolate_network);
        assert!(cfg.writable_paths.is_empty());
    }

    // ── bwrap arg construction ─────────────────────────────────

    #[test]
    fn test_build_args_fully_isolated() {
        let cfg = BubblewrapConfig::from_manifest(&sandbox_enabled());
        let exec = BubblewrapExecutor::new(cfg, "/tmp/test-workspace");
        let args = exec.build_args(&["echo".into(), "hello".into()]);

        assert_eq!(args[0], "bwrap");
        assert!(args.contains(&"--unshare-all".into()));
        assert!(args.contains(&"--unshare-net".into()));
        assert!(args.contains(&"--unshare-pid".into()));
        assert!(args.contains(&"--ro-bind".into()));
        assert!(args.contains(&"--tmpfs".into()));
        assert!(args.contains(&"--".into()));

        // User command should appear after "--"
        let dashdash_pos = args.iter().position(|a| a == "--").unwrap();
        assert_eq!(args[dashdash_pos + 1], "echo");
        assert_eq!(args[dashdash_pos + 2], "hello");
    }

    #[test]
    fn test_build_args_minimal_isolation() {
        let mut sandbox = sandbox_enabled();
        sandbox.isolate_network = false;
        sandbox.isolate_pid = false;
        sandbox.read_only_root = false;
        sandbox.writable_paths = vec![];

        let cfg = BubblewrapConfig::from_manifest(&sandbox);
        let exec = BubblewrapExecutor::new(cfg, "/tmp/ws");
        let args = exec.build_args(&["ls".into()]);

        // Still has --unshare-all, workspace bind, tmpfs
        assert!(args.contains(&"--unshare-all".into()));
        // These should NOT be present
        assert!(!args.contains(&"--unshare-net".into()));
        assert!(!args.contains(&"--unshare-pid".into()));
        assert!(!args.contains(&"--ro-bind".into()));
    }

    #[test]
    fn test_build_args_multiple_writable_paths() {
        let mut sandbox = sandbox_enabled();
        sandbox.writable_paths = vec!["/a".into(), "/b".into(), "/c".into()];

        let cfg = BubblewrapConfig::from_manifest(&sandbox);
        let exec = BubblewrapExecutor::new(cfg, "/tmp/ws");
        let args = exec.build_args(&["true".into()]);

        // Each writable path generates a --bind pair
        let bind_count = args.iter().filter(|a| *a == "--bind").count();
        assert_eq!(bind_count, 4); // workspace + 3 writable paths
    }

    // ── stub-mode error (bwrap not installed) ──────────────────

    #[test]
    fn test_run_disabled_returns_not_enabled() {
        let cfg = BubblewrapConfig::disabled();
        let exec = BubblewrapExecutor::new(cfg, "/tmp/ws");
        let result = exec.run(&["echo".into(), "hi".into()]);
        assert!(result.is_err());
        match result.unwrap_err() {
            SandboxError::NotEnabled => {} // expected
            other => panic!("expected NotEnabled, got {other:?}"),
        }
    }

    #[test]
    fn test_bubblewrap_not_found_error_display() {
        let err = SandboxError::BubblewrapNotFound;
        let msg = err.to_string();
        assert!(msg.contains("bubblewrap"));
        assert!(msg.contains("apt install"));
    }

    #[test]
    fn test_not_enabled_error_display() {
        let err = SandboxError::NotEnabled;
        assert!(err.to_string().contains("not enabled"));
    }
}
