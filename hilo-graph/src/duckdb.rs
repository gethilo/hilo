//! DuckDB convenience module.
//!
//! Re-exports [`GraphDB`] and provides helper constructors for the standard
//! Hilo graph database path.

pub use crate::graph::{GraphDB, GraphStats};

/// Create a [`GraphDB`] at the standard path `.vfs/graph/graph.db`.
///
/// The parent directory (`.vfs/graph`) is created if it does not already exist.
pub fn open_default() -> crate::error::GraphResult<GraphDB> {
    std::fs::create_dir_all(".vfs/graph")?;
    GraphDB::open(".vfs/graph/graph.db")
}
