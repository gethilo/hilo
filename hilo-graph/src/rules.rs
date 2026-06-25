//! Manifest-driven SQL rule engine for the DuckDB dependency graph.
//!
//! Executes [`Rule`] definitions (name + description + SQL query) against
//! the graph's DuckDB connection. Never panics on malformed SQL — all errors
//! are returned as structured [`RuleError`] values.

use duckdb::{params, Connection};
use serde::Serialize;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A single rule from the manifest — name, human-readable description, and
/// a SQL query that runs against the `edges` table (and related tables).
#[derive(Debug, Clone, Serialize)]
pub struct Rule {
    pub name: String,
    pub description: String,
    pub query: String,
}

/// The result of checking a single rule: which rows matched the query.
#[derive(Debug, Clone, Serialize)]
pub struct RuleCheckResult {
    pub rule: String,
    pub description: String,
    /// Each element is an array of column values (one per row).
    pub matches: Vec<Vec<String>>,
    /// Total number of matched rows.
    pub total: usize,
}

/// A structured error returned when a rule query fails.  The engine **never**
/// panics on invalid SQL — callers always receive this error type.
#[derive(Debug, Clone, Serialize)]
pub struct RuleError {
    pub rule: String,
    pub error: String,
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// Maximum columns we probe for when dynamically discovering the result width.
const MAX_COLUMNS: usize = 20;

/// Stateless engine for executing manifest-defined SQL rules.
///
/// All methods accept a borrowed [`Connection`] so the caller controls
/// the database lifecycle (open, close, reuse across calls).
pub struct RuleEngine;

impl RuleEngine {
    /// Execute a single rule and return the matching rows.
    ///
    /// Each row is a `Vec<String>` where position `i` corresponds to column
    /// `i` of the query result.  Column names are **not** included in the
    /// output — the caller is expected to know the schema from the rule
    /// definition.
    ///
    /// Uses dynamic column discovery: tries columns 0..MAX_COLUMNS and stops
    /// at the first `Err` (out-of-bounds) on each row.  This avoids the
    /// `column_count()` pitfall where DuckDB's Rust binding panics on unexecuted
    /// statements.
    pub fn check(conn: &Connection, rule: &Rule) -> Result<RuleCheckResult, RuleError> {
        // Prepare the statement (catches SQL syntax errors).
        let mut stmt = conn.prepare(&rule.query).map_err(|e| RuleError {
            rule: rule.name.clone(),
            error: format!("SQL parse error: {e}"),
        })?;

        // Execute and collect rows dynamically.
        let mut matches: Vec<Vec<String>> = Vec::new();

        let rows = stmt
            .query_map(params![], |row| {
                let mut cells: Vec<String> = Vec::new();
                for i in 0..MAX_COLUMNS {
                    // Try string first, then integer, then stop.
                    match row.get::<_, String>(i) {
                        Ok(s) => cells.push(s),
                        Err(_) => match row.get::<_, i64>(i) {
                            Ok(n) => cells.push(n.to_string()),
                            Err(_) => break, // no more columns for this row
                        },
                    }
                }
                Ok(cells)
            })
            .map_err(|e| RuleError {
                rule: rule.name.clone(),
                error: format!("Query execution error: {e}"),
            })?;

        for row in rows {
            match row {
                Ok(cells) => matches.push(cells),
                Err(e) => {
                    return Err(RuleError {
                        rule: rule.name.clone(),
                        error: format!("Row read error: {e}"),
                    });
                }
            }
        }

        let total = matches.len();
        Ok(RuleCheckResult {
            rule: rule.name.clone(),
            description: rule.description.clone(),
            matches,
            total,
        })
    }

    /// Execute multiple rules and collect results (successes and failures).
    ///
    /// Useful for bulk operations where partial failures are acceptable.
    pub fn check_all(conn: &Connection, rules: &[Rule]) -> Vec<Result<RuleCheckResult, RuleError>> {
        rules.iter().map(|r| Self::check(conn, r)).collect()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use duckdb::Connection;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE edges (\"from\" TEXT, \"to\" TEXT, rel TEXT)",
            params![],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO edges VALUES ('a.go', 'b.go', 'imports')",
            params![],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO edges VALUES ('b.go', 'c.go', 'imports')",
            params![],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO edges VALUES ('x.go', 'y.go', 'calls')",
            params![],
        )
        .unwrap();
        conn
    }

    #[test]
    fn rule_check_finds_matches() {
        let conn = setup_db();
        let rule = Rule {
            name: "imports".into(),
            description: "All import edges".into(),
            query: "SELECT \"from\", \"to\" FROM edges WHERE rel = 'imports'".into(),
        };
        let result = RuleEngine::check(&conn, &rule).unwrap();
        assert_eq!(result.total, 2);
        assert_eq!(result.matches.len(), 2);
        assert_eq!(result.matches[0][0], "a.go");
        assert_eq!(result.matches[0][1], "b.go");
    }

    #[test]
    fn rule_check_no_matches() {
        let conn = setup_db();
        let rule = Rule {
            name: "empty".into(),
            description: "No matches".into(),
            query: "SELECT \"from\" FROM edges WHERE rel = 'nonexistent'".into(),
        };
        let result = RuleEngine::check(&conn, &rule).unwrap();
        assert_eq!(result.total, 0);
        assert!(result.matches.is_empty());
    }

    #[test]
    fn rule_check_invalid_sql_returns_error() {
        let conn = setup_db();
        let rule = Rule {
            name: "bad_sql".into(),
            description: "This will fail".into(),
            query: "SELEC * FROM nonexistent".into(),
        };
        let err = RuleEngine::check(&conn, &rule).unwrap_err();
        assert_eq!(err.rule, "bad_sql");
        assert!(
            err.error.contains("SQL parse error") || err.error.contains("Query execution error")
        );
    }

    #[test]
    fn rule_check_nonexistent_table() {
        let conn = setup_db();
        let rule = Rule {
            name: "no_table".into(),
            description: "Table doesn't exist".into(),
            query: "SELECT * FROM nonexistent".into(),
        };
        let err = RuleEngine::check(&conn, &rule).unwrap_err();
        assert_eq!(err.rule, "no_table");
        assert!(
            err.error.contains("Query execution error") || err.error.contains("SQL parse error"),
            "Expected execution or parse error, got: {}",
            err.error
        );
    }

    #[test]
    fn rule_check_handles_integer_columns() {
        let conn = setup_db();
        let rule = Rule {
            name: "count_by_rel".into(),
            description: "Count edges by relation type".into(),
            query: "SELECT rel, COUNT(*) AS cnt FROM edges GROUP BY rel ORDER BY cnt DESC".into(),
        };
        let result = RuleEngine::check(&conn, &rule).unwrap();
        assert_eq!(result.total, 2);
        assert!(result
            .matches
            .iter()
            .any(|r| r[0] == "imports" && r[1] == "2"));
        assert!(result
            .matches
            .iter()
            .any(|r| r[0] == "calls" && r[1] == "1"));
    }

    #[test]
    fn check_all_mixed_success_and_failure() {
        let conn = setup_db();
        let rules = vec![
            Rule {
                name: "good".into(),
                description: "Works".into(),
                query: "SELECT \"from\" FROM edges LIMIT 1".into(),
            },
            Rule {
                name: "bad".into(),
                description: "Broken SQL".into(),
                query: "BROKEN SQL!!!".into(),
            },
        ];
        let results = RuleEngine::check_all(&conn, &rules);
        assert_eq!(results.len(), 2);
        assert!(results[0].is_ok());
        assert!(results[1].is_err());
    }
}
