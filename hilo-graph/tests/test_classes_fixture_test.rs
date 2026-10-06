//! COV-3 AC4 — the test-class rule pinned on a fixture tree.
//!
//! `hilo-graph/tests/fixtures/test_classes/` holds exactly one test of each of
//! the eight classes. This test asserts the file→class mapping one entry at a
//! time, so a rule change that reclassified everything (say, every file
//! collapsing to `unit`) breaks seven or eight assertions at once and cannot
//! pass silently.
//!
//! The fixture files are inert: they live in a nested fixture directory, are
//! not cargo targets, and are excluded from [`enumerate`] by its fixture-tree
//! rule (proven by the second test below).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use hilo_graph::classify::TestClass;
use hilo_graph::test_classes::enumerate;

/// The pinned mapping — one file, one class.
const EXPECTED: &[(&str, TestClass)] = &[
    ("src/widget.rs", TestClass::Unit),
    ("tests/integration_bar.rs", TestClass::Integration),
    ("tests/e2e_cli.rs", TestClass::E2eProcess),
    ("conformance/golden_parse.rs", TestClass::ConformanceGolden),
    ("fuzz/fuzz_target_parse.rs", TestClass::PropertyFuzz),
    ("chaos/fault_inject_db.rs", TestClass::ChaosFault),
    ("benches/throughput.rs", TestClass::Bench),
    ("tests/doc_smoke.rs", TestClass::DocSmoke),
];

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/test_classes")
}

#[test]
fn fixture_tree_maps_exactly_one_test_to_each_class() {
    let files = enumerate(&fixture_root());
    let got: BTreeMap<&str, TestClass> = files.iter().map(|f| (f.path.as_str(), f.class)).collect();

    assert_eq!(
        got.len(),
        EXPECTED.len(),
        "the fixture tree holds exactly the eight files; got {got:?}"
    );
    for (path, class) in EXPECTED {
        assert_eq!(
            got.get(path).copied(),
            Some(*class),
            "class rule for fixture file {path} (got {got:?})"
        );
    }

    // The set of classes is EVERY class — a rule that collapsed several files
    // onto one class would shrink this set (and fail the loop above).
    let classes: BTreeSet<TestClass> = files.iter().map(|f| f.class).collect();
    let all: BTreeSet<TestClass> = TestClass::ALL.into_iter().collect();
    assert_eq!(classes, all, "every class must be represented exactly once");
}

#[test]
fn fixture_tree_is_not_counted_as_a_repo_test_by_enumerate() {
    // The crate directory contains `tests/fixtures/test_classes/**`; the
    // enumeration's fixture-tree rule must keep those out of the repo's own
    // test-class totals.
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let files = enumerate(crate_root);
    assert!(
        !files.iter().any(|f| f.path.contains("fixtures")),
        "fixture files leaked into the repo enumeration: {:?}",
        files
            .iter()
            .filter(|f| f.path.contains("fixtures"))
            .map(|f| &f.path)
            .collect::<Vec<_>>()
    );
    // Sanity: the crate's own real tests ARE enumerated.
    assert!(
        files.iter().any(|f| f.path == "tests/graph_test.rs"),
        "real crate tests must be enumerated"
    );
}

#[test]
fn crate_bench_files_are_classified_as_bench_by_path() {
    // The path rule (§1) on the real crate, not a fixture: every `benches/`
    // file is Bench, and nothing else in the crate is.
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let files = enumerate(crate_root);
    let mut benches: Vec<&str> = files
        .iter()
        .filter(|f| f.class == TestClass::Bench)
        .map(|f| f.path.as_str())
        .collect();
    benches.sort_unstable();
    assert_eq!(
        benches,
        vec![
            "benches/graph_bench.rs",
            "benches/hotpath_bench.rs",
            "benches/semantic_bench.rs",
            "benches/signal_bench.rs",
        ],
        "bench-class files in the crate"
    );
}
