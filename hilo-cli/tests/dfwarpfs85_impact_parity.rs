//! DF-WARPFS-85 — reproduce Kotlin template impact parity against the Exposed
//! corpus, end to end through the compiled `hilo` binary.
//!
//! The claim under test is that a warm over a real Kotlin tree reports
//! `graph impact pkg:<pkg>` depth-1 dependents equal to the set of files that
//! actually import that package, and that `.kt` files living under
//! `src/test/resources/...` (non-compilable build templates) contribute their
//! `imports` edges like any other file.
//!
//! The corpus is external (a checkout of `JetBrains/Exposed`), so the test is
//! gated on `HILO_EXPOSED_CORPUS`. Unset means a clean, successful skip — CI
//! must not need the corpus. Set but broken parity is a FAILURE, never a skip.
//!
//!   HILO_EXPOSED_CORPUS=/path/to/exposed cargo test -p hilo-cli \
//!     --test dfwarpfs85_impact_parity -- --nocapture

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_hilo");
const TABLE_PACKAGE: &str = "pkg:org.jetbrains.exposed.v1.core.Table";
const TABLE_IMPORT: &str = "import org.jetbrains.exposed.v1.core.Table";
const TEMPLATE_FILES: [&str; 2] = [
    "exposed-maven-plugin/src/test/resources/template/sourceFiles/Cities.kt",
    "exposed-maven-plugin/src/test/resources/template/sourceFiles/Users.kt",
];

/// A throwaway Hilo project holding a private copy of the corpus, plus an
/// injected HOME so the warm HOME-guard never trips and no ambient XDG state
/// leaks into the measurement.
struct Project {
    dir: tempfile::TempDir,
    home: tempfile::TempDir,
}

impl Project {
    fn from_corpus(corpus: &Path) -> Self {
        let dir = tempfile::TempDir::new().expect("temp project dir");
        copy_tree(corpus, dir.path());
        // A cache carried in with the checkout would let a previous binary's
        // graph answer for this one; measure what THIS binary parses.
        let stale = dir.path().join(".vfs");
        if stale.exists() {
            fs::remove_dir_all(&stale).expect("drop corpus-carried .vfs");
        }
        Self {
            dir,
            home: tempfile::TempDir::new().expect("temp home dir"),
        }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn run(&self, args: &[&str]) -> Output {
        let xdg = self.home.path().join("xdg");
        Command::new(BIN)
            .args(args)
            .current_dir(self.root())
            .env("HOME", self.home.path())
            .env("XDG_CACHE_HOME", xdg.join("cache"))
            .env("XDG_CONFIG_HOME", xdg.join("config"))
            .env("XDG_DATA_HOME", xdg.join("data"))
            .env_remove("HILO_VFS_DIR")
            .output()
            .expect("hilo binary must spawn")
    }
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("create destination dir");
    for entry in fs::read_dir(source).expect("read source dir") {
        let entry = entry.expect("dir entry");
        let from = entry.path();
        let to = destination.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            fs::copy(&from, &to).expect("copy file");
        }
    }
}

/// Project-relative paths of every `.kt` file importing the package — the
/// `grep -rl` import-truth, computed here so the test needs no external tools.
fn import_truth(root: &Path) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    collect_importers(root, root, &mut found);
    found
}

fn collect_importers(root: &Path, dir: &Path, found: &mut BTreeSet<String>) {
    for entry in fs::read_dir(dir).expect("read dir") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.is_dir() {
            collect_importers(root, &path, found);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("kt") {
            let Ok(contents) = fs::read_to_string(&path) else {
                continue;
            };
            if contents.lines().any(|line| line.contains(TABLE_IMPORT)) {
                let relative = path.strip_prefix(root).expect("path under project root");
                found.insert(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }
}

/// Depth-1 dependents parsed out of the impact listing's own rows
/// (`<path>  ←  imports  (depth: 1, scope=...)`), not out of its summary note.
fn depth_one_dependents(stdout: &str) -> BTreeSet<String> {
    stdout
        .lines()
        .filter(|line| line.contains("←") && line.contains("(depth: 1,"))
        .filter_map(|line| line.split("  ←  ").next())
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect()
}

/// The integers on the `Coverage:` line, in order:
/// `N files = a contribute edges + b package facades (...) + c no imports
/// + d parse-miss + e unreadable + f unsupported extension`.
fn coverage_numbers(line: &str) -> Vec<usize> {
    line.split_whitespace()
        .filter_map(|token| token.trim_end_matches(':').parse().ok())
        .collect()
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn exposed_kotlin_template_impact_matches_import_truth() {
    let Some(corpus) = env::var_os("HILO_EXPOSED_CORPUS") else {
        println!("skipped: HILO_EXPOSED_CORPUS not set");
        return;
    };
    let corpus = PathBuf::from(corpus);
    assert!(
        corpus.is_dir(),
        "HILO_EXPOSED_CORPUS is not a directory: {}",
        corpus.display()
    );

    let project = Project::from_corpus(&corpus);

    let init = project.run(&["init"]);
    assert!(
        init.status.success(),
        "hilo init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let warm = project.run(&["graph", "warm"]);
    assert!(
        warm.status.success(),
        "graph warm failed: {}",
        String::from_utf8_lossy(&warm.stderr)
    );
    let warm_stdout = stdout_of(&warm);
    let coverage = warm_stdout
        .lines()
        .find(|line| line.starts_with("Coverage: "))
        .unwrap_or_else(|| panic!("graph warm printed no Coverage line:\n{warm_stdout}"));

    // (a) the Coverage line's arithmetic closes: total == sum of its buckets.
    let numbers = coverage_numbers(coverage);
    assert!(
        numbers.len() >= 7,
        "unexpected Coverage line (need total + 6 buckets): {coverage}"
    );
    assert_eq!(
        numbers[0],
        numbers[1..].iter().sum::<usize>(),
        "Coverage arithmetic does not close: {coverage}"
    );

    let impact = project.run(&["graph", "impact", TABLE_PACKAGE, "--max-depth", "1"]);
    assert!(
        impact.status.success(),
        "graph impact failed: {}",
        String::from_utf8_lossy(&impact.stderr)
    );
    let impact_stdout = stdout_of(&impact);
    let dependents = depth_one_dependents(&impact_stdout);
    let truth = import_truth(project.root());

    // (b) depth-1 impact == grep import-truth, as SETS of files, both non-empty.
    assert!(
        !truth.is_empty(),
        "no Kotlin file in the corpus imports {TABLE_PACKAGE}: the measurement premise is gone"
    );
    assert!(
        !dependents.is_empty(),
        "impact reported no depth-1 dependents:\n{impact_stdout}"
    );
    let missing: Vec<&String> = truth.difference(&dependents).collect();
    let extra: Vec<&String> = dependents.difference(&truth).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "impact/import-truth parity broke (impact={}, grep={})\n  missing from impact: {missing:?}\n  extra in impact: {extra:?}\n{impact_stdout}",
        dependents.len(),
        truth.len()
    );
    println!("{coverage}");
    println!(
        "impact depth-1 dependents: {} | grep import-truth: {} | parity: OK",
        dependents.len(),
        truth.len()
    );

    // (c) the template files carry real `imports` edges to that package.
    let edges_path = project.root().join(".vfs/graph/edges.jsonl");
    let edges = fs::read_to_string(&edges_path).expect("graph warm must write edges.jsonl");
    let edges: Vec<Value> = edges
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect();
    for template in TEMPLATE_FILES {
        let edge = edges.iter().find(|edge| {
            edge.get("from").and_then(Value::as_str) == Some(template)
                && edge.get("to").and_then(Value::as_str) == Some(TABLE_PACKAGE)
                && edge.get("rel").and_then(Value::as_str) == Some("imports")
        });
        assert!(
            edge.is_some(),
            "missing `imports` edge {template} -> {TABLE_PACKAGE} (template .kt files must contribute edges)"
        );
        assert!(
            dependents.contains(template),
            "{template} has an imports edge but is absent from the depth-1 impact listing"
        );
    }

    println!("both template files carry `imports` edges to {TABLE_PACKAGE}");
}
