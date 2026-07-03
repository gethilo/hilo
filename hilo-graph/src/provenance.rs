//! Provenance tracking for graph edges.
//!
//! Every edge in the Hilo graph carries a [`Provenance`] tag that indicates
//! how the edge was discovered and a `confidence` weight (0.0 – 1.0) that
//! reflects the reliability of that discovery method.
//!
//! ## Provenance levels (highest → lowest trust)
//!
//! | Level | Weight | Meaning |
//! |-------|--------|---------|
//! | `AstExact` | 1.0 | Directly extracted from the AST — ground truth |
//! | `AstInferred` | 0.8 | AST-derived but required inference (e.g. resolved path) |
//! | `Heuristic` | 0.5 | Pattern-synthesized (e.g. filename-based test association) |
//! | `Lexical` | 0.3 | Discovered by BM25 / text search |
//! | `Latent` | 0.3 | Discovered by LSA / semantic search |
//! | `Unresolved` | 0.0 | Static path ends here — dynamic dispatch target unknown |

use serde::{Deserialize, Serialize};

/// How an edge was discovered.
///
/// Serialized as a lowercase snake_case string in JSONL and DuckDB:
/// `ast_exact`, `ast_inferred`, `heuristic`, `lexical`, `latent`, `unresolved`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    /// Directly from the AST — the only ground truth (weight 1.0).
    #[default]
    AstExact,
    /// AST-derived but required inference (weight 0.8).
    AstInferred,
    /// Pattern-synthesized (weight 0.5).
    Heuristic,
    /// Discovered by BM25 / text search (weight 0.3).
    Lexical,
    /// Discovered by LSA / semantic search (weight 0.3).
    Latent,
    /// Static path ends here — dynamic dispatch (weight 0.0).
    Unresolved,
}

impl Provenance {
    /// Map each provenance level to its trust weight (0.0 – 1.0).
    pub fn trust_weight(&self) -> f64 {
        match self {
            Provenance::AstExact => 1.0,
            Provenance::AstInferred => 0.8,
            Provenance::Heuristic => 0.5,
            Provenance::Lexical => 0.3,
            Provenance::Latent => 0.3,
            Provenance::Unresolved => 0.0,
        }
    }

    /// Returns `true` only for [`Provenance::AstExact`] — the ground-truth level.
    pub fn is_ground_truth(&self) -> bool {
        matches!(self, Provenance::AstExact)
    }

    /// Convert to the lowercase snake_case string used in DuckDB and JSONL.
    pub fn as_str(&self) -> &'static str {
        match self {
            Provenance::AstExact => "ast_exact",
            Provenance::AstInferred => "ast_inferred",
            Provenance::Heuristic => "heuristic",
            Provenance::Lexical => "lexical",
            Provenance::Latent => "latent",
            Provenance::Unresolved => "unresolved",
        }
    }

    /// Parse a provenance string (case-insensitive) into a [`Provenance`].
    ///
    /// Returns `None` for unrecognized strings.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "ast_exact" => Some(Provenance::AstExact),
            "ast_inferred" => Some(Provenance::AstInferred),
            "heuristic" => Some(Provenance::Heuristic),
            "lexical" => Some(Provenance::Lexical),
            "latent" => Some(Provenance::Latent),
            "unresolved" => Some(Provenance::Unresolved),
            _ => None,
        }
    }
}

impl std::fmt::Display for Provenance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_weights() {
        assert_eq!(Provenance::AstExact.trust_weight(), 1.0);
        assert_eq!(Provenance::AstInferred.trust_weight(), 0.8);
        assert_eq!(Provenance::Heuristic.trust_weight(), 0.5);
        assert_eq!(Provenance::Lexical.trust_weight(), 0.3);
        assert_eq!(Provenance::Latent.trust_weight(), 0.3);
        assert_eq!(Provenance::Unresolved.trust_weight(), 0.0);
    }

    #[test]
    fn is_ground_truth_only_ast_exact() {
        assert!(Provenance::AstExact.is_ground_truth());
        assert!(!Provenance::AstInferred.is_ground_truth());
        assert!(!Provenance::Heuristic.is_ground_truth());
        assert!(!Provenance::Unresolved.is_ground_truth());
    }

    #[test]
    fn serde_roundtrip() {
        for p in [
            Provenance::AstExact,
            Provenance::AstInferred,
            Provenance::Heuristic,
            Provenance::Lexical,
            Provenance::Latent,
            Provenance::Unresolved,
        ] {
            let json = serde_json::to_string(&p).unwrap();
            let back: Provenance = serde_json::from_str(&json).unwrap();
            assert_eq!(p, back, "roundtrip failed for {json}");
        }
    }

    #[test]
    fn serde_snake_case_strings() {
        assert_eq!(
            serde_json::to_string(&Provenance::AstExact).unwrap(),
            "\"ast_exact\""
        );
        assert_eq!(
            serde_json::to_string(&Provenance::AstInferred).unwrap(),
            "\"ast_inferred\""
        );
        assert_eq!(
            serde_json::to_string(&Provenance::Heuristic).unwrap(),
            "\"heuristic\""
        );
        assert_eq!(
            serde_json::to_string(&Provenance::Lexical).unwrap(),
            "\"lexical\""
        );
        assert_eq!(
            serde_json::to_string(&Provenance::Latent).unwrap(),
            "\"latent\""
        );
        assert_eq!(
            serde_json::to_string(&Provenance::Unresolved).unwrap(),
            "\"unresolved\""
        );
    }

    #[test]
    fn parse_case_insensitive() {
        assert_eq!(Provenance::parse("ast_exact"), Some(Provenance::AstExact));
        assert_eq!(Provenance::parse("AST_EXACT"), Some(Provenance::AstExact));
        assert_eq!(Provenance::parse("Heuristic"), Some(Provenance::Heuristic));
        assert_eq!(Provenance::parse("unknown"), None);
    }

    #[test]
    fn default_is_ast_exact() {
        assert_eq!(Provenance::default(), Provenance::AstExact);
    }

    #[test]
    fn display_matches_as_str() {
        assert_eq!(format!("{}", Provenance::AstExact), "ast_exact");
        assert_eq!(format!("{}", Provenance::Unresolved), "unresolved");
    }
}
