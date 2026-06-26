//! DuckDB graph initialization and edge querying.
//!
//! Creates and manages the `.vfs/graph/graph.db` database for graph edge
//! storage and querying.

use duckdb::{params, Connection};
use hilo_metadata::inventory::Edge;

use crate::error::GraphResult;

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
    /// Count of distinct `from` values (source files).
    pub unique_files: i64,
    /// Count of distinct `to` values (unique dependencies).
    pub unique_dependencies: i64,
    /// The top 10 most-referenced dependencies as `(to, count)` pairs,
    /// ordered by reference count descending.
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

    /// Create the `edges` table and an index on `("from", rel)`.
    ///
    /// `"from"` and `"to"` are quoted because they are SQL keywords.
    fn init_schema(conn: &Connection) -> GraphResult<()> {
        conn.execute(
            "CREATE TABLE IF NOT EXISTS edges (\
                \"from\" TEXT NOT NULL,\
                \"to\" TEXT NOT NULL,\
                rel TEXT NOT NULL\
             )",
            params![],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_edges_from_rel ON edges(\"from\", rel)",
            params![],
        )?;
        conn.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_edges_unique ON edges(\"from\", \"to\", rel)",
            params![],
        )?;
        Ok(())
    }

    /// Insert multiple edges into the database using a prepared statement.
    pub fn insert_edges(&self, edges: &[Edge]) -> GraphResult<()> {
        for edge in edges {
            self.conn.execute(
                "INSERT OR IGNORE INTO edges (\"from\", \"to\", rel) VALUES (?, ?, ?)",
                params![edge.from, edge.to, edge.rel],
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

        let (sql, params_vec): (&str, Vec<Box<dyn duckdb::ToSql>>) = if let Some(rel) = rel_filter {
            (
                &*format!("SELECT \"from\", \"to\", rel FROM edges WHERE {column} = ? AND rel = ?"),
                vec![Box::new(path.to_string()), Box::new(rel.to_string())],
            )
        } else {
            (
                &*format!("SELECT \"from\", \"to\", rel FROM edges WHERE {column} = ?"),
                vec![Box::new(path.to_string())],
            )
        };
        let param_refs: Vec<&dyn duckdb::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();

        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            Ok(Edge {
                from: row.get::<_, String>(0)?,
                to: row.get::<_, String>(1)?,
                rel: row.get::<_, String>(2)?,
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

        Ok(GraphStats {
            total_edges,
            unique_files,
            unique_dependencies,
            top_dependencies: top,
        })
    }
}
