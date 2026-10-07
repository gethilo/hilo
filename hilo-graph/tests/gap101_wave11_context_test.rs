//! GAP-101 — Wave 11 context-pack required-path recall (integration).
//!
//! The Wave 11 six-repository bake-off scored `graph understand` context packs
//! by *required-path recall*: the fraction of a case's pinned truth paths whose
//! exact string appears anywhere in the pack output. Hilo's mean recall was
//! 19.3% (0/2 Pydantic TypeAdapter, 0/5 Ruff B905, 2/6 Svelte keyed-each, 5/8
//! Deno CLI/worker, 1/5 Spring Boot auto-configuration, 0/4 gRPC Alarm).
//!
//! The fixtures under `tests/fixtures/wave11-context/` pin each case to its
//! real prompt, its real required paths at the pinned repository SHA, and a
//! reduced-but-real neighbourhood of that repository (real competing paths and
//! the real import edges the baked graph holds among them). This test is the
//! same law the lib test enforces, over the public entry point.
//!
//! It complements `signal.rs`'s unit coverage of the domain-object module
//! boost: that one also measures the pre-fix pack on the same graphs, this one
//! pins the shipped behaviour end to end.

use std::collections::HashSet;

use hilo_graph::signal::{understand, SignalOpts};
use hilo_graph::Edge;
use hilo_graph::GraphDB;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Fixture {
    id: String,
    repo: String,
    repo_sha: String,
    prompt: String,
    truth_paths: Vec<String>,
    files: Vec<String>,
    edges: Vec<(String, String)>,
}

/// Every Wave 11 context-pack case, in the bake-off's report order.
const IDS: [&str; 6] = [
    "pydantic.type-adapter-validation",
    "ruff.b905-context",
    "svelte.keyed-each-context",
    "deno.cli-subcommand-worker",
    "spring-boot.autoconfig-context",
    "grpc.alarm-fastpath-context",
];

/// The required-path recall the boost must clear (AC3).
const MEAN_RECALL_FLOOR: f64 = 0.50;

/// The Deno case's pre-fix hit count, which must not regress (AC4).
const DENO_HITS_FLOOR: usize = 5;

fn load(id: &str) -> Fixture {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/wave11-context")
        .join(format!("{id}.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("wave11 fixture {} unreadable: {e}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("wave11 fixture {} malformed: {e}", path.display()))
}

fn graph(fixture: &Fixture) -> GraphDB {
    let db = GraphDB::open(":memory:").expect("in-memory graph");
    let edges: Vec<Edge> = fixture
        .edges
        .iter()
        .map(|(from, to)| Edge::new(from.clone(), to.clone(), "imports"))
        .collect();
    db.insert_edges(&edges).expect("insert fixture edges");
    db
}

/// Required-path recall of the shipped pack for one case.
fn recall(fixture: &Fixture) -> (usize, usize) {
    let db = graph(fixture);
    let opts = SignalOpts {
        token_budget: 6000,
        ..Default::default()
    };
    let result = understand(&db, &fixture.prompt, &opts).expect("understand");
    let hits = fixture
        .truth_paths
        .iter()
        .filter(|path| result.text.contains(path.as_str()))
        .count();
    (hits, fixture.truth_paths.len())
}

#[test]
fn wave11_context_pack_mean_required_path_recall_clears_the_bar() {
    let mut total = 0.0;
    let mut rows: Vec<String> = Vec::new();
    for id in IDS {
        let fixture = load(id);
        assert!(
            !fixture.truth_paths.is_empty(),
            "{id}: fixture pins no required paths"
        );
        assert!(
            fixture.repo_sha.len() == 40,
            "{id}: fixture must pin a full 40-hex repository SHA, got {}",
            fixture.repo_sha
        );
        // Every required path must be a real path of the pinned repo.
        let known: std::collections::HashSet<&str> =
            fixture.files.iter().map(String::as_str).collect();
        for path in &fixture.truth_paths {
            assert!(
                known.contains(path.as_str()),
                "{id}: required path {path} is not in the fixture file set"
            );
        }
        let (hits, required) = recall(&fixture);
        let case_recall = hits as f64 / required as f64;
        total += case_recall;
        rows.push(format!(
            "  {} ({}, sha {}) {hits}/{required} = {:.0}%",
            fixture.id,
            fixture.repo,
            &fixture.repo_sha[..12],
            case_recall * 100.0
        ));
    }
    let mean = total / IDS.len() as f64;
    assert!(
        mean >= MEAN_RECALL_FLOOR,
        "Wave 11 context-pack mean required-path recall {:.1}% < {:.0}%\n{}",
        mean * 100.0,
        MEAN_RECALL_FLOOR * 100.0,
        rows.join("\n")
    );
}

#[test]
fn wave11_deno_context_pack_keeps_its_strong_case() {
    let fixture = load("deno.cli-subcommand-worker");
    assert_eq!(fixture.truth_paths.len(), 8, "Deno pins eight paths");
    let (hits, required) = recall(&fixture);
    assert_eq!(required, 8);
    assert!(
        hits >= DENO_HITS_FLOOR,
        "Deno required-path recall regressed below {DENO_HITS_FLOOR}/8: {hits}/{required}"
    );
}

#[test]
fn wave11_fixtures_are_self_consistent() {
    // A fixture whose files are not all reachable through its edges cannot be
    // queried: the graph derives its file set from edge endpoints only.
    for id in IDS {
        let fixture = load(id);
        let endpoints: HashSet<&str> = fixture
            .edges
            .iter()
            .flat_map(|(from, to)| [from.as_str(), to.as_str()])
            .collect();
        for path in &fixture.files {
            assert!(
                endpoints.contains(path.as_str()),
                "{id}: file {path} has no edge endpoint, so the graph cannot see it"
            );
        }
    }
}
