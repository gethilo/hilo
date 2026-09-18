//! Hilo MCP server — stdio-based Model Context Protocol server.
//!
//! Implements seventeen vfs_* tools (vfs_get_metadata, vfs_graph_related,
//! vfs_graph_stats, and 14 more) — see tools/mod.rs for the dispatch table.

pub mod error;
pub mod rate_limiter;
pub mod server;
pub mod tools;
