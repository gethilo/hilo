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
    #[error("invalid input: {message}")]
    InvalidInput { message: String },
    #[error("not found: {path}")]
    NotFound { path: String },
    #[error("backend unavailable: {message}")]
    BackendUnavailable { message: String },
    #[error("internal error: {message}")]
    InternalError { message: String },
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
    hilo_metadata::get_vfs_xattr(p, key).map_err(|e| HiloError::InternalError {
        message: format!("failed to read metadata for {}: {e}", p.display()),
    })
}

fn vfs_set_metadata(path: &str, key: &str, value: &str) -> Result<SetMetadataResult, HiloError> {
    if key.is_empty() {
        return Err(HiloError::InvalidInput {
            message: "metadata key must not be empty".into(),
        });
    }
    let p = std::path::Path::new(path);
    let previous = hilo_metadata::get_vfs_xattr(p, key).map_err(|e| HiloError::InternalError {
        message: format!("failed to read metadata for {}: {e}", p.display()),
    })?;
    hilo_metadata::set_vfs_xattr(p, key, value).map_err(|e| HiloError::InternalError {
        message: format!("failed to set metadata for {}: {e}", p.display()),
    })?;
    Ok(SetMetadataResult {
        path: Some(path.to_string()),
        key: Some(key.to_string()),
        value: Some(value.to_string()),
        previous_value: previous,
        success: Some(true),
    })
}

pub struct HiloHandle {
    root: std::path::PathBuf,
}

impl HiloHandle {
    fn new(root: String) -> Result<Self, HiloError> {
        if root.trim().is_empty() {
            return Err(HiloError::InvalidInput {
                message: "repository root must not be empty".into(),
            });
        }
        let requested = std::path::PathBuf::from(&root);
        if !requested.is_absolute() {
            return Err(HiloError::InvalidInput {
                message: format!("repository root must be absolute: {root}"),
            });
        }
        let root = requested
            .canonicalize()
            .map_err(|_| HiloError::NotFound { path: root.clone() })?;
        if !root.is_dir() {
            return Err(HiloError::InvalidInput {
                message: format!("repository root is not a directory: {}", root.display()),
            });
        }
        Ok(Self { root })
    }

    fn graph_db(&self) -> Result<hilo_graph::GraphDB, HiloError> {
        let path = self.root.join(".vfs/graph/graph.db");
        if !path.is_file() {
            return Err(HiloError::NotFound {
                path: path.to_string_lossy().into_owned(),
            });
        }
        hilo_graph::GraphDB::open(path.to_string_lossy().as_ref()).map_err(|e| {
            HiloError::InternalError {
                message: format!("failed to open graph {}: {e}", path.display()),
            }
        })
    }

    fn graph_subject(&self, path: &str) -> Result<String, HiloError> {
        if path.starts_with("pkg:") || path.starts_with("sys:") {
            return Ok(path.to_string());
        }
        let requested = std::path::Path::new(path);
        let relative = if requested.is_absolute() {
            requested
                .strip_prefix(&self.root)
                .map_err(|_| HiloError::InvalidInput {
                    message: format!(
                        "path {} is outside repository root {}",
                        requested.display(),
                        self.root.display()
                    ),
                })?
                .to_path_buf()
        } else {
            requested.to_path_buf()
        };
        if relative.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::RootDir
            )
        }) {
            return Err(HiloError::InvalidInput {
                message: format!("path escapes repository root: {path}"),
            });
        }
        Ok(relative.to_string_lossy().replace('\\', "/"))
    }

    fn manifest_path(&self) -> Result<std::path::PathBuf, HiloError> {
        for relative in [
            ".vfs/manifest.yaml",
            "manifest.yaml",
            ".vfs/manifest.yml",
            "manifest.yml",
        ] {
            let candidate = self.root.join(relative);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
        Err(HiloError::NotFound {
            path: self
                .root
                .join(".vfs/manifest.yaml")
                .to_string_lossy()
                .into_owned(),
        })
    }

    fn vfs_graph_related(&self, path: &str) -> Result<GraphRelatedResult, HiloError> {
        let db = self.graph_db()?;
        let subject = self.graph_subject(path)?;
        let known = db
            .file_in_graph(&subject)
            .map_err(|e| HiloError::InternalError {
                message: format!("failed to resolve graph subject {subject}: {e}"),
            })?;
        if !known {
            return Err(HiloError::NotFound {
                path: self.root.join(&subject).to_string_lossy().into_owned(),
            });
        }
        let edges = db
            .related(&subject, None, hilo_graph::Direction::Forward)
            .map_err(|e| HiloError::InternalError {
                message: format!("related query failed for {subject}: {e}"),
            })?;
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

    fn vfs_graph_impact(&self, path: &str, max_depth: u32) -> Result<GraphImpactResult, HiloError> {
        let db = self.graph_db()?;
        let subject = self.graph_subject(path)?;
        let known = db
            .file_in_graph(&subject)
            .map_err(|e| HiloError::InternalError {
                message: format!("failed to resolve graph subject {subject}: {e}"),
            })?;
        if !known {
            return Err(HiloError::NotFound {
                path: self.root.join(&subject).to_string_lossy().into_owned(),
            });
        }
        let results = hilo_graph::compute_impact(db.conn(), &subject, max_depth).map_err(|e| {
            HiloError::InternalError {
                message: format!("impact query failed for {subject}: {e}"),
            }
        })?;
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

    fn vfs_graph_stats(&self) -> Result<GraphStats, HiloError> {
        let db = self.graph_db()?;
        let stats = db.stats().map_err(|e| HiloError::InternalError {
            message: format!("graph stats query failed: {e}"),
        })?;
        let untested = db
            .untested_files_at(&self.root)
            .map_err(|e| HiloError::InternalError {
                message: format!("graph coverage query failed: {e}"),
            })?;
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

    fn vfs_resolve_backend(&self, path: &str) -> Result<BackendInfo, HiloError> {
        let p = self.root.join(path.trim_start_matches('/'));
        let exists = p.exists();
        let mounts_path = self.root.join(".vfs/backends/mounts.yaml");
        let mount = if mounts_path.is_file() {
            let entries = hilo_backends::read_mount_entries(&mounts_path).map_err(|error| {
                HiloError::InternalError {
                    message: error.to_string(),
                }
            })?;
            hilo_backends::mount_for_path(&entries, path).cloned()
        } else {
            None
        };

        if let Some(mount) = mount {
            let remote_url = if mount.kind == "s3" {
                mount.bucket.as_ref().map(|bucket| {
                    format!(
                        "s3://{bucket}/{}",
                        mount
                            .prefix
                            .as_deref()
                            .unwrap_or("")
                            .trim_start_matches('/')
                    )
                })
            } else {
                mount.remote.clone()
            };
            Ok(BackendInfo {
                path: p.to_string_lossy().into_owned(),
                backend: Some(mount.kind),
                remote_url,
                cache_path: exists.then(|| p.to_string_lossy().into_owned()),
                last_synced: Some("unknown (no sync timestamp recorded)".into()),
                cached: Some(exists),
            })
        } else {
            Ok(BackendInfo {
                path: p.to_string_lossy().into_owned(),
                backend: Some("local-only".into()),
                remote_url: None,
                cache_path: None,
                last_synced: Some(if exists {
                    "not applicable (unmanaged local path)".into()
                } else {
                    "not found on disk (unmanaged path)".into()
                }),
                cached: Some(exists),
            })
        }
    }

    fn vfs_rule_check(&self, rule_name: &str) -> Result<RuleCheckResult, HiloError> {
        let manifest_path = self.manifest_path()?;
        let manifest =
            hilo_core::manifest::Manifest::from_file(manifest_path.to_string_lossy().as_ref())
                .map_err(|e| HiloError::InternalError {
                    message: format!("failed to load manifest {}: {e}", manifest_path.display()),
                })?;

        let query_rule = manifest
            .rules
            .iter()
            .find(|rule| rule.name == rule_name)
            .ok_or_else(|| HiloError::NotFound {
                path: format!("{}#rule:{rule_name}", manifest_path.display()),
            })?;

        let rule = hilo_graph::Rule {
            name: query_rule.name.clone(),
            description: query_rule.description.clone(),
            query: query_rule.query.clone(),
        };

        let db = self.graph_db()?;

        let result = hilo_graph::RuleEngine::check(db.conn(), &rule).map_err(|e| {
            HiloError::InternalError {
                message: format!("rule {rule_name} failed: {e:?}"),
            }
        })?;
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

    fn vfs_list_directory(&self, path: &str) -> Result<DirectoryListing, HiloError> {
        let requested = std::path::Path::new(path);
        let dir_path = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            self.root.join(requested)
        };
        if !dir_path.is_dir() {
            return Err(HiloError::NotFound {
                path: dir_path.to_string_lossy().into_owned(),
            });
        }

        let read_dir = std::fs::read_dir(&dir_path).map_err(|e| HiloError::InternalError {
            message: format!("failed to list {}: {e}", dir_path.display()),
        })?;
        let mut entries = Vec::new();
        for entry in read_dir {
            let entry = entry.map_err(|e| HiloError::InternalError {
                message: format!("failed to read entry under {}: {e}", dir_path.display()),
            })?;
            let name = entry.file_name().to_string_lossy().to_string();
            let file_type = entry.file_type().map_err(|e| HiloError::InternalError {
                message: format!("failed to inspect {}: {e}", entry.path().display()),
            })?;
            let size = entry.metadata().ok().map(|metadata| metadata.len());
            entries.push(DirectoryEntry {
                name,
                entry_type: if file_type.is_dir() {
                    "directory".to_string()
                } else {
                    "file".to_string()
                },
                backend: None,
                size,
                is_virtual: Some(false),
            });
        }
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        let total = entries.len() as u32;
        Ok(DirectoryListing { entries, total })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cwd_test_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    struct RestoreCwd(std::path::PathBuf);

    impl Drop for RestoreCwd {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.0).ok();
        }
    }

    #[test]
    fn graph_calls_use_the_named_repo_from_a_foreign_cwd() {
        let _lock = cwd_test_lock().lock().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let graph_dir = repo.path().join(".vfs/graph");
        std::fs::create_dir_all(&graph_dir).unwrap();
        let db_path = graph_dir.join("graph.db");
        let db = hilo_graph::GraphDB::open(db_path.to_str().unwrap()).unwrap();
        db.insert_edges(&[hilo_graph::Edge {
            from: "src/main.rs".into(),
            to: "src/lib.rs".into(),
            rel: "imports".into(),
            provenance: "test".into(),
            confidence: 1.0,
        }])
        .unwrap();
        drop(db);

        let previous = std::env::current_dir().unwrap();
        let _restore = RestoreCwd(previous);
        let foreign = tempfile::tempdir().unwrap();
        std::env::set_current_dir(foreign.path()).unwrap();

        let handle = HiloHandle::new(repo.path().to_string_lossy().into_owned()).unwrap();
        let stats = handle
            .vfs_graph_stats()
            .expect("explicit repo graph should resolve");
        assert_eq!(stats.total_edges, 1);
        let related = handle
            .vfs_graph_related("src/main.rs")
            .expect("related should use the named repo root");
        assert_eq!(related.total, 1);
        let impact = handle
            .vfs_graph_impact("src/lib.rs", 4)
            .expect("impact should use the same named repo root");
        assert_eq!(impact.total, 1);
    }

    #[test]
    fn backend_resolution_follows_mount_configuration_changes() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".vfs/backends")).unwrap();
        std::fs::create_dir_all(repo.path().join("s3data/src")).unwrap();
        std::fs::write(repo.path().join("s3data/src/main.rs"), "fn main() {}\n").unwrap();
        let mounts = repo.path().join(".vfs/backends/mounts.yaml");
        std::fs::write(
            &mounts,
            "- name: dog\n  type: s3\n  bucket: hilo-s3-dogfood\n  prefix: dog/\n  at: /s3data\n  tool: native\n  mode: mirror\n",
        )
        .unwrap();
        let handle = HiloHandle::new(repo.path().to_string_lossy().into_owned()).unwrap();

        let s3 = handle.vfs_resolve_backend("s3data/src/main.rs").unwrap();
        assert_eq!(s3.backend.as_deref(), Some("s3"));
        assert_eq!(s3.remote_url.as_deref(), Some("s3://hilo-s3-dogfood/dog/"));
        assert_eq!(
            s3.last_synced.as_deref(),
            Some("unknown (no sync timestamp recorded)")
        );

        std::fs::write(
            &mounts,
            "- name: checkout\n  type: local\n  prefix: s3data\n  at: /s3data\n  tool: native\n  mode: mirror\n",
        )
        .unwrap();
        let local = handle.vfs_resolve_backend("s3data/src/main.rs").unwrap();
        assert_eq!(local.backend.as_deref(), Some("local"));
        assert!(local.remote_url.is_none());
    }

    #[test]
    fn missing_graph_is_an_error_that_names_the_resolved_path() {
        let _lock = cwd_test_lock().lock().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let expected = repo.path().join(".vfs/graph/graph.db");

        let handle = HiloHandle::new(repo.path().to_string_lossy().into_owned()).unwrap();
        let error = handle
            .vfs_graph_stats()
            .expect_err("missing graph must not look empty");
        assert!(
            error.to_string().contains(&expected.to_string_lossy()[..]),
            "error must name {}: {error}",
            expected.display()
        );
    }
}
