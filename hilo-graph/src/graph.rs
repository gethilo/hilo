//! DuckDB graph initialization and edge querying.
//!
//! Creates and manages the `.vfs/graph/graph.db` database for graph edge
//! storage and querying.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use duckdb::{params, Connection};
use hilo_core::manifest::{DuckDbPerf, Manifest};
use hilo_metadata::inventory::Edge;

use crate::error::{GraphError, GraphResult};
use crate::impact::{self, ImpactFile};
use crate::parser::{Language, Parser};
use crate::resolution::{LocalSpecResolver, PkgResolver};

/// The TS/JS extensions rule (iii) applies to, derived from
/// [`Language::from_extension`] (the parser's own TS/JS entries) plus `.mjs`
/// and `.cjs`, which the extension-probing list in `resolution.rs` treats
/// as first-class JS (`classify_js` emits `local:` nodes for them the same
/// way; `Language::from_extension` folds them under JavaScript's `.js`/
/// `.jsx` entries). Kept as one function so the predicate and the parser's
/// language table cannot drift apart silently.
fn is_js_like_file(path: &str) -> bool {
    let Some(ext) = path.rsplit('.').next() else {
        return false;
    };
    if ext == path {
        return false;
    }
    matches!(ext, "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs")
}

/// Strip a leading `file:` id prefix (DF-WARPFS-2): `file:src/main.rs`
/// resolves exactly like the bare repo-relative path. One strip only, so
/// `file:file:x` still targets a path literally named `file:x`; `pkg:` and
/// `sys:` id forms (and everything else) pass through untouched. Shared by
/// the graph entry points so MCP and FFI callers get the same
/// normalization as the CLI.
pub fn strip_file_prefix(path: &str) -> &str {
    path.strip_prefix("file:").unwrap_or(path)
}

/// The unresolvable-target error message shared by `impact_or_parse`,
/// `related_or_parse`, and the CLI's external-impact bail. Keeps the stable
/// "is not in the graph" substring (asserted across the CLI/MCP test
/// suites) and, since DF-WARPFS-2, teaches the accepted id forms so an
/// agent can self-correct (e.g. drop a `file:` prefix) instead of reading
/// the message as plain file-not-found.
pub const UNRESOLVABLE_TARGET_HINT: &str =
    "Accepted id forms: bare repo-relative path (src/main.rs), sys:<header>, pkg:<crate>";

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
    /// True when replay was skipped to preserve the configured memory ceiling.
    degraded: bool,
    /// Canonical edge source used by the degraded streaming query path.
    edges_jsonl: Option<PathBuf>,
    /// Effective byte ceiling that caused replay to be skipped.
    spill_watermark: Option<u64>,
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

/// Default number of edges committed per replay/insert transaction.
///
/// DuckDB retains unique-index and payload deltas until commit, so bounding
/// each transaction prevents cold-cache replay memory from growing with the
/// full `edges.jsonl` corpus.
pub const RECONCILE_CHUNK_ROWS: usize = 2_048;

const INSERT_EDGE_SQL: &str =
    "INSERT OR IGNORE INTO edges (\"from\", \"to\", rel, provenance, confidence) VALUES (?, ?, ?, ?, ?)";

/// Insert owned edges in bounded transactions, returning the number of rows
/// executed (including duplicates ignored by `INSERT OR IGNORE`).
fn insert_edges_in_chunks<I>(conn: &Connection, edges: I, chunk_size: usize) -> GraphResult<usize>
where
    I: IntoIterator<Item = Edge>,
{
    assert!(chunk_size > 0, "chunk_size must be positive");
    let mut edges = edges.into_iter();
    let mut processed = 0;

    loop {
        let chunk: Vec<Edge> = edges.by_ref().take(chunk_size).collect();
        if chunk.is_empty() {
            return Ok(processed);
        }

        conn.execute_batch("BEGIN TRANSACTION")?;
        let insert_result = (|| -> GraphResult<()> {
            let mut stmt = conn.prepare(INSERT_EDGE_SQL)?;
            for edge in &chunk {
                stmt.execute(params![
                    edge.from,
                    edge.to,
                    edge.rel,
                    edge.provenance,
                    edge.confidence
                ])?;
            }
            Ok(())
        })();

        if let Err(error) = insert_result {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(error);
        }
        if let Err(error) = conn.execute_batch("COMMIT") {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(error.into());
        }
        processed += chunk.len();
    }
}

/// Insert edges into a raw DuckDB connection (INSERT OR IGNORE, idempotent).
///
/// Ensures the schema exists first (idempotent `CREATE TABLE IF NOT EXISTS`
/// and indexes), then inserts each edge with `INSERT OR IGNORE`. Safe to call
/// on connections opened via plain `duckdb::Connection::open` (without
/// `GraphDB::open`).
pub fn insert_edges_into(conn: &Connection, edges: &[Edge]) -> GraphResult<()> {
    ensure_schema(conn)?;
    // PERF-002: prepared inserts committed in bounded chunks avoid both the
    // old ~1ms-per-edge autocommit cost and an unbounded transaction delta.
    insert_edges_in_chunks(conn, edges.iter().cloned(), RECONCILE_CHUNK_ROWS)?;
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
/// old-format lines), and commits prepared inserts in bounded
/// [`RECONCILE_CHUNK_ROWS`] transactions. Malformed lines are silently skipped
/// — a corrupt line does not abort the whole reconcile.
///
/// Returns the number of edges **successfully parsed and inserted** (including
/// duplicates that were ignored by `INSERT OR IGNORE`). This is the count of
/// lines processed, not the count of *new* rows added.
///
/// - Missing file → `Ok(0)` (no-op, fresh project — not an error).
/// - Idempotent: calling twice inserts the same edges, `INSERT OR IGNORE` +
///   unique index ensures no duplicates.
pub fn reconcile_edges_from_jsonl(conn: &Connection, edges_jsonl: &Path) -> GraphResult<usize> {
    reconcile_edges_from_jsonl_with_chunk_size(conn, edges_jsonl, RECONCILE_CHUNK_ROWS)
}

/// [`reconcile_edges_from_jsonl`] with an explicit transaction size.
///
/// Identical in every other respect — same parse, same `INSERT OR IGNORE`,
/// same stamp-after-success rule — so this is the seam that prices the
/// throughput/peak-memory trade [`RECONCILE_CHUNK_ROWS`] makes. The shipped
/// default is deliberately NOT parameterised by env or config: a chunk size
/// that varies by environment makes replay memory untestable. Callers that
/// need a different size (benchmarks, the PERF-006 sweep) pass it explicitly.
pub fn reconcile_edges_from_jsonl_with_chunk_size(
    conn: &Connection,
    edges_jsonl: &Path,
    chunk_size: usize,
) -> GraphResult<usize> {
    if !edges_jsonl.exists() {
        return Ok(0);
    }

    let file = std::fs::File::open(edges_jsonl)?;
    let reader = BufReader::new(file);
    let edges = reader.lines().filter_map(|line| {
        let line = line.ok()?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return None;
        }
        serde_json::from_str::<Edge>(trimmed).ok()
    });

    // PERF-001/GAP-092: prepared inserts retain the old replay throughput,
    // while one commit per bounded chunk prevents transaction/index memory
    // from scaling with the entire file when the stamp gate misses.
    ensure_schema(conn)?;
    let processed = insert_edges_in_chunks(conn, edges, chunk_size)?;

    // Stamp AFTER a successful full replay so the next open() can trust the
    // cache without touching edges.jsonl. A failed chunk returns above.
    if let Some(fp) = jsonl_fingerprint(edges_jsonl) {
        let _ = std::fs::write(reconcile_stamp_path(edges_jsonl), fp);
    }
    Ok(processed)
}

fn duckdb_perf_for_path(db_path: &Path) -> DuckDbPerf {
    let manifest_path = db_path
        .parent()
        .and_then(Path::parent)
        .map(|vfs_dir| vfs_dir.join("manifest.yaml"));

    let Some(path) = manifest_path else {
        return DuckDbPerf::default();
    };
    if !path.exists() {
        return DuckDbPerf::default();
    }
    match Manifest::from_file(&path.to_string_lossy()) {
        Ok(manifest) => manifest.performance.duckdb,
        Err(error) => {
            eprintln!(
                "warning: failed to parse {} ({error}); using default DuckDB performance settings",
                path.display()
            );
            DuckDbPerf::default()
        }
    }
}

/// Parse the byte-size syntax accepted by the DuckDB memory settings.
fn parse_byte_size(value: &str) -> Option<u64> {
    let compact = value.trim().replace(' ', "").to_ascii_uppercase();
    let number_end = compact
        .find(|ch: char| !ch.is_ascii_digit() && ch != '.')
        .unwrap_or(compact.len());
    if number_end == 0 {
        return None;
    }
    let amount: f64 = compact[..number_end].parse().ok()?;
    let multiplier = match &compact[number_end..] {
        "" | "B" => 1_u64,
        "KB" => 1_000,
        "KIB" => 1_024,
        "MB" => 1_000_000,
        "MIB" => 1_048_576,
        "GB" => 1_000_000_000,
        "GIB" => 1_073_741_824,
        "TB" => 1_000_000_000_000,
        "TIB" => 1_099_511_627_776,
        _ => return None,
    };
    let bytes = amount * multiplier as f64;
    (amount.is_finite() && amount > 0.0 && bytes <= u64::MAX as f64).then_some(bytes as u64)
}

/// Resolve malformed values before applying them, and derive the JSONL spill
/// watermark. An omitted or malformed watermark falls back to memory_limit.
fn resolved_duckdb_perf(mut perf: DuckDbPerf) -> (DuckDbPerf, u64) {
    let default = DuckDbPerf::default();
    let memory_bytes = match parse_byte_size(&perf.memory_limit) {
        Some(bytes) => bytes,
        None => {
            eprintln!(
                "warning: invalid DuckDB memory_limit '{}'; using default {}",
                perf.memory_limit, default.memory_limit
            );
            perf.memory_limit = default.memory_limit;
            parse_byte_size(&perf.memory_limit).expect("default DuckDB memory limit must be valid")
        }
    };

    let spill_watermark = match perf.spill_watermark.clone() {
        None => memory_bytes,
        Some(value) => match parse_byte_size(&value) {
            Some(bytes) => bytes,
            None => {
                eprintln!(
                    "warning: invalid DuckDB spill_watermark '{value}'; using memory_limit {}",
                    perf.memory_limit
                );
                perf.spill_watermark = None;
                memory_bytes
            }
        },
    };
    (perf, spill_watermark)
}

fn apply_duckdb_setting(conn: &Connection, name: &str, sql: &str) {
    if let Err(error) = conn.execute_batch(sql) {
        eprintln!(
            "warning: failed to apply DuckDB {name}: {}",
            error.to_string().replace('\n', " ")
        );
    }
}

fn configure_disk_connection(conn: &Connection, perf: &DuckDbPerf) {
    let memory_limit = perf.memory_limit.replace('\'', "''");
    apply_duckdb_setting(
        conn,
        "memory_limit",
        &format!("SET memory_limit='{memory_limit}'"),
    );
    apply_duckdb_setting(conn, "threads", &format!("SET threads={}", perf.threads));
    apply_duckdb_setting(
        conn,
        "preserve_insertion_order",
        "SET preserve_insertion_order=false",
    );
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
        let mut disk_perf = None;
        let conn = if path == ":memory:" {
            Connection::open_in_memory()?
        } else {
            let (perf, spill_watermark) =
                resolved_duckdb_perf(duckdb_perf_for_path(Path::new(path)));
            let conn = Connection::open(path)?;
            configure_disk_connection(&conn, &perf);
            disk_perf = Some((perf, spill_watermark));
            conn
        };
        ensure_schema(&conn)?;

        let mut degraded = false;
        let mut edges_jsonl = None;
        let mut degraded_spill_watermark = None;
        // Read-through reconciliation: if a sibling edges.jsonl exists, load
        // any edges missing from the DuckDB cache. Only for on-disk DBs —
        // ":memory:" connections have no sibling file and are used in tests.
        if let Some((perf, spill_watermark)) = disk_perf {
            let jsonl_path = Path::new(path).parent().map(|dir| dir.join("edges.jsonl"));
            if let Some(jsonl) = jsonl_path {
                edges_jsonl = Some(jsonl.clone());
                let jsonl_size = std::fs::metadata(&jsonl)
                    .ok()
                    .map(|metadata| metadata.len());
                if jsonl_size.is_some_and(|size| size >= spill_watermark) {
                    degraded = true;
                    degraded_spill_watermark = Some(spill_watermark);
                    eprintln!(
                        "warning: edges.jsonl is {} bytes, at/above spill_watermark {} bytes (DuckDB memory_limit={}); skipping full replay and serving graph queries by streaming edges.jsonl",
                        jsonl_size.unwrap_or_default(),
                        spill_watermark,
                        perf.memory_limit
                    );
                } else {
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
        }

        Ok(GraphDB {
            conn,
            degraded,
            edges_jsonl,
            spill_watermark: degraded_spill_watermark,
        })
    }

    /// Whether this disk-backed connection skipped replay and serves graph
    /// queries by streaming the canonical `edges.jsonl` file.
    pub fn is_degraded(&self) -> bool {
        self.degraded
    }

    fn require_cached_query(&self, api: &str) -> GraphResult<()> {
        if !self.degraded {
            return Ok(());
        }
        Err(GraphError::Other(format!(
            "graph query unavailable in degraded (spill) mode: {api}; edges.jsonl exceeds spill_watermark {} bytes",
            self.spill_watermark.unwrap_or_default()
        )))
    }

    /// Build only the `local:` resolver index needed to preserve impact parity.
    /// The canonical corpus is still scanned; non-local rows are never replayed
    /// into the skipped cache.
    fn local_resolver_from_jsonl(&self) -> GraphResult<Option<LocalSpecResolver>> {
        let conn = Connection::open_in_memory()?;
        ensure_schema(&conn)?;
        let mut stmt = conn.prepare(INSERT_EDGE_SQL)?;
        let mut found = false;
        let mut insert_error = None;
        self.scan_jsonl_edges(|edge| {
            if edge.to.starts_with("local:") {
                found = true;
                if let Err(error) = stmt.execute(params![
                    edge.from,
                    edge.to,
                    edge.rel,
                    edge.provenance,
                    edge.confidence
                ]) {
                    insert_error = Some(error);
                    return false;
                }
            }
            true
        })?;
        if let Some(error) = insert_error {
            return Err(error.into());
        }
        drop(stmt);
        if found {
            Ok(Some(LocalSpecResolver::from_edges(&conn)?))
        } else {
            Ok(None)
        }
    }

    /// Scan valid canonical edges without materialising the corpus. Returning
    /// `false` from `visit` stops the scan early.
    fn scan_jsonl_edges(&self, mut visit: impl FnMut(Edge) -> bool) -> GraphResult<()> {
        let path = self.edges_jsonl.as_deref().ok_or_else(|| {
            GraphError::Other("degraded graph has no edges.jsonl path".to_string())
        })?;
        let reader = BufReader::new(std::fs::File::open(path)?);
        for line in reader.lines() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Ok(edge) = serde_json::from_str::<Edge>(trimmed) else {
                continue;
            };
            if !visit(edge) {
                break;
            }
        }
        Ok(())
    }

    /// Insert multiple edges into the database using a prepared statement.
    ///
    /// Delegates to [`insert_edges_into`] (the free function) so the INSERT
    /// SQL is defined in exactly one place.
    pub fn insert_edges(&self, edges: &[Edge]) -> GraphResult<()> {
        insert_edges_into(&self.conn, edges)
    }

    /// Return the total number of unique graph edges.
    pub fn count_edges(&self) -> GraphResult<i64> {
        if self.degraded {
            let mut seen = HashSet::new();
            self.scan_jsonl_edges(|edge| {
                seen.insert((edge.from, edge.to, edge.rel, edge.provenance));
                true
            })?;
            return Ok(seen.len() as i64);
        }

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
        self.require_cached_query("group_by_dependency")?;
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
        self.require_cached_query("distinct_files")?;
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

    /// The `(from, to, rel)` triples of every edge whose `rel` is in `rels`,
    /// sorted by `(from, to, rel)` for determinism (GAP-081-P3: the
    /// edge-aware search pass reads exactly the service-dimension edges).
    ///
    /// A row dedupes on all three columns: the same pair carried at two
    /// provenances (`grpc_ast` + `grpc_proto` contract edges) is one search
    /// fact, and the same file pair serving two services is two.
    pub fn distinct_service_edges(
        &self,
        rels: &[&str],
    ) -> GraphResult<Vec<(String, String, String)>> {
        self.require_cached_query("distinct_service_edges")?;
        let placeholders = rels.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
        let sql = format!(
            "SELECT DISTINCT \"from\", \"to\", rel FROM edges WHERE rel IN ({placeholders}) \
             ORDER BY \"from\", \"to\", rel"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let param_refs: Vec<&dyn duckdb::ToSql> =
            rels.iter().map(|r| r as &dyn duckdb::ToSql).collect();
        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Query the canonical JSONL source in degraded mode. Only matching rows
    /// are retained, so memory scales with the answer rather than the corpus.
    fn related_from_jsonl(
        &self,
        path: &str,
        rel_filter: Option<&str>,
        direction: Direction,
    ) -> GraphResult<Vec<Edge>> {
        let mut targets = vec![path.to_string()];
        if direction == Direction::Reverse {
            if let Some(pkg) = PkgResolver::new().pkg_node(path) {
                targets.push(pkg);
            }
        }

        let mut edges = Vec::new();
        let mut seen = HashSet::new();
        self.scan_jsonl_edges(|edge| {
            let endpoint = match direction {
                Direction::Forward => &edge.from,
                Direction::Reverse => &edge.to,
            };
            let matches = targets.iter().any(|target| target == endpoint)
                && rel_filter.is_none_or(|rel| rel == edge.rel);
            if matches {
                let key = (
                    edge.from.clone(),
                    edge.to.clone(),
                    edge.rel.clone(),
                    edge.provenance.clone(),
                );
                if seen.insert(key) {
                    edges.push(edge);
                }
            }
            true
        })?;
        Ok(edges)
    }

    /// Query edges for a file path, optionally filtered by relation type and
    /// direction.
    ///
    /// - `Forward` (default): `WHERE "from" = ?` — outgoing edges.
    /// - `Reverse`: `WHERE "to" = ?` — incoming edges (e.g. `imported_by`).
    ///
    /// GAP-083: each returned `Edge` keeps its own `to`, which is what lets the
    /// caller separate a file-level hit (`edge.to == path`) from a crate-level
    /// hit (`edge.to == pkg:<crate>`, added by the GAP-034 target below).
    ///
    /// Returns an empty `Vec` if no edges match.
    pub fn related(
        &self,
        path: &str,
        rel_filter: Option<&str>,
        direction: Direction,
    ) -> GraphResult<Vec<Edge>> {
        if self.degraded {
            return self.related_from_jsonl(path, rel_filter, direction);
        }

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

    /// Check whether a file path exists in the graph (as `from` or `to`).
    pub fn file_in_graph(&self, path: &str) -> GraphResult<bool> {
        if self.degraded {
            let mut found = false;
            self.scan_jsonl_edges(|edge| {
                found = edge.from == path || edge.to == path;
                !found
            })?;
            return Ok(found);
        }

        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM edges WHERE \"from\" = ? OR \"to\" = ?",
            params![path, path],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count > 0)
    }

    /// GAP-081 phase 2a: does the store hold `path` as an edge endpoint?
    ///
    /// The `edges` table is the authority on what the graph can answer for: a
    /// path that appears as `from` or `to` has a real answer even when its
    /// extension is not one of the 26 AST languages. The motivating case is a
    /// `.proto` service contract anchor — no AST language claims `.proto`, but
    /// `service_contract` edges point at it, so `impact`/`related` on that path
    /// is a real query with real dependents.
    ///
    /// Any query error reads as "not an endpoint": this predicate only ever
    /// RELAXES the PERF-004 fast failure, so it must not panic and must not
    /// propagate — a store that cannot answer leaves the previous, stricter
    /// behaviour in place.
    fn path_is_graph_endpoint(&self, path: &str) -> bool {
        self.file_in_graph(path).unwrap_or(false)
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
        self.require_cached_query("untested_files")?;
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
    /// file-level edges such as `tests/lib_test.rs -> src/lib.rs`), OR —
    /// for languages whose package node is finer than the file, i.e. Python
    /// and Go — its resolved `pkg:` node is the literal `to` of some
    /// `tested_by` edge, OR — for TypeScript/JavaScript — any `local:` node
    /// the [`LocalSpecResolver`] resolves the file to is one, provided the
    /// `tested_by` edge's own importer resolves to the same file (GAP-071;
    /// see [`GraphDB::file_is_covered`]). `.rs` files are deliberately excluded from the
    /// second rule: their resolved node is the Cargo CRATE, and one
    /// crate-root integration test would otherwise mark every member file
    /// covered (GAP-057/064 lineage — see [`GraphDB::file_is_covered`]).
    /// Package-ancestor matching is deliberately out of scope: a test that
    /// targets `pkg:fastapi.dependencies` does not cover
    /// `fastapi/dependencies/utils.py`.
    pub fn untested_files_at(&self, root: &Path) -> GraphResult<Vec<String>> {
        self.require_cached_query("untested_files_at")?;
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
        // path costs at most one filesystem walk. GAP-071: one
        // `LocalSpecResolver` per query as well — the index is a single
        // SQL scan over the `local:` edges.
        let tested = self.tested_by_targets()?;
        let tested_importers = self.tested_by_importers()?;
        let mut resolver = PkgResolver::new();
        let local_resolver = LocalSpecResolver::from_edges(&self.conn).ok();
        files.retain(|f| {
            !Self::file_is_covered(
                f,
                &tested,
                &tested_importers,
                root,
                &mut resolver,
                local_resolver.as_ref(),
            )
        });
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
        self.require_cached_query("module_files")?;
        self.module_files_at(Path::new("."), module_name)
    }

    /// [`GraphDB::module_files`] with an explicit resolution root (GAP-066).
    ///
    /// `test_coverage_pct` counts a file under the prefix as covered under
    /// the same three rules `untested_files_at` uses (literal `tested_by`
    /// target; the file's resolved `pkg:` node is one, where the package
    /// node is finer than the file — Python and Go; or one of the file's
    /// resolved `local:` nodes is one, with the edge importer resolving to
    /// the same file — TypeScript/JavaScript, GAP-071). `.rs` files are
    /// deliberately excluded from the node rules: `PkgResolver::pkg_node`
    /// returns the enclosing Cargo CRATE for a `.rs` file, so a single
    /// crate-root `tested_by` edge would report a crate's whole `src/`
    /// module as covered (GAP-057/064 lineage — see
    /// [`GraphDB::file_is_covered`]). `files` and `edges_count` are the
    /// unmodified file-level views of the module.
    pub fn module_files_at(&self, root: &Path, module_name: &str) -> GraphResult<ModuleStats> {
        self.require_cached_query("module_files_at")?;
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
        // the `tested_by` edge names the file itself, the `pkg:` node it
        // belongs to, or — TS/JS (GAP-071) — a `local:` node resolving to
        // it.
        let tested = self.tested_by_targets()?;
        let tested_importers = self.tested_by_importers()?;
        let mut resolver = PkgResolver::new();
        let local_resolver = LocalSpecResolver::from_edges(&self.conn).ok();
        let tested_count = files
            .iter()
            .filter(|f| {
                Self::file_is_covered(
                    f,
                    &tested,
                    &tested_importers,
                    root,
                    &mut resolver,
                    local_resolver.as_ref(),
                )
            })
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

    /// The `to` node of every `local:`-targeted `tested_by` edge, mapped to
    /// the set of edge `from` importers (GAP-071).
    ///
    /// Rule (iii) needs the importer side of the edge, not just the target
    /// set: the SAME `local:` node string can be emitted from different
    /// directories naming DIFFERENT targets, so only an importer whose own
    /// directory resolves the node to this file proves a test targets THIS
    /// file (the mirror of [`LocalSpecResolver`]'s importer sets).
    fn tested_by_importers(&self) -> GraphResult<HashMap<String, HashSet<String>>> {
        let mut stmt = self.conn.prepare(
            "SELECT \"to\", \"from\" FROM edges \
             WHERE rel = 'tested_by' AND \"to\" LIKE 'local:%'",
        )?;
        let rows = stmt.query_map(params![], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut map: HashMap<String, HashSet<String>> = HashMap::new();
        for r in rows {
            let (to, from) = r?;
            map.entry(to).or_default().insert(from);
        }
        Ok(map)
    }

    /// Whether `file` counts as covered by some test (GAP-066, extended by
    /// GAP-071).
    ///
    /// Rule (i): `file` is the literal target of a `tested_by` edge. Rule
    /// (ii): the `pkg:` node `file` resolves to (via `root`) is. Rule
    /// (iii): a `local:` node `file` resolves to is a `tested_by` target
    /// AND that edge's own importer (`from`) is among the importers the
    /// [`LocalSpecResolver`] associates with the file for that node. A file
    /// that resolves to no package or `local:` node is never covered by
    /// rules (ii)/(iii) — it must not silently fall back to an enclosing
    /// crate, module, or directory.
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
    /// Rule (iii) is a *specifier* rule for the TS/JS family (`.ts`, `.tsx`,
    /// `.js`, `.jsx`, `.mjs`, `.cjs`): the parser stores relative specifiers
    /// verbatim (`local:../forwardConsole`), so one target file is named by
    /// many node strings and the target itself never appears as an edge
    /// target. The importer check is load-bearing, not hygiene — the same
    /// node string can be emitted from different directories naming
    /// DIFFERENT files, so membership of the node in the tested set alone
    /// would cover a decoy file that merely shares the specifier from
    /// another directory. A bare node-name match is deliberately
    /// insufficient. (`.rs` is excluded here too: its node graph is
    /// crate-level, and rule (i) already serves it.)
    ///
    /// This is GAP-057/064/071 lineage, not an oversight: the granularity
    /// dispatch below mirrors `PkgResolver::pkg_node`'s own extension
    /// dispatch, which routes `.go` and `.py` to their own walks and
    /// everything else to the Cargo walk.
    fn file_is_covered(
        file: &str,
        tested: &HashSet<String>,
        tested_importers: &HashMap<String, HashSet<String>>,
        root: &Path,
        resolver: &mut PkgResolver,
        local_resolver: Option<&LocalSpecResolver>,
    ) -> bool {
        if tested.contains(file) {
            return true;
        }
        // `.rs` stays out of every node-resolution rule: both the Cargo
        // crate node (rule ii) and the `local:` index (rule iii) are
        // coarser or orthogonal to the file, and the Rust parser already
        // emits file-level `tested_by` edges, so rule (i) is the only rule
        // a Rust file can safely be covered by.
        if file.ends_with(".rs") {
            return false;
        }
        // Rule (ii) applies only to package nodes finer than the file
        // (Python, Go). The Cargo crate node is coarser than a `.rs` file,
        // so a crate-level `tested_by` target must never cover crate
        // members. See the doc comment above.
        if let Some(node) = resolver.pkg_node(&root.join(file).to_string_lossy()) {
            return tested.contains(&node);
        }
        // Rule (iii): TS/JS `local:` resolution (GAP-071). Only files of
        // the JS family are eligible; a file that resolves to no `local:`
        // node stays uncovered (no directory fallback).
        let Some(local) = local_resolver else {
            return false;
        };
        if !is_js_like_file(file) {
            return false;
        }
        local.nodes_for(file).into_iter().any(|(node, importers)| {
            tested.contains(&node)
                && tested_importers
                    .get(&node)
                    .is_some_and(|froms| froms.iter().any(|from| importers.contains(from)))
        })
    }

    /// Compute comprehensive [`GraphStats`] using DuckDB aggregate queries.
    pub fn stats(&self) -> GraphResult<GraphStats> {
        self.require_cached_query("stats")?;
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
        if self.degraded {
            return self
                .require_cached_query("ensure_parsed")
                .and(Ok(Vec::new()));
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
        // PERF-004: same short-circuit as impact_or_parse (see there). GAP-081
        // phase 2a: it applies only when the path is NOT a graph endpoint — a
        // `.proto` contract anchor is an endpoint, so its answer is real.
        // DF-WARPFS-2: a `file:`-prefixed target resolves exactly like the
        // bare path — normalize once up front so every check below (guards,
        // cache lookup, on-the-fly parse) and the query itself use the bare
        // form, and an unresolvable target's error names the bare form.
        let path = strip_file_prefix(path);
        if !path.starts_with("pkg:") && !path.starts_with("sys:") {
            let not_indexable = Path::new(path)
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| crate::parser::Language::from_extension(e).is_none())
                .unwrap_or(false);
            if not_indexable && !self.path_is_graph_endpoint(path) {
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
                "'{path}' is not in the graph (no such file and no matching graph node). {UNRESOLVABLE_TARGET_HINT}"
            )));
        }

        // Cache hit → query directly.
        if self.file_in_graph(path)? {
            return self.related(path, rel_filter, direction);
        }
        if self.degraded {
            return self
                .require_cached_query("related_or_parse")
                .and(Ok(Vec::new()));
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
        // DF-WARPFS-2: a `file:`-prefixed target resolves exactly like the
        // bare path — normalize once up front so every check below (guards,
        // cache lookup, on-the-fly parse) and the BFS itself use the bare
        // form, and an unresolvable target's error names the bare form.
        let start_path = strip_file_prefix(start_path);
        // Node-existence check at query time: unknown paths (not in graph,
        // not on disk) must fail loudly instead of looking like a node with
        // zero dependents. Symbol nodes (pkg:/sys:) pass when in the graph.
        // PERF-004: non-indexable extensions (markdown, yaml, json, lock files,
        // ...) can never appear in the AST graph — fail fast with the real
        // reason instead of a misleading "No dependents found". Symbol nodes
        // (pkg:/sys:) and files with no extension are exempt.
        //
        // GAP-081 phase 2a: the guard fires only when the path is NOT a graph
        // endpoint. PERF-004 exists so that a non-indexable file can never
        // return a silently empty answer; when the path IS an edge endpoint the
        // answer is real — a `.proto` service contract anchor has dependents
        // (`service_contract` edges from every caller of the declared service),
        // so the fast failure no longer applies to it. An unknown
        // non-indexable path still fails exactly as before.
        if !start_path.starts_with("pkg:") && !start_path.starts_with("sys:") {
            let not_indexable = Path::new(start_path)
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| crate::parser::Language::from_extension(e).is_none())
                .unwrap_or(false);
            if not_indexable && !self.path_is_graph_endpoint(start_path) {
                return Err(GraphError::Other(format!(
                    "'{start_path}' is not an indexable source file (26 AST languages) — impact/related cannot answer for it"
                )));
            }
        }
        let in_graph = self.file_in_graph(start_path)?;
        if !in_graph && !Path::new(start_path).exists() {
            return Err(GraphError::Other(format!(
                "'{start_path}' is not in the graph (no such file and no matching graph node). {UNRESOLVABLE_TARGET_HINT}"
            )));
        }
        if self.degraded {
            if !in_graph {
                return self
                    .require_cached_query("impact_or_parse")
                    .and(Ok(Vec::new()));
            }
            let local_resolver = self.local_resolver_from_jsonl()?;
            return impact::compute_impact_streaming(
                start_path,
                max_depth,
                local_resolver.as_ref(),
                |visit| self.scan_jsonl_edges(visit),
            );
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
    use crate::impact::SCOPE_FILE;

    fn write_go_file(dir: &Path, name: &str, content: &str) -> String {
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn current_setting(conn: &Connection, name: &str) -> String {
        conn.query_row(
            &format!("SELECT CAST(current_setting('{name}') AS VARCHAR)"),
            [],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn normalized_memory_setting(value: &str) -> String {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!("SET memory_limit='{}'", value.replace('\'', "''")))
            .unwrap();
        current_setting(&conn, "memory_limit")
    }

    fn create_graph_path(root: &Path) -> PathBuf {
        let graph_dir = root.join(".vfs/graph");
        std::fs::create_dir_all(&graph_dir).unwrap();
        graph_dir.join("graph.db")
    }

    #[test]
    fn graphdb_open_applies_manifest_duckdb_settings() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = create_graph_path(dir.path());
        std::fs::write(
            dir.path().join(".vfs/manifest.yaml"),
            "project:\n  name: knob-test\nperformance:\n  duckdb:\n    memory_limit: 1GB\n    threads: 2\n",
        )
        .unwrap();

        let db = GraphDB::open(db_path.to_str().unwrap()).unwrap();
        assert_eq!(current_setting(db.conn(), "threads"), "2");
        assert_eq!(
            current_setting(db.conn(), "memory_limit"),
            normalized_memory_setting("1GB")
        );
        assert_eq!(
            current_setting(db.conn(), "preserve_insertion_order"),
            "false"
        );
    }

    #[test]
    fn graphdb_open_without_manifest_uses_duckdb_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = create_graph_path(dir.path());

        let db = GraphDB::open(db_path.to_str().unwrap()).unwrap();
        assert_eq!(current_setting(db.conn(), "threads"), "4");
        assert_eq!(
            current_setting(db.conn(), "memory_limit"),
            normalized_memory_setting("512MB")
        );
    }

    #[test]
    fn graphdb_open_with_malformed_manifest_uses_duckdb_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = create_graph_path(dir.path());
        std::fs::write(dir.path().join(".vfs/manifest.yaml"), "not: [valid\n").unwrap();

        let db = GraphDB::open(db_path.to_str().unwrap()).unwrap();
        assert_eq!(current_setting(db.conn(), "threads"), "4");
        assert_eq!(
            current_setting(db.conn(), "memory_limit"),
            normalized_memory_setting("512MB")
        );
    }

    fn impact_facts(rows: &[ImpactFile]) -> Vec<(String, String, u32, String, Option<String>)> {
        let mut facts = rows
            .iter()
            .map(|row| {
                (
                    row.path.clone(),
                    row.relation.clone(),
                    row.depth,
                    row.scope.clone(),
                    row.via.clone(),
                )
            })
            .collect::<Vec<_>>();
        facts.sort();
        facts
    }

    fn write_spill_fixture(root: &Path, watermark: &str) -> (PathBuf, Vec<Edge>) {
        let db_path = create_graph_path(root);
        let jsonl = db_path.parent().unwrap().join("edges.jsonl");
        std::fs::write(
            root.join(".vfs/manifest.yaml"),
            format!(
                "project:\n  name: spill-test\nperformance:\n  duckdb:\n    memory_limit: 64MB\n    spill_watermark: {watermark}\n"
            ),
        )
        .unwrap();
        let edges = vec![
            Edge::new("src/main.rs", "pkg:serde", "imports"),
            Edge::new("src/main.rs", "src/dependent.rs", "imports"),
            Edge::new("src/dependent.rs", "src/lib.rs", "imports"),
            Edge::new("tests/lib_test.rs", "src/lib.rs", "tested_by"),
            Edge::new("src/rpc.rs", "service:payments", "calls_service"),
        ];
        let content = edges
            .iter()
            .map(|edge| serde_json::to_string(edge).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(jsonl, content).unwrap();
        (db_path, edges)
    }

    #[test]
    fn graphdb_open_degrades_to_streaming_above_spill_watermark() {
        let dir = tempfile::tempdir().unwrap();
        let (db_path, expected) = write_spill_fixture(dir.path(), "1B");
        let jsonl = db_path.parent().unwrap().join("edges.jsonl");

        let db = GraphDB::open(db_path.to_str().unwrap()).unwrap();
        assert!(
            db.is_degraded(),
            "the watermark must select JSONL streaming"
        );
        assert!(
            !reconcile_stamp_path(&jsonl).exists(),
            "a skipped replay must not claim a successful reconciliation"
        );
        let cached: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))
            .unwrap();
        assert_eq!(cached, 0, "degraded open must leave graph.db as-is");

        let related = db.related("src/main.rs", None, Direction::Forward).unwrap();
        assert_eq!(
            related,
            expected[..2],
            "streaming JSONL must answer correctly"
        );

        let impact = db
            .impact_or_parse("src/lib.rs", 8)
            .expect("degraded impact must stream the canonical JSONL");
        assert_eq!(
            impact_facts(&impact),
            vec![
                (
                    "src/dependent.rs".to_string(),
                    "imports".to_string(),
                    1,
                    SCOPE_FILE.to_string(),
                    None,
                ),
                (
                    "src/main.rs".to_string(),
                    "imports".to_string(),
                    2,
                    SCOPE_FILE.to_string(),
                    None,
                ),
                (
                    "tests/lib_test.rs".to_string(),
                    "tested_by".to_string(),
                    1,
                    SCOPE_FILE.to_string(),
                    None,
                ),
            ],
            "impact must not read the intentionally empty DuckDB cache"
        );

        let expected_error = |api: &str| {
            format!(
                "graph error: graph query unavailable in degraded (spill) mode: {api}; edges.jsonl exceeds spill_watermark 1 bytes"
            )
        };
        assert_eq!(
            db.group_by_dependency().unwrap_err().to_string(),
            expected_error("group_by_dependency")
        );
        assert_eq!(
            db.distinct_files().unwrap_err().to_string(),
            expected_error("distinct_files")
        );
        assert_eq!(
            db.distinct_service_edges(&["calls_service"])
                .unwrap_err()
                .to_string(),
            expected_error("distinct_service_edges")
        );
        assert_eq!(
            db.untested_files().unwrap_err().to_string(),
            expected_error("untested_files")
        );
        assert_eq!(
            db.untested_files_at(dir.path()).unwrap_err().to_string(),
            expected_error("untested_files_at")
        );
        assert_eq!(
            db.module_files("src").unwrap_err().to_string(),
            expected_error("module_files")
        );
        assert_eq!(
            db.module_files_at(dir.path(), "src")
                .unwrap_err()
                .to_string(),
            expected_error("module_files_at")
        );
        assert_eq!(db.stats().unwrap_err().to_string(), expected_error("stats"));
    }

    #[test]
    fn graphdb_open_replays_below_spill_watermark() {
        let dir = tempfile::tempdir().unwrap();
        let (db_path, edges) = write_spill_fixture(dir.path(), "1MB");
        let jsonl = db_path.parent().unwrap().join("edges.jsonl");

        let db = GraphDB::open(db_path.to_str().unwrap()).unwrap();
        assert!(!db.is_degraded(), "small corpora must keep the DuckDB path");
        assert!(
            reconcile_stamp_path(&jsonl).exists(),
            "a successful replay must write its stamp"
        );
        assert_eq!(db.count_edges().unwrap(), edges.len() as i64);
        assert_eq!(
            db.related("src/main.rs", None, Direction::Forward).unwrap(),
            edges[..2]
        );

        let impact = db
            .impact_or_parse("src/lib.rs", 8)
            .expect("non-degraded impact must use the replayed DuckDB cache");
        assert_eq!(
            impact_facts(&impact),
            vec![
                (
                    "src/dependent.rs".to_string(),
                    "imports".to_string(),
                    1,
                    SCOPE_FILE.to_string(),
                    None,
                ),
                (
                    "src/main.rs".to_string(),
                    "imports".to_string(),
                    2,
                    SCOPE_FILE.to_string(),
                    None,
                ),
                (
                    "tests/lib_test.rs".to_string(),
                    "tested_by".to_string(),
                    1,
                    SCOPE_FILE.to_string(),
                    None,
                ),
            ],
            "DuckDB control must match the degraded streaming answer"
        );
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

    /// GAP-081 phase 2a: a `.proto` service contract anchor is not an
    /// indexable AST file, but the store holds real dependents for it
    /// (`service_contract` edges from every caller of the declared service), so
    /// `impact_or_parse` and `related_or_parse` must answer rather than take
    /// the PERF-004 fast failure. The companion assertion is the falsification:
    /// an unseen non-indexable path still fails with the message unchanged.
    #[test]
    fn contract_anchor_that_is_an_edge_endpoint_answers_instead_of_failing() {
        let db = GraphDB::open(":memory:").unwrap();
        let edges = vec![
            Edge::new(
                "src/frontend/rpc.go",
                "protos/demo.proto",
                "service_contract",
            ),
            Edge::new(
                "src/checkoutservice/main.go",
                "protos/demo.proto",
                "service_contract",
            ),
        ];
        db.insert_edges(&edges).unwrap();
        assert!(
            db.path_is_graph_endpoint("protos/demo.proto"),
            "precondition: the anchor is an edge endpoint in the store"
        );

        // impact: the contract anchor has exactly the two callers as dependents.
        let impact = db
            .impact_or_parse("protos/demo.proto", 8)
            .expect("an endpoint must answer instead of taking the PERF-004 fast failure");
        let mut paths: Vec<String> = impact.iter().map(|f| f.path.clone()).collect();
        paths.sort();
        assert_eq!(
            paths,
            vec![
                "src/checkoutservice/main.go".to_string(),
                "src/frontend/rpc.go".to_string()
            ],
            "contract dependents must be returned, got {impact:?}"
        );
        assert!(
            impact.iter().all(|f| f.relation == "service_contract"),
            "the relation must survive the query: {impact:?}"
        );

        // related (reverse): same two edges, the other entry point.
        let related = db
            .related_or_parse("protos/demo.proto", None, Direction::Reverse)
            .expect("an endpoint must answer instead of taking the PERF-004 fast failure");
        assert_eq!(related.len(), 2, "expected 2 incoming edges: {related:?}");

        // Falsification: a non-indexable path the store has never seen is still
        // rejected, with the message byte-identical to the pre-change one.
        for unseen in ["protos/unknown.proto", "docs/notes.md"] {
            let expected = format!(
                "graph error: '{unseen}' is not an indexable source file (26 AST languages) — impact/related cannot answer for it"
            );
            let impact_err = db
                .impact_or_parse(unseen, 8)
                .expect_err("an unknown non-indexable path must still fail");
            assert_eq!(impact_err.to_string(), expected);
            let related_err = db
                .related_or_parse(unseen, None, Direction::Reverse)
                .expect_err("an unknown non-indexable path must still fail");
            assert_eq!(related_err.to_string(), expected);
        }
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

    // ── DF-WARPFS-2: `file:` id-prefix normalization + teaching errors ──

    #[test]
    fn strip_file_prefix_strips_one_prefix_only() {
        // One strip: `file:src/main.rs` → the bare path. `pkg:`/`sys:` and
        // unprefixed paths pass through; a doubled `file:file:x` keeps its
        // inner `file:` (it targets a path literally named `file:x`).
        assert_eq!(strip_file_prefix("file:src/main.rs"), "src/main.rs");
        assert_eq!(strip_file_prefix("src/main.rs"), "src/main.rs");
        assert_eq!(strip_file_prefix("pkg:serde"), "pkg:serde");
        assert_eq!(strip_file_prefix("sys:stdio.h"), "sys:stdio.h");
        assert_eq!(strip_file_prefix("file:file:x.rs"), "file:x.rs");
    }

    #[test]
    fn file_prefixed_path_resolves_like_bare_path_in_related() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_go_file(dir.path(), "main.go", "package main\n\nimport \"fmt\"\n");
        let edges = vec![Edge::new("consumer.go", &path, "imports")];

        let bare_db = GraphDB::open(":memory:").unwrap();
        bare_db.insert_edges(&edges).unwrap();
        let bare = bare_db
            .related_or_parse(&path, None, Direction::Reverse)
            .expect("bare path must resolve");
        assert_eq!(bare.len(), 1, "the bare node has exactly the seeded edge");

        // An identical store queried through the `file:` form must return
        // the same edges — the prefix resolves to the bare node.
        let prefixed_db = GraphDB::open(":memory:").unwrap();
        prefixed_db.insert_edges(&edges).unwrap();
        let prefixed = prefixed_db
            .related_or_parse(&format!("file:{path}"), None, Direction::Reverse)
            .expect("a file:-prefixed path must resolve like the bare path");
        assert_eq!(prefixed, bare, "prefixed and bare queries must agree");
        assert!(
            !prefixed_db.file_in_graph(&format!("file:{path}")).unwrap(),
            "the prefixed form must not become its own graph node"
        );
    }

    #[test]
    fn file_prefixed_path_resolves_like_bare_path_in_impact() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_go_file(dir.path(), "main.go", "package main\n\nimport \"fmt\"\n");
        let edges = vec![Edge::new("consumer.go", &path, "imports")];

        let bare_db = GraphDB::open(":memory:").unwrap();
        bare_db.insert_edges(&edges).unwrap();
        let bare = bare_db.impact_or_parse(&path, 8).unwrap();

        let prefixed_db = GraphDB::open(":memory:").unwrap();
        prefixed_db.insert_edges(&edges).unwrap();
        let prefixed = prefixed_db
            .impact_or_parse(&format!("file:{path}"), 8)
            .expect("a file:-prefixed path must resolve like the bare path");

        let mut bare_paths: Vec<String> = bare.iter().map(|f| f.path.clone()).collect();
        bare_paths.sort();
        let mut prefixed_paths: Vec<String> = prefixed.iter().map(|f| f.path.clone()).collect();
        prefixed_paths.sort();
        assert_eq!(
            prefixed_paths, bare_paths,
            "prefixed and bare impact must agree, bare={bare:?} prefixed={prefixed:?}"
        );
        assert!(
            !prefixed_db.file_in_graph(&format!("file:{path}")).unwrap(),
            "the prefixed form must not become its own graph node"
        );
    }

    #[test]
    fn unresolvable_target_error_teaches_id_forms() {
        // DF-WARPFS-2: the unresolvable-target error must keep the stable
        // "is not in the graph" contract AND teach the three accepted id
        // shapes — a `file:`-prefixed typo must read as "drop the prefix",
        // not as plain file-not-found. Same wording for both entry points
        // and for the bare and `file:` input forms.
        let db = GraphDB::open(":memory:").unwrap();
        for target in ["no/such/file_xyz.rs", "file:no/such/file_xyz.rs"] {
            let impact_msg = db
                .impact_or_parse(target, 3)
                .expect_err("unresolvable target must error")
                .to_string();
            assert!(
                impact_msg.contains("is not in the graph"),
                "stable substring must survive, got: {impact_msg}"
            );
            for form in [
                "Accepted id forms",
                "bare repo-relative path",
                "sys:<header>",
                "pkg:<crate>",
            ] {
                assert!(
                    impact_msg.contains(form),
                    "impact error must name '{form}', got: {impact_msg}"
                );
            }

            let related_msg = db
                .related_or_parse(target, None, Direction::Reverse)
                .expect_err("unresolvable target must error")
                .to_string();
            for form in [
                "is not in the graph",
                "Accepted id forms",
                "bare repo-relative path",
                "sys:<header>",
                "pkg:<crate>",
            ] {
                assert!(
                    related_msg.contains(form),
                    "related error must name '{form}', got: {related_msg}"
                );
            }
        }
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
    fn reconcile_tiny_chunks_preserve_counts_content_and_idempotency() {
        let dir = tempfile::tempdir().unwrap();
        let jsonl = dir.path().join("edges.jsonl");
        let first = serde_json::to_string(&Edge::new("a.go", "b.go", "imports")).unwrap();
        let second = serde_json::to_string(&Edge::new("c.go", "d.go", "tested_by")).unwrap();
        std::fs::write(
            &jsonl,
            format!("{first}\nnot json\n{second}\n{first}\n   \n"),
        )
        .unwrap();

        let conn = Connection::open_in_memory().unwrap();
        let processed = reconcile_edges_from_jsonl_with_chunk_size(&conn, &jsonl, 2).unwrap();
        assert_eq!(
            processed, 3,
            "the duplicate is processed while malformed and blank lines are skipped"
        );

        let mut stmt = conn
            .prepare("SELECT \"from\", \"to\", rel FROM edges ORDER BY \"from\", \"to\", rel")
            .unwrap();
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                ("a.go".into(), "b.go".into(), "imports".into()),
                ("c.go".into(), "d.go".into(), "tested_by".into())
            ]
        );

        let processed_again = reconcile_edges_from_jsonl_with_chunk_size(&conn, &jsonl, 2).unwrap();
        assert_eq!(processed_again, 3);
        let row_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))
            .unwrap();
        assert_eq!(row_count, 2, "a second replay must not duplicate rows");
    }

    #[test]
    fn reconcile_5003_lines_uses_default_chunks_and_stamps_only_success() {
        let dir = tempfile::tempdir().unwrap();
        let jsonl = dir.path().join("edges.jsonl");
        let mut lines: Vec<String> = (0..5_000)
            .map(|i| {
                serde_json::to_string(&Edge::new(
                    format!("src/file_{i}.rs"),
                    format!("pkg:dep_{i}"),
                    "imports",
                ))
                .unwrap()
            })
            .collect();
        lines.push(lines[0].clone());
        lines.push("not json".into());
        lines.push("{\"from\":\"missing required fields\"}".into());
        assert_eq!(lines.len(), 5_003);
        std::fs::write(&jsonl, lines.join("\n") + "\n").unwrap();

        let stamp = reconcile_stamp_path(&jsonl);
        let broken = Connection::open_in_memory().unwrap();
        broken
            .execute_batch(
                "CREATE TABLE edges (\
                    \"from\" TEXT NOT NULL,\
                    \"to\" TEXT NOT NULL,\
                    rel TEXT NOT NULL CHECK (rel = 'never'),\
                    provenance TEXT NOT NULL DEFAULT 'ast_exact',\
                    confidence REAL NOT NULL DEFAULT 1.0\
                 )",
            )
            .unwrap();
        assert!(reconcile_edges_from_jsonl(&broken, &jsonl).is_err());
        assert!(
            !stamp.exists(),
            "a failed replay must never write the reconcile stamp"
        );

        let conn = Connection::open_in_memory().unwrap();
        let processed = reconcile_edges_from_jsonl(&conn, &jsonl).unwrap();
        assert_eq!(
            processed, 5_001,
            "5000 unique + one duplicate are processed"
        );
        let row_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))
            .unwrap();
        assert_eq!(row_count, 5_000);
        assert_eq!(
            std::fs::read_to_string(&stamp).unwrap(),
            jsonl_fingerprint(&jsonl).unwrap(),
            "the stamp is written only after the full successful replay"
        );

        let processed_again = reconcile_edges_from_jsonl(&conn, &jsonl).unwrap();
        assert_eq!(processed_again, 5_001);
        let row_count_again: i64 = conn
            .query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))
            .unwrap();
        assert_eq!(row_count_again, 5_000, "default replay must be idempotent");
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

    // -----------------------------------------------------------------------
    // GAP-071 — coverage consumers resolve `local:` tested_by edges (TS/JS)
    // -----------------------------------------------------------------------

    /// A tempdir shaped like the GAP-071 reproduction corpus: a TS
    /// monorepo where two packages hold a same-named `forwardConsole.ts`
    /// in different directories. The x-package spec covers its target the
    /// way `classify_js` emits edges — the relative specifier stored
    /// verbatim as `local:../forwardConsole`. The y package merely IMPORTS
    /// its own same-named target through the identical node string from
    /// another directory (`src/inner/app.ts`), with no `tested_by` edge of
    /// its own — the decoy a naive node-string match would wrongly cover.
    fn ts_corpus() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for rel in [
            "packages/x/src/shared/forwardConsole.ts",
            "packages/x/src/shared/__tests__/forwardConsole.spec.ts",
            "packages/y/src/forwardConsole.ts",
            "packages/y/src/inner/app.ts",
        ] {
            touch(dir.path(), rel);
        }
        dir
    }

    fn ts_corpus_edges() -> Vec<Edge> {
        vec![
            // The x spec covers its subject: specifier verbatim, so the
            // target file is named by a node string, never a file path.
            Edge::new(
                "packages/x/src/shared/__tests__/forwardConsole.spec.ts",
                "local:../forwardConsole",
                "imports",
            ),
            Edge::new(
                "packages/x/src/shared/__tests__/forwardConsole.spec.ts",
                "local:../forwardConsole",
                "tested_by",
            ),
            // The y package imports its own same-named target through the
            // IDENTICAL node string from a different directory — an
            // imports edge only, no test.
            Edge::new(
                "packages/y/src/inner/app.ts",
                "local:../forwardConsole",
                "imports",
            ),
            // Targets participate as importers so they enter the untested
            // query's candidate pool (and the module file list).
            Edge::new(
                "packages/x/src/shared/forwardConsole.ts",
                "pkg:node:path",
                "imports",
            ),
            Edge::new(
                "packages/y/src/forwardConsole.ts",
                "pkg:node:path",
                "imports",
            ),
        ]
    }

    #[test]
    fn untested_files_at_resolves_local_spec_coverage_for_typescript() {
        // GAP-071: the JS/TS parser stores relative specifiers verbatim
        // (`local:../forwardConsole`), so the target file is never the
        // literal `to` of a `tested_by` edge and `pkg_node` returns `None`
        // for `.ts` — both consumers read 0.0% while the edge data is real.
        let dir = ts_corpus();
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&ts_corpus_edges()).unwrap();

        let untested = db.untested_files_at(dir.path()).unwrap();
        assert!(
            !untested.contains(&"packages/x/src/shared/forwardConsole.ts".to_string()),
            "the spec's local: tested_by edge must cover the resolved target, got: {untested:?}"
        );

        // Both consumers agree on the same verdict for the same file.
        let stats = db.module_files_at(dir.path(), "packages/x").unwrap();
        assert_eq!(
            stats.files,
            vec![
                "packages/x/src/shared/__tests__/forwardConsole.spec.ts".to_string(),
                "packages/x/src/shared/forwardConsole.ts".to_string(),
            ],
            "file list must stay the file-level view of the module"
        );
        assert_eq!(
            stats.test_coverage_pct, 50.0,
            "the covered target counts toward module coverage (1 of 2 files)"
        );
    }

    #[test]
    fn local_spec_coverage_does_not_leak_across_directories() {
        // The precision trap: `local:../forwardConsole` is ONE node string
        // naming TWO different files (one per importer directory). Only
        // the x target has a covering `tested_by` edge — and its importer
        // (the x spec) resolves to the x target only. A raw string match
        // against the tested-target set would cover the y decoy too.
        let dir = ts_corpus();
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&ts_corpus_edges()).unwrap();

        let untested = db.untested_files_at(dir.path()).unwrap();
        assert!(
            untested.contains(&"packages/y/src/forwardConsole.ts".to_string()),
            "a file sharing the node string but with no tested_by edge of \
             its own must stay untested (importer context decides), got: {untested:?}"
        );
        assert_eq!(
            untested,
            vec![
                "packages/y/src/forwardConsole.ts".to_string(),
                "packages/y/src/inner/app.ts".to_string(),
            ],
            "exactly the two uncovered y files should remain, got: {untested:?}"
        );

        let y = db.module_files_at(dir.path(), "packages/y").unwrap();
        assert_eq!(y.files.len(), 2);
        assert_eq!(
            y.test_coverage_pct, 0.0,
            "the decoy package must read 0.0%: its files share a node \
             string with a test, but no test resolves to them"
        );
    }

    #[test]
    fn local_spec_coverage_covers_all_specifier_forms() {
        // `./x` (same dir), `../x` (parent dir), extension probing
        // (`../foo` -> `foo.ts`) and `index.*` probing (`../depth` ->
        // `depth/index.ts`) — the forms `LocalSpecResolver` documents.
        // Specs live in `__tests__/` (or `.test.ts`) so `is_test_file`
        // excludes them from the untested candidate pool, like production
        // corpora do.
        let dir = tempfile::tempdir().unwrap();
        for rel in [
            "src/widget.ts",
            "src/widget.test.ts",
            "src/util.ts",
            "src/__tests__/util.spec.ts",
            "src/foo.ts",
            "src/__tests__/probe.spec.ts",
            "src/depth/index.ts",
            "src/__tests__/idx.spec.ts",
        ] {
            touch(dir.path(), rel);
        }
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[
            // ./x — same directory as the target.
            Edge::new("src/widget.test.ts", "local:./widget", "imports"),
            Edge::new("src/widget.test.ts", "local:./widget", "tested_by"),
            // ../x — specifier crossing a directory boundary.
            Edge::new("src/__tests__/util.spec.ts", "local:../util", "imports"),
            Edge::new("src/__tests__/util.spec.ts", "local:../util", "tested_by"),
            // Extension probing: `../foo` resolves to `foo.ts`.
            Edge::new("src/__tests__/probe.spec.ts", "local:../foo", "imports"),
            Edge::new("src/__tests__/probe.spec.ts", "local:../foo", "tested_by"),
            // Index probing: `../depth` resolves to `depth/index.ts`.
            Edge::new("src/__tests__/idx.spec.ts", "local:../depth", "imports"),
            Edge::new("src/__tests__/idx.spec.ts", "local:../depth", "tested_by"),
            // Targets enter the candidate pool via their own imports edges.
            Edge::new("src/widget.ts", "pkg:node:path", "imports"),
            Edge::new("src/util.ts", "pkg:node:path", "imports"),
            Edge::new("src/foo.ts", "pkg:node:path", "imports"),
            Edge::new("src/depth/index.ts", "pkg:node:path", "imports"),
        ])
        .unwrap();

        let untested = db.untested_files_at(dir.path()).unwrap();
        assert_eq!(
            untested,
            Vec::<String>::new(),
            "every resolved target is covered and every spec is a test \
             file; got: {untested:?}"
        );

        let stats = db.module_files_at(dir.path(), "src").unwrap();
        // 8 files: the 4 targets plus the 4 specs (each spec is the `from`
        // of its own imports edge). Only the 4 targets are covered.
        assert_eq!(stats.files.len(), 8);
        assert_eq!(
            stats.test_coverage_pct, 50.0,
            "4 covered targets of 8 module files"
        );
    }

    #[test]
    fn typescript_files_without_covering_edges_stay_untested() {
        // A `.ts` file with no `local:` tested_by edge at all must stay
        // listed — rule (iii) must not blanket-cover the JS family.
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "src/orphan.ts");
        touch(dir.path(), "src/covered.ts");
        touch(dir.path(), "src/__tests__/covered.spec.ts");

        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[
            Edge::new("src/orphan.ts", "pkg:node:path", "imports"),
            Edge::new("src/covered.ts", "pkg:node:path", "imports"),
            Edge::new(
                "src/__tests__/covered.spec.ts",
                "local:../covered",
                "imports",
            ),
            Edge::new(
                "src/__tests__/covered.spec.ts",
                "local:../covered",
                "tested_by",
            ),
        ])
        .unwrap();

        let untested = db.untested_files_at(dir.path()).unwrap();
        assert_eq!(
            untested,
            vec!["src/orphan.ts".to_string()],
            "only the file with no covering edge stays listed, got: {untested:?}"
        );
    }
}
