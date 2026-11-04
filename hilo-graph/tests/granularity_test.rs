//! GAP-121: function-level coverage, proven across languages.
//!
//! The bug this file is the regression test for: Hilo emitted
//! `tested_by` from a test FILE to a source FILE at `ast_exact conf=1.00`,
//! which means only "a test file exists beside this file". A file whose test
//! exercises one of its two functions was therefore reported as covered, and
//! omitted from `graph untested` — a false green that steers review away from
//! the untested function, which is where the defect lives
//! (totalwindupflightsystems/consensus `internal/harness/planning.go`,
//! `handleToolCallDuringPlanning`).
//!
//! Every test below runs over the committed fixture tree
//! `fixtures/tested-by-granularity/{go,typescript,python,rust}` — same shape in
//! each language: one file, two public functions, a test that references
//! exactly one.
//!
//! The negative control lives here rather than as a second committed tree: each
//! case copies the fixture to a temp dir, deletes the reference to the covered
//! function from the test file, and asserts BOTH functions are then reported.
//! A run that still reports coverage fails — that is the teeth this check is
//! required to have.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use hilo_graph::audit::{
    self, collect_corpus, function_coverage, AuditBucket, AuditOptions, FileCoverage,
};
use hilo_graph::parser::Language;
use hilo_metadata::inventory::Edge;

/// One language's fixture: the file under test, the symbols, and the exact
/// text the negative control deletes.
struct Case {
    dir: &'static str,
    file: &'static str,
    entry: &'static str,
    test_file: &'static str,
    used: &'static str,
    never: &'static str,
    /// The call line the test file carries and the control removes.
    call: &'static str,
}

const CASES: [Case; 4] = [
    Case {
        dir: "go",
        file: "lib.go",
        entry: "main.go",
        test_file: "lib_test.go",
        used: "Used",
        never: "NeverCalled",
        call: "\tUsed()\n",
    },
    Case {
        dir: "typescript",
        file: "lib.ts",
        entry: "main.ts",
        test_file: "lib.test.ts",
        used: "used",
        never: "neverCalled",
        call: "  used();\n",
    },
    Case {
        dir: "python",
        file: "lib.py",
        entry: "main.py",
        test_file: "test_lib.py",
        used: "used",
        never: "never_called",
        call: "    used()\n",
    },
    Case {
        dir: "rust",
        file: "src/lib.rs",
        entry: "src/main.rs",
        test_file: "tests/used.rs",
        used: "used",
        never: "never_called",
        call: "    granularity_fixture::used();\n",
    },
];

/// Resolve the fixture root.
///
/// Chain (the fixture-path rule for this workspace): explicit env override →
/// the manifest-dir sibling (`<crate>/../fixtures/…`, which is where the tree
/// is committed) → CWD-relative → panic naming every path tried. A test binary
/// runs with the package root as CWD, so the manifest-dir path is the one that
/// normally wins.
fn fixture_root() -> PathBuf {
    let mut tried: Vec<PathBuf> = Vec::new();
    if let Ok(dir) = std::env::var("HILO_GAP121_FIXTURES") {
        let path = PathBuf::from(dir);
        if path.is_dir() {
            return path;
        }
        tried.push(path);
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let from_manifest = manifest
        .join("..")
        .join("fixtures")
        .join("tested-by-granularity");
    if from_manifest.is_dir() {
        return from_manifest;
    }
    tried.push(from_manifest);
    let from_cwd = PathBuf::from("fixtures").join("tested-by-granularity");
    if from_cwd.is_dir() {
        return from_cwd;
    }
    tried.push(from_cwd);
    panic!("tested-by-granularity fixtures not found; tried: {tried:?} (set HILO_GAP121_FIXTURES)");
}

/// Copy a directory tree.
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create fixture copy root");
    for entry in std::fs::read_dir(from).expect("read fixture dir") {
        let entry = entry.expect("fixture entry");
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy fixture file");
        }
    }
}

/// A corpus for one fixture directory.
fn corpus_of(dir: &Path) -> Vec<(Language, String, String)> {
    collect_corpus(dir)
}

/// The coverage row for `file`, or `None` when the file was not reported.
fn row_for<'a>(coverage: &'a [FileCoverage], file: &str) -> Option<&'a FileCoverage> {
    coverage.iter().find(|fc| fc.file == file)
}

fn uncovered_names(fc: &FileCoverage) -> BTreeSet<String> {
    fc.uncovered.iter().map(|s| s.name.clone()).collect()
}

/// AC(1)+(3): with one of two functions referenced, the UNREFERENCED one is
/// listed and the referenced one is not — for every fixture language.
///
/// This is the direct regression test for the false positive: a file-level
/// answer cannot distinguish these two functions at all.
#[test]
fn every_language_lists_only_the_unreferenced_function() {
    let root = fixture_root();
    for case in &CASES {
        let dir = root.join(case.dir);
        assert!(dir.is_dir(), "missing fixture dir {}", dir.display());
        let coverage = function_coverage(&corpus_of(&dir));

        let row = row_for(&coverage, case.file).unwrap_or_else(|| {
            panic!(
                "{}: {} produced no function-coverage row (rows: {:?})",
                case.dir,
                case.file,
                coverage.iter().map(|c| c.file.clone()).collect::<Vec<_>>()
            )
        });
        assert_eq!(
            row.total, 2,
            "{}: fixture must define two functions",
            case.dir
        );
        assert_eq!(
            row.covered, 1,
            "{}: exactly one function is referenced",
            case.dir
        );
        assert_eq!(
            uncovered_names(row),
            BTreeSet::from([case.never.to_string()]),
            "{}: the unreferenced function must be the ONLY uncovered one",
            case.dir
        );
        // The referenced function is covered, and names its witness test.
        let used = row
            .symbols
            .iter()
            .find(|s| s.name == case.used)
            .unwrap_or_else(|| panic!("{}: {} row missing", case.dir, case.used));
        assert!(used.covered, "{}: {} must be covered", case.dir, case.used);
        assert!(
            used.covered_by.iter().any(|t| t == case.test_file),
            "{}: {} must name {} as its witness, got {:?}",
            case.dir,
            case.used,
            case.test_file,
            used.covered_by
        );
    }
}

/// AC(2) NEGATIVE CONTROL: delete the reference to the covered function from
/// the test file. BOTH functions must now be listed.
///
/// A run that still reports coverage here is a phantom check, so this test is
/// the teeth for the one above.
#[test]
fn negative_control_without_the_reference_both_functions_are_listed() {
    let root = fixture_root();
    for case in &CASES {
        let tmp = tempfile::tempdir().expect("temp dir");
        let dir = tmp.path().join(case.dir);
        copy_tree(&root.join(case.dir), &dir);

        // Delete the call, keeping the import: an unused import must NOT count
        // as coverage (that is the sharper half of the rule).
        let test_path = dir.join(case.test_file);
        let body = std::fs::read_to_string(&test_path).expect("read test file");
        assert!(
            body.contains(case.call),
            "{}: fixture test file must contain the call line {:?}",
            case.dir,
            case.call
        );
        let stripped = body.replacen(case.call, "", 1);
        std::fs::write(&test_path, &stripped).expect("write stripped test file");

        let coverage = function_coverage(&corpus_of(&dir));
        let row = row_for(&coverage, case.file).unwrap_or_else(|| {
            panic!(
                "{}: {} vanished from the report after the control edit",
                case.dir, case.file
            )
        });
        assert_eq!(
            uncovered_names(row),
            BTreeSet::from([case.used.to_string(), case.never.to_string()]),
            "{}: with the reference removed BOTH functions must be reported uncovered \
             (file-level coverage would still claim this file is covered)",
            case.dir
        );
        assert_eq!(row.covered, 0);
    }
}

/// AC(3): a file where EVERY function is exercised must NOT appear at function
/// granularity.
#[test]
fn fully_covered_file_is_not_reported() {
    let root = fixture_root();
    for case in &CASES {
        let tmp = tempfile::tempdir().expect("temp dir");
        let dir = tmp.path().join(case.dir);
        copy_tree(&root.join(case.dir), &dir);

        // Add a reference to the second function, so both are exercised.
        let test_path = dir.join(case.test_file);
        let body = std::fs::read_to_string(&test_path).expect("read test file");
        let extra = format!("{}\n", case.never); // bare identifier: a reference
        std::fs::write(&test_path, format!("{body}\n{extra}")).expect("write test file");

        let coverage = function_coverage(&corpus_of(&dir));
        let row = row_for(&coverage, case.file).expect("row exists");
        assert_eq!(
            row.covered, 2,
            "{}: both functions now referenced",
            case.dir
        );
        assert!(
            !row.is_partial_or_uncovered(),
            "{}: a fully covered file must not be reported as partial",
            case.dir
        );
        assert!(row.uncovered.is_empty());
    }
}

/// AC(5): `reachable --from <entrypoint>` reports the unreferenced function as
/// UNREACHABLE and the referenced one as reachable, per language.
#[test]
fn reachable_from_entrypoint_separates_the_two_functions() {
    let root = fixture_root();
    for case in &CASES {
        let dir = root.join(case.dir);
        let corpus = corpus_of(&dir);
        let opts = AuditOptions {
            entry_override: Some(case.entry.to_string()),
            ..Default::default()
        };
        let report = audit::audit(&corpus, &dir.to_string_lossy(), &opts);
        assert!(
            report.entry_override_missing.is_none(),
            "{}: entrypoint {} must resolve",
            case.dir,
            case.entry
        );
        assert_eq!(
            report.entrypoints.files,
            vec![case.entry.to_string()],
            "{}: reachability must be rooted at the named entrypoint only",
            case.dir
        );

        let find = |name: &str| {
            report
                .symbols
                .iter()
                .find(|s| s.name == name && s.file == case.file)
                .unwrap_or_else(|| {
                    panic!(
                        "{}: symbol {} not audited (symbols: {:?})",
                        case.dir,
                        name,
                        report
                            .symbols
                            .iter()
                            .map(|s| s.name.clone())
                            .collect::<Vec<_>>()
                    )
                })
        };

        assert!(
            find(case.used).reachable,
            "{}: {} is called from {}, so it must be reachable",
            case.dir,
            case.used,
            case.entry
        );
        assert!(
            !find(case.never).reachable,
            "{}: {} is called from nowhere and must be reported unreachable from {}",
            case.dir,
            case.never,
            case.entry
        );
    }
}

/// A `--from` that names no source file must be named, never silently answered
/// with "everything is unreachable".
#[test]
fn unknown_entrypoint_is_named() {
    let root = fixture_root();
    let dir = root.join("python");
    let corpus = corpus_of(&dir);
    let opts = AuditOptions {
        entry_override: Some("does/not/exist.py".to_string()),
        ..Default::default()
    };
    let report = audit::audit(&corpus, &dir.to_string_lossy(), &opts);
    assert_eq!(
        report.entry_override_missing.as_deref(),
        Some("does/not/exist.py")
    );
}

/// AC(4): the edge model carries an explicit granularity, so a file-level
/// `tested_by ... conf=1.00` row cannot be read as symbol-level proof.
#[test]
fn edges_carry_their_granularity_and_file_rows_stay_byte_identical() {
    // A file-level edge: no `granularity` key on disk (byte-identical to what
    // every pre-GAP-121 release wrote), and it reads back as `file`.
    let file_edge = Edge::with_provenance(
        "tests/lib_test.rs",
        "src/lib.rs",
        "tested_by",
        "ast_exact",
        1.0,
    );
    let json = serde_json::to_string(&file_edge).expect("serialize file edge");
    assert!(
        !json.contains("granularity"),
        "file-level rows must not grow a key (canonical JSONL bytes unchanged): {json}"
    );
    assert_eq!(file_edge.granularity, "file");
    let back: Edge = serde_json::from_str(&json).expect("deserialize file edge");
    assert_eq!(
        back, file_edge,
        "round-trip preserves the file-level meaning"
    );

    // Legacy rows (no key at all) also read as file-level.
    let legacy: Edge = serde_json::from_str(
        r#"{"from":"tests/a.rs","to":"src/a.rs","rel":"tested_by","provenance":"heuristic","confidence":0.8}"#,
    )
    .expect("legacy row parses");
    assert_eq!(legacy.granularity, "file");
    assert!(!legacy.is_symbol_granularity());

    // A symbol-level edge says so on the row itself.
    let symbol_edge = Edge::symbol(
        "tests/lib_test.rs",
        "sym:src/lib.rs::used",
        "tested_by",
        "ast_exact",
        1.0,
    );
    let json = serde_json::to_string(&symbol_edge).expect("serialize symbol edge");
    assert!(
        json.contains(r#""granularity":"symbol""#),
        "symbol rows must be labelled: {json}"
    );
    assert!(symbol_edge.is_symbol_granularity());
    let back: Edge = serde_json::from_str(&json).expect("deserialize symbol edge");
    assert_eq!(back, symbol_edge);
}

/// The rule text must keep saying that import declarations are excluded — the
/// sharpening that gives the negative control its teeth.
#[test]
fn rule_text_documents_the_evidence_boundary() {
    let rule = audit::FUNCTION_COVERAGE_RULE;
    assert!(
        rule.contains("import"),
        "rule must name the import exclusion: {rule}"
    );
    assert!(
        rule.contains("tested_by"),
        "rule must say file-level tested_by is not function coverage: {rule}"
    );
}

/// The bucket view still classifies reachability-linked symbols the same way
/// when no entrypoint override is used (additive-change guard for the audit
/// that existed before this row).
#[test]
fn audit_without_override_still_uses_every_entrypoint() {
    let root = fixture_root();
    let dir = root.join("go");
    let corpus = corpus_of(&dir);
    let report = audit::audit(&corpus, &dir.to_string_lossy(), &AuditOptions::default());
    assert!(
        !report.entrypoints.files.is_empty(),
        "the go fixture has a main.go entrypoint"
    );
    // `NeverCalled` is unreachable under either rooting.
    let never = report
        .symbols
        .iter()
        .find(|s| s.name == "NeverCalled")
        .expect("NeverCalled audited");
    assert_eq!(never.bucket, AuditBucket::Unreachable);
}
