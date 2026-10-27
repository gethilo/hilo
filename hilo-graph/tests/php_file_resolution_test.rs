//! DF-WARPFS-82: PHP same-namespace inheritance reaches graph impact.

use hilo_graph::graph::GraphDB;
use hilo_graph::impact::compute_impact_at;
use hilo_graph::parser::{Language, Parser};
use hilo_graph::resolution::AnchoredRoot;
use hilo_metadata::inventory::Edge;

const BASE_FILE: &str = "src/Example/Command/BaseCommand.php";
const BASE_NODE: &str = "pkg:Example\\Command\\BaseCommand";

fn php_edges(file: &str, source: &str) -> Vec<Edge> {
    Parser::for_language(Language::Php)
        .expect("PHP parser should build")
        .parse_imports(file, source)
        .expect("PHP parse should succeed")
}

fn fixture_db() -> (tempfile::TempDir, GraphDB) {
    let root = tempfile::tempdir().unwrap();
    let graph_path = root.path().join(".vfs/graph/graph.db");
    std::fs::create_dir_all(graph_path.parent().unwrap()).unwrap();

    let base_source =
        "<?php\nnamespace Example\\Command;\nuse Vendor\\Dependency;\nclass BaseCommand {}\n";
    let child_a_source =
        "<?php\nnamespace Example\\Command;\nclass ChildA extends BaseCommand implements Runnable {}\n";
    let child_b_source =
        "<?php\nnamespace Example\\Command;\nclass ChildB extends BaseCommand {}\n";
    for (file, source) in [
        (BASE_FILE, base_source),
        ("src/Example/Command/ChildA.php", child_a_source),
        ("src/Example/Command/ChildB.php", child_b_source),
    ] {
        let path = root.path().join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
    }

    let mut edges = Vec::new();
    edges.extend(php_edges(BASE_FILE, base_source));
    edges.extend(php_edges("src/Example/Command/ChildA.php", child_a_source));
    edges.extend(php_edges("src/Example/Command/ChildB.php", child_b_source));

    let graph = GraphDB::open(graph_path.to_str().unwrap()).unwrap();
    graph.insert_edges(&edges).unwrap();
    (root, graph)
}

#[test]
fn php_same_namespace_inheritance_is_visible_to_impact_and_stats() {
    let (_root, graph) = fixture_db();
    let impact = compute_impact_at(graph.conn(), BASE_NODE, 1, None).unwrap();
    let paths: Vec<&str> = impact.iter().map(|row| row.path.as_str()).collect();
    assert_eq!(
        impact.len(),
        2,
        "both derived classes must depend on BaseCommand"
    );
    assert!(paths.contains(&"src/Example/Command/ChildA.php"));
    assert!(paths.contains(&"src/Example/Command/ChildB.php"));

    let stats = graph.stats().unwrap();
    assert!(
        !stats.orphans.iter().any(|path| path == BASE_FILE),
        "BaseCommand has incoming pkg-node references and must not be an orphan: {:?}",
        stats.orphans
    );
}

#[test]
fn php_file_form_impact_resolves_declared_class_node() {
    let (root, graph) = fixture_db();
    let anchored = AnchoredRoot::new(root.path());
    let impact = compute_impact_at(graph.conn(), BASE_FILE, 5, Some(&anchored)).unwrap();
    let paths: Vec<&str> = impact.iter().map(|row| row.path.as_str()).collect();
    assert_eq!(impact.len(), 2, "file-form impact must resolve BaseCommand");
    assert!(paths.contains(&"src/Example/Command/ChildA.php"));
    assert!(paths.contains(&"src/Example/Command/ChildB.php"));
}
