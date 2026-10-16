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
pub fn run(mcp: bool, http: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to determine the current directory")?;
    run_in(&cwd, mcp, http)
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
///
/// REV-WARPFS-5: `--http` (the HTTP transport + `/metrics` endpoint) is
/// reserved for REV-WARPFS-3 and not implemented in this crate yet; ask for
/// it and you get a loud refusal, never a stdio server that silently ignores
/// the flag.
pub fn run_in(root: &Path, mcp: bool, http: bool) -> Result<()> {
    if http {
        anyhow::bail!(
            "--http is not implemented yet (HTTP transport tracks REV-WARPFS-3); \
             only --mcp (stdio) is available"
        );
    }
    if !mcp {
        anyhow::bail!("No server mode selected. Use --mcp for MCP server.");
    }

    guard::ensure_project_root(root)?;

    hilo_core::logging::init_logging_to(true, std::io::stderr);
    let rate_limit_rps = load_rate_limit_rps(root)?;
    hilo_mcp::server::run(rate_limit_rps)?;
    Ok(())
}

/// Load `rate_limit_rps` from the manifest, defaulting to 0 (unlimited).
///
/// Precedence is `.vfs/manifest.yaml` then a root-level `manifest.yaml` —
/// the project manifest `hilo init` writes wins when both exist, which is
/// deliberately stricter than [`guard::manifest_path`]'s presence check.
///
/// REV-WARPFS-5: an UNPARSABLE manifest is now a hard error, not a silent
/// fallback to unlimited. The old `Err(_) => 0` fallback defeated manifest
/// validation entirely — a typo'd or invalid `rate_limit_rps` (e.g. a
/// negative value, which hilo-core rejects at load) would have started an
/// UNLIMITED server instead of telling the operator their configuration was
/// never applied. A manifest that is absent entirely stays unlimited.
fn load_rate_limit_rps(root: &Path) -> Result<u32> {
    let primary = root.join(".vfs").join("manifest.yaml");
    let fallback = root.join("manifest.yaml");

    let path = if primary.exists() {
        primary
    } else if fallback.exists() {
        fallback
    } else {
        return Ok(0); // No manifest → no rate limiting
    };

    let path_str = path.to_str().unwrap_or(".vfs/manifest.yaml");
    let manifest = hilo_core::manifest::Manifest::from_file(path_str)
        .with_context(|| format!("failed to load manifest {path_str}"))?;
    Ok(manifest.performance.rate_limit_rps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn refuse_missing_manifest_names_init() {
        let dir = TempDir::new().unwrap();
        let err = run_in(dir.path(), true, false).unwrap_err();
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
        let err = run_in(dir.path(), false, false).unwrap_err();
        assert!(format!("{err:#}").contains("--mcp"));
    }

    /// REV-WARPFS-5: `--http` (HTTP transport + `/metrics`) belongs to
    /// REV-WARPFS-3 and is not implemented here — asking for it must fail
    /// loudly, before any project check, never silently start stdio.
    #[test]
    fn refuse_http_mode_naming_the_tracker() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".vfs")).unwrap();
        std::fs::write(
            dir.path().join(".vfs/manifest.yaml"),
            "version: 2\nproject:\n  name: rl\n",
        )
        .unwrap();
        let err = run_in(dir.path(), true, true).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("--http"), "error must name the flag: {msg}");
        assert!(
            msg.contains("REV-WARPFS-3"),
            "error must name the tracking row: {msg}"
        );
    }

    /// REV-WARPFS-5: a manifest with a negative `rate_limit_rps` must fail
    /// the STARTUP path with an error naming the field — the silent
    /// `Err(_) => 0` fallback would have started an UNLIMITED server while
    /// the operator believed their limit was applied.
    #[test]
    fn negative_rate_limit_manifest_fails_startup() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".vfs")).unwrap();
        std::fs::write(
            dir.path().join(".vfs/manifest.yaml"),
            "version: 2\nproject:\n  name: rl\nperformance:\n  rate_limit_rps: -1\n",
        )
        .unwrap();
        let err = run_in(dir.path(), true, false).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("rate_limit_rps"),
            "startup error must name the invalid field: {msg}"
        );
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
        assert_eq!(load_rate_limit_rps(dir.path()).unwrap(), 17);
    }

    #[test]
    fn rate_limit_defaults_to_unlimited_without_a_manifest() {
        let dir = TempDir::new().unwrap();
        assert_eq!(load_rate_limit_rps(dir.path()).unwrap(), 0);
    }
}
