//! Inventory file I/O for the `.vfs/` directory tree.
//!
//! Handles:
//! - `.vfs/` directory structure creation (§16)
//! - `graph/edges.jsonl` append/read (JSONL — one JSON object per line)
//! - `backends/mounts.yaml` read/write (YAML)

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use crate::MetadataError;

// ──────────────────────────── Types ────────────────────────────

/// A directed graph edge between two files or nodes.
///
/// Serialized as a single JSONL line in `edges.jsonl`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub rel: String,
}

/// A virtual mount entry mapping a backend to a path in the VFS.
///
/// Serialized in `backends/mounts.yaml`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct BackendMount {
    pub name: String,
    pub backend_type: String,
    pub path: String,
}

// ──────────────────────── Directory structure ────────────────────

/// Subdirectories that make up the `.vfs/` tree (§16).
const VFS_SUBDIRS: &[&str] = &["graph", "backends", "blobs", "features", "plugins", "cache"];

/// Create the `.vfs/` directory tree at `root`.
///
/// Produces:
/// ```text
/// root/.vfs/
///   graph/
///   backends/
///   blobs/
///   features/
///   plugins/
///   cache/
/// ```
///
/// Does **not** create `manifest.yaml` — that is the CLI's responsibility.
/// Idempotent: safe to call multiple times.
pub fn create_vfs_structure(root: &Path) -> Result<(), MetadataError> {
    let vfs_root = root.join(".vfs");
    fs::create_dir_all(&vfs_root)?;
    for subdir in VFS_SUBDIRS {
        fs::create_dir_all(vfs_root.join(subdir))?;
    }
    Ok(())
}

// ────────────────────────── Edge I/O ──────────────────────────

/// Serialize a single [`Edge`] to a JSONL line (compact JSON + newline).
pub fn edge_to_jsonl(edge: &Edge) -> Result<String, MetadataError> {
    let mut json = serde_json::to_string(edge)?;
    json.push('\n');
    Ok(json)
}

/// Append a single edge to `edges.jsonl`.
///
/// Creates the parent directory and the file itself if they do not exist.
pub fn append_edge(edges_jsonl: &Path, edge: &Edge) -> Result<(), MetadataError> {
    append_edges(edges_jsonl, std::slice::from_ref(edge))
}

/// Append multiple edges to `edges.jsonl` — all or nothing.
///
/// Each edge becomes one JSONL line. The file is opened once and all lines
/// are written in a single I/O operation. Does NOT deduplicate — use
/// `append_edges_deduped` if you need deduplication.
pub fn append_edges(edges_jsonl: &Path, edges: &[Edge]) -> Result<(), MetadataError> {
    if let Some(parent) = edges_jsonl.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut buf = Vec::with_capacity(edges.len() * 128);
    for edge in edges {
        serde_json::to_writer(&mut buf, edge)?;
        buf.push(b'\n');
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(edges_jsonl)?;
    file.write_all(&buf)?;
    Ok(())
}

/// Append only edges that don't already exist in the file.
///
/// Reads existing edges from the file, deduplicates by `(from, to, rel)`
/// tuple, and appends only genuinely new edges. Returns the count of
/// actually-appended edges (0 if all were duplicates).
pub fn append_edges_deduped(edges_jsonl: &Path, edges: &[Edge]) -> Result<usize, MetadataError> {
    // Read existing edges to build a dedup set.
    let mut seen = std::collections::HashSet::new();
    if edges_jsonl.exists() {
        let contents = fs::read_to_string(edges_jsonl)?;
        for line in contents.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(edge) = serde_json::from_str::<Edge>(line) {
                seen.insert((edge.from.clone(), edge.to.clone(), edge.rel.clone()));
            }
        }
    }

    // Filter to genuinely new edges.
    let new_edges: Vec<&Edge> = edges
        .iter()
        .filter(|e| !seen.contains(&(e.from.clone(), e.to.clone(), e.rel.clone())))
        .collect();

    if new_edges.is_empty() {
        return Ok(0);
    }

    append_edges(
        edges_jsonl,
        &new_edges.iter().map(|&e| e.clone()).collect::<Vec<_>>(),
    )?;
    Ok(new_edges.len())
}

// ──────────────────────── Mounts YAML I/O ────────────────────────

/// Read `backends/mounts.yaml` and deserialize as a list of [`BackendMount`].
///
/// Returns an empty vec if the file does not exist (not an error — just no
/// mounts configured yet).
pub fn read_mounts(mounts_yaml: &Path) -> Result<Vec<BackendMount>, MetadataError> {
    let contents = match fs::read(mounts_yaml) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Vec::new());
        }
        Err(e) => return Err(e.into()),
    };

    // An empty file is not valid YAML for a sequence — return empty vec.
    let trimmed = String::from_utf8_lossy(&contents);
    if trimmed.trim().is_empty() {
        return Ok(Vec::new());
    }

    let mounts: Vec<BackendMount> = serde_yaml::from_str(&trimmed)?;
    Ok(mounts)
}

/// Serialize mounts to YAML and write to `backends/mounts.yaml`.
///
/// Creates the parent directory if it does not exist.
pub fn write_mounts(mounts_yaml: &Path, mounts: &[BackendMount]) -> Result<(), MetadataError> {
    if let Some(parent) = mounts_yaml.parent() {
        fs::create_dir_all(parent)?;
    }

    let yaml = serde_yaml::to_string(mounts)?;
    fs::write(mounts_yaml, yaml)?;
    Ok(())
}
