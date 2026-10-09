//! GAP-098 — flow symbol recall on the flask and gin request flows.
//!
//! `graph understand` used to list each file's first 8 definitions in source
//! order, so the definitions a flow question is *about* could not appear once a
//! file held more than eight. Both of the bake-off's architecture/flow questions
//! regressed against that cap:
//!
//! * `flask.A2` — "request flow WSGI->view->response": `wsgi_app` is app.py's
//!   41st of 42 definitions and `full_dispatch_request` / `dispatch_request` /
//!   `finalize_request` sit at 25–27; measured **1/4** symbols on the shipped
//!   binary (only `dispatch_request` surfaced).
//! * `gin.A2` — "request dispatch flow ServeHTTP->tree->handler":
//!   `ServeHTTP`/`handleHTTPRequest` are gin.go's 43rd and 45th of 50 and
//!   `getValue` is tree.go's 20th of 23; measured **1/4** (only `Next`).
//!
//! The fix (`SYMBOL_FLOOR` / `SYMBOL_NAMED` / `SYMBOL_NAMED_WINDOW` in
//! `signal.rs`) makes the symbol allowance follow the task: a file whose
//! definitions name the task lists its definitions in source order up to 24 and
//! appends every task-named definition past that, while a file that does not
//! name the task keeps the historical 8 byte for byte.
//!
//! This harness is the *named-repository* guard the lib tests never had: it runs
//! the real pipeline (the real parser, the real graph, the real signal engine)
//! over the real upstream files at the pinned SHAs, so a ranking change that
//! reinstates the flat cap fails here on flask and gin by name. The corpus is a
//! reduced subset of each repository (see `fixtures/flow-recall/README.md` and
//! `PROVENANCE.json`) — enough to reproduce the flow's file anchoring and
//! traversal without a multi-hundred-file checkout.
//!
//! Acceptance floor: **>= 3 of 4** flow symbols per repository. The shipped
//! behaviour scores 4/4 on both; the floor is stated so a partial regression is
//! caught explicitly rather than silently tolerated.

use std::path::{Path, PathBuf};

use hilo_graph::signal::{understand_with_source, SignalOpts};
use hilo_graph::{Edge, GraphDB, Language, Parser};

const FIXTURE_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/flow-recall");

/// The acceptance floor: at least three of a case's four flow symbols.
const FLOW_SYMBOL_FLOOR: usize = 3;

/// One bake-off flow case: the question actually asked of `graph understand`,
/// the upstream files the flow spans, and the four symbols the wave-10
/// scorecard graded.
struct FlowCase {
    /// Repo key — the fixture subdirectory holding the pinned files.
    repo: &'static str,
    /// The pinned upstream revision the fixture files were taken from.
    repo_sha: &'static str,
    /// The bake-off question verbatim.
    prompt: &'static str,
    /// `(symbol, file that defines it)` — the four graded flow symbols.
    symbols: [(&'static str, &'static str); 4],
}

const CASES: [FlowCase; 2] = [
    FlowCase {
        repo: "flask",
        repo_sha: "d73fa1cdcbd8b1465c151db8924ba58b1dd14e35",
        prompt: "request flow WSGI->view->response",
        symbols: [
            ("wsgi_app", "src/flask/app.py"),
            ("full_dispatch_request", "src/flask/app.py"),
            ("dispatch_request", "src/flask/app.py"),
            ("finalize_request", "src/flask/app.py"),
        ],
    },
    FlowCase {
        repo: "gin",
        repo_sha: "dcaa4296d111981ffb31ac3eba90bb63e1eb5ab9",
        prompt: "request dispatch flow ServeHTTP->tree->handler",
        symbols: [
            ("ServeHTTP", "gin.go"),
            ("handleHTTPRequest", "gin.go"),
            ("getValue", "tree.go"),
            ("Next", "context.go"),
        ],
    },
];

/// Every regular file under `dir`, as forward-slash paths relative to it, sorted.
fn corpus_files(dir: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// Build an in-memory graph over the fixture exactly as `graph warm` builds one:
/// every file is parsed with the real language parser and its `imports` edges
/// (including `pkg:`/`std:` targets) are inserted, with the fixture root stripped
/// so endpoints stay repo-relative.
fn fixture_graph(dir: &Path) -> GraphDB {
    let root = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let db = GraphDB::open(":memory:").expect("open in-memory graph");
    let mut edges: Vec<Edge> = Vec::new();
    for rel in corpus_files(dir) {
        let abs = dir.join(&rel);
        let Ok(source) = std::fs::read_to_string(&abs) else {
            continue;
        };
        let Some(lang) = Language::from_path(&abs) else {
            continue;
        };
        let mut parser = match Parser::for_language(lang) {
            Ok(p) => p,
            Err(err) => panic!("no parser for {rel}: {err}"),
        };
        let abs_str = abs
            .canonicalize()
            .unwrap_or_else(|_| abs.clone())
            .to_string_lossy()
            .into_owned();
        let mut parsed = parser
            .parse_imports(&abs_str, &source)
            .unwrap_or_else(|e| panic!("parse {rel}: {e}"));
        for edge in parsed.iter_mut() {
            for endpoint in [&mut edge.from, &mut edge.to] {
                if let Ok(stripped) = Path::new(endpoint.as_str()).strip_prefix(&root) {
                    *endpoint = stripped.to_string_lossy().replace('\\', "/");
                }
            }
        }
        edges.extend(parsed);
    }
    db.insert_edges(&edges).expect("insert fixture edges");
    db
}

/// Run one case and return `(hits, symbols of each flow file, whole output text)`.
fn recall(case: &FlowCase) -> (Vec<(String, String, bool)>, String) {
    let dir: PathBuf = PathBuf::from(FIXTURE_ROOT).join(case.repo);
    assert!(
        dir.is_dir(),
        "flow-recall fixture for {} missing at {}",
        case.repo,
        dir.display()
    );
    let db = fixture_graph(&dir);

    let reader = |path: &str| std::fs::read_to_string(dir.join(path)).ok();
    let opts = SignalOpts {
        token_budget: 6000,
        ..Default::default()
    };
    let result = understand_with_source(&db, case.prompt, &opts, Some(reader)).expect("understand");

    // A flow file that never enters the pack is a miss, not a low rank — say so.
    let mut rows = Vec::new();
    for (symbol, file) in case.symbols {
        let signal_file = result
            .files
            .iter()
            .find(|f| f.path == file)
            .unwrap_or_else(|| {
                panic!(
                    "{}: flow file {file} absent from the pack for {:?} (files: {:?})",
                    case.repo,
                    case.prompt,
                    result
                        .files
                        .iter()
                        .map(|f| f.path.as_str())
                        .collect::<Vec<_>>()
                )
            });
        // The MAP row lists the file's definitions; a Go entry carries its whole
        // declaration (`func (n *node) getValue(...)`) and a Python entry its
        // name with the opening paren, so match on the definition NAME.
        let hit = signal_file.symbols.iter().any(|s| s.contains(symbol));
        rows.push((symbol.to_string(), file.to_string(), hit));
    }
    (rows, result.text)
}

#[test]
fn flask_request_flow_symbols_surface() {
    let case = &CASES[0];
    assert_eq!(case.repo, "flask");
    let (rows, text) = recall(case);
    let hits = rows.iter().filter(|(_, _, hit)| *hit).count();
    for (symbol, file, hit) in &rows {
        println!("flask {symbol} in {file}: {hit}");
    }
    // The graded behaviour is "the symbol appears anywhere in the pack output".
    for (symbol, _, _) in &rows {
        assert!(
            text.contains(symbol.as_str()),
            "flask: {symbol} must appear in the pack output\n{text}"
        );
    }
    assert!(
        hits >= FLOW_SYMBOL_FLOOR,
        "flask flow recall regressed: {hits}/4 symbols on {:?} (floor {FLOW_SYMBOL_FLOOR}); rows: {rows:?}",
        case.prompt
    );
}

#[test]
fn gin_request_flow_symbols_surface() {
    let case = &CASES[1];
    assert_eq!(case.repo, "gin");
    let (rows, text) = recall(case);
    let hits = rows.iter().filter(|(_, _, hit)| *hit).count();
    for (symbol, file, hit) in &rows {
        println!("gin {symbol} in {file}: {hit}");
    }
    for (symbol, _, _) in &rows {
        assert!(
            text.contains(symbol.as_str()),
            "gin: {symbol} must appear in the pack output\n{text}"
        );
    }
    assert!(
        hits >= FLOW_SYMBOL_FLOOR,
        "gin flow recall regressed: {hits}/4 symbols on {:?} (floor {FLOW_SYMBOL_FLOOR}); rows: {rows:?}",
        case.prompt
    );
}

/// The corpus is only evidence while it is the files it claims to be: re-verify
/// every pinned byte count and SHA-256 from `PROVENANCE.json` on each run, and
/// assert every case's pinned revision is represented.
#[test]
fn fixture_provenance_matches_pinned_revisions() {
    use sha2::{Digest, Sha256};

    let path = Path::new(FIXTURE_ROOT).join("PROVENANCE.json");
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let entries: Vec<hilo_graph::serde_json::Value> = hilo_graph::serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
    assert_eq!(
        entries.len(),
        6,
        "the flow-recall corpus is six pinned files"
    );

    let mut shas: Vec<String> = Vec::new();
    for entry in &entries {
        let corpus = entry["corpus"].as_str().unwrap();
        let sha = entry["repo_sha"].as_str().unwrap();
        let rel = entry["path"].as_str().unwrap();
        let bytes = entry["bytes"].as_u64().unwrap() as usize;
        let expected = entry["sha256"].as_str().unwrap();
        shas.push(sha.to_string());

        let file = Path::new(FIXTURE_ROOT).join(corpus).join(rel);
        let content =
            std::fs::read(&file).unwrap_or_else(|e| panic!("read fixture {}: {e}", file.display()));
        assert_eq!(
            content.len(),
            bytes,
            "{} drifted in size ({} != {bytes})",
            file.display(),
            content.len()
        );
        let digest = format!("{:x}", Sha256::digest(&content));
        assert_eq!(
            digest,
            expected,
            "{} does not match its pinned sha256 — the corpus drifted",
            file.display()
        );
    }

    for case in &CASES {
        assert!(
            shas.iter().any(|s| s == case.repo_sha),
            "case {} pins revision {} which no corpus entry records",
            case.repo,
            case.repo_sha
        );
    }
}
