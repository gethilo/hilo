//! Hilo graph engine — tree-sitter AST parsing and DuckDB graph queries.
//!
//! ## Crate modules
//! - `parser` — tree-sitter Go AST parsing, import extraction
//! - `graph` — DuckDB graph initialization and edge querying
//! - `impact` — transitive impact analysis (who depends on this file)
//! - `duckdb` — DuckDB convenience module and default-path constructors
//! - `error` — error types for graph operations

pub mod classify;
pub mod conformance;
pub mod coverage_links;
pub mod duckdb;
pub mod edges;
pub mod error;
pub mod graph;
pub mod impact;
pub mod parser;
pub mod provenance;
pub mod resolution;
pub mod rollup;
pub mod rules;
pub mod semantic;
pub mod signal;
pub mod surfaces;
pub mod terraform;
pub mod test_classes;
pub mod wiring;
pub mod wiring_report;

pub use classify::{
    classify_file, classify_test_file, classify_test_functions, entry_symbols_for_classification,
    extract_entry_symbols, infer_feature, Classification, TestClass, TestFunctionClass,
    ENTRY_SYMBOLS_CAP,
};
pub use conformance::{
    conformance_supported, drain_go_assertion_sites, extract_conformance, ConformanceSite,
    SiteKind, CONFORMANCE_CONFIDENCE, CONFORMANCE_PROVENANCE, CONSUMES_REL, IMPLEMENTS_REL,
};
pub use error::{GraphError, GraphResult};
pub use graph::{
    cache_matches_edges, ensure_schema, insert_edges_into, reconcile_edges_from_jsonl,
    reconcile_edges_from_jsonl_with_chunk_size, request_path_reconcile_budget_ms,
    set_request_path_reconcile_budget_ms, strip_file_prefix, DegradedReason, Direction,
    GraphAccess, GraphDB, ModuleStats, ReconcileMode, ReconcileReport,
    DEFAULT_REQUEST_PATH_RECONCILE_BUDGET_MS, RECONCILE_CHUNK_ROWS, UNBOUNDED_RECONCILE_BUDGET_MS,
    UNRESOLVABLE_TARGET_HINT,
};
pub use impact::{
    compute_impact, compute_impact_at, compute_impact_with_external, compute_review_set,
    expand_review_set, ImpactFile, ImpactResult, REL_SELF, REVIEW_FORWARD_DEPTH,
    REVIEW_REVERSE_DEPTH, SCOPE_CRATE, SCOPE_DEPENDENCY, SCOPE_FILE, SCOPE_LINK, SCOPE_SELF,
};
pub use parser::{Language, Parser};
pub use provenance::Provenance;
pub use resolution::{AnchoredRoot, PkgResolver};
pub use rules::{Rule, RuleCheckResult, RuleEngine, RuleError};
pub use signal::{
    extract_symbol_names_for_index, understand, understand_with_source, Resolution, SignalFile,
    SignalOpts, SignalResult, SymbolSignature, Tier,
};
pub use wiring::{
    detect_wiring, language_state, Satisfier, SatisfierRole, WiringResult, WiringState,
};

pub use wiring_report::{
    build_report, classify_module, compute_delta, edge_id, module_index, module_of, read_edges,
    snapshot, BaselineInfo, ExcludedDir, InterfaceResult, InterfaceSatisfier, ModuleClass,
    ModuleDelta, ModuleFile, ModuleIndex, ModuleSnapshot, ModuleWiring, SurfaceRef, WiringBaseline,
    WiringInputs, WiringReport, WiringTotals, ROOT_MODULE, WIRING_BASELINE_VERSION,
    WIRING_REPORT_VERSION,
};

pub use semantic::tokenize as semantic_tokenize;
pub use semantic::{
    default_doc_extractor, default_symbol_extractor, extract_doc_tokens, reciprocal_rank_fusion,
    search, search_with_symbols, search_with_symbols_and_docs, DocExtractor, SearchOpts,
    SearchResult, TfIdfIndex, DOC_TOKEN_BUDGET,
};

pub use surfaces::{
    append_surfaces_deduped, surface_id, KindCensus, Surface, SurfaceInventory, SurfaceKind,
    SurfaceScope,
};

pub use rollup::{
    crate_of, resolve_group, ClassCount, GroupBy, GroupRollup, GroupSource, RollupReport,
    SourceCount, SurfaceGrouping, CRATE_MANIFESTS,
};

pub use coverage_links::{
    append_coverage_links_deduped, cause_census, link_id, merge_links, read_links, CoverageLink,
    CoverageLinkReport, EvidenceKind, UnlinkedSurface,
};

/// Re-export of the shared [`Edge`] type from `hilo_metadata`.
pub use hilo_metadata::inventory::Edge;

/// Re-export of `serde_json` for downstream crates (e.g., `hilo-cli`) that
/// need JSON serialization without declaring it as a direct dependency.
pub use serde_json;
