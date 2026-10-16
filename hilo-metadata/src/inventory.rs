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

// REV-WARPFS-8: every writer of `edges.jsonl` (warm append, trigger append,
// `graph gc`) takes this advisory lock for its whole read-modify-write cycle.
// The advisory flock cross-process; in-process, the guard is held for the
// function's body, so sequential calls from one process serialize naturally
// and only concurrent OS threads within one process can contend.
/// The `(from, to, rel, provenance)` identity shared by every dedup surface:
/// the append filter, `graph gc`, and the DuckDB reconcile stamps. Distinct
/// relations between the same file pair remain distinct edges — dedup is
/// never on `(from, to)` alone.
pub type EdgeKey = (String, String, String, String);

/// The dedup identity of an [`Edge`]: `(from, to, rel, provenance)`.
pub fn edge_key(edge: &Edge) -> EdgeKey {
    (
        edge.from.clone(),
        edge.to.clone(),
        edge.rel.clone(),
        edge.provenance.clone(),
    )
}

/// Take an exclusive advisory lock on `<edges_jsonl>.lock` (creating the
/// lock file if needed) and run `f` with it held.
///
/// The lock file is `<edges>.lock`, never `edges.jsonl` itself: `warm`'s
/// append path creates the lock file even for a first-ever append, and
/// `.vfs/graph/` untracked lock files must not collide with the tracked
/// inventory file.
///
/// Blocks until the lock is free — a concurrent `hilo graph gc` and a
/// trigger append serialize instead of interleaving. A failed lock is
/// propagated as an error (fail closed): silently writing unlocked on an
/// exotic filesystem would reintroduce exactly the interleaving this lock
/// exists to prevent.
fn with_edges_lock<T>(
    edges_jsonl: &Path,
    f: impl FnOnce() -> Result<T, MetadataError>,
) -> Result<T, MetadataError> {
    let lock_path = edges_jsonl.with_extension("jsonl.lock");
    // The lock file's parent may not exist yet (first-ever append into a
    // fresh tree, gc on a missing file) — create it, matching what the
    // append path does for the data file itself.
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)?;
    }
    // truncate(false) is load-bearing: the lock file is a lock TOKEN, its
    // content is never written or replaced.
    let lock_file = match OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
    {
        Ok(file) => file,
        Err(e) => {
            return Err(MetadataError::Io(std::io::Error::new(
                e.kind(),
                format!("failed to open lock file {}: {e}", lock_path.display()),
            )))
        }
    };
    let mut rwlock = fd_lock::RwLock::new(lock_file);
    let _guard = match rwlock.write() {
        Ok(guard) => guard,
        Err(e) => {
            return Err(MetadataError::Io(std::io::Error::new(
                e.kind(),
                format!("failed to lock {}: {e}", lock_path.display()),
            )))
        }
    };
    f()
}

// ──────────────────────────── Types ────────────────────────────

/// A directed graph edge between two files or nodes.
///
/// Serialized as a single JSONL line in `edges.jsonl`.
///
/// ## Backward compatibility
///
/// The `provenance` and `confidence` fields default to `AstExact` / `1.0`
/// when missing from JSON (old `edges.jsonl` files without provenance). This
/// is handled via `#[serde(default)]` on both fields.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub rel: String,
    /// How this edge was discovered (AST, heuristic, lexical, etc.).
    ///
    /// Defaults to `ast_exact` when missing (backward compat with old JSONL).
    #[serde(default = "default_provenance")]
    pub provenance: String,
    /// Confidence weight (0.0 – 1.0) reflecting the reliability of this edge.
    ///
    /// Defaults to `1.0` when missing (backward compat with old JSONL).
    #[serde(default = "default_confidence")]
    pub confidence: f64,
}

/// Default provenance string for old edges without the field.
fn default_provenance() -> String {
    "ast_exact".to_string()
}

/// Default confidence for old edges without the field.
fn default_confidence() -> f64 {
    1.0
}

impl Edge {
    /// Create a new edge with `ast_exact` provenance and `1.0` confidence.
    ///
    /// This is the standard constructor for edges extracted directly from
    /// the AST (the most common case).
    pub fn new(from: impl Into<String>, to: impl Into<String>, rel: impl Into<String>) -> Self {
        Edge {
            from: from.into(),
            to: to.into(),
            rel: rel.into(),
            provenance: "ast_exact".to_string(),
            confidence: 1.0,
        }
    }

    /// Create a new edge with a specific provenance and confidence.
    pub fn with_provenance(
        from: impl Into<String>,
        to: impl Into<String>,
        rel: impl Into<String>,
        provenance: impl Into<String>,
        confidence: f64,
    ) -> Self {
        Edge {
            from: from.into(),
            to: to.into(),
            rel: rel.into(),
            provenance: provenance.into(),
            confidence,
        }
    }
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
///
/// REV-WARPFS-8: the dedup set also grows with each accepted edge, so
/// duplicates WITHIN one batch collapse to their first occurrence (the
/// original implementation filtered only against pre-existing file
/// content, letting an intra-batch duplicate land twice). The whole
/// read-filter-append cycle runs under the `<edges>.jsonl.lock` advisory
/// flock, so concurrent writers (warm, triggers, `graph gc`) serialize
/// instead of interleaving.
pub fn append_edges_deduped(edges_jsonl: &Path, edges: &[Edge]) -> Result<usize, MetadataError> {
    with_edges_lock(edges_jsonl, || {
        // Read existing edges to build a dedup set.
        let mut seen: std::collections::HashSet<EdgeKey> = std::collections::HashSet::new();
        if edges_jsonl.exists() {
            let contents = fs::read_to_string(edges_jsonl)?;
            for line in contents.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(edge) = serde_json::from_str::<Edge>(line) {
                    seen.insert(edge_key(&edge));
                }
            }
        }

        // Filter to genuinely new edges, growing the set as we accept so
        // an intra-batch duplicate is skipped too.
        let mut new_edges: Vec<&Edge> = Vec::with_capacity(edges.len());
        for e in edges {
            if seen.insert(edge_key(e)) {
                new_edges.push(e);
            }
        }

        if new_edges.is_empty() {
            // INV-001: a warm that parsed the tree still materializes the
            // tracked inventory file — an empty-but-present edges.jsonl is the
            // honest "this project has no discovered edges" state and keeps
            // the file's existence independent of edge count.
            if !edges_jsonl.exists() {
                if let Some(parent) = edges_jsonl.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::File::create(edges_jsonl)?;
            }
            return Ok(0);
        }

        append_edges(
            edges_jsonl,
            &new_edges.iter().map(|&e| e.clone()).collect::<Vec<_>>(),
        )?;
        Ok(new_edges.len())
    })
}

/// Remove exact-duplicate rows from `edges.jsonl` (REV-WARPFS-8).
///
/// Deduplication identity is `(from, to, rel, provenance)` — the same key
/// [`append_edges_deduped`] filters on, so a file that is clean by this
/// pass stays clean under appends, and vice versa. The FIRST occurrence of
/// each identity is kept (stable order: the output preserves the file's
/// row order for the surviving rows); `confidence` is payload, not
/// identity, and travels with the kept row.
///
/// Mechanics: read → filter under the `<edges>.jsonl.lock` advisory flock →
/// write a sibling temp file → rename over the original. The rename is
/// atomic per-filesystem, so a reader never sees a torn file and a crash
/// leaves the original intact. Malformed lines (unparseable JSON) are
/// PRESERVED verbatim rather than dropped — this pass removes exact
/// duplicates only, and silently discarding unparseable rows would be a
/// lossy rewrite hiding a different problem. Empty lines are dropped (the
/// appender never writes them; a trailing blank is already not a row).
///
/// Returns `(rows_before, rows_after, duplicates_removed)`. Idempotent: a
/// second run on the same file returns zero removed and rewrites nothing
/// content-wise (the rewrite is skipped entirely when nothing was removed).
pub fn gc_edges(edges_jsonl: &Path) -> Result<(usize, usize, usize), MetadataError> {
    with_edges_lock(edges_jsonl, || {
        if !edges_jsonl.exists() {
            return Ok((0, 0, 0));
        }
        let contents = fs::read_to_string(edges_jsonl)?;
        let mut seen: std::collections::HashSet<EdgeKey> = std::collections::HashSet::new();
        let mut kept: Vec<String> = Vec::new();
        let mut malformed: Vec<&str> = Vec::new();
        let mut rows_before: usize = 0;
        for line in contents.lines() {
            if line.trim().is_empty() {
                continue;
            }
            rows_before += 1;
            match serde_json::from_str::<Edge>(line) {
                Ok(edge) => {
                    if seen.insert(edge_key(&edge)) {
                        kept.push(line.to_string());
                    }
                }
                Err(_) => malformed.push(line),
            }
        }
        let rows_after = kept.len() + malformed.len();
        let removed = rows_before - rows_after;
        if removed == 0 {
            return Ok((rows_before, rows_after, 0));
        }
        // Atomic swap: write the compacted file next to the original, then
        // rename over it. Same filesystem => atomic rename.
        let tmp_path = edges_jsonl.with_extension("jsonl.gc-tmp");
        {
            let mut tmp = fs::File::create(&tmp_path)?;
            for line in kept.iter() {
                writeln!(tmp, "{line}")?;
            }
            for line in malformed.iter() {
                writeln!(tmp, "{line}")?;
            }
            tmp.flush()?;
            tmp.sync_all()?;
        }
        fs::rename(&tmp_path, edges_jsonl)?;
        Ok((rows_before, rows_after, removed))
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// INV-001 / a905c0b companion: a warm that discovers zero new edges
    /// still materializes the tracked edges.jsonl — empty-but-present is
    /// the honest no-edges state (pre-fix the file was never created and
    /// the INV-001 contract test panicked on the missing file).
    #[test]
    fn append_deduped_creates_empty_inventory_when_absent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("graph").join("edges.jsonl");
        let n = append_edges_deduped(&path, &[]).unwrap();
        assert_eq!(n, 0);
        assert!(
            path.exists(),
            "zero-edge warm must still create the tracked inventory"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
    }

    /// Control: a real edge is appended exactly once across repeated calls.
    #[test]
    fn append_deduped_appends_once_and_is_idempotent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("edges.jsonl");
        let edge = Edge {
            from: "src/main.rs".into(),
            to: "src/lib.rs".into(),
            rel: "imports".into(),
            provenance: "ast_exact".into(),
            confidence: 1.0,
        };
        assert_eq!(
            append_edges_deduped(&path, std::slice::from_ref(&edge)).unwrap(),
            1
        );
        assert_eq!(append_edges_deduped(&path, &[edge]).unwrap(), 0);
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents.lines().count(), 1, "no duplicates: {contents}");
    }

    /// REV-WARPFS-8: duplicates WITHIN one batch collapse to their first
    /// occurrence — the pre-fix filter only consulted pre-existing file
    /// content, so both copies of an intra-batch duplicate landed.
    #[test]
    fn append_deduped_collapses_intra_batch_duplicates() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("edges.jsonl");
        let edge = Edge {
            from: "a.py".into(),
            to: "b.py".into(),
            rel: "imports".into(),
            provenance: "ast_exact".into(),
            confidence: 1.0,
        };
        let n = append_edges_deduped(&path, &[edge.clone(), edge.clone(), edge]).unwrap();
        assert_eq!(n, 1, "one identity, one row");
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents.lines().count(), 1, "no duplicates: {contents}");
    }

    /// REV-WARPFS-8: the dedup identity is the 4-tuple, never `(from, to)`
    /// alone — two relations between the same pair stay two rows.
    #[test]
    fn append_deduped_keeps_distinct_relations_between_same_pair() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("edges.jsonl");
        let imports = Edge {
            from: "tests/test_a.py".into(),
            to: "src/a.py".into(),
            rel: "imports".into(),
            provenance: "ast_exact".into(),
            confidence: 1.0,
        };
        let tests = Edge {
            from: "tests/test_a.py".into(),
            to: "src/a.py".into(),
            rel: "tests".into(),
            provenance: "heuristic".into(),
            confidence: 0.8,
        };
        let n = append_edges_deduped(&path, &[imports, tests]).unwrap();
        assert_eq!(n, 2, "distinct (rel, provenance) => distinct edges");
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 2);
    }

    /// REV-WARPFS-8: `gc_edges` removes only exact duplicates, keeps first
    /// occurrences in order, preserves distinct relations, and is
    /// idempotent (a second pass removes nothing and does not rewrite).
    #[test]
    fn gc_edges_removes_duplicates_and_is_idempotent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("edges.jsonl");
        let e = |from: &str, to: &str, rel: &str, prov: &str| Edge {
            from: from.into(),
            to: to.into(),
            rel: rel.into(),
            provenance: prov.into(),
            confidence: 1.0,
        };
        append_edges(&path, &[e("a.py", "b.py", "imports", "ast_exact")]).unwrap();
        append_edges(&path, &[e("a.py", "b.py", "imports", "ast_exact")]).unwrap();
        append_edges(&path, &[e("a.py", "b.py", "tests", "heuristic")]).unwrap();
        append_edges(&path, &[e("a.py", "b.py", "imports", "ast_exact")]).unwrap();

        let (before, after, removed) = gc_edges(&path).unwrap();
        assert_eq!((before, after, removed), (4, 2, 2));

        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2, "kept rows: {contents}");
        // First occurrence stays first; distinct relation preserved.
        let parsed: Vec<Edge> = lines
            .iter()
            .map(|l| serde_json::from_str::<Edge>(l).unwrap())
            .collect();
        assert_eq!(parsed[0].rel, "imports");
        assert_eq!(parsed[1].rel, "tests");

        // Idempotent: nothing left to remove, file untouched.
        let snapshot = contents.clone();
        let (b2, a2, r2) = gc_edges(&path).unwrap();
        assert_eq!((b2, a2, r2), (2, 2, 0));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), snapshot);
    }

    /// REV-WARPFS-8: gc preserves malformed lines verbatim (duplicate
    /// removal must not silently drop unparseable rows) and reports
    /// (0, 0, 0) for a missing file.
    #[test]
    fn gc_edges_preserves_malformed_lines_and_handles_missing_file() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("nope").join("edges.jsonl");
        assert_eq!(gc_edges(&missing).unwrap(), (0, 0, 0));

        let path = dir.path().join("edges.jsonl");
        let e = Edge {
            from: "a.py".into(),
            to: "b.py".into(),
            rel: "imports".into(),
            provenance: "ast_exact".into(),
            confidence: 1.0,
        };
        append_edges(&path, &[e.clone(), e]).unwrap();
        let mut contents = std::fs::read_to_string(&path).unwrap();
        contents.push_str("NOT JSON AT ALL\n");
        std::fs::write(&path, &contents).unwrap();

        let (before, after, removed) = gc_edges(&path).unwrap();
        assert_eq!((before, after, removed), (3, 2, 1));
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(
            out.contains("NOT JSON AT ALL"),
            "malformed line must survive gc: {out}"
        );
        assert_eq!(out.lines().count(), 2);
    }
}
