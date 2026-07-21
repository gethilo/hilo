//! Semantic search benchmarks — tokenization, TF-IDF build, search, BM25, RRF.
//!
//! Run with: `cargo bench -p hilo_graph --bench semantic_bench`

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use hilo_graph::graph::GraphDB;
use hilo_graph::semantic::{search, SearchOpts, TfIdfIndex};
use hilo_metadata::inventory::Edge;

/// Build a densely-connected star graph: center → N leaf files.
fn edge_star(n: usize) -> Vec<Edge> {
    (0..n)
        .map(|i| Edge {
            from: "center.rs".into(),
            to: format!("leaf_{i}.rs"),
            rel: "imports".into(),
            provenance: "ast_exact".to_string(),
            confidence: 1.0,
        })
        .collect()
}

/// Build a chain of N edges (file_0 → file_1 → … → file_N) with thematic names.
fn edge_chain_themed(n: usize) -> Vec<Edge> {
    let themes = [
        "auth",
        "db",
        "cache",
        "middleware",
        "api",
        "config",
        "logger",
        "metrics",
    ];
    (0..n)
        .map(|i| Edge {
            from: format!("src/{}/{}.rs", themes[i % themes.len()], i),
            to: format!("src/{}/{}.rs", themes[(i + 1) % themes.len()], i + 1),
            rel: "imports".into(),
            provenance: "ast_exact".to_string(),
            confidence: 1.0,
        })
        .collect()
}

// ── tokenize: various identifier conventions ────────────────────

fn bench_tokenize_camelcase(c: &mut Criterion) {
    c.bench_function("semantic/tokenize/camelcase-AuthMiddleware", |b| {
        b.iter(|| {
            let result = hilo_graph::semantic_tokenize(black_box("AuthMiddleware"));
            black_box(result);
        });
    });
}

fn bench_tokenize_snake_case(c: &mut Criterion) {
    c.bench_function("semantic/tokenize/snake-rate_limiter", |b| {
        b.iter(|| {
            let result = hilo_graph::semantic_tokenize(black_box("rate_limiter"));
            black_box(result);
        });
    });
}

fn bench_tokenize_mixed(c: &mut Criterion) {
    c.bench_function("semantic/tokenize/mixed-AuthMiddleware_rate_limiter", |b| {
        b.iter(|| {
            let result = hilo_graph::semantic_tokenize(black_box("AuthMiddleware_rate_limiter"));
            black_box(result);
        });
    });
}

fn bench_tokenize_short(c: &mut Criterion) {
    c.bench_function("semantic/tokenize/short-abc", |b| {
        b.iter(|| {
            let result = hilo_graph::semantic_tokenize(black_box("abc"));
            black_box(result);
        });
    });
}

fn bench_tokenize_long_path(c: &mut Criterion) {
    c.bench_function("semantic/tokenize/long-path", |b| {
        b.iter(|| {
            let result = hilo_graph::semantic_tokenize(black_box(
                "src/auth/middleware/handler/validator/rate_limiter_cache.go",
            ));
            black_box(result);
        });
    });
}

// ── TfIdfIndex::build ───────────────────────────────────────────

fn bench_tfidf_build_100(c: &mut Criterion) {
    let db = GraphDB::open(":memory:").unwrap();
    let edges = edge_star(100);
    db.insert_edges(&edges).unwrap();

    c.bench_function("semantic/tfidf/build/100-files", |b| {
        b.iter(|| {
            let index = TfIdfIndex::build(black_box(&db)).unwrap();
            black_box(index);
        });
    });
}

fn bench_tfidf_build_500(c: &mut Criterion) {
    let db = GraphDB::open(":memory:").unwrap();
    let edges = edge_star(500);
    db.insert_edges(&edges).unwrap();

    c.bench_function("semantic/tfidf/build/500-files", |b| {
        b.iter(|| {
            let index = TfIdfIndex::build(black_box(&db)).unwrap();
            black_box(index);
        });
    });
}

fn bench_tfidf_build_1000(c: &mut Criterion) {
    let db = GraphDB::open(":memory:").unwrap();
    let edges = edge_star(1_000);
    db.insert_edges(&edges).unwrap();

    c.bench_function("semantic/tfidf/build/1000-files", |b| {
        b.iter(|| {
            let index = TfIdfIndex::build(black_box(&db)).unwrap();
            black_box(index);
        });
    });
}

// ── tfidf_search on pre-built 500-file index ───────────────────

fn setup_tfidf_500() -> (GraphDB, TfIdfIndex) {
    let db = GraphDB::open(":memory:").unwrap();
    let edges = edge_chain_themed(500);
    db.insert_edges(&edges).unwrap();
    let index = TfIdfIndex::build(&db).unwrap();
    (db, index)
}

fn bench_tfidf_search_5pct(c: &mut Criterion) {
    let (_db, index) = setup_tfidf_500();

    c.bench_function("semantic/tfidf/search/5pct-auth", |b| {
        b.iter(|| {
            // "auth" matches ~12.5% of files (1/8 themes), but since
            // themes cycle through 8 groups, "auth" = ~62 files = ~12% of 500.
            let results = index.tfidf_search(black_box("auth"));
            black_box(results);
        });
    });
}

fn bench_tfidf_search_20pct(c: &mut Criterion) {
    let (_db, index) = setup_tfidf_500();

    c.bench_function("semantic/tfidf/search/20pct-auth-db", |b| {
        b.iter(|| {
            // "auth db" matches ~25% of files (2/8 themes).
            let results = index.tfidf_search(black_box("auth db"));
            black_box(results);
        });
    });
}

fn bench_tfidf_search_50pct(c: &mut Criterion) {
    let (_db, index) = setup_tfidf_500();

    c.bench_function(
        "semantic/tfidf/search/50pct-auth-db-cache-middleware",
        |b| {
            b.iter(|| {
                // Four themes = ~50% of 500 files.
                let results = index.tfidf_search(black_box("auth db cache middleware"));
                black_box(results);
            });
        },
    );
}

// ── bm25_search on pre-built 500-file index ────────────────────

fn setup_bm25_500() -> (GraphDB, TfIdfIndex) {
    let db = GraphDB::open(":memory:").unwrap();
    let edges = edge_chain_themed(500);
    db.insert_edges(&edges).unwrap();
    let index = TfIdfIndex::build(&db).unwrap();
    (db, index)
}

fn bench_bm25_search_5pct(c: &mut Criterion) {
    let (_db, index) = setup_bm25_500();

    c.bench_function("semantic/bm25/search/5pct-auth", |b| {
        b.iter(|| {
            let results = index.bm25_search(black_box("auth"));
            black_box(results);
        });
    });
}

fn bench_bm25_search_20pct(c: &mut Criterion) {
    let (_db, index) = setup_bm25_500();

    c.bench_function("semantic/bm25/search/20pct-auth-db", |b| {
        b.iter(|| {
            let results = index.bm25_search(black_box("auth db"));
            black_box(results);
        });
    });
}

fn bench_bm25_search_50pct(c: &mut Criterion) {
    let (_db, index) = setup_bm25_500();

    c.bench_function("semantic/bm25/search/50pct-auth-db-cache-middleware", |b| {
        b.iter(|| {
            let results = index.bm25_search(black_box("auth db cache middleware"));
            black_box(results);
        });
    });
}

// ── search: end-to-end (build + TF-IDF + BM25 + RRF) ───────────

fn bench_search_e2e(c: &mut Criterion) {
    let db = GraphDB::open(":memory:").unwrap();
    let edges = edge_chain_themed(500);
    db.insert_edges(&edges).unwrap();
    let opts = SearchOpts { limit: 20 };

    c.bench_function("semantic/search/e2e-500-files", |b| {
        b.iter(|| {
            // Builds index, runs TF-IDF + BM25 + RRF, returns results.
            let results = search(
                black_box(&db),
                black_box("auth middleware"),
                black_box(&opts),
            )
            .unwrap();
            black_box(results);
        });
    });
}

criterion_group!(
    benches,
    bench_tokenize_camelcase,
    bench_tokenize_snake_case,
    bench_tokenize_mixed,
    bench_tokenize_short,
    bench_tokenize_long_path,
    bench_tfidf_build_100,
    bench_tfidf_build_500,
    bench_tfidf_build_1000,
    bench_tfidf_search_5pct,
    bench_tfidf_search_20pct,
    bench_tfidf_search_50pct,
    bench_bm25_search_5pct,
    bench_bm25_search_20pct,
    bench_bm25_search_50pct,
    bench_search_e2e,
);
criterion_main!(benches);
