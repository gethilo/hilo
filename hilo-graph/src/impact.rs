//! Transitive impact analysis — find all files that depend on a given file, directly or transitively.

use std::collections::{HashSet, VecDeque};

use duckdb::{params, Connection};
use serde::Serialize;

use crate::error::GraphResult;
use crate::resolution::PkgResolver;

/// GAP-083 `scope` value: the row is a true file-level dependent (an edge whose
/// target IS the queried file, or a `local:` node that resolves to it).
pub const SCOPE_FILE: &str = "file";
/// GAP-083 `scope` value: the row was matched through a `pkg:<crate>` node
/// rather than the queried file itself.
pub const SCOPE_CRATE: &str = "crate";

/// A single file in the impact chain.
#[derive(Debug, Clone, Serialize)]
pub struct ImpactFile {
    /// The file path.
    pub path: String,
    /// The relation type from the edge that connects this file to its dependent.
    pub relation: String,
    /// Distance from the start file (1 = direct dependent, N = N-hop dependent).
    pub depth: u32,
    /// GAP-083: how this row was matched — `SCOPE_FILE` when the edge targets
    /// the queried file, `SCOPE_CRATE` when it was reached through the queried
    /// file's `pkg:<crate>` node. Always serialized: without it "0 files import
    /// this" and "N files import this file's crate" look identical.
    pub scope: String,
    /// GAP-083: for crate-scoped rows, the `pkg:<crate>` node the row was
    /// matched through (`None` for file-scoped rows).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    /// How the edge was discovered (provenance string).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<String>,
    /// Confidence weight (0.0 – 1.0) of the edge.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
}

/// How a batch of edge rows was matched. Carried into [`collect`] /
/// [`collect_family`] so every emitted row states its own scope and depth.
#[derive(Debug, Clone, Copy)]
struct MatchKind<'a> {
    /// `SCOPE_FILE` or `SCOPE_CRATE`.
    scope: &'a str,
    /// Extra hops this match costs on top of the single edge hop. A FILE query
    /// that reaches rows through its `pkg:<crate>` node pays one extra hop
    /// (file → crate → importer); that is what keeps `depth: 1` a strict claim
    /// about true file-level importers. Symbol-node queries (whose target IS the
    /// crate node) keep the historical accounting, so `extra` is 0 there.
    extra: u32,
    /// For crate matches: the `pkg:<crate>` node the rows were matched through.
    via: Option<&'a str>,
}

/// Mutable BFS state shared by every collector of one query: the result set,
/// the visited set, the frontier and the caller's depth budget. Bundling them
/// keeps the collector signatures small (GAP-083 added two more per-batch
/// parameters) and makes it impossible to pass one query's queue with another's
/// visited set.
struct Collector<'a> {
    results: &'a mut Vec<ImpactFile>,
    visited: &'a mut HashSet<String>,
    queue: &'a mut VecDeque<(String, u32)>,
    max_depth: u32,
}

/// Result of an impact analysis.
#[derive(Debug, Clone, Serialize)]
pub struct ImpactResult {
    pub files: Vec<ImpactFile>,
}

/// Collect query rows into the BFS result set (visited-dedup, depth-tagged).
///
/// `kind` carries the scope label and the extra hop cost of this match class
/// (GAP-083), so the same helper serves file-level and crate-level batches
/// without either of them being able to claim the other's depth.
fn collect(
    rows: impl Iterator<Item = duckdb::Result<(String, String, Option<String>, Option<f64>)>>,
    cx: &mut Collector<'_>,
    depth: u32,
    kind: MatchKind<'_>,
) -> GraphResult<()> {
    let child_depth = depth + 1 + kind.extra;
    for row in rows {
        let (from, rel, prov, conf) = row?;
        // A crate hop can push a row past the caller's budget; reporting it
        // (or queueing it) would be a lie about the traversal. The row is not
        // marked visited either, so a shallower route to it can still win.
        if child_depth > cx.max_depth {
            continue;
        }
        if cx.visited.insert(from.clone()) {
            cx.results.push(ImpactFile {
                path: from.clone(),
                relation: rel,
                depth: child_depth,
                scope: kind.scope.to_string(),
                via: kind.via.map(str::to_string),
                provenance: prov,
                confidence: conf,
            });
            cx.queue.push_back((from, child_depth));
        }
    }
    Ok(())
}

/// Collect pkg-family rows (GAP-048): edges whose `to` node is a member of
/// the crate family — `pkg:<name>::<member>` (brace-expanded imports) or
/// `pkg:<name>_<sibling>` (underscore companion crates such as serde_derive
/// for serde). The SQL pattern is deliberately over-broad (`pkg:<name>%`)
/// and the exact boundary is enforced here in Rust, because LIKE ESCAPE
/// semantics vary across DuckDB versions and a wildcard `_` in the pattern
/// would leak unrelated crates that merely share a name prefix (`pkg:ab`
/// must never match a query on `pkg:a`).
fn collect_family(
    rows: impl Iterator<Item = duckdb::Result<(String, String, String, Option<String>, Option<f64>)>>,
    pkg: &str,
    cx: &mut Collector<'_>,
    depth: u32,
    kind: MatchKind<'_>,
) -> GraphResult<()> {
    let member = format!("{pkg}::");
    let sibling = format!("{pkg}_");
    let child_depth = depth + 1 + kind.extra;
    for row in rows {
        let (from, to, rel, prov, conf) = row?;
        if !to.starts_with(&member) && !to.starts_with(&sibling) {
            continue;
        }
        if child_depth > cx.max_depth {
            continue;
        }
        if cx.visited.insert(from.clone()) {
            cx.results.push(ImpactFile {
                path: from.clone(),
                relation: rel,
                depth: child_depth,
                scope: kind.scope.to_string(),
                via: kind.via.map(str::to_string),
                provenance: prov,
                confidence: conf,
            });
            cx.queue.push_back((from, child_depth));
        }
    }
    Ok(())
}

/// Compute transitive impact: find all files that depend on `start_path`,
/// directly or transitively, up to `max_depth` hops.
///
/// The DuckDB `edges` table has columns `"from"` (source file), `"to"` (dependency),
/// `rel` (relation type). Impact analysis finds files whose `"from"` appears as
/// a dependent of `start_path` or its transitive dependents.
///
/// Uses BFS with a visited set to protect against circular imports.
/// Returns files ordered by discovery (BFS order) — direct dependents first,
/// then 2-hop, etc.
///
/// GAP-083: every returned row states its `scope`. `SCOPE_FILE` rows name the
/// queried path (or a `local:` node resolving to it) and keep the historical
/// depth accounting. `SCOPE_CRATE` rows were matched through the queried file's
/// `pkg:<crate>` node (GAP-034/GAP-048) and cost one extra hop, so `depth == 1`
/// is a strict claim: that file really does import this file. Symbol-node
/// queries (`pkg:`/`sys:`) keep their historical sets and depths — there the
/// crate node IS the query target.
pub fn compute_impact(
    conn: &Connection,
    start_path: &str,
    max_depth: u32,
) -> GraphResult<Vec<ImpactFile>> {
    if max_depth == 0 {
        return Ok(Vec::new());
    }

    let mut results: Vec<ImpactFile> = Vec::new();
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(start_path.to_string());

    let mut queue: VecDeque<(String, u32)> = VecDeque::new();
    queue.push_back((start_path.to_string(), 0));

    let mut stmt =
        conn.prepare(r#"SELECT "from", rel, provenance, confidence FROM edges WHERE "to" = ?"#)?;
    // GAP-048: pkg-family edges — member nodes `pkg:<name>::<member>` (the
    // parser expands brace imports like `use serde::{de::Deserializer}` into
    // one member pseudo-node per symbol) and underscore-sibling companion
    // crates `pkg:<name>_<sibling>` (serde_derive, serde_test — the Rust
    // companion-crate convention), which a crate re-exports into its public
    // surface. The SQL pattern is deliberately over-broad; the exact family
    // boundary is enforced in Rust (collect_family) so `pkg:ab` can never
    // leak into a query on `pkg:a`.
    let mut family_stmt = conn.prepare(
        r#"SELECT "from", "to", rel, provenance, confidence FROM edges WHERE "to" LIKE ?"#,
    )?;
    // GAP-069: TS/JS `local:` reverse index — built ONCE per query from a
    // single scan of the `local:` edges, so per-BFS-node resolution is a
    // hash lookup and the traversal stays linear.
    let local_resolver = crate::resolution::LocalSpecResolver::from_edges(conn).ok();
    let mut resolver = PkgResolver::new();
    // GAP-083: a query target that is a real FILE reaches crate-level rows one
    // extra hop away (file → pkg:<crate> → importer), so those rows can never
    // claim `depth: 1`. Symbol-node targets (`pkg:`/`sys:`) keep the historical
    // accounting: there the crate node IS the queried node.
    let file_query = !start_path.starts_with("pkg:") && !start_path.starts_with("sys:");

    let mut cx = Collector {
        results: &mut results,
        visited: &mut visited,
        queue: &mut queue,
        max_depth,
    };

    while let Some((path, depth)) = cx.queue.pop_front() {
        if depth >= max_depth {
            continue;
        }

        // Exact match. For a file node these rows are true file-level
        // dependents; when the queued node is the query's own symbol node
        // (`pkg:`/`sys:`) they were matched through that crate node instead.
        let symbol_node = path.starts_with("pkg:") || path.starts_with("sys:");
        let exact_kind = MatchKind {
            scope: if symbol_node { SCOPE_CRATE } else { SCOPE_FILE },
            extra: 0,
            via: None,
        };
        collect(
            stmt.query_map(params![path.clone()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<f64>>(3)?,
                ))
            })?,
            &mut cx,
            depth,
            exact_kind,
        )?;

        // GAP-034: file-level resolution — a file query must also match
        // dependents that target its crate's `pkg:<name>` node, because the
        // parser emits `pkg:` edges (not file→file edges). Symbol nodes
        // (`pkg:...`/`sys:...`) are not files and resolve to None.
        // GAP-048: a `pkg:<name>` node (whether the query target itself or
        // resolved from a file) must also match `pkg:<name>::<member>` and
        // `pkg:<name>_<sibling>` edges — exact matching alone misses
        // brace-expanded imports (6/148 on serde).
        let pkg_target = if path.starts_with("pkg:") {
            // The path is already a bare pkg node: the exact-match query
            // above covered `to = <path>`; only the family remains.
            Some(path.clone())
        } else {
            resolver.pkg_node(&path)
        };
        if let Some(pkg) = pkg_target {
            // GAP-083: rows matched through a `pkg:<crate>` node of a FILE
            // query are crate-scoped and cost one extra hop (the file→crate
            // hop), so `depth == 1` stays strictly "true file-level importer".
            let crate_kind = MatchKind {
                scope: SCOPE_CRATE,
                extra: u32::from(file_query),
                via: Some(pkg.as_str()),
            };
            if !path.starts_with("pkg:") {
                collect(
                    stmt.query_map(params![pkg.clone()], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, Option<f64>>(3)?,
                        ))
                    })?,
                    &mut cx,
                    depth,
                    crate_kind,
                )?;
            }
            collect_family(
                family_stmt.query_map(params![format!("{pkg}%")], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<f64>>(4)?,
                    ))
                })?,
                &pkg,
                &mut cx,
                depth,
                crate_kind,
            )?;
        }

        // GAP-069: a file path may also be named by TS/JS `local:` nodes —
        // raw specifiers the parser kept verbatim, one per (importer dir,
        // specifier) pair. Seed every resolved node's incoming edges with
        // the same collect helper (visited/depth semantics unchanged). An
        // edge counts only when its importer is one of the directories
        // whose specifier actually lands on this file: the SAME node string
        // can be emitted from other directories naming a DIFFERENT target.
        // Symbol/pseudo nodes never resolve to a local family.
        if !path.starts_with("pkg:") {
            if let Some(local) = local_resolver.as_ref() {
                let mut families = local.nodes_for(&path);
                families.sort();
                families.dedup();
                for (node, importers) in families {
                    collect(
                        stmt.query_map(params![node], |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, Option<String>>(2)?,
                                row.get::<_, Option<f64>>(3)?,
                            ))
                        })?
                        .filter(|row| {
                            row.as_ref()
                                .map_or(true, |(from, ..)| importers.contains(from))
                        }),
                        &mut cx,
                        depth,
                        // GAP-069 `local:` nodes resolve to the queried file
                        // itself, so they are file-level (GAP-083).
                        MatchKind {
                            scope: SCOPE_FILE,
                            extra: 0,
                            via: None,
                        },
                    )?;
                }
            }
        }
    }

    Ok(results)
}

/// Compute transitive impact with external cross-repo edge support.
///
/// When `include_external` is `true`, the BFS also follows `external:<repo>:<path>`
/// edges by matching `to LIKE '%:' || path`.  This allows impact analysis
/// to traverse across repository boundaries in a multi-repo workspace.
pub fn compute_impact_with_external(
    conn: &Connection,
    start_path: &str,
    max_depth: u32,
    include_external: bool,
) -> GraphResult<Vec<ImpactFile>> {
    if max_depth == 0 {
        return Ok(Vec::new());
    }

    let mut results: Vec<ImpactFile> = Vec::new();
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(start_path.to_string());

    let mut queue: VecDeque<(String, u32)> = VecDeque::new();
    queue.push_back((start_path.to_string(), 0));

    let mut stmt =
        conn.prepare(r#"SELECT "from", rel, provenance, confidence FROM edges WHERE "to" = ?"#)?;
    // GAP-048: pkg-family edges — member nodes and underscore-sibling
    // companion crates; the exact family boundary is enforced in Rust
    // (collect_family), see compute_impact for the full rationale.
    let mut family_stmt = conn.prepare(
        r#"SELECT "from", "to", rel, provenance, confidence FROM edges WHERE "to" LIKE ?"#,
    )?;
    let mut ext_stmt: Option<duckdb::Statement> = None;
    let mut resolver = PkgResolver::new();
    // GAP-069: TS/JS `local:` reverse index — one scan per query, shared
    // across the whole BFS (see compute_impact).
    let local_resolver = crate::resolution::LocalSpecResolver::from_edges(conn).ok();
    // GAP-083: same file-vs-symbol accounting as compute_impact — a FILE query
    // pays one extra hop for rows reached through its `pkg:<crate>` node.
    let file_query = !start_path.starts_with("pkg:") && !start_path.starts_with("sys:");

    let mut cx = Collector {
        results: &mut results,
        visited: &mut visited,
        queue: &mut queue,
        max_depth,
    };

    if include_external {
        // Match edges where `to` ends with `:path` (the external edge format
        // is `external:repo-name:path`).
        ext_stmt = Some(conn.prepare(
            r#"SELECT "from", rel, provenance, confidence FROM edges WHERE "to" LIKE '%:' || ?"#,
        )?);
    }

    while let Some((path, depth)) = cx.queue.pop_front() {
        if depth >= max_depth {
            continue;
        }

        // Exact match. Rows matched by the queued node itself: true
        // file-level dependents for a file node, crate-scoped matches when
        // the queued node is the query's own `pkg:`/`sys:` target.
        let symbol_node = path.starts_with("pkg:") || path.starts_with("sys:");
        let exact_kind = MatchKind {
            scope: if symbol_node { SCOPE_CRATE } else { SCOPE_FILE },
            extra: 0,
            via: None,
        };
        collect(
            stmt.query_map(params![path.clone()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<f64>>(3)?,
                ))
            })?,
            &mut cx,
            depth,
            exact_kind,
        )?;

        // GAP-034: file-level resolution — match dependents that target the
        // file's crate `pkg:<name>` node (the parser emits pkg: edges, not
        // file→file edges). Symbol nodes resolve to None.
        // GAP-048: also match the crate family (`pkg:<name>::<member>` and
        // `pkg:<name>_<sibling>` edges) via the Rust-side family filter.
        let pkg_target = if path.starts_with("pkg:") {
            // Bare pkg node queried directly: the exact-match query above
            // already covered `to = <path>`; only the family remains.
            Some(path.clone())
        } else {
            resolver.pkg_node(&path)
        };
        if let Some(pkg) = pkg_target {
            // GAP-083: rows matched through a `pkg:<crate>` node of a FILE
            // query are crate-scoped and cost one extra hop (the file→crate
            // hop), so `depth == 1` stays strictly "true file-level importer".
            let crate_kind = MatchKind {
                scope: SCOPE_CRATE,
                extra: u32::from(file_query),
                via: Some(pkg.as_str()),
            };
            if !path.starts_with("pkg:") {
                collect(
                    stmt.query_map(params![pkg.clone()], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, Option<f64>>(3)?,
                        ))
                    })?,
                    &mut cx,
                    depth,
                    crate_kind,
                )?;
            }
            collect_family(
                family_stmt.query_map(params![format!("{pkg}%")], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<f64>>(4)?,
                    ))
                })?,
                &pkg,
                &mut cx,
                depth,
                crate_kind,
            )?;
        }

        // GAP-069: seed the resolved TS/JS `local:` node family of this
        // BFS node (same importer-filtered semantics as compute_impact
        // above).
        if !path.starts_with("pkg:") {
            if let Some(local) = local_resolver.as_ref() {
                let mut families = local.nodes_for(&path);
                families.sort();
                families.dedup();
                for (node, importers) in families {
                    collect(
                        stmt.query_map(params![node], |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, Option<String>>(2)?,
                                row.get::<_, Option<f64>>(3)?,
                            ))
                        })?
                        .filter(|row| {
                            row.as_ref()
                                .map_or(true, |(from, ..)| importers.contains(from))
                        }),
                        &mut cx,
                        depth,
                        // GAP-069 `local:` nodes resolve to the queried file
                        // itself, so they are file-level (GAP-083).
                        MatchKind {
                            scope: SCOPE_FILE,
                            extra: 0,
                            via: None,
                        },
                    )?;
                }
            }
        }

        // External-edge match (cross-repo).
        // Convert `repo/path/to/file` → `repo:path/to/file` by replacing
        // only the first `/` with `:` to match the `external:repo:path` format.
        if let Some(ref mut estmt) = ext_stmt {
            let ext_path = path.replacen('/', ":", 1);
            let rows = estmt.query_map(params![ext_path], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<f64>>(3)?,
                ))
            })?;
            // `external:repo:path` edges name a file in another repo, so these
            // rows stay file-scoped (GAP-083) — only `pkg:` matches are
            // crate-level, and a `pkg:` node never appears here (the match is
            // on the popped node's own path suffix).
            let ext_kind = MatchKind {
                scope: SCOPE_FILE,
                extra: 0,
                via: None,
            };
            for row in rows {
                let (from, rel, prov, conf) = row?;
                let child_depth = depth + 1;
                if child_depth > cx.max_depth {
                    continue;
                }
                if cx.visited.insert(from.clone()) {
                    cx.results.push(ImpactFile {
                        path: from.clone(),
                        relation: rel,
                        depth: child_depth,
                        scope: ext_kind.scope.to_string(),
                        via: ext_kind.via.map(str::to_string),
                        provenance: prov,
                        confidence: conf,
                    });
                    cx.queue.push_back((from, child_depth));
                }
            }
        }
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::insert_edges_into;
    use crate::parser::{Language, Parser};

    /// Build a two-crate temp workspace:
    /// crates/a (lib) ← crates/b (bin, `use a::Glob;`).
    /// The parser emits (b/src/main.rs → pkg:a); nothing points at the file
    /// crates/a/src/lib.rs directly — resolution must bridge the gap.
    fn build_two_crate_workspace() -> (tempfile::TempDir, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let write = |rel: &str, content: &str| {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, content).unwrap();
            path.to_string_lossy().into_owned()
        };
        write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/a\", \"crates/b\"]\n",
        );
        write(
            "crates/a/Cargo.toml",
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n",
        );
        let lib = write("crates/a/src/lib.rs", "pub struct Glob;\n");
        write(
            "crates/b/Cargo.toml",
            "[package]\nname = \"b\"\nversion = \"0.1.0\"\n",
        );
        let main = write("crates/b/src/main.rs", "use a::Glob;\nfn main() {}\n");
        (dir, lib, main)
    }

    #[test]
    fn impact_on_crate_root_resolves_pkg_dependents() {
        let (_dir, lib, main) = build_two_crate_workspace();

        let conn = duckdb::Connection::open_in_memory().unwrap();
        insert_edges_into(&conn, &[]).unwrap();

        // Parse both files exactly like ensure_parsed/warm does.
        let parse = |path: &str| {
            let source = std::fs::read_to_string(path).unwrap();
            let mut parser = Parser::for_language(Language::Rust).unwrap();
            parser.parse_imports(path, &source).unwrap()
        };
        insert_edges_into(&conn, &parse(&lib)).unwrap();
        insert_edges_into(&conn, &parse(&main)).unwrap();

        // Sanity: pkg: query works (the old happy path). GAP-083 must not
        // change a symbol-node query: same set, same depths.
        let via_pkg = compute_impact(&conn, "pkg:a", 10).unwrap();
        assert!(
            via_pkg.iter().any(|f| f.path == main),
            "pkg:a must be found by main.rs, got: {via_pkg:?}"
        );
        assert_eq!(via_pkg[0].path, main);
        assert_eq!(via_pkg[0].depth, 1, "pkg: query depths are unchanged");
        assert_eq!(via_pkg[0].scope, SCOPE_CRATE);

        // The GAP-034 fix: file-level query resolves to the crate pkg node.
        // GAP-083: those rows are crate-scoped and pay one extra hop (the
        // file→crate hop), so they can never be read as file-level importers.
        let via_file = compute_impact(&conn, &lib, 10).unwrap();
        let crate_row = via_file.iter().find(|f| f.path == main).unwrap_or_else(|| {
            panic!("impact on crate root file must resolve to pkg:a dependents, got: {via_file:?}")
        });
        assert_eq!(crate_row.scope, SCOPE_CRATE);
        assert_eq!(
            crate_row.depth, 2,
            "one crate hop: 1 edge hop + 1 file→pkg:a hop"
        );
        assert_eq!(crate_row.via.as_deref(), Some("pkg:a"));
    }

    /// GAP-083 AC 3: `depth == 1` must mean "this file really imports the
    /// queried file". A row reached through the file's `pkg:<crate>` node is
    /// crate-scoped and sits at depth >= 2, while a true file-level row keeps
    /// depth 1 even when the same fixture also holds crate-level rows.
    #[test]
    fn file_query_separates_file_level_rows_from_crate_level_rows() {
        let (_dir, lib, main) = build_two_crate_workspace();

        let conn = duckdb::Connection::open_in_memory().unwrap();
        insert_edges_into(&conn, &[]).unwrap();
        let parse = |path: &str| {
            let source = std::fs::read_to_string(path).unwrap();
            let mut parser = Parser::for_language(Language::Rust).unwrap();
            parser.parse_imports(path, &source).unwrap()
        };
        insert_edges_into(&conn, &parse(&lib)).unwrap();
        insert_edges_into(&conn, &parse(&main)).unwrap();

        // A TRUE file-level importer: an edge whose target IS the queried file
        // (the Rust parser emits `pkg:` edges for `use a::...`, so this is
        // inserted directly — the shape a `mod`/path edge produces).
        let direct = "crates/c/src/lib.rs".to_string();
        insert_edges_into(
            &conn,
            &[crate::Edge {
                from: direct.clone(),
                to: lib.clone(),
                rel: "imports".into(),
                provenance: "ast_exact".into(),
                confidence: 1.0,
            }],
        )
        .unwrap();

        let results = compute_impact(&conn, &lib, 10).unwrap();

        let file_row = results
            .iter()
            .find(|f| f.path == direct)
            .unwrap_or_else(|| panic!("file-level importer must be reported: {results:?}"));
        assert_eq!(file_row.scope, SCOPE_FILE);
        assert_eq!(file_row.depth, 1, "a true file-level importer stays at 1");
        assert_eq!(file_row.via, None);

        let crate_row = results
            .iter()
            .find(|f| f.path == main)
            .unwrap_or_else(|| panic!("crate-level importer must be reported: {results:?}"));
        assert_eq!(crate_row.scope, SCOPE_CRATE);
        assert!(
            crate_row.depth >= 2,
            "crate-level rows can never claim depth 1, got {}",
            crate_row.depth
        );
        assert_eq!(crate_row.via.as_deref(), Some("pkg:a"));

        // The exact-match query runs before the crate expansion, so the true
        // file-level row is discovered (and ordered) first.
        assert_eq!(results[0].path, direct);
    }

    /// GAP-083: a crate-level row must also respect the caller's depth budget —
    /// the extra hop cannot push a row past `max_depth`.
    #[test]
    fn crate_rows_respect_max_depth_budget() {
        let (_dir, lib, main) = build_two_crate_workspace();

        let conn = duckdb::Connection::open_in_memory().unwrap();
        insert_edges_into(&conn, &[]).unwrap();
        let parse = |path: &str| {
            let source = std::fs::read_to_string(path).unwrap();
            let mut parser = Parser::for_language(Language::Rust).unwrap();
            parser.parse_imports(path, &source).unwrap()
        };
        insert_edges_into(&conn, &parse(&lib)).unwrap();
        insert_edges_into(&conn, &parse(&main)).unwrap();

        // max_depth = 1: only true file-level dependents fit (there are none).
        let shallow = compute_impact(&conn, &lib, 1).unwrap();
        assert!(
            shallow.is_empty(),
            "depth-1 budget must not admit crate-level rows: {shallow:?}"
        );

        // max_depth = 2: exactly the crate hop fits.
        let deep = compute_impact(&conn, &lib, 2).unwrap();
        assert_eq!(deep.len(), 1, "crate row fits at depth 2: {deep:?}");
        assert_eq!(deep[0].path, main);
        assert_eq!(deep[0].depth, 2);
        assert_eq!(deep[0].scope, SCOPE_CRATE);
    }

    #[test]
    fn impact_unknown_file_without_manifest_resolves_none() {
        // A file not under any Cargo.toml must not blow up and keeps the
        // exact-match behavior (empty results are fine).
        let dir = tempfile::tempdir().unwrap();
        let orphan = dir.path().join("src/orphan.rs");
        std::fs::create_dir_all(orphan.parent().unwrap()).unwrap();
        std::fs::write(&orphan, "fn x() {}\n").unwrap();
        let orphan = orphan.to_string_lossy().into_owned();

        let conn = duckdb::Connection::open_in_memory().unwrap();
        insert_edges_into(&conn, &[]).unwrap();
        let results = compute_impact(&conn, &orphan, 10).unwrap();
        assert!(results.is_empty());
    }

    /// GAP-048: the parser expands brace imports (`use a::{Glob, GlobSet}`)
    /// into `pkg:a::<member>` pseudo-nodes. Impact on the crate root file —
    /// and on the bare `pkg:a` node — must match those member edges, or the
    /// blast radius answers a tiny fraction of the truth (6/148 on serde).
    #[test]
    fn impact_on_crate_root_matches_brace_member_edges() {
        let dir = tempfile::tempdir().unwrap();
        let write = |rel: &str, content: &str| {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, content).unwrap();
            path.to_string_lossy().into_owned()
        };
        write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/a\", \"crates/b\"]\n",
        );
        write(
            "crates/a/Cargo.toml",
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n",
        );
        let lib = write(
            "crates/a/src/lib.rs",
            "pub struct Glob;\npub struct GlobSet;\n",
        );
        write(
            "crates/b/Cargo.toml",
            "[package]\nname = \"b\"\nversion = \"0.1.0\"\n",
        );
        let main = write(
            "crates/b/src/main.rs",
            "use a::{Glob, GlobSet};\nfn main() {}\n",
        );

        let conn = duckdb::Connection::open_in_memory().unwrap();
        insert_edges_into(&conn, &[]).unwrap();
        let parse = |path: &str| {
            let source = std::fs::read_to_string(path).unwrap();
            let mut parser = Parser::for_language(Language::Rust).unwrap();
            parser.parse_imports(path, &source).unwrap()
        };
        insert_edges_into(&conn, &parse(&lib)).unwrap();
        insert_edges_into(&conn, &parse(&main)).unwrap();

        // Premise: the parser emits member-prefixed edges, not pkg:a.
        let member_edges = compute_impact(&conn, "pkg:a::Glob", 10).unwrap();
        assert!(
            member_edges.iter().any(|f| f.path == main),
            "pkg:a::Glob must be found by main.rs, got: {member_edges:?}"
        );

        // File-level query: the crate root must match member edges via the
        // pkg prefix (the GAP-048 fix).
        let via_file = compute_impact(&conn, &lib, 10).unwrap();
        let crate_row = via_file.iter().find(|f| f.path == main).unwrap_or_else(|| {
            panic!("impact on crate root must match pkg:a::<member> edges, got: {via_file:?}")
        });
        // GAP-083: member edges are crate-level too — never depth 1.
        assert_eq!(crate_row.scope, SCOPE_CRATE);
        assert_eq!(crate_row.depth, 2);
        assert_eq!(crate_row.via.as_deref(), Some("pkg:a"));

        // Bare pkg node query must match member edges too.
        let via_pkg = compute_impact(&conn, "pkg:a", 10).unwrap();
        assert!(
            via_pkg.iter().any(|f| f.path == main),
            "impact on pkg:a must match pkg:a::<member> edges, got: {via_pkg:?}"
        );
    }

    /// GAP-048 negative: the pkg-family match must be anchored — a query
    /// for `pkg:a` must not match member edges of a sibling crate whose
    /// name merely starts with `a` (e.g. `ab`), nor vice versa.
    #[test]
    fn impact_member_prefix_does_not_leak_across_sibling_crates() {
        let dir = tempfile::tempdir().unwrap();
        let write = |rel: &str, content: &str| {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, content).unwrap();
            path.to_string_lossy().into_owned()
        };
        write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/a\", \"crates/ab\", \"crates/b\"]\n",
        );
        write(
            "crates/a/Cargo.toml",
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n",
        );
        let a_lib = write("crates/a/src/lib.rs", "pub struct Glob;\n");
        write(
            "crates/ab/Cargo.toml",
            "[package]\nname = \"ab\"\nversion = \"0.1.0\"\n",
        );
        write("crates/ab/src/lib.rs", "pub struct Thing;\n");
        write(
            "crates/b/Cargo.toml",
            "[package]\nname = \"b\"\nversion = \"0.1.0\"\n",
        );
        // b imports ONLY from crate ab — nothing from crate a.
        let main = write("crates/b/src/main.rs", "use ab::Thing;\nfn main() {}\n");

        let conn = duckdb::Connection::open_in_memory().unwrap();
        insert_edges_into(&conn, &[]).unwrap();
        let parse = |path: &str| {
            let source = std::fs::read_to_string(path).unwrap();
            let mut parser = Parser::for_language(Language::Rust).unwrap();
            parser.parse_imports(path, &source).unwrap()
        };
        for f in [&a_lib, &main] {
            insert_edges_into(&conn, &parse(f)).unwrap();
        }

        // The member edge pkg:ab::Thing must not resolve for a query on
        // crate a (the anchored family match cannot match pkg:ab::Thing).
        let via_a = compute_impact(&conn, &a_lib, 10).unwrap();
        assert!(
            !via_a.iter().any(|f| f.path == main),
            "crate a impact must not leak crate ab member edges, got: {via_a:?}"
        );
        let via_pkg_a = compute_impact(&conn, "pkg:a", 10).unwrap();
        assert!(
            !via_pkg_a.iter().any(|f| f.path == main),
            "pkg:a impact must not leak pkg:ab member edges, got: {via_pkg_a:?}"
        );
    }

    /// GAP-048 family semantics: a crate query also matches its
    /// underscore-sibling companion crates (`pkg:a_derive` for `pkg:a` —
    /// the Rust companion-crate convention, e.g. serde/serde_derive),
    /// which the crate re-exports into its public surface.
    #[test]
    fn impact_on_crate_root_matches_underscore_sibling_crates() {
        let dir = tempfile::tempdir().unwrap();
        let write = |rel: &str, content: &str| {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, content).unwrap();
            path.to_string_lossy().into_owned()
        };
        write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/a\", \"crates/a_derive\", \"crates/b\"]\n",
        );
        write(
            "crates/a/Cargo.toml",
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n",
        );
        let a_lib = write("crates/a/src/lib.rs", "pub struct Glob;\n");
        write(
            "crates/a_derive/Cargo.toml",
            "[package]\nname = \"a_derive\"\nversion = \"0.1.0\"\n",
        );
        write("crates/a_derive/src/lib.rs", "pub struct Derive;\n");
        write(
            "crates/b/Cargo.toml",
            "[package]\nname = \"b\"\nversion = \"0.1.0\"\n",
        );
        // b imports ONLY from companion crate a_derive — nothing from a.
        let main = write(
            "crates/b/src/main.rs",
            "use a_derive::Derive;\nfn main() {}\n",
        );

        let conn = duckdb::Connection::open_in_memory().unwrap();
        insert_edges_into(&conn, &[]).unwrap();
        let parse = |path: &str| {
            let source = std::fs::read_to_string(path).unwrap();
            let mut parser = Parser::for_language(Language::Rust).unwrap();
            parser.parse_imports(path, &source).unwrap()
        };
        for f in [&a_lib, &main] {
            insert_edges_into(&conn, &parse(f)).unwrap();
        }

        let via_file = compute_impact(&conn, &a_lib, 10).unwrap();
        let sibling_row = via_file.iter().find(|f| f.path == main).unwrap_or_else(|| {
            panic!("crate a file impact must include a_derive importers, got: {via_file:?}")
        });
        // GAP-083: family (sibling) matches are crate-level as well.
        assert_eq!(sibling_row.scope, SCOPE_CRATE);
        assert_eq!(sibling_row.depth, 2);
        assert_eq!(sibling_row.via.as_deref(), Some("pkg:a"));
        let via_pkg_a = compute_impact(&conn, "pkg:a", 10).unwrap();
        assert!(
            via_pkg_a.iter().any(|f| f.path == main),
            "pkg:a impact must include a_derive importers, got: {via_pkg_a:?}"
        );
    }
}
