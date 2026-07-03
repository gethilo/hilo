//! DuckDB graph initialization and edge querying.
//!
//! Creates and manages the `.vfs/graph/graph.db` database for graph edge
//! storage and querying.

use std::path::Path;

use duckdb::{params, Connection};
use hilo_metadata::inventory::Edge;

use crate::error::{GraphError, GraphResult};
use crate::impact::{self, ImpactFile};
use crate::parser::{Language, Parser};

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

impl GraphDB {
    /// Open (or create) the DuckDB database at `path`.
    ///
    /// Pass `":memory:"` for an ephemeral in-memory database (useful for
    /// tests). The `edges` table and its lookup index are created if missing.
    pub fn open(path: &str) -> GraphResult<Self> {
        let conn = if path == ":memory:" {
            Connection::open_in_memory()?
        } else {
            Connection::open(path)?
        };
        Self::init_schema(&conn)?;
        Ok(GraphDB { conn })
    }

    /// Create the `edges` table and indexes.
    ///
    /// `\"from\"` and `\"to\"` are quoted because they are SQL keywords.
    ///
    /// The schema includes `provenance` (TEXT, default `'ast_exact'`) and
    /// `confidence` (REAL, default `1.0`) columns. If the table already
    /// exists with the old 3-column schema (no `provenance`), it is
    /// auto-migrated by adding the missing columns with `ALTER TABLE`.
    fn init_schema(conn: &Connection) -> GraphResult<()> {
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
        Self::migrate_schema(conn)?;

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
            let mut stmt = conn.prepare(
                "SELECT count(*) FROM pragma_table_info('edges') WHERE name = 'provenance'",
            )?;
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

    /// Insert multiple edges into the database using a prepared statement.
    pub fn insert_edges(&self, edges: &[Edge]) -> GraphResult<()> {
        for edge in edges {
            self.conn.execute(
                "INSERT OR IGNORE INTO edges (\"from\", \"to\", rel, provenance, confidence) VALUES (?, ?, ?, ?, ?)",
                params![edge.from, edge.to, edge.rel, edge.provenance, edge.confidence],
            )?;
        }
        Ok(())
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

        let (sql, params_vec): (String, Vec<Box<dyn duckdb::ToSql>>) = if let Some(rel) = rel_filter
        {
            (
                format!(
                    "SELECT \"from\", \"to\", rel, provenance, confidence FROM edges WHERE {column} = ? AND rel = ?"
                ),
                vec![
                    Box::new(path.to_string()),
                    Box::new(rel.to_string()),
                ],
            )
        } else {
            (
                format!(
                    "SELECT \"from\", \"to\", rel, provenance, confidence FROM edges WHERE {column} = ?"
                ),
                vec![Box::new(path.to_string())],
            )
        };
        let param_refs: Vec<&dyn duckdb::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();

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

        let mut edges = Vec::new();
        for row in rows {
            edges.push(row?);
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
    /// Returns the list of source files that import other files but are not
    /// covered by any test (sorted alphabetically).
    pub fn untested_files(&self) -> GraphResult<Vec<String>> {
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
        Ok(files)
    }

    /// Query module-level statistics for a given directory prefix.
    ///
    /// Returns all distinct files (both `from` and `to`) whose path starts
    /// with `module_name`, along with the total edge count and test-coverage
    /// percentage (files that have at least one `tested_by` edge ÷ total files).
    pub fn module_files(&self, module_name: &str) -> GraphResult<ModuleStats> {
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

        // Files in the module that have at least one tested_by edge.
        let tested_count: i64 = self.conn.query_row(
            "SELECT COUNT(DISTINCT \"to\") FROM edges \
             WHERE rel = 'tested_by' \
             AND \"to\" LIKE ?1 ESCAPE '\\'",
            params![format!("{like_prefix}%")],
            |row| row.get::<_, i64>(0),
        )?;

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

        // Top dependencies by reference count.
        let mut stmt = self.conn.prepare(
            "SELECT \"to\", COUNT(*) AS cnt \
             FROM edges \
             GROUP BY \"to\" \
             ORDER BY cnt DESC \
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
    pub fn impact_or_parse(
        &self,
        start_path: &str,
        max_depth: u32,
    ) -> GraphResult<Vec<ImpactFile>> {
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
}
