//! Benchmark fixture for the COV-3 test-class taxonomy.
//!
//! The `benches/` path component names the `bench` class (§1) — and, unlike
//! the other fixtures, this one carries no marker at all, so it proves the
//! path rule on its own. Inert — never compiled as a target.

fn bench_throughput() {
    let _ = std::hint::black_box(1 + 1);
}
