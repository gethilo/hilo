//! FUSE metadata benchmarks — fs construction, path lookup, xattr.
//!
//! Run with: `cargo bench -p hilo_fuse`
//!
//! Benchmarks exercise the in-memory metadata layer without requiring a
//! live FUSE kernel mount.  All operations are synchronous and measure
//! pure CPU time (no kernel round-trips).

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use hilo_fuse::ops::{inode_for_path, readdir_listing_for_test, Hilo};
use hilo_fuse::FuseConfig;
use std::path::PathBuf;

fn default_config(mount: &str, allow_other: bool) -> FuseConfig {
    FuseConfig {
        mount_point: PathBuf::from(mount),
        allow_other,
        direct_io: false,
        auto_unmount: false,
        read_only: true,
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

// ── readdir listing (DF-WARPFS-135 hot path) ─────────────────────

/// Build a tree of `dirs × files_per_dir` so the inode map holds ~`total`
/// entries while each directory listing stays small — the shape that made
/// the old full-map `Path::parent()` filter O(all inodes) per readdir.
fn populate_tree(dirs: usize, files_per_dir: usize) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    for d in 0..dirs {
        let dir = tmp.path().join(format!("d_{d:04}"));
        std::fs::create_dir_all(&dir).unwrap();
        for f in 0..files_per_dir {
            std::fs::write(dir.join(format!("f_{f:04}.txt")), b"x").unwrap();
        }
    }
    let root = tmp.path().to_path_buf();
    (tmp, root)
}

/// Full readdir pipeline (populate + indexed enumeration + cookie builder)
/// for one directory while the map holds ~10k inodes. Pre-fix this was
/// O(all inodes) per listing; with the parent→children index it is
/// O(children) plus the disk read_dir walk.
fn bench_readdir_listing(c: &mut Criterion) {
    let (_tmp, root) = populate_tree(100, 100);
    let cfg = default_config("/tmp/bench-readdir", false);
    let fs = Hilo::new(root, cfg);
    // Prime: one pass populates the whole tree through the real pipeline.
    let prime = readdir_listing_for_test(&fs, 1, 0);
    assert_eq!(prime.len(), 102, "100 dirs + . + ..");
    for e in prime.iter().skip(2) {
        readdir_listing_for_test(&fs, e.0, 0);
    }

    let mut g = c.benchmark_group("fuse/readdir_df135");
    g.bench_function("full_pipeline/one_of_100_dirs_with_10k_inodes", |b| {
        b.iter(|| {
            // A middling directory: its listing must resolve through the
            // index, not the (now removed) 10k-entry map scan.
            let listing = readdir_listing_for_test(&fs, black_box(50u64), black_box(0i64));
            black_box(listing.len());
        })
    });
    // Resume call (offset mid-listing): same populate, but the cookie
    // builder skips the emitted prefix — measures enumeration + resume
    // slicing at scale.
    g.bench_function("resume_indexed_only", |b| {
        b.iter(|| {
            let listing = readdir_listing_for_test(&fs, black_box(50u64), black_box(50i64));
            black_box(listing.len());
        })
    });
    g.finish();
}

criterion_group!(
    benches,
    bench_construction_1k,
    bench_inode_lookup_miss,
    bench_inode_lookup_hit,
    bench_resolve_path,
    bench_readdir_listing,
);
criterion_main!(benches);
