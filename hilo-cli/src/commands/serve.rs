//! `hilo serve --mcp` — start the MCP server.

use anyhow::Result;

/// Start the MCP server on stdin/stdout when `--mcp` is set.
pub fn run(mcp: bool) -> Result<()> {
    if mcp {
        hilo_mcp::server::run()?;
        Ok(())
    } else {
        anyhow::bail!("No server mode selected. Use --mcp for MCP server.");
    }
}
