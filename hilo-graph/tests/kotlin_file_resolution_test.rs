//! DF-WARPFS-84: Kotlin file→package resolution for impact/related.
//!
//! The Kotlin parser emits `import org.jetbrains.exposed.v1.core.Table` →
//! `pkg:org.jetbrains.exposed.v1.core.Table` edges (file→pkg-node), so a
//! file-form query
//! (`exposed-core/src/main/kotlin/org/jetbrains/exposed/v1/core/Table.kt`)
//! must resolve to the same pkg node the pkg-form query names. Measured
//! defect (dogfood 2026-09-25): the file form answered "No dependents found"
//! while the pkg form returned 173/176 dependents (98.3%) on the exposed
//! corpus.
//!
//! These tests use a small exposed-shaped corpus (Table.kt plus importers) to
//! prove file-form and pkg-form answers are IDENTICAL — the same equivalence
//! the Ruby precedent (DF-WARPFS-78, `ruby_file_resolution_test.rs`) proves
//! for `require` modules. The corpus lives on disk under a tempdir (mirroring
//! `php_file_resolution_test.rs`): the Kotlin deriver reads the file's own
//! `package` header, so the anchored file-form query needs the real source,
//! unlike Ruby's purely syntactic derivation.

use hilo_graph::graph::{Direction, GraphDB};
use hilo_graph::impact::compute_impact_at;
use hilo_graph::parser::{Language, Parser};
use hilo_graph::resolution::{AnchoredRoot, PkgResolver};
use hilo_metadata::inventory::Edge;

const TARGET_FILE: &str = "exposed-core/src/main/kotlin/org/jetbrains/exposed/v1/core/Table.kt";
const TARGET_PKG: &str = "pkg:org.jetbrains.exposed.v1.core.Table";

/// Parse `source` as Kotlin and return the edges the parser emits for `file`.
fn kotlin_edges(file: &str, source: &str) -> Vec<Edge> {
    Parser::for_language(Language::Kotlin)
        .expect("Kotlin parser should build")
        .parse_imports(file, source)
        .expect("Kotlin parse should succeed")
}

/// An exposed-shaped corpus on disk: `Table.kt` plus importer files whose
/// `import` statements target the Table node. Importers live in a sibling
/// package under `src/main/kotlin` (as in the real corpus) so GAP-052 does
/// not double every edge with a `tested_by` copy — these are dependents, not
/// tests. Returns the anchored root plus a graph holding the real
/// parser-emitted edges (file→pkg), never hand-written ones.
fn exposed_fixture(importers: usize) -> (tempfile::TempDir, GraphDB) {
    let root = tempfile::tempdir().unwrap();
    let graph_path = root.path().join(".vfs/graph/graph.db");
    std::fs::create_dir_all(graph_path.parent().unwrap()).unwrap();

    let target_source = "package org.jetbrains.exposed.v1.core\n\nclass Table\n";
    let target_path = root.path().join(TARGET_FILE);
    std::fs::create_dir_all(target_path.parent().unwrap()).unwrap();
    std::fs::write(&target_path, target_source).unwrap();

    let mut edges = kotlin_edges(TARGET_FILE, target_source);
    for i in 0..importers {
        let importer_rel = format!(
            "exposed-core/src/main/kotlin/org/jetbrains/exposed/v1/core/tables/TableProbe{i}.kt"
        );
        let importer_source = "package org.jetbrains.exposed.v1.core.tables\n\nimport org.jetbrains.exposed.v1.core.Table\n\nclass Probe\n";
        let importer_path = root.path().join(&importer_rel);
        std::fs::create_dir_all(importer_path.parent().unwrap()).unwrap();
        std::fs::write(&importer_path, importer_source).unwrap();
        edges.extend(kotlin_edges(&importer_rel, importer_source));
    }

    let graph = GraphDB::open(graph_path.to_str().unwrap()).unwrap();
    graph.insert_edges(&edges).unwrap();
    (root, graph)
}

#[test]
fn kotlin_file_form_impact_matches_pkg_form_set() {
    let (root_dir, graph) = exposed_fixture(21);
    let anchored = AnchoredRoot::new(root_dir.path());

    // Pkg-form (the documented working query): 21 dependents, exact.
    let pkg = compute_impact_at(graph.conn(), TARGET_PKG, 5, None).unwrap();
    assert_eq!(pkg.len(), 21, "pkg-form must see all 21 importers");

    // File-form used to answer "No dependents found" (rc=0) because nothing
    // resolved the file to its declaration node. It must now match pkg-form.
    let file = compute_impact_at(graph.conn(), TARGET_FILE, 5, Some(&anchored)).unwrap();
    assert_eq!(
        file.len(),
        21,
        "file-form must match pkg-form (21 dependents)"
    );
    let mut pkg_paths: Vec<String> = pkg.iter().map(|r| r.path.clone()).collect();
    let mut file_paths: Vec<String> = file.iter().map(|r| r.path.clone()).collect();
    pkg_paths.sort();
    file_paths.sort();
    assert_eq!(
        pkg_paths, file_paths,
        "both id forms must return the identical dependent-id set"
    );
}

#[test]
fn kotlin_file_form_related_reverse_matches_pkg_form() {
    let (root_dir, graph) = exposed_fixture(21);
    let anchored = AnchoredRoot::new(root_dir.path());
    let _ = anchored; // related() resolves its own anchor from the DB root

    let pkg = db_related_reverse(&graph, TARGET_PKG);
    assert_eq!(pkg.len(), 21, "pkg-form reverse sees 21 incoming edges");

    // `related` derives its anchor from the DB root field; the graph was
    // opened at `<tmp>/.vfs/graph/graph.db`, so the stored root is the
    // tempdir root and the repo-relative subject resolves like impact does.
    let file = db_related_reverse(&graph, TARGET_FILE);
    assert_eq!(
        file.len(),
        21,
        "file-form reverse must match pkg-form (21 incoming edges)"
    );
}

fn db_related_reverse(graph: &GraphDB, subject: &str) -> Vec<Edge> {
    graph
        .related(subject, None, Direction::Reverse)
        .expect("related reverse should query")
}

#[test]
fn kotlin_file_no_longer_reported_as_stats_orphan() {
    let (_root, graph) = exposed_fixture(3);
    let stats = graph.stats().unwrap();
    assert!(
        !stats.orphans.iter().any(|o| o == TARGET_FILE),
        "Table.kt has incoming pkg-node references and must not be an orphan; got {:?}",
        stats.orphans
    );
}

#[test]
fn kotlin_file_resolves_through_pkg_node_at_with_real_anchor() {
    // The anchored variant impact actually uses: resolve the repo-relative
    // subject against a REAL on-disk root (the deriver must read the package
    // header there), and require the resolved node to equal the pkg-form node
    // exactly.
    let (root_dir, _graph) = exposed_fixture(1);
    let root = AnchoredRoot::new(root_dir.path());
    let mut resolver = PkgResolver::new();
    assert_eq!(
        resolver.pkg_node_at(&root, TARGET_FILE).as_deref(),
        Some(TARGET_PKG),
        "the anchored file resolution must name the pkg-form node"
    );
}
