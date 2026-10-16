//! `hilo serve` — start the MCP server over stdio (`--mcp`) and/or HTTP
//! (`--http`).
//!
//! Both flags may be combined in one process; with neither, the server exits
//! with a usage error. Logging is JSON-formatted (daemon mode) via tracing,
//! routed to stderr: in `--mcp` mode stdout must carry ONLY JSON-RPC protocol
//! bytes for MCP stdio framing.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::commands::guard;

/// Default HTTP port when the manifest is absent or unparsable.
///
/// Matches the manifest default (`hilo-core`'s `default_mcp_port`, 8766), so
/// the fallback and a manifest that omits `interfaces.mcp.port` agree.
pub const DEFAULT_HTTP_PORT: u16 = 8766;

/// Start the servers selected by the CLI flags.
pub fn run(http: bool, mcp: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to determine the current directory")?;
    run_in(&cwd, http, mcp)
}

/// Injectable core of [`run`]: the project root is a parameter so the
/// project precondition is testable without touching the process working
/// directory.
///
/// GAP-086: the server resolves its graph and inventory relative to the
/// current directory, so starting it outside a Hilo project used to bring up
/// a server over a zeroed graph — every tool then answered from nothing
/// instead of telling the operator what was wrong. Refuse up front, naming
/// `hilo init`. An initialized project with no edges yet stays valid: only
/// the manifest's presence is required, never graph contents.
pub fn run_in(root: &Path, http: bool, mcp: bool) -> Result<()> {
    if !http && !mcp {
        anyhow::bail!("No server mode selected. Use --mcp for MCP server, --http for HTTP.");
    }

    guard::ensure_project_root(root)?;

    hilo_core::logging::init_logging_to(true, std::io::stderr);

    if http {
        let port = resolve_http_port(root);
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        std::thread::spawn(move || {
            if let Err(e) = hilo_mcp::http::run_with_ready(port, Some(ready_tx)) {
                tracing::error!("HTTP transport stopped: {e:#}");
            }
        });
        // Wait for the listener to bind so a port conflict is a startup
        // error on the main thread (non-zero exit), not a race with the
        // first health probe. The bind error itself comes back through the
        // channel so the operator sees the cause ("Address already in
        // use"), not just the fact of a failure.
        match ready_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => {}
            Ok(Err(reason)) => {
                anyhow::bail!("HTTP transport failed to bind port {port}: {reason}");
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                anyhow::bail!("HTTP transport failed to bind port {port} within 10s");
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                anyhow::bail!("HTTP transport failed to bind port {port} (server thread exited)");
            }
        }
    }

    if mcp {
        let rate_limit_rps = load_rate_limit_rps(root);
        hilo_mcp::server::run(rate_limit_rps)?;
    } else {
        // `--http` only: this process is now a server and there is no CLI
        // work left. `park` may return spuriously by contract, so loop.
        loop {
            std::thread::park();
        }
    }
    Ok(())
}

/// Resolve the HTTP port: the manifest's `interfaces.mcp.port` when a
/// manifest is present and parsable, [`DEFAULT_HTTP_PORT`] otherwise.
pub fn resolve_http_port(root: &Path) -> u16 {
    load_manifest(root)
        .map(|m| m.interfaces.mcp.port)
        .unwrap_or(DEFAULT_HTTP_PORT)
}

/// Load `rate_limit_rps` from the manifest, defaulting to 0 (unlimited).
///
/// Returns 0 when the manifest is absent or unparsable; [`run_in`] has
/// already refused a root without any manifest.
pub fn load_rate_limit_rps(root: &Path) -> u32 {
    load_manifest(root)
        .map(|m| m.performance.rate_limit_rps)
        .unwrap_or(0)
}

/// Read the project manifest, if one is present and parsable.
///
/// Port and rate-limit resolution share this lookup so `.vfs/manifest.yaml`
/// beats a root-level `manifest.yaml` in exactly one place.
fn load_manifest(root: &Path) -> Option<hilo_core::manifest::Manifest> {
    let path = manifest_file(root)?;
    let path_str = path.to_str().unwrap_or(".vfs/manifest.yaml");
    hilo_core::manifest::Manifest::from_file(path_str).ok()
}

/// Locate the project manifest: `.vfs/manifest.yaml`, falling back to a
/// root-level `manifest.yaml`.
fn manifest_file(root: &Path) -> Option<PathBuf> {
    let primary = root.join(".vfs").join("manifest.yaml");
    let fallback = root.join("manifest.yaml");
    if primary.exists() {
        Some(primary)
    } else if fallback.exists() {
        Some(fallback)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn init_project(dir: &Path) {
        std::fs::create_dir_all(dir.join(".vfs")).unwrap();
        std::fs::write(dir.join(".vfs/manifest.yaml"), "version: 2\n").unwrap();
    }

    #[test]
    fn refuse_missing_manifest_names_init() {
        let dir = TempDir::new().unwrap();
        let err = run_in(dir.path(), false, true).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("hilo init"), "error must name the fix: {msg}");
        assert!(
            msg.contains(&dir.path().display().to_string()),
            "error must name the directory it refused: {msg}"
        );
    }

    #[test]
    fn refuse_no_mode_selected_names_both_flags() {
        // With clap accepting either flag, this branch is only reachable
        // from library callers; it must stay a loud usage error.
        let dir = TempDir::new().unwrap();
        init_project(dir.path());
        let err = run_in(dir.path(), false, false).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("--mcp") && msg.contains("--http"),
            "usage error must name both flags: {msg}"
        );
    }

    #[test]
    fn initialized_empty_project_passes_the_precondition() {
        let dir = TempDir::new().unwrap();
        init_project(dir.path());
        // No graph, no edges: still a project, so the precondition holds.
        guard::ensure_project_root(dir.path()).unwrap();
    }

    #[test]
    fn rate_limit_reads_the_project_manifest() {
        let dir = TempDir::new().unwrap();
        init_project(dir.path());
        std::fs::write(
            dir.path().join(".vfs/manifest.yaml"),
            "version: 2\nproject:\n  name: rl\nperformance:\n  rate_limit_rps: 17\n",
        )
        .unwrap();
        assert_eq!(load_rate_limit_rps(dir.path()), 17);
    }

    #[test]
    fn rate_limit_defaults_to_unlimited_without_a_manifest() {
        let dir = TempDir::new().unwrap();
        assert_eq!(load_rate_limit_rps(dir.path()), 0);
    }

    #[test]
    fn http_port_reads_the_manifest_mcp_section() {
        let dir = TempDir::new().unwrap();
        init_project(dir.path());
        std::fs::write(
            dir.path().join(".vfs/manifest.yaml"),
            "version: 2\nproject:\n  name: p\ninterfaces:\n  mcp:\n    port: 9999\n",
        )
        .unwrap();
        assert_eq!(resolve_http_port(dir.path()), 9999);
    }

    #[test]
    fn http_port_defaults_to_8766_when_manifest_omits_it() {
        let dir = TempDir::new().unwrap();
        init_project(dir.path());
        assert_eq!(resolve_http_port(dir.path()), 8766);
    }

    #[test]
    fn http_port_defaults_to_8766_without_a_manifest() {
        let dir = TempDir::new().unwrap();
        assert_eq!(resolve_http_port(dir.path()), 8766);
    }
}
