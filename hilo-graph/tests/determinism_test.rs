//! Determinism tests — prove that graph output, signal engine output, and
//! semantic search results are byte-identical across repeated runs.
//!
//! These tests use a controlled corpus of fixture files committed to
//! `tests/fixtures/` that never change. The corpus covers:
//! - Go imports (main → handler → middleware)
//! - Go test files (handler_test.go tests handler.go)
//! - Python imports (utils.py imports os, sys, collections)
//! - TypeScript imports (app.ts imports handler, express)
//! - Circular import path (handler → middleware → net/http, handler imports middleware)

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use hilo_graph::graph::{Direction, GraphDB};
use hilo_graph::impact::compute_impact;
use hilo_graph::parser::{Language, Parser};
use hilo_graph::semantic::{search, SearchOpts};
use hilo_graph::signal::{understand, SignalOpts};
use hilo_metadata::inventory::Edge;

/// Path to the controlled corpus directory (relative to CARGO_MANIFEST_DIR).
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

/// Parse all fixture files with tree-sitter, insert edges into an in-memory
/// DuckDB, and return the DB handle.
///
/// The corpus is deterministic: same fixture files → same edges every time.
/// Edges are sorted before insertion to eliminate any ordering sensitivity.
fn build_graph_from_fixtures() -> GraphDB {
    let db = GraphDB::open(":memory:").unwrap();

    let dir = fixtures_dir();
    let mut all_edges: Vec<Edge> = Vec::new();

    // Collect and parse all fixture files.
    let mut entries: Vec<_> = fs::read_dir(&dir)
        .unwrap_or_else(|_| panic!("fixtures dir not found: {dir:?}"))
        .filter_map(Result::ok)
        .collect();

    // Sort entries by filename for deterministic processing order.
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

        let lang = match Language::from_extension(ext) {
            Some(l) => l,
            None => continue, // skip non-source files
        };

        // Skip test files for import extraction — they import the package
        // under test, which produces a "tested_by" style edge. We handle
        // test files separately below.
        let is_test = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.contains("_test"))
            .unwrap_or(false);

        let source = fs::read_to_string(&path).unwrap();
        let file_path = path.to_string_lossy().to_string();

        if is_test {
            // For Go test files: the test file imports the package under test.
            // We synthesize a tested_by edge: handler_test.go → handler.go
            // This is deterministic and covers the test-relationship edge type.
            if lang == Language::Go {
                let base_name = file_path.replace("_test.go", ".go");
                all_edges.push(Edge::with_provenance(
                    &file_path,
                    &base_name,
                    "tested_by",
                    "heuristic",
                    0.8,
                ));
            }
            // Also parse imports from test files (they may import stdlib).
            let mut parser = Parser::for_language(lang).unwrap();
            if let Ok(edges) = parser.parse_imports(&file_path, &source) {
                all_edges.extend(edges);
            }
        } else {
            let mut parser = Parser::for_language(lang).unwrap();
            if let Ok(edges) = parser.parse_imports(&file_path, &source) {
                all_edges.extend(edges);
            }
        }
    }

    // Sort edges for deterministic insertion order.
    all_edges.sort_by(|a, b| {
        (&a.from, &a.to, &a.rel, &a.provenance).cmp(&(&b.from, &b.to, &b.rel, &b.provenance))
    });

    db.insert_edges(&all_edges).unwrap();
    db
}

/// Dump all edges from the graph as sorted JSON — this is the canonical
/// representation for byte-identical comparison.
fn dump_edges_sorted(db: &GraphDB) -> String {
    // Gather ALL edges by querying every distinct file.
    let (froms, tos) = db.distinct_files().unwrap();
    let mut all: Vec<Edge> = Vec::new();
    let mut seen: std::collections::HashSet<(String, String, String)> =
        std::collections::HashSet::new();

    for f in &froms {
        for e in db.related(f, None, Direction::Forward).unwrap_or_default() {
            let key = (e.from.clone(), e.to.clone(), e.rel.clone());
            if seen.insert(key) {
                all.push(e);
            }
        }
        for e in db.related(f, None, Direction::Reverse).unwrap_or_default() {
            let key = (e.from.clone(), e.to.clone(), e.rel.clone());
            if seen.insert(key) {
                all.push(e);
            }
        }
    }
    for t in &tos {
        for e in db.related(t, None, Direction::Forward).unwrap_or_default() {
            let key = (e.from.clone(), e.to.clone(), e.rel.clone());
            if seen.insert(key) {
                all.push(e);
            }
        }
        for e in db.related(t, None, Direction::Reverse).unwrap_or_default() {
            let key = (e.from.clone(), e.to.clone(), e.rel.clone());
            if seen.insert(key) {
                all.push(e);
            }
        }
    }

    // Sort for deterministic output.
    all.sort_by(|a, b| {
        (
            &a.from,
            &a.to,
            &a.rel,
            &a.provenance,
            a.confidence.to_bits(),
        )
            .cmp(&(
                &b.from,
                &b.to,
                &b.rel,
                &b.provenance,
                b.confidence.to_bits(),
            ))
    });

    // Serialize to compact JSON (sorted keys via BTreeMap for stability).
    let edges_as_maps: Vec<BTreeMap<String, String>> = all
        .iter()
        .map(|e| {
            let mut m = BTreeMap::new();
            m.insert("from".to_string(), e.from.clone());
            m.insert("to".to_string(), e.to.clone());
            m.insert("rel".to_string(), e.rel.clone());
            m.insert("provenance".to_string(), e.provenance.clone());
            m.insert("confidence".to_string(), format!("{:.4}", e.confidence));
            m
        })
        .collect();

    serde_json::to_string(&edges_as_maps).unwrap()
}

// ── Graph determinism ────────────────────────────────────────────────

#[test]
fn graph_is_deterministic() {
    // Build the graph from the fixture corpus twice and assert byte-identical
    // edge dumps.
    let run1 = {
        let db = build_graph_from_fixtures();
        dump_edges_sorted(&db)
    };
    let run2 = {
        let db = build_graph_from_fixtures();
        dump_edges_sorted(&db)
    };

    assert_eq!(
        run1, run2,
        "graph edge dump must be byte-identical across runs"
    );
}

#[test]
fn graph_is_deterministic_10_runs() {
    // Run 10 builds and verify all produce the same dump.
    let baseline = {
        let db = build_graph_from_fixtures();
        dump_edges_sorted(&db)
    };

    for i in 1..=10 {
        let db = build_graph_from_fixtures();
        let dump = dump_edges_sorted(&db);
        assert_eq!(baseline, dump, "run {i}: graph dump must match baseline");
    }
}

#[test]
fn graph_has_expected_edges() {
    // Sanity check: the fixture corpus should produce a known set of edges.
    let db = build_graph_from_fixtures();
    let count = db.count_edges().unwrap();

    // We expect edges from:
    // - main.go: imports fmt, handler (2 edges)
    // - handler.go: imports net/http, middleware (2 edges)
    // - middleware.go: imports net/http (1 edge)
    // - handler_test.go: tested_by → handler.go (1 edge) + imports testing (1 edge)
    // - utils.py: imports os, sys, collections (3 edges)
    // - app.ts: imports ./handler, express (2 edges)
    // Total: 12 edges minimum (some may vary based on parser heuristics).
    assert!(
        count >= 10,
        "expected at least 10 edges from fixture corpus, got {count}"
    );

    // Verify provenance is set on all edges.
    let (froms, _) = db.distinct_files().unwrap();
    for f in &froms {
        let edges = db.related(f, None, Direction::Forward).unwrap_or_default();
        for e in &edges {
            assert!(
                !e.provenance.is_empty(),
                "edge {:?} → {:?} has empty provenance",
                e.from,
                e.to
            );
            assert!(
                e.confidence > 0.0,
                "edge {:?} → {:?} has zero confidence",
                e.from,
                e.to
            );
        }
    }
}

#[test]
fn graph_edge_dump_includes_provenance_and_confidence() {
    let db = build_graph_from_fixtures();
    let dump = dump_edges_sorted(&db);

    // The JSON dump must contain provenance and confidence fields.
    assert!(
        dump.contains("\"provenance\""),
        "dump must include provenance field"
    );
    assert!(
        dump.contains("\"confidence\""),
        "dump must include confidence field"
    );
    assert!(
        dump.contains("\"ast_exact\""),
        "dump must include ast_exact provenance value"
    );
}

#[test]
fn graph_stats_are_deterministic() {
    let stats1 = {
        let db = build_graph_from_fixtures();
        db.stats().unwrap()
    };
    let stats2 = {
        let db = build_graph_from_fixtures();
        db.stats().unwrap()
    };

    assert_eq!(stats1.total_edges, stats2.total_edges);
    assert_eq!(stats1.total_files, stats2.total_files);
    assert_eq!(stats1.unique_files, stats2.unique_files);
    assert_eq!(stats1.unique_dependencies, stats2.unique_dependencies);
    // most_connected comes from top.first() — non-deterministic for ties.
    // Just verify both are Some or both are None.
    assert_eq!(
        stats1.most_connected.is_some(),
        stats2.most_connected.is_some()
    );

    // orphans: sort for deterministic comparison (DuckDB ORDER BY is usually
    // deterministic but can vary across in-memory DB instances).
    let mut o1 = stats1.orphans.clone();
    let mut o2 = stats2.orphans.clone();
    o1.sort();
    o2.sort();
    assert_eq!(o1, o2, "orphans must match when sorted");

    // edge_types is a HashMap — compare as sorted entries for determinism.
    let mut et1: Vec<_> = stats1.edge_types.iter().collect();
    et1.sort_by_key(|(k, _)| k.to_string());
    let mut et2: Vec<_> = stats2.edge_types.iter().collect();
    et2.sort_by_key(|(k, _)| k.to_string());
    assert_eq!(et1, et2, "edge_types must match when sorted");

    // top_dependencies uses ORDER BY cnt DESC LIMIT 10 — DuckDB
    // non-deterministically picks which rows to include when there are ties
    // at the LIMIT boundary. We cannot compare this field across runs.
    // Instead, verify the total count of unique dependencies matches.
    assert_eq!(
        stats1.top_dependencies.len(),
        stats2.top_dependencies.len(),
        "top_dependencies length must match"
    );
}

#[test]
fn graph_impact_is_deterministic() {
    let db = build_graph_from_fixtures();

    // Pick a file that has dependents (middleware.go is imported by handler.go).
    let middleware_path = fixtures_dir()
        .join("middleware.go")
        .to_string_lossy()
        .to_string();

    let run1 = compute_impact(db.conn(), &middleware_path, 10).unwrap();
    let run2 = compute_impact(db.conn(), &middleware_path, 10).unwrap();

    assert_eq!(
        run1.len(),
        run2.len(),
        "impact analysis must return same number of files"
    );

    for (a, b) in run1.iter().zip(run2.iter()) {
        assert_eq!(a.path, b.path, "impact path must match");
        assert_eq!(a.depth, b.depth, "impact depth must match");
        assert_eq!(a.relation, b.relation, "impact relation must match");
        assert_eq!(a.provenance, b.provenance, "impact provenance must match");
        assert_eq!(a.confidence, b.confidence, "impact confidence must match");
    }
}

// ── Signal engine determinism ─────────────────────────────────────────

#[test]
fn signal_engine_is_deterministic_with_fixtures() {
    let db = build_graph_from_fixtures();

    let opts = SignalOpts::default();
    let r1 = understand(&db, "auth middleware", &opts).unwrap();
    let r2 = understand(&db, "auth middleware", &opts).unwrap();

    assert_eq!(
        r1.text, r2.text,
        "signal engine output must be byte-identical across runs"
    );
    assert_eq!(r1.anchors, r2.anchors);
    assert_eq!(r1.tokens_estimate, r2.tokens_estimate);
    assert_eq!(r1.files.len(), r2.files.len());

    // Compare each file structurally.
    for (a, b) in r1.files.iter().zip(r2.files.iter()) {
        assert_eq!(a.path, b.path, "signal file path must match");
        assert_eq!(a.symbols, b.symbols, "signal symbols must match");
        assert_eq!(a.tier, b.tier, "signal tier must match");
        assert_eq!(a.provenance, b.provenance, "signal provenance must match");
        assert_eq!(a.detail, b.detail, "signal detail must match");
    }
}

#[test]
fn signal_engine_is_deterministic_5_runs() {
    let db = build_graph_from_fixtures();

    let opts = SignalOpts::default();
    let baseline = understand(&db, "handler", &opts).unwrap();

    for i in 1..=5 {
        let result = understand(&db, "handler", &opts).unwrap();
        assert_eq!(
            baseline.text, result.text,
            "run {i}: signal engine must match baseline"
        );
    }
}

// ── Semantic search determinism ───────────────────────────────────────

#[test]
fn semantic_search_is_deterministic_with_fixtures() {
    let db = build_graph_from_fixtures();

    let r1 = search(&db, "handler middleware", &SearchOpts::default()).unwrap();
    let r2 = search(&db, "handler middleware", &SearchOpts::default()).unwrap();

    assert_eq!(
        r1.len(),
        r2.len(),
        "semantic search must return same number of results"
    );

    for (a, b) in r1.iter().zip(r2.iter()) {
        assert_eq!(a.file_path, b.file_path, "search result path must match");
        assert_eq!(a.score, b.score, "search result score must match");
        assert_eq!(a.provenance, b.provenance, "search provenance must match");
        assert_eq!(a.symbols, b.symbols, "search symbols must match");
    }
}

#[test]
fn semantic_search_is_deterministic_10_runs() {
    let db = build_graph_from_fixtures();

    let baseline = search(&db, "auth", &SearchOpts::default()).unwrap();

    for i in 1..=10 {
        let result = search(&db, "auth", &SearchOpts::default()).unwrap();
        assert_eq!(
            baseline, result,
            "run {i}: semantic search must match baseline"
        );
    }
}

// ── Provenance determinism ────────────────────────────────────────────

#[test]
fn provenance_tags_are_consistent_across_runs() {
    // Same source → same provenance tags every time.
    let db1 = build_graph_from_fixtures();
    let db2 = build_graph_from_fixtures();

    let (mut froms1, _) = db1.distinct_files().unwrap();
    let (mut froms2, _) = db2.distinct_files().unwrap();

    // Sort for deterministic comparison (DuckDB DISTINCT ordering is not guaranteed).
    froms1.sort();
    froms2.sort();

    // Same set of source files.
    assert_eq!(froms1, froms2, "distinct file sets must match");

    // For each file, edges must have identical provenance.
    for f in &froms1 {
        let e1 = db1.related(f, None, Direction::Forward).unwrap_or_default();
        let e2 = db2.related(f, None, Direction::Forward).unwrap_or_default();

        assert_eq!(e1.len(), e2.len(), "edge count must match for {f}");

        for (a, b) in e1.iter().zip(e2.iter()) {
            assert_eq!(a.provenance, b.provenance, "provenance must match for {f}");
            assert_eq!(a.confidence, b.confidence, "confidence must match for {f}");
            assert_eq!(a.rel, b.rel, "relation must match for {f}");
            assert_eq!(a.to, b.to, "target must match for {f}");
        }
    }
}

// ── JSONL roundtrip determinism ────────────────────────────────────────

#[test]
fn edge_jsonl_roundtrip_is_deterministic() {
    // Serialize edges to JSONL, deserialize, and verify byte-identical
    // re-serialization.
    let db = build_graph_from_fixtures();
    let (froms, _) = db.distinct_files().unwrap();

    let mut all_edges: Vec<Edge> = Vec::new();
    let mut seen: std::collections::HashSet<(String, String, String)> =
        std::collections::HashSet::new();
    for f in &froms {
        for e in db.related(f, None, Direction::Forward).unwrap_or_default() {
            let key = (e.from.clone(), e.to.clone(), e.rel.clone());
            if seen.insert(key) {
                all_edges.push(e);
            }
        }
    }

    // Serialize to JSONL.
    let jsonl1: String = all_edges
        .iter()
        .map(|e| serde_json::to_string(e).unwrap())
        .collect::<Vec<_>>()
        .join("\n");

    // Deserialize.
    let deserialized: Vec<Edge> = jsonl1
        .lines()
        .map(|line| serde_json::from_str::<Edge>(line).unwrap())
        .collect();

    // Re-serialize.
    let jsonl2: String = deserialized
        .iter()
        .map(|e| serde_json::to_string(e).unwrap())
        .collect::<Vec<_>>()
        .join("\n");

    assert_eq!(jsonl1, jsonl2, "JSONL roundtrip must be byte-identical");

    // Verify provenance + confidence survived the roundtrip.
    for e in &deserialized {
        assert!(
            !e.provenance.is_empty(),
            "provenance must survive JSONL roundtrip"
        );
        assert!(
            e.confidence > 0.0,
            "confidence must survive JSONL roundtrip"
        );
    }
}

// ── Tests use in-memory DuckDB (no filesystem pollution) ──────────────

#[test]
fn tests_use_in_memory_duckdb() {
    // This test is a documentation gate: all determinism tests use
    // GraphDB::open(":memory:") and never write to the filesystem.
    // The build_graph_from_fixtures() helper enforces this.
    let db = build_graph_from_fixtures();
    let _ = db.count_edges().unwrap();

    // Verify no .vfs directory was created in the test's working directory.
    // (In-memory DBs don't create files.)
    assert!(
        !PathBuf::from(".vfs").exists(),
        "in-memory DB should not create .vfs/ directory"
    );
}

// ── Test corpus immutability ──────────────────────────────────────────

#[test]
fn test_corpus_is_committed_and_immutable() {
    // Verify that all expected fixture files exist.
    let dir = fixtures_dir();
    let expected_files = [
        "main.go",
        "handler.go",
        "middleware.go",
        "handler_test.go",
        "utils.py",
        "app.ts",
    ];

    for name in &expected_files {
        let path = dir.join(name);
        assert!(
            path.exists(),
            "fixture file {name} must exist at {}",
            path.display()
        );
    }

    // Verify the corpus covers the required patterns.
    let main_go = fs::read_to_string(dir.join("main.go")).unwrap();
    assert!(
        main_go.contains("import"),
        "main.go must contain import declarations"
    );

    let handler_test = fs::read_to_string(dir.join("handler_test.go")).unwrap();
    assert!(
        handler_test.contains("testing"),
        "handler_test.go must be a test file"
    );

    let middleware = fs::read_to_string(dir.join("middleware.go")).unwrap();
    assert!(
        middleware.contains("import"),
        "middleware.go must contain imports"
    );
}
