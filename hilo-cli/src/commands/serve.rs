//! `hilo serve --mcp` — start the MCP server.

use std::path::Path;

use anyhow::{Context, Result};

use crate::commands::guard;

/// Start the MCP server on stdin/stdout when `--mcp` is set.
///
/// Reads `rate_limit_rps` from the manifest's `performance` section.
/// If no manifest is present or `rate_limit_rps` is unset, rate limiting
/// is disabled (0 = unlimited).
/// Logging is JSON-formatted (daemon mode) via tracing, routed to stderr:
/// stdout must carry ONLY JSON-RPC protocol bytes for MCP stdio framing.
pub fn run(mcp: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to determine the current directory")?;
    run_in(&cwd, mcp)
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
pub fn run_in(root: &Path, mcp: bool) -> Result<()> {
    if !mcp {
        anyhow::bail!("No server mode selected. Use --mcp for MCP server.");
    }

    guard::ensure_project_root(root)?;

    hilo_core::logging::init_logging_to(true, std::io::stderr);
    let rate_limit_rps = load_rate_limit_rps(root);
    hilo_mcp::server::run(rate_limit_rps)?;
    Ok(())
}

/// Load `rate_limit_rps` from the manifest, defaulting to 0 (unlimited).
///
/// Precedence is `.vfs/manifest.yaml` then a root-level `manifest.yaml` —
/// the project manifest `hilo init` writes wins when both exist, which is
/// deliberately stricter than [`guard::manifest_path`]'s presence check.
/// Returns 0 when the manifest is absent or unparsable; [`run_in`] has
/// already refused a root without any manifest.
fn load_rate_limit_rps(root: &Path) -> u32 {
    let primary = root.join(".vfs").join("manifest.yaml");
    let fallback = root.join("manifest.yaml");

    let path = if primary.exists() {
        primary
    } else if fallback.exists() {
        fallback
    } else {
        return 0; // No manifest → no rate limiting
    };

    let path_str = path.to_str().unwrap_or(".vfs/manifest.yaml");
    match hilo_core::manifest::Manifest::from_file(path_str) {
        Ok(manifest) => manifest.performance.rate_limit_rps,
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn refuse_missing_manifest_names_init() {
        let dir = TempDir::new().unwrap();
        let err = run_in(dir.path(), true).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("hilo init"), "error must name the fix: {msg}");
        assert!(
            msg.contains(&dir.path().display().to_string()),
            "error must name the directory it refused: {msg}"
        );
    }

    #[test]
    fn refuse_non_mcp_mode_before_touching_the_project() {
        // `--mcp` is required by clap, so this branch is only reachable from
        // library callers; it must stay a loud usage error.
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".vfs")).unwrap();
        std::fs::write(dir.path().join(".vfs/manifest.yaml"), "version: 2\n").unwrap();
        let err = run_in(dir.path(), false).unwrap_err();
        assert!(format!("{err:#}").contains("--mcp"));
    }

    #[test]
    fn initialized_empty_project_passes_the_precondition() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".vfs")).unwrap();
        std::fs::write(dir.path().join(".vfs/manifest.yaml"), "version: 2\n").unwrap();
        // No graph, no edges: still a project, so the precondition holds.
        guard::ensure_project_root(dir.path()).unwrap();
    }

    #[test]
    fn rate_limit_reads_the_project_manifest() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".vfs")).unwrap();
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
}
