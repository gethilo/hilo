use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::manifest::Manifest as ProjectManifest;
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
    /// DF-WARPFS-50: the document is valid YAML but fits neither accepted
    /// manifest schema. The message names both schemas, the offending field
    /// and what was expected of it.
    #[error("{0}")]
    SchemaMismatch(String),
}

// ============================================================
// SCHEMA DIALECTS (DF-WARPFS-50)
// ============================================================

/// Default auto-pull interval, in seconds, used when the PROJECT schema
/// declares the boolean `auto_pull: true` on a repo.
///
/// The project schema (`hilo init`, `hilo_core::manifest::Repo`) types
/// `auto_pull` as a boolean; this workspace schema types it as an interval in
/// seconds. The conversion has to pick a number — one hour is the documented
/// default (docs/workspace.md).
pub const PROJECT_SCHEMA_AUTO_PULL_SECONDS: u64 = 3600;

/// Top-level keys that exist ONLY in the project schema written by
/// `hilo init` (`hilo_core::manifest::Manifest`).
const PROJECT_ONLY_KEYS: &[&str] = &[
    "version",
    "project",
    "interfaces",
    "metadata",
    "graph",
    "permissions",
    "triggers",
    "rules",
    "plugins",
    "discovery",
    "sandbox",
    "performance",
];

/// Which of the two accepted manifest dialects a document was written in.
///
/// Both are loaded by [`WorkspaceManifest`]: the workspace schema
/// (`repos[]`/`backends[]`/`mounts[]`) and the project schema that `hilo
/// init` writes (`.vfs/manifest.yaml` with `version`/`project`/`interfaces`
/// and `repos[].at` as the mount path).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestDialect {
    /// `repos[]` + `backends[]` + `mounts[]` — mounts are explicit (or
    /// implied by `repos[].at`).
    Workspace,
    /// The manifest `hilo init` writes: `version`/`project`/`interfaces`/
    /// `repos[]`/`backends{}`/... — each repo mounts at its `at` path.
    Project,
}

impl ManifestDialect {
    /// Short schema name used in user-facing output ("workspace"/"project").
    pub fn schema_name(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Project => "project",
        }
    }

    /// One-line description of the schema's shape, for error messages.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Workspace => {
                "workspace schema (top-level keys: repos[], backends[], mounts[], auto_dependency_order)"
            }
            Self::Project => {
                "project schema (the `.vfs/manifest.yaml` written by `hilo init`: \
                 version, project, interfaces, repos[] with at/auto_pull, ...)"
            }
        }
    }
}

impl std::fmt::Display for ManifestDialect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.describe())
    }
}

/// A manifest loaded from EITHER accepted schema, plus the details the CLI
/// needs to explain what it got (DF-WARPFS-50).
#[derive(Debug, Clone)]
pub struct LoadedWorkspace {
    /// The normalised workspace view — the same shape for both dialects.
    pub manifest: WorkspaceManifest,
    /// Which schema the source document was written in.
    pub dialect: ManifestDialect,
    /// The project schema's `interfaces.fuse.mount_point`, when declared.
    /// Used as the default mount point when the command line omits one.
    pub fuse_mount_point: Option<String>,
    /// Project-schema `backends:` entries (s3/remote/local). These are
    /// inventory declarations, NOT mount sources (spec §4, §13), so they are
    /// reported but never turned into mounts.
    pub inventory_backends: Vec<String>,
}

impl LoadedWorkspace {
    /// Number of mountable sources the DOCUMENT declared (repos + backends),
    /// before `mounts:` is considered.
    pub fn declared_sources(&self) -> usize {
        self.manifest.repos.len() + self.manifest.backends.len()
    }
}

/// True when the YAML document carries any project-schema-only top-level key.
fn document_is_project_schema(value: &serde_yaml::Value) -> bool {
    match value {
        serde_yaml::Value::Mapping(map) => map
            .keys()
            .filter_map(|key| key.as_str())
            .any(|key| PROJECT_ONLY_KEYS.contains(&key)),
        _ => false,
    }
}

/// Build the "fits neither schema" error: every part names the schema, the
/// field serde rejected and what it expected.
fn schema_mismatch_error(workspace_err: &str, project_err: &str) -> WorkspaceError {
    let hint = schema_hint(workspace_err, project_err);
    WorkspaceError::SchemaMismatch(format!(
        "manifest matches neither accepted schema\n  - {}: {workspace_err}\n  - {}: {project_err}{hint}",
        ManifestDialect::Workspace.describe(),
        ManifestDialect::Project.describe(),
    ))
}

/// Turn the two serde failures into one actionable line, for the field
/// confusions a real user hits (dogfood run 14).
fn schema_hint(workspace_err: &str, project_err: &str) -> String {
    if project_err.contains("unknown field `mounts`") {
        return "\n  hint: `mounts:` belongs to the workspace schema; in the project \
                schema a repo mounts at its own `repos[].at` path (default `/<name>`)."
            .to_string();
    }
    if workspace_err.contains("auto_pull") {
        return "\n  hint: `repos[].auto_pull` is a boolean in the project schema and an \
                interval in seconds in the workspace schema; both are accepted."
            .to_string();
    }
    if workspace_err.contains("backends")
        && (workspace_err.contains("map") || workspace_err.contains("sequence"))
    {
        return "\n  hint: `backends:` is a LIST of {name, type, config, mount_point} in the \
                workspace schema and an inventory MAP (s3/remote/local) in the project schema."
            .to_string();
    }
    if PROJECT_ONLY_KEYS
        .iter()
        .any(|k| workspace_err.contains(&format!("unknown field `{k}`")))
    {
        return "\n  hint: the file starts with project-schema keys (`version`/`project`/…), \
                so the project schema above is the one to fix."
            .to_string();
    }
    String::new()
}

/// Accept `auto_pull` as either an interval in seconds (workspace schema) or
/// a boolean (project schema), producing `Option<u64>`.
///
/// DF-WARPFS-50: the two schemas typed the same key differently, so a spec §4
/// example (`auto_pull: true`) was rejected by the workspace loader with
/// `invalid type: boolean, expected u64`.
fn bool_or_interval<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value: Option<serde_yaml::Value> = Option::deserialize(deserializer)?;
    match value {
        None | Some(serde_yaml::Value::Null) => Ok(None),
        Some(serde_yaml::Value::Bool(true)) => Ok(Some(PROJECT_SCHEMA_AUTO_PULL_SECONDS)),
        Some(serde_yaml::Value::Bool(false)) => Ok(None),
        Some(serde_yaml::Value::Number(n)) => n.as_u64().map(Some).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "repos[].auto_pull must be a non-negative interval in seconds (got {n})"
            ))
        }),
        Some(other) => Err(serde::de::Error::custom(format!(
            "repos[].auto_pull must be an interval in seconds (integer) or a boolean, \
             expected u64 or bool (got {other:?})"
        ))),
    }
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
    /// When true, `mount_plan()` topologically sorts repos so
    /// dependencies mount first. Requires `dependencies` param.
    #[serde(default)]
    pub auto_dependency_order: bool,
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
    ///
    /// Accepts the project schema's boolean as well (`true` →
    /// [`PROJECT_SCHEMA_AUTO_PULL_SECONDS`], `false`/absent → no auto-pull),
    /// so one file shape loads under both schemas (DF-WARPFS-50).
    #[serde(default, deserialize_with = "bool_or_interval")]
    pub auto_pull: Option<u64>,
    /// Mount point inside the workspace tree (project schema §4 spelling).
    ///
    /// When set, the repo is mounted at this path without a separate
    /// `mounts:` entry — the same meaning the key has in the project schema,
    /// so `repos[].at` mounts in BOTH dialects (DF-WARPFS-50).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// Per-repo manifest overrides file (project schema §4). Carried for
    /// schema compatibility; the workspace mount does not read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<String>,
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
    ///
    /// Accepts BOTH schemas (DF-WARPFS-50): the workspace schema and the
    /// project schema `hilo init` writes. Use
    /// [`Self::load_with_dialect`] when the caller needs to explain which one
    /// it got.
    pub fn load(path: &str) -> Result<Self, WorkspaceError> {
        Ok(Self::load_with_dialect(path)?.manifest)
    }

    /// Load a manifest file, reporting which schema it was written in.
    pub fn load_with_dialect(path: &str) -> Result<LoadedWorkspace, WorkspaceError> {
        let contents = std::fs::read_to_string(path)?;
        Self::parse_with_dialect(&contents)
    }

    /// Parse from a YAML string.
    ///
    /// Accepts both schemas; see [`Self::parse_with_dialect`].
    pub fn parse(yaml: &str) -> Result<Self, WorkspaceError> {
        Ok(Self::parse_with_dialect(yaml)?.manifest)
    }

    /// Parse a YAML string from EITHER accepted schema.
    ///
    /// The document's top-level keys decide which schema is tried first; the
    /// other one is tried second, so a file that is valid in either dialect
    /// loads. When both fail the error names both schemas, the rejected field
    /// and what that field expected (DF-WARPFS-50).
    pub fn parse_with_dialect(yaml: &str) -> Result<LoadedWorkspace, WorkspaceError> {
        // YAML syntax errors are schema-independent.
        let value: serde_yaml::Value = serde_yaml::from_str(yaml)?;
        let prefer_project = document_is_project_schema(&value);

        let try_workspace = || -> std::result::Result<LoadedWorkspace, String> {
            serde_yaml::from_str::<WorkspaceManifest>(yaml)
                .map(WorkspaceManifest::into_workspace_dialect)
                .map_err(|e| e.to_string())
        };
        let try_project = || -> std::result::Result<LoadedWorkspace, String> {
            ProjectManifest::parse(yaml)
                .map(|m| Self::from_project_manifest(&m))
                // The project parser wraps serde's text in its own prefixes
                // ("Invalid field: ", "Parse error: "); inside a message that
                // already says which schema failed, the prefix is noise.
                .map_err(|e| {
                    e.to_string()
                        .trim_start_matches("Invalid field: ")
                        .trim_start_matches("Parse error: ")
                        .to_string()
                })
        };

        if prefer_project {
            match try_project() {
                Ok(loaded) => Ok(loaded),
                Err(project_err) => try_workspace()
                    .map_err(|workspace_err| schema_mismatch_error(&workspace_err, &project_err)),
            }
        } else {
            match try_workspace() {
                Ok(loaded) => Ok(loaded),
                Err(workspace_err) => try_project()
                    .map_err(|project_err| schema_mismatch_error(&workspace_err, &project_err)),
            }
        }
    }

    /// Normalise a parsed workspace-schema document: materialise `repos[].at`
    /// into a mount entry (the project schema's spelling, accepted here too).
    fn into_workspace_dialect(mut manifest: Self) -> LoadedWorkspace {
        manifest.absorb_repo_at_mounts();
        LoadedWorkspace {
            manifest,
            dialect: ManifestDialect::Workspace,
            fuse_mount_point: None,
            inventory_backends: Vec::new(),
        }
    }

    /// Convert the project schema (`hilo init`) into the workspace view.
    ///
    /// * every repo mounts at its `repos[].at` path (default `/<name>`),
    ///   which is exactly what `at` means in the project schema;
    /// * the boolean `auto_pull` becomes an interval
    ///   ([`PROJECT_SCHEMA_AUTO_PULL_SECONDS`] when true);
    /// * `backends:` (s3/remote/local) is inventory, never a mount source —
    ///   it is reported, not mounted (spec §4, §13).
    pub fn from_project_manifest(project: &ProjectManifest) -> LoadedWorkspace {
        let mut repos = Vec::with_capacity(project.repos.len());
        for repo in &project.repos {
            let at = if repo.at.trim().is_empty() {
                format!("/{}", repo.name)
            } else {
                repo.at.clone()
            };
            repos.push(WorkspaceRepo {
                name: repo.name.clone(),
                url: repo.url.clone(),
                r#ref: repo.git_ref.clone(),
                writable: repo.writable,
                auto_pull: if repo.auto_pull {
                    Some(PROJECT_SCHEMA_AUTO_PULL_SECONDS)
                } else {
                    None
                },
                at: Some(at),
                manifest: repo.manifest.clone(),
            });
        }

        let mut inventory_backends = Vec::new();
        for backend in &project.backends.s3 {
            inventory_backends.push(format!("s3:{}", backend.bucket));
        }
        for backend in &project.backends.remote {
            inventory_backends.push(format!("remote:{}", backend.url));
        }
        for backend in &project.backends.local {
            inventory_backends.push(format!("local:{}", backend.path));
        }

        let fuse_mount_point = {
            let declared = project.interfaces.fuse.mount_point.trim();
            if declared.is_empty() {
                None
            } else {
                Some(declared.to_string())
            }
        };

        let mut manifest = Self {
            repos,
            backends: Vec::new(),
            mounts: Vec::new(),
            auto_dependency_order: false,
        };
        manifest.absorb_repo_at_mounts();

        LoadedWorkspace {
            manifest,
            dialect: ManifestDialect::Project,
            fuse_mount_point,
            inventory_backends,
        }
    }

    /// Materialise a mount for every repo that declares `at`, unless an
    /// explicit `mounts:` entry already covers that source (explicit wins).
    fn absorb_repo_at_mounts(&mut self) {
        for repo in &self.repos {
            let Some(at) = repo
                .at
                .as_deref()
                .map(str::trim)
                .filter(|at| !at.is_empty())
            else {
                continue;
            };
            if self.mounts.iter().any(|m| m.source == repo.name) {
                continue;
            }
            self.mounts.push(WorkspaceMount {
                source: repo.name.clone(),
                at: at.to_string(),
                options: None,
            });
        }
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

    /// Build a mount plan with optional topological ordering.
    ///
    /// When `auto_dependency_order` is `true` and `dependencies` is provided,
    /// repos are topologically sorted so that dependencies mount first.
    /// Circular dependencies are detected, logged to stderr, and broken.
    /// When `auto_dependency_order` is `false` or no dependencies are
    /// provided, falls back to declaration order (current behavior).
    ///
    /// `dependencies`: a map from repo name to the list of repos it depends
    /// on (i.e., "auth-service" → ["shared-lib"] means auth-service mounts
    /// AFTER shared-lib).
    pub fn mount_plan(
        &self,
        dependencies: Option<&std::collections::HashMap<String, Vec<String>>>,
    ) -> Result<Vec<MountEntry>, WorkspaceError> {
        let entries = self.build_mount_plan()?;

        if !self.auto_dependency_order {
            return Ok(entries);
        }

        let deps = match dependencies {
            Some(d) => d,
            None => return Ok(entries),
        };

        if deps.is_empty() || entries.len() <= 1 {
            return Ok(entries);
        }

        Ok(topological_sort_mounts(&entries, deps))
    }
}

/// Topologically sort mount entries using Kahn's algorithm.
///
/// Dependencies map from repo name → list of repos it depends on.
/// "auth-service" → ["shared-lib"] means shared-lib must come first.
/// Circular dependencies are detected, logged to stderr, and broken
/// by keeping declaration order for the cycle members.
pub fn topological_sort_mounts(
    entries: &[MountEntry],
    dependencies: &std::collections::HashMap<String, Vec<String>>,
) -> Vec<MountEntry> {
    if dependencies.is_empty() {
        return entries.to_vec();
    }

    let mut in_degree: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut dependents: std::collections::HashMap<&str, Vec<&str>> =
        std::collections::HashMap::new();

    // Initialise every entry
    for entry in entries {
        in_degree.entry(entry.name.as_str()).or_insert(0);
    }

    // Build reverse edges: dep → [dependents]
    for (repo, repo_deps) in dependencies {
        for dep in repo_deps {
            if in_degree.contains_key(dep.as_str()) && in_degree.contains_key(repo.as_str()) {
                dependents
                    .entry(dep.as_str())
                    .or_default()
                    .push(repo.as_str());
                *in_degree.entry(repo.as_str()).or_insert(0) += 1;
            }
        }
    }

    // Start with nodes that have zero in-degree
    let mut queue: std::collections::VecDeque<&str> = in_degree
        .iter()
        .filter(|(_, &deg)| deg == 0)
        .map(|(&name, _)| name)
        .collect();

    let mut sorted_names: Vec<&str> = Vec::with_capacity(entries.len());

    while let Some(name) = queue.pop_front() {
        sorted_names.push(name);
        if let Some(deps_of_this) = dependents.get(name) {
            for &dependent in deps_of_this {
                if let Some(deg) = in_degree.get_mut(dependent) {
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push_back(dependent);
                    }
                }
            }
        }
    }

    // Detect cycles
    let cyclic: Vec<&str> = in_degree
        .iter()
        .filter(|(_, &deg)| deg > 0)
        .map(|(&name, _)| name)
        .collect();

    if !cyclic.is_empty() {
        eprintln!(
            "WARNING: circular dependency detected among repos: {:?}. \
             Breaking cycle arbitrarily.",
            cyclic
        );
        for name in &cyclic {
            if !sorted_names.contains(name) {
                sorted_names.push(name);
            }
        }
    }

    // Reorder
    let name_to_idx: std::collections::HashMap<&str, usize> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| (e.name.as_str(), i))
        .collect();

    let mut reordered: Vec<MountEntry> = Vec::with_capacity(entries.len());
    let mut placed: std::collections::HashSet<&str> = std::collections::HashSet::new();

    for name in &sorted_names {
        if let Some(&idx) = name_to_idx.get(name) {
            reordered.push(entries[idx].clone());
            placed.insert(name);
        }
    }

    // Append entries not in the dependency graph
    for entry in entries {
        if !placed.contains(entry.name.as_str()) {
            reordered.push(entry.clone());
        }
    }

    reordered
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

    // ── mount_plan / topological_sort_mounts tests ──

    fn make_entry(name: &str) -> MountEntry {
        MountEntry {
            name: name.to_string(),
            backing_path: std::path::PathBuf::from(format!("/tmp/{}", name)),
            at: format!("/mnt/vfs/{}/", name),
            writable: false,
        }
    }

    #[test]
    fn test_topological_sort_linear_deps() {
        // C ← B ← A  →  C, B, A
        let entries = vec![
            make_entry("repo-a"),
            make_entry("repo-b"),
            make_entry("repo-c"),
        ];
        let mut deps = std::collections::HashMap::new();
        deps.insert("repo-a".to_string(), vec!["repo-b".to_string()]);
        deps.insert("repo-b".to_string(), vec!["repo-c".to_string()]);

        let sorted = super::topological_sort_mounts(&entries, &deps);
        let names: Vec<&str> = sorted.iter().map(|e| e.name.as_str()).collect();

        let idx_c = names.iter().position(|&n| n == "repo-c").unwrap();
        let idx_b = names.iter().position(|&n| n == "repo-b").unwrap();
        let idx_a = names.iter().position(|&n| n == "repo-a").unwrap();
        assert!(idx_c < idx_b, "repo-c must come before repo-b");
        assert!(idx_b < idx_a, "repo-b must come before repo-a");
        assert_eq!(sorted.len(), 3, "all 3 entries present");
    }

    #[test]
    fn test_topological_sort_no_deps_fallback() {
        // No dependencies → declaration order preserved
        let entries = vec![
            make_entry("repo-a"),
            make_entry("repo-b"),
            make_entry("repo-c"),
        ];
        let deps = std::collections::HashMap::new();

        let sorted = super::topological_sort_mounts(&entries, &deps);
        let names: Vec<&str> = sorted.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["repo-a", "repo-b", "repo-c"]);
    }

    #[test]
    fn test_topological_sort_circular_dependency() {
        // A → B → A (cycle) — should not panic, should include both
        let entries = vec![make_entry("repo-a"), make_entry("repo-b")];
        let mut deps = std::collections::HashMap::new();
        deps.insert("repo-a".to_string(), vec!["repo-b".to_string()]);
        deps.insert("repo-b".to_string(), vec!["repo-a".to_string()]);

        let sorted = super::topological_sort_mounts(&entries, &deps);
        assert_eq!(sorted.len(), 2, "both entries present despite cycle");
        let names: Vec<&str> = sorted.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"repo-a"));
        assert!(names.contains(&"repo-b"));
    }

    #[test]
    fn test_topological_sort_partial_deps() {
        // A depends on B, C has no deps → C can be anywhere, but B before A
        let entries = vec![
            make_entry("repo-a"),
            make_entry("repo-b"),
            make_entry("repo-c"),
        ];
        let mut deps = std::collections::HashMap::new();
        deps.insert("repo-a".to_string(), vec!["repo-b".to_string()]);

        let sorted = super::topological_sort_mounts(&entries, &deps);
        let names: Vec<&str> = sorted.iter().map(|e| e.name.as_str()).collect();

        let idx_b = names.iter().position(|&n| n == "repo-b").unwrap();
        let idx_a = names.iter().position(|&n| n == "repo-a").unwrap();
        assert!(idx_b < idx_a, "repo-b must come before repo-a");
        assert_eq!(sorted.len(), 3, "all 3 entries present");
    }

    #[test]
    fn test_topological_sort_diamond_deps() {
        // A depends on B and C, B depends on D, C depends on D
        // Valid orders: D, B, C, A  or  D, C, B, A
        let entries = vec![
            make_entry("repo-a"),
            make_entry("repo-b"),
            make_entry("repo-c"),
            make_entry("repo-d"),
        ];
        let mut deps = std::collections::HashMap::new();
        deps.insert(
            "repo-a".to_string(),
            vec!["repo-b".to_string(), "repo-c".to_string()],
        );
        deps.insert("repo-b".to_string(), vec!["repo-d".to_string()]);
        deps.insert("repo-c".to_string(), vec!["repo-d".to_string()]);

        let sorted = super::topological_sort_mounts(&entries, &deps);
        let names: Vec<&str> = sorted.iter().map(|e| e.name.as_str()).collect();

        let idx_d = names.iter().position(|&n| n == "repo-d").unwrap();
        let idx_b = names.iter().position(|&n| n == "repo-b").unwrap();
        let idx_c = names.iter().position(|&n| n == "repo-c").unwrap();
        let idx_a = names.iter().position(|&n| n == "repo-a").unwrap();

        assert!(idx_d < idx_b, "repo-d must come before repo-b");
        assert!(idx_d < idx_c, "repo-d must come before repo-c");
        assert!(idx_b < idx_a, "repo-b must come before repo-a");
        assert!(idx_c < idx_a, "repo-c must come before repo-a");
        assert_eq!(sorted.len(), 4, "all 4 entries present");
    }

    // ───────── DF-WARPFS-50: both accepted manifest schemas ─────────

    mod schema_dialects {
        use super::*;

        /// The manifest `hilo init` writes: `version: 2`, the derived project
        /// name, defaults everywhere else (see `hilo-cli/src/commands/init.rs`).
        fn init_manifest_yaml(project: &str) -> String {
            let manifest = ProjectManifest {
                version: 2,
                project: crate::manifest::Project {
                    name: project.to_string(),
                    description: String::new(),
                },
                interfaces: Default::default(),
                repos: Vec::new(),
                backends: Default::default(),
                metadata: Default::default(),
                graph: Default::default(),
                permissions: Default::default(),
                triggers: Vec::new(),
                rules: Vec::new(),
                plugins: Vec::new(),
                discovery: Default::default(),
                sandbox: Default::default(),
                performance: Default::default(),
            };
            serde_yaml::to_string(&manifest).expect("project manifest serializes")
        }

        /// AC1: the init manifest is accepted (project schema), validates, and
        /// declares nothing to mount — the CLI turns that into an exit 0.
        #[test]
        fn init_manifest_loads_as_the_project_schema() {
            let loaded = WorkspaceManifest::parse_with_dialect(&init_manifest_yaml("test-ws"))
                .expect("the manifest `hilo init` writes must load");
            assert_eq!(loaded.dialect, ManifestDialect::Project);
            assert_eq!(loaded.declared_sources(), 0);
            assert!(loaded.manifest.mounts.is_empty());
            assert!(
                loaded.manifest.validate().is_empty(),
                "init manifest must validate: {:?}",
                loaded.manifest.validate()
            );
            assert_eq!(
                loaded.fuse_mount_point.as_deref(),
                Some("/mnt/vfs/project"),
                "the project schema's fuse mount_point is the default target"
            );
        }

        /// AC1 through the FILE path the command actually uses.
        #[test]
        fn init_manifest_file_loads_via_load_with_dialect() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("manifest.yaml");
            std::fs::write(&path, init_manifest_yaml("test-ws")).unwrap();
            let loaded = WorkspaceManifest::load_with_dialect(path.to_str().unwrap()).unwrap();
            assert_eq!(loaded.dialect, ManifestDialect::Project);
            assert!(WorkspaceManifest::load(path.to_str().unwrap()).is_ok());
        }

        /// `repos[].at` is the mount path in the project schema: every repo
        /// mounts there, unchanged.
        #[test]
        fn project_repos_mount_at_their_at_path() {
            let yaml = "\
version: 2
project:
  name: ws
repos:
  - name: auth-service
    url: git@github.com:org/auth-service.git
    ref: main
    at: /mnt/vfs/workspace/auth-service/
    writable: true
  - name: shared-lib
    url: git@github.com:org/shared-lib.git
    ref: v2.1.0
    at: /mnt/vfs/workspace/shared-lib/
    writable: false
";
            let loaded = WorkspaceManifest::parse_with_dialect(yaml).unwrap();
            assert_eq!(loaded.dialect, ManifestDialect::Project);
            assert!(
                loaded.manifest.validate().is_empty(),
                "{:?}",
                loaded.manifest.validate()
            );
            let got: Vec<(String, String, bool)> = loaded
                .manifest
                .mounts
                .iter()
                .map(|m| {
                    let repo = loaded
                        .manifest
                        .repos
                        .iter()
                        .find(|r| r.name == m.source)
                        .expect("mount source is a repo");
                    (m.source.clone(), m.at.clone(), repo.writable)
                })
                .collect();
            assert_eq!(
                got,
                vec![
                    (
                        "auth-service".to_string(),
                        "/mnt/vfs/workspace/auth-service/".to_string(),
                        true
                    ),
                    (
                        "shared-lib".to_string(),
                        "/mnt/vfs/workspace/shared-lib/".to_string(),
                        false
                    ),
                ]
            );
        }

        #[test]
        fn project_repo_without_at_mounts_at_slash_name() {
            let yaml = "\
version: 2
project:
  name: ws
repos:
  - name: shared-lib
    url: git@github.com:org/shared-lib.git
    ref: main
";
            let loaded = WorkspaceManifest::parse_with_dialect(yaml).unwrap();
            assert_eq!(loaded.manifest.mounts.len(), 1);
            assert_eq!(loaded.manifest.mounts[0].source, "shared-lib");
            assert_eq!(loaded.manifest.mounts[0].at, "/shared-lib");
        }

        #[test]
        fn project_auto_pull_boolean_becomes_an_interval() {
            let yaml = "\
version: 2
project:
  name: ws
repos:
  - name: pulls
    url: git@github.com:org/a.git
    ref: main
    auto_pull: true
  - name: no-pull
    url: git@github.com:org/b.git
    ref: main
    auto_pull: false
";
            let loaded = WorkspaceManifest::parse_with_dialect(yaml).unwrap();
            let by_name = |name: &str| {
                loaded
                    .manifest
                    .repos
                    .iter()
                    .find(|r| r.name == name)
                    .unwrap()
                    .auto_pull
            };
            assert_eq!(by_name("pulls"), Some(PROJECT_SCHEMA_AUTO_PULL_SECONDS));
            assert_eq!(by_name("no-pull"), None);
        }

        /// DF-WARPFS-50 failure (2): spec §4's `at:` and boolean `auto_pull:`
        /// were rejected by the workspace schema. Both are accepted now.
        #[test]
        fn workspace_schema_accepts_spec_section_4_repo_fields() {
            let yaml = "\
repos:
  - name: shared-lib
    url: /tmp/shared-lib.git
    ref: main
    at: mnt/shared-lib
    auto_pull: true
";
            let loaded = WorkspaceManifest::parse_with_dialect(yaml).unwrap();
            assert_eq!(loaded.dialect, ManifestDialect::Workspace);
            assert_eq!(
                loaded.manifest.repos[0].auto_pull,
                Some(PROJECT_SCHEMA_AUTO_PULL_SECONDS)
            );
            assert_eq!(
                loaded.manifest.mounts.len(),
                1,
                "repos[].at mounts the repo"
            );
            assert_eq!(loaded.manifest.mounts[0].at, "mnt/shared-lib");
            assert!(loaded.manifest.validate().is_empty());
        }

        #[test]
        fn workspace_schema_explicit_mount_wins_over_repo_at() {
            let yaml = "\
repos:
  - name: shared-lib
    url: /tmp/shared-lib.git
    ref: main
    at: /ignored
mounts:
  - source: shared-lib
    at: /explicit
";
            let loaded = WorkspaceManifest::parse_with_dialect(yaml).unwrap();
            assert_eq!(
                loaded.manifest.mounts.len(),
                1,
                "an explicit mount must not be duplicated: {:?}",
                loaded.manifest.mounts
            );
            assert_eq!(loaded.manifest.mounts[0].at, "/explicit");
        }

        /// Project-schema `backends:` is inventory (spec §4, §13) — reported,
        /// never turned into a mount.
        #[test]
        fn project_backends_are_inventory_not_mounts() {
            let yaml = "\
version: 2
project:
  name: ws
backends:
  s3:
    - bucket: my-models
      at: /project/models/
  local:
    - path: /data/datasets/
      at: /project/data/
";
            let loaded = WorkspaceManifest::parse_with_dialect(yaml).unwrap();
            assert_eq!(loaded.dialect, ManifestDialect::Project);
            assert_eq!(
                loaded.declared_sources(),
                0,
                "backends are not mount sources"
            );
            assert!(loaded.manifest.mounts.is_empty());
            assert_eq!(
                loaded.inventory_backends,
                vec![
                    "s3:my-models".to_string(),
                    "local:/data/datasets/".to_string()
                ]
            );
        }

        /// AC2: a document that fits neither schema must name BOTH schemas,
        /// the rejected field, what was expected and an actionable hint.
        #[test]
        fn schema_mismatch_names_both_schemas_field_and_expectation() {
            let yaml = "\
version: 2
project:
  name: x
mounts:
  - source: a
    at: /a
";
            let err = WorkspaceManifest::parse_with_dialect(yaml).unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains("workspace schema"),
                "names workspace schema: {msg}"
            );
            assert!(
                msg.contains("project schema"),
                "names project schema: {msg}"
            );
            assert!(msg.contains("mounts"), "names the rejected field: {msg}");
            assert!(msg.contains("expected"), "states what was expected: {msg}");
            assert!(msg.contains("hint:"), "offers a hint: {msg}");
        }

        #[test]
        fn unknown_field_error_names_the_workspace_schema() {
            let yaml = "repos:\n  - name: r\n    url: u\n    ref: main\n    wat: 1\n";
            let err = WorkspaceManifest::parse_with_dialect(yaml).unwrap_err();
            let msg = err.to_string();
            assert!(msg.contains("workspace schema"), "{msg}");
            assert!(msg.contains("unknown field `wat`"), "{msg}");
            assert!(msg.contains("expected"), "{msg}");
        }

        // ── AC3: the spec §4 example validates ──

        /// The YAML block of spec §4.1, read from the spec itself so the
        /// example and the loader can never drift apart.
        fn spec_section_4_yaml() -> String {
            let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../specs/warpfs-spec.md");
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
            let heading = text
                .find("### 4.1 Complete Schema")
                .expect("spec §4.1 heading present");
            let rest = &text[heading..];
            let open = rest.find("```yaml").expect("§4.1 has a yaml fence") + "```yaml".len();
            let body = &rest[open..];
            let close = body.find("```").expect("§4.1 yaml fence closes");
            body[..close].to_string()
        }

        #[test]
        fn spec_section_4_example_validates_without_errors() {
            let yaml = spec_section_4_yaml();
            let loaded = WorkspaceManifest::parse_with_dialect(&yaml)
                .unwrap_or_else(|e| panic!("spec §4 example rejected: {e}\n{yaml}"));
            assert_eq!(loaded.dialect, ManifestDialect::Project);
            let errors = loaded.manifest.validate();
            assert!(
                errors.is_empty(),
                "spec §4 example must validate with no errors: {errors:?}"
            );
            assert_eq!(loaded.manifest.repos.len(), 2);
            let ats: Vec<&str> = loaded
                .manifest
                .mounts
                .iter()
                .map(|m| m.at.as_str())
                .collect();
            assert!(
                ats.contains(&"/mnt/vfs/workspace/auth-service/"),
                "repos[].at becomes the mount path: {ats:?}"
            );
            assert!(
                ats.contains(&"/mnt/vfs/workspace/shared-lib/"),
                "repos[].at becomes the mount path: {ats:?}"
            );
        }

        #[test]
        fn spec_section_4_example_is_a_valid_project_manifest() {
            ProjectManifest::parse(&spec_section_4_yaml())
                .expect("spec §4 example must stay valid for the project schema too");
        }
    }
}
