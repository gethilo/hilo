//! GAP-104-R2: bare-basename Python import evidence, for file-form queries.
//!
//! When importers sit in the module's own subdirectory and add that directory
//! to `sys.path` (script collections, driver/plugin dirs, sys.path-manipulating
//! test suites), they import the module by its BARE BASENAME: `from base
//! import ...` inside `scripts/drivers/` emits an edge to `pkg:base`, even
//! though the file resolves structurally to `pkg:scripts.drivers.base`. The
//! graph holds the edges; the file-form query joins to a node nobody targets —
//! `hilo graph impact <file>` is silently empty and `graph untested` calls the
//! covered module untested.
//!
//! Structure alone cannot decide the node: `scripts(reg)/drivers(plain)/
//! base.py` and flask's `src/flask(reg)/sansio(plain)/app.py` are
//! structurally identical (GAP-082 pins the latter to `flask.sansio.app`,
//! and the recurrence corpus — task-router, whose `scripts/drivers/` carries
//! an `__init__.py` — resolves fully-regular yet is still imported bare). The
//! graph itself is the discriminator: a real corpus has edges targeting
//! exactly the node the importers wrote, so pick the candidate the graph
//! carries edges TO.
//!
//! Evidence is read through [`EdgeEvidence`] — the DuckDB cache on the normal
//! path, the raw `edges.jsonl` scan on the degraded path — so impact,
//! related, untested, and stats all inherit the fix through the resolver they
//! already share.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use duckdb::{params, Connection};

use crate::resolution::{python_module_for_file, PkgResolver};

/// Where a query reads its edge evidence from.
///
/// The normal graph keeps its edges in the DuckDB cache; a degraded graph
/// (cache unwritable/destroyed) answers from the canonical JSONL. The
/// basename-evidence rule must agree on both, so both forms are one handle
/// the callers obtain from their own graph.
#[derive(Debug, Clone, Copy)]
pub enum EdgeEvidence<'a> {
    /// Query the DuckDB `edges` cache of this connection.
    Db(&'a Connection),
    /// Scan the canonical `edges.jsonl` at this path.
    Jsonl(&'a Path),
}

impl EdgeEvidence<'_> {
    /// Whether each of the two candidate nodes is targeted by at least one
    /// DISTINCT edge source. `(bare, dotted)` in the caller's order.
    pub fn targets(&self, bare: &str, dotted: &str) -> (bool, bool) {
        match self {
            EdgeEvidence::Db(conn) => target_counts_sql(conn, bare, dotted),
            EdgeEvidence::Jsonl(path) => target_counts_jsonl(path, bare, dotted),
        }
    }
}

/// Count the DISTINCT edge sources targeting each of the two nodes, over the
/// DuckDB `edges` cache. One indexed lookup answers both.
fn target_counts_sql(conn: &Connection, bare: &str, dotted: &str) -> (bool, bool) {
    let mut stmt = match conn.prepare(
        r#"SELECT "to", COUNT(DISTINCT "from") FROM edges
           WHERE "to" IN (?1, ?2) GROUP BY "to""#,
    ) {
        Ok(stmt) => stmt,
        // No edges table (or unreadable cache): no evidence either way.
        Err(_) => return (false, false),
    };
    let Ok(rows) = stmt.query_map(params![bare, dotted], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    }) else {
        return (false, false);
    };
    let mut out = (false, false);
    for row in rows.flatten() {
        let (to, froms) = row;
        if froms < 1 {
            continue;
        }
        if to == bare {
            out.0 = true;
        } else if to == dotted {
            out.1 = true;
        }
    }
    out
}

/// Count the DISTINCT edge sources targeting each of the two nodes, over the
/// raw JSONL edge stream (degraded mode). Unreadable files count as no
/// evidence — matching the degraded readers elsewhere in the crate.
fn target_counts_jsonl(path: &Path, bare: &str, dotted: &str) -> (bool, bool) {
    let Ok(file) = std::fs::File::open(path) else {
        return (false, false);
    };
    let reader = std::io::BufReader::new(file);
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut out = (false, false);
    for line in std::io::BufRead::lines(reader) {
        let Ok(line) = line else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(edge) = serde_json::from_str::<hilo_metadata::inventory::Edge>(trimmed) else {
            continue;
        };
        for (node, is_bare) in [(bare, true), (dotted, false)] {
            if edge.to == node && seen.insert((node.to_string(), edge.from.clone())) {
                if is_bare {
                    out.0 = true;
                } else {
                    out.1 = true;
                }
            }
        }
    }
    out
}

/// The bare-stem `pkg:` node of `rel_path` (`scripts/drivers/base.py` →
/// `pkg:base`), or `None` when the path has no stem.
fn bare_node(rel_path: &str) -> Option<String> {
    let stem = rel_path.rsplit('/').next()?.strip_suffix(".py")?;
    (!stem.is_empty()).then(|| format!("pkg:{stem}"))
}

/// The `(bare, dotted)` candidate pair for a Python file whose structural
/// module is dotted, or `None` for every other path (non-Python, flat
/// module, no stem). The caller counts the targeting edges itself and
/// applies the same `(bare_targeted && !dotted_targeted)` rule — this is the
/// scan-shared form of [`basename_seed_node`] for streaming consumers that
/// already hold one edge pass.
pub fn basename_seed_candidates(root: &Path, rel_path: &str) -> Option<(String, String)> {
    if !rel_path.ends_with(".py") {
        return None;
    }
    let dotted = python_module_for_file(&root.join(rel_path))?;
    if !dotted.contains('.') {
        return None;
    }
    let bare = bare_node(rel_path)?;
    let dotted_node = format!("pkg:{dotted}");
    (bare != dotted_node).then_some((bare, dotted_node))
}

/// The graph-evidence seed for a Python file path (GAP-104-R2).
///
/// Returns the bare-stem `pkg:` node when a real basename-import shape is
/// present — the file is a member of a dotted structural module AND the graph
/// carries edges targeting the bare node while NONE target the dotted one.
/// Every other shape resolves to `None` and the caller keeps its structural
/// answer.
pub fn basename_seed_node(
    evidence: EdgeEvidence<'_>,
    root: &Path,
    rel_path: &str,
) -> Option<String> {
    // 1. Python files only: the shape exists only in Python's basename
    //    import space. Other languages resolve structurally and unchanged.
    if !rel_path.ends_with(".py") {
        return None;
    }
    // 2. The file must be a real module member (a dotted structural node):
    //    flat modules (GAP-104) already resolve to their bare stem and have
    //    nothing to alias — the bare "seed" would equal the structural
    //    answer, which is not evidence of a different import surface.
    let dotted = python_module_for_file(&root.join(rel_path))?;
    if !dotted.contains('.') {
        return None;
    }
    let bare = bare_node(rel_path)?;
    let dotted_node = format!("pkg:{dotted}");
    if bare == dotted_node {
        return None;
    }
    // 3. The graph decides: bare-targeted and dotted-orphan → emit the seed.
    //    Bare AND dotted targeted → ambiguous; keep the structural answer.
    //    Neither, or dotted-only → the normal packaged layout.
    matches!(evidence.targets(&bare, &dotted_node), (true, false)).then_some(bare)
}

/// Convenience: the seeded node for `rel_path`, or `None` when the structural
/// resolution itself is `None` or the evidence rule does not fire. Callers
/// hold one `PkgResolver` per query; the structural answer is only verified
/// here, never replaced.
#[allow(dead_code)]
pub fn seeded_pkg_node(
    evidence: EdgeEvidence<'_>,
    root: &Path,
    rel_path: &str,
    resolver: &mut PkgResolver,
) -> Option<String> {
    resolver.pkg_node_at(&crate::resolution::AnchoredRoot::new(root), rel_path)?;
    basename_seed_node(evidence, root, rel_path)
}

/// Where the evidence JSONL path lives once a graph dir was derived from it.
/// `GraphDB::open` derives `<root>/.vfs/graph/` from the db path; the seed
/// callers reuse the same derivation for the degraded evidence arm.
pub fn edges_jsonl_for_db_path(db_path: &str) -> Option<PathBuf> {
    let path = Path::new(db_path);
    if path.file_name().and_then(|n| n.to_str()) != Some("graph.db") {
        return None;
    }
    path.parent().map(|dir| dir.join("edges.jsonl"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::GraphDB;
    use hilo_metadata::inventory::Edge;

    /// A repo-shaped graph dir: `.vfs/graph/graph.db` + `edges.jsonl`, the
    /// form `GraphDB::open` derives its root from. Returns the db handle and
    /// the JSONL path (for the degraded evidence arm).
    fn open_graph(root: &Path, edges: &[(&str, &str, &str)]) -> (GraphDB, PathBuf) {
        let graph_dir = root.join(".vfs/graph");
        std::fs::create_dir_all(&graph_dir).unwrap();
        let jsonl = graph_dir.join("edges.jsonl");
        let lines: Vec<String> = edges
            .iter()
            .map(|(from, to, rel)| serde_json::to_string(&Edge::new(*from, *to, *rel)).unwrap())
            .collect();
        std::fs::write(&jsonl, lines.join("\n") + "\n").unwrap();
        let db_path = graph_dir.join("graph.db");
        let db = GraphDB::open(db_path.to_str().unwrap()).unwrap();
        (db, jsonl)
    }

    /// The repro tree: `scripts/` is a regular package, `scripts/drivers/` is
    /// not; `base.py` is imported by bare basename from inside `drivers/`.
    fn repro_tree(root: &Path) {
        std::fs::create_dir_all(root.join("scripts/drivers")).unwrap();
        std::fs::write(root.join("scripts/__init__.py"), "").unwrap();
        std::fs::write(root.join("scripts/drivers/base.py"), "def helper(): ...\n").unwrap();
    }

    #[test]
    fn basename_seed_fires_for_the_sys_path_entry_shape() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        repro_tree(root);
        // Warm-time edges as the parser emits them: top.py does
        // `from base import helper` (bare, absolute-form) and the in-dir
        // sibling imports the same way — sys.path[0] makes every importer
        // write `base`, so ONLY pkg:base is targeted.
        let (db, _jsonl) = open_graph(
            root,
            &[
                ("scripts/drivers/__init__.py", "pkg:base", "imports"),
                ("top.py", "pkg:base", "imports"),
            ],
        );
        let evidence = EdgeEvidence::Db(db.conn());
        let mut resolver = PkgResolver::new();
        assert_eq!(
            seeded_pkg_node(evidence, root, "scripts/drivers/base.py", &mut resolver).as_deref(),
            Some("pkg:base"),
            "bare node targeted, dotted node orphaned → the seed fires"
        );
    }

    #[test]
    fn packaged_layout_with_real_dotted_edges_keeps_the_dotted_node() {
        // Flask shape: namespace dir under a regular package whose edges all
        // target the dotted node — the seed must NOT fire.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src/flask/sansio")).unwrap();
        std::fs::write(root.join("src/flask/__init__.py"), "").unwrap();
        std::fs::write(root.join("src/flask/sansio/app.py"), "class App: ...\n").unwrap();
        let (db, _) = open_graph(
            root,
            &[
                ("src/flask/config.py", "pkg:flask.sansio.app", "imports"),
                ("tests/app_test.py", "pkg:flask.sansio.app", "tested_by"),
            ],
        );
        let evidence = EdgeEvidence::Db(db.conn());
        let mut resolver = PkgResolver::new();
        assert_eq!(
            seeded_pkg_node(evidence, root, "src/flask/sansio/app.py", &mut resolver),
            None,
            "dotted node is targeted → the structural answer stands, no seed"
        );
    }

    #[test]
    fn fully_regular_but_bare_imported_corpus_shape_seeds() {
        // The recurrence corpus (task-router) shape: scripts/drivers/ HAS an
        // __init__.py (fully-regular chain), yet tests sys.path-insert the
        // drivers dir and import by bare basename — pkg:hermes is targeted,
        // pkg:scripts.drivers.hermes is not. The dotted structural answer
        // would strand coverage; the evidence rule picks the bare node.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("scripts/drivers")).unwrap();
        std::fs::write(root.join("scripts/__init__.py"), "").unwrap();
        std::fs::write(root.join("scripts/drivers/__init__.py"), "").unwrap();
        std::fs::write(root.join("scripts/drivers/hermes.py"), "def t(): ...\n").unwrap();
        let (db, _) = open_graph(
            root,
            &[
                (
                    "tests/test_proxy_driver_hermes.py",
                    "pkg:hermes",
                    "tested_by",
                ),
                ("scripts/drivers/__init__.py", "pkg:hermes", "imports"),
            ],
        );
        let evidence = EdgeEvidence::Db(db.conn());
        let mut resolver = PkgResolver::new();
        assert_eq!(
            seeded_pkg_node(evidence, root, "scripts/drivers/hermes.py", &mut resolver).as_deref(),
            Some("pkg:hermes"),
            "fully-regular chain, bare-imported: the seed picks the evidenced node"
        );
    }

    #[test]
    fn both_nodes_targeted_is_ambiguous_and_keeps_the_structural_answer() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        repro_tree(root);
        let (db, _) = open_graph(
            root,
            &[
                ("scripts/drivers/__init__.py", "pkg:base", "imports"),
                ("top.py", "pkg:scripts.drivers.base", "imports"),
                ("other.py", "pkg:scripts.drivers.base", "tested_by"),
            ],
        );
        let evidence = EdgeEvidence::Db(db.conn());
        let mut resolver = PkgResolver::new();
        assert_eq!(
            seeded_pkg_node(evidence, root, "scripts/drivers/base.py", &mut resolver),
            None,
            "both candidates targeted → ambiguous, structural answer stands"
        );
    }

    #[test]
    fn non_python_and_flat_modules_never_seed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        repro_tree(root);
        // Flat module: structural answer IS the bare node — nothing to alias.
        std::fs::write(root.join("util.py"), "def h(): ...\n").unwrap();
        let (db, _) = open_graph(
            root,
            &[
                ("app.py", "pkg:base", "imports"),
                ("app.py", "pkg:util", "imports"),
            ],
        );
        let evidence = EdgeEvidence::Db(db.conn());
        let mut resolver = PkgResolver::new();
        assert_eq!(
            seeded_pkg_node(evidence, root, "util.py", &mut resolver),
            None,
            "flat modules already resolve to their bare stem (GAP-104)"
        );
        // Non-Python files are out of the shape entirely.
        std::fs::write(root.join("lib.rs"), "pub fn f() {}\n").unwrap();
        assert_eq!(
            seeded_pkg_node(evidence, root, "lib.rs", &mut resolver),
            None,
            "other languages resolve structurally and unchanged"
        );
    }

    #[test]
    fn neither_node_targeted_is_not_this_shape() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        repro_tree(root);
        let (db, _) = open_graph(root, &[("unrelated.py", "pkg:os", "imports")]);
        let evidence = EdgeEvidence::Db(db.conn());
        let mut resolver = PkgResolver::new();
        assert_eq!(
            seeded_pkg_node(evidence, root, "scripts/drivers/base.py", &mut resolver),
            None,
            "no bare evidence → the caller keeps its structural answer"
        );
    }

    #[test]
    fn degraded_jsonl_graphs_read_the_same_evidence() {
        // Degraded evidence arm: the raw JSONL scan must answer identically
        // to the DuckDB arm, so degraded impact inherits the fix.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        repro_tree(root);
        let (_db, jsonl) = open_graph(
            root,
            &[("scripts/drivers/__init__.py", "pkg:base", "imports")],
        );
        let evidence = EdgeEvidence::Jsonl(&jsonl);
        assert_eq!(
            basename_seed_node(evidence, root, "scripts/drivers/base.py").as_deref(),
            Some("pkg:base"),
            "JSONL-only graphs must answer from the same evidence rule"
        );

        // The same JSONL read through the DB arm of a graph whose cache
        // holds the same rows must agree (the normal warm path).
        let (db, _) = open_graph(
            root,
            &[("scripts/drivers/__init__.py", "pkg:base", "imports")],
        );
        assert_eq!(
            basename_seed_node(EdgeEvidence::Db(db.conn()), root, "scripts/drivers/base.py")
                .as_deref(),
            Some("pkg:base"),
            "DB-backed evidence must agree with the JSONL arm"
        );
    }

    #[test]
    fn jsonl_evidence_tolerates_missing_or_malformed_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        repro_tree(root);
        // Missing file: no evidence, no seed.
        let missing = dir.path().join("absent.jsonl");
        assert_eq!(
            basename_seed_node(
                EdgeEvidence::Jsonl(&missing),
                root,
                "scripts/drivers/base.py"
            ),
            None,
            "an unreadable evidence file is no evidence at all"
        );
        // Malformed lines are skipped, valid ones still count.
        let (db, jsonl) = open_graph(
            root,
            &[("scripts/drivers/__init__.py", "pkg:base", "imports")],
        );
        let mut text = std::fs::read_to_string(&jsonl).unwrap();
        text.push_str("{\"from\": broken\n");
        text.push_str("not json at all\n");
        std::fs::write(&jsonl, text).unwrap();
        let _ = db; // keep the graph alive for the tempdir's sake
        assert_eq!(
            basename_seed_node(EdgeEvidence::Jsonl(&jsonl), root, "scripts/drivers/base.py")
                .as_deref(),
            Some("pkg:base"),
            "valid lines survive malformed neighbors"
        );
    }

    #[test]
    fn evidence_path_derivation_matches_the_open_shape() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join(".vfs/graph/graph.db");
        let derived = edges_jsonl_for_db_path(db_path.to_str().unwrap()).unwrap();
        assert_eq!(derived, dir.path().join(".vfs/graph/edges.jsonl"));
        // A non-graph.db path derives nothing (defensive; the callers always
        // pass the real db path).
        assert_eq!(edges_jsonl_for_db_path("/tmp/other.sqlite"), None);
    }
}
