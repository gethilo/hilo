use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::worktree::WorktreeManager;

// ============================================================
// ERROR TYPE
// ============================================================

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Parse error: {0}")]
    Parse(#[from] serde_yaml::Error),
    #[error("Validation failed: {0}")]
    Validation(String),
}

// ============================================================
// VALIDATION ERROR
// ============================================================

#[derive(Debug, Clone, PartialEq)]
pub struct ValidationError {
    pub field: String,
    pub message: String,
}

// ============================================================
// WORKSPACE MANIFEST (top-level)
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct WorkspaceManifest {
    #[serde(default)]
    pub repos: Vec<WorkspaceRepo>,
    #[serde(default)]
    pub backends: Vec<WorkspaceBackend>,
    #[serde(default)]
    pub mounts: Vec<WorkspaceMount>,
}

// ============================================================
// WORKSPACE REPO
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct WorkspaceRepo {
    pub name: String,
    pub url: String,
    #[serde(rename = "ref")]
    pub r#ref: String,
    #[serde(default)]
    pub writable: bool,
    /// Auto-pull interval in seconds. None means no auto-pull.
    #[serde(default)]
    pub auto_pull: Option<u64>,
}

// ============================================================
// WORKSPACE BACKEND
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct WorkspaceBackend {
    pub name: String,
    #[serde(rename = "type")]
    pub r#type: String,
    pub config: serde_yaml::Value,
    pub mount_point: String,
}

// ============================================================
// WORKSPACE MOUNT
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct WorkspaceMount {
    pub source: String,
    pub at: String,
    #[serde(default)]
    pub options: Option<serde_yaml::Value>,
}

// ============================================================
// IMPL
// ============================================================

impl WorkspaceManifest {
    /// Load and parse a workspace manifest YAML file.
    pub fn load(path: &str) -> Result<Self, WorkspaceError> {
        let contents = std::fs::read_to_string(path)?;
        let manifest: Self = serde_yaml::from_str(&contents)?;
        Ok(manifest)
    }

    /// Parse from a YAML string.
    pub fn parse(yaml: &str) -> Result<Self, WorkspaceError> {
        Ok(serde_yaml::from_str(yaml)?)
    }

    /// Validate the manifest and return all errors found.
    pub fn validate(&self) -> Vec<ValidationError> {
        let mut errors = Vec::new();

        // Validate repos
        let mut seen_repo_names = std::collections::HashSet::new();
        for repo in &self.repos {
            if repo.name.is_empty() {
                errors.push(ValidationError {
                    field: "repos[].name".into(),
                    message: "repo name must not be empty".into(),
                });
            }
            if repo.url.is_empty() {
                errors.push(ValidationError {
                    field: format!("repos.{}.url", repo.name),
                    message: "repo url must not be empty".into(),
                });
            }
            if repo.r#ref.is_empty() {
                errors.push(ValidationError {
                    field: format!("repos.{}.ref", repo.name),
                    message: "repo ref must not be empty".into(),
                });
            }
            if !seen_repo_names.insert(&repo.name) {
                errors.push(ValidationError {
                    field: format!("repos.{}", repo.name),
                    message: "duplicate repo name".into(),
                });
            }
        }

        // Validate backends
        let mut seen_backend_names = std::collections::HashSet::new();
        let valid_backend_types = ["s3", "git", "local"];
        for backend in &self.backends {
            if backend.name.is_empty() {
                errors.push(ValidationError {
                    field: "backends[].name".into(),
                    message: "backend name must not be empty".into(),
                });
            }
            if !valid_backend_types.contains(&backend.r#type.as_str()) {
                errors.push(ValidationError {
                    field: format!("backends.{}.type", backend.name),
                    message: format!(
                        "invalid backend type '{}', must be s3/git/local",
                        backend.r#type
                    ),
                });
            }
            if backend.mount_point.is_empty() {
                errors.push(ValidationError {
                    field: format!("backends.{}.mount_point", backend.name),
                    message: "backend mount_point must not be empty".into(),
                });
            }
            if !seen_backend_names.insert(&backend.name) {
                errors.push(ValidationError {
                    field: format!("backends.{}", backend.name),
                    message: "duplicate backend name".into(),
                });
            }
        }

        // Validate mounts
        let mut seen_mount_points = std::collections::HashSet::new();
        let all_sources: std::collections::HashSet<&str> = self
            .repos
            .iter()
            .map(|r| r.name.as_str())
            .chain(self.backends.iter().map(|b| b.name.as_str()))
            .collect();

        for mount in &self.mounts {
            if mount.source.is_empty() {
                errors.push(ValidationError {
                    field: "mounts[].source".into(),
                    message: "mount source must not be empty".into(),
                });
            } else if !all_sources.contains(mount.source.as_str()) {
                errors.push(ValidationError {
                    field: format!("mounts.{}.source", mount.source),
                    message: format!(
                        "mount source '{}' does not reference a declared repo or backend",
                        mount.source
                    ),
                });
            }
            if mount.at.is_empty() {
                errors.push(ValidationError {
                    field: "mounts[].at".into(),
                    message: "mount path must not be empty".into(),
                });
            }
            if !seen_mount_points.insert(&mount.at) {
                errors.push(ValidationError {
                    field: format!("mounts.at={}", mount.at),
                    message: "duplicate mount point".into(),
                });
            }
        }

        errors
    }

    /// Build a mount plan: resolve each mount source to its backing path.
    pub fn build_mount_plan(&self) -> Result<Vec<MountEntry>, WorkspaceError> {
        let mut entries = Vec::new();
        let mgr = WorktreeManager::new().map_err(|e| WorkspaceError::Validation(e.to_string()))?;

        for mount in &self.mounts {
            // Check if source is a repo
            if let Some(repo) = self.repos.iter().find(|r| r.name == mount.source) {
                let path = mgr
                    .ensure(&repo.name, &repo.url, &repo.r#ref)
                    .map_err(|e| {
                        WorkspaceError::Validation(format!(
                            "failed to ensure worktree for {}: {}",
                            repo.name, e
                        ))
                    })?;
                entries.push(MountEntry {
                    name: mount.source.clone(),
                    backing_path: path,
                    at: mount.at.clone(),
                    writable: repo.writable,
                });
            } else if let Some(backend) = self.backends.iter().find(|b| b.name == mount.source) {
                let path = PathBuf::from(&backend.mount_point);
                entries.push(MountEntry {
                    name: mount.source.clone(),
                    backing_path: path,
                    at: mount.at.clone(),
                    writable: false, // backends default to read-only unless configured
                });
            }
        }

        Ok(entries)
    }
}

/// An entry in the workspace mount plan — one mounted source.
#[derive(Debug, Clone)]
pub struct MountEntry {
    pub name: String,
    pub backing_path: PathBuf,
    pub at: String,
    pub writable: bool,
}

// ============================================================
// TESTS
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_str_full_manifest() {
        let yaml = r#"
repos:
  - name: auth-service
    url: git@github.com:org/auth-service.git
    ref: main
    writable: true
    auto_pull: 3600
  - name: shared-lib
    url: git@github.com:org/shared-lib.git
    ref: v2.1.0
backends:
  - name: models
    type: s3
    config:
      bucket: my-models
      region: us-east-1
    mount_point: /models/
  - name: datasets
    type: local
    config:
      path: /data/datasets/
    mount_point: /data/
mounts:
  - source: auth-service
    at: /mnt/vfs/auth-service/
  - source: models
    at: /mnt/vfs/models/
"#;
        let manifest = WorkspaceManifest::parse(yaml).unwrap();

        assert_eq!(manifest.repos.len(), 2);
        assert_eq!(manifest.repos[0].name, "auth-service");
        assert_eq!(manifest.repos[0].url, "git@github.com:org/auth-service.git");
        assert_eq!(manifest.repos[0].r#ref, "main");
        assert!(manifest.repos[0].writable);
        assert_eq!(manifest.repos[0].auto_pull, Some(3600));
        assert_eq!(manifest.repos[1].r#ref, "v2.1.0");
        assert!(!manifest.repos[1].writable);
        assert!(manifest.repos[1].auto_pull.is_none());

        assert_eq!(manifest.backends.len(), 2);
        assert_eq!(manifest.backends[0].r#type, "s3");
        assert_eq!(manifest.backends[1].r#type, "local");

        assert_eq!(manifest.mounts.len(), 2);
        assert_eq!(manifest.mounts[0].source, "auth-service");
        assert_eq!(manifest.mounts[0].at, "/mnt/vfs/auth-service/");
        assert!(manifest.mounts[0].options.is_none());
    }

    #[test]
    fn test_from_str_minimal() {
        let yaml = "repos: []\nbackends: []\nmounts: []\n";
        let manifest = WorkspaceManifest::parse(yaml).unwrap();
        assert!(manifest.repos.is_empty());
        assert!(manifest.backends.is_empty());
        assert!(manifest.mounts.is_empty());
    }

    #[test]
    fn test_from_str_empty() {
        let yaml = "";
        let manifest = WorkspaceManifest::parse(yaml).unwrap();
        assert!(manifest.repos.is_empty());
        assert!(manifest.backends.is_empty());
        assert!(manifest.mounts.is_empty());
    }

    #[test]
    fn test_from_str_invalid_yaml() {
        let yaml = "repos: [broken";
        let err = WorkspaceManifest::parse(yaml).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Parse error"),
            "expected Parse error, got: {msg}"
        );
    }

    #[test]
    fn test_validate_missing_required_fields() {
        // Repo with empty name should be caught by validation
        let yaml = r#"
repos:
  - name: ""
    url: ""
    ref: ""
"#;
        let manifest = WorkspaceManifest::parse(yaml).unwrap();
        let errors = manifest.validate();
        assert!(!errors.is_empty(), "expected validation errors");
        // Empty name, empty url, empty ref = at least 3 errors
        assert!(
            errors.len() >= 3,
            "expected >= 3 errors, got {}",
            errors.len()
        );
    }

    #[test]
    fn test_validate_duplicate_repo() {
        let yaml = r#"
repos:
  - name: dup
    url: git@github.com:a/b.git
    ref: main
  - name: dup
    url: git@github.com:a/c.git
    ref: develop
"#;
        let manifest = WorkspaceManifest::parse(yaml).unwrap();
        let errors = manifest.validate();
        assert!(!errors.is_empty());
        let dup_errors: Vec<_> = errors
            .iter()
            .filter(|e| e.message.contains("duplicate"))
            .collect();
        assert!(!dup_errors.is_empty(), "expected duplicate repo error");
    }

    #[test]
    fn test_validate_invalid_backend_type() {
        let yaml = r#"
backends:
  - name: bad
    type: ftp
    config: {}
    mount_point: /bad/
"#;
        let manifest = WorkspaceManifest::parse(yaml).unwrap();
        let errors = manifest.validate();
        assert!(!errors.is_empty());
        assert!(errors
            .iter()
            .any(|e| e.field.contains("backends") && e.message.contains("ftp")));
    }

    #[test]
    fn test_validate_mount_source_not_found() {
        let yaml = r#"
mounts:
  - source: nonexistent-repo
    at: /mnt/vfs/somewhere/
"#;
        let manifest = WorkspaceManifest::parse(yaml).unwrap();
        let errors = manifest.validate();
        assert!(!errors.is_empty());
        assert!(errors
            .iter()
            .any(|e| e.message.contains("does not reference")));
    }

    #[test]
    fn test_validate_duplicate_mount_point() {
        let yaml = r#"
repos:
  - name: repo-a
    url: git@github.com:a/b.git
    ref: main
mounts:
  - source: repo-a
    at: /same/path/
  - source: repo-a
    at: /same/path/
"#;
        let manifest = WorkspaceManifest::parse(yaml).unwrap();
        let errors = manifest.validate();
        let dup_mounts: Vec<_> = errors
            .iter()
            .filter(|e| e.message.contains("duplicate mount"))
            .collect();
        assert!(
            !dup_mounts.is_empty(),
            "expected duplicate mount point error"
        );
    }

    #[test]
    fn test_validate_mount_source_matches_repo() {
        let yaml = r#"
repos:
  - name: my-repo
    url: git@github.com:org/my-repo.git
    ref: main
mounts:
  - source: my-repo
    at: /mnt/vfs/my-repo/
"#;
        let manifest = WorkspaceManifest::parse(yaml).unwrap();
        let errors = manifest.validate();
        assert!(errors.is_empty(), "expected no errors, got {:?}", errors);
    }

    #[test]
    fn test_validate_mount_source_matches_backend() {
        let yaml = r#"
backends:
  - name: my-bucket
    type: s3
    config:
      bucket: data
    mount_point: /data/
mounts:
  - source: my-bucket
    at: /mnt/vfs/data/
"#;
        let manifest = WorkspaceManifest::parse(yaml).unwrap();
        let errors = manifest.validate();
        assert!(errors.is_empty(), "expected no errors, got {:?}", errors);
    }

    #[test]
    fn test_load_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("manifest.yaml");
        let yaml = r#"
repos:
  - name: test-repo
    url: git@github.com:test/repo.git
    ref: main
    auto_pull: 1800
"#;
        std::fs::write(&path, yaml).unwrap();
        let manifest = WorkspaceManifest::load(path.to_str().unwrap()).unwrap();
        assert_eq!(manifest.repos.len(), 1);
        assert_eq!(manifest.repos[0].name, "test-repo");
        assert_eq!(manifest.repos[0].auto_pull, Some(1800));
    }

    #[test]
    fn test_load_file_not_found() {
        let err = WorkspaceManifest::load("/nonexistent/path/manifest.yaml").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("IO error"), "expected IO error, got: {msg}");
    }

    #[test]
    fn test_deny_unknown_fields() {
        let yaml = r#"
repos:
  - name: r
    url: git@x:y.git
    ref: main
    extra_unknown_field: should_fail
"#;
        let err = WorkspaceManifest::parse(yaml).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unknown field") || msg.contains("extra_unknown_field"),
            "expected unknown field error, got: {msg}"
        );
    }

    #[test]
    fn test_validate_duplicate_backend_name() {
        let yaml = r#"
backends:
  - name: dup-backend
    type: s3
    config: {}
    mount_point: /a/
  - name: dup-backend
    type: local
    config: {}
    mount_point: /b/
"#;
        let manifest = WorkspaceManifest::parse(yaml).unwrap();
        let errors = manifest.validate();
        let dup: Vec<_> = errors
            .iter()
            .filter(|e| e.message.contains("duplicate"))
            .collect();
        assert!(!dup.is_empty(), "expected duplicate backend error");
    }

    #[test]
    fn test_validate_backend_empty_mount_point() {
        let yaml = r#"
backends:
  - name: be
    type: s3
    config: {}
    mount_point: ""
"#;
        let manifest = WorkspaceManifest::parse(yaml).unwrap();
        let errors = manifest.validate();
        assert!(errors
            .iter()
            .any(|e| e.field.contains("mount_point") && e.message.contains("empty")));
    }

    #[test]
    fn test_validate_empty_mount_source() {
        let yaml = r#"
mounts:
  - source: ""
    at: /some/path/
"#;
        let manifest = WorkspaceManifest::parse(yaml).unwrap();
        let errors = manifest.validate();
        assert!(errors.iter().any(|e| e.message.contains("source")));
    }

    #[test]
    fn test_validate_empty_mount_at() {
        let yaml = r#"
repos:
  - name: r
    url: git@x:y.git
    ref: main
mounts:
  - source: r
    at: ""
"#;
        let manifest = WorkspaceManifest::parse(yaml).unwrap();
        let errors = manifest.validate();
        assert!(errors.iter().any(|e| e.message.contains("mount path")));
    }

    #[test]
    fn test_validate_all_clear() {
        let yaml = r#"
repos:
  - name: repo1
    url: git@github.com:a/repo1.git
    ref: main
    writable: true
    auto_pull: 3600
  - name: repo2
    url: git@github.com:a/repo2.git
    ref: develop
backends:
  - name: s3-backend
    type: s3
    config:
      bucket: my-data
      region: us-east-1
    mount_point: /models/
  - name: local-data
    type: local
    config:
      path: /data/
    mount_point: /datasets/
  - name: git-backend
    type: git
    config:
      url: git@github.com:a/vendor.git
    mount_point: /vendor/
mounts:
  - source: repo1
    at: /mnt/vfs/repo1/
  - source: s3-backend
    at: /mnt/vfs/models/
  - source: local-data
    at: /mnt/vfs/datasets/
"#;
        let manifest = WorkspaceManifest::parse(yaml).unwrap();
        assert_eq!(manifest.repos.len(), 2);
        assert_eq!(manifest.backends.len(), 3);
        assert_eq!(manifest.mounts.len(), 3);
        let errors = manifest.validate();
        assert!(
            errors.is_empty(),
            "expected no validation errors, got {:?}",
            errors
        );
    }
}
