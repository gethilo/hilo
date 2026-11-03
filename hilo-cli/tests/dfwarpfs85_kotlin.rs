//! DF-WARPFS-85 — Kotlin build templates must not lose their imports, and a
//! file warm could not parse must be named as a parse-miss.
//!
//! Reproduces the reported corpus: maven/velocity-style `.kt` templates under
//! `src/test/resources` whose `package` header holds `${...}` placeholders.
//! Those bytes are not valid Kotlin, and tree-sitter-kotlin-ng responded by
//! collapsing the whole file into a single ERROR node — the real `import`
//! statements below the header vanished and the file was silently filed under
//! "no imports".
//!
//! The test drives the compiled `hilo` binary end to end: the template must
//! contribute an `imports` edge, an unparseable template with no imports must
//! land in the parse-miss bucket with its path and reason, and a warm served
//! entirely from the parse cache must report the same buckets.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_hilo");

/// A throwaway Hilo project: manifest + injected HOME (so the warm HOME-guard
/// never trips). No git needed — warm does not require a repository.
struct Project {
    dir: tempfile::TempDir,
    home: tempfile::TempDir,
}

impl Project {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".vfs")).unwrap();
        std::fs::write(
            dir.path().join(".vfs").join("manifest.yaml"),
            "version: 2\nproject:\n  name: kotlin-template-fixture\n",
        )
        .unwrap();
        Self { dir, home }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn write(&self, rel: &str, content: &str) {
        let path = self.path().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, content).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .current_dir(self.path())
            .env("HOME", self.home.path())
            .output()
            .expect("hilo binary must spawn")
    }

    fn warm(&self) -> Output {
        self.run(&["graph", "warm"])
    }

    fn coverage_line(stdout: &str) -> String {
        stdout
            .lines()
            .find(|line| line.starts_with("Coverage: "))
            .unwrap_or_else(|| panic!("warm must print a Coverage: line, got:\n{stdout}"))
            .to_string()
    }

    fn edges_jsonl(&self) -> String {
        std::fs::read_to_string(self.path().join(".vfs/graph/edges.jsonl"))
            .expect("warm must write edges.jsonl")
    }

    fn coverage_ledger(&self) -> PathBuf {
        self.path().join(".vfs/graph/coverage.json")
    }
}

/// The reported bug, end to end: a template `.kt` with a real import produces
/// an `imports` edge; a template the grammar cannot parse and that declares no
/// imports is named in the parse-miss bucket.
#[test]
fn warm_recovers_template_kotlin_imports_and_reports_parse_miss() {
    let project = Project::new();
    // The corpus shape that produced zero edges: a Velocity/Maven package
    // header the Kotlin grammar cannot parse, with a real import below it.
    project.write(
        "src/test/resources/Cities.kt",
        "package ${package}.db\n\n\
         import org.jetbrains.exposed.v1.core.Table\n\n\
         object Cities : Table(\"cities\") {\n    val id = integer(\"id\")\n}\n",
    );
    // Same template family, but nothing to recover: imports are missing, so
    // the only truthful bucket is parse-miss.
    project.write(
        "src/test/resources/Users.kt",
        "package ${package}.db\n\n@parse directive garbage (({\nobject Users\n",
    );
    // A clean Kotlin file that genuinely imports nothing keeps its old bucket.
    project.write(
        "src/main/kotlin/Constants.kt",
        "package com.example\n\nconst val MAX = 10\n",
    );

    let out = project.warm();
    assert!(
        out.status.success(),
        "graph warm failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);

    // AC-1: the template's import is a real edge in the graph.
    let edges = project.edges_jsonl();
    let template_edges: Vec<Value> = edges
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|e| e.get("from").and_then(|v| v.as_str()) == Some("src/test/resources/Cities.kt"))
        .collect();
    assert!(
        template_edges.iter().any(|e| {
            e.get("rel").and_then(|v| v.as_str()) == Some("imports")
                && e.get("to").and_then(|v| v.as_str())
                    == Some("pkg:org.jetbrains.exposed.v1.core.Table")
        }),
        "the template's Table import must be an imports edge, edges.jsonl was:\n{edges}"
    );

    // AC-2: the coverage line carries the parse-miss bucket and closes.
    let coverage = Project::coverage_line(&stdout);
    assert_eq!(
        coverage,
        "Coverage: 3 files = 1 contribute edges + 0 package facades (__init__.py) \
         + 1 no imports + 1 parse-miss + 0 unreadable + 0 unsupported extension",
        "stdout:\n{stdout}"
    );
    // The reason travels with the path, and a repair is never silent.
    assert!(
        stdout.contains("  parse-miss: src/test/resources/Users.kt — syntax errors"),
        "parse-miss files must be named with their reason:\n{stdout}"
    );
    assert!(
        stdout.contains("  parse recovered: src/test/resources/Cities.kt"),
        "a recovered parse must be named:\n{stdout}"
    );

    // A warm served entirely from the parse cache reports the same buckets:
    // the diagnostics ride in `.parse_cache.json`, not in this run's memory.
    let cached = project.warm();
    assert!(
        cached.status.success(),
        "cached warm failed: {}",
        String::from_utf8_lossy(&cached.stderr)
    );
    let cached_stdout = String::from_utf8_lossy(&cached.stdout);
    assert_eq!(
        Project::coverage_line(&cached_stdout),
        coverage,
        "the full-cache-hit path must report the same coverage buckets:\n{cached_stdout}"
    );
    assert!(
        cached_stdout.contains("[all cached, graph unchanged]"),
        "second warm should be a full cache hit:\n{cached_stdout}"
    );
    assert!(
        cached_stdout.contains("  parse-miss: src/test/resources/Users.kt"),
        "cached warm must still name the parse-miss file:\n{cached_stdout}"
    );

    // The ledger persists the bucket for `graph stats`, and stats keeps the
    // zero-edge arithmetic truthful by naming it.
    let ledger: Value = serde_json::from_str(
        &std::fs::read_to_string(project.coverage_ledger()).expect("warm must write coverage.json"),
    )
    .expect("coverage.json must be valid JSON");
    assert_eq!(
        ledger.get("parse_miss").and_then(|v| v.as_u64()),
        Some(1),
        "coverage.json must record the parse-miss count: {ledger}"
    );
    assert_eq!(
        ledger.get("parse_recovered").and_then(|v| v.as_u64()),
        Some(1),
        "coverage.json must record the recovered count: {ledger}"
    );

    let stats = project.run(&["graph", "stats"]);
    assert!(
        stats.status.success(),
        "graph stats failed: {}",
        String::from_utf8_lossy(&stats.stderr)
    );
    let stats_stdout = String::from_utf8_lossy(&stats.stdout);
    assert!(
        stats_stdout.contains("1 parse-miss"),
        "stats must account for the parse-miss zero-edge file:\n{stats_stdout}"
    );

    let _ = &project.home;
}

/// A Kotlin file the grammar cannot parse at all and that has no import lines
/// must never be counted as a contributor — but it must also never be silently
/// dropped: it is named, with its reason.
#[test]
fn warm_names_unparseable_kotlin_template_without_edges() {
    let project = Project::new();
    project.write(
        "src/test/resources/Only.kt",
        "###PACKAGE###\n\n@parse directive garbage (({\n",
    );

    let out = project.warm();
    assert!(
        out.status.success(),
        "graph warm failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        Project::coverage_line(&stdout),
        "Coverage: 1 files = 0 contribute edges + 0 package facades (__init__.py) \
         + 0 no imports + 1 parse-miss + 0 unreadable + 0 unsupported extension",
        "stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("  parse-miss: src/test/resources/Only.kt — syntax errors"),
        "the file must be named with its reason:\n{stdout}"
    );
}
