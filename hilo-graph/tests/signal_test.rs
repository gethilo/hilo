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

// ch:trace row=DF-WARPFS-118 spec=none:board-acceptance wave=none:serial-implementation test=hilo-graph/tests/signal_test.rs::understand_detail_owner_precedes_example_files_in_detail_order doc=none:board-acceptance prompt=/home/kara/.hermes/cache/scratch/warpfs-foreman-2026-11-14-16-47-33-attempt-1/brief-DF-WARPFS-118.txt evidence=hilo-graph/tests/signal_test.rs witness=none:foreman-verifies-after-merge verdict=none:foreman-verifies-after-merge commit=none:worker-commit memory=none:foreman-closeout
// STATUS: partial
// TIER: T2
// EVIDENCE: focused regression test plus commit

#[test]
fn understand_detail_owner_precedes_example_files_in_detail_order() {
    // DF-WARPFS-118: path-exact example anchors must not consume DETAIL ahead
    // of the implementation file that owns the request-flow symbols.
    let db = GraphDB::open(":memory:").unwrap();
    let mut edges = vec![
        edge(
            "examples/celery/src/task_app/views.py",
            "pkg:task_app",
            "imports",
        ),
        edge(
            "examples/javascript/js_example/views.py",
            "pkg:js_example",
            "imports",
        ),
        edge("src/flask/views.py", "pkg:flask.globals", "imports"),
        edge("src/flask/app.py", "src/flask/views.py", "imports"),
        edge("src/flask/helpers.py", "src/flask/views.py", "imports"),
    ];
    for dep in [
        "pkg:flask.ctx",
        "pkg:flask.globals",
        "pkg:flask.helpers",
        "pkg:flask.sessions",
        "pkg:flask.templating",
        "pkg:flask.wrappers",
    ] {
        edges.push(edge("src/flask/app.py", dep, "imports"));
    }
    db.insert_edges(&edges).unwrap();

    let reader = |path: &str| -> Option<String> {
        let source = match path {
            "src/flask/app.py" => {
                "def wsgi_app(self, environ):\n    pass\n\ndef full_dispatch_request(self):\n    pass\n\ndef finalize_request(self, rv):\n    return rv\n"
            }
            "src/flask/views.py" => "def render_view():\n    pass\n",
            "src/flask/helpers.py" => "def make_response():\n    pass\n",
            _ => "def handler():\n    pass\n",
        };
        Some(source.to_string())
    };
    let opts = SignalOpts {
        seed_limit: 5,
        map_exclude_paths: vec!["examples".to_string()],
        ..Default::default()
    };
    let result = understand_with_source(
        &db,
        "request flow WSGI->view->response",
        &opts,
        Some(reader),
    )
    .unwrap();

    let detail_paths: Vec<&str> = result
        .files
        .iter()
        .filter(|file| file.tier == Tier::Detail)
        .map(|file| file.path.as_str())
        .collect();
    let owner = detail_paths
        .iter()
        .position(|path| *path == "src/flask/app.py")
        .expect("the task-owning app.py must be selected for DETAIL");
    let example_indices: Vec<usize> = [
        "examples/celery/src/task_app/views.py",
        "examples/javascript/js_example/views.py",
    ]
    .iter()
    .filter_map(|example| detail_paths.iter().position(|path| path == example))
    .collect();
    assert!(
        example_indices.iter().all(|index| owner < *index),
        "owner must precede lower-priority examples: {detail_paths:?}"
    );

    let detail_headers: Vec<&str> = result
        .text
        .split("## DETAIL")
        .nth(1)
        .unwrap_or("")
        .lines()
        .filter(|line| line.contains(" [provenance="))
        .collect();
    let owner_header = detail_headers
        .iter()
        .position(|line| line.starts_with("src/flask/app.py "))
        .expect("the owner must render a DETAIL block");
    let example_headers: Vec<usize> = detail_headers
        .iter()
        .enumerate()
        .filter_map(|(index, line)| line.starts_with("examples/").then_some(index))
        .collect();
    assert!(
        example_headers.iter().all(|index| owner_header < *index),
        "rendered DETAIL must put the owner before examples: {detail_headers:?}"
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

/// GAP-084: `understand` output is a clean contract — no format template
/// shipped as content, every tier header carries a body, and the budget knob
/// is observable (non-decreasing size, 200 vs 40000 measurably different).
#[test]
fn test_signal_understand_output_contract_gap084() {
    let db = GraphDB::open(":memory:").unwrap();
    let edges: Vec<Edge> = (0..12)
        .map(|i| edge(&format!("src/auth/mod{i}.go"), "src/auth/lib.go", "imports"))
        .chain(std::iter::once(edge(
            "src/auth/lib.go",
            "src/main.go",
            "imports",
        )))
        .collect();
    db.insert_edges(&edges).unwrap();

    // 48 functions per file → one DETAIL block is ≈3 KB, so a small budget
    // cannot fit even the first block.
    let reader = |path: &str| -> Option<String> {
        if !path.ends_with(".go") {
            return None;
        }
        let mut src = String::from("package fixture\n\n");
        for i in 0..48 {
            src.push_str(&format!(
                "func Handler{i}(ctx context.Context) error {{\n    return nil\n}}\n\n"
            ));
        }
        Some(src)
    };

    let text_at = |budget: usize| -> String {
        understand_with_source(
            &db,
            "auth",
            &SignalOpts {
                token_budget: budget,
                ..Default::default()
            },
            Some(reader),
        )
        .unwrap()
        .text
    };

    let small = text_at(200);
    let big = text_at(40000);

    // 1. No format template ever ships as content.
    for line in small.lines().chain(big.lines()) {
        assert!(
            !line.starts_with('<') && !line.starts_with("  - <"),
            "placeholder-shaped line leaked: {line:?}"
        );
        assert!(!line.contains("<file>"), "template token leaked: {line:?}");
    }

    // 2. Every tier header carries a body — rows or the explicit empty marker.
    for text in [&small, &big] {
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if line.starts_with("## ") {
                let body = lines.get(i + 1).map(|l| l.trim()).unwrap_or("");
                assert!(!body.is_empty(), "bare tier header {line:?}");
            }
        }
    }
    assert!(
        small.contains("(no files in this tier"),
        "budget 200 must state why DETAIL is empty:\n{small}"
    );
    assert!(
        big.contains(" [provenance="),
        "budget 40000 must render real detail blocks"
    );

    // 3. Budget semantics: non-decreasing size, 200 != 40000.
    let mut prev = 0usize;
    for budget in [1usize, 200, 6000, 40000] {
        let size = text_at(budget).len();
        assert!(
            size >= prev,
            "budget {budget} shrank the output: {size} < {prev}"
        );
        prev = size;
    }
    assert!(
        big.len() > small.len(),
        "budget 40000 ({}) must differ from budget 200 ({})",
        big.len(),
        small.len()
    );

    // 4. Flat resolution stays budget-independent (documented on Resolution).
    let flat_at = |budget: usize| -> String {
        understand_with_source(
            &db,
            "auth",
            &SignalOpts {
                token_budget: budget,
                resolution: Resolution::Flat,
                ..Default::default()
            },
            Some(reader),
        )
        .unwrap()
        .text
    };
    assert_eq!(
        flat_at(200),
        flat_at(40000),
        "flat resolution ignores the budget by design"
    );
}
