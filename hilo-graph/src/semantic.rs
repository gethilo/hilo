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
use crate::resolution::LocalSpecResolver;

/// Function type for extracting symbols from a file path.
pub type SymbolExtractor<'a> = Option<&'a dyn Fn(&str) -> Vec<String>>;

/// Default symbol source for index documents that look like repo files.
///
/// GAP-077: an exact symbol query (`url_for`) can never match when the
/// document for its defining file holds only path tokens — the definition
/// name is not in the index. For every document with a known source
/// extension, read the file from disk (relative to `root`) and pull the
/// top-level definition names via the shared tree-sitter walker. Anything
/// unreadable/unparseable yields no symbols (previous behavior). Pseudo-
/// nodes (`pkg:`/`local:`) get none — GAP-079 then ranks real files above
/// import-specifier ghosts because the files now carry their definitions.
pub fn default_symbol_extractor<'a>(
    root: &'a std::path::Path,
) -> impl Fn(&str) -> Vec<String> + 'a {
    use std::cell::RefCell;
    use std::collections::HashMap;
    // Memoize per index build: a single search parses each file at most once.
    // (Tree-sitter parse of every doc is what `understand` already pays; the
    // cache keeps search-then-search-again from paying it twice.)
    let cache: RefCell<HashMap<String, Vec<String>>> = RefCell::new(HashMap::new());
    move |doc_path: &str| {
        if let Some(hit) = cache.borrow().get(doc_path) {
            return hit.clone();
        }
        let out = (|| {
            if doc_path.starts_with("pkg:") || doc_path.starts_with("local:") {
                return Vec::new();
            }
            let ext = std::path::Path::new(doc_path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            if crate::parser::Language::from_extension(ext).is_none() {
                return Vec::new();
            }
            let full = root.join(doc_path);
            let Ok(meta) = std::fs::metadata(&full) else {
                return Vec::new();
            };
            // Skip generated/minified blobs (>1MB) — definition extraction
            // on them is waste and they pollute the index.
            if meta.len() > 1_000_000 {
                return Vec::new();
            }
            let Ok(source) = std::fs::read_to_string(&full) else {
                return Vec::new();
            };
            crate::signal::extract_symbol_names_for_index(doc_path, &source)
        })();
        cache.borrow_mut().insert(doc_path.to_string(), out.clone());
        out
    }
}

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
    /// Enrich the index with file-defined symbols before matching (GAP-077).
    /// Default OFF: the first query with it ON pays one tree-sitter parse
    /// per source file (memoized within the query; cached warms don't help
    /// across processes). Callers that need exact-symbol recall — CLI
    /// interactive use, MCP `vfs_graph_search` — opt in.
    pub index_symbols: bool,
    /// Root directory the default symbol extractor reads fixture files
    /// from. `None` (the default) reads the process CWD — the historical
    /// behavior for CLI/MCP callers. Callers holding a root that is NOT
    /// the process CWD (tests with a temp dir, embedded/sandboxed hosts)
    /// set this explicitly instead of chdir'ing the process.
    pub root: Option<std::path::PathBuf>,
}

impl Default for SearchOpts {
    fn default() -> Self {
        Self {
            limit: 20,
            index_symbols: false,
            root: None,
        }
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
        // GAP-091: raw relative-import specifiers (`pkg:.config`,
        // `pkg:..json.provider`, `pkg:.`) are import STRINGS, not documents.
        // Corpora warmed by a pre-GAP-091 binary still carry them (parse-cache
        // hits re-append cached legacy lines verbatim), so the corpus is
        // filtered here rather than only at parse time. Without symbols and
        // without a resolvable file they outranked the real module file for
        // its own name (bake-off wave 6: `pkg:..config` 0.0313 vs
        // `src/flask/config.py` 0.0308).
        all_files.retain(|p| !is_relative_specifier_node(p));

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

/// Shared index→(tfidf, bm25, RRF) pipeline for `search_with_symbols`.
///
/// `early_exit` encodes the `index.is_empty()` short-circuit (an empty index
/// means no results); kept as a parameter because the empty check must run
/// INSIDE the borrowing scope where the index exists.
fn build_fused(
    db: &GraphDB,
    query: &str,
    index: &TfIdfIndex,
    early_exit: bool,
) -> GraphResult<Vec<(String, f64)>> {
    let _ = db; // reserved for future edge-aware scoring
    if early_exit {
        return Ok(Vec::new());
    }
    let tfidf_results = index.tfidf_search(query);
    let bm25_results = index.bm25_search(query);
    Ok(reciprocal_rank_fusion(&tfidf_results, &bm25_results, 60))
}

// ──────────────────────────── Public search API ────────────────────────────

/// Whether `path` is a raw relative-import specifier node (GAP-091): a `pkg:`
/// node whose module text begins with the leading dots Python relative
/// imports are written with (`pkg:.config`, `pkg:..json.provider`, bare
/// `pkg:.` / `pkg:..`). These are import strings, never documents — no file
/// resolves to them and no file can be opened through them.
///
/// Non-`pkg:` paths are never specifiers, and `pkg:` nodes with an absolute
/// module (`pkg:flask.config`) or a symbol expansion (`pkg:crate::x`) don't
/// start with the dot, so the check is a strict subset of the `pkg:` family.
pub fn is_relative_specifier_node(path: &str) -> bool {
    path.starts_with("pkg:.")
}

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

/// The relations the edge-aware pass scans. Deliberately scoped to the
/// GAP-081 service dimension — the caller-by-service recall hole the lexical
/// index cannot close — and never a lift of arbitrary edge endpoints.
const EDGE_SEARCH_RELS: [&str; 2] = ["service_call", "service_contract"];

/// A query token is `near-matched` by an edge token when the edge token
/// STARTS WITH the query token (never the reverse). This is what lets the
/// path-less caller of `ProductCatalogService` surface for the query
/// "product catalog service": its edge metadata carries `catalog`, the
/// query carries `catalogservice`, and containment in THAT direction —
/// plus `service`, which is exact — is enough. Query-side stems only
/// (`catalog` for a token of `catalogservice`), never token prefixes:
/// "ser" finding every service is the false-positive shape this avoids.
fn edge_token_near_matches(edge_token: &str, query_token: &str) -> bool {
    edge_token.starts_with(query_token)
}

/// One matched edge, folded to the (file, marker, score) a result needs.
struct EdgeHit {
    /// The repo-relative file this hit surfaces.
    file: String,
    /// The provenance marker: `edge:<rel>`.
    marker: String,
    /// The query tokens the edge matched — the hit's `symbols` (what matched,
    /// per the SearchResult contract) and, by count, its score.
    matched: Vec<String>,
}

/// Scan the graph's service-dimension edges for query-token containment and
/// fold each matching edge into at most one hit per file.
///
/// A matching edge surfaces BOTH endpoints: the caller (`from`) — the file
/// whose own path carries none of the service's tokens, which is the point —
/// and the provider/contract (`to`) when that side is not a pseudo-node.
/// Endpoints are tokenized with the SAME tokenizer as the lexical index, so
/// `edge.to = "pkg:ProductCatalogService"` contributes `product`, `catalog`,
/// `service`, while `edge.to = "src/catalogservice/server.go"` contributes
/// its path segments; the rel contributes its own words (`service_call` →
/// `service`, `call`), making the relation itself searchable metadata.
///
/// Determinism: edges come back sorted by `(from, to, rel)` from
/// `distinct_service_edges`, and files fold into a BTreeMap.
fn edge_hits(db: &GraphDB, query_tokens: &[String]) -> GraphResult<Vec<EdgeHit>> {
    let mut hits: Vec<EdgeHit> = Vec::new();
    if query_tokens.is_empty() {
        return Ok(hits);
    }
    for (from, to, rel) in db.distinct_service_edges(&EDGE_SEARCH_RELS)? {
        // Token set of the edge's metadata: rel words, both endpoints.
        let mut tokens: HashSet<String> = tokenize(&rel).into_iter().collect();
        tokens.extend(tokenize(&from));
        tokens.extend(tokenize(&to));
        let matched: Vec<String> = query_tokens
            .iter()
            .filter(|qt| tokens.iter().any(|t| edge_token_near_matches(t, qt)))
            .cloned()
            .collect();
        if matched.is_empty() {
            continue;
        }
        let marker = format!("edge:{rel}");
        hits.push(EdgeHit {
            file: from,
            marker: marker.clone(),
            matched: matched.clone(),
        });
        // A `pkg:`/`local:` target is a pseudo-node, not an openable file —
        // the same result-shape contract as the lexical pass.
        if !to.starts_with("pkg:") && !to.starts_with("local:") {
            hits.push(EdgeHit {
                file: to,
                marker,
                matched,
            });
        }
    }
    Ok(hits)
}

/// Score of one edge-aware hit: `EDGE_HIT_SCORE_PER_TOKEN` per DISTINCT
/// matched query token. Modest by construction — even a three-token match
/// stays below the smallest RRF fused score (2/61 ≈ 0.033), so edge hits
/// boost recall without ever re-ordering the lexical results above them;
/// the count still ranks a stem+`service` match above a bare rel match.
const EDGE_HIT_SCORE_PER_TOKEN: f64 = 0.01;

/// Run semantic search with optional symbol extraction.
///
/// When `symbol_extractor` is provided, symbols (function/type names) are
/// added to each file's document text, enriching the search index.
///
/// Result paths are OPENABLE files. Malformed `pkg:{` pseudo-nodes are
/// excluded (GAP-038), and TS/JS `local:` hits — raw import specifiers the
/// parser stored verbatim (`local:../pluginContainer`), which name no file
/// a user can open — are resolved to their repo-relative target files via
/// [`LocalSpecResolver`] before results are returned (GAP-072). A specifier
/// emitted from several directories may name several files; each distinct
/// target becomes its own result at the node's fused score, in
/// lexicographic order, and a target already reached as a plain file hit is
/// not duplicated (the higher-ranked occurrence stands). A specifier that
/// resolves to nothing (dangling import) is dropped. All of this runs
/// BEFORE the final `opts.limit` truncation, so dropped hits never consume
/// result budget and expansions are truncated, not silently lost.
pub fn search_with_symbols(
    db: &GraphDB,
    query: &str,
    opts: &SearchOpts,
    symbol_extractor: SymbolExtractor,
) -> GraphResult<Vec<SearchResult>> {
    // GAP-077: a bare `search()` call (CLI/MCP/understand-fallback pass None)
    // must still index file-defined symbols, else exact-symbol queries find
    // nothing. The default extractor reads from `opts.root` when set, else
    // the process cwd — matching how the rest of the CLI resolves repo
    // files; callers with a real extractor (tests, MCP sandboxing) keep
    // full control.
    // Borrow-shape: when the default extractor is needed, EVERYTHING that
    // borrows it (cwd, extractor closure, index) lives inside one scope and
    // `fused` escapes as owned data. No cross-statement borrows.
    let fused = match symbol_extractor {
        Some(f) => {
            let index = TfIdfIndex::build_with_symbols(db, Some(f))?;
            build_fused(db, query, &index, index.is_empty())?
        }
        None if opts.index_symbols => {
            let root_tmp = opts
                .root
                .clone()
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            let extracted = default_symbol_extractor(&root_tmp);
            let index = TfIdfIndex::build_with_symbols(db, Some(&extracted))?;
            build_fused(db, query, &index, index.is_empty())?
        }
        None => {
            let index = TfIdfIndex::build_with_symbols(db, None)?;
            build_fused(db, query, &index, index.is_empty())?
        }
    };

    // One resolver for the whole result pass (GAP-072): building it is a
    // single SQL scan over the `local:` edges — the same budget
    // `compute_impact` already pays per query — and it is consulted only
    // when a fused hit actually is a `local:` node.
    let local_resolver = LocalSpecResolver::from_edges(db.conn()).ok();

    // The query's tokens, matched against each result path's own tokens —
    // hoisted out of the per-hit loop (the previous code re-tokenized the
    // query for every result).
    let query_tokens = tokenize(query);

    // Map fused hits to SearchResults. Resolution and exclusion happen
    // during the mapping and the `opts.limit` truncation happens AFTER it:
    // `.take(limit)` on the fused list (the pre-GAP-072 shape) would hand
    // budget to hits that later resolve to nothing or expand to several
    // files, and the contract is that neither may distort the result set.
    let mut results: Vec<SearchResult> = Vec::new();
    let mut emitted: HashSet<String> = HashSet::new();
    for (path, score) in fused {
        if path.starts_with("pkg:{") {
            // Malformed `pkg:{` pseudo-nodes (legacy garbage from
            // unresolvable multi-name use statements, GAP-035) are not real
            // search targets (GAP-038).
            continue;
        }
        if is_relative_specifier_node(&path) {
            // Raw relative-import specifiers (GAP-091) name no openable
            // file. Dropped here too so a corpus written before the parse
            // fix cannot surface them from the fused list even if a future
            // index source re-admits them; running before truncation means
            // they never consume result budget.
            continue;
        }
        if path.starts_with("local:") {
            // A raw specifier string is not an openable file. Resolve it to
            // its target file(s): one target → the file replaces the node;
            // several targets (the same specifier emitted from different
            // directories naming different files) → one result per file,
            // lexicographic order from `targets_for_node` (determinism);
            // zero targets (dangling import) → the hit is dropped.
            if let Some(resolver) = local_resolver.as_ref() {
                for target in resolver.targets_for_node(&path) {
                    if emitted.insert(target.clone()) {
                        results.push(lexical_result(target, &query_tokens, score));
                    }
                }
            }
            continue;
        }
        if emitted.insert(path.clone()) {
            results.push(lexical_result(path, &query_tokens, score));
        }
    }

    // GAP-081-P3: the edge-aware pass. Lexical search is path(+symbol)-only,
    // so the Go caller whose path names neither the service nor the provider
    // — `src/checkoutservice/main.go`, holding a `service_call` edge to the
    // product catalog service — can never surface for the query "product
    // catalog service", even though the graph already carries the fact. The
    // pass scans ONLY the GAP-081 service rels (never arbitrary endpoints),
    // keeps the hits the lexical set has not already emitted, marks each with
    // `edge:<rel>` so a row's provenance is always stated, scores them below
    // every lexical score (see EDGE_HIT_SCORE_PER_TOKEN), and folds into the
    // SAME final truncation — added hits consume result budget only after the
    // lexical results are safe, never displacing them.
    let lexical_paths: HashSet<String> = results.iter().map(|r| r.file_path.clone()).collect();
    let mut edge_files: std::collections::BTreeMap<String, (String, Vec<String>)> =
        std::collections::BTreeMap::new();
    for hit in edge_hits(db, &query_tokens)? {
        if lexical_paths.contains(&hit.file) {
            // Already surfaced lexically — the dedupe the contract asks for;
            // the lexical row keeps its provenance and score.
            continue;
        }
        // A file can sit on several matching edges (service_call + contract);
        // the strongest rel wins by (match count, rel) so the marker is
        // deterministic. Ties resolve lexicographically (BTreeMap update rule
        // below keeps the FIRST strictly-greater entry).
        let score = hit.matched.len() as f64 * EDGE_HIT_SCORE_PER_TOKEN;
        let entry = edge_files.entry(hit.file).or_default();
        let incumbent_score = entry.1.len() as f64 * EDGE_HIT_SCORE_PER_TOKEN;
        if score > incumbent_score || (score == incumbent_score && hit.marker < entry.0) {
            *entry = (hit.marker, hit.matched);
        }
    }
    for (file, (marker, matched)) in edge_files {
        if emitted.insert(file.clone()) {
            let score = matched.len() as f64 * EDGE_HIT_SCORE_PER_TOKEN;
            results.push(SearchResult {
                file_path: file,
                symbols: matched,
                score,
                provenance: marker,
            });
        }
    }
    results.truncate(opts.limit);

    Ok(results)
}

/// Build one lexical SearchResult: the file path plus the query tokens that
/// appear in the path's own tokens (the symbol match shared by plain and
/// resolved hits).
fn lexical_result(file_path: String, query_tokens: &[String], score: f64) -> SearchResult {
    let path_tokens: HashSet<String> = tokenize(&file_path).into_iter().collect();
    let symbols: Vec<String> = query_tokens
        .iter()
        .filter(|t| path_tokens.contains(*t))
        .cloned()
        .collect();
    SearchResult {
        file_path,
        symbols,
        score,
        provenance: "lexical".to_string(),
    }
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

    // ── GAP-077: file-defined symbols searchable ──

    fn gap077_fixture() -> (tempfile::TempDir, GraphDB) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/flask")).unwrap();
        std::fs::write(
            dir.path().join("src/flask/helpers.py"),
            "def url_for(endpoint, **values):\n    return endpoint\n",
        )
        .unwrap();
        let db_path = dir.path().join("graph.db");
        let db = GraphDB::open(db_path.to_str().unwrap()).unwrap();
        db.insert_edges(&[crate::Edge::new(
            "src/flask/helpers.py",
            "pkg:jinja2",
            "imports",
        )])
        .unwrap();
        (dir, db)
    }

    #[test]
    fn search_with_symbols_finds_defining_file_by_exact_symbol() {
        let (dir, db) = gap077_fixture();
        // The default extractor reads fixture files relative to the
        // EXPLICIT root — never the process CWD (chdir is process-global
        // and races sibling tests on the parallel harness, DF-WARPFS-18).
        let opts = SearchOpts {
            limit: 10,
            index_symbols: true,
            root: Some(dir.path().to_path_buf()),
        };
        let results = search(&db, "url_for", &opts).unwrap();
        assert!(
            results
                .iter()
                .any(|r| r.file_path == "src/flask/helpers.py"),
            "exact symbol query must surface its defining file, got: {:?}",
            results
        );
    }

    #[test]
    fn search_without_symbols_keeps_path_only_behavior() {
        let (dir, db) = gap077_fixture();
        // Default: index_symbols=false — the cheap pre-GAP-077 contract.
        // Root is still explicit (defaults ignored here); symbols-off means
        // the definition file must not match by definition name.
        let results = search(
            &db,
            "url_for",
            &SearchOpts {
                root: Some(dir.path().to_path_buf()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            !results
                .iter()
                .any(|r| r.file_path == "src/flask/helpers.py"),
            "symbols-off search must not match by definition name"
        );
    }

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
    fn search_excludes_malformed_pkg_pseudo_nodes() {
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[
            edge("src/main.rs", "pkg:{\n    globset", "imports"),
            edge("src/main.rs", "pkg:globset::GlobSet", "imports"),
        ])
        .unwrap();

        let results = search(&db, "globset", &SearchOpts::default()).unwrap();
        assert!(
            results
                .iter()
                .any(|r| r.file_path == "pkg:globset::GlobSet"),
            "valid pkg nodes must still be searchable, got {:?}",
            results.iter().map(|r| &r.file_path).collect::<Vec<_>>()
        );
        assert!(
            !results.iter().any(|r| r.file_path.starts_with("pkg:{")),
            "malformed pkg:{{ pseudo-nodes must be excluded, got {:?}",
            results.iter().map(|r| &r.file_path).collect::<Vec<_>>()
        );
    }

    // ── GAP-091: relative-import specifier nodes are corpus pollution ──

    /// A legacy-shaped graph: the raw `pkg:`-relative specifiers GAP-076
    /// used to emit (`pkg:.config`, `pkg:..json`) are ALREADY in the edge
    /// store alongside the resolved nodes. Query-time exclusion must keep
    /// them out of search even before the corpus is re-warmed, because a
    /// parse-cache hit re-warm re-appends cached legacy lines verbatim.
    fn gap091_legacy_db() -> GraphDB {
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[
            edge("src/flask/config.py", "pkg:flask.sansio.app", "imports"),
            edge("src/flask/config.py", "pkg:.sansio.app", "imports"),
            edge("src/flask/__init__.py", "pkg:flask.config", "imports"),
            edge("src/flask/__init__.py", "pkg:.config", "imports"),
            edge("src/flask/sansio/app.py", "pkg:..config", "imports"),
            edge("src/flask/globals.py", "pkg:..config", "imports"),
            edge("src/flask/app.py", "pkg:.", "imports"),
        ])
        .unwrap();
        db
    }

    #[test]
    fn search_ranks_real_module_file_above_any_relative_specifier_node() {
        // The bake-off observation, as an assertion: for the query "config"
        // the real file must lead, and every `pkg:.`-relative specifier node
        // must be excluded outright. The stub extractor mirrors what the
        // CLI's symbol-indexed default does (GAP-079: definition names are
        // what let a real file out-score specifier strings that tie it on
        // path tokens alone; without symbols the absolute `pkg:flask.config`
        // legitimately ties the file and wins the alphabetical tie-break).
        let db = gap091_legacy_db();
        let opts = SearchOpts {
            limit: 10,
            ..Default::default()
        };
        let symbols = |path: &str| {
            if path == "src/flask/config.py" {
                vec!["Config".to_string()]
            } else {
                Vec::new()
            }
        };
        let results = search_with_symbols(&db, "config", &opts, Some(&symbols)).unwrap();
        assert!(!results.is_empty(), "the real config.py must still surface");
        assert_eq!(
            results[0].file_path,
            "src/flask/config.py",
            "the real file must lead its own module-name query, got: {:?}",
            results.iter().map(|r| &r.file_path).collect::<Vec<_>>()
        );
        assert!(
            results.iter().all(|r| !r.file_path.starts_with("pkg:.")
                && !r.file_path.starts_with("pkg:..")
                && r.file_path != "pkg:."),
            "no pkg:.-relative specifier node may appear in results, got: {:?}",
            results.iter().map(|r| &r.file_path).collect::<Vec<_>>()
        );
    }

    #[test]
    fn search_excludes_unresolvable_relative_specifier_entirely() {
        // `pkg:.sansio.app` names no openable file and resolves to nothing
        // the resolved node does not already carry — it must be dropped, not
        // demoted, and it must not consume result budget.
        let db = gap091_legacy_db();
        let opts = SearchOpts {
            limit: 50,
            ..Default::default()
        };
        let results = search(&db, "sansio app", &opts).unwrap();
        assert!(
            results.iter().all(|r| !r.file_path.starts_with("pkg:.")
                && !r.file_path.starts_with("pkg:..")
                && r.file_path != "pkg:."),
            "relative specifier nodes must be excluded entirely, got: {:?}",
            results.iter().map(|r| &r.file_path).collect::<Vec<_>>()
        );
        assert!(
            results
                .iter()
                .any(|r| r.file_path == "src/flask/sansio/app.py"),
            "the real file must be unaffected, got: {:?}",
            results.iter().map(|r| &r.file_path).collect::<Vec<_>>()
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

        let results = search(
            &db,
            "auth",
            &SearchOpts {
                limit: 2,
                ..Default::default()
            },
        )
        .unwrap();
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

    // ── `local:` node resolution in results (GAP-072) ──

    /// An edge whose importer is stored ABSOLUTE — the JIT-parse form
    /// (`LocalSpecResolver`'s absolute-importer path). The resolution tests
    /// need the importer's directory inside the tempdir so the resolver's
    /// `probe_existing` hits the fixture files; repo-relative importers
    /// (the `graph warm` form) would resolve against the test process's
    /// cwd and confirm nothing. The DB itself is `:memory:`: probing reads
    /// the FILES, not the database.
    fn abs_edge(root: &std::path::Path, from_rel: &str, to: &str, rel: &str) -> Edge {
        edge(root.join(from_rel).to_string_lossy().as_ref(), to, rel)
    }

    #[test]
    fn search_resolves_local_node_to_file_path() {
        let dir = tempfile::tempdir().unwrap();
        // The vite-shaped case from the board: `server/index.ts` imports
        // its sibling `./pluginContainer`, so the graph carries the raw
        // specifier node `local:./pluginContainer` — the node, not the
        // file, is what search hits look like before GAP-072. The
        // root-level importer anchors the derived repo root so the
        // resolved target is emitted repo-relative.
        let target = "src/node/server/pluginContainer.ts";
        std::fs::create_dir_all(dir.path().join("src/node/server")).unwrap();
        std::fs::write(dir.path().join(target), "export {};\n").unwrap();
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[
            abs_edge(
                dir.path(),
                "src/node/server/index.ts",
                "local:./pluginContainer",
                "imports",
            ),
            abs_edge(
                dir.path(),
                "vite.config.ts",
                "local:./src/node/server/pluginContainer",
                "imports",
            ),
        ])
        .unwrap();

        let results = search(&db, "plugin container", &SearchOpts::default()).unwrap();

        // The raw nodes must never surface as result paths...
        assert!(
            !results.iter().any(|r| r.file_path.starts_with("local:")),
            "raw local: nodes must not appear as results, got {:?}",
            results.iter().map(|r| &r.file_path).collect::<Vec<_>>()
        );
        // ...and the resolved repo-relative file must — once, regardless of
        // how many specifier forms point at it.
        let hits: Vec<&str> = results
            .iter()
            .map(|r| r.file_path.as_str())
            .filter(|p| *p == target)
            .collect();
        assert_eq!(
            hits,
            vec![target],
            "resolved target must appear exactly once, got {:?}",
            results.iter().map(|r| &r.file_path).collect::<Vec<_>>()
        );
    }

    #[test]
    fn search_expands_ambiguous_local_node_deterministically() {
        let dir = tempfile::tempdir().unwrap();
        // The SAME specifier emitted from two DIFFERENT directories names
        // two DIFFERENT files — string equality alone would call this one
        // node; only per-(node, importer) probing knows it is two targets.
        std::fs::create_dir_all(dir.path().join("a")).unwrap();
        std::fs::create_dir_all(dir.path().join("b")).unwrap();
        std::fs::write(dir.path().join("a/helper.ts"), "export {};\n").unwrap();
        std::fs::write(dir.path().join("b/helper.ts"), "export {};\n").unwrap();
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[
            abs_edge(dir.path(), "a/consumer.ts", "local:./helper", "imports"),
            abs_edge(dir.path(), "b/consumer.ts", "local:./helper", "imports"),
            // The same file ALSO reachable as a plain absolute path hit —
            // the expansion must not duplicate it (first occurrence wins).
            abs_edge(
                dir.path(),
                "root.ts",
                &dir.path().join("a/helper.ts").to_string_lossy(),
                "imports",
            ),
        ])
        .unwrap();

        let opts = SearchOpts::default();
        let results = search(&db, "helper consumer", &opts).unwrap();
        let expanded: Vec<&str> = results
            .iter()
            .map(|r| r.file_path.as_str())
            .filter(|p| *p == "a/helper.ts" || *p == "b/helper.ts")
            .collect();

        assert_eq!(
            expanded,
            vec!["a/helper.ts", "b/helper.ts"],
            "both targets must appear, lexicographically ordered (determinism)"
        );
        // One result per distinct target: the plain-path occurrence of
        // a/helper.ts must have been deduplicated, not duplicated.
        assert_eq!(
            results
                .iter()
                .filter(|r| r.file_path == "a/helper.ts")
                .count(),
            1,
            "a file reached both as plain hit and via expansion appears once"
        );

        // Same query + same graph → byte-identical output, including the
        // expansion order.
        let again = search(&db, "helper consumer", &opts).unwrap();
        assert_eq!(results, again, "expansion must be deterministic");
    }

    #[test]
    fn search_excludes_unresolvable_local_node() {
        let dir = tempfile::tempdir().unwrap();
        // A dangling import: the probe confirms no target file, so the node
        // resolves to nothing and the hit must be dropped rather than
        // surfaced as a path no user can open.
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[abs_edge(
            dir.path(),
            "src/entry.ts",
            "local:./missing",
            "imports",
        )])
        .unwrap();

        let results = search(&db, "missing", &SearchOpts::default()).unwrap();
        assert!(
            results.is_empty(),
            "unresolvable local: hit must be dropped, got {:?}",
            results.iter().map(|r| &r.file_path).collect::<Vec<_>>()
        );
    }

    #[test]
    fn search_excludes_unresolvable_local_node_without_consuming_budget() {
        let dir = tempfile::tempdir().unwrap();
        // The dangling import ranks FIRST (its specifier tokenizes to the
        // query terms exactly); a plain file ranks second. With limit 1 the
        // dropped hit must NOT eat the budget — the truncation happens
        // after resolution, so the real file survives. Deliberately NO
        // fixture file on disk: the node is truly dangling (the index does
        // not need the file to exist — only the probe does).
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[
            abs_edge(
                dir.path(),
                "src/entry.ts",
                "local:./missing_helper",
                "imports",
            ),
            abs_edge(
                dir.path(),
                "src/other.go",
                &dir.path().join("src/missing_helper.go").to_string_lossy(),
                "imports",
            ),
        ])
        .unwrap();

        let results = search(
            &db,
            "missing helper",
            &SearchOpts {
                limit: 1,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(results.len(), 1, "limit still applies");
        assert!(
            results[0].file_path.ends_with("missing_helper.go"),
            "the plain file must survive budget-wise, got {:?}",
            results[0].file_path
        );
    }

    #[test]
    fn search_leaves_plain_paths_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        // Plain absolute-path results are byte-identical whether or not the
        // graph also carries `local:` edges: the resolution pass touches
        // only `local:` hits.
        std::fs::create_dir_all(dir.path().join("a")).unwrap();
        std::fs::write(dir.path().join("a/helper.ts"), "export {};\n").unwrap();
        let helper_abs = dir.path().join("a/helper.ts").to_string_lossy().to_string();

        let db_plain = GraphDB::open(":memory:").unwrap();
        db_plain
            .insert_edges(&[abs_edge(
                dir.path(),
                "a/consumer.ts",
                &helper_abs,
                "imports",
            )])
            .unwrap();
        let results_plain = search(&db_plain, "helper", &SearchOpts::default()).unwrap();

        let db_mixed = GraphDB::open(":memory:").unwrap();
        db_mixed
            .insert_edges(&[
                abs_edge(dir.path(), "a/consumer.ts", &helper_abs, "imports"),
                abs_edge(dir.path(), "a/consumer.ts", "local:./helper", "imports"),
            ])
            .unwrap();
        let results_mixed = search(&db_mixed, "helper", &SearchOpts::default()).unwrap();

        assert!(!results_plain.is_empty());
        // Every plain hit's PATH is present, unchanged, in the mixed graph
        // (scores legitimately shift with the document count; the pass must
        // not change WHICH paths a query returns).
        let mixed_paths: std::collections::HashSet<&str> =
            results_mixed.iter().map(|r| r.file_path.as_str()).collect();
        for r in &results_plain {
            assert!(
                mixed_paths.contains(r.file_path.as_str()),
                "plain result {} must be unchanged, mixed = {:?}",
                r.file_path,
                results_mixed
                    .iter()
                    .map(|r| &r.file_path)
                    .collect::<Vec<_>>()
            );
        }
        assert!(
            results_mixed
                .iter()
                .all(|r| !r.file_path.starts_with("local:")),
            "no raw local: nodes anywhere, got {:?}",
            results_mixed
                .iter()
                .map(|r| &r.file_path)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn search_resolved_local_node_keeps_symbols() {
        let dir = tempfile::tempdir().unwrap();
        // The resolved target carries the query's tokens in its path, so
        // the symbol match is computed for the TARGET — resolution
        // enriches, it never blanks symbols.
        std::fs::create_dir_all(dir.path().join("server")).unwrap();
        std::fs::write(dir.path().join("server/pluginContainer.ts"), "export {};\n").unwrap();
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[abs_edge(
            dir.path(),
            "server/index.ts",
            "local:./pluginContainer",
            "imports",
        )])
        .unwrap();

        let results = search(&db, "plugin container", &SearchOpts::default()).unwrap();
        let resolved = results
            .iter()
            .find(|r| r.file_path == "pluginContainer.ts")
            .expect("resolved target must appear");
        assert!(
            resolved.symbols.contains(&"plugin".to_string())
                && resolved.symbols.contains(&"container".to_string()),
            "resolved hit keeps its matched symbols, got {:?}",
            resolved.symbols
        );
    }

    // ── GAP-081-P3: edge-aware results for service_call/service_contract ──

    /// The tick-189 Go gRPC shape, distilled: `src/catalogservice/server.go`
    /// registers `ProductCatalogService` (`Register<ProductCatalog>ServiceServer`,
    /// so `graph warm`'s service pass links it as the provider) and
    /// `src/checkoutservice/main.go` calls it (`New<ProductCatalog>ServiceClient`),
    /// with the `.proto` contract alongside. NOTHING in the caller's path
    /// carries the service's tokens — "product catalog service" is lexical
    /// dead air for `src/checkoutservice/main.go` — which is exactly the
    /// recall hole this pass exists for.
    fn gap081_fixture() -> GraphDB {
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[
            // Caller -> provider, as the `graph warm` service pass writes it.
            Edge {
                from: "src/checkoutservice/main.go".to_string(),
                to: "src/catalogservice/server.go".to_string(),
                rel: "service_call".to_string(),
                provenance: "grpc_ast".to_string(),
                confidence: 1.0,
            },
            // Caller -> declaring contract (phase 2a).
            Edge {
                from: "src/checkoutservice/main.go".to_string(),
                to: "protos/demo.proto".to_string(),
                rel: "service_contract".to_string(),
                provenance: "grpc_proto".to_string(),
                confidence: 1.0,
            },
            // An unrelated import edge: proves the pass is SCOPED to the
            // service rels and never lifts arbitrary edge endpoints.
            Edge::new(
                "src/checkoutservice/main.go",
                "src/checkoutservice/stdlib.go",
                "imports",
            ),
        ])
        .unwrap();
        db
    }

    #[test]
    fn search_surfaces_service_caller_by_service_stem_with_edge_marker() {
        let db = gap081_fixture();
        // Symbols OFF: the caller must surface through its edge metadata
        // alone, not through file-defined symbols.
        let opts = SearchOpts {
            limit: 10,
            index_symbols: false,
            root: None,
        };
        let results = search(&db, "product catalog service", &opts).unwrap();

        let caller = results
            .iter()
            .find(|r| r.file_path == "src/checkoutservice/main.go")
            .expect("the service_call caller must surface for its service stem");
        assert_eq!(
            caller.provenance, "edge:service_call",
            "edge-derived hits carry the edge:<rel> marker"
        );
        assert!(
            caller.symbols.contains(&"catalog".to_string()),
            "edge hits report which query tokens matched, got {:?}",
            caller.symbols
        );

        let provider = results
            .iter()
            .find(|r| r.file_path == "src/catalogservice/server.go")
            .expect("the provider file is edge metadata for the same edge");
        assert_eq!(provider.provenance, "edge:service_call");

        let contract = results
            .iter()
            .find(|r| r.file_path == "protos/demo.proto")
            .expect("the declaring contract surfaces via its endpoint tokens");
        assert_eq!(contract.provenance, "edge:service_contract");

        // The pass is scoped: the import edge's endpoint must NOT appear —
        // "stdlib.go" matches no service-edge token.
        assert!(
            !results
                .iter()
                .any(|r| r.file_path == "src/checkoutservice/stdlib.go"),
            "import-edge endpoints stay out of the edge pass, got {:?}",
            results.iter().map(|r| &r.file_path).collect::<Vec<_>>()
        );
    }

    #[test]
    fn search_service_call_query_finds_caller_via_rel_tokens() {
        let db = gap081_fixture();
        // A query of ONLY rel tokens ("service_call") still finds the caller:
        // the relation itself is searchable metadata. The caller sits on both
        // service edges, so the stronger service_call marker wins.
        let results = search(&db, "service call", &SearchOpts::default()).unwrap();
        let caller = results
            .iter()
            .find(|r| r.file_path == "src/checkoutservice/main.go")
            .expect("rel-token query must surface the caller holding the edges");
        assert_eq!(caller.provenance, "edge:service_call");
    }

    #[test]
    fn search_contract_query_marks_contract_hits() {
        let db = gap081_fixture();
        // "contract" is a token of service_contract's rel — and of nothing
        // else in the fixture — so every hit must carry the contract marker.
        let results = search(&db, "contract", &SearchOpts::default()).unwrap();
        assert!(
            !results.is_empty(),
            "the contract rel token must surface its edge endpoints"
        );
        for r in &results {
            assert_eq!(
                r.provenance, "edge:service_contract",
                "rel-token-only hits keep that rel as their marker"
            );
        }
        assert!(
            results.iter().any(|r| r.file_path == "protos/demo.proto")
                && results
                    .iter()
                    .any(|r| r.file_path == "src/checkoutservice/main.go"),
            "both endpoints of the contract edge surface"
        );
    }

    #[test]
    fn search_dedupes_edge_hits_against_lexical_hits() {
        let db = gap081_fixture();
        // "checkoutservice" is a path token, so the caller is a lexical hit
        // AND an edge hit — it must appear exactly once, with the lexical
        // provenance standing.
        let results = search(&db, "checkoutservice", &SearchOpts::default()).unwrap();
        let caller_hits: Vec<&SearchResult> = results
            .iter()
            .filter(|r| r.file_path == "src/checkoutservice/main.go")
            .collect();
        assert_eq!(
            caller_hits.len(),
            1,
            "a file reached both lexically and via edges appears once"
        );
        assert_eq!(
            caller_hits[0].provenance, "lexical",
            "lexical provenance wins the dedupe"
        );
    }

    #[test]
    fn search_edge_pass_stays_silent_when_nothing_matches() {
        let db = gap081_fixture();
        // No path token, no edge token: empty, never a fabricated hit.
        let results = search(&db, "zigdatabase", &SearchOpts::default()).unwrap();
        assert!(
            results.is_empty(),
            "a query matching neither paths nor edge metadata stays empty, got {:?}",
            results.iter().map(|r| &r.file_path).collect::<Vec<_>>()
        );
    }

    #[test]
    fn search_edge_hits_do_not_displace_lexical_results_from_the_limit() {
        let db = gap081_fixture();
        // The caller (lexical 1.0 + edge boost) out-ranks everything; with
        // limit 1 exactly it survives, and the edge pass may not push the
        // total past the limit.
        let results = search(
            &db,
            "checkoutservice",
            &SearchOpts {
                limit: 1,
                index_symbols: false,
                root: None,
            },
        )
        .unwrap();
        assert_eq!(results.len(), 1, "limit caps the merged result set");
        assert_eq!(results[0].file_path, "src/checkoutservice/main.go");
    }

    #[test]
    fn search_edge_hits_are_deterministic() {
        let db = gap081_fixture();
        let opts = SearchOpts {
            limit: 10,
            index_symbols: false,
            root: None,
        };
        let r1 = search(&db, "product catalog service", &opts).unwrap();
        let r2 = search(&db, "product catalog service", &opts).unwrap();
        assert_eq!(r1, r2, "edge-aware search must be deterministic");
    }
}
