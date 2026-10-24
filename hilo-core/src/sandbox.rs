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
use std::path::{Path, PathBuf};
use thiserror::Error;

// ── Error ──────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum SandboxError {
    #[error("bubblewrap binary not found (install with: apt install bubblewrap)")]
    BubblewrapNotFound,
    #[error("sandbox is not enabled in manifest")]
    NotEnabled,
    /// REV-WARPFS-11: a manifest `sandbox.writable_paths` entry such as
    /// `.` or `data/logs` was bound verbatim into the bwrap argv, and
    /// bubblewrap resolved it against the wrong root — the user saw a
    /// misleading chdir failure instead of "your writable path is
    /// relative". Reject it by name before any bwrap args are built.
    #[error("invalid sandbox writable_paths entry {path:?}: every writable_paths entry must be an absolute path (e.g. \"/data/logs\", not \"data/logs\")")]
    RelativeWritablePath { path: String },
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

    /// REV-WARPFS-11: reject a relative `sandbox.writable_paths` entry.
    ///
    /// bwrap resolves a `--bind` source against the mount namespace's
    /// root, not the project directory, so a manifest value like `.` or
    /// `data/logs` produced a confusing chdir/mount failure that never
    /// mentioned the offending field. Catch it here — before any bwrap
    /// argv is built — with an error that names `writable_paths`.
    ///
    /// A DISABLED config is never validated: an inactive block must keep
    /// behaving exactly as before (the mount path and `run()` own the
    /// `NotEnabled` result), so relative entries in a disabled manifest
    /// stay harmless until someone enables the sandbox.
    pub fn validate(&self) -> Result<(), SandboxError> {
        if !self.enabled {
            return Ok(());
        }
        for wp in &self.writable_paths {
            if !Path::new(wp).is_absolute() {
                return Err(SandboxError::RelativeWritablePath { path: wp.clone() });
            }
        }
        Ok(())
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
        // bwrap always unshares the mount namespace; the PID and
        // network namespaces are opt-in per the manifest (§14.3), so
        // they are added only when the corresponding flag is set.
        if self.config.isolate_pid {
            args.push("--unshare-pid".into());
        }
        if self.config.isolate_network {
            args.push("--unshare-net".into());
        }

        // ── root filesystem ────────────────────────────────────
        // Bind the FULL host root (read-only or read-write) so the
        // sandbox can resolve the command and its dynamic libraries.
        // A bare namespace has an empty root — no /bin, /usr or /lib —
        // so `execvp` fails with "No such file or directory" without
        // this. §14.1: root is READ-ONLY by default.
        if self.config.read_only_root {
            args.push("--ro-bind".into());
        } else {
            args.push("--bind".into());
        }
        args.push("/".into());
        args.push("/".into());

        // ── device + proc mounts ───────────────────────────────
        // Fresh /dev and /proc so the sandbox gets a clean procfs
        // (and device nodes) instead of the host's (§14.1 "isolated
        // process space"). These mount over the directories the root
        // bind already provides, so the mount points exist.
        args.push("--dev".into());
        args.push("/dev".into());
        args.push("--proc".into());
        args.push("/proc".into());

        // ── tmpfs for /tmp (BEFORE the rw binds — DF-WARPFS-150) ──
        // Mount order matters: bwrap applies mounts in argument order,
        // and a later mount shadows an earlier one. The tmpfs must come
        // FIRST so the workspace / writable-path binds below win when
        // they live under /tmp; pushed after (the pre-150 order), a
        // workspace under /tmp was silently covered by the fresh tmpfs —
        // writes to it vanished instead of landing in the workspace.
        args.push("--tmpfs".into());
        args.push("/tmp".into());

        // ── workspace bind-mount (read-write) ─────────────────
        // Bind the workspace at its OWN absolute path, not a fixed
        // `/workspace`: under a read-only root there is no
        // `/workspace` mount point, and bwrap cannot create one
        // inside the read-only bind ("Can't mkdir /workspace").
        // Self-binding works because the path already exists in the
        // root bind and bwrap mounts over it read-write.
        let workspace_abs = self
            .workspace
            .canonicalize()
            .unwrap_or_else(|_| self.workspace.clone());
        let workspace_path = workspace_abs.to_string_lossy().to_string();
        args.push("--bind".into());
        args.push(workspace_path.clone());
        args.push(workspace_path.clone());

        // ── writable paths ─────────────────────────────────────
        for wp in &self.config.writable_paths {
            args.push("--bind".into());
            args.push(wp.clone());
            args.push(wp.clone());
        }

        // ── start in the workspace (DF-WARPFS-150) ─────────────
        // §14.1 "the agent's shell (cd /workspace)": the child starts in
        // the workspace, not the caller's host cwd. Without this, an exec
        // from a foreign directory with a read-only root fails on its
        // first write ("Read-only file system") with no hint that the
        // workspace is elsewhere. `--chdir` makes the path explicit and
        // covers the captured (`run`) and inherited-stdio (`run_inherit`)
        // paths alike; the workspace is already bind-mounted rw above.
        args.push("--chdir".into());
        args.push(workspace_path.clone());

        // ── user's command ─────────────────────────────────────
        args.push("--".into());
        args.extend(command.iter().cloned());

        args
    }

    /// Execute `command` inside the bubblewrap sandbox.
    ///
    /// Returns `Err(SandboxError::NotEnabled)` when the config has
    /// `enabled=false`, and `Err(SandboxError::BubblewrapNotFound)`
    /// when `bwrap` is not installed. A relative `writable_paths`
    /// entry is rejected with `SandboxError::RelativeWritablePath`
    /// before any bwrap args are built (REV-WARPFS-11).
    pub fn run(&self, command: &[String]) -> Result<std::process::Output, SandboxError> {
        if !self.config.enabled {
            return Err(SandboxError::NotEnabled);
        }

        // REV-WARPFS-11: fail with the field name, not bwrap's chdir error.
        self.config.validate()?;

        if !Self::is_available() {
            return Err(SandboxError::BubblewrapNotFound);
        }

        let args = self.build_args(command);
        let output = std::process::Command::new(&args[0])
            .args(&args[1..])
            .output()?;

        Ok(output)
    }

    /// Execute `command` inside the sandbox with INHERITED stdio, returning
    /// the child's ExitStatus.
    ///
    /// DF-WARPFS-77 rework: production surface for `hilo sandbox exec` — the
    /// spec §14.1 model where the agent's shell runs bwrap-wrapped and its
    /// output streams through to the caller instead of being captured.
    ///
    /// Like [`Self::run`], rejects a relative `writable_paths` entry with
    /// `SandboxError::RelativeWritablePath` before building bwrap args
    /// (REV-WARPFS-11).
    pub fn run_inherit(
        &self,
        command: &[String],
    ) -> Result<std::process::ExitStatus, SandboxError> {
        if !self.config.enabled {
            return Err(SandboxError::NotEnabled);
        }

        // REV-WARPFS-11: fail with the field name, not bwrap's chdir error.
        self.config.validate()?;

        if !Self::is_available() {
            return Err(SandboxError::BubblewrapNotFound);
        }

        let args = self.build_args(command);
        let status = std::process::Command::new(&args[0])
            .args(&args[1..])
            .status()?;

        Ok(status)
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
        // Namespace isolation flags are honored individually (§14.3).
        assert!(args.contains(&"--unshare-net".into()));
        assert!(args.contains(&"--unshare-pid".into()));
        // Root is bound read-only, plus fresh /dev + /proc + /tmp.
        assert!(args.contains(&"--ro-bind".into()));
        assert!(args.contains(&"--dev".into()));
        assert!(args.contains(&"--proc".into()));
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

        // Root is bound read-WRITE (plain --bind, not --ro-bind) when
        // read_only_root is false; /dev + /proc + tmpfs are still there.
        assert!(args.contains(&"--bind".into()));
        assert!(args.contains(&"--dev".into()));
        assert!(args.contains(&"--proc".into()));
        assert!(args.contains(&"--tmpfs".into()));
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

    // ── REV-WARPFS-11: writable_paths must be absolute ──────────
    //
    // A relative entry (`.`, `data/logs`) used to reach bwrap verbatim
    // and surface as a misleading chdir/mount failure that never named
    // the field. The executor must reject it with an error that names
    // `writable_paths` BEFORE building any bwrap argv.

    #[test]
    fn test_validate_rejects_relative_writable_path() {
        let mut sandbox = sandbox_enabled();
        sandbox.writable_paths = vec!["/tmp".into(), "data/logs".into()];
        let cfg = BubblewrapConfig::from_manifest(&sandbox);

        let err = cfg
            .validate()
            .expect_err("relative writable_paths entry must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("writable_paths"),
            "error must name writable_paths: {msg}"
        );
        assert!(
            msg.contains("absolute"),
            "error must demand absolute: {msg}"
        );
        assert!(
            msg.contains("data/logs"),
            "error must echo the offending value: {msg}"
        );
        assert!(
            matches!(err, SandboxError::RelativeWritablePath { .. }),
            "expected RelativeWritablePath, got {err:?}"
        );
    }

    #[test]
    fn test_validate_rejects_dot_writable_path() {
        let mut sandbox = sandbox_enabled();
        sandbox.writable_paths = vec![".".into()];
        let cfg = BubblewrapConfig::from_manifest(&sandbox);
        let err = cfg.validate().expect_err("`\".\"` is not an absolute path");
        assert!(err.to_string().contains("writable_paths"));
    }

    #[test]
    fn test_validate_accepts_absolute_writable_paths() {
        // Positive arm: valid absolute entries keep passing, so the
        // existing bind behavior is preserved.
        let cfg = BubblewrapConfig::from_manifest(&sandbox_enabled());
        cfg.validate()
            .expect("absolute writable_paths must stay valid");
    }

    #[test]
    fn test_validate_disabled_config_with_relative_path_is_ok() {
        // Preserve disabled-sandbox behavior: an inactive block with a
        // relative entry must not gain a new failure mode at validation;
        // run() still answers NotEnabled as before.
        let mut sandbox = sandbox_disabled();
        sandbox.writable_paths = vec!["data/logs".into()];
        let cfg = BubblewrapConfig::from_manifest(&sandbox);
        cfg.validate().expect("disabled config is never validated");
        let exec = BubblewrapExecutor::new(cfg, "/tmp/ws");
        let result = exec.run(&["echo".into(), "hi".into()]);
        assert!(
            matches!(result, Err(SandboxError::NotEnabled)),
            "disabled sandbox must keep returning NotEnabled"
        );
    }

    #[test]
    fn test_run_rejects_relative_writable_path_before_bwrap() {
        // The live-defect regression: with an ENABLED sandbox and a
        // relative entry, run() must fail with the named validation
        // error. Pre-fix this built bwrap args anyway and returned an
        // Ok(...) whose bwrap chdir/mount failure never mentioned
        // writable_paths (or BubblewrapNotFound on hosts without bwrap).
        let mut sandbox = sandbox_enabled();
        sandbox.writable_paths = vec!["data/logs".into()];
        let cfg = BubblewrapConfig::from_manifest(&sandbox);
        let exec = BubblewrapExecutor::new(cfg, "/tmp/ws");

        let err = exec
            .run(&["echo".into(), "hi".into()])
            .expect_err("relative writable_paths must be rejected");
        match err {
            SandboxError::RelativeWritablePath { path } => {
                assert_eq!(path, "data/logs");
            }
            other => panic!("expected RelativeWritablePath, got {other:?}"),
        }
    }

    #[test]
    fn test_run_inherit_rejects_relative_writable_path() {
        // `hilo sandbox exec` uses run_inherit — same guard on the
        // inherited-stdio surface (REV-WARPFS-11).
        let mut sandbox = sandbox_enabled();
        sandbox.writable_paths = vec![".".into()];
        let cfg = BubblewrapConfig::from_manifest(&sandbox);
        let exec = BubblewrapExecutor::new(cfg, "/tmp/ws");

        let err = exec
            .run_inherit(&["echo".into(), "hi".into()])
            .expect_err("relative writable_paths must be rejected");
        assert!(err.to_string().contains("writable_paths"));
    }

    // ── DF-WARPFS-150: chdir + mount order ─────────────────────

    #[test]
    fn test_build_args_chdir_workspace() {
        let cfg = BubblewrapConfig::from_manifest(&sandbox_enabled());
        let exec = BubblewrapExecutor::new(cfg, "/tmp/test-workspace");
        let args = exec.build_args(&["echo".into(), "hi".into()]);

        // --chdir <workspace> is present right before the user command.
        let chdir_pos = args
            .iter()
            .position(|a| a == "--chdir")
            .expect("--chdir missing");
        assert_eq!(args[chdir_pos + 1], "/tmp/test-workspace");
        assert_eq!(args[chdir_pos + 2], "--");
    }

    #[test]
    fn test_build_args_tmpfs_before_workspace_bind() {
        // DF-WARPFS-150: with read_only_root the tmpfs on /tmp must be
        // mounted BEFORE the workspace / writable-path binds, because
        // bwrap applies mounts in argument order and a later mount
        // shadows an earlier one — a workspace under /tmp would
        // otherwise be covered by the fresh tmpfs.
        let cfg = BubblewrapConfig::from_manifest(&sandbox_enabled());
        let exec = BubblewrapExecutor::new(cfg, "/tmp/ws");
        let args = exec.build_args(&["true".into()]);

        let tmpfs_pos = args.iter().position(|a| a == "--tmpfs").unwrap();
        let workspace_bind_pos = args
            .windows(3)
            .position(|w| w[0] == "--bind" && w[1] == "/tmp/ws")
            .unwrap();
        assert!(
            tmpfs_pos < workspace_bind_pos,
            "tmpfs ({tmpfs_pos}) must precede the workspace bind ({workspace_bind_pos})"
        );
    }

    // ── live execution (bwrap installed) ────────────────────────
    //
    // DF-WARPFS-77 regression: the old build_args produced a bwrap
    // invocation that could never run (empty root → `execvp sh: No such
    // file or directory`, or read-only root → `Can't mkdir /workspace`).
    // Prove `run()` actually executes a command end-to-end when bwrap is
    // present; skip when it is not installed or cannot spawn.

    #[test]
    fn test_run_executes_sandboxed_command_when_bwrap_available() {
        if !BubblewrapExecutor::is_available() {
            eprintln!("skipping: bwrap not installed on this host");
            return;
        }
        let ws = std::env::temp_dir().join(format!("hilo-bwrap-test-{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();

        let mut sandbox = sandbox_enabled();
        sandbox.writable_paths = vec![];
        let cfg = BubblewrapConfig::from_manifest(&sandbox);
        let exec = BubblewrapExecutor::new(cfg, ws.to_str().unwrap());

        let output = match exec.run(&["sh".into(), "-c".into(), "echo ok".into()]) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("skipping: bwrap could not run ({e})");
                let _ = std::fs::remove_dir_all(&ws);
                return;
            }
        };

        assert!(output.status.success(), "bwrap exit: {}", output.status);
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "ok");
        let _ = std::fs::remove_dir_all(&ws);
    }

    // ── live network isolation (bwrap installed) ────────────────
    //
    // DF-WARPFS-77 AC1: a process run through the executor with
    // `isolate_network: true` must NOT reach the network (--unshare-net).
    // curl inside the namespace fails DNS/connect; rc=0 would mean the
    // network was reachable (regression). Skips when bwrap or curl is
    // absent (same guard as the live-execution test).

    #[test]
    fn test_run_blocks_network_when_isolate_network_enabled() {
        if !BubblewrapExecutor::is_available() {
            eprintln!("skipping: bwrap not installed on this host");
            return;
        }
        let curl_present = std::process::Command::new("sh")
            .args(["-c", "command -v curl"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !curl_present {
            eprintln!("skipping: curl not available on host");
            return;
        }
        let ws = std::env::temp_dir().join(format!("hilo-bwrap-net-{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();

        let mut sandbox = sandbox_enabled(); // isolate_network: true
        sandbox.writable_paths = vec![];
        let cfg = BubblewrapConfig::from_manifest(&sandbox);
        let exec = BubblewrapExecutor::new(cfg, ws.to_str().unwrap());

        let output = match exec.run(&[
            "sh".into(),
            "-c".into(),
            "curl -s --max-time 3 -o /dev/null https://example.com; echo rc=$?".into(),
        ]) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("skipping: bwrap could not run ({e})");
                let _ = std::fs::remove_dir_all(&ws);
                return;
            }
        };

        assert!(output.status.success(), "bwrap exit: {}", output.status);
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        assert_ne!(
            stdout, "rc=0",
            "network was reachable inside the sandbox — isolate_network is broken"
        );
        eprintln!("network blocked inside sandbox (curl {stdout})");
        let _ = std::fs::remove_dir_all(&ws);
    }

    // ── workspace-relative cwd (bwrap installed) ────────────────
    //
    // DF-WARPFS-150 regression: the child must START in the workspace
    // (§14.1 "the agent's shell (cd /workspace)"), not in the host cwd —
    // under `read_only_root: true` the pre-fix behavior made the first
    // write fail with "Read-only file system". Prove `pwd` prints the
    // workspace path AND a write to `.` lands as a file inside it (with
    // read_only_root: true the only writable spot is the rw workspace
    // bind, so a successful write to `.` already implies the workspace
    // cwd). Skips when bwrap is not installed or cannot spawn (same
    // guard as the live-execution test).

    #[test]
    fn test_run_starts_in_workspace_directory() {
        if !BubblewrapExecutor::is_available() {
            eprintln!("skipping: bwrap not installed on this host");
            return;
        }
        let ws = std::env::temp_dir().join(format!("hilo-bwrap-cwd-{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();

        let mut sandbox = sandbox_enabled();
        sandbox.writable_paths = vec![];
        let cfg = BubblewrapConfig::from_manifest(&sandbox);
        let exec = BubblewrapExecutor::new(cfg, ws.to_str().unwrap());

        let output = match exec.run(&[
            "sh".into(),
            "-c".into(),
            "pwd && echo cwd-marker > ./df150marker.out".into(),
        ]) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("skipping: bwrap could not run ({e})");
                let _ = std::fs::remove_dir_all(&ws);
                return;
            }
        };

        assert!(output.status.success(), "bwrap exit: {}", output.status);
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let expected = ws.to_string_lossy().to_string();
        assert!(
            stdout.split('\n').any(|l| l == expected),
            "child cwd is not the workspace: pwd printed {stdout:?}, expected {expected:?}"
        );
        // A write to `.` inside the sandbox must land in the workspace dir.
        assert!(
            ws.join("df150marker.out").exists(),
            "marker file missing from the workspace — the child's `.` is not the workspace"
        );
        assert_eq!(
            std::fs::read_to_string(ws.join("df150marker.out"))
                .unwrap()
                .trim(),
            "cwd-marker",
        );
        let _ = std::fs::remove_dir_all(&ws);
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
