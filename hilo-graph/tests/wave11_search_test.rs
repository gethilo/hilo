//! Wave 11 natural-language / exact-symbol retrieval regression harness (GAP-116).
//!
//! Wave 11 measured Hilo's `graph search` on six pinned repositories. Natural
//! (intent) search scored MRR 0.025 — four of the six behaviour-owning files
//! never appeared in the top 20 — while exact-symbol search scored 0.457 on
//! the twelve exact cases. The gap was intent-to-owner ranking, not exact
//! symbol lookup.
//!
//! This harness pins the eighteen case definitions (6 natural + 12 exact) as
//! DATA (`tests/fixtures/wave11/cases.jsonl`, transcribed verbatim from the
//! Wave 11 case file) and runs the real `search` pipeline over a checked-in
//! corpus of the real upstream files at the pinned SHAs
//! (`tests/fixtures/wave11/<repo>/...`). See `tests/fixtures/wave11/README.md`
//! for provenance and for how to re-run against full repository clones.
//!
//! Output records, per case: the pinned source SHA, the query, and the 1-based
//! rank of the behaviour-owning path — `rank=MISS` when the owner is absent
//! from the ranked output, so a miss is never reported as a low-ranked match.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use hilo_graph::graph::GraphDB;
use hilo_graph::semantic::{search, SearchOpts};
use hilo_graph::serde_json::Value;
use hilo_metadata::inventory::Edge;

const CORPUS_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/wave11");
const CASES_FILE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/wave11/cases.jsonl"
);

/// Wave 11 baseline exact-symbol MRR (AC3 floor).
const EXACT_MRR_BASELINE: f64 = 0.457;
/// AC2 latency: behaviour-owning path inside the top 5.
const TOP_K: usize = 5;
/// AC2: at least 5 of the 6 natural cases inside the top 5.
const NATURAL_TOP_K_MIN: usize = 5;
/// AC2: MRR floor over the fixed 6-case denominator.
const NATURAL_MRR_MIN: f64 = 0.20;
/// Ranked output window (matches the Wave 11 `--limit 20` invocation).
const SEARCH_LIMIT: usize = 20;

#[derive(Debug, Clone)]
struct Case {
    id: String,
    category: String,
    repo: String,
    repo_sha: String,
    /// The query actually issued: the natural prompt, or the first exact symbol.
    query: String,
    owner_path: String,
    truth_symbols: Vec<String>,
}

fn load_cases() -> Vec<Case> {
    let raw =
        std::fs::read_to_string(CASES_FILE).unwrap_or_else(|e| panic!("read {CASES_FILE}: {e}"));
    let mut out = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: Value = hilo_graph::serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("parse case line: {e}"));
        let category = v["category"].as_str().unwrap().to_string();
        let symbols: Vec<String> = v["truth_symbols"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        // Wave 11 invoked `graph search <prompt>` for natural cases and
        // `graph search <truth_symbols[0]>` for exact cases.
        let query = if category == "exact-search" {
            symbols
                .first()
                .cloned()
                .unwrap_or_else(|| panic!("exact case {} carries no truth symbol", v["id"]))
        } else {
            v["prompt"].as_str().unwrap().to_string()
        };
        out.push(Case {
            id: v["id"].as_str().unwrap().to_string(),
            category,
            repo: v["repo"].as_str().unwrap().to_string(),
            repo_sha: v["repo_sha"].as_str().unwrap().to_string(),
            query,
            owner_path: v["owner_path"].as_str().unwrap().to_string(),
            truth_symbols: symbols,
        });
    }
    out
}

/// Every regular file under `dir`, as repo-relative forward-slash paths, sorted.
fn corpus_files(dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, prefix: &str, out: &mut BTreeSet<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let path = entry.path();
            if path.is_dir() {
                walk(&path, &rel, out);
            } else {
                out.insert(rel);
            }
        }
    }
    let mut set = BTreeSet::new();
    walk(dir, "", &mut set);
    set.into_iter().collect()
}

/// Build an in-memory graph whose document set is exactly the corpus files.
///
/// Search ranks graph documents by their path (+ extracted symbols); a chain of
/// `imports` edges is enough to publish every file as a document without
/// introducing pseudo-nodes. No `service_call`/`service_contract` edges exist,
/// so the edge-aware pass contributes nothing here.
fn corpus_graph(files: &[String]) -> GraphDB {
    let db = GraphDB::open(":memory:").expect("open in-memory graph");
    assert!(files.len() >= 2, "corpus must hold at least two files");
    let edges: Vec<Edge> = files
        .windows(2)
        .map(|w| Edge::new(w[0].as_str(), w[1].as_str(), "imports"))
        .collect();
    db.insert_edges(&edges).expect("insert corpus edges");
    db
}

/// 1-based rank of `owner` in the ranked search output, or `None` on a miss.
fn rank_of(db: &GraphDB, corpus_dir: &Path, query: &str, owner: &str) -> Option<usize> {
    ranked(db, corpus_dir, query)
        .iter()
        .position(|p| p == owner)
        .map(|i| i + 1)
}

/// The ranked file paths for a query (the search output order).
fn ranked(db: &GraphDB, corpus_dir: &Path, query: &str) -> Vec<String> {
    let opts = SearchOpts {
        limit: SEARCH_LIMIT,
        index_symbols: true,
        root: Some(corpus_dir.to_path_buf()),
    };
    search(db, query, &opts)
        .expect("search")
        .into_iter()
        .map(|r| r.file_path)
        .collect()
}

fn reciprocal_rank(rank: Option<usize>) -> f64 {
    match rank {
        Some(r) if r >= 1 => 1.0 / r as f64,
        _ => 0.0,
    }
}

fn render(rank: Option<usize>) -> String {
    match rank {
        Some(r) => format!("rank={r}"),
        None => "rank=MISS".to_string(),
    }
}

/// One scored row plus the rendering the ACs ask for.
struct Score {
    case: Case,
    rank: Option<usize>,
}

impl Score {
    fn line(&self) -> String {
        let symbols = if self.case.truth_symbols.is_empty() {
            String::new()
        } else {
            format!(" symbols={}", self.case.truth_symbols.join("|"))
        };
        format!(
            "case={} repo={} repo_sha={} query={:?} owner={} {}{}",
            self.case.id,
            self.case.repo,
            self.case.repo_sha,
            self.case.query,
            self.case.owner_path,
            render(self.rank),
            symbols
        )
    }
}

fn score_cases(all: &[Case], category: &str) -> Vec<Score> {
    let dump = std::env::var("HILO_WAVE11_DUMP").is_ok();
    all.iter()
        .filter(|c| c.category == category)
        .map(|c| {
            let corpus_dir = PathBuf::from(CORPUS_ROOT).join(&c.repo);
            let files = corpus_files(&corpus_dir);
            assert!(
                files.iter().any(|f| f == &c.owner_path),
                "corpus {} does not contain owner {}",
                corpus_dir.display(),
                c.owner_path
            );
            let db = corpus_graph(&files);
            if dump {
                for (i, p) in ranked(&db, &corpus_dir, &c.query)
                    .iter()
                    .take(8)
                    .enumerate()
                {
                    let mark = if *p == c.owner_path { " <== owner" } else { "" };
                    println!("    #{} {}{}", i + 1, p, mark);
                }
            }
            let rank = rank_of(&db, &corpus_dir, &c.query, &c.owner_path);
            Score {
                case: c.clone(),
                rank,
            }
        })
        .collect()
}

fn report(scores: &[Score]) -> (f64, usize) {
    println!("── ── ── ── ── ── ── ── ── ── ── ── ── ── ──");
    let mut sum = 0.0;
    let mut top_k = 0usize;
    for s in scores {
        println!("{}", s.line());
        sum += reciprocal_rank(s.rank);
        if matches!(s.rank, Some(r) if r <= TOP_K) {
            top_k += 1;
        }
    }
    let mrr = sum / scores.len() as f64;
    println!(
        "summary n={} top{TOP_K}={top_k} misses={} mrr={mrr:.4}",
        scores.len(),
        scores.iter().filter(|s| s.rank.is_none()).count()
    );
    (mrr, top_k)
}

#[test]
fn wave11_natural_search_returns_behaviour_owners_and_does_not_regress_exact_mrr() {
    let all = load_cases();
    assert_eq!(all.len(), 18, "Wave 11 pins 6 natural + 12 exact cases");

    let natural = score_cases(&all, "natural-search");
    let exact = score_cases(&all, "exact-search");
    assert_eq!(natural.len(), 6, "fixed 6-case natural denominator");
    assert_eq!(exact.len(), 12, "12 exact cases");

    println!("── natural-search (intent prompts) ──");
    let (natural_mrr, natural_top_k) = report(&natural);
    println!("── exact-search (first truth symbol) ──");
    let (exact_mrr, _) = report(&exact);

    // AC2: intent-to-owner retrieval.
    assert!(
        natural_top_k >= NATURAL_TOP_K_MIN,
        "AC2: behaviour-owning path must land in the top {TOP_K} on >= {NATURAL_TOP_K_MIN}/6 cases; got {natural_top_k}/6"
    );
    assert!(
        natural_mrr >= NATURAL_MRR_MIN,
        "AC2: natural-search MRR must be >= {NATURAL_MRR_MIN:.2} over the fixed 6-case denominator; got {natural_mrr:.4}"
    );

    // AC3: exact-symbol baseline must not regress.
    assert!(
        exact_mrr >= EXACT_MRR_BASELINE,
        "AC3: exact-symbol MRR must not regress below the Wave 11 baseline {EXACT_MRR_BASELINE:.3}; got {exact_mrr:.4}"
    );

    // AC4: every case's ranking is recorded with its source SHA and query —
    // emitted above; the whole table is also re-printed on any assertion
    // failure via the messages.
    for s in natural.iter().chain(exact.iter()) {
        assert!(
            s.case.repo_sha.len() == 40,
            "case {} must record a pinned 40-hex source SHA",
            s.case.id
        );
    }
}
