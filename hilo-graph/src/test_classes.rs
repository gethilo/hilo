//! Test-class taxonomy + per-surface class mix (COV-3).
//!
//! The taxonomy itself lives in [`crate::classify::TestClass`] (path + file
//! name + marker/attribute → one of eight classes); this module walks a repo,
//! assigns every test-bearing file — and, where discoverable, each test
//! function — a class, and joins that class map onto the COV-2 coverage links
//! so each surface reports the set of classes that reach it.
//!
//! The signal is **class diversity**, not test count: a surface backed by forty
//! unit tests and zero integration/e2e/conformance tests is not covered in the
//! sense the owner asked about. A surface whose reachable classes number
//! exactly one is flagged `class_gap` with the missing classes named (AC3), and
//! a class with no tests reports `0` and still names itself — never an absent
//! key (AC5: "a NULL must carry a reason").
//!
//! Join key: a coverage link's `target` is a `surface_id` from
//! `.vfs/graph/surfaces.jsonl` (or a module/file path when the target does not
//! resolve to exactly one surface). Only targets that resolve to a surface are
//! reported per-surface here; unmodeled module/file targets remain visible in
//! the coverage-link rows themselves.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::classify::{classify_test_file, classify_test_functions, TestClass, TestFunctionClass};
use crate::coverage_links::CoverageLink;
use crate::parser::Language;
use crate::surfaces::Surface;

/// The largest source file [`enumerate`] will read for marker and
/// test-function discovery. Above this, the file is classified by path alone —
/// a bounded read keeps a giant vendored/generated file from stalling the
/// command, and the class is still assigned.
const MAX_TEST_SOURCE_BYTES: u64 = 2_000_000;

/// Directory names never walked: VCS/build/dependency trees and **fixture**
/// trees. Fixtures are test *inputs* — a repo's fixture trees are full of
/// deliberately-minimal source files that are not themselves the repo's tests,
/// so counting them would double-count the tests that consume them.
const SKIP_DIRS: &[&str] = &["target", "node_modules", "vendor", "fixtures"];

/// One test-bearing file and the class it (and its discovered functions)
/// belong to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestFileClass {
    /// Repo-relative, `/`-separated path — the join key against a coverage
    /// link's `test_file`.
    pub path: String,
    pub class: TestClass,
    /// Test functions discovered in the file, each with its own class. Empty
    /// when the language's test functions are not cheaply discoverable.
    pub functions: Vec<TestFunctionClass>,
}

/// Per-class totals over the repo's test files and discovered test functions.
///
/// One row per [`TestClass::ALL`] is always emitted, so a class with zero
/// tests reports `0` and names itself (AC5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassTotal {
    pub class: TestClass,
    /// Test-bearing FILES of this class.
    pub files: usize,
    /// Discovered test FUNCTIONS of this class.
    pub functions: usize,
}

/// The set of classes that reach one surface, with the gap flag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceClassMix {
    pub surface_id: String,
    /// The surface kind (`cli_verb`, `mcp_tool`, …), from the inventory.
    pub kind: String,
    pub name: String,
    /// The classes that reach this surface, in [`TestClass::ALL`] order.
    pub classes: Vec<TestClass>,
    /// True when exactly one class reaches the surface — the "unit-only"
    /// common case AC3 flags.
    pub class_gap: bool,
    /// The classes that do NOT reach this surface, in [`TestClass::ALL`]
    /// order. Meaningful (and named in the text output) when `class_gap`.
    pub missing_classes: Vec<TestClass>,
    /// How many distinct linked test files reach the surface.
    pub test_files: usize,
}

/// The full `hilo graph test-classes` report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TestClassReport {
    pub schema: u32,
    /// One row per class, always all eight (AC5).
    pub totals: Vec<ClassTotal>,
    /// One row per surface in the inventory, sorted by (kind, name, id).
    pub surfaces: Vec<SurfaceClassMix>,
    /// The `surface_id`s of every `class_gap` surface (AC3/AC6).
    pub class_gap_surfaces: Vec<String>,
    /// The `surface_id`s of every surface no linked test reaches at all —
    /// reported explicitly so an empty class set is never invisible.
    pub uncovered_surfaces: Vec<String>,
    /// The rules the derivation ran under — the documented classification rule
    /// plus the join's own accounting line.
    pub rules: Vec<String>,
}

impl TestClassReport {
    /// The current schema version written to `--json` output.
    pub const SCHEMA: u32 = 1;
}

/// Walk `root` and classify every test-bearing source file.
///
/// Hidden directories and [`SKIP_DIRS`] are never entered. Paths are
/// repo-relative and `/`-separated; the result is sorted by path so it is
/// deterministic.
pub fn enumerate(root: &Path) -> Vec<TestFileClass> {
    let mut out = Vec::new();
    walk(root, Path::new(""), &mut out);
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn walk(root: &Path, rel: &Path, out: &mut Vec<TestFileClass>) {
    let dir = root.join(rel);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy().to_string();
        if name_str.starts_with('.') || SKIP_DIRS.contains(&name_str.as_str()) {
            continue;
        }
        let child_rel = rel.join(&name_str);
        let child_abs = dir.join(&name_str);
        if child_abs.is_dir() {
            walk(root, &child_rel, out);
            continue;
        }
        let ext = child_abs.extension().and_then(|e| e.to_str()).unwrap_or("");
        if Language::from_extension(ext).is_none() {
            continue;
        }
        let rel_path = child_rel.to_string_lossy().replace('\\', "/");
        let source = read_source_capped(&child_abs);
        if let Some(class) = classify_test_file(&rel_path, &source) {
            let functions = classify_test_functions(&rel_path, &source);
            out.push(TestFileClass {
                path: rel_path,
                class,
                functions,
            });
        }
    }
}

fn read_source_capped(path: &Path) -> String {
    let Ok(meta) = std::fs::metadata(path) else {
        return String::new();
    };
    if meta.len() > MAX_TEST_SOURCE_BYTES {
        return String::new();
    }
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Per-class totals over `files` — always one row per class, in
/// [`TestClass::ALL`] order (AC5: zero is reported, never dropped).
pub fn totals(files: &[TestFileClass]) -> Vec<ClassTotal> {
    TestClass::ALL
        .iter()
        .map(|&class| ClassTotal {
            class,
            files: files.iter().filter(|f| f.class == class).count(),
            functions: files
                .iter()
                .flat_map(|f| f.functions.iter())
                .filter(|f| f.class == class)
                .count(),
        })
        .collect()
}

/// Build the report: per-class totals plus the per-surface class mix joined
/// through `links` on `surface_id`.
///
/// `links` are the COV-2 coverage links (`.vfs/graph/coverage_links.jsonl`);
/// `files` is the output of [`enumerate`]. A link whose `test_file` is not in
/// `files` (a stale link, or a file whose language has no parser) is still
/// classified by its path alone, so a link is never silently dropped.
pub fn derive(
    surfaces: &[Surface],
    links: &[CoverageLink],
    files: &[TestFileClass],
) -> TestClassReport {
    let class_of: BTreeMap<&str, TestClass> =
        files.iter().map(|f| (f.path.as_str(), f.class)).collect();

    let mut classes_by_target: BTreeMap<&str, BTreeSet<TestClass>> = BTreeMap::new();
    let mut test_files_by_target: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for link in links {
        let class = class_of
            .get(link.test_file.as_str())
            .copied()
            .or_else(|| classify_test_file(&link.test_file, ""));
        let Some(class) = class else { continue };
        classes_by_target
            .entry(link.target.as_str())
            .or_default()
            .insert(class);
        test_files_by_target
            .entry(link.target.as_str())
            .or_default()
            .insert(link.test_file.as_str());
    }

    let mut rows: Vec<SurfaceClassMix> = Vec::with_capacity(surfaces.len());
    for s in surfaces {
        let reachable = classes_by_target.get(s.surface_id.as_str());
        let classes: Vec<TestClass> = TestClass::ALL
            .iter()
            .copied()
            .filter(|c| reachable.is_some_and(|set| set.contains(c)))
            .collect();
        let missing_classes: Vec<TestClass> = TestClass::ALL
            .iter()
            .copied()
            .filter(|c| !reachable.is_some_and(|set| set.contains(c)))
            .collect();
        rows.push(SurfaceClassMix {
            surface_id: s.surface_id.clone(),
            kind: s.kind.as_str().to_string(),
            name: s.name.clone(),
            class_gap: classes.len() == 1,
            classes,
            missing_classes,
            test_files: test_files_by_target
                .get(s.surface_id.as_str())
                .map(|set| set.len())
                .unwrap_or(0),
        });
    }
    rows.sort_by(|a, b| (&a.kind, &a.name, &a.surface_id).cmp(&(&b.kind, &b.name, &b.surface_id)));

    let class_gap_surfaces: Vec<String> = rows
        .iter()
        .filter(|r| r.class_gap)
        .map(|r| r.surface_id.clone())
        .collect();
    let uncovered_surfaces: Vec<String> = rows
        .iter()
        .filter(|r| r.classes.is_empty())
        .map(|r| r.surface_id.clone())
        .collect();

    TestClassReport {
        schema: TestClassReport::SCHEMA,
        totals: totals(files),
        surfaces: rows,
        class_gap_surfaces,
        uncovered_surfaces,
        rules: rule_lines(files.len(), links.len()),
    }
}

/// The documented classification rule + the join's accounting line.
fn rule_lines(files: usize, links: usize) -> Vec<String> {
    vec![
        "class rule (precedence, first match wins): path component / file name -> bench \
         (benches|benchmark[s]|*bench*), property_fuzz (fuzz|fuzzers|fuzz_targets|*fuzz*), \
         chaos_fault (chaos|faults|fault_injection|*chaos*|*fault*), conformance_golden \
         (conformance|golden[s]|*golden*|*conformance*|*snapshot*), e2e_process \
         (e2e|e2e_tests|end_to_end|*e2e*), doc_smoke (doctests|doc_tests|smoke|*smoke*|*doctest*), \
         integration (any tests|test|spec component), unit (in-source / co-located test)"
            .to_string(),
        "class markers (applied when the path names no class, on test-bearing files only): \
         #[bench]|criterion_* -> bench; proptest!|quickcheck!|fuzz_target!|libfuzzer_sys -> \
         property_fuzz; fault_inject|inject_fault -> chaos_fault; insta::assert*|assert_snapshot|\
         expect_file!|expect_test -> conformance_golden; assert_cmd|cargo_bin|process::Command|\
         subprocess -> e2e_process; doctest -> doc_smoke"
            .to_string(),
        format!(
            "{files} test-bearing source file(s) classified; {links} coverage link(s) joined on \
             surface_id (module/file targets are not surfaces and are not listed per-surface)"
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surfaces::SurfaceKind;
    use tempfile::tempdir;

    use crate::coverage_links::{CoverageLink, EvidenceKind};

    fn write(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    /// The AC4 fixture, built inline: one test of each class.
    fn fixture_tree(root: &Path) {
        write(
            root,
            "src/widget.rs",
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn spins() {}\n}\n",
        );
        write(root, "tests/integration_bar.rs", "#[test]\nfn bar() {}\n");
        write(root, "tests/e2e_cli.rs", "#[test]\nfn cli() {}\n");
        write(
            root,
            "conformance/golden_parse.rs",
            "#[test]\nfn golden() {}\n",
        );
        write(
            root,
            "fuzz/fuzz_target_parse.rs",
            "fuzz_target!(|data: &[u8]| { let _ = data; });\n",
        );
        write(
            root,
            "chaos/fault_inject_db.rs",
            "// fault_inject harness\n#[test]\nfn chaos() {}\n",
        );
        write(root, "benches/throughput.rs", "fn bench_throughput() {}\n");
        write(root, "tests/doc_smoke.rs", "#[test]\nfn smoke() {}\n");
        // A production file: no tests, must NOT be classified.
        write(root, "src/plain.rs", "pub fn run() -> u32 { 1 }\n");
        // A fixture tree: excluded from enumeration.
        write(
            root,
            "tests/fixtures/not_a_repo_test.rs",
            "#[test]\nfn fixture_only() {}\n",
        );
    }

    #[test]
    fn enumerate_maps_one_test_of_each_class_and_excludes_fixtures() {
        let dir = tempdir().unwrap();
        fixture_tree(dir.path());
        let files = enumerate(dir.path());
        let by_path: BTreeMap<&str, TestClass> =
            files.iter().map(|f| (f.path.as_str(), f.class)).collect();

        let expected = [
            ("src/widget.rs", TestClass::Unit),
            ("tests/integration_bar.rs", TestClass::Integration),
            ("tests/e2e_cli.rs", TestClass::E2eProcess),
            ("conformance/golden_parse.rs", TestClass::ConformanceGolden),
            ("fuzz/fuzz_target_parse.rs", TestClass::PropertyFuzz),
            ("chaos/fault_inject_db.rs", TestClass::ChaosFault),
            ("benches/throughput.rs", TestClass::Bench),
            ("tests/doc_smoke.rs", TestClass::DocSmoke),
        ];
        for (path, class) in expected {
            assert_eq!(by_path.get(path).copied(), Some(class), "class of {path}");
        }
        // The fixture tree and the production file are excluded.
        assert!(
            !files.iter().any(|f| f.path.contains("fixtures")),
            "fixture trees must not count as repo tests: {files:?}"
        );
        assert!(!by_path.contains_key("src/plain.rs"));
        // Exactly one file per class — a rule change that reclassified
        // everything would break the assertions above loudly.
        let distinct: BTreeSet<TestClass> = files.iter().map(|f| f.class).collect();
        assert_eq!(distinct.len(), TestClass::ALL.len(), "{files:?}");

        // Function discovery: the unit file names its function.
        let unit = files.iter().find(|f| f.path == "src/widget.rs").unwrap();
        assert_eq!(
            unit.functions,
            vec![TestFunctionClass {
                name: "spins".to_string(),
                class: TestClass::Unit,
            }]
        );
    }

    #[test]
    fn totals_always_name_all_eight_classes_even_at_zero() {
        let dir = tempdir().unwrap();
        fixture_tree(dir.path());
        let files = enumerate(dir.path());
        let rows = totals(&files);
        assert_eq!(rows.len(), TestClass::ALL.len());
        let names: Vec<&str> = rows.iter().map(|t| t.class.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "unit",
                "integration",
                "e2e_process",
                "conformance_golden",
                "property_fuzz",
                "chaos_fault",
                "bench",
                "doc_smoke"
            ]
        );
        // A class with zero tests reports 0 and still names itself.
        assert!(rows.iter().all(|t| t.files > 0), "fixture has one of each");
        let empty = totals(&[]);
        assert_eq!(empty.len(), 8);
        assert!(empty.iter().all(|t| t.files == 0 && t.functions == 0));
        assert!(empty.iter().any(|t| t.class == TestClass::ChaosFault));
    }

    #[test]
    fn class_gap_when_single_class_and_missing_classes_named() {
        let dir = tempdir().unwrap();
        fixture_tree(dir.path());
        let files = enumerate(dir.path());

        let unit_only = Surface::new(
            SurfaceKind::PublicApiItem,
            "src/widget.rs",
            "spins",
            "spins",
            true,
        );
        let multi = Surface::new(
            SurfaceKind::CliVerb,
            "hilo-cli/src/cli.rs",
            "run",
            "run",
            true,
        );
        let links = vec![
            CoverageLink::new(
                "src/widget.rs",
                unit_only.surface_id.clone(),
                EvidenceKind::Import,
            ),
            CoverageLink::new(
                "src/widget.rs",
                multi.surface_id.clone(),
                EvidenceKind::Import,
            ),
            CoverageLink::new(
                "tests/e2e_cli.rs",
                multi.surface_id.clone(),
                EvidenceKind::Import,
            ),
        ];
        let report = derive(&[unit_only.clone(), multi.clone()], &links, &files);

        let row = |id: &str| {
            report
                .surfaces
                .iter()
                .find(|r| r.surface_id == id)
                .unwrap()
                .clone()
        };

        let u = row(&unit_only.surface_id);
        assert_eq!(u.classes, vec![TestClass::Unit]);
        assert!(u.class_gap, "single class is a gap");
        assert_eq!(u.missing_classes.len(), 7);
        assert!(u.missing_classes.contains(&TestClass::E2eProcess));
        assert!(!u.missing_classes.contains(&TestClass::Unit));
        assert_eq!(u.test_files, 1);

        let m = row(&multi.surface_id);
        assert_eq!(m.classes, vec![TestClass::Unit, TestClass::E2eProcess]);
        assert!(!m.class_gap, "two classes is not a gap");
        assert_eq!(m.test_files, 2);

        assert_eq!(
            report.class_gap_surfaces,
            vec![unit_only.surface_id.clone()]
        );
        assert!(report.uncovered_surfaces.is_empty());
        assert_eq!(report.schema, 1);
    }

    #[test]
    fn uncovered_surface_reports_empty_classes_and_is_named() {
        let dir = tempdir().unwrap();
        fixture_tree(dir.path());
        let files = enumerate(dir.path());
        let orphan = Surface::new(
            SurfaceKind::PublicApiItem,
            "src/plain.rs",
            "run",
            "run",
            true,
        );
        let report = derive(std::slice::from_ref(&orphan), &[], &files);
        let row = &report.surfaces[0];
        assert!(row.classes.is_empty());
        assert!(
            !row.class_gap,
            "no classes is uncovered, not a single-class gap"
        );
        assert_eq!(row.missing_classes.len(), 8);
        assert_eq!(report.uncovered_surfaces, vec![orphan.surface_id.clone()]);
        assert!(report.class_gap_surfaces.is_empty());
    }

    #[test]
    fn report_json_shape_is_locked() {
        let dir = tempdir().unwrap();
        fixture_tree(dir.path());
        let files = enumerate(dir.path());
        let s = Surface::new(SurfaceKind::McpTool, "f.rs", "tool", "tool", true);
        let report = derive(&[s], &[], &files);
        let value = serde_json::to_value(&report).unwrap();
        let obj = value.as_object().unwrap();
        assert_eq!(obj.get("schema").and_then(|v| v.as_u64()), Some(1));
        for key in [
            "totals",
            "surfaces",
            "class_gap_surfaces",
            "uncovered_surfaces",
            "rules",
        ] {
            assert!(obj.contains_key(key), "report missing '{key}'");
        }
        assert_eq!(obj["totals"].as_array().unwrap().len(), 8);
        for key in [
            "surface_id",
            "kind",
            "name",
            "classes",
            "class_gap",
            "missing_classes",
            "test_files",
        ] {
            assert!(
                obj["surfaces"][0].get(key).is_some(),
                "surface row missing {key}"
            );
        }
        // Class values are the snake_case wire vocabulary.
        assert_eq!(obj["totals"][0]["class"].as_str(), Some("unit"));
    }

    #[test]
    fn stale_link_file_is_classified_by_path_not_dropped() {
        // A link naming a file `enumerate` did not see (e.g. removed since the
        // links were derived) is still classified by path.
        let s = Surface::new(SurfaceKind::CliVerb, "x.rs", "v", "v", true);
        let link = CoverageLink::new(
            "tests/gone_e2e.rs",
            s.surface_id.clone(),
            EvidenceKind::Import,
        );
        let report = derive(std::slice::from_ref(&s), &[link], &[]);
        let row = &report.surfaces[0];
        assert_eq!(
            row.classes,
            vec![TestClass::E2eProcess],
            "path rule applied"
        );
        assert_eq!(row.test_files, 1);
    }
}
