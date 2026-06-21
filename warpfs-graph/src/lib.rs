//! WarpFS graph engine — tree-sitter AST parsing and DuckDB graph queries.
//!
//! ## Crate modules
//! - `parser` — tree-sitter Go AST parsing, import extraction
//! - `graph` — DuckDB graph initialization and edge querying
//! - `duckdb` — DuckDB convenience module and default-path constructors
//! - `error` — error types for graph operations

pub mod duckdb;
pub mod error;
pub mod graph;
pub mod parser;

pub use error::{GraphError, GraphResult};
pub use graph::GraphDB;
pub use parser::{Language, Parser};

/// Re-export of the shared [`Edge`] type from `warpfs_metadata`.
pub use warpfs_metadata::inventory::Edge;
