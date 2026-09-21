//! Hot-path benchmarks for the two paths the PERF-006 profile flagged:
//! the `edges.jsonl` reconcile/insert path (chunked at
//! [`RECONCILE_CHUNK_ROWS`]) and the `GraphDB::stats()` aggregate path.
//!
//! Design notes worth keeping:
//!
//! * The corpus is generated, never checked in: [`synthetic_edges`] derives
//!   every row from its index, so two runs on two machines replay
//!   byte-identical input without a fixture file living in the repo.
//! * Each measured reconcile runs against a FRESH database in a fresh tempdir,
//!   created in the untimed setup arm. Replaying twice into one database would
//!   measure duplicate detection under the unique index instead of the insert
//!   path GAP-092 chunked.
//! * The database is ON DISK, not `:memory:` — `.vfs/graph/graph.db` is what
//!   ships, and commit behaviour (WAL/checkpoint) is part of the chunk-size
//!   trade being measured.
//! * Threads are pinned to the manifest default (`performance.duckdb.threads`
//!   = 4) so a bench number does not move with the CI box's core count.
//! * Every routine asserts what it processed. A bench that silently replayed
//!   nothing would still report a number.
//!
//! Run with: `cargo bench -p hilo_graph --bench hotpath_bench`
//! CI smoke form (each bench once, no measurement):
//! `cargo bench -p hilo_graph --bench hotpath_bench -- --test`
//!
//! `HILO_BENCH_RECONCILE_ROWS` / `HILO_BENCH_STATS_ROWS` override the corpus
//! sizes for a one-off measurement; the defaults stay small enough for CI.

use std::path::{Path, PathBuf};

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};
use duckdb::Connection;
use hilo_graph::graph::{
    reconcile_edges_from_jsonl, reconcile_edges_from_jsonl_with_chunk_size, GraphDB,
    RECONCILE_CHUNK_ROWS,
};
use hilo_metadata::inventory::Edge;

/// Rows in the reconcile corpus: at the shipped default this spans several
/// `RECONCILE_CHUNK_ROWS` boundaries, and it is the corpus size the existing
/// `reconcile_5003_lines_uses_default_chunks_and_stamps_only_success` unit test
/// already replays, so the bench and the test price the same shape.
const RECONCILE_ROWS_DEFAULT: usize = 5_000;

/// Rows in the `stats()` corpus. The aggregates run over a populated graph;
/// the number is the corpus, not a claim about what a real project holds.
const STATS_ROWS_DEFAULT: usize = 5_000;

/// Chunk sizes compared by the sweep group. `RECONCILE_CHUNK_ROWS` (the
/// shipped default) is in the list by construction — the point of the group is
/// to price what the default costs relative to a much larger transaction.
const CHUNK_SIZES: [usize; 3] = [512, RECONCILE_CHUNK_ROWS, 8_192];

/// `performance.duckdb.threads` default (`hilo_core::manifest`). Pinned so the
/// measurement does not scale with the host's core count.
const BENCH_THREADS: u32 = 4;

fn rows_from_env(var: &str, default: usize) -> usize {
    std::env::var(var)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(default)
}

/// Deterministic synthetic edges: every row is derived from its index, so no
/// RNG and no clock enters the corpus. `from` embeds `i` and is therefore
/// unique per row, which is what keeps every row a real insert rather than a
/// duplicate ignored by the unique index on `(from, to, rel, provenance)`.
fn synthetic_edges(n: usize) -> Vec<Edge> {
    const RELS: [&str; 4] = ["imports", "calls", "tested_by", "references"];
    (0..n)
        .map(|i| {
            let target = (i * 7 + 3) % n;
            Edge::new(
                format!("src/mod_{}/file_{i}.rs", i % 97),
                format!("src/mod_{}/file_{target}.rs", target % 97),
                RELS[i % RELS.len()],
            )
        })
        .collect()
}

/// Write `n` synthetic edges to `<dir>/edges.jsonl`, plus one duplicate row and
/// one malformed row — the two line classes the replayer documents (duplicates
/// processed-and-ignored, malformed skipped). A corpus without them would skip
/// the branches the real file exercises.
///
/// Returns the path and the number of lines the replayer is expected to process
/// (`n` unique + 1 duplicate).
fn write_corpus(dir: &Path, n: usize) -> (PathBuf, usize) {
    let edges = synthetic_edges(n);
    let mut lines: Vec<String> = edges
        .iter()
        .map(|edge| serde_json::to_string(edge).expect("synthetic edge must serialize"))
        .collect();
    lines.push(serde_json::to_string(&edges[0]).expect("synthetic edge must serialize"));
    lines.push("{\"from\":\"truncated\"".to_string());
    lines.push(String::new());

    let path = dir.join("edges.jsonl");
    std::fs::write(&path, lines.join("\n") + "\n").expect("corpus must be writable");
    (path, n + 1)
}

/// Untimed per-iteration setup: a fresh tempdir plus an empty ON-DISK DuckDB
/// configured the way the shipped open path configures it. The tempdir is
/// returned so it outlives the measured reconcile.
fn fresh_disk_db() -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = Connection::open(dir.path().join("graph.db")).expect("duckdb open");
    conn.execute_batch(&format!(
        "SET threads={BENCH_THREADS}; SET preserve_insertion_order=false;"
    ))
    .expect("duckdb settings");
    (dir, conn)
}

// ── reconcile: cold replay into an empty DuckDB cache ────────────

/// Time the shipped `reconcile_edges_from_jsonl` at its default chunk size.
fn bench_reconcile_default(c: &mut Criterion) {
    let rows = rows_from_env("HILO_BENCH_RECONCILE_ROWS", RECONCILE_ROWS_DEFAULT);
    let dir = tempfile::tempdir().unwrap();
    let (jsonl, expected) = write_corpus(dir.path(), rows);

    c.bench_function(&format!("hotpath/reconcile/{rows}-edges"), |b| {
        b.iter_batched(
            fresh_disk_db,
            |(_dir, conn)| {
                let processed =
                    reconcile_edges_from_jsonl(black_box(&conn), black_box(&jsonl)).unwrap();
                assert_eq!(processed, expected, "every parseable line must replay");
                black_box(processed)
            },
            BatchSize::SmallInput,
        );
    });
}

/// The chunk-size sweep: same corpus, same cold-replay discipline, at 512 /
/// [`RECONCILE_CHUNK_ROWS`] / 8192 rows per transaction, so the throughput
/// price of the GAP-092 memory cap is a guarded number rather than a one-off
/// measurement.
fn bench_reconcile_chunk_sizes(c: &mut Criterion) {
    let rows = rows_from_env("HILO_BENCH_RECONCILE_ROWS", RECONCILE_ROWS_DEFAULT);
    let dir = tempfile::tempdir().unwrap();
    let (jsonl, expected) = write_corpus(dir.path(), rows);

    let mut group = c.benchmark_group(format!("hotpath/reconcile-chunk-size/{rows}-edges"));
    for chunk_size in CHUNK_SIZES {
        group.bench_with_input(format!("{chunk_size}"), &chunk_size, |b, &chunk_size| {
            b.iter_batched(
                fresh_disk_db,
                |(_dir, conn)| {
                    let processed = reconcile_edges_from_jsonl_with_chunk_size(
                        black_box(&conn),
                        black_box(&jsonl),
                        chunk_size,
                    )
                    .unwrap();
                    assert_eq!(processed, expected, "every parseable line must replay");
                    black_box(processed)
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

// ── stats(): the aggregate path ──────────────────────────────────

/// Time `stats()` over a populated graph. The graph is built once outside the
/// timing loop: this row prices the aggregate, not ingest. It is a DISK
/// `GraphDB` (the shipped shape) so the aggregates read the same storage the
/// CLI serves from.
fn bench_stats(c: &mut Criterion) {
    let rows = rows_from_env("HILO_BENCH_STATS_ROWS", STATS_ROWS_DEFAULT);
    let dir = tempfile::tempdir().unwrap();
    let db = GraphDB::open(&dir.path().join("graph.db").to_string_lossy()).unwrap();
    db.insert_edges(&synthetic_edges(rows)).unwrap();

    c.bench_function(&format!("hotpath/stats/{rows}-edges"), |b| {
        b.iter(|| {
            let stats = db.stats().unwrap();
            assert_eq!(stats.total_edges, rows as i64);
            black_box(stats)
        });
    });
}

// CI affordability: a 5,000-row on-disk replay costs seconds per iteration
// (DuckDB insert plus unique-index maintenance against a real file), so
// Criterion's default 100 samples would spend ~6 minutes on ONE bench. Ten
// samples keeps the whole target inside a few minutes while still reporting a
// mean and a slope. Raise it with `--sample-size N` for a tighter number, and
// raise the corpus with the `HILO_BENCH_*` env vars.
criterion_group!(
    name = benches;
    config = Criterion::default().sample_size(10);
    targets = bench_reconcile_default, bench_reconcile_chunk_sizes, bench_stats
);
criterion_main!(benches);
