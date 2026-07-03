//! Transitive impact analysis — find all files that depend on a given file, directly or transitively.

use std::collections::{HashSet, VecDeque};

use duckdb::{params, Connection};
use serde::Serialize;

use crate::error::GraphResult;

/// A single file in the impact chain.
#[derive(Debug, Clone, Serialize)]
pub struct ImpactFile {
    /// The file path.
    pub path: String,
    /// The relation type from the edge that connects this file to its dependent.
    pub relation: String,
    /// Distance from the start file (1 = direct dependent, N = N-hop dependent).
    pub depth: u32,
    /// How the edge was discovered (provenance string).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<String>,
    /// Confidence weight (0.0 – 1.0) of the edge.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
}

/// Result of an impact analysis.
#[derive(Debug, Clone, Serialize)]
pub struct ImpactResult {
    pub files: Vec<ImpactFile>,
}

/// Compute transitive impact: find all files that depend on `start_path`,
/// directly or transitively, up to `max_depth` hops.
///
/// The DuckDB `edges` table has columns `"from"` (source file), `"to"` (dependency),
/// `rel` (relation type). Impact analysis finds files whose `"from"` appears as
/// a dependent of `start_path` or its transitive dependents.
///
/// Uses BFS with a visited set to protect against circular imports.
/// Returns files ordered by discovery (BFS order) — direct dependents first,
/// then 2-hop, etc.
pub fn compute_impact(
    conn: &Connection,
    start_path: &str,
    max_depth: u32,
) -> GraphResult<Vec<ImpactFile>> {
    if max_depth == 0 {
        return Ok(Vec::new());
    }

    let mut results: Vec<ImpactFile> = Vec::new();
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(start_path.to_string());

    let mut queue: VecDeque<(String, u32)> = VecDeque::new();
    queue.push_back((start_path.to_string(), 0));

    let mut stmt =
        conn.prepare(r#"SELECT "from", rel, provenance, confidence FROM edges WHERE "to" = ?"#)?;

    while let Some((path, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }

        let rows = stmt.query_map(params![path], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<f64>>(3)?,
            ))
        })?;

        for row in rows {
            let (from, rel, prov, conf) = row?;
            if visited.insert(from.clone()) {
                results.push(ImpactFile {
                    path: from.clone(),
                    relation: rel,
                    depth: depth + 1,
                    provenance: prov,
                    confidence: conf,
                });
                queue.push_back((from, depth + 1));
            }
        }
    }

    Ok(results)
}

/// Compute transitive impact with external cross-repo edge support.
///
/// When `include_external` is `true`, the BFS also follows `external:<repo>:<path>`
/// edges by matching `to LIKE '%:' || path`.  This allows impact analysis
/// to traverse across repository boundaries in a multi-repo workspace.
pub fn compute_impact_with_external(
    conn: &Connection,
    start_path: &str,
    max_depth: u32,
    include_external: bool,
) -> GraphResult<Vec<ImpactFile>> {
    if max_depth == 0 {
        return Ok(Vec::new());
    }

    let mut results: Vec<ImpactFile> = Vec::new();
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(start_path.to_string());

    let mut queue: VecDeque<(String, u32)> = VecDeque::new();
    queue.push_back((start_path.to_string(), 0));

    let mut stmt =
        conn.prepare(r#"SELECT "from", rel, provenance, confidence FROM edges WHERE "to" = ?"#)?;
    let mut ext_stmt: Option<duckdb::Statement> = None;

    if include_external {
        // Match edges where `to` ends with `:path` (the external edge format
        // is `external:repo-name:path`).
        ext_stmt = Some(conn.prepare(
            r#"SELECT "from", rel, provenance, confidence FROM edges WHERE "to" LIKE '%:' || ?"#,
        )?);
    }

    while let Some((path, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }

        // Exact match (local edges).
        let rows = stmt.query_map(params![path.clone()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<f64>>(3)?,
            ))
        })?;
        for row in rows {
            let (from, rel, prov, conf) = row?;
            if visited.insert(from.clone()) {
                results.push(ImpactFile {
                    path: from.clone(),
                    relation: rel,
                    depth: depth + 1,
                    provenance: prov,
                    confidence: conf,
                });
                queue.push_back((from, depth + 1));
            }
        }

        // External-edge match (cross-repo).
        // Convert `repo/path/to/file` → `repo:path/to/file` by replacing
        // only the first `/` with `:` to match the `external:repo:path` format.
        if let Some(ref mut estmt) = ext_stmt {
            let ext_path = path.replacen('/', ":", 1);
            let rows = estmt.query_map(params![ext_path], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<f64>>(3)?,
                ))
            })?;
            for row in rows {
                let (from, rel, prov, conf) = row?;
                if visited.insert(from.clone()) {
                    results.push(ImpactFile {
                        path: from.clone(),
                        relation: rel,
                        depth: depth + 1,
                        provenance: prov,
                        confidence: conf,
                    });
                    queue.push_back((from, depth + 1));
                }
            }
        }
    }

    Ok(results)
}
