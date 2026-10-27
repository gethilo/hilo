//! Reconcile the append-only edge inventory with the tree it describes
//! (DF-WARPFS-170).
//!
//! `edges.jsonl` is append-only by design (AGENTS.md design rule 3), and the
//! DuckDB cache only ever inserts (`INSERT OR IGNORE`). Both are correct for
//! *discovery* and wrong for *removal*: when a file is deleted — or an import
//! is deleted from a file that still exists — every row that referenced it
//! keeps answering `graph impact`, `graph related`, `graph stats`, and
//! `graph untested`. The graph then reports a dependency the agent REMOVED as
//! a live dependent, forever. That is a false green in the dangerous
//! direction: the review set looks smaller/safer than it is, or a deleted
//! file still looks like part of the codebase.
//!
//! This module is warm's reconciliation pass. Two independent rules, applied
//! once per warm after every edge for this pass has been derived:
//!
//! 1. **Missing path** — an endpoint that names a FILE (anything without a
//!    `<scheme>:` node prefix) but does not exist under the corpus root is
//!    stale, whichever side of the edge it sits on. This is absolute truth
//!    and applies to every warm, including `--changed` and `--language`.
//! 2. **Superseded** — a row whose `from` is one of the source files this pass
//!    re-derived, but whose identity the re-derivation no longer emits. This
//!    is the "the import was deleted, the file is still there" case. It needs
//!    a COMPLETE derivation to be safe, so callers pass `None` for
//!    `derived_sources` on `--changed` / `--language` warms (there the derived
//!    set covers only a subset of the corpus and dropping "absent" rows would
//!    delete live edges).
//!
//! Both rules are idempotent: a reconciled corpus is not rewritten at all, so
//! repeated warms leave the bytes — and the reconcile checkpoint that
//! fingerprints them — untouched.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use hilo_metadata::inventory::{self, edge_key, Edge, EdgeKey};

use crate::error::{GraphError, GraphResult};
use crate::graph::GraphDB;

/// Why a row left the inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PruneReason {
    /// A file endpoint the row references no longer exists under the root —
    /// the file was deleted (or moved) since the row was appended.
    MissingPath,
    /// The row's source file was re-derived this pass and no longer emits the
    /// edge — the dependency was removed from a file that still exists.
    Superseded,
}

impl PruneReason {
    /// Lowercase label used in warm's output.
    pub fn as_str(self) -> &'static str {
        match self {
            PruneReason::MissingPath => "missing-path",
            PruneReason::Superseded => "superseded",
        }
    }
}

/// What a prune pass removed, and from how many rows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PruneReport {
    /// Non-blank rows the inventory held before the prune.
    pub rows_before: usize,
    /// Non-blank rows the inventory holds after the prune.
    pub rows_after: usize,
    /// Rows dropped because a file endpoint no longer exists.
    pub missing_path: usize,
    /// Rows dropped because their source no longer emits the edge.
    pub superseded: usize,
    /// The dropped edges, in inventory order — the identity list a derived
    /// cache (DuckDB) must delete to agree with the pruned inventory.
    pub removed: Vec<Edge>,
}

impl PruneReport {
    /// How many rows the prune removed.
    pub fn removed_count(&self) -> usize {
        self.removed.len()
    }

    /// True when the pass changed nothing (the corpus was already reconciled).
    pub fn is_empty(&self) -> bool {
        self.removed.is_empty()
    }
}

/// Does this endpoint name a file on disk rather than a graph NODE?
///
/// Graph nodes carry a lowercase `<scheme>:` prefix — `pkg:serde`,
/// `std:fmt`, `sys:os`, `local:../util.js`, `iface:Backend`, `type:Foo`,
/// `external:repo:path`. Everything else is a repo-relative (or absolute)
/// path, and only paths can be checked against the filesystem. A Windows
/// drive letter (`C:/…`) is treated as a scheme, i.e. as a node: the
/// conservative direction, since a node is never pruned.
pub fn is_file_endpoint(endpoint: &str) -> bool {
    match endpoint.split_once(':') {
        None => !endpoint.is_empty(),
        Some((head, _)) => {
            let mut chars = head.chars();
            let Some(first) = chars.next() else {
                return true;
            };
            let scheme_like = first.is_ascii_alphabetic()
                && head
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '+');
            !scheme_like
        }
    }
}

/// Does `endpoint` resolve to an existing path under `root`?
///
/// Absolute endpoints are checked as-is; relative ones are resolved against
/// the corpus root (the project directory warm walked), which is the frame
/// every edge path is written in.
fn endpoint_exists(root: &Path, endpoint: &str) -> bool {
    let path = Path::new(endpoint);
    if path.is_absolute() {
        return path.exists();
    }
    // `./src/a.rs` and `src/a.rs` are the same file; join normalises the
    // leading `./` on every platform.
    root.join(path).exists()
}

/// Provenance tags warm itself writes.
///
/// The supersession rule only judges rows warm could have authored: a row
/// carrying a tag this crate never emits (`manual`, a third-party tool's
/// marker) is not ours to reconcile, and dropping it would silently delete
/// hand-maintained inventory. Kept in sync with the emitters — the parser and
/// [`crate::provenance::Provenance`], the conformance extractor
/// (`ast_heuristic`), the test-association / manifest-extension passes
/// (`heuristic`), and the gRPC service passes (`grpc_ast`, `grpc_proto`).
const WARM_PROVENANCE: &[&str] = &[
    "ast_exact",
    "ast_inferred",
    "ast_heuristic",
    "heuristic",
    "lexical",
    "latent",
    "unresolved",
    "grpc_ast",
    "grpc_proto",
];

/// Could warm have written this row? See [`WARM_PROVENANCE`].
pub fn is_warm_provenance(provenance: &str) -> bool {
    WARM_PROVENANCE
        .iter()
        .any(|tag| tag.eq_ignore_ascii_case(provenance))
}

/// Why should this row be dropped, if at all?
fn stale_reason(
    edge: &Edge,
    root: &Path,
    exists_cache: &mut HashMap<String, bool>,
    derived_ids: &HashSet<EdgeKey>,
    derived_sources: Option<&HashSet<String>>,
) -> Option<PruneReason> {
    for endpoint in [&edge.from, &edge.to] {
        if !is_file_endpoint(endpoint) {
            continue;
        }
        let exists = *exists_cache
            .entry(endpoint.clone())
            .or_insert_with(|| endpoint_exists(root, endpoint));
        if !exists {
            return Some(PruneReason::MissingPath);
        }
    }

    let sources = derived_sources?;
    if !sources.contains(&edge.from) {
        return None;
    }
    // Only rows warm could have authored are judged against the derivation.
    if !is_warm_provenance(&edge.provenance) {
        return None;
    }
    // Cross-repo rows exist only for a `--workspace` derivation (the resolver
    // rewrites a mounted repo's path into `external:<repo>:<path>`). A plain
    // warm of the same tree derives nothing for them, so dropping them would
    // make the inventory flip-flop with the flag instead of reconciling it.
    if edge.to.starts_with("external:") {
        return None;
    }
    if derived_ids.contains(&edge_key(edge)) {
        None
    } else {
        Some(PruneReason::Superseded)
    }
}

/// Reconcile `edges_jsonl` against the corpus at `root` (DF-WARPFS-170).
///
/// * `derived` — every edge this warm derived (the authoritative set for the
///   portion of the corpus it covered).
/// * `derived_sources` — the repo-relative source paths whose edge sets
///   `derived` covers COMPLETELY. `None` disables supersession pruning and
///   leaves only the filesystem rule, which is what partial warms
///   (`--changed`, `--language`) must use.
///
/// Returns what was removed; `removed` is the identity list
/// [`prune_cache`] mirrors into an open [`GraphDB`]. A missing inventory file
/// is a no-op (first warm).
pub fn prune_inventory(
    root: &Path,
    edges_jsonl: &Path,
    derived: &[Edge],
    derived_sources: Option<&HashSet<String>>,
) -> GraphResult<PruneReport> {
    let derived_ids: HashSet<EdgeKey> = derived.iter().map(edge_key).collect();
    let mut exists_cache: HashMap<String, bool> = HashMap::new();
    let mut classified: Vec<(Edge, PruneReason)> = Vec::new();

    let outcome = inventory::prune_edges(edges_jsonl, |edge| {
        match stale_reason(edge, root, &mut exists_cache, &derived_ids, derived_sources) {
            Some(reason) => {
                classified.push((edge.clone(), reason));
                true
            }
            None => false,
        }
    })
    .map_err(|error| GraphError::Other(format!("failed to prune {edges_jsonl:?}: {error}")))?;

    let mut missing_path = 0;
    let mut superseded = 0;
    for (_, reason) in &classified {
        match reason {
            PruneReason::MissingPath => missing_path += 1,
            PruneReason::Superseded => superseded += 1,
        }
    }

    Ok(PruneReport {
        rows_before: outcome.rows_before,
        rows_after: outcome.rows_after,
        missing_path,
        superseded,
        removed: classified.into_iter().map(|(edge, _)| edge).collect(),
    })
}

/// Mirror a [`PruneReport`] into an open cache handle (DF-WARPFS-170).
///
/// Deletes the removed identities from the `edges` table and re-records the
/// reconcile checkpoint, so the cache answers every query
/// (`impact`/`related`/`stats`/`untested`) from the pruned graph AND the next
/// open still trusts the cache instead of replaying the corpus. Returns the
/// number of cache rows deleted; a report with nothing removed is a no-op and
/// does not touch the checkpoint.
pub fn prune_cache(graph: &GraphDB, report: &PruneReport) -> GraphResult<usize> {
    if report.removed.is_empty() {
        return Ok(0);
    }
    let deleted = graph.delete_edges(&report.removed)?;
    graph.restamp_checkpoint()?;
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{cache_matches_edges, GraphDB};
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn edge(from: &str, to: &str, rel: &str) -> Edge {
        Edge::new(from, to, rel)
    }

    fn sources(paths: &[&str]) -> HashSet<String> {
        paths.iter().map(|p| p.to_string()).collect()
    }

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }

    fn read_rows(path: &Path) -> Vec<Edge> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn file_endpoints_are_bare_paths_and_nodes_carry_a_scheme() {
        assert!(is_file_endpoint("src/main.rs"));
        assert!(is_file_endpoint("./src/main.rs"));
        assert!(is_file_endpoint("hilo-graph/src/graph.rs"));
        assert!(!is_file_endpoint("pkg:serde::de"));
        assert!(!is_file_endpoint("std:fmt"));
        assert!(!is_file_endpoint("sys:os"));
        assert!(!is_file_endpoint("local:../util.js"));
        assert!(!is_file_endpoint("iface:Backend"));
        assert!(!is_file_endpoint("type:Foo"));
        assert!(!is_file_endpoint("external:shared-lib:pkg/utils.go"));
        // A drive letter is a scheme (node) — never pruned.
        assert!(!is_file_endpoint("C:/repo/src/main.go"));
        assert!(!is_file_endpoint(""));
    }

    #[test]
    fn deleted_file_edges_are_pruned_from_both_sides() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(&root.join("src/main.rs"), "mod helper;\n");
        write(&root.join("src/helper.rs"), "pub fn helper() {}\n");
        let jsonl = root.join(".vfs/graph/edges.jsonl");
        let rows = vec![
            edge("src/main.rs", "src/helper.rs", "imports"),
            edge("src/main.rs", "src/gone.rs", "imports"),
            edge("src/gone.rs", "src/helper.rs", "imports"),
            edge("src/main.rs", "pkg:std", "imports"),
        ];
        write(
            &jsonl,
            &rows
                .iter()
                .map(|e| serde_json::to_string(e).unwrap() + "\n")
                .collect::<String>(),
        );

        let report =
            prune_inventory(root, &jsonl, &rows, Some(&sources(&["src/main.rs"]))).unwrap();

        assert_eq!(report.missing_path, 2, "both rows naming src/gone.rs");
        assert_eq!(report.superseded, 0);
        assert_eq!(report.rows_before, 4);
        assert_eq!(report.rows_after, 2);
        let kept = read_rows(&jsonl);
        assert!(kept
            .iter()
            .all(|e| !e.to.contains("gone") && !e.from.contains("gone")));
        assert!(kept.iter().any(|e| e.to == "pkg:std"));
    }

    #[test]
    fn removed_import_is_pruned_when_its_source_is_rederived() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(&root.join("src/main.rs"), "fn main() {}\n");
        let jsonl = root.join(".vfs/graph/edges.jsonl");
        let stale = edge("src/main.rs", "pkg:serde", "imports");
        let live = edge("src/main.rs", "pkg:std", "imports");
        write(
            &jsonl,
            &format!(
                "{}\n{}\n",
                serde_json::to_string(&stale).unwrap(),
                serde_json::to_string(&live).unwrap()
            ),
        );

        // The current derivation of src/main.rs no longer imports serde.
        let derived = vec![live.clone()];
        let report =
            prune_inventory(root, &jsonl, &derived, Some(&sources(&["src/main.rs"]))).unwrap();

        assert_eq!(report.superseded, 1);
        assert_eq!(report.missing_path, 0);
        assert_eq!(report.removed, vec![stale]);
        assert_eq!(read_rows(&jsonl), vec![live]);
    }

    #[test]
    fn rows_from_sources_outside_the_derivation_are_never_superseded() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(&root.join("src/main.rs"), "fn main() {}\n");
        write(&root.join("src/other.rs"), "pub fn other() {}\n");
        let jsonl = root.join(".vfs/graph/edges.jsonl");
        let untouched = edge("src/other.rs", "pkg:serde", "imports");
        write(&jsonl, &(serde_json::to_string(&untouched).unwrap() + "\n"));

        // src/other.rs was not re-derived (e.g. a --changed warm), so its rows
        // are out of scope even though the derived set does not contain them.
        let report = prune_inventory(root, &jsonl, &[], Some(&sources(&["src/main.rs"]))).unwrap();

        assert!(
            report.is_empty(),
            "out-of-scope rows must survive: {report:?}"
        );
        assert_eq!(read_rows(&jsonl), vec![untouched]);
    }

    #[test]
    fn hand_maintained_rows_are_not_superseded() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(&root.join("src/main.rs"), "fn main() {}\n");
        let jsonl = root.join(".vfs/graph/edges.jsonl");
        // A row an operator added by hand: same source file, a provenance warm
        // never writes. The derivation cannot judge it, so it must survive.
        let manual = Edge::with_provenance("src/main.rs", "sys:manual", "imports", "manual", 1.0);
        write(&jsonl, &(serde_json::to_string(&manual).unwrap() + "\n"));

        let report = prune_inventory(root, &jsonl, &[], Some(&sources(&["src/main.rs"]))).unwrap();

        assert!(
            report.is_empty(),
            "hand-maintained rows are kept: {report:?}"
        );
        assert_eq!(read_rows(&jsonl), vec![manual]);
    }

    #[test]
    fn warm_provenance_is_the_emitter_set() {
        for tag in [
            "ast_exact",
            "ast_inferred",
            "ast_heuristic",
            "heuristic",
            "lexical",
            "latent",
            "unresolved",
            "grpc_ast",
            "grpc_proto",
        ] {
            assert!(is_warm_provenance(tag), "{tag} is emitted by warm");
        }
        for tag in ["manual", "hand-written", "sometool"] {
            assert!(!is_warm_provenance(tag), "{tag} is not warm's to prune");
        }
    }

    #[test]
    fn supersession_is_disabled_without_a_complete_derivation() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(&root.join("src/main.rs"), "fn main() {}\n");
        let jsonl = root.join(".vfs/graph/edges.jsonl");
        let row = edge("src/main.rs", "pkg:serde", "imports");
        write(&jsonl, &(serde_json::to_string(&row).unwrap() + "\n"));

        let report = prune_inventory(root, &jsonl, &[], None).unwrap();

        assert!(report.is_empty());
        assert_eq!(read_rows(&jsonl), vec![row]);
    }

    #[test]
    fn cross_repo_external_rows_survive_a_plain_warm() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(&root.join("src/main.rs"), "fn main() {}\n");
        let jsonl = root.join(".vfs/graph/edges.jsonl");
        let external = edge("src/main.rs", "external:shared-lib:pkg/utils.go", "imports");
        write(&jsonl, &(serde_json::to_string(&external).unwrap() + "\n"));

        let report = prune_inventory(root, &jsonl, &[], Some(&sources(&["src/main.rs"]))).unwrap();

        assert!(report.is_empty(), "external rows are not locally derivable");
        assert_eq!(read_rows(&jsonl), vec![external]);
    }

    #[test]
    fn prune_is_idempotent_and_rewrites_nothing_on_a_clean_corpus() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(&root.join("src/main.rs"), "fn main() {}\n");
        write(&root.join("src/helper.rs"), "pub fn helper() {}\n");
        let jsonl = root.join(".vfs/graph/edges.jsonl");
        let rows = vec![
            edge("src/main.rs", "src/helper.rs", "imports"),
            edge("src/main.rs", "src/gone.rs", "imports"),
        ];
        write(
            &jsonl,
            &rows
                .iter()
                .map(|e| serde_json::to_string(e).unwrap() + "\n")
                .collect::<String>(),
        );

        let first = prune_inventory(root, &jsonl, &rows, Some(&sources(&["src/main.rs"]))).unwrap();
        assert_eq!(first.removed_count(), 1);
        let after_first = std::fs::read(&jsonl).unwrap();
        let mtime_first = std::fs::metadata(&jsonl).unwrap().modified().unwrap();

        // A second pass over the reconciled inventory changes nothing at all —
        // not even a byte-for-byte rewrite (which would invalidate the DuckDB
        // reconcile checkpoint and cost a full replay on the next open).
        std::thread::sleep(std::time::Duration::from_millis(20));
        let second =
            prune_inventory(root, &jsonl, &rows, Some(&sources(&["src/main.rs"]))).unwrap();
        assert!(second.is_empty(), "second pass must remove nothing");
        assert_eq!(second.rows_after, first.rows_after);
        assert_eq!(std::fs::read(&jsonl).unwrap(), after_first);
        assert_eq!(
            std::fs::metadata(&jsonl).unwrap().modified().unwrap(),
            mtime_first,
            "an idempotent pass must not touch the file"
        );
    }

    #[test]
    fn malformed_rows_survive_the_rewrite_verbatim() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(&root.join("src/main.rs"), "fn main() {}\n");
        let jsonl = root.join(".vfs/graph/edges.jsonl");
        let bad = "{\"from\":\"src/main.rs\",\"to\":";
        let good = serde_json::to_string(&edge("src/main.rs", "src/gone.rs", "imports")).unwrap();
        write(&jsonl, &format!("{bad}\n{good}\n"));

        let report = prune_inventory(root, &jsonl, &[], None).unwrap();

        assert_eq!(report.rows_before, 2);
        assert_eq!(report.removed_count(), 1);
        let text = std::fs::read_to_string(&jsonl).unwrap();
        assert_eq!(text, format!("{bad}\n"), "unparseable rows are preserved");
    }

    #[test]
    fn missing_inventory_is_a_noop() {
        let dir = tempdir().unwrap();
        let jsonl = dir.path().join(".vfs/graph/edges.jsonl");
        let report = prune_inventory(dir.path(), &jsonl, &[], None).unwrap();
        assert!(report.is_empty());
        assert!(!jsonl.exists());
    }

    /// The acceptance shape, end to end: a warmed cache answers queries from
    /// the pruned graph and keeps trusting its checkpoint afterwards.
    #[test]
    fn prune_cache_deletes_rows_and_keeps_the_checkpoint_trusted() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(&root.join("src/main.rs"), "mod helper;\n");
        write(&root.join("src/helper.rs"), "pub fn helper() {}\n");
        let graph_dir = root.join(".vfs/graph");
        let jsonl = graph_dir.join("edges.jsonl");
        let db_path = graph_dir.join("graph.db");
        let rows = vec![
            edge("src/main.rs", "src/helper.rs", "imports"),
            edge("src/main.rs", "pkg:std", "imports"),
            edge("src/helper.rs", "src/gone.rs", "imports"),
        ];
        write(
            &jsonl,
            &rows
                .iter()
                .map(|e| serde_json::to_string(e).unwrap() + "\n")
                .collect::<String>(),
        );

        let db_str = db_path.to_str().unwrap();
        let graph = GraphDB::open(db_str).unwrap();
        assert_eq!(graph.count_edges().unwrap(), 3);
        drop(graph);

        let report = prune_inventory(
            root,
            &jsonl,
            &rows,
            Some(&sources(&["src/main.rs", "src/helper.rs"])),
        )
        .unwrap();
        assert_eq!(report.removed_count(), 1);

        let graph = GraphDB::open(db_str).unwrap();
        let deleted = prune_cache(&graph, &report).unwrap();
        assert_eq!(deleted, 1, "the cache row must go with the inventory row");
        assert_eq!(graph.count_edges().unwrap(), 2);
        let stats = graph.stats().unwrap();
        assert_eq!(stats.total_edges, 2);
        drop(graph);

        // The checkpoint was rewritten to describe the post-prune pair: the
        // next open trusts the cache instead of replaying, and the cache it
        // trusts is the pruned one.
        assert!(
            cache_matches_edges(db_str),
            "prune_cache must leave a checkpoint the next open can trust"
        );
        let reopened = GraphDB::open(db_str).unwrap();
        assert_eq!(reopened.count_edges().unwrap(), 2);
        let (froms, tos) = reopened.distinct_files().unwrap();
        assert!(
            !froms.iter().any(|f| f == "src/gone.rs") && !tos.iter().any(|t| t == "src/gone.rs"),
            "the deleted file must not survive as a graph node: from={froms:?} to={tos:?}"
        );
    }

    #[test]
    fn prune_cache_is_a_noop_for_an_empty_report() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let graph_dir = root.join(".vfs/graph");
        write(&graph_dir.join("edges.jsonl"), "");
        let graph = GraphDB::open(graph_dir.join("graph.db").to_str().unwrap()).unwrap();
        let report = PruneReport::default();
        assert_eq!(prune_cache(&graph, &report).unwrap(), 0);
    }

    /// Guard against the classic over-prune: a path that is a graph NODE must
    /// never be checked for existence (it never is on disk) even when it
    /// looks like a path.
    #[test]
    fn node_endpoints_are_not_subject_to_the_filesystem_rule() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(&root.join("src/main.rs"), "fn main() {}\n");
        let jsonl = root.join(".vfs/graph/edges.jsonl");
        let rows = vec![
            edge("src/main.rs", "type:Missing", "implements"),
            edge("src/main.rs", "iface:Backend", "consumes"),
            edge("src/main.rs", "local:../js/util.js", "imports"),
        ];
        write(
            &jsonl,
            &rows
                .iter()
                .map(|e: &Edge| serde_json::to_string(e).unwrap() + "\n")
                .collect::<String>(),
        );

        let report = prune_inventory(root, &jsonl, &rows, None).unwrap();

        assert!(
            report.is_empty(),
            "node endpoints are never pruned: {report:?}"
        );
        assert_eq!(read_rows(&jsonl), rows);
    }

    #[test]
    fn absolute_endpoints_are_checked_as_written() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let live = root.join("src/main.rs");
        write(&live, "fn main() {}\n");
        let jsonl = root.join(".vfs/graph/edges.jsonl");
        let rows = vec![
            edge(live.to_str().unwrap(), "pkg:std", "imports"),
            edge(
                root.join("src/gone.rs").to_str().unwrap(),
                "pkg:std",
                "imports",
            ),
        ];
        write(
            &jsonl,
            &rows
                .iter()
                .map(|e| serde_json::to_string(e).unwrap() + "\n")
                .collect::<String>(),
        );

        let report = prune_inventory(root, &jsonl, &rows, None).unwrap();

        assert_eq!(report.removed_count(), 1);
        assert!(PathBuf::from(&report.removed[0].from).ends_with("src/gone.rs"));
    }
}
