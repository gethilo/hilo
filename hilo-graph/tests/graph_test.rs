//! Integration tests for the DuckDB graph backend.

use hilo_graph::graph::GraphDB;
use hilo_metadata::inventory::Edge;

#[test]
fn test_graph_insert_and_count() {
    let db = GraphDB::open(":memory:").unwrap();
    let edges = vec![
        Edge {
            from: "a.go".into(),
            to: "std:fmt".into(),
            rel: "imports".into(),
        },
        Edge {
            from: "b.go".into(),
            to: "std:os".into(),
            rel: "imports".into(),
        },
    ];
    db.insert_edges(&edges).unwrap();
    assert_eq!(db.count_edges().unwrap(), 2);
}

#[test]
fn test_graph_group_by() {
    let db = GraphDB::open(":memory:").unwrap();
    let edges = vec![
        Edge {
            from: "a.go".into(),
            to: "std:fmt".into(),
            rel: "imports".into(),
        },
        Edge {
            from: "b.go".into(),
            to: "std:fmt".into(),
            rel: "imports".into(),
        },
    ];
    db.insert_edges(&edges).unwrap();
    let groups = db.group_by_dependency().unwrap();
    assert_eq!(groups.len(), 1); // one unique (to, rel) pair
    assert_eq!(groups[0].2, 2); // count = 2
}

#[test]
fn test_graph_stats() {
    let db = GraphDB::open(":memory:").unwrap();
    let edges = vec![
        Edge {
            from: "a.go".into(),
            to: "std:fmt".into(),
            rel: "imports".into(),
        },
        Edge {
            from: "b.go".into(),
            to: "std:os".into(),
            rel: "imports".into(),
        },
        Edge {
            from: "c.go".into(),
            to: "std:fmt".into(),
            rel: "imports".into(),
        },
    ];
    db.insert_edges(&edges).unwrap();
    let stats = db.stats().unwrap();
    assert_eq!(stats.total_edges, 3);
    assert_eq!(stats.unique_files, 3); // a.go, b.go, c.go
    assert_eq!(stats.unique_dependencies, 2); // fmt, os
}

#[test]
fn test_graph_distinct_files() {
    let db = GraphDB::open(":memory:").unwrap();
    let edges = vec![
        Edge {
            from: "a.go".into(),
            to: "std:fmt".into(),
            rel: "imports".into(),
        },
        Edge {
            from: "a.go".into(),
            to: "std:os".into(),
            rel: "imports".into(),
        },
        Edge {
            from: "b.go".into(),
            to: "std:fmt".into(),
            rel: "imports".into(),
        },
    ];
    db.insert_edges(&edges).unwrap();
    let (froms, tos) = db.distinct_files().unwrap();
    assert_eq!(froms.len(), 2); // a.go, b.go
    assert_eq!(tos.len(), 2); // fmt, os
    assert!(froms.contains(&"a.go".to_string()));
    assert!(froms.contains(&"b.go".to_string()));
}

#[test]
fn test_graph_top_dependencies() {
    let db = GraphDB::open(":memory:").unwrap();
    let edges = vec![
        Edge {
            from: "a.go".into(),
            to: "std:fmt".into(),
            rel: "imports".into(),
        },
        Edge {
            from: "b.go".into(),
            to: "std:fmt".into(),
            rel: "imports".into(),
        },
        Edge {
            from: "c.go".into(),
            to: "std:fmt".into(),
            rel: "imports".into(),
        },
        Edge {
            from: "a.go".into(),
            to: "std:os".into(),
            rel: "imports".into(),
        },
    ];
    db.insert_edges(&edges).unwrap();
    let stats = db.stats().unwrap();
    // fmt (3 refs) should be ranked above os (1 ref).
    assert!(!stats.top_dependencies.is_empty());
    assert_eq!(stats.top_dependencies[0].0, "std:fmt");
    assert_eq!(stats.top_dependencies[0].1, 3);
}

#[test]
fn test_graph_insert_dedup() {
    let db = GraphDB::open(":memory:").unwrap();
    let edges = vec![
        Edge {
            from: "a.go".into(),
            to: "std:fmt".into(),
            rel: "imports".into(),
        },
        Edge {
            from: "a.go".into(),
            to: "std:os".into(),
            rel: "imports".into(),
        },
        Edge {
            from: "b.go".into(),
            to: "std:fmt".into(),
            rel: "imports".into(),
        },
    ];
    // First insert: all 3 edges are new.
    db.insert_edges(&edges).unwrap();
    assert_eq!(db.count_edges().unwrap(), 3);
    // Second insert: all 3 edges are duplicates — count stays at 3.
    db.insert_edges(&edges).unwrap();
    assert_eq!(db.count_edges().unwrap(), 3);
    // Insert a mix of old and new — only the new edge is added.
    let mixed = vec![
        Edge {
            from: "a.go".into(),
            to: "std:fmt".into(),
            rel: "imports".into(),
        }, // duplicate
        Edge {
            from: "c.go".into(),
            to: "std:io".into(),
            rel: "imports".into(),
        }, // new
    ];
    db.insert_edges(&mixed).unwrap();
    assert_eq!(db.count_edges().unwrap(), 4);
}
