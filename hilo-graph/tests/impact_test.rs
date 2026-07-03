//! Integration tests for `hilo_graph::impact`.

use hilo_graph::graph::GraphDB;
use hilo_graph::impact::{compute_impact, ImpactFile, ImpactResult};
use hilo_metadata::inventory::Edge;

/// Helper: build an `Edge` from string slices.
fn edge(from: &str, to: &str, rel: &str) -> Edge {
    Edge::new(from, to, rel)
}

#[test]
fn test_impact_direct() -> Result<(), Box<dyn std::error::Error>> {
    // Chain: a → b → c  (a imports b, b imports c)
    let graph = GraphDB::open(":memory:")?;
    graph.insert_edges(&[edge("a", "b", "imports"), edge("b", "c", "imports")])?;

    // Impact of c: b depends on c (depth 1), a depends on b (depth 2).
    let results = compute_impact(graph.conn(), "c", 10)?;

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].path, "b");
    assert_eq!(results[0].relation, "imports");
    assert_eq!(results[0].depth, 1);
    assert_eq!(results[1].path, "a");
    assert_eq!(results[1].depth, 2);

    Ok(())
}

#[test]
fn test_impact_transitive() -> Result<(), Box<dyn std::error::Error>> {
    // Chain: a → b → c → d
    let graph = GraphDB::open(":memory:")?;
    graph.insert_edges(&[
        edge("a", "b", "imports"),
        edge("b", "c", "imports"),
        edge("c", "d", "imports"),
    ])?;

    // Impact of d, max_depth=3: c (1), b (2), a (3).
    let results = compute_impact(graph.conn(), "d", 3)?;

    assert_eq!(results.len(), 3);
    assert_eq!(results[0].path, "c");
    assert_eq!(results[0].depth, 1);
    assert_eq!(results[1].path, "b");
    assert_eq!(results[1].depth, 2);
    assert_eq!(results[2].path, "a");
    assert_eq!(results[2].depth, 3);

    Ok(())
}

#[test]
fn test_impact_circular() -> Result<(), Box<dyn std::error::Error>> {
    // Cycle: a → b, b → a
    let graph = GraphDB::open(":memory:")?;
    graph.insert_edges(&[edge("a", "b", "imports"), edge("b", "a", "imports")])?;

    // Impact of a: b depends on a (depth 1). a is already visited, so no loop.
    let results = compute_impact(graph.conn(), "a", 10)?;

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].path, "b");
    assert_eq!(results[0].depth, 1);

    Ok(())
}

#[test]
fn test_impact_max_depth_zero() -> Result<(), Box<dyn std::error::Error>> {
    let graph = GraphDB::open(":memory:")?;
    graph.insert_edges(&[edge("a", "b", "imports"), edge("b", "c", "imports")])?;

    // max_depth=0 → no traversal at all.
    let results = compute_impact(graph.conn(), "c", 0)?;
    assert!(results.is_empty());

    Ok(())
}

#[test]
fn test_impact_max_depth_one() -> Result<(), Box<dyn std::error::Error>> {
    // Chain: a → b → c
    let graph = GraphDB::open(":memory:")?;
    graph.insert_edges(&[edge("a", "b", "imports"), edge("b", "c", "imports")])?;

    // max_depth=1 → only direct dependent of c (b). a is 2 hops away, excluded.
    let results = compute_impact(graph.conn(), "c", 1)?;

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].path, "b");
    assert_eq!(results[0].depth, 1);

    Ok(())
}

#[test]
fn test_impact_not_in_graph() -> Result<(), Box<dyn std::error::Error>> {
    // Empty graph — no edges at all.
    let graph = GraphDB::open(":memory:")?;

    let results = compute_impact(graph.conn(), "nonexistent", 10)?;
    assert!(results.is_empty());

    Ok(())
}

#[test]
fn test_impact_json_format() -> Result<(), Box<dyn std::error::Error>> {
    let result = ImpactResult {
        files: vec![
            ImpactFile {
                path: "b.rs".to_string(),
                relation: "imports".to_string(),
                depth: 1,
                provenance: None,
                confidence: None,
            },
            ImpactFile {
                path: "a.rs".to_string(),
                relation: "imports".to_string(),
                depth: 2,
                provenance: None,
                confidence: None,
            },
        ],
    };

    let json = serde_json::to_string_pretty(&result)?;
    assert!(json.contains("\"files\""));
    assert!(json.contains("\"path\": \"b.rs\""));
    assert!(json.contains("\"relation\": \"imports\""));
    assert!(json.contains("\"depth\": 1"));
    assert!(json.contains("\"path\": \"a.rs\""));
    assert!(json.contains("\"depth\": 2"));

    Ok(())
}
