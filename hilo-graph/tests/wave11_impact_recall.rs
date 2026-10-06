//! GAP-117 — Wave 11 impact-recall fixtures and rubric.
//!
//! The Wave 11 six-repository bake-off scored Hilo's `graph impact` +
//! `graph understand` workflow at 26.7% required-path recall on the six
//! `impact-tests` cases, against CodeGraph's 59.4% — while Hilo led on the
//! narrower build-target subset (2/3 eligible, 66.7%). This file locks the six
//! truth sets as deterministic fixtures and re-scores the pipeline with the
//! *same* rubric the report used.
//!
//! The fixtures under `tests/fixtures/wave11-impact/cases.jsonl` carry, per
//! case:
//!
//! * the pinned `changed_path`, the required `truth_paths`, the `test_paths`
//!   and the `test_build_paths` (the runner/build target), exactly as recorded
//!   by the report's case file;
//! * `wave11_baseline_hits` — the paths the report's run actually recalled, so
//!   the 26.7% baseline is reproduced rather than asserted from memory;
//! * a `source_path_kinds` / `test_path_kinds` partition that keeps
//!   fixture/config files distinct from the test runner and the build target
//!   (GAP-117 AC1);
//! * the edge set of a reduced graph built from the real relation structure
//!   the case's evidence URLs describe (imports, tests/tested_by links, the
//!   BUILD target that names the test), plus decoy files so an answer is not
//!   trivially "everything".
//!
//! Scoring mirrors `score_wave11_full.py`: recall is
//! `|set(hits) ∩ set(targets)| / |set(targets)|`, where a hit is a truth path
//! appearing as a literal substring of the retained output text.
//!
//! Raw outputs (per-case review-set JSON, legacy dependent-set JSON, and the
//! task-context text) are retained under `<target>/wave11-impact/` so the same
//! rubric can be re-run by `tests/fixtures/wave11-impact/score_wave11_impact.py`
//! (AC4).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use hilo_graph::graph::GraphDB;
use hilo_graph::impact::{compute_impact, compute_review_set, ImpactResult, SCOPE_SELF};
use hilo_graph::signal::{understand_with_source, SignalOpts};
use hilo_metadata::inventory::Edge;
use serde::Deserialize;

/// One locked Wave 11 `impact-tests` case.
#[derive(Debug, Deserialize)]
struct Case {
    id: String,
    repo: String,
    changed_path: String,
    prompt: String,
    truth_paths: Vec<String>,
    test_paths: Vec<String>,
    test_build_paths: Vec<String>,
    /// Truth paths that are not test paths, classified
    /// `changed_source` / `dependency` / `dependent`.
    source_path_kinds: HashMap<String, String>,
    /// Test/build paths, classified `test_runner` / `test_fixture` /
    /// `test_config` / `build_target`.
    test_path_kinds: HashMap<String, String>,
    /// What the report's Wave 11 run actually recalled for this case.
    wave11_baseline_hits: Vec<String>,
    edges: Vec<EdgeSpec>,
}

#[derive(Debug, Deserialize)]
struct EdgeSpec {
    from: String,
    to: String,
    rel: String,
}

/// The report's measured baseline: `hilo` `impact-tests` mean path recall.
const WAVE11_BASELINE_MEAN_PATH_RECALL: f64 = 0.266_666_666_666_666_66;
/// GAP-117 AC2 floor.
const MIN_MEAN_PATH_RECALL: f64 = 0.50;
/// GAP-117 AC3 floor on the eligible build-target paths (2/3).
const MIN_BUILD_PATH_RECALL: f64 = 2.0 / 3.0;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("wave11-impact")
        .join("cases.jsonl")
}

/// Directory the raw per-case artifacts are retained in.
fn raw_output_dir() -> PathBuf {
    let base = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("target")
        });
    base.join("wave11-impact")
}

fn load_cases() -> Vec<Case> {
    let path = fixture_path();
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {e}", path.display()));
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            hilo_graph::serde_json::from_str(l)
                .unwrap_or_else(|e| panic!("failed to parse fixture line: {e}\n{l}"))
        })
        .collect()
}

/// Comma-free source reader — the fixtures carry no source text, so anchor
/// ranking runs on the graph shape and tier rendering prints paths only.
fn no_source(_path: &str) -> Option<String> {
    None
}

/// `score_wave11_full.py` hit test: a target is recalled when it appears as a
/// literal substring of the output text.
fn hits(text: &str, targets: &[String]) -> Vec<String> {
    targets
        .iter()
        .filter(|t| !t.is_empty() && text.contains(t.as_str()))
        .cloned()
        .collect()
}

/// `score_wave11_full.py` recall: `|set(hits) ∩ set(targets)| / |set(targets)|`.
/// `None` for an empty target set (the report's `—`, not a zero).
fn recall(hits: &[String], targets: &[String]) -> Option<f64> {
    let targets: HashSet<&String> = targets.iter().collect();
    if targets.is_empty() {
        return None;
    }
    let hit: HashSet<&String> = hits.iter().filter(|h| targets.contains(h)).collect();
    Some(hit.len() as f64 / targets.len() as f64)
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

fn mean_present(values: &[Option<f64>]) -> (f64, usize) {
    let present: Vec<f64> = values.iter().flatten().copied().collect();
    (
        present.iter().sum::<f64>() / present.len() as f64,
        present.len(),
    )
}

/// AC1: the locked fixtures must keep fixture/config files distinct from the
/// test runner and the build target, and must classify every required path.
fn assert_kind_partition(case: &Case) {
    let err = |msg: String| panic!("{}: {msg}", case.id);

    for p in &case.truth_paths {
        let known = case.source_path_kinds.contains_key(p) || case.test_path_kinds.contains_key(p);
        if !known {
            err(format!("truth path {p} has no kind classification"));
        }
    }
    let changed_kind = case
        .source_path_kinds
        .get(&case.changed_path)
        .map(String::as_str);
    if changed_kind != Some("changed_source") {
        err(format!(
            "changed_path {} must be classified changed_source, got {changed_kind:?}",
            case.changed_path
        ));
    }
    for p in &case.test_paths {
        let kind = case
            .test_path_kinds
            .get(p)
            .unwrap_or_else(|| panic!("{}: test path {p} has no test_path_kinds entry", case.id));
        if !matches!(
            kind.as_str(),
            "test_runner" | "test_fixture" | "test_config"
        ) {
            err(format!("test path {p} has invalid kind {kind}"));
        }
    }
    for p in &case.test_build_paths {
        let kind = case.test_path_kinds.get(p).unwrap_or_else(|| {
            panic!("{}: build target {p} has no test_path_kinds entry", case.id)
        });
        if !matches!(kind.as_str(), "test_runner" | "build_target") {
            err(format!(
                "build target {p} must be a runner or a build_target, got {kind}"
            ));
        }
    }
    // The load-bearing distinction: a fixture/config file is never the thing
    // you build or run.
    for (p, kind) in &case.test_path_kinds {
        if matches!(kind.as_str(), "test_fixture" | "test_config")
            && case.test_build_paths.contains(p)
        {
            err(format!(
                "fixture/config file {p} must not be listed as a build target"
            ));
        }
    }
}

/// AC3: an answer for a real subject is never an empty success — the subject
/// row leads, so a consumer can always name the file the answer is about.
#[test]
fn impact_review_set_always_names_its_subject() {
    let db = GraphDB::open(":memory:").unwrap();
    db.insert_edges(&[Edge::new(
        "cli/args/mod.rs",
        "cli/tools/test/mod.rs",
        "imports",
    )])
    .unwrap();

    let review = compute_review_set(db.conn(), "cli/tools/test/mod.rs", 10).unwrap();
    assert!(
        !review.is_empty(),
        "a real subject must never yield an empty set"
    );
    assert_eq!(review[0].scope, SCOPE_SELF);
    assert_eq!(review[0].path, "cli/tools/test/mod.rs");
    assert_eq!(review[0].depth, 0);

    // A subject with no in-graph neighbours still answers with itself.
    let isolated = compute_review_set(db.conn(), "cli/tools/test/orphan.rs", 10).unwrap();
    assert_eq!(isolated.len(), 1);
    assert_eq!(isolated[0].scope, SCOPE_SELF);

    // pkg:/sys: pseudo-nodes are not files and must keep the historical
    // symbol-node answer (dependents only, no fabricated subject row).
    let symbol = compute_review_set(db.conn(), "pkg:deno", 10).unwrap();
    assert!(
        symbol.iter().all(|r| r.scope != SCOPE_SELF),
        "a symbol node must not produce a self row: {symbol:?}"
    );
}

#[test]
fn wave11_impact_fixtures_meet_path_and_build_recall_targets() {
    let cases = load_cases();
    assert_eq!(
        cases.len(),
        6,
        "the six Wave 11 impact-tests cases are locked"
    );

    let out_dir = raw_output_dir();
    std::fs::create_dir_all(&out_dir)
        .unwrap_or_else(|e| panic!("failed to create {}: {e}", out_dir.display()));

    let mut baseline_path_recalls: Vec<f64> = Vec::new();
    let mut legacy_path_recalls: Vec<f64> = Vec::new();
    let mut review_path_recalls: Vec<f64> = Vec::new();
    let mut combined_path_recalls: Vec<f64> = Vec::new();
    let mut legacy_test_recalls: Vec<Option<f64>> = Vec::new();
    let mut review_test_recalls: Vec<Option<f64>> = Vec::new();
    let mut legacy_build_recalls: Vec<Option<f64>> = Vec::new();
    let mut review_build_recalls: Vec<Option<f64>> = Vec::new();
    let mut per_case_report: Vec<hilo_graph::serde_json::Value> = Vec::new();

    for case in &cases {
        assert_kind_partition(case);

        let db = GraphDB::open(":memory:").unwrap();
        let edges: Vec<Edge> = case
            .edges
            .iter()
            .map(|e| Edge::new(e.from.clone(), e.to.clone(), e.rel.clone()))
            .collect();
        db.insert_edges(&edges).unwrap();

        // The pipeline under test.
        let review = compute_review_set(db.conn(), &case.changed_path, 10).unwrap();
        assert!(
            !review.is_empty(),
            "{}: review set must never be empty",
            case.id
        );
        assert_eq!(
            review[0].scope, SCOPE_SELF,
            "{}: subject row must lead",
            case.id
        );
        assert_eq!(
            review[0].path, case.changed_path,
            "{}: subject row path",
            case.id
        );
        let result = ImpactResult {
            subject: case.changed_path.clone(),
            total: review.len(),
            files: review.clone(),
        };
        let review_json = hilo_graph::serde_json::to_string_pretty(&result).unwrap();

        // The pre-change pipeline: reverse-only dependents, no subject row.
        let legacy = compute_impact(db.conn(), &case.changed_path, 10).unwrap();
        let legacy_json = hilo_graph::serde_json::to_string(&legacy).unwrap();

        // The task-context half of the scored workflow.
        let reader: fn(&str) -> Option<String> = no_source;
        let context =
            understand_with_source(&db, &case.prompt, &SignalOpts::default(), Some(reader))
                .unwrap()
                .text;
        // The report concatenated the impact command's output with the
        // understand command's; the review-set JSON is this pipeline's impact
        // output, so the combined text mirrors that pair.
        let combined_text = format!("{review_json}\n{context}");

        let baseline_hits = hits(&review_json, &case.wave11_baseline_hits).len();
        let baseline_recall = recall(&case.wave11_baseline_hits.clone(), &case.truth_paths)
            .expect("every case has truth paths");

        let legacy_hits = hits(&legacy_json, &case.truth_paths);
        let review_hits = hits(&review_json, &case.truth_paths);
        let combined_hits = hits(&combined_text, &case.truth_paths);
        let legacy_test_hits = hits(&legacy_json, &case.test_paths);
        let review_test_hits = hits(&review_json, &case.test_paths);
        let legacy_build_hits = hits(&legacy_json, &case.test_build_paths);
        let review_build_hits = hits(&review_json, &case.test_build_paths);

        baseline_path_recalls.push(baseline_recall);
        legacy_path_recalls.push(recall(&legacy_hits, &case.truth_paths).unwrap());
        review_path_recalls.push(recall(&review_hits, &case.truth_paths).unwrap());
        combined_path_recalls.push(recall(&combined_hits, &case.truth_paths).unwrap());
        legacy_test_recalls.push(recall(&legacy_test_hits, &case.test_paths));
        review_test_recalls.push(recall(&review_test_hits, &case.test_paths));
        legacy_build_recalls.push(recall(&legacy_build_hits, &case.test_build_paths));
        review_build_recalls.push(recall(&review_build_hits, &case.test_build_paths));

        // Retain the raw artifacts the rubric scores (AC4).
        std::fs::write(
            out_dir.join(format!("{}-review.json", case.id)),
            &review_json,
        )
        .unwrap();
        std::fs::write(
            out_dir.join(format!("{}-legacy.json", case.id)),
            &legacy_json,
        )
        .unwrap();
        std::fs::write(out_dir.join(format!("{}-context.txt", case.id)), &context).unwrap();

        per_case_report.push(hilo_graph::serde_json::json!({
            "id": case.id.clone(),
            "repo": case.repo.clone(),
            "changed_path": case.changed_path.clone(),
            "review_path_recall": recall(&review_hits, &case.truth_paths),
            "review_test_path_recall": recall(&review_test_hits, &case.test_paths),
            "review_build_path_recall": recall(&review_build_hits, &case.test_build_paths),
            "legacy_path_recall": recall(&legacy_hits, &case.truth_paths),
            "legacy_test_path_recall": recall(&legacy_test_hits, &case.test_paths),
            "legacy_build_path_recall": recall(&legacy_build_hits, &case.test_build_paths),
            "wave11_baseline_path_recall": baseline_recall,
            "wave11_baseline_hits": case.wave11_baseline_hits.clone(),
            "review_hits": review_hits.clone(),
            "summary": "review = GAP-117 review set; legacy = reverse-only compute_impact_at; wave11_baseline = the report's actual hits",
        }));

        // The locked baseline must reproduce the report's number per case, and
        // recall can never decrease (the review set is a superset of the
        // dependents and always adds the subject).
        assert!(
            *legacy_path_recalls.last().unwrap() <= *review_path_recalls.last().unwrap() + 1e-9,
            "{}: review recall must not fall below the legacy recall",
            case.id
        );
        assert!(
            baseline_recall <= *review_path_recalls.last().unwrap() + 1e-9,
            "{}: the review set must recall at least what Wave 11 recalled ({} vs {})",
            case.id,
            baseline_hits,
            review_hits.len()
        );
    }

    let baseline_mean = mean(&baseline_path_recalls);
    let legacy_mean = mean(&legacy_path_recalls);
    let review_mean = mean(&review_path_recalls);
    let combined_mean = mean(&combined_path_recalls);
    let (legacy_build_mean, eligible_build_cases) = mean_present(&legacy_build_recalls);
    let (review_build_mean, review_eligible_build_cases) = mean_present(&review_build_recalls);

    // AC4: the locked baseline reproduces the report's measured 26.7%.
    assert!(
        (baseline_mean - WAVE11_BASELINE_MEAN_PATH_RECALL).abs() < 1e-9,
        "locked Wave 11 baseline must reproduce the report's 26.7%, got {baseline_mean}"
    );

    // AC2: at least 50% of required paths, on the review set alone and on the
    // impact + task-context pair the report scored.
    assert!(
        review_mean >= MIN_MEAN_PATH_RECALL,
        "AC2: review-set mean path recall {review_mean} must be >= {MIN_MEAN_PATH_RECALL}"
    );
    assert!(
        combined_mean >= MIN_MEAN_PATH_RECALL,
        "AC2: impact + task-context mean path recall {combined_mean} must be >= {MIN_MEAN_PATH_RECALL}"
    );

    // The gain must be structural, not a fixture tautology: the review set has
    // to beat the reverse-only BFS on the same graphs.
    assert!(
        review_mean > legacy_mean,
        "the review set ({review_mean}) must recall strictly more than the reverse-only BFS ({legacy_mean})"
    );

    // AC3: retain at least 2/3 recall on the eligible build-target paths.
    assert_eq!(
        eligible_build_cases, 3,
        "three Wave 11 cases carry an eligible build-target path"
    );
    assert_eq!(review_eligible_build_cases, 3);
    assert!(
        review_build_mean >= MIN_BUILD_PATH_RECALL - 1e-9,
        "AC3: build-target recall {review_build_mean} must be >= {MIN_BUILD_PATH_RECALL}"
    );

    let summary = hilo_graph::serde_json::json!({
        "scoring_status": "gap-117-fixture-reduced-graphs",
        "rubric": "score_wave11_full.py (recall = |hits ∩ targets| / |targets| over literal substring hits)",
        "cases": cases.len(),
        "wave11_baseline_mean_path_recall": baseline_mean,
        "legacy_mean_path_recall": legacy_mean,
        "review_mean_path_recall": review_mean,
        "combined_mean_path_recall": combined_mean,
        "legacy_mean_test_path_recall": mean_present(&legacy_test_recalls).0,
        "review_mean_test_path_recall": mean_present(&review_test_recalls).0,
        "eligible_build_cases": eligible_build_cases,
        "legacy_mean_build_path_recall": legacy_build_mean,
        "review_mean_build_path_recall": review_build_mean,
        "per_case": per_case_report,
        "note": "Reduced fixture graphs, not the six real repositories: the legacy column is the same BFS run over the fixture edges, and the Wave 11 column is the report's recorded hits. Re-running on the pinned real repos is GAP-118.",
    });
    std::fs::write(
        out_dir.join("scores.json"),
        hilo_graph::serde_json::to_string_pretty(&summary).unwrap(),
    )
    .unwrap();

    assert!(
        review_mean >= MIN_MEAN_PATH_RECALL && combined_mean >= MIN_MEAN_PATH_RECALL,
        "summary written to {}",
        out_dir.display()
    );
    println!(
        "wave11 impact recall — baseline {baseline_mean:.3}, legacy {legacy_mean:.3}, review {review_mean:.3}, combined {combined_mean:.3}, build (review) {review_build_mean:.3} over {eligible_build_cases} eligible; raw outputs: {}",
        out_dir.display()
    );
}
