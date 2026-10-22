//! DF-WARPFS-78: Ruby file→package resolution for impact/related.
//!
//! The Ruby parser emits `require 'sidekiq/api'` → `pkg:sidekiq/api` edges
//! (file→pkg-node), so a file-form query (`lib/sidekiq/api.rb`) must resolve
//! to the same pkg node the pkg-form query (`pkg:sidekiq/api`) names. These
//! tests use a sidekiq-shaped corpus (`lib/sidekiq/api.rb`, 21 importers) to
//! prove file-form and pkg-form answers are IDENTICAL across impact, related,
//! and the stats orphans block — the three surfaces that previously reported
//! the file as a silent no-result / orphan.

use hilo_graph::graph::{Direction, GraphDB};
use hilo_graph::impact::compute_impact_at;
use hilo_graph::resolution::{AnchoredRoot, PkgResolver};
use hilo_metadata::inventory::Edge;

const TARGET_FILE: &str = "lib/sidekiq/api.rb";
const TARGET_PKG: &str = "pkg:sidekiq/api";

/// A sidekiq-shaped corpus: `lib/sidekiq/api.rb` plus N importer files.
/// Returns the edge list the parser would have emitted (file→pkg edges).
fn sidekiq_edges(importers: usize) -> Vec<Edge> {
    let mut edges = vec![
        Edge::new(TARGET_FILE, "pkg:sidekiq/worker", "requires"),
        Edge::new("lib/sidekiq/worker.rb", "pkg:sidekiq/options", "requires"),
    ];
    for i in 0..importers {
        edges.push(Edge::new(
            format!("app/workers/worker_{i}.rb"),
            TARGET_PKG,
            "requires",
        ));
    }
    edges
}

fn sidekiq_db() -> GraphDB {
    let db = GraphDB::open(":memory:").unwrap();
    db.insert_edges(&sidekiq_edges(21)).unwrap();
    db
}

#[test]
fn ruby_file_form_impact_matches_pkg_form() {
    let db = sidekiq_db();

    // Pkg-form (the documented working query): 21 dependents, exact.
    let pkg = compute_impact_at(db.conn(), TARGET_PKG, 5, None).unwrap();
    assert_eq!(pkg.len(), 21, "pkg-form must see all 21 importers");

    // File-form used to answer "No dependents found" (rc=0) because nothing
    // resolved the file to its module node. It must now match pkg-form.
    let root = AnchoredRoot::new("/tmp/dogfood-warpfs/sidekiq");
    let file = compute_impact_at(db.conn(), TARGET_FILE, 5, Some(&root)).unwrap();
    assert_eq!(
        file.len(),
        21,
        "file-form must match pkg-form (21 dependents)"
    );
    for row in &file {
        assert_eq!(
            row.scope,
            hilo_graph::impact::SCOPE_CRATE,
            "every row arrives via the resolved module node"
        );
    }
}

#[test]
fn ruby_file_form_related_reverse_matches_pkg_form() {
    let db = sidekiq_db();

    let pkg = db.related(TARGET_PKG, None, Direction::Reverse).unwrap();
    assert_eq!(pkg.len(), 21, "pkg-form reverse sees 21 incoming edges");

    // `related` derives its anchor from the DB root field (None for
    // :memory:), so pass the file path repo-relative — the same string the
    // edges store.
    let file = db.related(TARGET_FILE, None, Direction::Reverse).unwrap();
    assert_eq!(
        file.len(),
        21,
        "file-form reverse must match pkg-form (21 incoming edges)"
    );
    let froms: Vec<&str> = file.iter().map(|e| e.from.as_str()).collect();
    assert!(froms.contains(&"app/workers/worker_0.rb"));
    assert!(froms.contains(&"app/workers/worker_20.rb"));
}

#[test]
fn ruby_file_no_longer_reported_as_stats_orphan() {
    let db = sidekiq_db();
    let stats = db.stats().unwrap();
    assert!(
        !stats.orphans.iter().any(|o| o == TARGET_FILE),
        "lib/sidekiq/api.rb has 21 importers and must not be an orphan; got {:?}",
        stats.orphans
    );
    // The parser-emitted importer files themselves (file→pkg, never
    // targeted) remain orphans — the resolver only covers the TARGET side.
    assert!(
        stats
            .orphans
            .contains(&"app/workers/worker_0.rb".to_string()),
        "importer files with no incoming references stay orphans"
    );
}

#[test]
fn ruby_file_form_untested_and_module_queries_resolve() {
    // `untested`/`module` coverage routes through PkgResolver too: a file
    // whose module node carries a `tested_by` edge is covered.
    let db = GraphDB::open(":memory:").unwrap();
    db.insert_edges(&[
        Edge::new(TARGET_FILE, "pkg:sidekiq/worker", "requires"),
        Edge::new("spec/worker_spec.rb", "pkg:sidekiq/worker", "tested_by"),
    ])
    .unwrap();
    let mut resolver = PkgResolver::new();
    assert_eq!(
        resolver.pkg_node(TARGET_FILE).as_deref(),
        Some(TARGET_PKG),
        "untested/module surfaces resolve the file through this same node"
    );
}

#[test]
fn ruby_windows_and_dot_forms_stay_stable_through_the_db() {
    // Every query surface builds its own resolver; all must answer the same
    // module path for the same stored path, regardless of separator form.
    let mut a = PkgResolver::new();
    let mut b = PkgResolver::new();
    let fwd = a.pkg_node("lib\\sidekiq\\api.rb");
    let dot = b.pkg_node("app/../lib/sidekiq/api.rb");
    assert_eq!(fwd.as_deref(), Some(TARGET_PKG));
    assert_eq!(dot.as_deref(), Some(TARGET_PKG));
}
