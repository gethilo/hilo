//! Error types for the hilo-graph crate.

use thiserror::Error;

/// Errors produced by graph parsing and DuckDB operations.
#[derive(Error, Debug)]
pub enum GraphError {
    /// A tree-sitter language-loading or parse failure.
    #[error("tree-sitter parse error: {0}")]
    Parse(#[from] tree_sitter::LanguageError),

    /// An underlying DuckDB error (connection, query, type conversion).
    #[error("DuckDB error: {0}")]
    DuckDB(#[from] duckdb::Error),

    /// Filesystem I/O error (e.g. creating the `.vfs/graph` directory).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Any other graph error not covered by the variants above.
    #[error("graph error: {0}")]
    Other(String),
}

/// Convenience `Result` alias for graph operations.
pub type GraphResult<T> = Result<T, GraphError>;
