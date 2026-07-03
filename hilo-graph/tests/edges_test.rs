//! Integration tests for external cross-repo graph edges and impact analysis.

use hilo_graph::edges;
use hilo_graph::graph::{Direction, GraphDB};
use hilo_graph::impact::compute_impact_with_external;
use hilo_metadata::inventory::Edge;

fn edge(from: &str, to: &str, rel: &str) -> Edge {
    Edge {
        from: from.to_string(),
        to: to.to_string(),
        rel: rel.to_string(),
        provenance: "ast_exact".to_string(),
        confidence: 1.0,
    }
}

#[test]
fn test_external_edge_detection_in_graph() {
    let db = GraphDB::open(":memory:").unwrap();
    // Local edge: auth-service imports fmt
    // External edge: auth-service imports shared-lib/pkg/utils
    db.insert_edges(&[
        edge("auth-service/src/handler.go", "std:fmt", "imports"),
        edge(
            "auth-service/src/handler.go",
            "external:shared-lib:pkg/utils.go",
            "imports",
        ),
    ])
    .unwrap();

    // `related` should return both local and external edges.
    let results = db
        .related("auth-service/src/handler.go", None, Direction::Forward)
        .unwrap();
    assert_eq!(results.len(), 2);

    // External edge should have the external: prefix.
    let external_edges: Vec<_> = results
        .iter()
        .filter(|e| edges::is_external(&e.to))
        .collect();
    assert_eq!(external_edges.len(), 1);
    assert_eq!(external_edges[0].to, "external:shared-lib:pkg/utils.go");
}

#[test]
fn test_external_impact_traverses_cross_repo() {
    // Cross-repo impact: shared-lib/pkg/utils.go is imported by
    // auth-service (via external: edge) and payment-service (via local edge).
    // Impact of shared-lib/pkg/utils.go should find both.
    let db = GraphDB::open(":memory:").unwrap();
    db.insert_edges(&[
        // auth-service imports via external edge
        edge(
            "auth-service/src/handler.go",
            "external:shared-lib:pkg/utils.go",
            "imports",
        ),
        // payment-service imports via local edge (same workspace)
        edge(
            "payment-service/src/handler.go",
            "shared-lib/pkg/utils.go",
            "imports",
        ),
        // transitively: auth imports some other dep
        edge(
            "auth-service/src/router.go",
            "auth-service/src/handler.go",
            "imports",
        ),
    ])
    .unwrap();

    // Without external flag: only payment-service found.
    let local =
        compute_impact_with_external(db.conn(), "shared-lib/pkg/utils.go", 10, false).unwrap();
    let local_paths: Vec<&str> = local.iter().map(|f| f.path.as_str()).collect();
    assert!(
        local_paths.contains(&"payment-service/src/handler.go"),
        "should find local dependent"
    );
    assert!(
        !local_paths.contains(&"auth-service/src/handler.go"),
        "should NOT find external dependent without --external"
    );

    // With external flag: both are found.
    let all = compute_impact_with_external(db.conn(), "shared-lib/pkg/utils.go", 10, true).unwrap();
    let all_paths: Vec<&str> = all.iter().map(|f| f.path.as_str()).collect();
    assert!(
        all_paths.contains(&"payment-service/src/handler.go"),
        "should find local dependent"
    );
    assert!(
        all_paths.contains(&"auth-service/src/handler.go"),
        "should find external dependent via external: prefix"
    );
    // Transitive: auth-service/src/router.go depends on handler.go which
    // depends on utils.go (depth 2).
    assert!(
        all_paths.contains(&"auth-service/src/router.go"),
        "should find transitive external dependent (depth 2)"
    );
}

#[test]
fn test_external_impact_parsed_edge_format() {
    // Verify that parse_external_edge correctly decomposes external references.
    let (repo, path) = edges::parse_external_edge("external:shared-lib:pkg/utils.go").unwrap();
    assert_eq!(repo, "shared-lib");
    assert_eq!(path, "pkg/utils.go");

    // Verify that the formatted edge matches.
    let formatted = edges::format_external_edge(repo, path);
    assert_eq!(formatted, "external:shared-lib:pkg/utils.go");
}
