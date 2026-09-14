//! DuckDB graph initialization and edge querying.
//!
//! Creates and manages the `.vfs/graph/graph.db` database for graph edge
//! storage and querying.

use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use duckdb::{params, Connection};
use hilo_metadata::inventory::Edge;

use crate::error::{GraphError, GraphResult};
use crate::impact::{self, ImpactFile};
use crate::parser::{Language, Parser};
use crate::resolution::PkgResolver;

/// Direction for edge queries: forward (`"from" = ?`) or reverse (`"to" = ?`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Query outgoing edges: `WHERE "from" = ?`.
    Forward,
    /// Query incoming edges: `WHERE "to" = ?`.
    Reverse,
}

impl std::fmt::Display for Direction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Direction::Forward => write!(f, "forward"),
            Direction::Reverse => write!(f, "reverse"),
        }
    }
}

impl Direction {
    /// Parse a direction string.  Recognises "reverse", "incoming", "in", and
    /// "backward" (case-insensitive).  Everything else defaults to `Forward`.
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "reverse" | "incoming" | "in" | "backward" => Direction::Reverse,
            _ => Direction::Forward,
        }
    }
}

/// Manages the DuckDB graph database at `.vfs/graph/graph.db`.
pub struct GraphDB {
    conn: Connection,
}

/// Aggregate statistics computed over the `edges` table.
#[derive(Debug, Clone, serde::Serialize)]
pub struct GraphStats {
    /// Total number of rows in `edges`.
    pub total_edges: i64,
    /// Total number of distinct source files in the graph.
    pub total_files: i64,
    /// Count of distinct `from` values (source files).
    #[serde(skip)]
    pub unique_files: i64,
    /// Count of distinct `to` values (unique dependencies).
    #[serde(skip)]
    pub unique_dependencies: i64,
    /// The single most-referenced file in the graph, if any.
    pub most_connected: Option<String>,
    /// Files that appear as `from` but have no edges pointing at them.
    pub orphans: Vec<String>,
    /// Edge count broken down by relation type.
    pub edge_types: std::collections::HashMap<String, i64>,
    /// The top 10 most-referenced dependencies as `(to, count)` pairs,
    /// ordered by reference count descending.
    #[serde(skip)]
    pub top_dependencies: Vec<(String, i64)>,
}

/// Per-module statistics returned by `vfs_graph_module`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModuleStats {
    /// The module prefix (e.g. "src/auth/").
    pub module: String,
    /// All distinct file paths within the module.
    pub files: Vec<String>,
    /// Total number of edges touching files in this module.
    pub edges_count: i64,
    /// Percentage of files that have test coverage (0.0–100.0).
    pub test_coverage_pct: f64,
}

// ──────────── Free functions for raw DuckDB connections ────────────

/// Ensure the `edges` table schema exists on a raw DuckDB connection.
///
/// Creates the table (IF NOT EXISTS), auto-migrates old 3-column schemas,
/// and creates lookup indexes. All statements are idempotent — safe to call
/// on a connection that already has the schema, including connections opened
/// via plain `duckdb::Connection::open` (without `GraphDB::open`).
pub fn ensure_schema(conn: &Connection) -> GraphResult<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS edges (\
            \"from\" TEXT NOT NULL,\
            \"to\" TEXT NOT NULL,\
            rel TEXT NOT NULL,\
            provenance TEXT NOT NULL DEFAULT 'ast_exact',\
            confidence REAL NOT NULL DEFAULT 1.0\
         )",
        params![],
    )?;

    // Auto-migrate: if the table was created with the old 3-column
    // schema (pre-v0.2), add the missing columns. DuckDB's
    // `pragma_table_info` lets us check without parsing CREATE TABLE.
    migrate_schema(conn)?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_edges_from_rel ON edges(\"from\", rel)",
        params![],
    )?;
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_edges_unique ON edges(\"from\", \"to\", rel, provenance)",
        params![],
    )?;
    Ok(())
}

/// Check for and apply schema migrations for the `edges` table.
///
/// Currently handles one migration:
/// - v0.1 (3-column) → v0.2 (5-column): add `provenance` and `confidence`.
///
/// Uses `pragma_table_info('edges')` to check column existence. If
/// `provenance` is missing, both columns are added with ALTER TABLE.
fn migrate_schema(conn: &Connection) -> GraphResult<()> {
    // Check if 'provenance' column exists.
    let has_provenance: bool = {
        let mut stmt = conn
            .prepare("SELECT count(*) FROM pragma_table_info('edges') WHERE name = 'provenance'")?;
        let count: i64 = stmt.query_row(params![], |row| row.get(0))?;
        count > 0
    };

    if !has_provenance {
        // Old 3-column schema → add provenance + confidence.
        // DuckDB doesn't support ADD COLUMN with NOT NULL constraints,
        // so we add nullable columns with defaults and then backfill.
        conn.execute(
            "ALTER TABLE edges ADD COLUMN provenance TEXT DEFAULT 'ast_exact'",
            params![],
        )?;
        conn.execute(
            "ALTER TABLE edges ADD COLUMN confidence REAL DEFAULT 1.0",
            params![],
        )?;
        // Backfill any NULLs (shouldn't be any due to DEFAULT, but be safe).
        conn.execute(
            "UPDATE edges SET provenance = 'ast_exact' WHERE provenance IS NULL",
            params![],
        )?;
        conn.execute(
            "UPDATE edges SET confidence = 1.0 WHERE confidence IS NULL",
            params![],
        )?;
    }

    Ok(())
}

/// Insert edges into a raw DuckDB connection (INSERT OR IGNORE, idempotent).
///
/// Ensures the schema exists first (idempotent `CREATE TABLE IF NOT EXISTS`
/// and indexes), then inserts each edge with `INSERT OR IGNORE`. Safe to call
/// on connections opened via plain `duckdb::Connection::open` (without
/// `GraphDB::open`).
pub fn insert_edges_into(conn: &Connection, edges: &[Edge]) -> GraphResult<()> {
    ensure_schema(conn)?;
    // PERF-002: single transaction + prepared statement — the row-by-row
    // version made `graph warm` pay ~1ms per edge even on full cache hits.
    let mut stmt = conn.prepare(
        "INSERT OR IGNORE INTO edges (\"from\", \"to\", rel, provenance, confidence) VALUES (?, ?, ?, ?, ?)",
    )?;
    conn.execute_batch("BEGIN TRANSACTION")?;
    for edge in edges {
        stmt.execute(params![
            edge.from,
            edge.to,
            edge.rel,
            edge.provenance,
            edge.confidence
        ])?;
    }
    conn.execute_batch("COMMIT")?;
    Ok(())
}

/// Fingerprint of `edges.jsonl` used to decide whether the DuckDB cache is
/// stale: `<mtime-nanos>:<size>`. Any real writer (JIT-001 write-through,
/// parse-and-diff append, `graph clean` + re-warm, another process) changes
/// the mtime or the length, so a matching stamp means the cache was built
/// from exactly this file. (PERF-001: stamp-only, no row-count parity —
/// verifying counts would require re-reading the file and defeat the gate.)
fn jsonl_fingerprint(edges_jsonl: &Path) -> Option<String> {
    let md = std::fs::metadata(edges_jsonl).ok()?;
    let nanos = md
        .modified()
        .ok()?
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(format!("{}:{}", nanos, md.len()))
}

/// Path of the reconcile stamp next to a graph DB / edges.jsonl pair.
/// Both live in `.vfs/graph/`, so either path's parent is the stamp dir.
fn reconcile_stamp_path(graph_dir_file: &Path) -> PathBuf {
    graph_dir_file
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".last_reconcile")
}

/// True when the DuckDB cache must be reconciled from `edges.jsonl`.
///
/// Skipped (cache trusted) only when a stamp from a previous *successful full
/// replay* matches the current fingerprint. Missing/unreadable stamp ->
/// reconcile (first run, legacy cache, post-`graph clean`).
fn reconcile_needed(edges_jsonl: &Path) -> bool {
    let Some(fp) = jsonl_fingerprint(edges_jsonl) else {
        return false; // no edges.jsonl -> open()'s reconcile no-ops anyway
    };
    match std::fs::read_to_string(reconcile_stamp_path(edges_jsonl)) {
        Ok(stamped) => stamped.trim() != fp,
        Err(_) => true,
    }
}

/// Reconcile the DuckDB cache from the canonical `edges.jsonl` file.
///
/// Reads every non-empty line from `edges_jsonl`, deserialises each as an
/// [`Edge`] (via serde, which fills `provenance`/`confidence` defaults for
/// old-format lines), and inserts via [`insert_edges_into`] in batches of 512.
/// Malformed lines are silently skipped — a corrupt line does not abort the
/// whole reconcile.
///
/// Returns the number of edges **successfully parsed and inserted** (including
/// duplicates that were ignored by `INSERT OR IGNORE`). This is the count of
/// lines processed, not the count of *new* rows added.
///
/// - Missing file → `Ok(0)` (no-op, fresh project — not an error).
/// - Idempotent: calling twice inserts the same edges, `INSERT OR IGNORE` +
///   unique index ensures no duplicates.
pub fn reconcile_edges_from_jsonl(conn: &Connection, edges_jsonl: &Path) -> GraphResult<usize> {
    if !edges_jsonl.exists() {
        return Ok(0);
    }

    let file = std::fs::File::open(edges_jsonl)?;
    let reader = BufReader::new(file);

    // PERF-001: one prepared statement inside a single transaction. The old
    // path re-ran ensure_schema and issued one un-prepared execute per row
    // (~1.6 ms/row on DuckDB — ~12 s for tokio's 7.4k edges). Transaction +
    // prepared execute is orders of magnitude cheaper, which keeps a full
    // replay affordable whenever the stamp gate in `open()` does miss.
    ensure_schema(conn)?;
    conn.execute_batch("BEGIN TRANSACTION")?;

    let replay = (|| -> GraphResult<usize> {
        let mut stmt = conn.prepare(
            "INSERT OR IGNORE INTO edges (\"from\", \"to\", rel, provenance, confidence) VALUES (?, ?, ?, ?, ?)",
        )?;
        let mut count: usize = 0;
        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => continue, // skip unreadable lines
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<Edge>(trimmed) {
                Ok(edge) => {
                    stmt.execute(params![
                        edge.from,
                        edge.to,
                        edge.rel,
                        edge.provenance,
                        edge.confidence
                    ])?;
                    count += 1;
                }
                Err(_) => continue, // skip malformed JSON lines
            }
        }
        Ok(count)
    })();

    match replay {
        Ok(count) => {
            conn.execute_batch("COMMIT")?;
            // Stamp AFTER a successful full replay so the next open() can
            // trust the cache without touching edges.jsonl.
            if let Some(fp) = jsonl_fingerprint(edges_jsonl) {
                let _ = std::fs::write(reconcile_stamp_path(edges_jsonl), fp);
            }
            Ok(count)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

impl GraphDB {
    /// Open (or create) the DuckDB database at `path`.
    ///
    /// Pass `":memory:"` for an ephemeral in-memory database (useful for
    /// tests). The `edges` table and its lookup index are created if missing.
    ///
    /// For on-disk databases, this also performs **read-through cache
    /// reconciliation**: if a sibling `edges.jsonl` file exists in the same
    /// directory as `path`, it is read and any edges missing from the DuckDB
    /// cache are inserted (via `INSERT OR IGNORE`, idempotent). This ensures
    /// that edges appended to `edges.jsonl` by a process or binary without
    /// JIT-001 write-through are still visible to queries after the next open.
    /// Malformed lines in `edges.jsonl` are silently skipped.
    pub fn open(path: &str) -> GraphResult<Self> {
        let conn = if path == ":memory:" {
            Connection::open_in_memory()?
        } else {
            Connection::open(path)?
        };
        ensure_schema(&conn)?;

        // Read-through reconciliation: if a sibling edges.jsonl exists, load
        // any edges missing from the DuckDB cache. Only for on-disk DBs —
        // ":memory:" connections have no sibling file and are used in tests.
        if path != ":memory:" {
            let jsonl_path = Path::new(path).parent().map(|dir| dir.join("edges.jsonl"));
            if let Some(jsonl) = jsonl_path {
                // PERF-001: skip the full edges.jsonl replay when a previous
                // successful replay stamped this exact file (fingerprint
                // match). Any writer that appends/rewrites edges.jsonl (JIT-001
                // write-through, parse-and-diff, graph warm, another process)
                // changes the mtime/size -> mismatch -> full reconcile runs.
                // reconcile_edges_from_jsonl returns Ok(0) if the file is
                // missing — safe no-op for fresh projects.
                if reconcile_needed(&jsonl) {
                    reconcile_edges_from_jsonl(&conn, &jsonl)?;
                }
            }
        }

        Ok(GraphDB { conn })
    }

    /// Insert multiple edges into the database using a prepared statement.
    ///
    /// Delegates to [`insert_edges_into`] (the free function) so the INSERT
    /// SQL is defined in exactly one place.
    pub fn insert_edges(&self, edges: &[Edge]) -> GraphResult<()> {
        insert_edges_into(&self.conn, edges)
    }

    /// Return the total number of rows in `edges` (`SELECT COUNT(*) FROM edges`).
    pub fn count_edges(&self) -> GraphResult<i64> {
        let count = self
            .conn
            .query_row("SELECT COUNT(*) FROM edges", params![], |row| {
                row.get::<_, i64>(0)
            })?;
        Ok(count)
    }

    /// Group edges by `("to", rel)` and return `(to, rel, count)` triples
    /// ordered by count descending.
    pub fn group_by_dependency(&self) -> GraphResult<Vec<(String, String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT \"to\", rel, COUNT(*) AS cnt \
             FROM edges \
             GROUP BY \"to\", rel \
             ORDER BY cnt DESC",
        )?;
        let rows = stmt.query_map(params![], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Return the distinct source files and distinct dependencies.
    ///
    /// The first element of the tuple is the set of distinct `from` values,
    /// the second is the set of distinct `to` values.
    pub fn distinct_files(&self) -> GraphResult<(Vec<String>, Vec<String>)> {
        let froms = {
            let mut stmt = self.conn.prepare("SELECT DISTINCT \"from\" FROM edges")?;
            let rows = stmt.query_map(params![], |row| row.get::<_, String>(0))?;
            let mut v = Vec::new();
            for r in rows {
                v.push(r?);
            }
            v
        };
        let tos = {
            let mut stmt = self.conn.prepare("SELECT DISTINCT \"to\" FROM edges")?;
            let rows = stmt.query_map(params![], |row| row.get::<_, String>(0))?;
            let mut v = Vec::new();
            for r in rows {
                v.push(r?);
            }
            v
        };
        Ok((froms, tos))
    }

    /// Query edges for a file path, optionally filtered by relation type and
    /// direction.
    ///
    /// - `Forward` (default): `WHERE "from" = ?` — outgoing edges.
    /// - `Reverse`: `WHERE "to" = ?` — incoming edges (e.g. `imported_by`).
    ///
    /// Returns an empty `Vec` if no edges match.
    pub fn related(
        &self,
        path: &str,
        rel_filter: Option<&str>,
        direction: Direction,
    ) -> GraphResult<Vec<Edge>> {
        let column = match direction {
            Direction::Forward => "\"from\"",
            Direction::Reverse => "\"to\"",
        };

        // GAP-034: reverse lookups on a file must also match dependents that
        // target the file's crate `pkg:<name>` node — the parser emits pkg:
        // edges, not file→file edges, so a plain `WHERE "to" = <file>`
        // returns nothing for files that are only imported as part of their
        // crate. Symbol nodes (`pkg:...`/`sys:...`) resolve to None.
        let mut targets: Vec<String> = vec![path.to_string()];
        if direction == Direction::Reverse {
            if let Some(pkg) = crate::resolution::PkgResolver::new().pkg_node(path) {
                targets.push(pkg);
            }
        }

        let mut edges = Vec::new();
        for target in &targets {
            let (sql, params_vec): (String, Vec<Box<dyn duckdb::ToSql>>) = if let Some(rel) =
                rel_filter
            {
                (
                        format!(
                            "SELECT \"from\", \"to\", rel, provenance, confidence FROM edges WHERE {column} = ? AND rel = ?"
                        ),
                        vec![
                            Box::new(target.clone()),
                            Box::new(rel.to_string()),
                        ],
                    )
            } else {
                (
                        format!(
                            "SELECT \"from\", \"to\", rel, provenance, confidence FROM edges WHERE {column} = ?"
                        ),
                        vec![Box::new(target.clone())],
                    )
            };
            let param_refs: Vec<&dyn duckdb::ToSql> =
                params_vec.iter().map(|p| p.as_ref()).collect();

            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(param_refs.as_slice(), |row| {
                Ok(Edge {
                    from: row.get::<_, String>(0)?,
                    to: row.get::<_, String>(1)?,
                    rel: row.get::<_, String>(2)?,
                    provenance: row.get::<_, String>(3)?,
                    confidence: row.get::<_, f64>(4)?,
                })
            })?;
            for row in rows {
                edges.push(row?);
            }
        }

        // GAP-069: TS/JS `local:` nodes — raw specifiers the JS/TS parser
        // kept verbatim, one per (importer dir, specifier) pair, and the
        // SAME node string can be emitted from different directories naming
        // DIFFERENT targets. The reverse index resolves each node against
        // its own importer's directory; a kept edge must name this file's
        // node AND its importer must be one of the directories whose spec
        // actually lands on this file (`from ∈ allowed importers`), so
        // same-named nodes from other directories never cross-match.
        if direction == Direction::Reverse {
            if let Ok(resolver) = crate::resolution::LocalSpecResolver::from_edges(&self.conn) {
                let mut keep = resolver.nodes_for(path);
                keep.sort();
                keep.dedup();
                for (node, importers) in keep {
                    // The rel filter applies to local:-resolved targets
                    // exactly as it does to plain targets above.
                    let (sql, params_vec): (String, Vec<Box<dyn duckdb::ToSql>>) =
                        if let Some(rel) = rel_filter {
                            (
                                "SELECT \"from\", \"to\", rel, provenance, confidence \
                                 FROM edges WHERE \"to\" = ? AND rel = ?"
                                    .to_string(),
                                vec![Box::new(node), Box::new(rel.to_string())],
                            )
                        } else {
                            (
                                "SELECT \"from\", \"to\", rel, provenance, confidence \
                                 FROM edges WHERE \"to\" = ?"
                                    .to_string(),
                                vec![Box::new(node)],
                            )
                        };
                    let param_refs: Vec<&dyn duckdb::ToSql> =
                        params_vec.iter().map(|p| p.as_ref()).collect();
                    let mut stmt = self.conn.prepare(&sql)?;
                    let rows = stmt.query_map(param_refs.as_slice(), |row| {
                        Ok(Edge {
                            from: row.get::<_, String>(0)?,
                            to: row.get::<_, String>(1)?,
                            rel: row.get::<_, String>(2)?,
                            provenance: row.get::<_, String>(3)?,
                            confidence: row.get::<_, f64>(4)?,
                        })
                    })?;
                    for row in rows {
                        let edge = row?;
                        if importers.contains(&edge.from) {
                            edges.push(edge);
                        }
                    }
                }
            }
        }
        Ok(edges)
    }

    /// Check whether a file path exists in the `edges` table (as `from` or `to`).
    pub fn file_in_graph(&self, path: &str) -> GraphResult<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM edges WHERE \"from\" = ? OR \"to\" = ?",
            params![path, path],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count > 0)
    }

    /// Access the underlying DuckDB connection (for direct queries by other modules).
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Find files that have import edges but no `tested_by` edges pointing at them.
    ///
    /// Test and bench files are excluded: they legitimately have `imports`
    /// edges (they import the crate under test) without ever being the
    /// *target* of a `tested_by` edge, so an unfiltered query would list
    /// every test file as "untested". Files matching the same
    /// depth-agnostic test/bench path patterns used by `classify` (top-level
    /// `tests/`/`benches/`, nested `crates/*/tests/`, `*_test.rs`, ...) are
    /// filtered out before returning.
    ///
    /// Returns the list of *production* source files that import other files
    /// but are not covered by any test (sorted alphabetically).
    ///
    /// Delegates to [`GraphDB::untested_files_at`] with the process cwd as
    /// the resolution root, so `pkg:`-resolved coverage (GAP-066) works for
    /// callers that query a graph laid out relative to their own cwd.
    pub fn untested_files(&self) -> GraphResult<Vec<String>> {
        self.untested_files_at(Path::new("."))
    }

    /// [`GraphDB::untested_files`] with an explicit resolution root.
    ///
    /// `root` is joined onto every candidate file path before the file is
    /// resolved to its `pkg:` node, because the parser emits `tested_by`
    /// edges to package nodes (`pkg:<crate>`, `pkg:<go import path>`,
    /// `pkg:<dotted module>`) rather than to the covered file itself
    /// (GAP-066). Edge paths are stored relative to the repo root, so
    /// `root` must be that repo root — otherwise a covered file resolves to
    /// `None` and is wrongly reported as untested.
    ///
    /// A production file counts as covered when EITHER it is the literal
    /// `to` of some `tested_by` edge (the pre-GAP-066 behavior, kept for
    /// file-level edges such as `tests/lib_test.rs -> src/lib.rs`) OR —
    /// for languages whose package node is finer than the file, i.e. Python
    /// and Go — its resolved `pkg:` node is the literal `to` of some
    /// `tested_by` edge. `.rs` files are deliberately excluded from the
    /// second rule: their resolved node is the Cargo CRATE, and one
    /// crate-root integration test would otherwise mark every member file
    /// covered (GAP-057/064 lineage — see [`GraphDB::file_is_covered`]).
    /// Package-ancestor matching is deliberately out of scope: a test that
    /// targets `pkg:fastapi.dependencies` does not cover
    /// `fastapi/dependencies/utils.py`.
    pub fn untested_files_at(&self, root: &Path) -> GraphResult<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT \"from\" FROM edges \
             WHERE rel = 'imports' \
             AND \"from\" NOT IN (SELECT \"to\" FROM edges WHERE rel = 'tested_by') \
             ORDER BY \"from\"",
        )?;
        let rows = stmt.query_map(params![], |row| row.get::<_, String>(0))?;
        let mut files = Vec::new();
        for r in rows {
            files.push(r?);
        }
        // Exclude test/bench files — they are not "untested production code".
        // Same predicate classify uses for role=test, so the two commands
        // can never disagree about a file (GAP-036).
        files.retain(|f| !crate::classify::is_test_file(f));

        // GAP-066: the remaining files may still be covered through their
        // package node. One resolver instance for the whole query so each
        // path costs at most one filesystem walk.
        let tested = self.tested_by_targets()?;
        let mut resolver = PkgResolver::new();
        files.retain(|f| !Self::file_is_covered(f, &tested, root, &mut resolver));
        Ok(files)
    }

    /// Query module-level statistics for a given directory prefix.
    ///
    /// Returns all distinct files (both `from` and `to`) whose path starts
    /// with `module_name`, along with the total edge count and test-coverage
    /// percentage (covered files ÷ total files).
    ///
    /// Delegates to [`GraphDB::module_files_at`] with the process cwd as the
    /// resolution root.
    pub fn module_files(&self, module_name: &str) -> GraphResult<ModuleStats> {
        self.module_files_at(Path::new("."), module_name)
    }

    /// [`GraphDB::module_files`] with an explicit resolution root (GAP-066).
    ///
    /// `test_coverage_pct` counts a file under the prefix as covered under
    /// the same two rules `untested_files_at` uses (literal `tested_by`
    /// target, or the file's resolved `pkg:` node is one, where the package
    /// node is finer than the file — Python and Go). `.rs` files are
    /// deliberately excluded from the node rule: `PkgResolver::pkg_node`
    /// returns the enclosing Cargo CRATE for a `.rs` file, so a single
    /// crate-root `tested_by` edge would report a crate's whole `src/`
    /// module as covered (GAP-057/064 lineage — see
    /// [`GraphDB::file_is_covered`]). `files` and `edges_count` are the
    /// unmodified file-level views of the module.
    pub fn module_files_at(&self, root: &Path, module_name: &str) -> GraphResult<ModuleStats> {
        let prefix = if module_name.ends_with('/') {
            module_name.to_string()
        } else {
            format!("{module_name}/")
        };

        // Escape LIKE pattern: `%` and `_` are special in LIKE.  They are
        // extremely unlikely in directory names but defensively escape them.
        let like_prefix = prefix.replace('%', "\\%").replace('_', "\\_");

        let files: Vec<String> = {
            let sql = "SELECT DISTINCT path FROM (\
                 SELECT \"from\" AS path FROM edges WHERE \"from\" LIKE ?1 ESCAPE '\\'\
                 UNION \
                 SELECT \"to\" AS path FROM edges WHERE \"to\" LIKE ?1 ESCAPE '\\')\
                 ORDER BY path"
                .to_string();
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(params![format!("{like_prefix}%")], |row| {
                row.get::<_, String>(0)
            })?;
            let mut v = Vec::new();
            for r in rows {
                v.push(r?);
            }
            v
        };

        let total_files = files.len() as i64;

        let edges_count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM edges \
             WHERE \"from\" LIKE ?1 ESCAPE '\\' OR \"to\" LIKE ?1 ESCAPE '\\'",
            params![format!("{like_prefix}%")],
            |row| row.get::<_, i64>(0),
        )?;

        // GAP-066: files in the module that are covered by a test, whether
        // the `tested_by` edge names the file itself or the `pkg:` node it
        // belongs to.
        let tested = self.tested_by_targets()?;
        let mut resolver = PkgResolver::new();
        let tested_count = files
            .iter()
            .filter(|f| Self::file_is_covered(f, &tested, root, &mut resolver))
            .count() as i64;

        let test_coverage_pct = if total_files > 0 {
            ((tested_count as f64 / total_files as f64) * 100.0 * 10.0).round() / 10.0
        } else {
            0.0
        };

        Ok(ModuleStats {
            module: module_name.to_string(),
            files,
            edges_count,
            test_coverage_pct,
        })
    }

    /// The set of literal `to` nodes of every `tested_by` edge.
    ///
    /// GAP-066: shared by the coverage consumers so both agree on what the
    /// target of a test is (either a file path or a `pkg:` node).
    fn tested_by_targets(&self) -> GraphResult<HashSet<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT \"to\" FROM edges WHERE rel = 'tested_by'")?;
        let rows = stmt.query_map(params![], |row| row.get::<_, String>(0))?;
        let mut targets = HashSet::new();
        for r in rows {
            targets.insert(r?);
        }
        Ok(targets)
    }

    /// Whether `file` counts as covered by some test (GAP-066).
    ///
    /// Rule (i): `file` is the literal target of a `tested_by` edge. Rule
    /// (ii): the `pkg:` node `file` resolves to (via `root`) is. A file that
    /// resolves to no package node is never covered by rule (ii) — it must
    /// not silently fall back to an enclosing crate or module.
    ///
    /// Rule (ii) is a *package* rule and only applies where the resolved
    /// package node is FINER than the file's directory — today that is `.py`
    /// (`pkg:<dotted module>`, GAP-064) and `.go` (`pkg:<import path>`,
    /// GAP-057). For `.rs` files `PkgResolver::pkg_node` returns the
    /// enclosing Cargo CRATE, a node far COARSER than the file: the Rust
    /// parser emits one crate-level `tested_by` edge per integration test
    /// that imports the crate root (`tests/it.rs -> pkg:mylib`), so applying
    /// rule (ii) to Rust would mark every member of the crate covered off a
    /// single crate-root test — trading a false "untested" for a false
    /// "covered", which is strictly worse for a coverage tool. Rust relies
    /// on rule (i) alone: only a file-level `tested_by` edge (e.g.
    /// `tests/lib_test.rs -> src/lib.rs`) covers a Rust file.
    ///
    /// This is GAP-057/064 lineage, not an oversight: the granularity
    /// dispatch below mirrors `PkgResolver::pkg_node`'s own extension
    /// dispatch, which routes `.go` and `.py` to their own walks and
    /// everything else to the Cargo walk.
    fn file_is_covered(
        file: &str,
        tested: &HashSet<String>,
        root: &Path,
        resolver: &mut PkgResolver,
    ) -> bool {
        if tested.contains(file) {
            return true;
        }
        // Rule (ii) applies only to package nodes finer than the file
        // (Python, Go). The Cargo crate node is coarser than a `.rs` file,
        // so a crate-level `tested_by` target must never cover crate
        // members. See the doc comment above.
        if file.ends_with(".rs") {
            return false;
        }
        match resolver.pkg_node(&root.join(file).to_string_lossy()) {
            Some(node) => tested.contains(&node),
            None => false,
        }
    }

    /// Compute comprehensive [`GraphStats`] using DuckDB aggregate queries.
    pub fn stats(&self) -> GraphResult<GraphStats> {
        let total_edges: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM edges", params![], |row| {
                    row.get::<_, i64>(0)
                })?;
        let unique_files: i64 = self.conn.query_row(
            "SELECT COUNT(DISTINCT \"from\") FROM edges",
            params![],
            |row| row.get::<_, i64>(0),
        )?;
        let unique_dependencies: i64 = self.conn.query_row(
            "SELECT COUNT(DISTINCT \"to\") FROM edges",
            params![],
            |row| row.get::<_, i64>(0),
        )?;

        // Top dependencies by reference count. Malformed `pkg:{` pseudo-nodes
        // (legacy garbage from unresolvable multi-name use statements, GAP-035)
        // are excluded — they are not real dependencies (GAP-038).
        let mut stmt = self.conn.prepare(
            "SELECT \"to\", COUNT(*) AS cnt \
             FROM edges \
             WHERE \"to\" NOT LIKE 'pkg:{%' \
             GROUP BY \"to\" \
             ORDER BY cnt DESC, \"to\" ASC \
             LIMIT 10",
        )?;
        let rows = stmt.query_map(params![], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut top = Vec::new();
        for r in rows {
            top.push(r?);
        }
        let most_connected = top.first().map(|(name, _)| name.clone());

        // Edge types: count per relation.
        let mut edge_types = std::collections::HashMap::new();
        let mut rel_stmt = self
            .conn
            .prepare("SELECT rel, COUNT(*) AS cnt FROM edges GROUP BY rel ORDER BY cnt DESC")?;
        let rel_rows = rel_stmt.query_map(params![], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for r in rel_rows {
            let (rel, cnt) = r?;
            edge_types.insert(rel, cnt);
        }

        // Orphans: files that appear as \"from\" but never appear as \"to\"
        // in any edge — truly isolated source files with no incoming references.
        let mut orphan_stmt = self.conn.prepare(
            "SELECT DISTINCT e.\"from\" \
             FROM edges e \
             WHERE e.\"from\" NOT IN (SELECT DISTINCT \"to\" FROM edges) \
             ORDER BY e.\"from\"",
        )?;
        let orphan_rows = orphan_stmt.query_map(params![], |row| row.get::<_, String>(0))?;
        let mut orphans = Vec::new();
        for r in orphan_rows {
            orphans.push(r?);
        }

        Ok(GraphStats {
            total_edges,
            total_files: unique_files,
            unique_files,
            unique_dependencies,
            most_connected,
            orphans,
            edge_types,
            top_dependencies: top,
        })
    }

    // -----------------------------------------------------------------
    // JIT / lazy-parse methods
    // -----------------------------------------------------------------

    /// JIT-parse a single file and cache its edges in DuckDB.
    ///
    /// If the file already has outgoing edges in the graph (as `"from"`),
    /// returns the cached edges immediately without re-parsing. Otherwise
    /// detects the language from the file extension, reads the file from
    /// disk, parses its imports with tree-sitter, and inserts the resulting
    /// edges into the cache.
    ///
    /// Returns empty vec for unsupported extensions or unreadable files.
    pub fn ensure_parsed(&self, file_path: &str) -> GraphResult<Vec<Edge>> {
        // 1. Cache check: return existing outgoing edges if any.
        let existing = self.related(file_path, None, Direction::Forward)?;
        if !existing.is_empty() {
            return Ok(existing);
        }

        // 2. Detect language from extension.
        let path = Path::new(file_path);
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let lang = match Language::from_extension(ext) {
            Some(l) => l,
            None => return Ok(Vec::new()),
        };

        // 3. Read the file from disk.
        let source = match std::fs::read_to_string(file_path) {
            Ok(s) => s,
            Err(_) => return Ok(Vec::new()),
        };

        // 4. Parse imports with tree-sitter.
        let mut parser = Parser::for_language(lang)
            .map_err(|e| GraphError::Other(format!("failed to create parser for {ext}: {e}")))?;
        let edges = parser
            .parse_imports(file_path, &source)
            .map_err(|e| GraphError::Other(format!("parse error in {file_path}: {e}")))?;

        // 5. Insert into DuckDB cache (INSERT OR IGNORE → idempotent).
        if !edges.is_empty() {
            self.insert_edges(&edges)?;
        }

        Ok(edges)
    }

    /// Query edges for a file, falling back to on-the-fly parsing if the
    /// file is not yet in the graph cache.
    pub fn related_or_parse(
        &self,
        path: &str,
        rel_filter: Option<&str>,
        direction: Direction,
    ) -> GraphResult<Vec<Edge>> {
        // PERF-004: same short-circuit as impact_or_parse (see there).
        if !path.starts_with("pkg:") && !path.starts_with("sys:") {
            let not_indexable = Path::new(path)
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| crate::parser::Language::from_extension(e).is_none())
                .unwrap_or(false);
            if not_indexable {
                return Err(GraphError::Other(format!(
                    "'{path}' is not an indexable source file (26 AST languages) — impact/related cannot answer for it"
                )));
            }
        }

        // GAP-059: node-existence check at query time, same contract as
        // `impact_or_parse` (GAP-039). Without it a path that is neither in
        // the graph nor on disk silently produced an empty edge list, which
        // the CLI rendered as "No incoming/outgoing edges" with exit 0 —
        // indistinguishable from a real node with no edges. `impact` already
        // errors on the same input, so `related` must too.
        // Symbol nodes (pkg:/sys:) are legitimately never on disk: they are
        // exempt from the file check and must still answer when in the graph.
        if !path.starts_with("pkg:")
            && !path.starts_with("sys:")
            && !self.file_in_graph(path)?
            && !Path::new(path).exists()
        {
            return Err(GraphError::Other(format!(
                "'{path}' is not in the graph (no such file and no matching graph node)"
            )));
        }

        // Cache hit → query directly.
        if self.file_in_graph(path)? {
            return self.related(path, rel_filter, direction);
        }
        // Cache miss → parse on-the-fly, then query.
        self.ensure_parsed(path)?;
        self.related(path, rel_filter, direction)
    }

    /// Compute transitive impact with lazy parsing of the start file.
    ///
    /// Parses the start file on-the-fly if not cached, then runs BFS over
    /// whatever edges are in the DuckDB cache. When `max_depth` is 0,
    /// returns empty immediately.
    ///
    /// GAP-039: a start path that is neither a known graph node nor a file
    /// on disk is an error (`'<path>' is not in the graph ...`) — the old
    /// behavior silently returned "No dependents found" with exit 0,
    /// indistinguishable from a real node with no dependents.
    pub fn impact_or_parse(
        &self,
        start_path: &str,
        max_depth: u32,
    ) -> GraphResult<Vec<ImpactFile>> {
        // Node-existence check at query time: unknown paths (not in graph,
        // not on disk) must fail loudly instead of looking like a node with
        // zero dependents. Symbol nodes (pkg:/sys:) pass when in the graph.
        // PERF-004: non-indexable extensions (markdown, yaml, json, lock files,
        // ...) can never appear in the AST graph — fail fast with the real
        // reason instead of a misleading "No dependents found". Symbol nodes
        // (pkg:/sys:) and files with no extension are exempt.
        if !start_path.starts_with("pkg:") && !start_path.starts_with("sys:") {
            let not_indexable = Path::new(start_path)
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| crate::parser::Language::from_extension(e).is_none())
                .unwrap_or(false);
            if not_indexable {
                return Err(GraphError::Other(format!(
                    "'{start_path}' is not an indexable source file (26 AST languages) — impact/related cannot answer for it"
                )));
            }
        }
        if !self.file_in_graph(start_path)? && !Path::new(start_path).exists() {
            return Err(GraphError::Other(format!(
                "'{start_path}' is not in the graph (no such file and no matching graph node)"
            )));
        }
        // Parse the start file first (no-op if already cached).
        self.ensure_parsed(start_path)?;
        // Delegate to existing BFS over the DuckDB edges cache.
        impact::compute_impact(&self.conn, start_path, max_depth)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_go_file(dir: &Path, name: &str, content: &str) -> String {
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn perf004_non_indexable_extension_rejected_in_impact() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("notes.md");
        std::fs::write(&p, "# notes\n").unwrap();
        let db = GraphDB::open(":memory:").unwrap();
        let err = db
            .impact_or_parse(p.to_str().unwrap(), 8)
            .expect_err("must error on .md");
        assert!(
            err.to_string().contains("not an indexable source file"),
            "wrong error: {err}"
        );
    }

    #[test]
    fn perf004_non_indexable_extension_rejected_in_related() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("cfg.yaml");
        std::fs::write(&p, "a: 1\n").unwrap();
        let db = GraphDB::open(":memory:").unwrap();
        let err = db
            .related_or_parse(p.to_str().unwrap(), None, Direction::Reverse)
            .expect_err("must error on .yaml");
        assert!(
            err.to_string().contains("not an indexable source file"),
            "wrong error: {err}"
        );
    }

    #[test]
    fn perf004_indexable_extension_not_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("lib.rs");
        std::fs::write(&p, "fn a() {}\n").unwrap();
        let db = GraphDB::open(":memory:").unwrap();
        // must NOT fail with the indexability guard (other errors OK: empty graph etc.)
        if let Err(e) = db.impact_or_parse(p.to_str().unwrap(), 8) {
            assert!(
                !e.to_string().contains("not an indexable source file"),
                "indexable file must not be rejected: {e}"
            );
        }
    }

    #[test]
    fn ensure_parsed_go_file_returns_edges() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_go_file(
            dir.path(),
            "main.go",
            "package main\n\nimport (\n\t\"fmt\"\n\t\"os\"\n)\n",
        );
        let db = GraphDB::open(":memory:").unwrap();
        let edges = db.ensure_parsed(&path).unwrap();
        assert!(
            !edges.is_empty(),
            "Go file with imports should produce edges"
        );
    }

    #[test]
    fn ensure_parsed_caches_and_returns_same_edges() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_go_file(dir.path(), "main.go", "package main\n\nimport \"fmt\"\n");
        let db = GraphDB::open(":memory:").unwrap();

        let edges1 = db.ensure_parsed(&path).unwrap();
        let count1 = edges1.len();

        let edges2 = db.ensure_parsed(&path).unwrap();
        assert_eq!(edges2.len(), count1);

        let total = db.count_edges().unwrap();
        assert_eq!(
            total as usize, count1,
            "edge count must not double on second call"
        );
    }

    #[test]
    fn ensure_parsed_unsupported_extension_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_go_file(dir.path(), "readme.md", "# Hello");
        let db = GraphDB::open(":memory:").unwrap();
        let edges = db.ensure_parsed(&path).unwrap();
        assert!(edges.is_empty());
    }

    #[test]
    fn ensure_parsed_missing_file_returns_empty() {
        let db = GraphDB::open(":memory:").unwrap();
        let edges = db.ensure_parsed("/nonexistent/path/file.go").unwrap();
        assert!(edges.is_empty());
    }

    #[test]
    fn related_or_parse_falls_back_to_parse() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_go_file(dir.path(), "main.go", "package main\n\nimport \"fmt\"\n");
        let db = GraphDB::open(":memory:").unwrap();
        let edges = db
            .related_or_parse(&path, None, Direction::Forward)
            .unwrap();
        assert!(!edges.is_empty(), "should return edges after lazy parse");
    }

    #[test]
    fn impact_or_parse_parses_start_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_go_file(dir.path(), "main.go", "package main\n\nimport \"fmt\"\n");
        let db = GraphDB::open(":memory:").unwrap();
        let _ = db.impact_or_parse(&path, 3).unwrap();
        assert!(
            db.file_in_graph(&path).unwrap(),
            "file should be in graph after impact_or_parse"
        );
    }

    #[test]
    fn impact_or_parse_max_depth_zero_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_go_file(dir.path(), "main.go", "package main\n\nimport \"fmt\"\n");
        let db = GraphDB::open(":memory:").unwrap();
        let results = db.impact_or_parse(&path, 0).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn impact_or_parse_unknown_path_errors_not_empty_silence() {
        // GAP-039: a path absent from both disk and graph must error loudly,
        // not return an empty result (indistinguishable from a real node
        // with no dependents).
        let db = GraphDB::open(":memory:").unwrap();
        let err = db
            .impact_or_parse("/nonexistent/path/unknown.rs", 3)
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("not in the graph"),
            "error should say 'not in the graph', got: {msg}"
        );
        assert!(
            msg.contains("unknown.rs"),
            "error should name the path, got: {msg}"
        );
    }

    #[test]
    fn related_or_parse_unknown_path_errors_not_empty_silence() {
        // GAP-059: `related` carried no node-existence check, so a path
        // absent from both disk and graph produced an empty edge list that
        // the CLI printed as "No incoming/outgoing edges" with exit 0 —
        // indistinguishable from a real node with no edges. It must error
        // exactly like `impact_or_parse` (GAP-039).
        let db = GraphDB::open(":memory:").unwrap();
        let err = db
            .related_or_parse("no/such/file_xyz.rs", None, Direction::Reverse)
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("not in the graph"),
            "error should say 'not in the graph', got: {msg}"
        );
        assert!(
            msg.contains("file_xyz.rs"),
            "error should name the path, got: {msg}"
        );

        // Companion assertion: the guard must not break the happy path. A
        // path that IS a graph node (here not on disk at all) still answers —
        // the check is node-existence, not file-existence.
        let edges = vec![
            Edge::new("producer.go", "consumer.go", "imports"),
            Edge::new("other.go", "pkg:fmt", "imports"),
        ];
        db.insert_edges(&edges).unwrap();
        let ok = db
            .related_or_parse("consumer.go", None, Direction::Reverse)
            .expect("a graph-resident path must still return Ok");
        assert_eq!(
            ok.len(),
            1,
            "consumer.go should have exactly 1 incoming edge, got {ok:?}"
        );
    }

    #[test]
    fn impact_or_parse_symbol_node_in_graph_still_works() {
        // GAP-039: pkg:/sys: symbol nodes that ARE in the graph must keep
        // working (documented query form) — the check is node-existence,
        // not file-existence.
        let db = GraphDB::open(":memory:").unwrap();
        let edges = vec![
            Edge::new("a.go", "pkg:fmt", "imports"),
            Edge::new("b.go", "pkg:fmt", "imports"),
        ];
        db.insert_edges(&edges).unwrap();
        let results = db.impact_or_parse("pkg:fmt", 3).unwrap();
        assert_eq!(results.len(), 2, "pkg:fmt should have 2 dependents");
    }

    // ── Free-function tests: ensure_schema + insert_edges_into ──────────

    #[test]
    fn insert_edges_into_raw_connection_inserts_edges() {
        // A raw connection that never had GraphDB::open called — schema
        // must be auto-ensured by insert_edges_into.
        let conn = Connection::open_in_memory().unwrap();
        let edges = vec![
            Edge::new("a.go", "b.go", "imports"),
            Edge::new("a.go", "c.go", "imports"),
        ];
        insert_edges_into(&conn, &edges).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2, "both edges should be inserted");
    }

    #[test]
    fn insert_edges_into_is_idempotent_on_duplicates() {
        let conn = Connection::open_in_memory().unwrap();
        let edges = vec![Edge::new("a.go", "b.go", "imports")];

        insert_edges_into(&conn, &edges).unwrap();
        insert_edges_into(&conn, &edges).unwrap(); // duplicate

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            count, 1,
            "duplicate insert via INSERT OR IGNORE must not add a row"
        );
    }

    #[test]
    fn insert_edges_into_creates_schema_on_raw_connection() {
        // Verify that the edges table and indexes exist after
        // insert_edges_into on a connection that never had schema init.
        let conn = Connection::open_in_memory().unwrap();
        let edges = vec![Edge::new("x.go", "y.go", "imports")];
        insert_edges_into(&conn, &edges).unwrap();

        // Table exists with all 5 columns.
        let col_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM pragma_table_info('edges')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(col_count, 5, "edges table should have 5 columns");

        // Data is queryable.
        let to_val: String = conn
            .query_row(
                "SELECT \"to\" FROM edges WHERE \"from\" = 'x.go'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(to_val, "y.go");
    }

    #[test]
    fn ensure_schema_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_schema(&conn).unwrap();
        ensure_schema(&conn).unwrap(); // second call must not error

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0, "table should be empty");
    }

    // ── reconcile_edges_from_jsonl tests ──────────────────────────────

    #[test]
    fn reconcile_inserts_edges_from_jsonl_into_raw_connection() {
        let dir = tempfile::tempdir().unwrap();
        let jsonl = dir.path().join("edges.jsonl");
        let edges = [
            Edge::new("main.go", "fmt", "imports"),
            Edge::new("main.go", "os", "imports"),
            Edge::new("util.go", "strings", "imports"),
        ];
        let json_lines: Vec<String> = edges
            .iter()
            .map(|e| serde_json::to_string(e).unwrap())
            .collect();
        std::fs::write(&jsonl, json_lines.join("\n") + "\n").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        let n = reconcile_edges_from_jsonl(&conn, &jsonl).unwrap();
        assert_eq!(n, 3, "should report 3 edges processed");

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 3, "all 3 edges should be in the DB");
    }

    #[test]
    fn reconcile_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let jsonl = dir.path().join("edges.jsonl");
        let edges = [Edge::new("a.go", "b.go", "imports")];
        let json_lines: Vec<String> = edges
            .iter()
            .map(|e| serde_json::to_string(e).unwrap())
            .collect();
        std::fs::write(&jsonl, json_lines.join("\n") + "\n").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        let _ = reconcile_edges_from_jsonl(&conn, &jsonl).unwrap();
        let _ = reconcile_edges_from_jsonl(&conn, &jsonl).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1, "second reconcile must not duplicate rows");
    }

    #[test]
    fn reconcile_writes_stamp_and_open_skips_replay_when_fresh() {
        // PERF-001: after a successful reconcile, a stamp file records the
        // edges.jsonl fingerprint; the next open() must NOT re-replay
        // (observable: the stamp exists, and reconcile_needed() flips false).
        let dir = std::env::temp_dir().join(format!("hilo_perf001_a_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let jsonl = dir.join("edges.jsonl");
        std::fs::write(
            &jsonl,
            "{\"from\":\"a.rs\",\"to\":\"pkg:x\",\"rel\":\"imports\"}\n",
        )
        .unwrap();

        let db_path = dir.join("graph.db");
        {
            let db = GraphDB::open(db_path.to_str().unwrap()).unwrap();
            assert_eq!(db.count_edges().unwrap(), 1);
        }
        // Stamp written on the successful replay.
        let stamp = dir.join(".last_reconcile");
        assert!(stamp.exists(), "stamp must be written after full replay");
        // Fingerprint unchanged -> gate says no reconcile needed.
        assert!(!super::reconcile_needed(&jsonl));

        // Touching edges.jsonl (content change) flips the gate.
        std::fs::write(
            &jsonl,
            "{\"from\":\"a.rs\",\"to\":\"pkg:x\",\"rel\":\"imports\"}\n{\"from\":\"b.rs\",\"to\":\"pkg:y\",\"rel\":\"imports\"}\n",
        )
        .unwrap();
        assert!(
            super::reconcile_needed(&jsonl),
            "changed jsonl must invalidate stamp"
        );

        // Reopen: reconcile runs, new edge visible, stamp refreshed.
        {
            let db = GraphDB::open(db_path.to_str().unwrap()).unwrap();
            assert_eq!(db.count_edges().unwrap(), 2);
        }
        assert!(!super::reconcile_needed(&jsonl));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_stamp_triggers_reconcile_for_legacy_caches() {
        // Legacy cache (pre-PERF-001) has no stamp -> must still reconcile.
        let dir = std::env::temp_dir().join(format!("hilo_perf001_b_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let jsonl = dir.join("edges.jsonl");
        std::fs::write(
            &jsonl,
            "{\"from\":\"a.rs\",\"to\":\"pkg:x\",\"rel\":\"imports\"}\n",
        )
        .unwrap();
        assert!(super::reconcile_needed(&jsonl), "no stamp -> reconcile");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reconcile_missing_file_returns_zero() {
        let conn = Connection::open_in_memory().unwrap();
        let missing = Path::new("/nonexistent/path/edges.jsonl");
        let result = reconcile_edges_from_jsonl(&conn, missing).unwrap();
        assert_eq!(result, 0, "missing file should return Ok(0)");
    }

    #[test]
    fn reconcile_skips_malformed_lines() {
        let dir = tempfile::tempdir().unwrap();
        let jsonl = dir.path().join("edges.jsonl");
        // Mix valid and malformed lines.
        let valid1 = serde_json::to_string(&Edge::new("a.go", "b.go", "imports")).unwrap();
        let valid2 = serde_json::to_string(&Edge::new("c.go", "d.go", "imports")).unwrap();
        let content =
            format!("{valid1}\n{{\"this is\": \"broken\"}}\n   \nnot json at all\n{valid2}\n");
        std::fs::write(&jsonl, content).unwrap();

        let conn = Connection::open_in_memory().unwrap();
        let n = reconcile_edges_from_jsonl(&conn, &jsonl).unwrap();
        assert_eq!(
            n, 2,
            "should process 2 valid edges, skip 2 malformed + 1 blank"
        );

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2, "only 2 valid edges should be in the DB");
    }

    #[test]
    fn drift_scenario_open_graphdb_sees_directly_appended_edge() {
        // Simulate drift: edge A is inserted via the write-through path
        // (insert_edges_into), edge B is appended directly to edges.jsonl
        // (bypassing write-through). Opening GraphDB on the same graph.db
        // should reconcile B from edges.jsonl and make it queryable.
        let dir = tempfile::tempdir().unwrap();
        let graph_dir = dir.path().join("graph");
        std::fs::create_dir_all(&graph_dir).unwrap();
        let db_path = graph_dir.join("graph.db");
        let jsonl_path = graph_dir.join("edges.jsonl");

        // 1. Insert edge A via write-through (simulating JIT-001 path).
        {
            let conn = Connection::open(&db_path).unwrap();
            let edge_a = Edge::new("main.go", "fmt", "imports");
            insert_edges_into(&conn, &[edge_a]).unwrap();
        }

        // 2. Append edge B directly to edges.jsonl (simulating drift —
        //    a write path that bypasses the DuckDB cache).
        let edge_b = Edge::new("main.go", "os", "imports");
        let json_line = serde_json::to_string(&edge_b).unwrap();
        std::fs::write(&jsonl_path, json_line + "\n").unwrap();

        // 3. Open GraphDB — read-through reconcile should load edge B.
        let db_path_str = db_path.to_str().unwrap();
        let db = GraphDB::open(db_path_str).unwrap();

        // 4. Query: edge B should be visible (reconciled from edges.jsonl).
        let related = db.related("main.go", None, Direction::Forward).unwrap();
        let tos: Vec<&str> = related.iter().map(|e| e.to.as_str()).collect();
        assert!(
            tos.contains(&"os"),
            "drift edge 'os' should be visible after reconcile, got: {tos:?}"
        );
        assert!(
            tos.contains(&"fmt"),
            "write-through edge 'fmt' should still be present, got: {tos:?}"
        );

        // 5. Idempotent: opening again should not duplicate.
        let db2 = GraphDB::open(db_path_str).unwrap();
        let count = db2.count_edges().unwrap();
        assert_eq!(count, 2, "re-open must not duplicate edges");
    }

    #[test]
    fn untested_files_excludes_test_and_bench_files() {
        // GAP-036: test/bench files have `imports` edges (they import the
        // crate under test) but are never the *target* of a `tested_by`
        // edge, so an unfiltered query lists every test file as untested.
        let db = GraphDB::open(":memory:").unwrap();
        let edges = vec![
            // Production file with imports but no tests -> genuinely untested.
            Edge::new("src/util.rs", "pkg:std", "imports"),
            // Production file imported by a test -> covered.
            Edge::new("src/lib.rs", "pkg:std", "imports"),
            Edge::new("tests/lib_test.rs", "src/lib.rs", "imports"),
            Edge::new("tests/lib_test.rs", "src/lib.rs", "tested_by"),
            // Bench file with an imports edge -> must not appear.
            Edge::new("benches/graph_bench.rs", "src/lib.rs", "imports"),
            // Nested crate-level test file -> must not appear.
            Edge::new(
                "crates/globset/tests/matcher_test.rs",
                "pkg:globset",
                "imports",
            ),
        ];
        db.insert_edges(&edges).unwrap();

        let untested = db.untested_files().unwrap();
        assert!(
            untested.contains(&"src/util.rs".to_string()),
            "genuinely untested production file must be listed, got: {untested:?}"
        );
        assert!(
            !untested.contains(&"src/lib.rs".to_string()),
            "tested file must not be listed, got: {untested:?}"
        );
        assert!(
            !untested.contains(&"tests/lib_test.rs".to_string()),
            "test file must be excluded from untested, got: {untested:?}"
        );
        assert!(
            !untested.contains(&"benches/graph_bench.rs".to_string()),
            "bench file must be excluded from untested, got: {untested:?}"
        );
        assert!(
            !untested.contains(&"crates/globset/tests/matcher_test.rs".to_string()),
            "nested test file must be excluded from untested, got: {untested:?}"
        );
        assert_eq!(
            untested.len(),
            1,
            "only src/util.rs should remain, got: {untested:?}"
        );
    }

    // -----------------------------------------------------------------------
    // GAP-066 — coverage consumers resolve `pkg:` nodes
    // -----------------------------------------------------------------------

    /// Create `dir/<rel>` (with parents) and return its path.
    fn touch(dir: &Path, rel: &str) -> PathBuf {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, "").unwrap();
        path
    }

    /// A tempdir shaped like the GAP-064 reproduction corpus: a Python
    /// package with a nested subpackage, a second package, a test file and a
    /// standalone script (no `__init__.py`).
    fn python_corpus() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for rel in [
            "fastapi/__init__.py",
            "fastapi/routing.py",
            "fastapi/dependencies/__init__.py",
            "fastapi/dependencies/utils.py",
            "app/__init__.py",
            "app/main.py",
            "tests/test_routing.py",
            "standalone.py",
        ] {
            touch(dir.path(), rel);
        }
        dir
    }

    #[test]
    fn untested_files_at_resolves_pkg_covered_files() {
        // GAP-066: the parser emits `tested_by` edges to `pkg:` nodes, so a
        // covered Python file is never the literal target of its test — the
        // file-level query alone lists it as untested.
        let dir = python_corpus();
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[
            Edge::new(
                "fastapi/routing.py",
                "pkg:fastapi.dependencies.utils",
                "imports",
            ),
            Edge::new("app/main.py", "pkg:fastapi.routing", "imports"),
            Edge::new("fastapi/dependencies/utils.py", "pkg:os", "imports"),
            Edge::new("standalone.py", "pkg:os", "imports"),
            Edge::new("tests/test_routing.py", "pkg:fastapi.routing", "imports"),
            Edge::new("tests/test_routing.py", "pkg:fastapi.routing", "tested_by"),
            Edge::new(
                "tests/test_routing.py",
                "pkg:fastapi.dependencies",
                "tested_by",
            ),
        ])
        .unwrap();

        let untested = db.untested_files_at(dir.path()).unwrap();

        assert!(
            !untested.contains(&"fastapi/routing.py".to_string()),
            "file whose pkg node is a tested_by target must be covered, got: {untested:?}"
        );
        assert!(
            untested.contains(&"app/main.py".to_string()),
            "uncovered importer (non-matching pkg node) must stay listed, got: {untested:?}"
        );
        assert!(
            untested.contains(&"standalone.py".to_string()),
            "standalone script resolves to no package and must stay listed, got: {untested:?}"
        );
        // Package-ANCESTOR coverage is out of scope: the test targets
        // `pkg:fastapi.dependencies`, not the nested module node.
        assert!(
            untested.contains(&"fastapi/dependencies/utils.py".to_string()),
            "ancestor-only coverage must not mark a file covered, got: {untested:?}"
        );
        assert!(
            !untested.contains(&"tests/test_routing.py".to_string()),
            "test file must not be listed as untested production code, got: {untested:?}"
        );
        assert_eq!(
            untested,
            vec![
                "app/main.py".to_string(),
                "fastapi/dependencies/utils.py".to_string(),
                "standalone.py".to_string(),
            ],
            "exactly the three uncovered production files should remain"
        );
    }

    #[test]
    fn module_files_at_counts_pkg_resolved_coverage() {
        // GAP-066: `count(DISTINCT to) WHERE to LIKE 'fastapi/%'` can never
        // match `pkg:fastapi.routing`, so module coverage read 0.0% for a
        // fully covered module.
        let dir = python_corpus();
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[
            Edge::new(
                "fastapi/routing.py",
                "pkg:fastapi.dependencies.utils",
                "imports",
            ),
            Edge::new("fastapi/dependencies/utils.py", "pkg:os", "imports"),
            Edge::new("app/main.py", "pkg:fastapi.routing", "imports"),
            Edge::new("tests/test_corpus.py", "pkg:fastapi.routing", "imports"),
            Edge::new("tests/test_corpus.py", "pkg:fastapi.routing", "tested_by"),
            Edge::new(
                "tests/test_corpus.py",
                "pkg:fastapi.dependencies.utils",
                "tested_by",
            ),
        ])
        .unwrap();

        let stats = db.module_files_at(dir.path(), "fastapi").unwrap();
        assert_eq!(
            stats.files,
            vec![
                "fastapi/dependencies/utils.py".to_string(),
                "fastapi/routing.py".to_string()
            ],
            "file list must stay the file-level view of the module"
        );
        assert_eq!(stats.edges_count, 2, "edge count must be unchanged");
        assert_eq!(
            stats.test_coverage_pct, 100.0,
            "both fastapi files are covered through their pkg nodes"
        );

        // A module whose files have no test reads 0.0%.
        let app = db.module_files_at(dir.path(), "app").unwrap();
        assert_eq!(app.files, vec!["app/main.py".to_string()]);
        assert_eq!(app.edges_count, 1);
        assert_eq!(app.test_coverage_pct, 0.0);

        // An uncovered sibling still drags the percentage down, rounded to
        // one decimal.
        touch(dir.path(), "fastapi/legacy.py");
        db.insert_edges(&[Edge::new("fastapi/legacy.py", "pkg:os", "imports")])
            .unwrap();
        let stats = db.module_files_at(dir.path(), "fastapi").unwrap();
        assert_eq!(stats.files.len(), 3);
        assert_eq!(stats.test_coverage_pct, 66.7);
    }

    #[test]
    fn untested_files_at_keeps_file_level_tested_by_coverage() {
        // Rule (i) must survive GAP-066: a file-level `tested_by` target is
        // still covered even though the resolver maps the file to its crate
        // node (`pkg:demo`) rather than its own path.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\n",
        )
        .unwrap();
        touch(dir.path(), "src/lib.rs");
        touch(dir.path(), "src/util.rs");
        touch(dir.path(), "tests/lib_test.rs");

        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[
            Edge::new("src/lib.rs", "pkg:std", "imports"),
            Edge::new("src/util.rs", "pkg:std", "imports"),
            Edge::new("tests/lib_test.rs", "src/lib.rs", "imports"),
            Edge::new("tests/lib_test.rs", "src/lib.rs", "tested_by"),
        ])
        .unwrap();

        let untested = db.untested_files_at(dir.path()).unwrap();
        assert!(
            !untested.contains(&"src/lib.rs".to_string()),
            "literal tested_by target must stay covered, got: {untested:?}"
        );
        assert!(
            untested.contains(&"src/util.rs".to_string()),
            "Rust sibling with no test must stay listed, got: {untested:?}"
        );
        assert_eq!(untested, vec!["src/util.rs".to_string()]);
    }

    #[test]
    fn untested_files_at_does_not_fall_back_to_crate_for_standalone_python() {
        // A standalone `.py` has no package node. If the Python branch fell
        // through to the Cargo walk, `standalone.py` would resolve to
        // `pkg:demo` and be swallowed by the crate-level `tested_by` edge.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\n",
        )
        .unwrap();
        touch(dir.path(), "standalone.py");
        touch(dir.path(), "tests/demo_test.rs");

        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[
            Edge::new("standalone.py", "pkg:os", "imports"),
            Edge::new("tests/demo_test.rs", "pkg:demo", "imports"),
            Edge::new("tests/demo_test.rs", "pkg:demo", "tested_by"),
        ])
        .unwrap();

        let untested = db.untested_files_at(dir.path()).unwrap();
        assert_eq!(
            untested,
            vec!["standalone.py".to_string()],
            "unresolvable Python file must stay uncovered, never fall back to the crate node"
        );
    }

    /// A tempdir shaped like a minimal Rust crate with an integration test
    /// that imports only the crate root (`mylib`), mirroring the GAP-066
    /// rework reproduction: module `a` is reachable from the test, modules
    /// `b` and `c` are not tested at all.
    fn rust_crate_corpus() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"mylib\"\n",
        )
        .unwrap();
        // `touch` creates parents; write `lib.rs` after so `src/` exists.
        touch(dir.path(), "src/a.rs");
        touch(dir.path(), "src/b.rs");
        touch(dir.path(), "src/c.rs");
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub mod a;\npub mod b;\npub mod c;\n",
        )
        .unwrap();
        touch(dir.path(), "tests/it.rs");
        dir
    }

    #[test]
    fn untested_files_at_ignores_crate_level_tested_by_for_rust_members() {
        // GAP-066 rework: `PkgResolver::pkg_node` returns the Cargo CRATE for
        // a `.rs` file, so rule (ii) would turn one crate-root integration
        // test into coverage for every module in `src/`. A crate-level
        // `tested_by` target must never cover a crate member file.
        let dir = rust_crate_corpus();
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[
            Edge::new("src/a.rs", "src/b.rs", "imports"),
            Edge::new("src/b.rs", "src/c.rs", "imports"),
            Edge::new("tests/it.rs", "pkg:mylib", "imports"),
            Edge::new("tests/it.rs", "pkg:mylib", "tested_by"),
        ])
        .unwrap();

        let untested = db.untested_files_at(dir.path()).unwrap();
        assert_eq!(
            untested,
            vec!["src/a.rs".to_string(), "src/b.rs".to_string()],
            "a crate-level tested_by edge must not cover crate member files"
        );

        let stats = db.module_files_at(dir.path(), "src").unwrap();
        assert_eq!(
            stats.files,
            vec![
                "src/a.rs".to_string(),
                "src/b.rs".to_string(),
                "src/c.rs".to_string()
            ],
            "file list must stay the file-level view of the module"
        );
        assert_eq!(stats.edges_count, 2, "edge count must be unchanged");
        assert_eq!(
            stats.test_coverage_pct, 0.0,
            "the crate-root test covers no `src/` file by itself"
        );
    }

    #[test]
    fn untested_files_at_keeps_file_level_tested_by_for_rust_members() {
        // Rule (i) stays intact for Rust: a FILE-level `tested_by` edge (an
        // integration test naming one module) still covers that file, while
        // its siblings stay uncovered.
        let dir = rust_crate_corpus();
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[
            Edge::new("src/a.rs", "src/b.rs", "imports"),
            Edge::new("src/b.rs", "src/c.rs", "imports"),
            Edge::new("src/lib.rs", "src/a.rs", "imports"),
            Edge::new("tests/it.rs", "pkg:mylib", "imports"),
            Edge::new("tests/it.rs", "pkg:mylib", "tested_by"),
            Edge::new("tests/lib_test.rs", "src/lib.rs", "imports"),
            Edge::new("tests/lib_test.rs", "src/lib.rs", "tested_by"),
        ])
        .unwrap();

        let untested = db.untested_files_at(dir.path()).unwrap();
        assert!(
            !untested.contains(&"src/lib.rs".to_string()),
            "literal file-level tested_by target must stay covered, got: {untested:?}"
        );
        assert_eq!(
            untested,
            vec!["src/a.rs".to_string(), "src/b.rs".to_string()],
            "only the file-level edge counts; crate-root test covers nothing else"
        );

        let stats = db.module_files_at(dir.path(), "src").unwrap();
        assert_eq!(stats.files.len(), 4);
        assert_eq!(
            stats.test_coverage_pct, 25.0,
            "1 of 4 `src/` files covered by the file-level edge only"
        );
    }
}
