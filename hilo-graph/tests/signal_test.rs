//! Integration tests for the signal engine (`hilo_graph::signal`).

use hilo_graph::graph::GraphDB;
use hilo_graph::signal::{understand, understand_with_source, Resolution, SignalOpts, Tier};
use hilo_metadata::inventory::Edge;

fn edge(from: &str, to: &str, rel: &str) -> Edge {
    Edge::new(from, to, rel)
}

#[test]
fn test_signal_understand_finds_anchors() {
    let db = GraphDB::open(":memory:").unwrap();
    db.insert_edges(&[
        edge("src/auth/middleware.go", "src/auth/handler.go", "imports"),
        edge("src/auth/handler.go", "src/db/query.go", "imports"),
    ])
    .unwrap();

    let result = understand(&db, "auth middleware", &SignalOpts::default()).unwrap();

    assert!(
        result.anchors.iter().any(|a| a.contains("middleware")),
        "anchors should include middleware file"
    );
}

#[test]
fn test_signal_understand_is_deterministic() {
    let db = GraphDB::open(":memory:").unwrap();
    db.insert_edges(&[
        edge("src/auth/middleware.go", "src/auth/handler.go", "imports"),
        edge("src/auth/handler.go", "src/db/query.go", "imports"),
    ])
    .unwrap();

    let opts = SignalOpts::default();
    let r1 = understand(&db, "auth middleware", &opts).unwrap();
    let r2 = understand(&db, "auth middleware", &opts).unwrap();

    assert_eq!(
        r1.text, r2.text,
        "output must be byte-identical across runs"
    );
    assert_eq!(r1.anchors, r2.anchors);
}

#[test]
fn test_signal_understand_has_three_tiers() {
    let db = GraphDB::open(":memory:").unwrap();
    db.insert_edges(&[
        edge("src/auth/middleware.go", "src/auth/handler.go", "imports"),
        edge("src/auth/handler.go", "src/db/query.go", "imports"),
    ])
    .unwrap();

    let result = understand(&db, "auth", &SignalOpts::default()).unwrap();

    assert!(result.text.contains("## MAP"), "should have MAP tier");
    assert!(
        result.text.contains("## SIGNATURES"),
        "should have SIGNATURES tier"
    );
    assert!(result.text.contains("## DETAIL"), "should have DETAIL tier");
}

#[test]
fn test_signal_flat_resolution_omits_map_and_signatures() {
    let db = GraphDB::open(":memory:").unwrap();
    db.insert_edges(&[edge("src/auth.go", "src/db.go", "imports")])
        .unwrap();

    let opts = SignalOpts {
        resolution: Resolution::Flat,
        ..Default::default()
    };
    let result = understand(&db, "auth", &opts).unwrap();

    assert!(result.text.contains("DETAIL (flat)"));
    assert!(!result.text.contains("## MAP"));
}

#[test]
fn test_signal_understand_empty_graph() {
    let db = GraphDB::open(":memory:").unwrap();
    let result = understand(&db, "nonexistent feature", &SignalOpts::default()).unwrap();
    assert!(result.files.is_empty());
    assert!(result.anchors.is_empty());
}

#[test]
fn test_signal_understand_with_source_reader() {
    let db = GraphDB::open(":memory:").unwrap();
    db.insert_edges(&[edge("src/auth.go", "src/db.go", "imports")])
        .unwrap();

    let reader = |path: &str| -> Option<String> {
        if path.contains("auth") {
            Some("package auth\n\nfunc Authenticate() bool {\n    return true\n}\n".into())
        } else {
            None
        }
    };

    let result = understand_with_source(&db, "auth", &SignalOpts::default(), Some(reader)).unwrap();

    let auth_file = result.files.iter().find(|f| f.path.contains("auth"));
    assert!(auth_file.is_some());
    let auth_file = auth_file.unwrap();
    assert!(
        auth_file.symbols.iter().any(|s| s.contains("Authenticate")),
        "symbols should include Authenticate"
    );
}

#[test]
fn test_signal_understand_respects_max_nodes() {
    let db = GraphDB::open(":memory:").unwrap();
    let edges: Vec<Edge> = (0..50)
        .map(|i| edge(&format!("src/file_{i}.go"), "src/common.go", "imports"))
        .collect();
    db.insert_edges(&edges).unwrap();

    let opts = SignalOpts {
        max_nodes: 10,
        ..Default::default()
    };
    let result = understand(&db, "file", &opts).unwrap();
    assert!(
        result.files.len() <= 10,
        "should cap at max_nodes=10, got {}",
        result.files.len()
    );
}

#[test]
fn test_signal_understand_traverses_graph() {
    let db = GraphDB::open(":memory:").unwrap();
    db.insert_edges(&[
        edge("src/auth/middleware.go", "src/auth/handler.go", "imports"),
        edge("src/auth/handler.go", "src/db/query.go", "imports"),
    ])
    .unwrap();

    let opts = SignalOpts {
        depth: 2,
        ..Default::default()
    };
    let result = understand(&db, "auth", &opts).unwrap();

    let paths: Vec<&str> = result.files.iter().map(|f| f.path.as_str()).collect();
    assert!(paths.iter().any(|p| p.contains("middleware")));
    assert!(
        paths.iter().any(|p| p.contains("handler")),
        "should include 1-hop neighbor"
    );
}

#[test]
fn test_signal_understand_tokens_estimate_within_budget() {
    let db = GraphDB::open(":memory:").unwrap();
    db.insert_edges(&[edge(
        "src/auth/middleware.go",
        "src/auth/handler.go",
        "imports",
    )])
    .unwrap();

    let opts = SignalOpts {
        token_budget: 6000,
        ..Default::default()
    };
    let result = understand(&db, "auth", &opts).unwrap();

    assert!(
        result.tokens_estimate < 6000,
        "estimate should be within budget, got {}",
        result.tokens_estimate
    );
}

#[test]
fn test_signal_detail_tier_has_minified_source() {
    let db = GraphDB::open(":memory:").unwrap();
    db.insert_edges(&[edge("src/auth.go", "src/db.go", "imports")])
        .unwrap();

    let reader = |path: &str| -> Option<String> {
        if path.contains("auth") {
            Some(
                "package auth\n\n\nfunc Authenticate() bool {\n    return true\n}\n\n\n"
                    .to_string(),
            )
        } else {
            None
        }
    };

    let result = understand_with_source(&db, "auth", &SignalOpts::default(), Some(reader)).unwrap();

    let detail_file = result
        .files
        .iter()
        .find(|f| f.tier == Tier::Detail && f.detail.is_some());
    assert!(
        detail_file.is_some(),
        "should have at least one detail file"
    );
    let detail = detail_file.unwrap().detail.as_ref().unwrap();
    assert!(
        !detail.contains("\n\n"),
        "detail should have no blank lines (whitespace-minified)"
    );
}

#[test]
fn test_signal_signatures_include_line_numbers() {
    let db = GraphDB::open(":memory:").unwrap();
    db.insert_edges(&[edge("src/auth.go", "src/db.go", "imports")])
        .unwrap();

    let reader = |path: &str| -> Option<String> {
        if path.contains("auth") {
            Some("package auth\n\nfunc Authenticate() bool {\n    return true\n}\n".into())
        } else {
            None
        }
    };

    let result = understand_with_source(&db, "auth", &SignalOpts::default(), Some(reader)).unwrap();

    // SIGNATURES tier should contain file:line  signature format.
    assert!(
        result.text.contains("auth.go:3"),
        "signatures should include line numbers"
    );
}
