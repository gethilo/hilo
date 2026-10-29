//! REV-WARPFS-8: exact-duplicate edge deduplication in edges.jsonl.
//!
//! AC surface for the dedup work: repeated warm-style append passes over
//! the same corpus must leave edges.jsonl with zero duplicate rows
//! (`sort edges.jsonl | uniq -d` empty), duplicates WITHIN one append
//! batch collapse, distinct relations between the same file pair survive
//! (dedup is never on `(from, to)` alone), and `gc_edges` removes
//! duplicates from an existing file idempotently.
//!
//! The append/gc mechanics live in `hilo_metadata::inventory`
//! (`append_edges_deduped` / `gc_edges`); hilo-graph is the package the
//! acceptance command names, and warm's derived-dimension passes (service
//! edges, manifest assoc edges) are produced here — so these tests pin the
//! contract at the consumer side.

use hilo_metadata::inventory::{append_edges, append_edges_deduped, gc_edges, Edge};

fn edge(from: &str, to: &str, rel: &str, provenance: &str) -> Edge {
    Edge {
        from: from.to_string(),
        to: to.to_string(),
        rel: rel.to_string(),
        provenance: provenance.to_string(),
        confidence: 1.0,
    }
}

fn duplicate_groups(path: &std::path::Path) -> usize {
    let contents = std::fs::read_to_string(path).unwrap();
    let mut lines: Vec<String> = contents
        .lines()
        .map(|l| l.to_string())
        .filter(|l| !l.trim().is_empty())
        .collect();
    lines.sort();
    let mut groups = 0;
    let mut i = 0;
    while i < lines.len() {
        let mut j = i;
        while j + 1 < lines.len() && lines[j + 1] == lines[i] {
            j += 1;
        }
        if j > i {
            groups += 1;
        }
        i = j + 1;
    }
    groups
}

/// Warm-pass simulation: the same derived edge set appended three times
/// (warm run 1 = create, runs 2-3 = re-append on a cache-miss path). After
/// three passes the file must have zero duplicate rows.
#[test]
fn edges_dedup_three_warm_passes_leave_no_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("edges.jsonl");

    // One derived batch as warm would produce it: AST edges plus a
    // heuristic test edge and an external edge, including one pair with
    // two distinct relations.
    let warm_batch = |pass: usize| -> Vec<Edge> {
        let _ = pass;
        vec![
            edge("src/main.py", "src/helper.py", "imports", "ast_exact"),
            edge("src/main.py", "std:os", "imports", "ast_exact"),
            edge(
                "src/main.py",
                "external:shared-lib:pkg/utils.py",
                "imports",
                "ast_exact",
            ),
            edge("tests/test_main.py", "src/main.py", "tests", "heuristic"),
            edge("tests/test_main.py", "src/main.py", "imports", "ast_exact"),
            edge("src/assoc_edge", "src/manifest.yaml", "assoc", "manifest"),
        ]
    };

    for pass in 1..=3 {
        let appended = append_edges_deduped(&path, &warm_batch(pass)).unwrap();
        if pass == 1 {
            assert_eq!(appended, 6, "first pass writes every edge once");
        } else {
            assert_eq!(appended, 0, "pass {pass} must append nothing new");
        }
    }

    assert_eq!(
        duplicate_groups(&path),
        0,
        "three warm passes => zero duplicate rows"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap().lines().count(),
        6,
        "row count equals unique edges"
    );
}

/// Dedup identity is `(from, to, rel, provenance)` — never `(from, to)`
/// alone. Same pair, different relation or provenance: both rows survive
/// three passes.
#[test]
fn edges_dedup_keeps_distinct_relations_for_same_pair() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("edges.jsonl");
    let batch = [
        edge("a.py", "b.py", "imports", "ast_exact"),
        edge("a.py", "b.py", "imports", "ast_exact"), // exact dup
        edge("a.py", "b.py", "tests", "heuristic"),   // distinct rel+prov
        edge("a.py", "b.py", "imports", "lexical"),   // distinct provenance
    ];
    let n = append_edges_deduped(&path, &batch).unwrap();
    assert_eq!(n, 3, "exact dup collapses; distinct identities stay");
    for _ in 0..2 {
        append_edges_deduped(&path, &batch).unwrap();
    }
    assert_eq!(duplicate_groups(&path), 0);
    assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 3);
}

/// An append batch containing the same identity twice collapses to its
/// first occurrence — intra-batch duplicates never reach the file.
#[test]
fn edges_dedup_collapses_duplicates_within_one_append_batch() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("edges.jsonl");
    let batch = [
        edge("a.rs", "b.rs", "imports", "ast_exact"),
        edge("a.rs", "b.rs", "imports", "ast_exact"),
        edge("a.rs", "b.rs", "imports", "ast_exact"),
    ];
    let n = append_edges_deduped(&path, &batch).unwrap();
    assert_eq!(n, 1);
    assert_eq!(duplicate_groups(&path), 0);
}

/// `gc_edges` compacts an ALREADY-duplicated file (the shape old repos
/// have): duplicate groups removed, first occurrences kept in order,
/// second run removes nothing (idempotent).
#[test]
fn edges_dedup_gc_removes_duplicates_from_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("edges.jsonl");
    let row = edge("a.py", "b.py", "imports", "ast_exact");
    // 4 rows: 3 identical + 1 distinct.
    append_edges(&path, &[row.clone(), row.clone(), row.clone()]).unwrap();
    append_edges(&path, &[edge("c.py", "d.py", "imports", "ast_exact")]).unwrap();
    assert_eq!(duplicate_groups(&path), 1, "premise: file is duplicated");

    let (before, after, removed) = gc_edges(&path).unwrap();
    assert_eq!((before, after, removed), (4, 2, 2));
    assert_eq!(duplicate_groups(&path), 0, "gc left no duplicates");

    let (b2, a2, r2) = gc_edges(&path).unwrap();
    assert_eq!((b2, a2, r2), (2, 2, 0), "second gc is a no-op");
    assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 2);
}

/// DF-WARPFS-62 acceptance: a file that arrives ALREADY polluted (a
/// pre-REV-WARPFS-8 writer, a stale `hilo` on PATH, or the union of two
/// branch inventories at a git merge) is healed by a re-parse, not merely
/// spared further growth. Dedup-on-append alone can never fix that shape —
/// it only inspects what it is about to ADD.
#[test]
fn edges_dedup_reparse_heals_pre_seeded_duplicate_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("edges.jsonl");
    let imports = edge("src/main.py", "src/helper.py", "imports", "ast_exact");
    let tests = edge("tests/test_main.py", "src/main.py", "tests", "heuristic");

    // Premise: the x3 accumulation shape — three copies of one identity next
    // to one distinct row (4 rows, 3 unique).
    append_edges(&path, &[imports.clone(), imports.clone(), imports.clone()]).unwrap();
    append_edges(&path, std::slice::from_ref(&tests)).unwrap();
    assert_eq!(duplicate_groups(&path), 1, "premise: file is duplicated");
    assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 4);

    // A warm pass over the same corpus derives both edges again.
    let appended = append_edges_deduped(&path, &[imports, tests]).unwrap();
    assert_eq!(appended, 0, "a re-parse of unchanged content adds nothing");
    assert_eq!(duplicate_groups(&path), 0, "re-parse must heal duplicates");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap().lines().count(),
        2,
        "line count must equal the unique edge count"
    );
}

/// Three consecutive re-parse passes over a polluted inventory converge on
/// the first pass and then hold exactly constant — the acceptance command
/// (`sort edges.jsonl | uniq -d`) stays empty and `wc -l` stops moving.
#[test]
fn edges_dedup_three_passes_over_polluted_file_converge_and_hold() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("edges.jsonl");
    let row = edge("src/app.rs", "src/lib.rs", "imports", "ast_exact");
    // Five copies: the multiplicity-5 shape seen in a real tracked inventory.
    append_edges(
        &path,
        &[row.clone(), row.clone(), row.clone(), row.clone(), row],
    )
    .unwrap();
    let polluted = std::fs::read_to_string(&path).unwrap();
    assert_eq!(polluted.lines().count(), 5);
    assert_eq!(duplicate_groups(&path), 1);

    let batch = vec![edge("src/app.rs", "src/lib.rs", "imports", "ast_exact")];
    let mut counts = Vec::new();
    for _ in 0..3 {
        append_edges_deduped(&path, &batch).unwrap();
        counts.push(std::fs::read_to_string(&path).unwrap().lines().count());
    }
    assert_eq!(counts, vec![1, 1, 1], "count is stable after the heal");
    assert_eq!(duplicate_groups(&path), 0, "`uniq -d` must be empty");
}
