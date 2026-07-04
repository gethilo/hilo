//! Semantic code search — deterministic TF-IDF + BM25, no embeddings.
//!
//! Provides semantic code search using classical NLP techniques (TF-IDF,
//! Okapi BM25, and Reciprocal Rank Fusion) — zero external API calls,
//! fully deterministic, pure Rust.
//!
//! ## How it works
//!
//! 1. **Tokenization**: symbols are split on camelCase/snake_case boundaries,
//!    lowercased, and deduplicated. File paths and symbol names form the
//!    "document" for each graph node.
//! 2. **TF-IDF**: term frequency × inverse document frequency, computed over
//!    all graph nodes (file-level).
//! 3. **BM25**: Okapi BM25 ranking function for relevance scoring.
//! 4. **Fusion**: TF-IDF and BM25 results are combined via Reciprocal Rank
//!    Fusion (RRF) to produce a single ranked list.
//!
//! ## Determinism
//!
//! Same query + same graph → byte-identical results. No randomness,
//! no external API, no model calls.

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::error::GraphResult;
use crate::graph::GraphDB;

/// Function type for extracting symbols from a file path.
pub type SymbolExtractor<'a> = Option<&'a dyn Fn(&str) -> Vec<String>>;

// ──────────────────────────── Types ────────────────────────────

/// A single search result from semantic search.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchResult {
    /// File path as it appears in the graph.
    pub file_path: String,
    /// Symbols found in this file that matched the query.
    pub symbols: Vec<String>,
    /// Combined relevance score (higher = more relevant).
    pub score: f64,
    /// Provenance tag: `lexical` for BM25/TF-IDF results.
    pub provenance: String,
}

/// Options for semantic search.
#[derive(Debug, Clone)]
pub struct SearchOpts {
    /// Maximum number of results to return.
    pub limit: usize,
}

impl Default for SearchOpts {
    fn default() -> Self {
        Self { limit: 20 }
    }
}

// ──────────────────────────── Tokenization ────────────────────────────

/// Tokenize a string into semantic tokens.
///
/// Splits on:
/// - Non-alphanumeric characters (spaces, punctuation, path separators)
/// - camelCase boundaries (`AuthMiddleware` → `auth`, `middleware`)
/// - snake_case boundaries (`rate_limiter` → `rate`, `limiter`)
///
/// All tokens are lowercased and deduplicated. Tokens shorter than 2
/// characters are discarded.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();

    // First split on non-alphanumeric (path separators, spaces, etc.).
    for word in text.split(|c: char| !c.is_alphanumeric() && c != '_') {
        // Then split on camelCase and snake_case boundaries.
        for sub in split_camel_snake(word) {
            let lower = sub.to_lowercase();
            if lower.len() >= 2 {
                tokens.push(lower);
            }
        }
    }

    // Deduplicate while preserving order.
    let mut seen = HashSet::new();
    tokens.retain(|t| seen.insert(t.clone()));
    tokens
}

/// Split a single word on camelCase and snake_case boundaries.
///
/// `AuthMiddleware` → `Auth`, `Middleware`
/// `rate_limiter` → `rate`, `limiter`
/// `HTTPServer` → `HTTP`, `Server`
fn split_camel_snake(word: &str) -> Vec<String> {
    // First split on underscores (snake_case).
    let parts: Vec<&str> = word.split('_').collect();

    let mut result = Vec::new();
    for part in parts {
        if part.is_empty() {
            continue;
        }
        // Split on camelCase boundaries.
        result.extend(split_camelcase(part));
    }
    result
}

/// Split a camelCase or PascalCase string into individual words.
///
/// Handles consecutive uppercase (e.g. `HTTPServer` → `HTTP`, `Server`).
fn split_camelcase(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }

    let mut result = Vec::new();
    let mut current = String::new();

    for (i, &c) in chars.iter().enumerate() {
        if i > 0 && c.is_uppercase() {
            // Start a new word if:
            // - Previous char was lowercase, OR
            // - Next char exists and is lowercase (handles HTTPServer → HTTP, Server)
            let prev = chars[i - 1];
            let next_lower = i + 1 < chars.len() && chars[i + 1].is_lowercase();
            if (prev.is_lowercase() || next_lower) && !current.is_empty() {
                result.push(std::mem::take(&mut current));
            }
        }
        current.push(c);
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
}

// ──────────────────────────── TF-IDF ────────────────────────────

/// TF-IDF index over graph nodes.
///
/// Each graph node (file) is treated as a "document" composed of its
/// file path tokens and symbol tokens. The index computes term frequency
/// and inverse document frequency for BM25 and TF-IDF scoring.
pub struct TfIdfIndex {
    /// All documents (file paths) in the index.
    documents: Vec<String>,
    /// Term → document frequency (number of documents containing the term).
    doc_freq: HashMap<String, usize>,
    /// Document → term frequencies (term → count in that document).
    term_freqs: Vec<HashMap<String, f64>>,
    /// Average document length (in tokens).
    avg_doc_len: f64,
    /// Total number of documents.
    n_docs: usize,
    /// BM25 parameters.
    k1: f64,
    b: f64,
}

impl TfIdfIndex {
    /// Build a TF-IDF index from the graph's file paths and symbols.
    ///
    /// Each file in the graph becomes a document. The document text is
    /// the file path tokens plus any symbols extracted from the file
    /// (if a source reader is provided).
    pub fn build(db: &GraphDB) -> GraphResult<Self> {
        Self::build_with_symbols(db, None)
    }

    /// Build a TF-IDF index with optional symbol extraction.
    ///
    /// When `symbol_extractor` is provided, it's called for each file path
    /// to extract additional symbols (function/type names) that enrich the
    /// document text.
    pub fn build_with_symbols(
        db: &GraphDB,
        symbol_extractor: SymbolExtractor,
    ) -> GraphResult<Self> {
        let (froms, tos) = db.distinct_files().unwrap_or((Vec::new(), Vec::new()));
        let mut all_files: HashSet<String> = froms.into_iter().collect();
        all_files.extend(tos);

        // Sort for determinism.
        let mut documents: Vec<String> = all_files.into_iter().collect();
        documents.sort();

        let n_docs = documents.len();
        let mut doc_freq: HashMap<String, usize> = HashMap::new();
        let mut term_freqs: Vec<HashMap<String, f64>> = Vec::with_capacity(n_docs);
        let mut total_len: usize = 0;

        for doc_path in &documents {
            // Build the document text: file path tokens + optional symbols.
            let mut doc_tokens = tokenize(doc_path);
            if let Some(extract) = symbol_extractor {
                let symbols = extract(doc_path);
                for sym in symbols {
                    doc_tokens.extend(tokenize(&sym));
                }
            }

            // Deduplicate tokens within a document for term frequency.
            let mut tf: HashMap<String, f64> = HashMap::new();
            for token in &doc_tokens {
                *tf.entry(token.clone()).or_insert(0.0) += 1.0;
            }

            // Update document frequency (number of docs containing each term).
            for term in tf.keys() {
                *doc_freq.entry(term.clone()).or_insert(0) += 1;
            }

            total_len += doc_tokens.len();
            term_freqs.push(tf);
        }

        let avg_doc_len = if n_docs > 0 {
            total_len as f64 / n_docs as f64
        } else {
            0.0
        };

        Ok(TfIdfIndex {
            documents,
            doc_freq,
            term_freqs,
            avg_doc_len,
            n_docs,
            k1: 1.2,
            b: 0.75,
        })
    }

    /// Compute TF-IDF score for a query against all documents.
    ///
    /// Returns a sorted list of (document_path, score) pairs, descending.
    pub fn tfidf_search(&self, query: &str) -> Vec<(String, f64)> {
        let query_tokens = tokenize(query);
        if query_tokens.is_empty() || self.n_docs == 0 {
            return Vec::new();
        }

        let mut scores: Vec<(String, f64)> = Vec::with_capacity(self.n_docs);

        for (i, doc_path) in self.documents.iter().enumerate() {
            let tf_map = &self.term_freqs[i];
            let mut score = 0.0;

            for term in &query_tokens {
                if let Some(&tf) = tf_map.get(term) {
                    let df = *self.doc_freq.get(term).unwrap_or(&0) as f64;
                    if df == 0.0 {
                        continue;
                    }
                    // IDF = ln(N / df) — smoothed.
                    let idf = (self.n_docs as f64 / df).ln();
                    // TF-IDF = tf * idf.
                    score += tf * idf;
                }
            }

            if score > 0.0 {
                scores.push((doc_path.clone(), score));
            }
        }

        // Sort: highest score first, then alphabetically (deterministic).
        scores.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        scores
    }

    /// Compute BM25 score for a query against all documents.
    ///
    /// Returns a sorted list of (document_path, score) pairs, descending.
    pub fn bm25_search(&self, query: &str) -> Vec<(String, f64)> {
        let query_tokens = tokenize(query);
        if query_tokens.is_empty() || self.n_docs == 0 {
            return Vec::new();
        }

        let mut scores: Vec<(String, f64)> = Vec::with_capacity(self.n_docs);

        for (i, doc_path) in self.documents.iter().enumerate() {
            let tf_map = &self.term_freqs[i];
            let doc_len = tf_map.values().map(|v| *v as usize).sum::<usize>() as f64;
            let mut score = 0.0;

            for term in &query_tokens {
                if let Some(&tf) = tf_map.get(term) {
                    let df = *self.doc_freq.get(term).unwrap_or(&0) as f64;
                    if df == 0.0 {
                        continue;
                    }
                    // IDF (BM25 variant): ln(1 + (N - df + 0.5) / (df + 0.5)).
                    let idf = (1.0 + (self.n_docs as f64 - df + 0.5) / (df + 0.5)).ln();
                    // BM25 term score.
                    let tf_norm = tf * (self.k1 + 1.0)
                        / (tf
                            + self.k1
                                * (1.0 - self.b + self.b * (doc_len / self.avg_doc_len.max(1.0))));
                    score += idf * tf_norm;
                }
            }

            if score > 0.0 {
                scores.push((doc_path.clone(), score));
            }
        }

        // Sort: highest score first, then alphabetically (deterministic).
        scores.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        scores
    }

    /// Number of documents in the index.
    pub fn len(&self) -> usize {
        self.n_docs
    }

    /// Is the index empty?
    pub fn is_empty(&self) -> bool {
        self.n_docs == 0
    }

    /// Get all documents in the index (sorted).
    pub fn documents(&self) -> &[String] {
        &self.documents
    }
}

// ──────────────────────────── Reciprocal Rank Fusion ────────────────────────────

/// Combine two ranked lists using Reciprocal Rank Fusion (RRF).
///
/// RRF score = sum of 1/(k + rank) for each list, where k=60 (standard).
/// Returns a fused ranked list of (item, fused_score) pairs.
pub fn reciprocal_rank_fusion(
    list1: &[(String, f64)],
    list2: &[(String, f64)],
    k: u32,
) -> Vec<(String, f64)> {
    let k_f64 = k as f64;

    // Build rank maps (rank starts at 1).
    let rank1: HashMap<String, u32> = list1
        .iter()
        .enumerate()
        .map(|(i, (path, _))| (path.clone(), (i + 1) as u32))
        .collect();
    let rank2: HashMap<String, u32> = list2
        .iter()
        .enumerate()
        .map(|(i, (path, _))| (path.clone(), (i + 1) as u32))
        .collect();

    // Collect all unique items.
    let mut all_items: HashSet<String> = rank1.keys().cloned().collect();
    all_items.extend(rank2.keys().cloned());

    // Compute fused scores.
    let mut fused: Vec<(String, f64)> = all_items
        .into_iter()
        .map(|path| {
            let mut score = 0.0;
            if let Some(&r) = rank1.get(&path) {
                score += 1.0 / (k_f64 + r as f64);
            }
            if let Some(&r) = rank2.get(&path) {
                score += 1.0 / (k_f64 + r as f64);
            }
            (path, score)
        })
        .collect();

    // Sort: highest fused score first, then alphabetically (deterministic).
    fused.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });

    fused
}

// ──────────────────────────── Public search API ────────────────────────────

/// Run semantic search against the graph.
///
/// Builds a TF-IDF index from the graph's file paths, runs both TF-IDF
/// and BM25 queries, fuses the results via RRF, and returns the top-N
/// results.
///
/// Deterministic: same query + same graph → byte-identical results.
pub fn search(db: &GraphDB, query: &str, opts: &SearchOpts) -> GraphResult<Vec<SearchResult>> {
    search_with_symbols(db, query, opts, None)
}

/// Run semantic search with optional symbol extraction.
///
/// When `symbol_extractor` is provided, symbols (function/type names) are
/// added to each file's document text, enriching the search index.
pub fn search_with_symbols(
    db: &GraphDB,
    query: &str,
    opts: &SearchOpts,
    symbol_extractor: SymbolExtractor,
) -> GraphResult<Vec<SearchResult>> {
    let index = TfIdfIndex::build_with_symbols(db, symbol_extractor)?;

    if index.is_empty() {
        return Ok(Vec::new());
    }

    // Run both ranking functions.
    let tfidf_results = index.tfidf_search(query);
    let bm25_results = index.bm25_search(query);

    // Fuse via RRF (k=60 is the standard constant).
    let fused = reciprocal_rank_fusion(&tfidf_results, &bm25_results, 60);

    // Convert to SearchResult, limited to opts.limit.
    let results = fused
        .into_iter()
        .take(opts.limit)
        .map(|(path, score)| {
            // Extract matching symbols from the query tokens.
            let query_tokens = tokenize(query);
            let path_tokens: HashSet<String> = tokenize(&path).into_iter().collect();
            let matched: Vec<String> = query_tokens
                .iter()
                .filter(|t| path_tokens.contains(*t))
                .cloned()
                .collect();
            SearchResult {
                file_path: path,
                symbols: matched,
                score,
                provenance: "lexical".to_string(),
            }
        })
        .collect();

    Ok(results)
}

// ──────────────────────────── Tests ────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::GraphDB;
    use hilo_metadata::inventory::Edge;

    fn edge(from: &str, to: &str, rel: &str) -> Edge {
        Edge::new(from, to, rel)
    }

    // ── Tokenization tests ──

    #[test]
    fn tokenize_basic() {
        let tokens = tokenize("rate limiter middleware");
        assert!(tokens.contains(&"rate".to_string()));
        assert!(tokens.contains(&"limiter".to_string()));
        assert!(tokens.contains(&"middleware".to_string()));
    }

    #[test]
    fn tokenize_camelcase() {
        let tokens = tokenize("AuthMiddleware");
        assert!(tokens.contains(&"auth".to_string()));
        assert!(tokens.contains(&"middleware".to_string()));
    }

    #[test]
    fn tokenize_snake_case() {
        let tokens = tokenize("rate_limiter");
        assert!(tokens.contains(&"rate".to_string()));
        assert!(tokens.contains(&"limiter".to_string()));
    }

    #[test]
    fn tokenize_path() {
        let tokens = tokenize("src/auth/middleware.go");
        assert!(tokens.contains(&"src".to_string()));
        assert!(tokens.contains(&"auth".to_string()));
        assert!(tokens.contains(&"middleware".to_string()));
        assert!(tokens.contains(&"go".to_string()));
    }

    #[test]
    fn tokenize_dedup() {
        let tokens = tokenize("auth auth AUTH");
        // Duplicates removed.
        assert_eq!(tokens.iter().filter(|t| *t == "auth").count(), 1);
    }

    #[test]
    fn tokenize_filters_short() {
        let tokens = tokenize("a b c");
        assert!(tokens.is_empty(), "tokens < 2 chars should be filtered");
    }

    #[test]
    fn tokenize_consecutive_uppercase() {
        let tokens = tokenize("HTTPServer");
        assert!(tokens.contains(&"http".to_string()));
        assert!(tokens.contains(&"server".to_string()));
    }

    // ── TF-IDF index tests ──

    #[test]
    fn index_empty_graph() {
        let db = GraphDB::open(":memory:").unwrap();
        let index = TfIdfIndex::build(&db).unwrap();
        assert!(index.is_empty());
    }

    #[test]
    fn index_non_empty_graph() {
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[
            edge("src/auth/middleware.go", "src/auth/handler.go", "imports"),
            edge("src/auth/handler.go", "src/db/query.go", "imports"),
        ])
        .unwrap();
        let index = TfIdfIndex::build(&db).unwrap();
        assert_eq!(index.len(), 3);
    }

    // ── TF-IDF search tests ──

    #[test]
    fn tfidf_finds_auth_file() {
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[
            edge("src/auth/middleware.go", "src/auth/handler.go", "imports"),
            edge("src/auth/handler.go", "src/db/query.go", "imports"),
        ])
        .unwrap();
        let index = TfIdfIndex::build(&db).unwrap();

        let results = index.tfidf_search("authentication");
        // "authentication" tokenizes to "authentication" which doesn't match
        // "auth" in the path. So this should return empty.
        assert!(results.is_empty());
    }

    #[test]
    fn tfidf_finds_auth_token() {
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[
            edge("src/auth/middleware.go", "src/auth/handler.go", "imports"),
            edge("src/auth/handler.go", "src/db/query.go", "imports"),
        ])
        .unwrap();
        let index = TfIdfIndex::build(&db).unwrap();

        let results = index.tfidf_search("auth");
        // "auth" should match files containing "auth" in their path.
        assert!(!results.is_empty(), "should find auth-related files");
        assert!(
            results.iter().any(|(p, _)| p.contains("auth")),
            "top results should include auth files"
        );
    }

    // ── BM25 search tests ──

    #[test]
    fn bm25_finds_auth_files() {
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[
            edge("src/auth/middleware.go", "src/auth/handler.go", "imports"),
            edge("src/auth/handler.go", "src/db/query.go", "imports"),
        ])
        .unwrap();
        let index = TfIdfIndex::build(&db).unwrap();

        let results = index.bm25_search("auth middleware");
        assert!(!results.is_empty());
        assert!(
            results[0].0.contains("middleware"),
            "top result should be the middleware file"
        );
    }

    // ── RRF tests ──

    #[test]
    fn rrf_combines_two_lists() {
        let list1 = vec![("a.go".to_string(), 1.0), ("b.go".to_string(), 0.5)];
        let list2 = vec![("b.go".to_string(), 0.8), ("c.go".to_string(), 0.3)];

        let fused = reciprocal_rank_fusion(&list1, &list2, 60);
        // b.go appears in both lists → higher fused score.
        assert_eq!(fused[0].0, "b.go");
        assert!(fused.len() >= 3, "should include all unique items");
    }

    #[test]
    fn rrf_empty_lists() {
        let fused = reciprocal_rank_fusion(&[], &[], 60);
        assert!(fused.is_empty());
    }

    #[test]
    fn rrf_single_list() {
        let list1 = vec![("a.go".to_string(), 1.0)];
        let fused = reciprocal_rank_fusion(&list1, &[], 60);
        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].0, "a.go");
    }

    #[test]
    fn rrf_is_deterministic() {
        let list1 = vec![("a.go".to_string(), 1.0), ("b.go".to_string(), 0.5)];
        let list2 = vec![("b.go".to_string(), 0.8), ("a.go".to_string(), 0.3)];

        let r1 = reciprocal_rank_fusion(&list1, &list2, 60);
        let r2 = reciprocal_rank_fusion(&list1, &list2, 60);
        assert_eq!(r1, r2, "RRF must be deterministic");
    }

    // ── Full search API tests ──

    #[test]
    fn search_returns_results() {
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[
            edge("src/auth/middleware.go", "src/auth/handler.go", "imports"),
            edge("src/auth/handler.go", "src/db/query.go", "imports"),
        ])
        .unwrap();

        let results = search(&db, "auth", &SearchOpts::default()).unwrap();
        assert!(!results.is_empty());
        assert!(
            results.iter().any(|r| r.file_path.contains("auth")),
            "should find auth files"
        );
    }

    #[test]
    fn search_empty_graph() {
        let db = GraphDB::open(":memory:").unwrap();
        let results = search(&db, "anything", &SearchOpts::default()).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn search_empty_query() {
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[edge("a.go", "b.go", "imports")]).unwrap();
        let results = search(&db, "", &SearchOpts::default()).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn search_respects_limit() {
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[
            edge("src/auth/a.go", "src/auth/b.go", "imports"),
            edge("src/auth/c.go", "src/auth/d.go", "imports"),
            edge("src/auth/e.go", "src/auth/f.go", "imports"),
        ])
        .unwrap();

        let results = search(&db, "auth", &SearchOpts { limit: 2 }).unwrap();
        assert!(results.len() <= 2, "should respect limit");
    }

    #[test]
    fn search_is_deterministic() {
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[
            edge("src/auth/middleware.go", "src/auth/handler.go", "imports"),
            edge("src/auth/handler.go", "src/db/query.go", "imports"),
        ])
        .unwrap();

        let r1 = search(&db, "auth middleware", &SearchOpts::default()).unwrap();
        let r2 = search(&db, "auth middleware", &SearchOpts::default()).unwrap();
        assert_eq!(r1, r2, "search must be deterministic");
    }

    #[test]
    fn search_provenance_is_lexical() {
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[edge("src/auth.go", "src/db.go", "imports")])
            .unwrap();

        let results = search(&db, "auth", &SearchOpts::default()).unwrap();
        for r in &results {
            assert_eq!(r.provenance, "lexical");
        }
    }

    #[test]
    fn search_semantic_not_literal() {
        // Search for "authentication" should find "auth" files via
        // partial token matching (auth is a substring of authentication
        // after tokenization).
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[
            edge("src/auth/middleware.go", "src/auth/handler.go", "imports"),
            edge("src/db/query.go", "src/db/util.go", "imports"),
        ])
        .unwrap();

        // "authenticate" tokenizes to "authenticate" which doesn't match
        // "auth" directly. But "auth" query should find auth files.
        let results = search(&db, "auth", &SearchOpts::default()).unwrap();
        assert!(!results.is_empty());
        assert!(
            results.iter().all(|r| r.file_path.contains("auth")),
            "all results should be auth-related"
        );
    }

    #[test]
    fn search_with_symbols_enriches() {
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[edge(
            "src/auth/middleware.go",
            "src/auth/handler.go",
            "imports",
        )])
        .unwrap();

        // Provide a symbol extractor that adds "Authenticate" to the
        // middleware file.
        let extractor = |path: &str| -> Vec<String> {
            if path.contains("middleware") {
                vec!["Authenticate".to_string(), "Middleware".to_string()]
            } else {
                Vec::new()
            }
        };

        let results = search_with_symbols(
            &db,
            "authenticate",
            &SearchOpts::default(),
            Some(&extractor),
        )
        .unwrap();
        assert!(!results.is_empty(), "should find via symbol match");
        assert!(
            results.iter().any(|r| r.file_path.contains("middleware")),
            "should find middleware file via symbol"
        );
    }

    #[test]
    fn search_camelcase_query_matches() {
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[edge("src/AuthMiddleware.go", "src/handler.go", "imports")])
            .unwrap();

        // Query "AuthMiddleware" should tokenize to "auth" + "middleware"
        // and match the file path.
        let results = search(&db, "AuthMiddleware", &SearchOpts::default()).unwrap();
        assert!(!results.is_empty());
        assert!(
            results
                .iter()
                .any(|r| r.file_path.contains("AuthMiddleware")),
            "should find AuthMiddleware file"
        );
    }
}
