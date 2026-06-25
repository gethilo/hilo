//! Error types for the hilo-mcp crate.

use hilo_graph::GraphError;
use hilo_metadata::MetadataError;

/// Errors that can arise during MCP server operation.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("metadata error: {0}")]
    Metadata(#[from] MetadataError),

    #[error("graph error: {0}")]
    Graph(#[from] GraphError),

    #[error("MCP protocol error: {0}")]
    Protocol(String),
}

/// Convenience `Result` alias for MCP operations.
pub type McpResult<T> = Result<T, McpError>;
