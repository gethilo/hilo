//! Git-tracked annotation sidecar: `.vfs/annotations.jsonl`.
//!
//! Xattrs do not survive `git push`/`git clone` — an annotation written by
//! `hilo meta --set` dies at the push. This module persists the same
//! annotations in a JSONL sidecar inside `.vfs/`, which IS committed (like
//! `manifest.yaml` and `graph/edges.jsonl`), so agent-curated metadata can
//! travel with the repository and be re-applied after a fresh clone via
//! `hilo meta apply`.
//!
//! One JSON object per line (compact), one record per `(rel_path, attr)`:
//! re-annotating the same key updates the record in place instead of
//! appending a duplicate. Reads are tolerant: blank or malformed lines are
//! skipped, never fatal — a hand-edited sidecar must not brick `hilo meta`.

use std::path::{Path, PathBuf};

use crate::MetadataError;

/// One annotation: `user.vfs.<attr> = <value>` on the file at `rel_path`
/// (relative to the workspace root, `/`-separated).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AnnotationRecord {
    /// Workspace-relative path of the annotated file.
    pub rel_path: String,
    /// Logical attribute name without the `user.vfs.` prefix.
    pub attr: String,
    /// Annotation value (may be empty — `--value ""` is a real write).
    pub value: String,
}

/// Sidecar location: `<workspace_root>/.vfs/annotations.jsonl`.
pub fn sidecar_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".vfs").join("annotations.jsonl")
}

/// Find the enclosing Hilo workspace root by walking up from `path` to the
/// nearest ancestor (including `path` itself) that contains `.vfs/`.
///
/// Returns `None` outside any Hilo workspace — callers keep their
/// pre-sidecar behavior (xattr-only) in that case.
pub fn find_workspace_root(path: &Path) -> Option<PathBuf> {
    for dir in path.ancestors() {
        if dir.join(".vfs").is_dir() {
            return Some(dir.to_path_buf());
        }
    }
    None
}

/// Append (or update in place) `rec` in the sidecar at `root`.
///
/// If a record with the same `(rel_path, attr)` already exists, the file is
/// rewritten with that record replaced at its position (atomic: temp file +
/// rename in the same directory); otherwise the record is appended. The
/// sidecar (and its `.vfs/` parent) is created when missing.
pub fn record_annotation(root: &Path, rec: &AnnotationRecord) -> Result<(), MetadataError> {
    let sidecar = sidecar_path(root);
    if let Some(parent) = sidecar.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let existing = match std::fs::read_to_string(&sidecar) {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };

    let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();
    let mut replaced = false;
    for line in lines.iter_mut() {
        if let Ok(existing_rec) = serde_json::from_str::<AnnotationRecord>(line) {
            if existing_rec.rel_path == rec.rel_path && existing_rec.attr == rec.attr {
                *line = serde_json::to_string(rec)?;
                replaced = true;
                break;
            }
        }
    }
    if !replaced {
        lines.push(serde_json::to_string(rec)?);
    }

    let mut out = String::new();
    for line in &lines {
        out.push_str(line);
        out.push('\n');
    }
    write_atomic(&sidecar, out.as_bytes())
}

/// Read all annotations from the sidecar at `root`.
///
/// Tolerant by design: a missing sidecar reads as no annotations, and blank
/// or malformed lines are skipped (a hand-edited sidecar must not make
/// `hilo meta` fail for everyone). Well-formed records are returned in file
/// order.
pub fn read_annotations(root: &Path) -> Result<Vec<AnnotationRecord>, MetadataError> {
    let sidecar = sidecar_path(root);
    let contents = match std::fs::read_to_string(&sidecar) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut records = Vec::new();
    for line in contents.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(rec) = serde_json::from_str::<AnnotationRecord>(line) {
            records.push(rec);
        }
    }
    Ok(records)
}

/// Remove every record matching `(rel_path, attr)` from the sidecar.
///
/// Missing sidecar is a no-op (nothing recorded). Returns the number of
/// records removed.
pub fn remove_annotation(root: &Path, rel_path: &str, attr: &str) -> Result<usize, MetadataError> {
    let sidecar = sidecar_path(root);
    let contents = match std::fs::read_to_string(&sidecar) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e.into()),
    };
    let mut kept: Vec<String> = Vec::new();
    let mut removed = 0usize;
    for line in contents.lines() {
        let matches = serde_json::from_str::<AnnotationRecord>(line)
            .map(|rec| rec.rel_path == rel_path && rec.attr == attr)
            .unwrap_or(false);
        if matches {
            removed += 1;
        } else {
            kept.push(line.to_string());
        }
    }
    if removed == 0 {
        return Ok(0);
    }
    let mut out = String::new();
    for line in &kept {
        out.push_str(line);
        out.push('\n');
    }
    write_atomic(&sidecar, out.as_bytes())?;
    Ok(removed)
}

/// Write `bytes` to `path` atomically: write a sibling temp file in the same
/// directory, then rename over the target (rename is atomic on the same
/// filesystem — a crash mid-write leaves the old sidecar intact).
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), MetadataError> {
    let tmp = tempfile_name(path);
    std::fs::write(&tmp, bytes)?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Best-effort cleanup so failed rewrites do not litter .vfs/.
            let _ = std::fs::remove_file(&tmp);
            Err(e.into())
        }
    }
}

/// Sibling temp-file path for `target` (same dir, dot-prefixed, pid-tagged).
fn tempfile_name(target: &Path) -> PathBuf {
    let file_name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "annotations.jsonl".to_string());
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!(".{}.tmp.{}", file_name, std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn ws() -> TempDir {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join(".vfs")).unwrap();
        dir
    }

    fn rec(rel: &str, attr: &str, value: &str) -> AnnotationRecord {
        AnnotationRecord {
            rel_path: rel.to_string(),
            attr: attr.to_string(),
            value: value.to_string(),
        }
    }

    #[test]
    fn sidecar_path_is_vfs_annotations_jsonl() {
        let dir = TempDir::new().unwrap();
        assert_eq!(
            sidecar_path(dir.path()),
            dir.path().join(".vfs").join("annotations.jsonl")
        );
    }

    #[test]
    fn find_workspace_root_walks_up_to_vfs_dir() {
        let dir = ws();
        let nested = dir.path().join("a/b/c");
        fs::create_dir_all(&nested).unwrap();
        assert_eq!(find_workspace_root(&nested), Some(dir.path().to_path_buf()));
    }

    #[test]
    fn find_workspace_root_none_outside_workspace() {
        let dir = TempDir::new().unwrap();
        assert_eq!(find_workspace_root(&dir.path().join("nowhere")), None);
    }

    #[test]
    fn record_annotation_creates_missing_file_and_vfs_dir() {
        let dir = TempDir::new().unwrap();
        // No .vfs/ at all — record_annotation must create it.
        record_annotation(dir.path(), &rec("src/main.rs", "role", "entrypoint")).unwrap();
        let contents =
            fs::read_to_string(sidecar_path(dir.path())).expect("sidecar must be created");
        assert_eq!(
            contents,
            "{\"rel_path\":\"src/main.rs\",\"attr\":\"role\",\"value\":\"entrypoint\"}\n"
        );
    }

    #[test]
    fn record_annotation_appends_distinct_keys() {
        let dir = ws();
        record_annotation(dir.path(), &rec("a.rs", "role", "entrypoint")).unwrap();
        record_annotation(dir.path(), &rec("a.rs", "component", "cli")).unwrap();
        let records = read_annotations(dir.path()).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].attr, "role");
        assert_eq!(records[1].attr, "component");
    }

    #[test]
    fn record_annotation_updates_in_place_without_duplicates() {
        let dir = ws();
        record_annotation(dir.path(), &rec("a.rs", "role", "old")).unwrap();
        record_annotation(dir.path(), &rec("b.rs", "role", "other")).unwrap();
        record_annotation(dir.path(), &rec("a.rs", "role", "new")).unwrap();
        let records = read_annotations(dir.path()).unwrap();
        // Exactly one a.rs/role record, still in its original position.
        let matches: Vec<_> = records
            .iter()
            .filter(|r| r.rel_path == "a.rs" && r.attr == "role")
            .collect();
        assert_eq!(matches.len(), 1, "no duplicate line for same rel_path+attr");
        assert_eq!(matches[0].value, "new");
        assert_eq!(records[0].rel_path, "a.rs", "replaced in place");
        assert_eq!(records[1].rel_path, "b.rs", "unrelated record untouched");
        assert_eq!(records.len(), 2);
    }

    #[test]
    fn read_annotations_missing_sidecar_is_empty() {
        let dir = TempDir::new().unwrap();
        assert!(read_annotations(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn read_annotations_tolerates_blank_and_malformed_lines() {
        let dir = ws();
        fs::write(
            sidecar_path(dir.path()),
            "{\"rel_path\":\"good.rs\",\"attr\":\"role\",\"value\":\"lib\"}\n\
             \n\
             not json at all\n\
             {\"broken\": true}\n\
             {\"rel_path\":\"also-good.rs\",\"attr\":\"status\",\"value\":\"stable\"}\n",
        )
        .unwrap();
        let records = read_annotations(dir.path()).unwrap();
        assert_eq!(records.len(), 2, "malformed lines skipped, not fatal");
        assert_eq!(records[0].rel_path, "good.rs");
        assert_eq!(records[1].rel_path, "also-good.rs");
    }

    #[test]
    fn remove_annotation_drops_matching_records_only() {
        let dir = ws();
        record_annotation(dir.path(), &rec("a.rs", "role", "x")).unwrap();
        record_annotation(dir.path(), &rec("a.rs", "other", "y")).unwrap();
        record_annotation(dir.path(), &rec("b.rs", "role", "z")).unwrap();
        let removed = remove_annotation(dir.path(), "a.rs", "role").unwrap();
        assert_eq!(removed, 1);
        let records = read_annotations(dir.path()).unwrap();
        assert_eq!(records.len(), 2);
        assert!(records
            .iter()
            .all(|r| !(r.rel_path == "a.rs" && r.attr == "role")));
    }

    #[test]
    fn remove_annotation_missing_sidecar_is_noop() {
        let dir = TempDir::new().unwrap();
        assert_eq!(remove_annotation(dir.path(), "a.rs", "role").unwrap(), 0);
    }
}
