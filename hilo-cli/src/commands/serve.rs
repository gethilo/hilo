//! `hilo serve --mcp` — start the MCP server.

use anyhow::Result;

/// Start the MCP server on stdin/stdout when `--mcp` is set.
///
/// Reads `rate_limit_rps` from the manifest's `performance` section.
/// If no manifest is present or `rate_limit_rps` is unset, rate limiting
/// is disabled (0 = unlimited).
pub fn run(mcp: bool) -> Result<()> {
    if mcp {
        let rate_limit_rps = load_rate_limit_rps();
        hilo_mcp::server::run(rate_limit_rps)?;
        Ok(())
    } else {
        anyhow::bail!("No server mode selected. Use --mcp for MCP server.");
    }
}

/// Load `rate_limit_rps` from the manifest, defaulting to 0 (unlimited).
fn load_rate_limit_rps() -> u32 {
    let primary = std::path::Path::new(".vfs/manifest.yaml");
    let fallback = std::path::Path::new("manifest.yaml");

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
