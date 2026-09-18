//! Integration tests for the Hilo CLI binary.
//!
//! These tests exercise the compiled `hilo` binary via [`std::process::Command`].
//! They intentionally avoid tree-sitter, DuckDB, and xattr dependencies so they
//! pass in any CI environment — only filesystem operations are exercised.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Path to the compiled CLI binary, injected by Cargo at compile time.
const BIN: &str = env!("CARGO_BIN_EXE_hilo");

/// Create a unique temporary directory under the system temp dir.
///
/// Uses `std::env::temp_dir` instead of the `tempfile` crate (which is not a
/// dependency of this crate). Each call produces a unique path from the process
/// id and the current nanosecond timestamp to avoid collisions between parallel
/// test runs.
fn unique_tempdir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock moved backwards")
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("hilo-test-{label}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&dir).expect("failed to create temp dir");
    dir
}

/// Run `hilo init` in `dir` and assert it succeeded.
///
/// GAP-086: `graph warm` and `serve --mcp` require a project root, so any
/// test that expects them to proceed has to initialize first.
fn init_project(dir: &std::path::Path) {
    let output = Command::new(BIN)
        .arg("init")
        .current_dir(dir)
        .output()
        .expect("failed to spawn hilo init");
    assert!(
        output.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Run `hilo <args>` in `dir`, asserting it exited 0, and return stdout.
fn run_hilo_ok(dir: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn hilo {}: {e}", args.join(" ")));
    assert!(
        output.status.success(),
        "hilo {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

// ─────────────────────── init ───────────────────────

#[test]
fn init_creates_vfs_and_manifest() {
    let dir = unique_tempdir("init");
    let output = Command::new(BIN)
        .arg("init")
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo init");

    assert!(
        output.status.success(),
        "init exited non-zero: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // .vfs/ directory tree must exist.
    assert!(dir.join(".vfs").exists(), ".vfs/ was not created");

    // manifest.yaml must exist and contain version: 2.
    let manifest_path = dir.join(".vfs").join("manifest.yaml");
    assert!(manifest_path.exists(), "manifest.yaml was not created");
    let manifest = fs::read_to_string(&manifest_path).expect("failed to read manifest");
    assert!(
        manifest.contains("version: 2"),
        "manifest should contain 'version: 2', got:\n{manifest}"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn init_is_idempotent() {
    let dir = unique_tempdir("idempotent");

    for i in 0..2 {
        let output = Command::new(BIN)
            .arg("init")
            .current_dir(&dir)
            .output()
            .expect("failed to spawn hilo init");
        assert!(
            output.status.success(),
            "init pass {i} exited non-zero: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Running twice should not have destroyed or corrupted the manifest.
    let manifest = fs::read_to_string(dir.join(".vfs").join("manifest.yaml"))
        .expect("failed to read manifest after double-init");
    assert!(manifest.contains("version: 2"));

    let _ = fs::remove_dir_all(&dir);
}

// ─────────────────────── meta ───────────────────────

#[test]
fn meta_nonexistent_file_errors() {
    let output = Command::new(BIN)
        .args(["meta", "/nonexistent/path/to/no/such/file"])
        .output()
        .expect("failed to spawn hilo meta");

    assert!(
        !output.status.success(),
        "meta should exit non-zero for a nonexistent file"
    );
}

#[test]
fn meta_set_with_equals_in_name_is_usage_error() {
    let dir = unique_tempdir("meta-eq");
    let file = dir.join("sample.txt");
    fs::write(&file, b"data").expect("write file");

    // GAP-067: `--set role=core` used to exit 0 and create a garbage xattr
    // literally named user.vfs.role=core with an empty value.
    let output = Command::new(BIN)
        .args([
            "meta",
            file.to_str().expect("utf8 path"),
            "--set",
            "role=core",
        ])
        .output()
        .expect("failed to spawn hilo meta");

    assert!(
        !output.status.success(),
        "meta --set role=core must exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("invalid attribute name") && stderr.contains("role=core"),
        "usage error must name the rejected attribute, got: {stderr}"
    );
    assert!(
        stderr.contains("--value"),
        "usage error must show the correct --set/--value syntax, got: {stderr}"
    );

    let _ = fs::remove_dir_all(&dir);
}

// ─────────────────────── graph ───────────────────────

#[test]
fn graph_stats_no_data_prints_message() {
    let dir = unique_tempdir("graph-stats");

    let output = Command::new(BIN)
        .args(["graph", "stats"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo graph stats");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "graph stats should succeed (exit 0) when there is no data: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("Graph cache is empty"),
        "expected a 'Graph cache is empty' message, got:\n{stdout}"
    );

    let _ = fs::remove_dir_all(&dir);
}

// ─────────────────────── classify ───────────────────────

#[test]
fn classify_dry_run_does_not_require_vfs() {
    // `classify --dry-run` on an empty directory should exit 0 gracefully.
    let dir = unique_tempdir("classify-dry");

    let output = Command::new(BIN)
        .args(["classify", "--dry-run"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo classify");

    assert!(
        output.status.success(),
        "classify --dry-run should succeed even on empty dir: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn classify_dry_run_with_source_file() {
    // `classify --dry-run` with a source file should print a classification.
    let dir = unique_tempdir("classify-src");
    let src_dir = dir.join("src");
    fs::create_dir_all(&src_dir).expect("failed to create src dir");
    fs::write(src_dir.join("main.rs"), "fn main() {}").expect("failed to write main.rs");

    let output = Command::new(BIN)
        .args(["classify", "--dry-run", "-v"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo classify");

    assert!(
        output.status.success(),
        "classify --dry-run -v should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("entrypoint") || stdout.contains("library") || stdout.contains("main.rs"),
        "classify --dry-run -v should mention a classification for main.rs, got:\n{stdout}"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn graph_warm_reports_exclusions_on_normal_and_cached_runs() {
    let dir = unique_tempdir("warm-exclusions");
    fs::create_dir_all(dir.join("src")).expect("failed to create src");
    fs::write(dir.join("src/main.rs"), "fn main() {}\n").expect("failed to write main.rs");

    let excluded_sources = [
        "vendor/dep.rs",
        "go/pkg/mod/example/dep.go",
        "node_modules/index.js",
        ".hidden/secret.py",
    ];
    for rel in excluded_sources {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().expect("source parent")).expect("failed to create parent");
        fs::write(path, "fn excluded() {}\n").expect("failed to write excluded source");
    }
    // These must not inflate the supported-source count.
    fs::write(dir.join("vendor/README.txt"), "documentation\n").expect("failed to write README");
    fs::write(dir.join("vendor/asset.bin"), [0_u8, 1, 2]).expect("failed to write asset");

    let init = Command::new(BIN)
        .arg("init")
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo init");
    assert!(
        init.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let warm = Command::new(BIN)
        .args(["graph", "warm"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn first graph warm");
    assert!(
        warm.status.success(),
        "first graph warm failed: {}",
        String::from_utf8_lossy(&warm.stderr)
    );
    let first_stdout = String::from_utf8_lossy(&warm.stdout);
    let first_summary = first_stdout
        .lines()
        .find(|line| line.starts_with("Excluded "))
        .expect("first warm should report exclusions");
    assert_eq!(
        first_summary,
        "Excluded 4 supported source files (vendor: 1, go/pkg/mod: 1, node_modules: 1, hidden: 1)"
    );

    let cached = Command::new(BIN)
        .args(["graph", "warm"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn cached graph warm");
    assert!(
        cached.status.success(),
        "cached graph warm failed: {}",
        String::from_utf8_lossy(&cached.stderr)
    );
    let cached_stdout = String::from_utf8_lossy(&cached.stdout);
    assert!(
        cached_stdout.contains("[all cached, graph unchanged]"),
        "second warm should use the full cache-hit path: {cached_stdout}"
    );
    let cached_summary = cached_stdout
        .lines()
        .find(|line| line.starts_with("Excluded "))
        .expect("cached warm should report exclusions");
    assert_eq!(cached_summary, first_summary);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn graph_warm_accounts_for_every_discovered_file() {
    // GAP-065: warm must classify every discovered file — contributes /
    // package facade / no imports / unreadable — and the summary arithmetic
    // must close.  The unreadable case needs a chmod-000 file (unix, non-root).
    let dir = unique_tempdir("warm-coverage");

    fs::write(
        dir.join("app.py"),
        "import helper\n\n\ndef run():\n    helper.go()\n",
    )
    .expect("failed to write app.py");
    fs::write(
        dir.join("helper.py"),
        "import os\n\n\ndef go():\n    os.path.join('x')\n",
    )
    .expect("failed to write helper.py");
    // Package facade: an empty __init__.py has zero edges by design.
    let pkg = dir.join("pkg");
    fs::create_dir_all(&pkg).expect("failed to create pkg");
    fs::write(pkg.join("__init__.py"), "").expect("failed to write __init__.py");
    // No imports: a constant-only module has zero edges and no facade name.
    fs::write(dir.join("constants.py"), "MAX = 1\n").expect("failed to write constants.py");
    // Unreadable: chmod 000 — readable only to root, so the chmod is skipped
    // (and the expectations adjusted) when the test runs as root.
    fs::write(dir.join("locked.rs"), "fn locked() {}\n").expect("failed to write locked.rs");
    #[cfg(unix)]
    // SAFETY: `geteuid` is a trivially safe libc call (reads the real uid).
    let is_root = unsafe { libc::geteuid() } == 0;
    #[cfg(not(unix))]
    let is_root = true;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if !is_root {
            fs::set_permissions(dir.join("locked.rs"), fs::Permissions::from_mode(0o000))
                .expect("failed to chmod locked.rs to 000");
        }
    }

    let init = Command::new(BIN)
        .arg("init")
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo init");
    assert!(
        init.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let warm = Command::new(BIN)
        .args(["graph", "warm"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn graph warm");
    assert!(
        warm.status.success(),
        "graph warm failed: {}",
        String::from_utf8_lossy(&warm.stderr)
    );
    let stdout = String::from_utf8_lossy(&warm.stdout);
    let (expected_no_imports, expected_unreadable) = if is_root { (2, 0) } else { (1, 1) };
    let expected_summary = format!(
        "Coverage: 5 files = 2 contribute edges + 1 package facades (__init__.py) \
         + {expected_no_imports} no imports + {expected_unreadable} unreadable \
         + 0 unsupported extension"
    );
    let summary = stdout
        .lines()
        .find(|line| line.starts_with("Coverage: "))
        .expect("warm should print the coverage summary");
    assert_eq!(summary, expected_summary, "stdout:\n{stdout}");
    if !is_root {
        assert!(
            stdout.contains("  unreadable source: locked.rs"),
            "unreadable file must be named in the summary:\n{stdout}"
        );
    }

    // Restore the locked file and warm twice more: the re-parse fills its
    // cache entry, then the final run takes the full-cache-hit return path —
    // which must print the same arithmetic, with the (now readable) former
    // locked file classified as a no-imports file straight from the cache.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir.join("locked.rs"), fs::Permissions::from_mode(0o644))
            .expect("failed to restore locked.rs permissions");
    }
    for round in 2..=3 {
        let rewarm = Command::new(BIN)
            .args(["graph", "warm"])
            .current_dir(&dir)
            .output()
            .expect("failed to spawn rewarm");
        assert!(
            rewarm.status.success(),
            "rewarm {round} failed: {}",
            String::from_utf8_lossy(&rewarm.stderr)
        );
        let rewarm_stdout = String::from_utf8_lossy(&rewarm.stdout);
        let rewarm_summary = rewarm_stdout
            .lines()
            .find(|line| line.starts_with("Coverage: "))
            .expect("rewarm should print the coverage summary");
        assert_eq!(
            rewarm_summary,
            "Coverage: 5 files = 2 contribute edges + 1 package facades (__init__.py) \
             + 2 no imports + 0 unreadable + 0 unsupported extension",
            "rewarm {round} stdout:\n{rewarm_stdout}"
        );
        if round == 3 {
            assert!(
                rewarm_stdout.contains("[all cached, graph unchanged]"),
                "third warm should take the full-cache-hit path:\n{rewarm_stdout}"
            );
        }
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn graph_warm_help_documents_discovery_overrides() {
    let output = Command::new(BIN)
        .args(["graph", "warm", "--help"])
        .output()
        .expect("failed to spawn graph warm help");
    assert!(
        output.status.success(),
        "graph warm --help failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let help = String::from_utf8_lossy(&output.stdout);
    for term in [
        "dependency",
        "cache",
        "vendor",
        "hidden",
        "graph.include_paths",
        ".vfs/manifest.yaml",
        "manifest.yaml",
        ".hiloignore",
        "backend sync",
        "does not override graph discovery",
    ] {
        assert!(
            help.contains(term),
            "warm help should contain {term:?}: {help}"
        );
    }
}

// ─────────────────────── graph warm ───────────────────────

#[test]
fn graph_warm_creates_graph_directory() {
    // `graph warm` should create `.vfs/graph/` and produce edges.
    let dir = unique_tempdir("warm");

    // Create a small project with imports.
    let src = dir.join("src");
    fs::create_dir_all(&src).expect("failed to create src");
    // main.go imports fmt and helper — helper.go defines helper.
    fs::write(
        src.join("main.go"),
        "package main\nimport \"fmt\"\nfunc main() { fmt.Println(\"hi\") }\n",
    )
    .expect("failed to write main.go");
    fs::write(
        src.join("helper.go"),
        "package main\nfunc Helper() string { return \"help\" }\n",
    )
    .expect("failed to write helper.go");

    // Initialize VFS first.
    let init_output = Command::new(BIN)
        .arg("init")
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo init");
    assert!(
        init_output.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init_output.stderr)
    );

    // Run graph warm.
    let output = Command::new(BIN)
        .args(["graph", "warm"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo graph warm");

    assert!(
        output.status.success(),
        "graph warm should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The .vfs/graph/ directory should exist.
    assert!(
        dir.join(".vfs").join("graph").exists(),
        ".vfs/graph/ was not created by graph warm"
    );

    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("Excluded "),
        "warm with no pruned sources must not print a zero-exclusion summary"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn graph_clean_rewarms_after_invalidating_parse_cache() {
    // GAP-070: cleaning the graph must also invalidate the per-file parse
    // cache, otherwise the immediate warm takes the all-cached fast path and
    // never recreates edges.jsonl or graph.db.
    let dir = unique_tempdir("graph-clean-rewarm");
    let src = dir.join("src");
    fs::create_dir_all(&src).expect("failed to create src");
    fs::write(
        src.join("main.go"),
        "package main\nimport \"fmt\"\nfunc main() { fmt.Println(\"hi\") }\n",
    )
    .expect("failed to write main.go");

    let init = Command::new(BIN)
        .arg("init")
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo init");
    assert!(
        init.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let warm = Command::new(BIN)
        .args(["graph", "warm"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn first graph warm");
    assert!(
        warm.status.success(),
        "first graph warm failed: {}",
        String::from_utf8_lossy(&warm.stderr)
    );
    let parse_cache = dir.join(".vfs/graph/.parse_cache.json");
    assert!(
        parse_cache.exists(),
        "first warm must populate the parse cache"
    );

    let clean = Command::new(BIN)
        .args(["graph", "clean"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn graph clean");
    assert!(
        clean.status.success(),
        "graph clean failed: {}",
        String::from_utf8_lossy(&clean.stderr)
    );
    assert!(
        String::from_utf8_lossy(&clean.stdout).contains(".parse_cache.json"),
        "graph clean must report removing the parse cache: {}",
        String::from_utf8_lossy(&clean.stdout)
    );
    assert!(
        !parse_cache.exists(),
        "graph clean must remove the per-file parse cache"
    );

    // Do not touch the source between clean and warm: this is the stale-cache
    // recovery path that previously hit the all-cached early return.
    let rewarm = Command::new(BIN)
        .args(["graph", "warm"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn graph warm after clean");
    assert!(
        rewarm.status.success(),
        "graph warm after clean failed: {}",
        String::from_utf8_lossy(&rewarm.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&rewarm.stdout).contains("[all cached, graph unchanged]"),
        "rewarm must reparse files after clean: {}",
        String::from_utf8_lossy(&rewarm.stdout)
    );

    let stats = Command::new(BIN)
        .args(["graph", "stats"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn graph stats after rewarm");
    assert!(
        stats.status.success(),
        "graph stats failed after rewarm: {}",
        String::from_utf8_lossy(&stats.stderr)
    );
    let stats_stdout = String::from_utf8_lossy(&stats.stdout);
    let total_edges = stats_stdout
        .lines()
        .find_map(|line| {
            line.strip_prefix("Total edges: ")?
                .split_whitespace()
                .next()?
                .parse::<usize>()
                .ok()
        })
        .expect("graph stats must report total edges after rewarm");
    assert!(
        total_edges > 0,
        "graph stats must prove the rebuilt graph is non-empty: {stats_stdout}"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn graph_warm_language_filter_unknown_errors() {
    // `--language` with an unsupported language should exit non-zero.
    let dir = unique_tempdir("warm-lang");

    // Init first.
    let init_output = Command::new(BIN)
        .arg("init")
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo init");
    assert!(
        init_output.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init_output.stderr)
    );

    let output = Command::new(BIN)
        .args(["graph", "warm", "--language", "cobol"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo graph warm");

    assert!(
        !output.status.success(),
        "graph warm with unknown language should exit non-zero"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown language") || stderr.contains("cobol"),
        "stderr should mention the unknown language, got: {stderr}"
    );

    let _ = fs::remove_dir_all(&dir);
}

// ─────────────────────── graph impact (absent path) ───────────────────────

#[test]
fn graph_impact_nonexistent_file_errors() {
    let dir = unique_tempdir("impact");

    let output = Command::new(BIN)
        .args(["graph", "impact", "nonexistent.rs"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo graph impact");

    // Contract (GAP-039): a path absent from the graph AND from disk must fail
    // loudly with a non-zero exit and a "not in the graph" error — not silently
    // succeed with an empty result that is indistinguishable from a real node
    // with no dependents.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "graph impact on nonexistent file should exit non-zero, stderr: {stderr}"
    );
    assert!(
        stderr.contains("is not in the graph"),
        "stderr should identify the path as not in the graph, got: {stderr}"
    );

    let _ = fs::remove_dir_all(&dir);
}

// ─────────────────────── graph related (absent path) ─────────────────────

#[test]
fn graph_related_nonexistent_file_errors() {
    let dir = unique_tempdir("related");

    let cases: [&[&str]; 2] = [
        &["graph", "related", "nonexistent.rs"],
        &[
            "graph",
            "related",
            "nonexistent.rs",
            "--direction",
            "reverse",
        ],
    ];

    for args in cases {
        let output = Command::new(BIN)
            .args(args)
            .current_dir(&dir)
            .output()
            .expect("failed to spawn hilo graph related");

        // Contract (GAP-059): `related` must fail loudly on a path absent
        // from the graph AND from disk, exactly like `graph impact` does — a
        // silent "No outgoing/incoming edges" line with exit 0 is
        // indistinguishable from a real node with no edges.
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "graph related {args:?} on nonexistent file should exit non-zero, stderr: {stderr}"
        );
        assert!(
            stderr.contains("is not in the graph"),
            "stderr should identify the path as not in the graph, got: {stderr}"
        );
    }

    let _ = fs::remove_dir_all(&dir);
}

// ─────────────── graph related (GAP-083 file vs crate level) ───────────────

/// GAP-083: a reverse query on a file with ZERO file-level importers but N
/// crate-level ones must say so. The summary line comes first (so "0 files
/// import this" can never be mistaken for "N files import this file's crate")
/// and every row is explicitly labelled crate-level instead of the misleading
/// `[external package]` tag, which hid that the row is crate-scoped.
#[test]
fn graph_related_reverse_reports_file_level_and_crate_level_counts() {
    let dir = unique_tempdir("related-crate-level");

    // Two-crate workspace: only crates/b imports crate `a`, so the sole
    // dependents of crates/a/src/lib.rs live on the `pkg:a` node.
    fs::create_dir_all(dir.join("crates/a/src")).expect("failed to create crates/a/src");
    fs::create_dir_all(dir.join("crates/b/src")).expect("failed to create crates/b/src");
    fs::write(
        dir.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/a\", \"crates/b\"]\n",
    )
    .expect("failed to write workspace Cargo.toml");
    fs::write(
        dir.join("crates/a/Cargo.toml"),
        "[package]\nname = \"a\"\nversion = \"0.1.0\"\n",
    )
    .expect("failed to write crates/a/Cargo.toml");
    fs::write(dir.join("crates/a/src/lib.rs"), "pub struct Glob;\n")
        .expect("failed to write crates/a/src/lib.rs");
    fs::write(
        dir.join("crates/b/Cargo.toml"),
        "[package]\nname = \"b\"\nversion = \"0.1.0\"\n",
    )
    .expect("failed to write crates/b/Cargo.toml");
    fs::write(
        dir.join("crates/b/src/main.rs"),
        "use a::Glob;\nfn main() {}\n",
    )
    .expect("failed to write crates/b/src/main.rs");

    let init = Command::new(BIN)
        .arg("init")
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo init");
    assert!(
        init.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    // The importer's edge must exist in the graph: `related` only lazily parses
    // the QUERIED file, so warm is what puts crates/b/src/main.rs in.
    let warm = Command::new(BIN)
        .args(["graph", "warm"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo graph warm");
    assert!(
        warm.status.success(),
        "graph warm failed: {}",
        String::from_utf8_lossy(&warm.stderr)
    );

    let output = Command::new(BIN)
        .args([
            "graph",
            "related",
            "crates/a/src/lib.rs",
            "--direction",
            "reverse",
        ])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo graph related");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "graph related failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(!lines.is_empty(), "expected output, got: {stdout:?}");

    // 1. The summary line comes FIRST and names 0 direct (file-level)
    //    dependents plus the real crate-level count and node.
    let summary = lines[0];
    assert!(
        summary.starts_with("0 direct (file-level) dependents;"),
        "first line must be the file-level summary, got: {summary:?}"
    );
    assert!(
        summary.contains("1 crate-level dependents via pkg:a"),
        "summary must name the crate-level count and node, got: {summary:?}"
    );

    // 2. Rows come after the summary; here every row is crate-level and must
    //    say so explicitly (and never be called an external package — pkg:a is
    //    an in-repo workspace member).
    assert!(
        lines.len() > 1,
        "crate-level rows must still be listed after the summary: {stdout:?}"
    );
    for row in &lines[1..] {
        assert!(
            row.contains("[crate-level pkg:a]"),
            "every crate-level row must be explicitly labelled, got: {row:?}"
        );
        assert!(
            !row.contains("[external package]"),
            "an in-repo crate node must not be called an external package: {row:?}"
        );
    }

    let _ = fs::remove_dir_all(&dir);
}

// ─────────────── project preconditions (GAP-086) ───────────────

#[test]
fn graph_warm_without_init_errors_naming_init() {
    // A tree that is not a Hilo project must be refused: warm used to walk it
    // and leave a partial `.vfs/graph/` (parse cache, edges.jsonl, DuckDB
    // cache) with no manifest and none of the standard `.vfs/` layout.
    let dir = unique_tempdir("warm-without-init");
    let src = dir.join("src");
    fs::create_dir_all(&src).expect("failed to create src");
    fs::write(
        src.join("main.go"),
        "package main\nimport \"fmt\"\nfunc main() { fmt.Println(\"hi\") }\n",
    )
    .expect("failed to write main.go");

    let output = Command::new(BIN)
        .args(["graph", "warm"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo graph warm");

    assert!(
        !output.status.success(),
        "warm without a project must exit non-zero; stdout was:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("hilo init"),
        "the error must name the fix (`hilo init`), got: {stderr}"
    );
    assert!(
        !dir.join(".vfs").exists(),
        "a refused warm must not scatter .vfs state into a non-project tree"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn graph_warm_without_init_creates_no_partial_graph() {
    // Same refusal, asserted on the exact artifacts the defect produced: no
    // parse cache, no edges, no DuckDB cache, no warm marker.
    let dir = unique_tempdir("warm-without-init-artifacts");
    fs::write(dir.join("lib.rs"), "fn lib() {}\n").expect("failed to write lib.rs");

    let output = Command::new(BIN)
        .args(["graph", "warm"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo graph warm");
    assert!(!output.status.success(), "warm without init must fail");

    for artifact in [
        ".vfs/graph/.parse_cache.json",
        ".vfs/graph/.last_warm",
        ".vfs/graph/.last_reconcile",
        ".vfs/graph/edges.jsonl",
        ".vfs/graph/graph.db",
    ] {
        assert!(
            !dir.join(artifact).exists(),
            "{artifact} must not exist after a refused warm"
        );
    }

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn graph_warm_after_init_still_warms() {
    // Positive control for the refusal: with a manifest present the same tree
    // warms exactly as before, producing the graph artifacts.
    let dir = unique_tempdir("warm-after-init");
    let src = dir.join("src");
    fs::create_dir_all(&src).expect("failed to create src");
    fs::write(
        src.join("main.go"),
        "package main\nimport \"fmt\"\nfunc main() { fmt.Println(\"hi\") }\n",
    )
    .expect("failed to write main.go");

    init_project(&dir);
    let stdout = run_hilo_ok(&dir, &["graph", "warm"]);
    assert!(
        stdout.contains("Discovered"),
        "warm must report its discovery summary, got: {stdout}"
    );
    assert!(
        dir.join(".vfs").join("graph").join("edges.jsonl").exists(),
        "init + warm must still produce edges.jsonl"
    );

    let _ = fs::remove_dir_all(&dir);
}

// ─────────────── ignored cache artifacts (GAP-086) ───────────────

/// The repository root — this crate lives in `<root>/hilo-cli`.
#[cfg(unix)]
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("hilo-cli must have a parent directory")
        .to_path_buf()
}

/// `git` in `dir`, isolated from the developer's global/system config so the
/// verdict comes from the repository's own committed `.gitignore`.
#[cfg(unix)]
fn git_in(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(args)
        .output()
        .expect("failed to run git")
}

#[cfg(unix)]
#[test]
fn repo_gitignore_ignores_only_the_rebuildable_cache_artifacts() {
    let root = repo_root();
    let out = git_in(
        &root,
        &[
            "check-ignore",
            "-v",
            ".vfs/graph/.parse_cache.json",
            ".vfs/graph/.last_reconcile",
        ],
    );
    assert!(
        out.status.success(),
        "both cache artifacts must be ignored, check-ignore said: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        text.lines().count(),
        2,
        "expected one ignore verdict per artifact, got: {text}"
    );
    assert!(
        text.lines().all(|l| l.starts_with(".gitignore:")),
        "the rules must come from the committed .gitignore, got: {text}"
    );

    // Precision: inventory truth must NOT be swallowed by a broad rule.
    for path in [".vfs/manifest.yaml", ".vfs/graph/edges.jsonl"] {
        let out = git_in(&root, &["check-ignore", "-v", path]);
        assert!(
            !out.status.success(),
            "{path} is inventory truth and must stay visible, but it is ignored: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        let tracked = git_in(&root, &["ls-files", "--error-unmatch", path]);
        assert!(
            tracked.status.success(),
            "{path} must be tracked in this repository: {}",
            String::from_utf8_lossy(&tracked.stderr)
        );
    }
}

#[cfg(unix)]
#[test]
fn init_warm_classify_leaves_no_untracked_cache_artifacts_in_a_fresh_clone() {
    let dir = unique_tempdir("gap086-fresh-clone");
    // A fresh clone's ignore rules come from the repo's committed .gitignore.
    fs::copy(repo_root().join(".gitignore"), dir.join(".gitignore"))
        .expect("failed to copy the repository .gitignore");

    // A real project: two Go files so warm emits edges and reconcile stamps.
    let src = dir.join("src");
    fs::create_dir_all(&src).expect("failed to create src");
    fs::write(
        src.join("main.go"),
        "package main\nimport \"fmt\"\nfunc main() { fmt.Println(\"hi\") }\n",
    )
    .expect("failed to write main.go");
    fs::write(
        src.join("helper.go"),
        "package main\nfunc Helper() string { return \"help\" }\n",
    )
    .expect("failed to write helper.go");

    let init = git_in(&dir, &["init", "-q"]);
    assert!(
        init.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    init_project(&dir);
    run_hilo_ok(&dir, &["graph", "warm"]);
    run_hilo_ok(&dir, &["classify"]);

    // Premise: the artifacts the acceptance criterion names really exist.
    for artifact in [".vfs/graph/.parse_cache.json", ".vfs/graph/.last_reconcile"] {
        assert!(
            dir.join(artifact).exists(),
            "premise failed: {artifact} must exist after init+warm+classify"
        );
    }

    let status = git_in(
        &dir,
        &[
            "status",
            "--porcelain",
            "-uall",
            "--",
            ".vfs/graph/.parse_cache.json",
            ".vfs/graph/.last_reconcile",
        ],
    );
    assert!(
        status.status.success(),
        "git status failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let text = String::from_utf8_lossy(&status.stdout);
    assert!(
        text.is_empty(),
        "cache artifacts must not be untracked after init+warm+classify: {text}"
    );

    // Precision control in the same tree: inventory truth is still reported
    // as untracked (nothing committed here), so the ignore rule hides only
    // the cache files.
    let inventory = git_in(
        &dir,
        &[
            "status",
            "--porcelain",
            "-uall",
            "--",
            ".vfs/manifest.yaml",
            ".vfs/graph/edges.jsonl",
        ],
    );
    let text = String::from_utf8_lossy(&inventory.stdout);
    for path in [".vfs/manifest.yaml", ".vfs/graph/edges.jsonl"] {
        assert!(
            text.contains(&format!("?? {path}")),
            "{path} must stay visible as untracked, got: {text}"
        );
    }

    let _ = fs::remove_dir_all(&dir);
}

// ─────────────────────── serve ───────────────────────

#[test]
fn serve_mcp_exits_cleanly_on_eof() {
    // `serve --mcp` starts the MCP stdio server.  With no stdin piped
    // (Command::output gives an empty/closed stdin) the server reads EOF
    // immediately and exits 0.  GAP-086: the server requires a project root,
    // so run it in an initialized project (an empty one is valid).
    let dir = unique_tempdir("serve-eof");
    init_project(&dir);
    let output = Command::new(BIN)
        .args(["serve", "--mcp"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo serve --mcp");

    assert!(
        output.status.success(),
        "serve --mcp should exit 0 on stdin EOF: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("MCP server started"),
        "an initialized (empty) project must start the server"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn serve_mcp_without_project_errors_naming_init() {
    // GAP-086: outside a Hilo project the server used to start and answer
    // from a zeroed graph. It must refuse up front and name `hilo init`.
    let dir = unique_tempdir("serve-without-project");
    let output = Command::new(BIN)
        .args(["serve", "--mcp"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo serve --mcp");

    assert!(
        !output.status.success(),
        "serve --mcp outside a project must exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("hilo init"),
        "the error must name the fix (`hilo init`), got: {stderr}"
    );
    assert!(
        !stderr.contains("MCP server started"),
        "the server must not start before the project precondition: {stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "a refused server must not emit JSON-RPC bytes on stdout: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn serve_without_flag_errors() {
    let output = Command::new(BIN)
        .args(["serve"])
        .output()
        .expect("failed to spawn hilo serve");

    assert!(
        !output.status.success(),
        "serve without --mcp should exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--mcp"),
        "stderr should mention --mcp, got: {stderr}"
    );
}

#[test]
fn mcp_stdio_stdout_is_pure_jsonrpc() {
    use std::io::Write;
    use std::process::Stdio;

    let dir = unique_tempdir("mcp-purity");
    // GAP-086: the MCP server requires a project root.
    init_project(&dir);
    let mut child = Command::new(BIN)
        .args(["serve", "--mcp"])
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn hilo serve --mcp");

    // Naive client: send initialize -> tools/list -> tools/call, then EOF.
    {
        let stdin = child.stdin.as_mut().expect("stdin not piped");
        stdin
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n")
            .expect("failed to write initialize");
        stdin
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n")
            .expect("failed to write tools/list");
        stdin
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"vfs_get_metadata\",\"arguments\":{\"path\":\"/nonexistent/hilo-mcp-purity-test\"}}}\n")
            .expect("failed to write tools/call");
    } // stdin dropped -> EOF -> server exits

    let output = child
        .wait_with_output()
        .expect("failed to wait for hilo serve --mcp");
    assert!(
        output.status.success(),
        "serve --mcp exited non-zero: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Every stdout line must be a JSON-RPC response — zero non-JSON-RPC bytes.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        3,
        "expected exactly 3 JSON-RPC responses, got {lines:?}"
    );
    for (line, id) in lines.iter().zip([1i64, 2, 3]) {
        let v: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("stdout line is not valid JSON ({e}): {line:?}"));
        assert_eq!(v["jsonrpc"], "2.0", "stdout line is not JSON-RPC: {line}");
        assert_eq!(v["id"].as_i64(), Some(id), "response id mismatch: {line}");
    }

    // Tracing logs must have gone to stderr, proving stdout is protocol-only.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("MCP server started"),
        "startup log should be on stderr, got: {stderr}"
    );

    let _ = fs::remove_dir_all(&dir);
}

// ─────────────────────── ignore check ───────────────────────

#[test]
fn ignore_check_reports_decision_and_rule() {
    let dir = unique_tempdir("ignore-check");
    fs::write(dir.join(".hiloignore"), "*.bin\nbuild/\n!keep.bin\n").expect("write .hiloignore");

    // Ignored path: prints ignored:true with the matching rule.
    let out = Command::new(BIN)
        .args(["ignore", "check", "a.bin"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo ignore check");
    assert!(
        out.status.success(),
        "ignore check exited non-zero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("ignored: true"),
        "expected ignored:true, got: {stdout}"
    );
    assert!(
        stdout.contains("rule: *.bin"),
        "expected rule line, got: {stdout}"
    );
    assert!(
        stdout.contains("source: "),
        "expected source line, got: {stdout}"
    );

    // Re-included path: not ignored, but the deciding rule is reported.
    let out = Command::new(BIN)
        .args(["ignore", "check", "keep.bin"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo ignore check");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("ignored: false"),
        "expected ignored:false for re-included path, got: {stdout}"
    );
    assert!(
        stdout.contains("rule: !keep.bin"),
        "expected negation rule reported, got: {stdout}"
    );

    // Unmatched path: not ignored, no rule.
    let out = Command::new(BIN)
        .args(["ignore", "check", "src/main.rs"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo ignore check");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("ignored: false"),
        "expected ignored:false for unmatched path, got: {stdout}"
    );
    assert!(
        stdout.contains("rule: (none)"),
        "expected rule: (none) for unmatched path, got: {stdout}"
    );

    let _ = fs::remove_dir_all(&dir);
}

// ─────────────────────── ignore check: builtins + nested ───────────────────────

#[test]
fn ignore_check_reports_builtin_defaults_and_no_defaults_flag() {
    let dir = unique_tempdir("ignore-check-builtins");
    // No .hiloignore: built-in defaults apply (spec §4.2).
    let out = Command::new(BIN)
        .args(["ignore", "check", "target/artifact.bin"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo ignore check");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("ignored: true"),
        "builtin target/ should apply: {stdout}"
    );
    assert!(
        stdout.contains("source: builtin defaults"),
        "expected builtin source, got: {stdout}"
    );

    // --no-default-ignores disables the builtins.
    let out = Command::new(BIN)
        .args([
            "ignore",
            "check",
            "target/artifact.bin",
            "--no-default-ignores",
        ])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo ignore check");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("ignored: false"),
        "no-default-ignores should disable builtins, got: {stdout}"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn ignore_check_reports_nested_ignore_source() {
    let dir = unique_tempdir("ignore-check-nested");
    fs::create_dir(dir.join("sub")).expect("mkdir sub");
    fs::write(dir.join("sub/.hiloignore"), "secret.txt\n").expect("write nested");
    let out = Command::new(BIN)
        .args(["ignore", "check", "sub/secret.txt"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo ignore check");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("ignored: true"),
        "nested rule should apply, got: {stdout}"
    );
    assert!(
        stdout.contains("source: nested sub"),
        "expected nested source, got: {stdout}"
    );
    let _ = fs::remove_dir_all(&dir);
}

// ─────────────────────── workspace ephemeral / wipe ───────────────────────

/// Build a scratch workspace: src/main.rs (persistent), target/artifact.bin
/// and node_modules/pkg/index.js (ephemeral by the built-in catalog).
fn ephemeral_fixture(label: &str) -> PathBuf {
    let dir = unique_tempdir(label);
    fs::create_dir_all(dir.join("src")).expect("mkdir src");
    fs::create_dir_all(dir.join("target")).expect("mkdir target");
    fs::create_dir_all(dir.join("node_modules/pkg")).expect("mkdir node_modules");
    fs::write(dir.join("src/main.rs"), "fn main() {}\n").expect("write main.rs");
    fs::write(dir.join("target/artifact.bin"), vec![0u8; 64]).expect("write artifact");
    fs::write(dir.join("node_modules/pkg/index.js"), vec![0u8; 32]).expect("write index");
    dir
}

#[test]
fn workspace_ephemeral_lists_ephemeral_files_as_tsv() {
    let dir = ephemeral_fixture("ephemeral-list");
    let out = Command::new(BIN)
        .args(["workspace", "ephemeral"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo workspace ephemeral");
    assert!(
        out.status.success(),
        "workspace ephemeral exited non-zero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("target/artifact.bin\t64\t"),
        "expected ephemeral TSV row for target/artifact.bin, got: {stdout}"
    );
    assert!(
        stdout.contains("node_modules/pkg/index.js\t32\t"),
        "expected ephemeral TSV row for node_modules/pkg/index.js, got: {stdout}"
    );
    assert!(
        !stdout.contains("src/main.rs"),
        "persistent src/main.rs must not be listed, got: {stdout}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn workspace_ephemeral_path_filter_limits_listing() {
    let dir = ephemeral_fixture("ephemeral-filter");
    let out = Command::new(BIN)
        .args(["workspace", "ephemeral", "node_modules"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo workspace ephemeral");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("node_modules/pkg/index.js"),
        "expected node_modules row, got: {stdout}"
    );
    assert!(
        !stdout.contains("target/artifact.bin"),
        "path filter must exclude target/, got: {stdout}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn workspace_wipe_dry_run_lists_plan_without_deleting() {
    let dir = ephemeral_fixture("wipe-dry");
    let out = Command::new(BIN)
        .args(["workspace", "wipe", "--ephemeral"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo workspace wipe");
    assert!(
        out.status.success(),
        "workspace wipe exited non-zero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("would remove\ttarget/artifact.bin"),
        "expected dry-run row, got: {stdout}"
    );
    assert!(
        stdout.contains("would free 96 bytes across 2 file(s)"),
        "expected freed-bytes summary, got: {stdout}"
    );
    // Nothing deleted on a dry run.
    assert!(
        dir.join("target/artifact.bin").exists(),
        "dry-run must not delete"
    );
    assert!(dir.join("src/main.rs").exists(), "src must survive");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn workspace_wipe_apply_deletes_only_ephemeral_and_reports_freed() {
    let dir = ephemeral_fixture("wipe-apply");
    let out = Command::new(BIN)
        .args(["workspace", "wipe", "--ephemeral", "--apply"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo workspace wipe");
    assert!(
        out.status.success(),
        "workspace wipe --apply exited non-zero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("removed\ttarget/artifact.bin"),
        "expected removed row, got: {stdout}"
    );
    assert!(
        stdout.contains("freed 96 bytes across 2 file(s)"),
        "expected freed summary, got: {stdout}"
    );
    assert!(
        !dir.join("target/artifact.bin").exists(),
        "ephemeral file must be deleted"
    );
    assert!(
        !dir.join("node_modules/pkg/index.js").exists(),
        "ephemeral file must be deleted"
    );
    assert!(
        dir.join("src/main.rs").exists(),
        "persistent src/main.rs must survive the wipe"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn workspace_wipe_respects_hiloephemeral_negation() {
    let dir = ephemeral_fixture("wipe-negation");
    fs::write(dir.join(".hiloephemeral"), "!target/keep.bin\n").expect("write .hiloephemeral");
    fs::write(dir.join("target/keep.bin"), vec![0u8; 8]).expect("write keep.bin");

    let out = Command::new(BIN)
        .args(["workspace", "wipe", "--ephemeral", "--apply"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo workspace wipe");
    assert!(
        out.status.success(),
        "workspace wipe exited non-zero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // target/keep.bin is re-included by the user rule: it survives; the
    // other ephemeral files are still removed.
    assert!(
        dir.join("target/keep.bin").exists(),
        "negated path must survive the wipe"
    );
    assert!(
        !dir.join("target/artifact.bin").exists(),
        "un-negated ephemeral file must be deleted"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn e2e_ephemeral_sync_wipe_and_regenerate_loop() {
    // Spec §15 E2E (LocalDriver as backend): init → persistent + ephemeral
    // files → `backend sync --push` transfers only persistent files → wipe
    // dry-run lists only ephemeral → --apply frees bytes → rebuild
    // regenerates the artifact (reclassified ephemeral) → `.vfs/graph/`
    // (ephemeral catalog) wiped → `graph clean && graph warm` rebuild it.
    let dir = unique_tempdir("e2e-eph");
    let workspace = dir.join("workspace");
    let backend_root = dir.join("backend-root");
    fs::create_dir_all(&workspace).expect("failed to create workspace");
    fs::create_dir_all(&backend_root).expect("failed to create backend root");

    // init first so .vfs/manifest.yaml exists (never ephemeral).
    let init_output = Command::new(BIN)
        .arg("init")
        .current_dir(&workspace)
        .output()
        .expect("failed to spawn hilo init");
    assert!(
        init_output.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init_output.stderr)
    );

    // Persistent file (src/main.rs) + ephemeral file (target/artifact.bin).
    // main.go/helper.go give `graph warm` real edges (proven pattern from
    // graph_warm_creates_graph_directory).
    fs::create_dir_all(workspace.join("src")).expect("failed to create src");
    fs::create_dir_all(workspace.join("target")).expect("failed to create target");
    fs::write(workspace.join("src/main.rs"), "fn main() {}\n").expect("failed to write main.rs");
    fs::write(
        workspace.join("src/main.go"),
        "package main\nimport \"fmt\"\nfunc main() { fmt.Println(\"hi\") }\n",
    )
    .expect("failed to write main.go");
    fs::write(
        workspace.join("src/helper.go"),
        "package main\nfunc Helper() string { return \"help\" }\n",
    )
    .expect("failed to write helper.go");
    fs::write(workspace.join("target/artifact.bin"), vec![0u8; 64])
        .expect("failed to write artifact.bin");

    write_mounts_yaml(
        &workspace,
        &format!(
            "- name: test\n  type: local\n  prefix: {}\n  mode: mirror\n",
            backend_root.display()
        ),
    );

    // (1) push: ephemeral target/artifact.bin must NOT go upstream.
    let pushed = Command::new(BIN)
        .args(["backend", "sync", "--push"])
        .current_dir(&workspace)
        .output()
        .expect("failed to spawn hilo backend sync");
    assert!(
        pushed.status.success(),
        "sync --push failed: {}",
        String::from_utf8_lossy(&pushed.stderr)
    );
    let pushed_stdout = String::from_utf8_lossy(&pushed.stdout);
    assert!(
        pushed_stdout.contains("skipped ephemeral"),
        "expected skipped-ephemeral counter: {pushed_stdout}"
    );
    assert!(
        backend_root.join("src/main.rs").exists(),
        "persistent src/main.rs must be upstream"
    );
    assert!(
        !backend_root.join("target/artifact.bin").exists(),
        "ephemeral target/artifact.bin must NOT be upstream"
    );

    // (2) wipe dry-run lists only ephemeral files.
    let dry = Command::new(BIN)
        .args(["workspace", "wipe", "--ephemeral"])
        .current_dir(&workspace)
        .output()
        .expect("failed to spawn hilo workspace wipe");
    assert!(
        dry.status.success(),
        "wipe dry-run failed: {}",
        String::from_utf8_lossy(&dry.stderr)
    );
    let dry_stdout = String::from_utf8_lossy(&dry.stdout);
    assert!(
        dry_stdout.contains("would remove\ttarget/artifact.bin"),
        "dry-run must list target/artifact.bin: {dry_stdout}"
    );
    assert!(
        !dry_stdout.contains("would remove\tsrc/main.rs"),
        "persistent src/main.rs must not be listed: {dry_stdout}"
    );

    // (3) wipe --apply removes only ephemeral, reports freed bytes, and
    // never touches .vfs/manifest.yaml (workspace truth).
    let applied = Command::new(BIN)
        .args(["workspace", "wipe", "--ephemeral", "--apply"])
        .current_dir(&workspace)
        .output()
        .expect("failed to spawn hilo workspace wipe --apply");
    assert!(
        applied.status.success(),
        "wipe --apply failed: {}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let applied_stdout = String::from_utf8_lossy(&applied.stdout);
    assert!(
        applied_stdout.contains("removed\ttarget/artifact.bin"),
        "expected removed row: {applied_stdout}"
    );
    assert!(
        applied_stdout.contains("freed 64 bytes across 1 file(s)"),
        "expected freed summary: {applied_stdout}"
    );
    assert!(
        !workspace.join("target/artifact.bin").exists(),
        "ephemeral file must be deleted"
    );
    assert!(
        workspace.join("src/main.rs").exists(),
        "persistent file must survive"
    );
    assert!(
        workspace.join(".vfs/manifest.yaml").exists(),
        ".vfs/manifest.yaml must never be ephemeral"
    );

    // (4) regenerable: a rebuilt artifact is classified ephemeral again.
    fs::write(workspace.join("target/artifact.bin"), vec![0u8; 64])
        .expect("failed to rebuild artifact.bin");
    let relist = Command::new(BIN)
        .args(["workspace", "ephemeral"])
        .current_dir(&workspace)
        .output()
        .expect("failed to spawn hilo workspace ephemeral");
    assert!(
        relist.status.success(),
        "workspace ephemeral failed: {}",
        String::from_utf8_lossy(&relist.stderr)
    );
    assert!(
        String::from_utf8_lossy(&relist.stdout).contains("target/artifact.bin\t64\t"),
        "regenerated artifact must be listed ephemeral again"
    );

    // (5) graph rebuild path: .vfs/graph/ is in the ephemeral catalog;
    // wiping it and re-warming must still work.
    let warm = Command::new(BIN)
        .args(["graph", "warm"])
        .current_dir(&workspace)
        .output()
        .expect("failed to spawn hilo graph warm");
    assert!(
        warm.status.success(),
        "graph warm failed: {}",
        String::from_utf8_lossy(&warm.stderr)
    );
    assert!(
        workspace.join(".vfs/graph/edges.jsonl").exists(),
        "graph warm must produce edges.jsonl"
    );
    let wipe_graph = Command::new(BIN)
        .args(["workspace", "wipe", "--ephemeral", "--apply"])
        .current_dir(&workspace)
        .output()
        .expect("failed to spawn wipe after warm");
    assert!(
        wipe_graph.status.success(),
        "wipe after warm failed: {}",
        String::from_utf8_lossy(&wipe_graph.stderr)
    );
    assert!(
        !workspace.join(".vfs/graph/edges.jsonl").exists(),
        "wiped .vfs/graph/edges.jsonl must be gone"
    );
    let clean = Command::new(BIN)
        .args(["graph", "clean"])
        .current_dir(&workspace)
        .output()
        .expect("failed to spawn hilo graph clean");
    assert!(
        clean.status.success(),
        "graph clean failed: {}",
        String::from_utf8_lossy(&clean.stderr)
    );
    let rewarm = Command::new(BIN)
        .args(["graph", "warm"])
        .current_dir(&workspace)
        .output()
        .expect("failed to spawn hilo graph warm after clean");
    assert!(
        rewarm.status.success(),
        "graph warm after clean failed: {}",
        String::from_utf8_lossy(&rewarm.stderr)
    );
    assert!(
        workspace.join(".vfs/graph/edges.jsonl").exists(),
        "graph must rebuild after wipe"
    );

    let _ = fs::remove_dir_all(&dir);
}

// ─────────────────────── backend mount/sync/setup (§9) ───────────────────────

fn write_mounts_yaml(workspace: &std::path::Path, yaml: &str) {
    let dir = workspace.join(".vfs").join("backends");
    fs::create_dir_all(&dir).expect("failed to create .vfs/backends");
    fs::write(dir.join("mounts.yaml"), yaml).expect("failed to write mounts.yaml");
}

#[test]
fn backend_mount_new_surface_writes_mounts_yaml() {
    let dir = unique_tempdir("backend-mount");
    let output = Command::new(BIN)
        .args([
            "backend",
            "mount",
            "--type",
            "s3",
            "--bucket",
            "my-bucket",
            "--prefix",
            "workspace/",
            "--at",
            "/mnt/vfs/ws",
            "--tool",
            "native",
            "--mode",
            "mirror",
        ])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo backend mount");

    assert!(
        output.status.success(),
        "mount exited non-zero: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("mounted s3 s3://my-bucket/workspace/ at /mnt/vfs/ws"),
        "unexpected stdout: {stdout}"
    );

    let mounts = dir.join(".vfs").join("backends").join("mounts.yaml");
    assert!(mounts.exists(), "mounts.yaml was not written");
    let yaml = fs::read_to_string(&mounts).expect("failed to read mounts.yaml");
    assert!(yaml.contains("name: ws"), "mount name missing: {yaml}");
    assert!(yaml.contains("type: s3"), "type missing: {yaml}");
    assert!(yaml.contains("bucket: my-bucket"), "bucket missing: {yaml}");
    assert!(yaml.contains("at: /mnt/vfs/ws"), "at missing: {yaml}");
    assert!(yaml.contains("tool: native"), "tool missing: {yaml}");

    // Second mount with the same name must be rejected (exit 2, InvalidConfig).
    let dup = Command::new(BIN)
        .args([
            "backend",
            "mount",
            "--type",
            "s3",
            "--bucket",
            "other",
            "--at",
            "/mnt/vfs/ws",
            "--tool",
            "native",
        ])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn duplicate mount");
    assert_eq!(dup.status.code(), Some(2), "duplicate mount must exit 2");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn backend_mount_missing_tool_exits_4() {
    let dir = unique_tempdir("backend-mount-tool");
    let output = Command::new(BIN)
        .args([
            "backend",
            "mount",
            "--type",
            "gdrive",
            "--remote",
            "test:path",
            "--at",
            "/mnt/vfs/gd",
            "--tool",
            "gdrive",
        ])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo backend mount");

    // gdrive CLI is not installed in CI; ToolMissing must exit 4 (§12).
    assert_eq!(
        output.status.code(),
        Some(4),
        "expected exit 4, got {:?}; stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("required tool not found"),
        "expected ToolMissing message, got: {stderr}"
    );
    // Nothing may be written on failure.
    assert!(
        !dir.join(".vfs").exists(),
        "mounts.yaml must not be written on failed mount"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn backend_sync_local_pushes_pulls_and_filters() {
    let dir = unique_tempdir("backend-sync");
    let workspace = dir.join("workspace");
    let backend_root = dir.join("backend-root");
    fs::create_dir_all(&workspace).expect("failed to create workspace");
    fs::create_dir_all(&backend_root).expect("failed to create backend root");

    // Ignore *.tmp locally; never synced.
    fs::write(workspace.join(".hiloignore"), "*.tmp\n").expect("failed to write .hiloignore");
    fs::write(workspace.join("a.txt"), "hello\n").expect("failed to write a.txt");
    fs::write(workspace.join("x.tmp"), "local-only\n").expect("failed to write x.tmp");
    fs::create_dir_all(workspace.join("sub")).expect("failed to create sub");
    fs::write(workspace.join("sub/c.txt"), "nested\n").expect("failed to write sub/c.txt");

    write_mounts_yaml(
        &workspace,
        &format!(
            "- name: test\n  type: local\n  prefix: {}\n  mode: mirror\n",
            backend_root.display()
        ),
    );

    // Subtree filter: only sub/ is pushed.
    let filtered = Command::new(BIN)
        .args(["backend", "sync", "--push", "sub"])
        .current_dir(&workspace)
        .output()
        .expect("failed to spawn hilo backend sync");
    assert!(
        filtered.status.success(),
        "sync --push sub failed: {}",
        String::from_utf8_lossy(&filtered.stderr)
    );
    assert!(
        backend_root.join("sub/c.txt").exists(),
        "sub/c.txt should be pushed"
    );
    assert!(
        !backend_root.join("a.txt").exists(),
        "a.txt must not be pushed by a subtree-limited sync"
    );

    // Full push: a.txt lands, x.tmp stays local-only (ignored).
    let pushed = Command::new(BIN)
        .args(["backend", "sync", "--push"])
        .current_dir(&workspace)
        .output()
        .expect("failed to spawn hilo backend sync");
    assert!(
        pushed.status.success(),
        "sync --push failed: {}",
        String::from_utf8_lossy(&pushed.stderr)
    );
    let stdout = String::from_utf8_lossy(&pushed.stdout);
    assert!(
        stdout.contains("skipped ignored"),
        "expected skipped-ignored counter: {stdout}"
    );
    assert!(
        fs::read_to_string(backend_root.join("a.txt")).expect("read a.txt") == "hello\n",
        "a.txt content mismatch"
    );
    assert!(
        !backend_root.join("x.tmp").exists(),
        "ignored x.tmp must never be pushed"
    );

    // Idempotent: a second --both sync transfers nothing (equal mtimes after
    // mtime alignment → the spec's no-ping-pong tie-break).
    let again = Command::new(BIN)
        .args(["backend", "sync"])
        .current_dir(&workspace)
        .output()
        .expect("failed to spawn second sync");
    assert!(
        again.status.success(),
        "second sync failed: {}",
        String::from_utf8_lossy(&again.stderr)
    );
    assert!(
        String::from_utf8_lossy(&again.stdout).contains("0 to transfer"),
        "expected no-op second sync: {}",
        String::from_utf8_lossy(&again.stdout)
    );

    // Remote newer → pull updates the local copy.
    fs::write(backend_root.join("a.txt"), "from-remote\n").expect("failed to update remote");
    let pulled = Command::new(BIN)
        .args(["backend", "sync", "--pull"])
        .current_dir(&workspace)
        .output()
        .expect("failed to spawn pull sync");
    assert!(
        pulled.status.success(),
        "sync --pull failed: {}",
        String::from_utf8_lossy(&pulled.stderr)
    );
    assert!(
        fs::read_to_string(workspace.join("a.txt")).expect("read a.txt") == "from-remote\n",
        "pull did not update local copy"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn backend_sync_no_mounts_exits_2() {
    let dir = unique_tempdir("backend-sync-nomount");
    let output = Command::new(BIN)
        .args(["backend", "sync"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo backend sync");
    assert_eq!(
        output.status.code(),
        Some(2),
        "expected exit 2 without mounts, got {:?}",
        output.status.code()
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn backend_setup_reports_detection_and_next_steps() {
    let dir = unique_tempdir("backend-setup");
    let s3 = Command::new(BIN)
        .args(["backend", "setup", "--type", "s3"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo backend setup");
    assert!(
        s3.status.success(),
        "setup s3 failed: {}",
        String::from_utf8_lossy(&s3.stderr)
    );
    let stdout = String::from_utf8_lossy(&s3.stdout);
    assert!(stdout.contains("== s3 =="), "s3 header missing: {stdout}");
    assert!(
        stdout.contains("credentials:"),
        "credentials check missing: {stdout}"
    );
    assert!(
        stdout.contains("next steps:"),
        "next steps missing: {stdout}"
    );

    let gdrive = Command::new(BIN)
        .args(["backend", "setup", "--type", "gdrive"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo backend setup");
    assert!(
        gdrive.status.success(),
        "setup gdrive failed: {}",
        String::from_utf8_lossy(&gdrive.stderr)
    );
    let stdout = String::from_utf8_lossy(&gdrive.stdout);
    assert!(stdout.contains("gdrive:"), "gdrive line missing: {stdout}");
    assert!(stdout.contains("rclone:"), "rclone line missing: {stdout}");

    // Unknown type is a usage error (exit 1).
    let bad = Command::new(BIN)
        .args(["backend", "setup", "--type", "ftp"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo backend setup");
    assert!(!bad.status.success(), "unknown type must fail");
    let _ = fs::remove_dir_all(&dir);
}

// ─────────────────────── closed stdout pipe (GAP-063) ───────────────────────

/// GAP-063: `hilo` panicked with exit 101 when its stdout pipe closed early
/// (`hilo graph stats | head`, `... | true`).
///
/// The Rust runtime installs `SIG_IGN` for `SIGPIPE` before `main` runs, so a
/// write into a closed pipe surfaces as `EPIPE` / "Broken pipe (os error 32)"
/// and the `std` print macros escalate that write error into a panic. The CLI
/// now restores the default `SIGPIPE` disposition at startup, so it is killed
/// silently by the kernel (signal 13) like every other Unix filter.
///
/// The fixture MUST produce more than one pipe buffer of stdout (64 KiB on
/// Linux) and the test asserts that: a fixture whose output fits in the buffer
/// finishes writing before the reader goes away, so it would pass against the
/// unfixed binary too — a phantom test.
#[cfg(unix)]
#[test]
fn graph_stats_does_not_panic_when_stdout_closes_early() {
    use std::io::{BufRead, BufReader, Read};
    use std::os::unix::process::ExitStatusExt;
    use std::process::Stdio;

    /// Linux default pipe capacity (bytes). Output must exceed this for the
    /// child to still be writing once the read end is gone.
    const PIPE_BUFFER_BYTES: usize = 64 * 1024;
    /// `SIGPIPE`, as reported by `ExitStatus::signal()`.
    const SIGPIPE: i32 = 13;
    /// Enough standalone modules, with long relative paths, that
    /// `graph stats --limit 0` prints roughly 90 KiB of orphans.
    const MODULE_COUNT: usize = 700;
    const LONG_NAME: &str = "this_is_a_deliberately_long_generated_module_file_name_for_the_broken_pipe_regression_test";

    let dir = unique_tempdir("closed-stdout-pipe");

    // Each module imports only the stdlib, so it is an orphan (no incoming
    // edges) and appears in the `--limit 0` orphan dump; the stdlib `fmt`
    // import is what gives the graph its edges at all (an edge-less graph
    // short-circuits to "Graph cache is empty" and prints nothing).
    for i in 0..MODULE_COUNT {
        let section = dir
            .join("src")
            .join("generated")
            .join("pkgs")
            .join(format!("section_{:02}", i % 10));
        fs::create_dir_all(&section).expect("failed to create fixture section dir");
        fs::write(
            section.join(format!("{LONG_NAME}_{i:05}.go")),
            format!("package orphan\n\nimport \"fmt\"\n\nfunc F{i}() {{ fmt.Println({i}) }}\n"),
        )
        .expect("failed to write fixture module");
    }
    fs::write(
        dir.join("src").join("hub.go"),
        "package main\n\nimport \"fmt\"\n\nfunc Hub() { fmt.Println(\"hub\") }\n",
    )
    .expect("failed to write hub.go");

    let init = Command::new(BIN)
        .arg("init")
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo init");
    assert!(
        init.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let warm = Command::new(BIN)
        .args(["graph", "warm"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo graph warm");
    assert!(
        warm.status.success(),
        "graph warm failed: {}",
        String::from_utf8_lossy(&warm.stderr)
    );

    // 1. Prove the fixture really emits more than one pipe buffer, so the
    //    closed-pipe scenario below is genuine and not a phantom.
    let full = Command::new(BIN)
        .args(["graph", "stats", "--limit", "0"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn hilo graph stats");
    assert!(
        full.status.success(),
        "graph stats failed: {}",
        String::from_utf8_lossy(&full.stderr)
    );
    let full_len = full.stdout.len();
    eprintln!("graph stats --limit 0 produced {full_len} bytes of stdout");
    assert!(
        full_len > PIPE_BUFFER_BYTES,
        "fixture must produce more than one {PIPE_BUFFER_BYTES}-byte pipe buffer of \
         stdout (got {full_len} bytes) or the child finishes writing before the \
         reader is dropped and this test cannot exercise a closed pipe"
    );

    // 2. Read one line, then drop the read end while the child is still
    //    writing; the child must not panic.
    let mut child = Command::new(BIN)
        .args(["graph", "stats", "--limit", "0"])
        .current_dir(&dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn graph stats with a piped stdout");

    let stdout = child.stdout.take().expect("stdout was not piped");
    {
        let mut reader = BufReader::new(stdout);
        let mut first_line = String::new();
        reader
            .read_line(&mut first_line)
            .expect("failed to read the first line of graph stats");
        assert!(
            first_line.contains("Total edges:"),
            "unexpected first line from graph stats: {first_line:?}"
        );
        // `reader` — and with it the only read end of the pipe — is dropped
        // here, while the child is still writing.
    }

    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr was not piped")
        .read_to_string(&mut stderr)
        .expect("failed to read graph stats stderr");
    let status = child.wait().expect("failed to wait for graph stats");

    assert!(
        !stderr.contains("panicked"),
        "hilo must not panic when its stdout pipe closes early, stderr was:\n{stderr}"
    );
    assert!(
        !stderr.contains("Broken pipe"),
        "hilo must not report a broken-pipe error when its stdout pipe closes early, \
         stderr was:\n{stderr}"
    );
    assert_ne!(
        status.code(),
        Some(101),
        "hilo exited 101 (panic) when its stdout pipe closed early, stderr was:\n{stderr}"
    );
    assert!(
        status.code() == Some(0) || status.signal() == Some(SIGPIPE),
        "expected exit 0 or death by SIGPIPE ({SIGPIPE}), got code={:?} signal={:?}, \
         stderr was:\n{stderr}",
        status.code(),
        status.signal()
    );

    let _ = fs::remove_dir_all(&dir);
}
