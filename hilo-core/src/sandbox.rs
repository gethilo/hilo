//! Bubblewrap sandboxing — isolate agent processes with Linux namespaces.
//!
//! Uses `bwrap` (bubblewrap) to create unprivileged containers with
//! configurable network/PID/filesystem isolation. The manifest §14.3
//! sandbox block controls the configuration.
//!
//! When `bwrap` is not installed, all operations return
//! `SandboxError::BubblewrapNotFound` — fallback gracefully to
//! unsandboxed execution.
//!
//! DF-WARPFS-151: sandboxed processes get a WHITELISTED environment
//! (`--clearenv` + PATH/HOME/LANG/TERM/TZ/DISPLAY) and a minimal bind
//! set. The full host root is no longer bind-mounted — it carried
//! `~/.ssh`, `/root` and other host secrets straight into the sandbox.
//! Instead the sandbox root is an empty tmpfs plus read-only toolchain
//! binds (`/usr`, `/bin`, `/lib`, `/lib64`, `/sbin`, `/etc`), and the
//! workspace / writable-path binds are sanitized: any path that matches
//! a credential-directory pattern is NOT mounted.

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

// ── Environment + bind isolation (DF-WARPFS-151) ───────────────────

/// Environment variables explicitly forwarded to the sandboxed child
/// (and to the bwrap launcher process) when they are set on the host.
/// Everything else — API keys, tokens, cloud credentials — is dropped.
const MINIMAL_ENV_KEYS: [&str; 6] = ["PATH", "HOME", "LANG", "TERM", "TZ", "DISPLAY"];

/// Host directories bound READ-ONLY into the sandbox so the payload can
/// resolve its interpreter and dynamic libraries. Everything else in the
/// sandbox root is an empty tmpfs: host homes (`/root`, `$HOME` dotfiles,
/// `/etc` aside) are simply absent.
const MINIMAL_ROOT_DIRS: [&str; 6] = ["/usr", "/bin", "/lib", "/lib64", "/sbin", "/etc"];

/// Home-relative dot-directories whose contents are credentials by
/// definition. A workspace or writable path inside one of these (under
/// `/root` or under `$HOME`) is never mounted.
const SENSITIVE_HOME_DIRS: [&str; 8] = [
    ".ssh",
    ".gnupg",
    ".aws",
    ".bunker",
    ".kube",
    ".password-store",
    ".bws",
    ".config",
];

/// Substring markers (case-insensitive) that mark a bind path as
/// credential-bearing, per DF-WARPFS-151: "no bind path may point at a
/// path containing /secret|key|token/ (case-insensitive)". Substring
/// semantics are deliberately fail-closed: a directory named
/// `/data/api-keys` or `/etc/stripe/secret_api` is refused even though
/// the marker spans only part of a component, and a false positive
/// (e.g. a workspace named `~/keyboard-config`) costs nothing — the
/// bind is dropped and the warning names the path.
const SENSITIVE_MARKERS: [&str; 4] = ["secret", "key", "token", "credential"];

/// Collected whitelist environment for the sandbox child and the bwrap
/// launcher. Host vars not on `MINIMAL_ENV_KEYS` never appear here.
fn whitelisted_env() -> impl Iterator<Item = (String, String)> {
    MINIMAL_ENV_KEYS.into_iter().filter_map(|key| {
        std::env::var_os(key).map(|value| (key.to_string(), value.to_string_lossy().to_string()))
    })
}

/// Sanitization verdict for one path that would be bind-mounted into
/// the sandbox. `None` = drop the bind (credential-bearing path);
/// `Some(p)` = mount `p` (always the input path — never a substitute,
/// so nothing credential-shaped ever appears in the bwrap argv).
fn sanitize_bind(path: &str) -> Option<String> {
    let lower = path.to_ascii_lowercase();
    if SENSITIVE_MARKERS.iter().any(|m| lower.contains(m)) {
        return None;
    }
    if is_home_sensitive_path(Path::new(path)) {
        return None;
    }
    Some(path.to_string())
}

/// `true` when `path` sits in a home directory's credential-bearing
/// dot-directory (or is such a home itself). Two homes are checked:
/// the literal `/root` (host-independent) and the runtime `$HOME`
/// (canonicalized, so symlinks like `/home/me` → `/usr/home/me` are
/// matched on the real path).
fn is_home_sensitive_path(path: &Path) -> bool {
    if path.starts_with("/root") {
        let rest = path.strip_prefix("/root").unwrap_or_else(|_| Path::new(""));
        return match rest.components().next() {
            None => true, // bare /root is root's home — sensitive
            Some(first) => SENSITIVE_HOME_DIRS
                .iter()
                .any(|d| first.as_os_str().to_string_lossy().eq_ignore_ascii_case(d)),
        };
    }

    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        if let Ok(home_canonical) = home.canonicalize() {
            // The path itself may not exist yet; fall back to the raw
            // path when canonicalization fails.
            let probe = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
            if let Ok(rest) = probe.strip_prefix(&home_canonical) {
                if let Some(first) = rest.components().next() {
                    let name = first.as_os_str().to_string_lossy();
                    return SENSITIVE_HOME_DIRS
                        .iter()
                        .any(|d| name.eq_ignore_ascii_case(d));
                }
            }
        }
    }
    false
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
    ///
    /// DF-WARPFS-151 layout:
    /// - the sandbox root is an EMPTY tmpfs (not the host `/`), so host
    ///   homes (`/root`, `$HOME` dotfiles) are absent by construction;
    /// - read-only `-try` binds bring in only the toolchain dirs that
    ///   exist on this host;
    /// - the workspace and manifest-declared writable paths are bound
    ///   ONLY when `sanitize_bind` accepts them (credential-shaped paths
    ///   are dropped with a warning, never mounted);
    /// - `--clearenv` + a `--setenv` whitelist replace the inherited
    ///   environment inside the sandbox.
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

        // ── root filesystem: empty tmpfs base ──────────────────
        // DF-WARPFS-151: the old `--ro-bind / /` mounted the FULL host
        // root — ~/.ssh, /root, /etc/*secret* included — into every
        // sandboxed process. A bare namespace has an empty root (no
        // /bin, /usr or /lib), so the base is an empty tmpfs instead
        // and only the toolchain dirs below are laid back in.
        args.push("--tmpfs".into());
        args.push("/".into());

        // ── host toolchain, read-only, existing dirs only ──────
        for dir in MINIMAL_ROOT_DIRS {
            args.push("--ro-bind-try".into());
            args.push((*dir).to_string());
            args.push((*dir).to_string());
        }

        // ── device + proc mounts ───────────────────────────────
        // Fresh /dev and /proc so the sandbox gets a clean procfs
        // (and device nodes) instead of the host's (§14.1 "isolated
        // process space").
        args.push("--dev".into());
        args.push("/dev".into());
        args.push("--proc".into());
        args.push("/proc".into());

        // ── workspace bind-mount (read-write, sanitized) ───────
        // Bind the workspace at its OWN absolute path: bwrap creates
        // missing parent directories inside the tmpfs root, so the
        // self-bind works at any location.
        let workspace_abs = self
            .workspace
            .canonicalize()
            .unwrap_or_else(|_| self.workspace.clone());
        let workspace_path = workspace_abs.to_string_lossy().to_string();
        match sanitize_bind(&workspace_path) {
            Some(p) => {
                args.push("--bind".into());
                args.push(p.clone());
                args.push(p);
            }
            None => eprintln!(
                "warning: sandbox workspace {workspace_path:?} matches a \
                 credential-directory pattern — not binding it; the sandboxed \
                 command will likely fail (rename the directory)"
            ),
        }

        // ── tmpfs for /tmp ─────────────────────────────────────
        // Mounted BEFORE the writable-path binds so a declared writable
        // path under /tmp lands ON TOP of the fresh tmpfs instead of
        // being shadowed by it.
        args.push("--tmpfs".into());
        args.push("/tmp".into());

        // ── writable paths (sanitized) ─────────────────────────
        for wp in &self.config.writable_paths {
            match sanitize_bind(wp) {
                Some(p) => {
                    args.push("--bind".into());
                    args.push(p.clone());
                    args.push(p);
                }
                None => eprintln!(
                    "warning: sandbox writable path {wp:?} matches a \
                     credential-directory pattern — not mounting it"
                ),
            }
        }

        // ── environment isolation (DF-WARPFS-151) ──────────────
        // Drop the inherited environment inside the sandbox, then
        // re-add only the whitelisted variables that are actually set.
        args.push("--clearenv".into());
        for (key, value) in whitelisted_env() {
            args.push("--setenv".into());
            args.push(key);
            args.push(value);
        }

        // ── user's command ─────────────────────────────────────
        args.push("--".into());
        args.extend(command.iter().cloned());

        args
    }

    /// Environment for the bwrap LAUNCHER process (DF-WARPFS-151): the
    /// full host environment is cleared and the same minimal whitelist
    /// re-applied. The sandbox child's environment is governed by the
    /// argv (`--clearenv`/`--setenv`); the launcher must not carry the
    /// host's secrets either, since it execs bwrap which execs the
    /// payload. Whitelisting `PATH` also keeps program resolution
    /// working after `env_clear()`.
    fn apply_launcher_env(cmd: &mut std::process::Command) {
        cmd.env_clear();
        for (key, value) in whitelisted_env() {
            cmd.env(key, value);
        }
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
        let mut cmd = std::process::Command::new(&args[0]);
        cmd.args(&args[1..]);
        Self::apply_launcher_env(&mut cmd);
        let output = cmd.output()?;

        Ok(output)
    }

    /// Execute `command` inside the sandbox with INHERITED stdio, returning
    /// the child's ExitStatus.
    ///
    /// DF-WARPFS-77 rework: production surface for `hilo sandbox exec` — the
    /// spec §14.1 model where the agent's shell runs bwrap-wrapped and its
    /// output streams through to the caller instead of being captured.
    pub fn run_inherit(
        &self,
        command: &[String],
    ) -> Result<std::process::ExitStatus, SandboxError> {
        if !self.config.enabled {
            return Err(SandboxError::NotEnabled);
        }

        if !Self::is_available() {
            return Err(SandboxError::BubblewrapNotFound);
        }

        let args = self.build_args(command);
        let mut cmd = std::process::Command::new(&args[0]);
        cmd.args(&args[1..]);
        Self::apply_launcher_env(&mut cmd);
        let status = cmd.status()?;

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
        // Empty tmpfs root (DF-WARPFS-151), read-only toolchain binds,
        // fresh /dev + /proc + /tmp, cleared environment.
        assert!(args.contains(&"--tmpfs".into()));
        assert!(args.contains(&"--ro-bind-try".into()));
        assert!(args.contains(&"--dev".into()));
        assert!(args.contains(&"--proc".into()));
        assert!(args.contains(&"--clearenv".into()));
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

        // Toolchain dirs are still read-only bound (`--ro-bind-try`),
        // and /dev + /proc + tmpfs are always there.
        assert!(args.contains(&"--ro-bind-try".into()));
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

    // ── DF-WARPFS-151: no full-root bind ────────────────────────

    #[test]
    fn test_build_args_binds_no_full_root() {
        let cfg = BubblewrapConfig::from_manifest(&sandbox_enabled());
        let exec = BubblewrapExecutor::new(cfg, "/tmp/test-workspace");
        let args = exec.build_args(&["true".into()]);

        // No bind (ro or rw) may take the host root "/" — that was the
        // secret-leak vector (it mounted ~/.ssh, /root, /etc wholesale).
        for w in args.windows(3) {
            let is_bind = w[0] == "--bind" || w[0] == "--ro-bind" || w[0] == "--dev-bind";
            assert!(
                !(is_bind && w[1] == "/"),
                "full-root bind present in args: {args:?}"
            );
        }
    }

    // ── DF-WARPFS-151: no credential markers in the argv ────────

    #[test]
    fn test_build_args_no_sensitive_substrings() {
        let mut sandbox = sandbox_enabled();
        sandbox.writable_paths = vec![
            "/tmp/out".into(),
            "/home/agent/keys".into(),
            "/etc/stripe/secret_api".into(),
        ];
        let cfg = BubblewrapConfig::from_manifest(&sandbox);
        let exec = BubblewrapExecutor::new(cfg, "/tmp/test-workspace");
        let args = exec.build_args(&["sh".into(), "-c".into(), "echo hi".into()]);

        // AC: NO 'KEY', 'SECRET', 'TOKEN', 'private' substring in ANY
        // element of the bwrap arg vector.
        for arg in &args {
            let upper = arg.to_ascii_uppercase();
            for marker in ["KEY", "SECRET", "TOKEN", "PRIVATE"] {
                assert!(
                    !upper.contains(marker),
                    "sensitive marker {marker:?} leaked into bwrap arg {arg:?}"
                );
            }
        }

        // The clean writable path survives; the credential-shaped ones
        // are gone entirely.
        assert!(args.iter().any(|a| a == "/tmp/out"));
        assert!(!args.iter().any(|a| a.contains("keys")));
        assert!(!args.iter().any(|a| a.contains("stripe")));
    }

    #[test]
    fn test_build_args_home_ssh_workspace_dropped() {
        let home = std::env::var("HOME").unwrap();
        let ws = format!("{home}/.ssh/leaked-workspace");
        let cfg = BubblewrapConfig::from_manifest(&sandbox_enabled());
        let exec = BubblewrapExecutor::new(cfg, &ws);
        let args = exec.build_args(&["true".into()]);

        assert!(
            !args.iter().any(|a| a.contains(".ssh")),
            "workspace under $HOME/.ssh was bound: {args:?}"
        );
    }

    #[test]
    fn test_build_args_home_config_workspace_dropped() {
        let home = std::env::var("HOME").unwrap();
        // AC names ~/.config/stripe explicitly — config dirs hold API
        // credentials (cloud CLIs, stripe, gh) and are never mounted.
        let ws = format!("{home}/.config/stripe");
        let cfg = BubblewrapConfig::from_manifest(&sandbox_enabled());
        let exec = BubblewrapExecutor::new(cfg, &ws);
        let args = exec.build_args(&["true".into()]);

        assert!(
            !args.iter().any(|a| a.contains(".config")),
            "workspace under $HOME/.config was bound: {args:?}"
        );
    }

    #[test]
    fn test_build_args_root_home_workspace_dropped_but_normal_kept() {
        // /root and its credential dot-dirs are host-independent and are
        // never mounted.
        for p in ["/root", "/root/.ssh", "/root/.aws/config"] {
            let exec =
                BubblewrapExecutor::new(BubblewrapConfig::from_manifest(&sandbox_enabled()), p);
            let args = exec.build_args(&["true".into()]);
            assert!(
                !args.iter().any(|a| a.starts_with("/root")),
                "root-home bind leaked for {p}: {args:?}"
            );
        }
        // A non-credential /root subpath stays bindable.
        let exec = BubblewrapExecutor::new(
            BubblewrapConfig::from_manifest(&sandbox_enabled()),
            "/root/work",
        );
        let args = exec.build_args(&["true".into()]);
        assert!(args.iter().any(|a| a == "/root/work"));
    }

    #[test]
    fn test_writable_path_sensitive_dropped_clean_kept() {
        let mut sandbox = sandbox_enabled();
        sandbox.writable_paths = vec!["/var/tmp/out".into(), "/etc/stripe/secret_api".into()];
        let cfg = BubblewrapConfig::from_manifest(&sandbox);
        let exec = BubblewrapExecutor::new(cfg, "/tmp/ws");
        let args = exec.build_args(&["true".into()]);

        assert!(args.iter().any(|a| a == "/var/tmp/out"));
        assert!(!args.iter().any(|a| a.contains("stripe")));
        assert!(!args.iter().any(|a| a.contains("secret")));
    }

    // ── DF-WARPFS-151: whitelisted environment in the argv ──────

    #[test]
    fn test_build_args_clearenv_whitelist_only() {
        let cfg = BubblewrapConfig::from_manifest(&sandbox_enabled());
        let exec = BubblewrapExecutor::new(cfg, "/tmp/test-workspace");
        let args = exec.build_args(&["true".into()]);

        let clearenv_pos = args.iter().position(|a| a == "--clearenv").unwrap();
        let dashdash_pos = args.iter().position(|a| a == "--").unwrap();
        assert!(clearenv_pos < dashdash_pos);

        // Collect the --setenv KEY VALUE triples after --clearenv.
        let mut keys = Vec::new();
        let mut i = clearenv_pos + 1;
        while i < dashdash_pos {
            if args[i] == "--setenv" {
                assert!(i + 2 < args.len(), "--setenv without KEY VALUE: {args:?}");
                keys.push(args[i + 1].clone());
                i += 3;
            } else {
                i += 1;
            }
        }

        // PATH is set on any test host and must be whitelisted, not lost.
        assert!(
            keys.contains(&"PATH".to_string()),
            "PATH missing from whitelist: {keys:?}"
        );
        // Every forwarded key is on the whitelist, nothing else is.
        for key in &keys {
            assert!(
                MINIMAL_ENV_KEYS.contains(&key.as_str()),
                "non-whitelisted env key forwarded: {key}"
            );
        }
        assert!(keys.len() <= MINIMAL_ENV_KEYS.len());
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

    // ── live environment isolation (bwrap installed) ────────────
    //
    // DF-WARPFS-151 regression: a sandboxed `env` must show NO variable
    // whose name contains KEY|SECRET|TOKEN|PASSWD|CREDENTIALS — the
    // environment is whitelist-only. Host-shaped secret vars are planted
    // in THIS process (the launcher's environment); if any of them
    // reaches the child, the whitelist has regressed.

    #[test]
    fn test_run_env_isolation_child_sees_no_host_secrets() {
        if !BubblewrapExecutor::is_available() {
            eprintln!("skipping: bwrap not installed on this host");
            return;
        }
        let ws = std::env::temp_dir().join(format!("hilo-bwrap-env-{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();

        // Plant credential-shaped vars in the launcher's environment.
        // Names are test-unique so no sibling test reads them.
        std::env::set_var("HILO_TEST_API_KEY", "leak-marker-key-value");
        std::env::set_var("HILO_TEST_SECRET_TOKEN", "leak-marker-token-value");
        std::env::set_var(
            "HILO_TEST_AWS_SECRET_ACCESS_KEY",
            "leak-marker-secret-value",
        );

        let mut sandbox = sandbox_enabled();
        sandbox.writable_paths = vec![];
        let cfg = BubblewrapConfig::from_manifest(&sandbox);
        let exec = BubblewrapExecutor::new(cfg, ws.to_str().unwrap());

        let output = match exec.run(&["sh".into(), "-c".into(), "env".into()]) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("skipping: bwrap could not run ({e})");
                let _ = std::fs::remove_dir_all(&ws);
                return;
            }
        };

        std::env::remove_var("HILO_TEST_API_KEY");
        std::env::remove_var("HILO_TEST_SECRET_TOKEN");
        std::env::remove_var("HILO_TEST_AWS_SECRET_ACCESS_KEY");
        let _ = std::fs::remove_dir_all(&ws);

        assert!(output.status.success(), "bwrap exit: {}", output.status);
        let stdout = String::from_utf8_lossy(&output.stdout);

        // Positive control: the whitelist made it through.
        assert!(
            stdout.lines().any(|l| l.starts_with("PATH=")),
            "whitelisted PATH missing from child env:\n{stdout}"
        );

        // AC: no variable whose name contains KEY|SECRET|TOKEN|PASSWD|
        // CREDENTIALS may appear in the child's environment.
        for line in stdout.lines() {
            let upper = line.to_ascii_uppercase();
            for marker in ["KEY=", "SECRET=", "TOKEN=", "PASSWD=", "CREDENTIALS="] {
                assert!(
                    !upper.contains(marker),
                    "host env var leaked into the sandbox: {line:?}"
                );
            }
        }
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
