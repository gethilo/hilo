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
    pub provenance: Option<String>,
    pub confidence: Option<f64>,
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
    pub provenance: Option<String>,
    pub confidence: Option<f64>,
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

// --- Real implementations ---

fn vfs_get_metadata(path: &str, key: &str) -> Result<Option<String>, HiloError> {
    let p = std::path::Path::new(path);
    hilo_metadata::get_vfs_xattr(p, key).map_err(|_| HiloError::InternalError)
}

fn vfs_set_metadata(path: &str, key: &str, value: &str) -> Result<SetMetadataResult, HiloError> {
    if key.is_empty() {
        return Err(HiloError::InvalidInput);
    }
    let p = std::path::Path::new(path);
    let previous = hilo_metadata::get_vfs_xattr(p, key).map_err(|_| HiloError::InternalError)?;
    hilo_metadata::set_vfs_xattr(p, key, value).map_err(|_| HiloError::InternalError)?;
    Ok(SetMetadataResult {
        path: Some(path.to_string()),
        key: Some(key.to_string()),
        value: Some(value.to_string()),
        previous_value: previous,
        success: Some(true),
    })
}

fn vfs_graph_related(path: &str) -> Result<GraphRelatedResult, HiloError> {
    let db_path = ".vfs/graph/graph.db";
    if !std::path::Path::new(db_path).exists() {
        return Ok(GraphRelatedResult {
            edges: vec![],
            total: 0,
        });
    }
    let db = hilo_graph::GraphDB::open(db_path).map_err(|_| HiloError::InternalError)?;
    let edges = db
        .related(path, None, hilo_graph::Direction::Forward)
        .map_err(|_| HiloError::InternalError)?;
    let total = edges.len() as u32;
    let ffi_edges: Vec<GraphEdge> = edges
        .into_iter()
        .map(|e| GraphEdge {
            from: e.from,
            to: e.to,
            rel: e.rel,
            provenance: Some(e.provenance),
            confidence: Some(e.confidence),
        })
        .collect();
    Ok(GraphRelatedResult {
        edges: ffi_edges,
        total,
    })
}

fn vfs_graph_impact(path: &str, max_depth: u32) -> Result<GraphImpactResult, HiloError> {
    let db_path = ".vfs/graph/graph.db";
    if !std::path::Path::new(db_path).exists() {
        return Ok(GraphImpactResult {
            dependents: vec![],
            total: 0,
        });
    }
    let db = hilo_graph::GraphDB::open(db_path).map_err(|_| HiloError::InternalError)?;
    let results = db
        .impact_or_parse(path, max_depth)
        .map_err(|_| HiloError::InternalError)?;
    let total = results.len() as u32;
    let dependents: Vec<GraphImpactEntry> = results
        .into_iter()
        .map(|f| GraphImpactEntry {
            path: f.path,
            relation: f.relation,
            depth: f.depth,
            provenance: f.provenance,
            confidence: f.confidence,
        })
        .collect();
    Ok(GraphImpactResult { dependents, total })
}

fn vfs_graph_stats() -> Result<GraphStats, HiloError> {
    let db_path = ".vfs/graph/graph.db";
    if !std::path::Path::new(db_path).exists() {
        return Ok(GraphStats {
            total_files: 0,
            total_edges: 0,
            unique_relations: 0,
            tested_pct: 0.0,
        });
    }
    let db = hilo_graph::GraphDB::open(db_path).map_err(|_| HiloError::InternalError)?;
    let stats = db.stats().map_err(|_| HiloError::InternalError)?;
    let untested = db.untested_files().map_err(|_| HiloError::InternalError)?;
    let tested_pct = if stats.total_files > 0 {
        let tested = stats.total_files - untested.len() as i64;
        (tested as f64 / stats.total_files as f64) * 100.0
    } else {
        0.0
    };
    Ok(GraphStats {
        total_files: stats.total_files as u32,
        total_edges: stats.total_edges as u32,
        unique_relations: stats.edge_types.len() as u32,
        tested_pct,
    })
}

fn vfs_resolve_backend(path: &str) -> Result<BackendInfo, HiloError> {
    let p = std::path::Path::new(path);
    let exists = p.exists();
    Ok(BackendInfo {
        path: path.to_string(),
        backend: Some("local".to_string()),
        remote_url: None,
        cache_path: None,
        last_synced: if exists {
            Some("synced".to_string())
        } else {
            None
        },
        cached: Some(exists),
    })
}

fn vfs_rule_check(rule_name: &str) -> Result<RuleCheckResult, HiloError> {
    // Load manifest from primary or fallback path.
    let manifest_path = if std::path::Path::new("manifest.yaml").exists() {
        "manifest.yaml"
    } else if std::path::Path::new(".vfs/manifest.yaml").exists() {
        ".vfs/manifest.yaml"
    } else {
        return Ok(RuleCheckResult {
            rule_name: rule_name.to_string(),
            matches: vec![],
            total: 0,
        });
    };

    let manifest = hilo_core::manifest::Manifest::from_file(manifest_path)
        .map_err(|_| HiloError::InternalError)?;

    let query_rule = match manifest.rules.iter().find(|r| r.name == rule_name) {
        Some(r) => r,
        None => {
            return Ok(RuleCheckResult {
                rule_name: rule_name.to_string(),
                matches: vec![],
                total: 0,
            });
        }
    };

    let rule = hilo_graph::Rule {
        name: query_rule.name.clone(),
        description: query_rule.description.clone(),
        query: query_rule.query.clone(),
    };

    let db_path = ".vfs/graph/graph.db";
    if !std::path::Path::new(db_path).exists() {
        return Ok(RuleCheckResult {
            rule_name: rule_name.to_string(),
            matches: vec![],
            total: 0,
        });
    }

    let db = hilo_graph::GraphDB::open(db_path).map_err(|_| HiloError::InternalError)?;

    match hilo_graph::RuleEngine::check(db.conn(), &rule) {
        Ok(result) => {
            let matches: Vec<RuleResultEntry> = result
                .matches
                .into_iter()
                .map(|row| RuleResultEntry {
                    path: row.first().cloned().unwrap_or_default(),
                    severity: row.get(1).cloned(),
                    detail: row.get(2).cloned(),
                })
                .collect();
            let total = matches.len() as u32;
            Ok(RuleCheckResult {
                rule_name: rule_name.to_string(),
                matches,
                total,
            })
        }
        Err(_) => Ok(RuleCheckResult {
            rule_name: rule_name.to_string(),
            matches: vec![],
            total: 0,
        }),
    }
}

fn vfs_list_directory(path: &str) -> Result<DirectoryListing, HiloError> {
    let p = std::path::Path::new(path);
    let dir_path = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|_| HiloError::InternalError)?
            .join(p)
    };

    let mut entries = Vec::new();
    if let Ok(read_dir) = std::fs::read_dir(&dir_path) {
        for entry in read_dir.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let size = entry.metadata().ok().map(|m| m.len());
            entries.push(DirectoryEntry {
                name,
                entry_type: if is_dir {
                    "directory".to_string()
                } else {
                    "file".to_string()
                },
                backend: Some("local".to_string()),
                size,
                is_virtual: Some(false),
            });
        }
    }
    let total = entries.len() as u32;
    Ok(DirectoryListing { entries, total })
}
