//! Hilo MCP server — stdio-based Model Context Protocol server.
//!
//! Implements three tools: vfs_get_metadata, vfs_graph_related, vfs_graph_stats.

pub mod error;
pub mod rate_limiter;
pub mod server;
pub mod tools;
