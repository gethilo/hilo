//! Virtual directory listing — metadata-only, no FUSE required.
//!
//! Reads `backends/mounts.yaml` and presents backend entries as a virtual
//! directory structure. Agents see a unified tree even when files live in S3,
//! remote git repos, or local paths.

use serde::Serialize;
use std::path::Path;

use crate::manifest::Manifest;

/// A single entry in a virtual directory listing.
#[derive(Debug, Clone, Serialize)]
pub struct DirEntry {
    pub name: String,
    #[serde(rename = "type")]
    pub entry_type: String, // "file" or "directory"
    pub backend: Option<String>,
    pub size: Option<u64>,
    pub r#virtual: bool,
}

/// Result of resolving a virtual path to its real storage location.
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedPath {
    pub real_path: String,
    pub backend: String,
    pub cached: bool,
    pub sync_status: String,
}

/// List entries in a virtual directory by consulting the backends mount table.
///
/// Returns an empty vec if the virtual path has no configured backends.
pub fn list_directory(manifest: &Manifest, virtual_path: &str) -> Vec<DirEntry> {
    let mut entries = Vec::new();

    // S3 backends
    for s3 in &manifest.backends.s3 {
        if virtual_path == "/" || s3.at.starts_with(virtual_path) {
            entries.push(DirEntry {
                name: s3.at.trim_start_matches('/').to_string(),
                entry_type: "directory".to_string(),
                backend: Some("s3".to_string()),
                size: None,
                r#virtual: true,
            });
        }
    }

    // Remote git backends
    for remote in &manifest.backends.remote {
        if virtual_path == "/" || remote.at.starts_with(virtual_path) {
            entries.push(DirEntry {
                name: remote.at.trim_start_matches('/').to_string(),
                entry_type: "directory".to_string(),
                backend: Some("git".to_string()),
                size: None,
                r#virtual: true,
            });
        }
    }

    // Local path backends
    for local in &manifest.backends.local {
        if virtual_path == "/" || local.at.starts_with(virtual_path) {
            entries.push(DirEntry {
                name: local.at.trim_start_matches('/').to_string(),
                entry_type: "directory".to_string(),
                backend: Some("local".to_string()),
                size: None,
                r#virtual: true,
            });
        }
    }

    entries
}

/// Resolve a virtual path to its real storage location.
///
/// Checks each backend in order and returns the first match.
pub fn resolve_path(manifest: &Manifest, virtual_path: &str) -> Option<ResolvedPath> {
    // Check S3 backends
    for s3 in &manifest.backends.s3 {
        if virtual_path.starts_with(&s3.at) {
            let cache_path = s3.cache.as_ref().map(|c| {
                let rel = virtual_path.strip_prefix(&s3.at).unwrap_or(virtual_path);
                Path::new(c).join(rel.trim_start_matches('/'))
            });
            return Some(ResolvedPath {
                real_path: cache_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| {
                        format!("s3://{}/{}", s3.bucket, s3.prefix.as_deref().unwrap_or(""))
                    }),
                backend: "s3".to_string(),
                cached: cache_path.as_ref().map(|p| p.exists()).unwrap_or(false),
                sync_status: "unknown".to_string(),
            });
        }
    }

    // Check remote git backends
    for remote in &manifest.backends.remote {
        if virtual_path.starts_with(&remote.at) {
            let worktree = format!(
                "{}/{}",
                remote.cache.as_deref().unwrap_or("/tmp"),
                remote.url.replace('/', "_")
            );
            let exists = Path::new(&worktree).exists();
            return Some(ResolvedPath {
                real_path: worktree,
                backend: "git".to_string(),
                cached: exists,
                sync_status: "unknown".to_string(),
            });
        }
    }

    // Check local backends
    for local in &manifest.backends.local {
        if virtual_path.starts_with(&local.at) {
            return Some(ResolvedPath {
                real_path: local.path.clone(),
                backend: "local".to_string(),
                cached: Path::new(&local.path).exists(),
                sync_status: "synced".to_string(),
            });
        }
    }

    // Fallback: resolve relative paths against the current working directory.
    // When no backend matches, treat the path as a local workspace file.
    let cwd = std::env::current_dir().ok()?;
    let resolved = cwd.join(virtual_path.trim_start_matches('/'));
    let exists = resolved.exists();
    Some(ResolvedPath {
        real_path: resolved.to_string_lossy().to_string(),
        backend: "local".to_string(),
        cached: exists,
        sync_status: if exists {
            "synced".to_string()
        } else {
            "not found on disk".to_string()
        },
    })
}
