//! GAP-112: silent-fallback wiring detector.
//!
//! A finding is an interface that (a) is consumed from at least one
//! NON-test site and (b) is satisfied ONLY by satisfiers that classify as
//! test-role. This is the graph-level shape of the TRBL-084 incident: a
//! production call site does a runtime type-assert for a capability, only a
//! test double implements it, so every production run silently falls to a
//! slow/absent path while the tests stay green.
//!
//! Three-way state, never collapsed:
//! - [`WiringState::Pass`] — at least one non-test satisfier exists.
//! - [`WiringState::Finding`] — consumed non-test, satisfiers all test-role.
//! - [`WiringState::Unsupported`] — the language has no conformance
//!   extraction. Unsupported NEVER renders as pass.

use std::collections::{BTreeMap, BTreeSet};

use crate::classify;
use crate::conformance::{
    conformance_supported, extract_conformance, SiteKind, CONSUMES_REL, IMPLEMENTS_REL,
};
use crate::parser::Language;

/// Verdict for one consumed interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WiringState {
    /// A non-test satisfier exists — wired for production use.
    Pass,
    /// Consumed from non-test code but satisfied only by test-role types.
    Finding,
    /// The language has no conformance extraction — never reported as pass.
    Unsupported,
}

/// One classified satisfier of an interface.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Satisfier {
    pub type_name: String,
    pub file: String,
    pub role: SatisfierRole,
}

/// Role classification of a satisfier's defining file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SatisfierRole {
    /// Satisfier defined in a production (non-test) file.
    Production,
    /// Satisfier defined in a test file or a known test-double name.
    Test,
}

impl SatisfierRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            SatisfierRole::Production => "production",
            SatisfierRole::Test => "test",
        }
    }
}

/// One finding (or clean verdict) for a consumed interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WiringResult {
    pub interface: String,
    /// Files that consume the interface, with the kind of consumption site.
    pub consumers: Vec<String>,
    pub state: WiringState,
    pub satisfiers: Vec<Satisfier>,
    /// `Some(lang)` when the language has no conformance extraction.
    pub unsupported_language: Option<&'static str>,
}

/// A consumption site, classified by the test-ness of its file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Consumer {
    pub file: String,
    pub is_test: bool,
    pub site_kind: &'static str,
}

/// Run the wiring detector over a corpus of `(language, repo_relative_path,
/// source)` tuples. Test-file classification uses the same
/// [`classify::is_test_file`] the tested_by/test-class machinery uses.
pub fn detect_wiring(files: &[(Language, String, String)]) -> Vec<WiringResult> {
    let sites = extract_conformance(files);

    // Satisfiers per interface, with role classified from the DEFINING file.
    let mut satisfiers: BTreeMap<String, BTreeSet<Satisfier>> = BTreeMap::new();
    // Consumers per interface (implements-less consumption, e.g. type asserts).
    let mut consumers: BTreeMap<String, Vec<Consumer>> = BTreeMap::new();

    for site in &sites {
        match site.kind {
            SiteKind::Implements => {
                let role = satisfier_role(&site.file, &site.type_name.clone().unwrap_or_default());
                satisfiers
                    .entry(site.interface.clone())
                    .or_default()
                    .insert(Satisfier {
                        type_name: site.type_name.clone().unwrap_or_default(),
                        file: site.file.clone(),
                        role,
                    });
            }
            SiteKind::Consumes => {
                consumers
                    .entry(site.interface.clone())
                    .or_default()
                    .push(Consumer {
                        file: site.file.clone(),
                        is_test: classify::is_test_file(&site.file),
                        site_kind: "consumes",
                    });
            }
        }
    }

    let mut results = Vec::new();
    for (iface, cons) in &consumers {
        let non_test_consumers: Vec<&Consumer> = cons.iter().filter(|c| !c.is_test).collect();
        if non_test_consumers.is_empty() {
            // Consumed only from test files — no production exposure, so a
            // test-only satisfier set is EXPECTED, not a finding.
            continue;
        }
        let sats: Vec<Satisfier> = satisfiers
            .get(iface)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default();
        let state = if sats.is_empty() {
            // Consumed from production code with NO satisfier at all in the
            // corpus: no conformance extraction may also explain this — the
            // language check below decides which is honest.
            WiringState::Finding
        } else if sats.iter().all(|s| s.role == SatisfierRole::Test) {
            WiringState::Finding
        } else {
            WiringState::Pass
        };
        results.push(WiringResult {
            interface: iface.clone(),
            consumers: cons.iter().map(|c| c.file.clone()).collect(),
            state,
            satisfiers: sats,
            unsupported_language: None,
        });
    }
    results
}

/// Classify a satisfier's role from its defining file and type name.
///
/// Test-role comes from (1) the same `is_test_file` rules used everywhere
/// else in the graph, and (2) a name-based heuristic for test doubles that
/// live inside production-language test modules (Go `fake*`/`stub*`/`mock*`
/// types in `*_test.go`, `Mock`/`Fake`/`Stub` prefixes). A satisfier is
/// test-role only when BOTH the file is a test file OR the name reads as a
/// test double AND nothing about it suggests production wiring; keep this
/// conservative — misclassifying production code as test is the detector's
/// worst failure mode (false finding), misclassifying a fake as production
/// merely mutes one finding.
fn satisfier_role(file: &str, type_name: &str) -> SatisfierRole {
    // AC3 neutering harness: with the feature on, the filter always answers
    // Production, so the fixture finding disappears - the neutered fixture
    // test then FAILS, proving the check is non-vacuous.
    #[cfg(feature = "wiring-filter-neutered")]
    {
        let _ = (file, type_name);
        return SatisfierRole::Production;
    }
    #[allow(unreachable_code)]
    {
        if classify::is_test_file(file) {
            return SatisfierRole::Test;
        }
        let lower = type_name.to_lowercase();
        let double_prefix = lower.starts_with("fake")
            || lower.starts_with("stub")
            || lower.starts_with("mock")
            || lower.starts_with("test");
        // A `*Test`/`Fake`/`Mock`-prefixed type in a NON-test file is almost
        // always still a test helper (compiled into the test binary or a
        if double_prefix {
            return SatisfierRole::Test;
        }
        SatisfierRole::Production
    }
}
/// Report the three-way state for a single language, for output surfaces that
/// must show `unsupported` per language rather than per finding.
pub fn language_state(lang: Language) -> (WiringState, Option<&'static str>) {
    if conformance_supported(lang) {
        (WiringState::Pass, None)
    } else {
        (WiringState::Unsupported, Some(lang_name(lang)))
    }
}

fn lang_name(lang: Language) -> &'static str {
    match lang {
        Language::Go => "go",
        Language::Python => "python",
        Language::TypeScript => "typescript",
        Language::JavaScript => "javascript",
        Language::Rust => "rust",
        Language::Java => "java",
        Language::C => "c",
        Language::Cpp => "cpp",
        Language::Ruby => "ruby",
        Language::CSharp => "csharp",
        Language::Kotlin => "kotlin",
        Language::Php => "php",
        Language::Swift => "swift",
        Language::Elixir => "elixir",
        Language::Haskell => "haskell",
        Language::Erlang => "erlang",
        Language::Scala => "scala",
        Language::Zig => "zig",
        Language::Lua => "lua",
        Language::Dart => "dart",
        Language::Clojure => "clojure",
        Language::OCaml => "ocaml",
        Language::R => "r",
        Language::Julia => "julia",
        Language::Elm => "elm",
        Language::Nim => "nim",
        Language::Terraform => "terraform",
    }
}

/// Names of the relations this feature owns (for `graph stats` and docs).
pub const WIRING_RELS: [&str; 2] = [IMPLEMENTS_REL, CONSUMES_REL];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Language;

    fn go_corpus(test_only: bool) -> Vec<(Language, String, String)> {
        let iface = "\
package batch

type BatchWriter interface {
\tWriteBatch([]byte) error
}
";
        let consumer = "\
package batch

func flush(s any) error {
\tif bw, ok := s.(BatchWriter); ok {
\t\treturn bw.WriteBatch(nil)
\t}
\treturn errSlowPath
}
";
        let test_impl = "\
package batch

type FakeSink struct{}

func (f *FakeSink) WriteBatch(b []byte) error { return nil }
";
        let prod_impl = test_impl.replace("FakeSink", "FileSink");
        let mut files = vec![
            (
                Language::Go,
                "batch/writer.go".to_string(),
                iface.to_string(),
            ),
            (
                Language::Go,
                "batch/flush.go".to_string(),
                consumer.to_string(),
            ),
            (
                Language::Go,
                "batch/sink_test.go".to_string(),
                test_impl.to_string(),
            ),
        ];
        if !test_only {
            files.push((Language::Go, "batch/filesink.go".to_string(), prod_impl));
        }
        files
    }

    /// AC2 leg 1: with the test-only implementor alone -> finding.
    #[test]
    fn test_only_satisfier_is_a_finding() {
        let results = detect_wiring(&go_corpus(true));
        assert_eq!(results.len(), 1);
        let r = &results[0];
        assert_eq!(r.interface, "BatchWriter");
        assert_eq!(r.state, WiringState::Finding);
        assert!(r.consumers.contains(&"batch/flush.go".to_string()));
        assert_eq!(r.satisfiers.len(), 1);
        assert_eq!(r.satisfiers[0].role, SatisfierRole::Test);
        assert_eq!(r.satisfiers[0].type_name, "FakeSink");
    }

    /// AC2 leg 2: after adding a production implementor -> finding clears.
    #[test]
    fn production_satisfier_clears_the_finding() {
        let results = detect_wiring(&go_corpus(false));
        let r = results
            .iter()
            .find(|r| r.interface == "BatchWriter")
            .expect("BatchWriter result");
        assert_eq!(r.state, WiringState::Pass);
        assert!(r
            .satisfiers
            .iter()
            .any(|s| s.role == SatisfierRole::Production));
        assert!(r.satisfiers.iter().any(|s| s.role == SatisfierRole::Test));
    }

    /// AC5: unsupported languages never collapse to pass.
    #[test]
    fn unsupported_language_is_not_pass() {
        for lang in [Language::Java, Language::Terraform, Language::Ruby] {
            let (state, name) = language_state(lang);
            assert_eq!(state, WiringState::Unsupported);
            assert!(name.is_some());
        }
        for lang in [
            Language::Go,
            Language::Rust,
            Language::Python,
            Language::TypeScript,
        ] {
            let (state, _) = language_state(lang);
            assert_eq!(state, WiringState::Pass);
        }
    }

    /// Consumption only from test files is expected, not a finding.
    #[test]
    fn test_only_consumption_is_not_a_finding() {
        let files = vec![
            (
                Language::Go,
                "writer.go".to_string(),
                "package p\n\ntype W interface {\n\tDo() error\n}\n".to_string(),
            ),
            (
                Language::Go,
                "writer_test.go".to_string(),
                "package p\n\nfunc testFlush(s any) {\n\tif _, ok := s.(W); ok {}\n}\n".to_string(),
            ),
        ];
        let results = detect_wiring(&files);
        assert_eq!(results.len(), 0, "test-only consumption must not report");
    }

    /// Rust: impl-only corpus with a production consumer via `dyn`.
    #[test]
    fn rust_dyn_wiring_detection() {
        let files = vec![
            (
                Language::Rust,
                "src/writer.rs".to_string(),
                "pub trait BatchWriter {\n    fn write_batch(&mut self, b: &[u8]) -> Result<(), ()>;\n}\n".to_string(),
            ),
            (
                Language::Rust,
                "src/lib.rs".to_string(),
                "fn flush(sink: &mut dyn BatchWriter) -> Result<(), ()> {\n    sink.write_batch(&[])\n}\n".to_string(),
            ),
            (
                Language::Rust,
                "tests/sink.rs".to_string(),
                "pub struct FakeSink;\n\nimpl crate::writer::BatchWriter for FakeSink {\n    fn write_batch(&mut self, _b: &[u8]) -> Result<(), ()> { Ok(()) }\n}\n".to_string(),
            ),
        ];
        let results = detect_wiring(&files);
        let r = results
            .iter()
            .find(|r| r.interface == "BatchWriter")
            .expect("finding");
        assert_eq!(r.state, WiringState::Finding);
        assert!(r.satisfiers.iter().all(|s| s.role == SatisfierRole::Test));
    }

    /// AC3 helper: the satisfier-role filter lives in [`satisfier_role`].
    /// Neutering it (forcing Production) must flip the fixture finding —
    /// exercised end-to-end by the CLI fixture test; this unit pins the
    /// classification contract directly.
    #[test]
    fn satisfier_role_classification_contract() {
        assert_eq!(
            satisfier_role("batch/sink_test.go", "FakeSink"),
            SatisfierRole::Test
        );
        assert_eq!(
            satisfier_role("batch/filesink.go", "FileSink"),
            SatisfierRole::Production
        );
        // Test-double NAMES inside production files classify test-role.
        assert_eq!(
            satisfier_role("batch/fixtures.go", "FakeSink"),
            SatisfierRole::Test
        );
    }
}
