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

/// FastAPI-shaped fixture (GAP-064): a regular package tree with the file
/// shape the real project has (`fastapi/routing.py` next to
/// `fastapi/__init__.py`). The tempdir root is deliberately NOT a package —
/// it has no `__init__.py` — so this also proves host path components never
/// leak into the resolved module name.
fn write_fastapi_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("fastapi")).unwrap();
    std::fs::write(dir.path().join("fastapi/__init__.py"), "").unwrap();
    std::fs::write(
        dir.path().join("fastapi/routing.py"),
        "from fastapi.dependencies.utils import Depends\n",
    )
    .unwrap();
    dir
}

#[test]
fn test_impact_python_file_query_returns_package_importers(
) -> Result<(), Box<dyn std::error::Error>> {
    let dir = write_fastapi_fixture();
    // The file-form query — exactly what `hilo graph impact fastapi/routing.py`
    // issues. It must resolve to `pkg:fastapi.routing` (GAP-064).
    let routing = dir
        .path()
        .join("fastapi/routing.py")
        .to_string_lossy()
        .into_owned();

    let graph = GraphDB::open(":memory:")?;
    let mut edges: Vec<Edge> = ["app/main.py", "app/api/routes.py", "tests/test_routing.py"]
        .iter()
        .map(|importer| edge(importer, "pkg:fastapi.routing", "imports"))
        .collect();
    // GAP-048 family semantics apply to Python nodes too: a `::` member node
    // (`from fastapi.routing import APIRouter` style) is part of the family.
    edges.push(edge(
        "app/decorators.py",
        "pkg:fastapi.routing::APIRouter",
        "imports",
    ));
    // Decoys: neighbouring Python nodes that must NOT be pulled in.
    edges.push(edge("app/deps.py", "pkg:fastapi.dependencies", "imports"));
    edges.push(edge("app/init_consumer.py", "pkg:fastapi", "imports"));
    edges.push(edge(
        "other/starlette.py",
        "pkg:starlette.routing",
        "imports",
    ));
    graph.insert_edges(&edges)?;

    let results = compute_impact(graph.conn(), &routing, 1)?;

    let mut got: Vec<&str> = results.iter().map(|r| r.path.as_str()).collect();
    got.sort_unstable();
    let mut want = vec![
        "app/api/routes.py",
        "app/decorators.py",
        "app/main.py",
        "tests/test_routing.py",
    ];
    want.sort_unstable();
    assert_eq!(got, want, "file-form impact must return every importer");
    assert!(results.iter().all(|r| r.depth == 1));

    // Control: without the package (same file name, no `__init__.py`) the
    // query resolves to nothing and returns no importers.
    let plain = tempfile::tempdir().unwrap();
    std::fs::write(plain.path().join("standalone.py"), "").unwrap();
    let plain_file = plain
        .path()
        .join("standalone.py")
        .to_string_lossy()
        .into_owned();
    let empty = compute_impact(graph.conn(), &plain_file, 1)?;
    assert!(
        empty.is_empty(),
        "standalone .py must not resolve to a pkg node"
    );

    Ok(())
}
