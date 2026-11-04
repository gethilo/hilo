//! GAP-121 end-to-end: file-level coverage vs FUNCTION-level coverage through
//! the compiled `hilo` binary.
//!
//! The fixture is `fixtures/tested-by-granularity/repro-typescript`, a
//! minimisation of the reported false positive
//! (totalwindupflightsystems/consensus `internal/harness/planning.go`):
//!
//!   src/lib.ts       imports ./helper  -> it has an `imports` edge, so it is a
//!                                        candidate for `graph untested`
//!   src/lib.test.ts  imports ./lib     -> the graph carries a file-level
//!                                        `tested_by` edge to src/lib.ts
//!   src/lib.ts also defines neverCalled(), which NO test names.
//!
//! Before the fix `graph untested` omitted src/lib.ts — a confident green over
//! a file with an untested function, which is the failure direction that steers
//! review away from the defect.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const BIN: &str = env!("CARGO_BIN_EXE_hilo");

fn fixtures_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = manifest
        .join("..")
        .join("fixtures")
        .join("tested-by-granularity");
    assert!(
        path.is_dir(),
        "fixture tree missing at {} (committed at fixtures/tested-by-granularity)",
        path.display()
    );
    path
}

fn fixture_root() -> PathBuf {
    fixtures_root().join("repro-typescript")
}

fn unique_tempdir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "hilo-gap121-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create copy root");
    for entry in fs::read_dir(from).expect("read fixture dir") {
        let entry = entry.expect("fixture entry");
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy fixture file");
        }
    }
}

fn run(dir: &Path, args: &[&str]) -> String {
    let output = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        // The fixture is tiny; capping rayon keeps a parallel test run from
        // oversubscribing the box (GAP-121 hard rule: never warm outside a
        // dedicated directory, and cap the pool when you do).
        .env("RAYON_NUM_THREADS", "8")
        .output()
        .expect("failed to spawn hilo");
    assert!(
        output.status.success(),
        "hilo {} failed in {}: stderr={}",
        args.join(" "),
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn function_granularity_names_the_uncovered_function_the_file_view_hides() {
    let tmp = unique_tempdir("e2e");
    let repo = tmp.join("repro-typescript");
    copy_tree(&fixture_root(), &repo);

    run(&repo, &["init"]);
    run(&repo, &["graph", "warm"]);

    // (a) FILE granularity: the historical answer. src/lib.ts is NOT listed,
    // because a test file targets it. This is the false positive, reproduced.
    let file_level = run(&repo, &["graph", "untested"]);
    assert!(
        !file_level.contains("src/lib.ts"),
        "file-level output must stay as it was (src/lib.ts absent, covered by \
         a tested_by edge); got:\n{file_level}"
    );

    // (a') ... and the edge that produces it says, on the row, that it is a
    // file-level claim — not symbol-level proof (AC4).
    let related = run(
        &repo,
        &["graph", "related", "src/lib.ts", "--direction", "reverse"],
    );
    assert!(
        related.contains("tested_by"),
        "expected a file-level tested_by edge on src/lib.ts:\n{related}"
    );
    assert!(
        related.contains("granularity=file"),
        "the tested_by row must carry its granularity label:\n{related}"
    );
    assert!(
        related.contains("file-level: proves a test file targets this file"),
        "the tested_by row must say what a file-level conf=1.00 does NOT prove:\n{related}"
    );

    // (b) FUNCTION granularity: the uncovered function is named.
    let function_level = run(&repo, &["graph", "untested", "--granularity", "function"]);
    assert!(
        function_level.contains("src/lib.ts"),
        "function granularity must report the file:\n{function_level}"
    );
    assert!(
        function_level.contains("neverCalled  src/lib.ts:"),
        "function granularity must NAME the unreferenced function:\n{function_level}"
    );
    assert!(
        !function_level.contains("    used  src/lib.ts:"),
        "the referenced function must not be listed as uncovered:\n{function_level}"
    );
    // JSON view carries the granularity on every row (machine consumers).
    let json = run(
        &repo,
        &["graph", "untested", "--granularity", "function", "--json"],
    );
    assert!(json.contains(r#""granularity": "function""#), "{json}");
    assert!(json.contains(r#""granularity": "symbol""#), "{json}");

    // (e) reachability from the entrypoint: `used` reachable, `neverCalled` not.
    let reachable = run(&repo, &["graph", "reachable", "--from", "src/main.ts"]);
    assert!(
        reachable.contains("reachable    used  src/lib.ts:"),
        "used is called from src/main.ts and must be reachable:\n{reachable}"
    );
    assert!(
        reachable.contains("unreachable  neverCalled  src/lib.ts:"),
        "neverCalled is called from nowhere and must be unreachable:\n{reachable}"
    );

    // (2) NEGATIVE CONTROL with teeth: delete the call to `used` from the test
    // file (keeping its import — an unused import is not coverage). BOTH
    // functions must now be listed.
    let test_file = repo.join("src").join("lib.test.ts");
    let body = fs::read_to_string(&test_file).expect("read test file");
    let stripped = body.replace("  expect(used()).toBe(1);\n", "");
    assert_ne!(body, stripped, "control must actually remove the call");
    fs::write(&test_file, stripped).expect("write control test file");

    let control = run(&repo, &["graph", "untested", "--granularity", "function"]);
    assert!(
        control.contains("used  src/lib.ts:") && control.contains("neverCalled  src/lib.ts:"),
        "with the reference removed BOTH functions must be listed:\n{control}"
    );

    let _ = fs::remove_dir_all(&tmp);
}

/// AC(1) per language, through the binary: the same shape in Go, TypeScript,
/// Python and Rust must each name their unreferenced function, while the
/// file-level output (the unchanged default) does not report the file at all.
#[test]
fn every_language_names_its_unreferenced_function_through_the_cli() {
    // (fixture dir, defining file, referenced symbol, unreferenced symbol)
    let cases: [(&str, &str, &str, &str); 4] = [
        ("go", "lib.go", "Used", "NeverCalled"),
        ("typescript", "lib.ts", "used", "neverCalled"),
        ("python", "lib.py", "used", "never_called"),
        ("rust", "src/lib.rs", "used", "never_called"),
    ];

    for (dir, file, used, never) in cases {
        let tmp = unique_tempdir(&format!("lang-{dir}"));
        let repo = tmp.join(dir);
        copy_tree(&fixtures_root().join(dir), &repo);

        run(&repo, &["init"]);
        run(&repo, &["graph", "warm"]);

        // Default (file granularity): unchanged behaviour — the file is not
        // named as untested, because a test file targets it.
        let file_level = run(&repo, &["graph", "untested"]);
        assert!(
            !file_level.contains(file),
            "{dir}: file-level output must be unchanged and must not name {file}:\n{file_level}"
        );

        // Function granularity: the unreferenced symbol is named.
        let function_level = run(&repo, &["graph", "untested", "--granularity", "function"]);
        assert!(
            function_level.contains(&format!("{never}  {file}:")),
            "{dir}: function granularity must name {never} in {file}:\n{function_level}"
        );
        assert!(
            !function_level.contains(&format!("    {used}  {file}:")),
            "{dir}: {used} is referenced by the test and must not be listed:\n{function_level}"
        );

        let _ = fs::remove_dir_all(&tmp);
    }
}
