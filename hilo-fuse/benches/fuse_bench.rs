//! FUSE metadata benchmarks — fs construction, path lookup, xattr.
//!
//! Run with: `cargo bench -p hilo_fuse`
//!
//! Benchmarks exercise the in-memory metadata layer without requiring a
//! live FUSE kernel mount.  All operations are synchronous and measure
//! pure CPU time (no kernel round-trips).

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use hilo_fuse::ops::{inode_for_path, Hilo};
use hilo_fuse::FuseConfig;
use std::path::PathBuf;

fn default_config(mount: &str, allow_other: bool) -> FuseConfig {
    FuseConfig {
        mount_point: PathBuf::from(mount),
        allow_other,
        direct_io: false,
        auto_unmount: false,
        attr_timeout: 1.0,
        entry_timeout: 1.0,
        max_read: 131_072,
        max_write: 131_072,
        sandbox: None,
    }
}

/// Populate a temp directory with `n` text files.
fn populate_dir(n: usize) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    for i in 0..n {
        let name = format!("file_{i:04}.txt");
        std::fs::write(tmp.path().join(&name), format!("content-{i}\n")).unwrap();
    }
    let root = tmp.path().to_path_buf();
    // Keep tmp alive so the directory isn't deleted.
    (tmp, root)
}

// ── Construction ──────────────────────────────────────────────────

fn bench_construction_1k(c: &mut Criterion) {
    let (_tmp, root) = populate_dir(1_000);
    let cfg = default_config("/tmp/bench-fs", false);

    c.bench_function("fuse/new/1k-files", |b| {
        b.iter(|| {
            let fs = Hilo::new(root.clone(), cfg.clone());
            black_box(fs);
        });
    });
}

// ── Path lookup (string → inode) ──────────────────────────────────

fn bench_inode_lookup_miss(c: &mut Criterion) {
    let (_tmp, root) = populate_dir(100);
    let cfg = default_config("/tmp/bench-lookup", false);
    let fs = Hilo::new(root, cfg);

    c.bench_function("fuse/inode_lookup/miss", |b| {
        b.iter(|| {
            let result = inode_for_path(&fs, "nonexistent_file.xyz");
            black_box(result);
        });
    });
}

fn bench_inode_lookup_hit(c: &mut Criterion) {
    let (_tmp, root) = populate_dir(1_000);
    let cfg = default_config("/tmp/bench-lookup2", false);
    let fs = Hilo::new(root, cfg);

    c.bench_function("fuse/inode_lookup/hit", |b| {
        b.iter(|| {
            let result = inode_for_path(&fs, "file_0500.txt");
            black_box(result);
        });
    });
}

// ── Inode → path resolution ──────────────────────────────────────

fn bench_resolve_path(c: &mut Criterion) {
    let (_tmp, root) = populate_dir(1_000);
    let cfg = default_config("/tmp/bench-resolve", false);
    let fs = Hilo::new(root, cfg);
    // Grab an inode we know exists.
    let ino = inode_for_path(&fs, "file_0500.txt").unwrap();

    c.bench_function("fuse/resolve_path", |b| {
        b.iter(|| {
            let path = fs.resolve_path(ino);
            black_box(path);
        });
    });
}

criterion_group!(
    benches,
    bench_construction_1k,
    bench_inode_lookup_miss,
    bench_inode_lookup_hit,
    bench_resolve_path,
);
criterion_main!(benches);
