//! Tests for the inventory module — directory creation, JSONL edge I/O,
//! and YAML mount I/O.

use std::fs;
use tempfile::tempdir;
use warpfs_metadata::inventory::{self, BackendMount, Edge};

#[test]
fn test_create_vfs_structure_all_subdirs() {
    let dir = tempdir().expect("create tempdir");
    inventory::create_vfs_structure(dir.path()).expect("create vfs structure");

    let vfs = dir.path().join(".vfs");
    assert!(vfs.is_dir(), ".vfs/ should exist");

    let expected = [
        ".vfs/graph",
        ".vfs/backends",
        ".vfs/blobs",
        ".vfs/features",
        ".vfs/plugins",
        ".vfs/cache",
    ];

    for sub in &expected {
        let p = dir.path().join(sub);
        assert!(
            p.is_dir(),
            "{} should be a directory",
            sub
        );
    }
}

#[test]
fn test_create_vfs_structure_idempotent() {
    let dir = tempdir().expect("create tempdir");

    inventory::create_vfs_structure(dir.path()).expect("first create");
    // Second call should not error.
    inventory::create_vfs_structure(dir.path()).expect("second create (idempotent)");

    // Verify structure still intact.
    assert!(dir.path().join(".vfs/graph").is_dir());
    assert!(dir.path().join(".vfs/backends").is_dir());
}

#[test]
fn test_append_edge_single() {
    let dir = tempdir().expect("create tempdir");
    let edges_path = dir.path().join("graph").join("edges.jsonl");

    let edge = Edge {
        from: "src/handler.go".to_string(),
        to: "src/types.go".to_string(),
        rel: "imports".to_string(),
    };

    inventory::append_edge(&edges_path, &edge).expect("append edge");

    let contents = fs::read_to_string(&edges_path).expect("read edges.jsonl");
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(lines.len(), 1, "expected 1 line");

    // Verify it parses back as JSON.
    let parsed: Edge = serde_json::from_str(lines[0]).expect("parse JSON line");
    assert_eq!(parsed, edge);
}

#[test]
fn test_append_edges_bulk() {
    let dir = tempdir().expect("create tempdir");
    let edges_path = dir.path().join("graph").join("edges.jsonl");

    let edges = vec![
        Edge { from: "a.go".into(), to: "b.go".into(), rel: "imports".into() },
        Edge { from: "b.go".into(), to: "c.go".into(), rel: "calls".into() },
        Edge { from: "c.go".into(), to: "d.go".into(), rel: "uses".into() },
    ];

    inventory::append_edges(&edges_path, &edges).expect("append 3 edges");

    let contents = fs::read_to_string(&edges_path).expect("read edges.jsonl");
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(lines.len(), 3, "expected 3 lines, got {}", lines.len());

    // Verify each line parses correctly.
    for (i, line) in lines.iter().enumerate() {
        let parsed: Edge = serde_json::from_str(line).expect("parse line");
        assert_eq!(parsed, edges[i], "line {} mismatch", i);
    }
}

#[test]
fn test_append_edges_creates_parent_dir() {
    let dir = tempdir().expect("create tempdir");
    let edges_path = dir.path().join(".vfs/graph/edges.jsonl");

    // Parent directory .vfs/graph does not exist yet.
    assert!(!edges_path.parent().unwrap().exists());

    let edge = Edge {
        from: "x".into(),
        to: "y".into(),
        rel: "r".into(),
    };

    inventory::append_edge(&edges_path, &edge).expect("append with parent creation");
    assert!(edges_path.exists(), "edges.jsonl should exist after append");
}

#[test]
fn test_append_edges_appends_not_overwrites() {
    let dir = tempdir().expect("create tempdir");
    let edges_path = dir.path().join("graph/edges.jsonl");

    inventory::append_edge(&edges_path, &Edge {
        from: "first".into(),
        to: "second".into(),
        rel: "r".into(),
    })
    .expect("first append");

    inventory::append_edge(&edges_path, &Edge {
        from: "third".into(),
        to: "fourth".into(),
        rel: "r".into(),
    })
    .expect("second append");

    let contents = fs::read_to_string(&edges_path).expect("read");
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(lines.len(), 2, "expected 2 lines after two appends");
}

#[test]
fn test_read_mounts_nonexistent_returns_empty() {
    let dir = tempdir().expect("create tempdir");
    let mounts_path = dir.path().join("backends/mounts.yaml");

    // File does not exist.
    let mounts = inventory::read_mounts(&mounts_path).expect("read nonexistent mounts");
    assert!(mounts.is_empty(), "expected empty vec for nonexistent file");
}

#[test]
fn test_write_mounts_then_read_roundtrip() {
    let dir = tempdir().expect("create tempdir");
    let mounts_path = dir.path().join("backends/mounts.yaml");

    let original = vec![
        BackendMount {
            name: "auth-service".into(),
            backend_type: "git".into(),
            path: "/repos/auth-service".into(),
        },
        BackendMount {
            name: "models".into(),
            backend_type: "s3".into(),
            path: "s3://ml-bucket/models".into(),
        },
    ];

    inventory::write_mounts(&mounts_path, &original).expect("write mounts");
    let read_back = inventory::read_mounts(&mounts_path).expect("read mounts");

    assert_eq!(read_back.len(), 2);
    assert_eq!(read_back, original);
}

#[test]
fn test_read_mounts_empty_file_returns_empty() {
    let dir = tempdir().expect("create tempdir");
    let mounts_path = dir.path().join("backends/mounts.yaml");

    // Create parent dir, then an empty file.
    fs::create_dir_all(mounts_path.parent().unwrap()).expect("create parent");
    fs::write(&mounts_path, "").expect("write empty file");

    let mounts = inventory::read_mounts(&mounts_path).expect("read empty mounts");
    assert!(mounts.is_empty(), "expected empty vec for empty file");
}

#[test]
fn test_read_mounts_whitespace_only_file_returns_empty() {
    let dir = tempdir().expect("create tempdir");
    let mounts_path = dir.path().join("backends/mounts.yaml");

    fs::create_dir_all(mounts_path.parent().unwrap()).expect("create parent");
    fs::write(&mounts_path, "   \n\n  \n").expect("write whitespace file");

    let mounts = inventory::read_mounts(&mounts_path).expect("read whitespace mounts");
    assert!(mounts.is_empty(), "expected empty vec for whitespace-only file");
}

#[test]
fn test_write_mounts_creates_parent_dir() {
    let dir = tempdir().expect("create tempdir");
    let mounts_path = dir.path().join(".vfs/backends/mounts.yaml");

    assert!(!mounts_path.parent().unwrap().exists());

    inventory::write_mounts(
        &mounts_path,
        &[BackendMount {
            name: "test".into(),
            backend_type: "git".into(),
            path: "/test".into(),
        }],
    )
    .expect("write mounts with parent creation");

    assert!(mounts_path.exists(), "mounts.yaml should exist");
}

#[test]
fn test_edge_to_jsonl_format() {
    let edge = Edge {
        from: "a".into(),
        to: "b".into(),
        rel: "imports".into(),
    };

    let jsonl = inventory::edge_to_jsonl(&edge).expect("serialize edge");
    assert!(
        jsonl.ends_with('\n'),
        "JSONL line should end with newline"
    );

    // Strip the newline and verify it's valid JSON with the right fields.
    let trimmed = jsonl.trim_end();
    let parsed: Edge = serde_json::from_str(trimmed).expect("parse");
    assert_eq!(parsed, edge);
}

#[test]
fn test_append_multiple_edges_then_verify_order() {
    let dir = tempdir().expect("create tempdir");
    let edges_path = dir.path().join("edges.jsonl");

    // Append in two batches to verify ordering is preserved.
    let batch1 = vec![
        Edge { from: "1".into(), to: "2".into(), rel: "a".into() },
        Edge { from: "2".into(), to: "3".into(), rel: "b".into() },
    ];
    let batch2 = vec![
        Edge { from: "3".into(), to: "4".into(), rel: "c".into() },
    ];

    inventory::append_edges(&edges_path, &batch1).expect("batch 1");
    inventory::append_edges(&edges_path, &batch2).expect("batch 2");

    let contents = fs::read_to_string(&edges_path).expect("read");
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(lines.len(), 3, "expected 3 total lines");

    // Verify order: batch1[0], batch1[1], batch2[0]
    let p0: Edge = serde_json::from_str(lines[0]).unwrap();
    let p1: Edge = serde_json::from_str(lines[1]).unwrap();
    let p2: Edge = serde_json::from_str(lines[2]).unwrap();

    assert_eq!(p0.from, "1");
    assert_eq!(p1.from, "2");
    assert_eq!(p2.from, "3");
}
