//! Graph engine benchmarks — edge insertion and impact traversal.
//!
//! Run with: `cargo bench -p hilo_graph`

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use hilo_graph::{Direction, GraphDB};
use hilo_metadata::inventory::Edge;

/// Build a chain of N edges (file_0 → file_1 → … → file_N).
fn edge_chain(n: usize) -> Vec<Edge> {
    (0..n)
        .map(|i| Edge {
            from: format!("file_{i}.rs"),
            to: format!("file_{}.rs", i + 1),
            rel: "imports".into(),
        })
        .collect()
}

/// Build a densely-connected star graph: center → N leaf files.
fn edge_star(n: usize) -> Vec<Edge> {
    (0..n)
        .map(|i| Edge {
            from: "center.rs".into(),
            to: format!("leaf_{i}.rs"),
            rel: "imports".into(),
        })
        .collect()
}

// ── Edge insertion ────────────────────────────────────────────────

fn bench_insert_100(c: &mut Criterion) {
    c.bench_function("graph/insert/100", |b| {
        let edges = edge_chain(100);
        b.iter(|| {
            let db = GraphDB::open(":memory:").unwrap();
            db.insert_edges(&edges).unwrap();
            black_box(&db);
        });
    });
}

fn bench_insert_1k(c: &mut Criterion) {
    c.bench_function("graph/insert/1k", |b| {
        let edges = edge_chain(1_000);
        b.iter(|| {
            let db = GraphDB::open(":memory:").unwrap();
            db.insert_edges(&edges).unwrap();
            black_box(&db);
        });
    });
}

fn bench_insert_10k(c: &mut Criterion) {
    c.bench_function("graph/insert/10k", |b| {
        let edges = edge_chain(10_000);
        b.iter(|| {
            let db = GraphDB::open(":memory:").unwrap();
            db.insert_edges(&edges).unwrap();
            black_box(&db);
        });
    });
}

// ── Graph queries ─────────────────────────────────────────────────

fn bench_related_forward(c: &mut Criterion) {
    let db = GraphDB::open(":memory:").unwrap();
    let edges = edge_star(1_000);
    db.insert_edges(&edges).unwrap();

    c.bench_function("graph/related/star-1k-forward", |b| {
        b.iter(|| {
            let result = db.related("center.rs", None, Direction::Forward).unwrap();
            black_box(result);
        });
    });
}

fn bench_related_reverse(c: &mut Criterion) {
    let db = GraphDB::open(":memory:").unwrap();
    let edges = edge_star(1_000);
    db.insert_edges(&edges).unwrap();

    c.bench_function("graph/related/star-1k-reverse", |b| {
        b.iter(|| {
            let result = db.related("leaf_500.rs", None, Direction::Reverse).unwrap();
            black_box(result);
        });
    });
}

fn bench_impact_bfs(c: &mut Criterion) {
    let db = GraphDB::open(":memory:").unwrap();
    let edges = edge_chain(500);
    db.insert_edges(&edges).unwrap();

    c.bench_function("graph/impact/chain-500", |b| {
        b.iter(|| {
            let result = hilo_graph::compute_impact(db.conn(), "file_0.rs", 500).unwrap();
            black_box(result);
        });
    });
}

criterion_group!(
    benches,
    bench_insert_100,
    bench_insert_1k,
    bench_insert_10k,
    bench_related_forward,
    bench_related_reverse,
    bench_impact_bfs,
);
criterion_main!(benches);
