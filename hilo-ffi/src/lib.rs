// Hilo FFI — UniFFI bindings for Go, Python, Kotlin, and Swift.
//
// This crate defines the UniFFI interface definition (hilo.udl) and provides
// stub implementations. The generated language bindings are produced by
// `uniffi-bindgen` at build time and are NOT committed to this repository.
//
// Generated targets:
//   Go:      hilo-go/vfs/
//   Python:  hilo/ wheel
//   Kotlin:  hilo-kotlin/
//   Swift:   Hilo/

// Clippy: generated scaffolding may have empty lines after doc comments
#![allow(clippy::empty_line_after_doc_comments)]

uniffi::include_scaffolding!("hilo");

// --- Error type (matches UDL [Error] enum) ---

#[derive(Debug, thiserror::Error)]
pub enum HiloError {
    #[error("invalid input")]
    InvalidInput,
    #[error("not found")]
    NotFound,
    #[error("backend unavailable")]
    BackendUnavailable,
    #[error("internal error")]
    InternalError,
}

// --- Return types (fields match UDL dictionary definitions) ---
// NOTE: Do NOT derive uniffi::Record — the UDL scaffolding generates those impls.

#[derive(Debug, Clone)]
pub struct MetadataResult {
    pub value: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SetMetadataResult {
    pub path: Option<String>,
    pub key: Option<String>,
    pub value: Option<String>,
    pub previous_value: Option<String>,
    pub success: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    pub rel: String,
}

#[derive(Debug, Clone)]
pub struct GraphRelatedResult {
    pub edges: Vec<GraphEdge>,
    pub total: u32,
}

#[derive(Debug, Clone)]
pub struct GraphImpactEntry {
    pub path: String,
    pub relation: String,
    pub depth: u32,
}

#[derive(Debug, Clone)]
pub struct GraphImpactResult {
    pub dependents: Vec<GraphImpactEntry>,
    pub total: u32,
}

#[derive(Debug, Clone)]
pub struct GraphStats {
    pub total_files: u32,
    pub total_edges: u32,
    pub unique_relations: u32,
    pub tested_pct: f64,
}

#[derive(Debug, Clone)]
pub struct BackendInfo {
    pub path: String,
    pub backend: Option<String>,
    pub remote_url: Option<String>,
    pub cache_path: Option<String>,
    pub last_synced: Option<String>,
    pub cached: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct RuleResultEntry {
    pub path: String,
    pub severity: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RuleCheckResult {
    pub rule_name: String,
    pub matches: Vec<RuleResultEntry>,
    pub total: u32,
}

#[derive(Debug, Clone)]
pub struct DirectoryEntry {
    pub name: String,
    pub entry_type: String,
    pub backend: Option<String>,
    pub size: Option<u64>,
    pub is_virtual: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct DirectoryListing {
    pub entries: Vec<DirectoryEntry>,
    pub total: u32,
}

// --- Stub implementations ---

fn vfs_get_metadata(_path: &str, _key: &str) -> Result<Option<String>, HiloError> {
    Ok(None)
}

fn vfs_set_metadata(path: &str, key: &str, value: &str) -> Result<SetMetadataResult, HiloError> {
    if key.is_empty() {
        return Err(HiloError::InvalidInput);
    }
    Ok(SetMetadataResult {
        path: Some(path.to_string()),
        key: Some(key.to_string()),
        value: Some(value.to_string()),
        previous_value: None,
        success: Some(true),
    })
}

fn vfs_graph_related(_path: &str) -> Result<GraphRelatedResult, HiloError> {
    Ok(GraphRelatedResult {
        edges: vec![],
        total: 0,
    })
}

fn vfs_graph_impact(_path: &str, _max_depth: u32) -> Result<GraphImpactResult, HiloError> {
    Ok(GraphImpactResult {
        dependents: vec![],
        total: 0,
    })
}

fn vfs_graph_stats() -> Result<GraphStats, HiloError> {
    Ok(GraphStats {
        total_files: 0,
        total_edges: 0,
        unique_relations: 0,
        tested_pct: 0.0,
    })
}

fn vfs_resolve_backend(path: &str) -> Result<BackendInfo, HiloError> {
    Ok(BackendInfo {
        path: path.to_string(),
        backend: None,
        remote_url: None,
        cache_path: None,
        last_synced: None,
        cached: None,
    })
}

fn vfs_rule_check(rule_name: &str) -> Result<RuleCheckResult, HiloError> {
    Ok(RuleCheckResult {
        rule_name: rule_name.to_string(),
        matches: vec![],
        total: 0,
    })
}

fn vfs_list_directory(_path: &str) -> Result<DirectoryListing, HiloError> {
    Ok(DirectoryListing {
        entries: vec![],
        total: 0,
    })
}
