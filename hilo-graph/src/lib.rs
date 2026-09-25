//! Hilo graph engine — tree-sitter AST parsing and DuckDB graph queries.
//!
//! ## Crate modules
//! - `parser` — tree-sitter Go AST parsing, import extraction
//! - `graph` — DuckDB graph initialization and edge querying
//! - `impact` — transitive impact analysis (who depends on this file)
//! - `duckdb` — DuckDB convenience module and default-path constructors
//! - `error` — error types for graph operations

pub mod classify;
pub mod duckdb;
pub mod edges;
pub mod error;
pub mod graph;
pub mod impact;
pub mod parser;
pub mod provenance;
pub mod resolution;
pub mod rules;
pub mod semantic;
pub mod signal;

pub use classify::{
    classify_file, entry_symbols_for_classification, extract_entry_symbols, infer_feature,
    Classification, ENTRY_SYMBOLS_CAP,
};
pub use error::{GraphError, GraphResult};
pub use graph::{
    ensure_schema, insert_edges_into, reconcile_edges_from_jsonl,
    reconcile_edges_from_jsonl_with_chunk_size, request_path_reconcile_budget_ms,
    set_request_path_reconcile_budget_ms, strip_file_prefix, DegradedReason, Direction,
    GraphAccess, GraphDB, ModuleStats, ReconcileMode, ReconcileReport,
    DEFAULT_REQUEST_PATH_RECONCILE_BUDGET_MS, RECONCILE_CHUNK_ROWS, UNBOUNDED_RECONCILE_BUDGET_MS,
    UNRESOLVABLE_TARGET_HINT,
};
pub use impact::{
    compute_impact, compute_impact_at, compute_impact_with_external, ImpactFile, ImpactResult,
};
pub use parser::{Language, Parser};
pub use provenance::Provenance;
pub use resolution::{AnchoredRoot, PkgResolver};
pub use rules::{Rule, RuleCheckResult, RuleEngine, RuleError};
pub use signal::{
    extract_symbol_names_for_index, understand, understand_with_source, Resolution, SignalFile,
    SignalOpts, SignalResult, SymbolSignature, Tier,
};

pub use semantic::tokenize as semantic_tokenize;
pub use semantic::{
    default_symbol_extractor, reciprocal_rank_fusion, search, search_with_symbols, SearchOpts,
    SearchResult, TfIdfIndex,
};

/// Re-export of the shared [`Edge`] type from `hilo_metadata`.
pub use hilo_metadata::inventory::Edge;

/// Re-export of `serde_json` for downstream crates (e.g., `hilo-cli`) that
/// need JSON serialization without declaring it as a direct dependency.
pub use serde_json;
