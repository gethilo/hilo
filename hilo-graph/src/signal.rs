//! Signal engine — harmonic multi-resolution context compression.
//!
//! Produces budgeted, tiered output from the dependency graph so agents get
//! the *shape* of the code first (MAP), then the *spine* (SIGNATURES), and
//! finally the *exact lines* (DETAIL) — position-ordered to beat the
//! "lost in the middle" attention problem.
//!
//! ## Tiers
//!
//! | Tier | Budget | Content |
//! |------|--------|---------|
//! | MAP | 15% | `{ file: [symbols…] }` — orientation |
//! | SIGNATURES | 25% | `file:line  fn foo(x: i32) -> bool` — spine |
//! | DETAIL | 60% | whitespace-minified source blocks — exact lines |
//!
//! ## Position ordering
//!
//! Highest-signal files are placed at the **edges** of the output (first and
//! last), lower-signal files in the **middle**. This exploits the empirical
//! finding that attention-limited models attend more to the beginning and end
//! of context windows.
//!
//! ## Determinism
//!
//! The engine is fully deterministic: same task + same graph → byte-identical
//! text output. No randomness, no model calls, no external API.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::error::GraphResult;
use crate::graph::{Direction, GraphDB};
use crate::parser::{Language, Parser};
use crate::provenance::Provenance;

// ──────────────────────────── Types ────────────────────────────

/// Output resolution strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    /// MAP → SIGNATURES → DETAIL (default, 3-tier).
    #[default]
    Harmonic,
    /// Single-tier flat dump (all DETAIL).
    Flat,
}

/// Which tier a file was assigned to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Orientation: file → symbol list.
    Map,
    /// Spine: one-line signatures.
    Signature,
    /// Exact source lines (whitespace-minified).
    Detail,
}

/// A symbol signature extracted from a source file.
#[derive(Debug, Clone, Serialize)]
pub struct SymbolSignature {
    /// Symbol name (function/type/class).
    pub name: String,
    /// Line number (1-indexed).
    pub line: usize,
    /// One-line signature text.
    pub signature: String,
}

/// A single file in the signal result.
#[derive(Debug, Clone, Serialize)]
pub struct SignalFile {
    /// File path as it appears in the graph.
    pub path: String,
    /// Key symbols extracted from this file (function/type names).
    pub symbols: Vec<String>,
    /// Full symbol signatures with line numbers (for SIGNATURES tier).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub signatures: Vec<SymbolSignature>,
    /// Which tier this file was assigned.
    pub tier: Tier,
    /// Provenance of the strongest edge connecting this file to an anchor.
    pub provenance: String,
    /// Signal score (0.0 – 1.0). Higher = more relevant.
    pub signal_score: f64,
    /// Whitespace-minified source for `Detail` tier files, `None` otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Options controlling the signal engine output.
#[derive(Debug, Clone)]
pub struct SignalOpts {
    /// Approximate token budget (1 token ≈ 4 chars). Default 6000.
    pub token_budget: usize,
    /// Maximum anchor/seed files. Default 8.
    pub seed_limit: usize,
    /// Graph traversal depth from anchors. Default 2.
    pub depth: usize,
    /// Maximum total files in output. Default 60.
    pub max_nodes: usize,
    /// Harmonic (3-tier) or Flat (single-tier).
    pub resolution: Resolution,
}

impl Default for SignalOpts {
    fn default() -> Self {
        SignalOpts {
            token_budget: 6000,
            seed_limit: 8,
            depth: 2,
            max_nodes: 60,
            resolution: Resolution::Harmonic,
        }
    }
}

/// Result of a signal engine `understand()` call.
#[derive(Debug, Clone, Serialize)]
pub struct SignalResult {
    /// Formatted multi-tier text output.
    pub text: String,
    /// Machine-readable file list.
    pub files: Vec<SignalFile>,
    /// Estimated token count (chars / 4).
    pub tokens_estimate: usize,
    /// Anchor/seed file paths.
    pub anchors: Vec<String>,
}

// ──────────────────────────── Public API ────────────────────────────

/// Run the signal engine against a `GraphDB`.
///
/// `task` is a natural-language description of what the agent is trying to
/// do (e.g. "rate limiter middleware"). The engine tokenizes it and matches
/// against file paths and symbols to find anchor files, then traverses the
/// graph to collect related files.
///
/// The output is deterministic: same task + same graph state → identical text.
pub fn understand(db: &GraphDB, task: &str, opts: &SignalOpts) -> GraphResult<SignalResult> {
    understand_with_source::<fn(&str) -> Option<String>>(db, task, opts, None)
}

/// Run the signal engine with an optional source-file resolver.
///
/// `source_reader` is a closure that returns file contents for a given path.
/// When `None`, files are read from disk via `std::fs::read_to_string`.
/// In tests, a custom reader allows in-memory fixtures without disk I/O.
pub fn understand_with_source<F>(
    db: &GraphDB,
    task: &str,
    opts: &SignalOpts,
    source_reader: Option<F>,
) -> GraphResult<SignalResult>
where
    F: Fn(&str) -> Option<String>,
{
    // 1. Discover anchors by matching task tokens against file paths.
    let anchors = discover_anchors(db, task, opts.seed_limit);

    // 2. Traverse the graph from anchors to collect the file set.
    let file_scores = traverse_and_score(db, &anchors, opts.depth, opts.max_nodes);

    // 3. Sort files by score descending (deterministic — BTreeMap breaks ties).
    let mut sorted_files: Vec<(String, f64, String)> = file_scores
        .into_iter()
        .map(|(path, (score, prov))| (path, score, prov))
        .collect();
    sorted_files.sort_by(|a, b| {
        // Primary: score descending. Secondary: path ascending (deterministic).
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });

    // 4. Extract symbols and assign tiers.
    let signal_files = build_signal_files(&sorted_files, &anchors, opts, source_reader)?;

    // 5. Format output text.
    let text = format_output(&signal_files, &anchors, opts);

    let tokens_estimate = text.len() / 4;

    Ok(SignalResult {
        text,
        files: signal_files,
        tokens_estimate,
        anchors,
    })
}

// ──────────────────────────── Anchor discovery ────────────────────────────

/// Tokenize the task string and match against file paths in the graph.
///
/// Tokenization: lowercase, split on non-alphanumeric, filter tokens ≥ 3 chars.
/// Matching: a file path matches if any token appears as a substring of the
/// path (case-insensitive). Files are ranked by match count, then alphabetically.
fn discover_anchors(db: &GraphDB, task: &str, seed_limit: usize) -> Vec<String> {
    let tokens = tokenize_task(task);
    if tokens.is_empty() {
        return Vec::new();
    }

    // Collect all file paths from the graph.
    let (froms, tos) = db.distinct_files().unwrap_or((Vec::new(), Vec::new()));
    let mut all_files: HashSet<String> = froms.into_iter().collect();
    all_files.extend(tos);

    // Score each file by how many task tokens it matches.
    let mut scored: Vec<(String, usize)> = all_files
        .into_iter()
        .map(|path| {
            let lower = path.to_lowercase();
            let matches = tokens.iter().filter(|t| lower.contains(t.as_str())).count();
            (path, matches)
        })
        .filter(|(_, m)| *m > 0)
        .collect();

    // Sort: most matches first, then alphabetically (deterministic).
    scored.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    scored
        .into_iter()
        .take(seed_limit)
        .map(|(path, _)| path)
        .collect()
}

/// Tokenize a task string for anchor matching.
///
/// Splits on non-alphanumeric boundaries, lowercases, and filters tokens
/// shorter than 3 characters (to avoid matching "go", "rs", etc.).
fn tokenize_task(task: &str) -> Vec<String> {
    task.split(|c: char| !c.is_alphanumeric())
        .map(|s| s.to_lowercase())
        .filter(|s| s.len() >= 3)
        .collect::<Vec<_>>()
}

// ──────────────────────────── Graph traversal ────────────────────────────

/// Traverse the graph from anchor files, scoring each discovered file.
///
/// Returns a map of `path → (score, provenance)` where:
/// - Anchors get score 1.0
/// - Direct neighbors get score = provenance_weight * 0.8
/// - 2-hop neighbors get score = provenance_weight * 0.5
/// - Deeper files get score = provenance_weight * 0.3 / depth
fn traverse_and_score(
    db: &GraphDB,
    anchors: &[String],
    max_depth: usize,
    max_nodes: usize,
) -> HashMap<String, (f64, String)> {
    let mut scores: HashMap<String, (f64, String)> = HashMap::new();

    // Anchors: score 1.0, provenance "ast_exact".
    for anchor in anchors {
        scores.insert(anchor.clone(), (1.0, "ast_exact".to_string()));
    }

    // BFS from each anchor.
    for anchor in anchors {
        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(anchor.clone());

        let mut frontier: Vec<(String, usize)> = vec![(anchor.clone(), 0)];

        while let Some((path, depth)) = frontier.pop() {
            if depth >= max_depth {
                continue;
            }

            // Forward edges: what does this file import?
            let edges = db
                .related(&path, None, Direction::Forward)
                .unwrap_or_default();
            // Reverse edges: what imports this file?
            let reverse_edges = db
                .related(&path, None, Direction::Reverse)
                .unwrap_or_default();

            for edge in edges.iter().chain(reverse_edges.iter()) {
                let neighbor = if edge.from == path {
                    edge.to.clone()
                } else {
                    edge.from.clone()
                };

                if visited.insert(neighbor.clone()) {
                    let prov = Provenance::parse(&edge.provenance).unwrap_or(Provenance::AstExact);
                    let weight = prov.trust_weight();
                    let depth_factor = match depth + 1 {
                        1 => 0.8,
                        2 => 0.5,
                        _ => 0.3 / (depth as f64),
                    };
                    let new_score = weight * depth_factor;

                    // Keep the highest score (deterministic — first writer wins ties).
                    scores
                        .entry(neighbor.clone())
                        .and_modify(|(s, p)| {
                            if new_score > *s {
                                *s = new_score;
                                *p = edge.provenance.clone();
                            }
                        })
                        .or_insert((new_score, edge.provenance.clone()));

                    frontier.push((neighbor, depth + 1));
                }
            }
        }
    }

    // Cap at max_nodes: keep the highest-scoring files.
    if scores.len() > max_nodes {
        let mut all: Vec<(String, f64, String)> = scores
            .iter()
            .map(|(p, (s, prov))| (p.clone(), *s, prov.clone()))
            .collect();
        all.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        all.truncate(max_nodes);
        scores = all.into_iter().map(|(p, s, prov)| (p, (s, prov))).collect();
    }

    scores
}

// ──────────────────────────── Symbol extraction ────────────────────────────

/// A symbol extracted from a source file: name + line number + signature.
#[derive(Debug, Clone)]
struct Symbol {
    name: String,
    line: usize,
    signature: String,
}

/// Extract symbols (functions, types, classes) from a source file.
///
/// Uses tree-sitter to parse the file and extract top-level definitions.
/// Falls back to an empty list if the file can't be parsed.
fn extract_symbols(path: &str, source: &str) -> Vec<Symbol> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    let lang = match Language::from_extension(ext) {
        Some(l) => l,
        None => return Vec::new(),
    };

    let mut parser = match Parser::for_language(lang) {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };

    parser.parse_imports(path, source).ok(); // Warm up the parser

    // Use tree-sitter directly for symbol extraction.
    let mut ts_parser = tree_sitter::Parser::new();
    let ts_lang = match lang {
        Language::Go => tree_sitter_go::LANGUAGE.into(),
        Language::Python => tree_sitter_python::LANGUAGE.into(),
        Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Language::Rust => tree_sitter_rust::LANGUAGE.into(),
        Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        Language::Java => tree_sitter_java::LANGUAGE.into(),
        Language::C => tree_sitter_c::LANGUAGE.into(),
        Language::Cpp => tree_sitter_cpp::LANGUAGE.into(),
        Language::Ruby => tree_sitter_ruby::LANGUAGE.into(),
    };
    if ts_parser.set_language(&ts_lang).is_err() {
        return Vec::new();
    }

    let tree = match ts_parser.parse(source.as_bytes(), None) {
        Some(t) => t,
        None => return Vec::new(),
    };

    extract_symbols_from_ast(tree.root_node(), source.as_bytes(), lang)
}

/// Walk the AST and extract symbol definitions.
fn extract_symbols_from_ast(node: tree_sitter::Node, source: &[u8], lang: Language) -> Vec<Symbol> {
    let mut symbols = Vec::new();

    match lang {
        Language::Go => {
            collect_symbols(
                node,
                source,
                &mut symbols,
                &[
                    "function_declaration",
                    "type_declaration",
                    "method_declaration",
                ],
                extract_go_signature,
            );
        }
        Language::Rust => {
            collect_symbols(
                node,
                source,
                &mut symbols,
                &[
                    "function_item",
                    "struct_item",
                    "enum_item",
                    "trait_item",
                    "impl_item",
                    "macro_definition",
                ],
                extract_rust_signature,
            );
        }
        Language::Python => {
            collect_symbols(
                node,
                source,
                &mut symbols,
                &["function_definition", "class_definition"],
                extract_python_signature,
            );
        }
        Language::TypeScript | Language::JavaScript => {
            collect_symbols(
                node,
                source,
                &mut symbols,
                &[
                    "function_declaration",
                    "class_declaration",
                    "method_definition",
                    "interface_declaration",
                    "type_alias_declaration",
                ],
                extract_js_signature,
            );
        }
        Language::Java => {
            collect_symbols(
                node,
                source,
                &mut symbols,
                &[
                    "method_declaration",
                    "class_declaration",
                    "interface_declaration",
                ],
                extract_java_signature,
            );
        }
        Language::C | Language::Cpp => {
            collect_symbols(
                node,
                source,
                &mut symbols,
                &[
                    "function_definition",
                    "function_declaration",
                    "struct_specifier",
                ],
                extract_c_signature,
            );
        }
        Language::Ruby => {
            collect_symbols(
                node,
                source,
                &mut symbols,
                &["method", "class", "module"],
                extract_ruby_signature,
            );
        }
    }

    symbols
}

/// Recursively collect symbols from AST nodes matching `kinds`.
fn collect_symbols(
    node: tree_sitter::Node,
    source: &[u8],
    symbols: &mut Vec<Symbol>,
    kinds: &[&str],
    extractor: fn(tree_sitter::Node, &[u8]) -> Option<Symbol>,
) {
    if kinds.contains(&node.kind()) {
        if let Some(sym) = extractor(node, source) {
            symbols.push(sym);
        }
    }

    let children: Vec<tree_sitter::Node> = {
        let mut cursor = node.walk();
        node.children(&mut cursor).collect()
    };

    for child in children {
        collect_symbols(child, source, symbols, kinds, extractor);
    }
}

// ── Per-language signature extractors ──────────────────────────

fn extract_go_signature(node: tree_sitter::Node, source: &[u8]) -> Option<Symbol> {
    let text = node.utf8_text(source).ok()?;
    let line = node.start_position().row + 1;

    // Extract the first line as the signature.
    let first_line = text.lines().next().unwrap_or(text);
    let name = extract_identifier(first_line, "func", "type")
        .or_else(|| extract_identifier(first_line, "func", "func"))
        .unwrap_or_else(|| first_line.trim().to_string());

    Some(Symbol {
        name,
        line,
        signature: first_line.trim().to_string(),
    })
}

fn extract_rust_signature(node: tree_sitter::Node, source: &[u8]) -> Option<Symbol> {
    let text = node.utf8_text(source).ok()?;
    let line = node.start_position().row + 1;
    let first_line = text.lines().next().unwrap_or(text);
    let trimmed = first_line.trim();

    // Extract the symbol name from the signature line.
    // Handles: `fn foo`, `pub fn foo`, `pub async fn foo`, `struct Foo`,
    // `pub struct Foo`, `enum Foo`, `trait Foo`, `impl Foo`, `macro_rules! foo`
    let name = extract_rust_name(trimmed).unwrap_or_else(|| trimmed.to_string());

    Some(Symbol {
        name,
        line,
        signature: trimmed.to_string(),
    })
}

/// Extract the identifier name from a Rust definition line.
fn extract_rust_name(line: &str) -> Option<String> {
    // Handle macro_rules! separately.
    if let Some(rest) = line.strip_prefix("macro_rules!") {
        return Some(
            rest.trim_start()
                .trim_end_matches(['{', '('])
                .trim()
                .to_string(),
        );
    }

    // Skip visibility modifiers and qualifiers: pub, pub(crate), async, unsafe, etc.
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let mut idx = 0;

    // Skip `pub` and `pub(...)` tokens.
    while idx < tokens.len() && (tokens[idx] == "pub" || tokens[idx].starts_with("pub(")) {
        idx += 1;
    }
    // Skip `async`, `unsafe`, `const`, `extern`.
    while idx < tokens.len() && matches!(tokens[idx], "async" | "unsafe" | "const" | "extern") {
        idx += 1;
    }

    // Now tokens[idx] should be the keyword: fn, struct, enum, trait, impl, type.
    if idx >= tokens.len() {
        return None;
    }
    let _keyword = tokens[idx];
    idx += 1;

    if idx >= tokens.len() {
        return None;
    }

    // The name is the next token, with trailing punctuation removed.
    let name = tokens[idx]
        .trim_end_matches(['(', '{', '<', '='])
        .to_string();

    if name.is_empty() || name == "fn" || name == "struct" || name == "enum" {
        return None;
    }

    Some(name)
}

fn extract_python_signature(node: tree_sitter::Node, source: &[u8]) -> Option<Symbol> {
    let text = node.utf8_text(source).ok()?;
    let line = node.start_position().row + 1;
    let first_line = text.lines().next().unwrap_or(text);
    let trimmed = first_line.trim();

    let name = trimmed
        .split_whitespace()
        .nth(1)
        .unwrap_or(trimmed)
        .trim_end_matches('(')
        .to_string();

    Some(Symbol {
        name,
        line,
        signature: trimmed.to_string(),
    })
}

fn extract_js_signature(node: tree_sitter::Node, source: &[u8]) -> Option<Symbol> {
    let text = node.utf8_text(source).ok()?;
    let line = node.start_position().row + 1;
    let first_line = text.lines().next().unwrap_or(text);
    let trimmed = first_line.trim();

    let name = trimmed
        .split_whitespace()
        .next_back()
        .unwrap_or(trimmed)
        .trim_end_matches(['{', '('])
        .to_string();

    Some(Symbol {
        name,
        line,
        signature: trimmed.to_string(),
    })
}

fn extract_java_signature(node: tree_sitter::Node, source: &[u8]) -> Option<Symbol> {
    let text = node.utf8_text(source).ok()?;
    let line = node.start_position().row + 1;
    let first_line = text.lines().next().unwrap_or(text);
    let trimmed = first_line.trim();

    let name = trimmed
        .split_whitespace()
        .rfind(|s| !s.ends_with(':') && !s.is_empty())
        .unwrap_or(trimmed)
        .trim_end_matches(['{', '('])
        .to_string();

    Some(Symbol {
        name,
        line,
        signature: trimmed.to_string(),
    })
}

fn extract_c_signature(node: tree_sitter::Node, source: &[u8]) -> Option<Symbol> {
    let text = node.utf8_text(source).ok()?;
    let line = node.start_position().row + 1;
    let first_line = text.lines().next().unwrap_or(text);
    let trimmed = first_line.trim();

    let name = trimmed
        .split_whitespace()
        .next_back()
        .unwrap_or(trimmed)
        .trim_end_matches(['{', '(', ';'])
        .to_string();

    Some(Symbol {
        name,
        line,
        signature: trimmed.to_string(),
    })
}

fn extract_ruby_signature(node: tree_sitter::Node, source: &[u8]) -> Option<Symbol> {
    let text = node.utf8_text(source).ok()?;
    let line = node.start_position().row + 1;
    let first_line = text.lines().next().unwrap_or(text);
    let trimmed = first_line.trim();

    let name = trimmed
        .split_whitespace()
        .next_back()
        .unwrap_or(trimmed)
        .trim_end_matches('{')
        .to_string();

    Some(Symbol {
        name,
        line,
        signature: trimmed.to_string(),
    })
}

/// Extract an identifier after a keyword (e.g. "func Foo" → "Foo").
fn extract_identifier(line: &str, _kw1: &str, kw2: &str) -> Option<String> {
    let trimmed = line.trim();
    if let Some(after) = trimmed.strip_prefix(kw2) {
        let id = after.trim_start().split(['(', '{', ' ', '<']).next()?;
        if !id.is_empty() {
            return Some(id.to_string());
        }
    }
    None
}

// ──────────────────────────── Tier assignment & formatting ────────────────────────────

/// Build `SignalFile` entries from sorted file list, extracting symbols
/// and assigning tiers based on score.
fn build_signal_files<F>(
    sorted: &[(String, f64, String)],
    anchors: &[String],
    opts: &SignalOpts,
    source_reader: Option<F>,
) -> GraphResult<Vec<SignalFile>>
where
    F: Fn(&str) -> Option<String>,
{
    let anchor_set: HashSet<&str> = anchors.iter().map(|s| s.as_str()).collect();
    let n = sorted.len();

    // Tier assignment:
    // - Top `seed_limit` files → Detail (get source)
    // - Next `seed_limit` files → Signature
    // - Remaining → Map
    let detail_count = opts.seed_limit.min(n);
    let signature_count = (opts.seed_limit * 2).min(n);

    let mut files = Vec::with_capacity(n);

    for (i, (path, score, prov)) in sorted.iter().enumerate() {
        let tier = if opts.resolution == Resolution::Flat || i < detail_count {
            Tier::Detail
        } else if i < signature_count {
            Tier::Signature
        } else {
            Tier::Map
        };

        // Read source for symbol extraction and detail.
        let source = if tier == Tier::Detail
            || tier == Tier::Signature
            || anchor_set.contains(path.as_str())
        {
            if let Some(ref reader) = source_reader {
                reader(path)
            } else {
                std::fs::read_to_string(path).ok()
            }
        } else {
            None
        };

        let raw_symbols = if let Some(ref src) = source {
            extract_symbols(path, src)
        } else {
            Vec::new()
        };

        // Cap at 8 symbols per file for MAP tier.
        let symbols: Vec<String> = raw_symbols.iter().take(8).map(|s| s.name.clone()).collect();

        // Full signatures for SIGNATURES and DETAIL tiers.
        let signatures: Vec<SymbolSignature> = if tier == Tier::Signature || tier == Tier::Detail {
            raw_symbols
                .iter()
                .take(8)
                .map(|s| SymbolSignature {
                    name: s.name.clone(),
                    line: s.line,
                    signature: s.signature.clone(),
                })
                .collect()
        } else {
            Vec::new()
        };

        let detail = if tier == Tier::Detail {
            source.as_ref().map(|s| minify_whitespace(s))
        } else {
            None
        };

        files.push(SignalFile {
            path: path.clone(),
            symbols,
            signatures,
            tier,
            provenance: prov.clone(),
            signal_score: *score,
            detail,
        });
    }

    Ok(files)
}

/// Minify whitespace: dedent to 0, remove blank lines, collapse trailing
/// whitespace. Line-number gaps signal elision to the reader.
fn minify_whitespace(source: &str) -> String {
    let lines: Vec<&str> = source.lines().collect();

    // Find minimum indentation across non-blank lines.
    let min_indent = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);

    let mut result = Vec::with_capacity(lines.len());
    for line in &lines {
        let trimmed = if line.len() >= min_indent {
            &line[min_indent..]
        } else {
            line
        };
        // Skip blank lines.
        if trimmed.trim().is_empty() {
            continue;
        }
        // Trim trailing whitespace.
        result.push(trimmed.trim_end());
    }

    result.join("\n")
}

/// Format the full output text with MAP → SIGNATURES → DETAIL sections.
///
/// Files are position-ordered: highest-signal at the edges (first and last),
/// lower-signal in the middle. This beats "lost in the middle" for
/// attention-limited models.
fn format_output(files: &[SignalFile], anchors: &[String], opts: &SignalOpts) -> String {
    if files.is_empty() {
        return format!(
            "## MAP\nNo files matched task: {:?}\n\n(No anchors found — try a different task description.)\n",
            anchors
        );
    }

    if opts.resolution == Resolution::Flat {
        return format_flat(files);
    }

    // Position-ordered file list for DETAIL tier.
    // Highest-signal files at edges (first and last), lower-signal in middle.
    let detail_files: Vec<&SignalFile> = files.iter().filter(|f| f.tier == Tier::Detail).collect();
    let position_ordered = position_order(&detail_files);

    let mut output = String::with_capacity(opts.token_budget * 6);

    // ── MAP tier (15% budget) ──
    output.push_str("## MAP\n");
    output.push_str("<file> → <key symbols>\n\n");

    // Group by file, sorted by score then path (deterministic).
    let map_files: BTreeMap<&str, &SignalFile> =
        files.iter().map(|f| (f.path.as_str(), f)).collect();
    for (path, sf) in &map_files {
        let sym_list = if sf.symbols.is_empty() {
            "(no symbols extracted)".to_string()
        } else {
            sf.symbols.join(", ")
        };
        output.push_str(&format!("{path} → {sym_list}\n"));
    }
    output.push('\n');

    // ── SIGNATURES tier (25% budget) ──
    output.push_str("## SIGNATURES\n");
    output.push_str("<file>:<line>  <signature>\n\n");

    let sig_files: Vec<&SignalFile> = files
        .iter()
        .filter(|f| f.tier == Tier::Signature || f.tier == Tier::Detail)
        .collect();

    for sf in &sig_files {
        if sf.signatures.is_empty() {
            output.push_str(&format!("{}:0  (no symbols)\n", sf.path));
        } else {
            for sig in &sf.signatures {
                output.push_str(&format!("{}:{}  {}\n", sf.path, sig.line, sig.signature));
            }
        }
    }
    output.push('\n');

    // ── DETAIL tier (60% budget) ──
    output.push_str("## DETAIL\n");
    output.push_str("<file> [provenance=…, score=…]\n<source (whitespace-minified)>\n\n");

    let detail_budget_chars = opts.token_budget * 4 * 60 / 100;
    let mut used = 0usize;

    for sf in &position_ordered {
        if let Some(ref detail) = sf.detail {
            let header = format!(
                "{} [provenance={}, score={:.2}]\n",
                sf.path, sf.provenance, sf.signal_score
            );
            let block = format!("{header}{detail}\n\n");

            if used + block.len() > detail_budget_chars && !position_ordered.is_empty() {
                // Budget exhausted — stop adding detail blocks.
                break;
            }

            output.push_str(&block);
            used += block.len();
        }
    }

    output
}

/// Position-order files: highest-signal at the edges, lower-signal in the middle.
///
/// Given a slice sorted by score descending, interleave from front and back
/// to place the highest-scoring files at positions 0, N-1, 2, N-3, …
fn position_order<'a>(files: &'a [&'a SignalFile]) -> Vec<&'a SignalFile> {
    if files.len() <= 2 {
        return files.to_vec();
    }

    let mut result = Vec::with_capacity(files.len());
    let mut left = 0;
    let mut right = files.len();
    let mut take_front = true;

    while left < right {
        if take_front {
            result.push(files[left]);
            left += 1;
        } else {
            result.push(files[right - 1]);
            right -= 1;
        }
        take_front = !take_front;
    }

    result
}

/// Flat resolution: all files in DETAIL tier, no MAP/SIGNATURES sections.
fn format_flat(files: &[SignalFile]) -> String {
    let mut output = String::new();
    output.push_str("## DETAIL (flat)\n\n");

    for sf in files {
        let header = format!(
            "{} [provenance={}, score={:.2}]\n",
            sf.path, sf.provenance, sf.signal_score
        );
        if let Some(ref detail) = sf.detail {
            output.push_str(&format!("{header}{detail}\n\n"));
        } else {
            output.push_str(&format!("{header}(source unavailable)\n\n"));
        }
    }

    output
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

    #[test]
    fn tokenize_basic() {
        let tokens = tokenize_task("rate limiter middleware");
        assert_eq!(tokens, vec!["rate", "limiter", "middleware"]);
    }

    #[test]
    fn tokenize_filters_short() {
        let tokens = tokenize_task("go to db");
        assert_eq!(tokens, Vec::<String>::new()); // "go", "to", "db" are all < 3 chars
    }

    #[test]
    fn minify_dedents_and_strips_blanks() {
        let src = "    fn foo() {\n        let x = 1;\n\n        x\n    }\n";
        let minified = minify_whitespace(src);
        // Dedent by 4 (min indent), so 8-space lines become 4-space.
        assert!(!minified.contains("\n\n"), "blank lines should be removed");
        assert!(minified.contains("fn foo()"));
        assert!(minified.contains("let x = 1;"));
        // No leading whitespace on the first line (dedented).
        assert!(
            minified.starts_with("fn foo()"),
            "first line should be dedented to zero"
        );
    }

    #[test]
    fn position_order_places_high_score_at_edges() {
        let files = vec![
            SignalFile {
                path: "a".into(),
                symbols: vec![],
                signatures: vec![],
                tier: Tier::Detail,
                provenance: "ast_exact".into(),
                signal_score: 1.0,
                detail: None,
            },
            SignalFile {
                path: "b".into(),
                symbols: vec![],
                signatures: vec![],
                tier: Tier::Detail,
                provenance: "ast_exact".into(),
                signal_score: 0.8,
                detail: None,
            },
            SignalFile {
                path: "c".into(),
                symbols: vec![],
                signatures: vec![],
                tier: Tier::Detail,
                provenance: "ast_exact".into(),
                signal_score: 0.5,
                detail: None,
            },
            SignalFile {
                path: "d".into(),
                symbols: vec![],
                signatures: vec![],
                tier: Tier::Detail,
                provenance: "ast_exact".into(),
                signal_score: 0.3,
                detail: None,
            },
        ];

        let refs: Vec<&SignalFile> = files.iter().collect();
        let ordered = position_order(&refs);

        // Highest score (1.0) at position 0, second highest (0.8) at position 2.
        // Interleaving: a(1.0), d(0.3), b(0.8), c(0.5)
        assert_eq!(ordered[0].path, "a"); // highest at first edge
        assert_eq!(ordered[ordered.len() - 1].path, "c"); // lowest at last edge
                                                          // Lower scores in the middle.
        assert_eq!(ordered[1].path, "d"); // 0.3 — interleaved from back
        assert_eq!(ordered[2].path, "b"); // 0.5 — interleaved from front
    }

    #[test]
    fn understand_empty_graph_returns_empty_result() {
        let db = GraphDB::open(":memory:").unwrap();
        let result = understand(&db, "auth middleware", &SignalOpts::default()).unwrap();
        assert!(result.files.is_empty());
        assert!(result.anchors.is_empty());
        assert!(result.text.contains("No files matched"));
    }

    #[test]
    fn understand_finds_anchors_by_path_match() {
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[
            edge("src/auth/middleware.go", "src/auth/handler.go", "imports"),
            edge("src/auth/handler.go", "src/db/query.go", "imports"),
        ])
        .unwrap();

        let result = understand(&db, "auth middleware", &SignalOpts::default()).unwrap();

        // Should find auth/middleware.go as an anchor.
        assert!(
            result.anchors.iter().any(|a| a.contains("middleware")),
            "anchors should include middleware file"
        );
        assert!(
            result.anchors.iter().any(|a| a.contains("auth")),
            "anchors should include auth files"
        );
    }

    #[test]
    fn understand_is_deterministic() {
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[
            edge("src/auth/middleware.go", "src/auth/handler.go", "imports"),
            edge("src/auth/handler.go", "src/db/query.go", "imports"),
        ])
        .unwrap();

        let opts = SignalOpts::default();
        let r1 = understand(&db, "auth middleware", &opts).unwrap();
        let r2 = understand(&db, "auth middleware", &opts).unwrap();

        assert_eq!(r1.text, r2.text, "output must be byte-identical");
        assert_eq!(r1.files.len(), r2.files.len());
        assert_eq!(r1.anchors, r2.anchors);
    }

    #[test]
    fn understand_traverses_graph_from_anchors() {
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[
            edge("src/auth/middleware.go", "src/auth/handler.go", "imports"),
            edge("src/auth/handler.go", "src/db/query.go", "imports"),
        ])
        .unwrap();

        let opts = SignalOpts {
            depth: 2,
            max_nodes: 60,
            ..Default::default()
        };
        let result = understand(&db, "auth", &opts).unwrap();

        // Should include files traversed from auth anchors.
        let paths: Vec<&str> = result.files.iter().map(|f| f.path.as_str()).collect();
        assert!(
            paths.iter().any(|p| p.contains("middleware")),
            "should include middleware"
        );
        assert!(
            paths.iter().any(|p| p.contains("handler")),
            "should include handler (1-hop)"
        );
    }

    #[test]
    fn understand_flat_resolution_omits_map_and_signatures() {
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[edge("src/auth.go", "src/db.go", "imports")])
            .unwrap();

        let opts = SignalOpts {
            resolution: Resolution::Flat,
            ..Default::default()
        };
        let result = understand(&db, "auth", &opts).unwrap();

        assert!(result.text.contains("DETAIL (flat)"));
        assert!(!result.text.contains("## MAP"));
        assert!(!result.text.contains("## SIGNATURES"));
    }

    #[test]
    fn understand_with_source_reader_uses_custom_reader() {
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[edge("src/auth.go", "src/db.go", "imports")])
            .unwrap();

        let opts = SignalOpts::default();
        let reader = |path: &str| -> Option<String> {
            if path.contains("auth") {
                Some("package auth\n\nfunc Authenticate() bool {\n    return true\n}\n".into())
            } else {
                None
            }
        };

        let result = understand_with_source(&db, "auth", &opts, Some(reader)).unwrap();

        // The auth file should have symbols extracted.
        let auth_file = result.files.iter().find(|f| f.path.contains("auth"));
        assert!(auth_file.is_some(), "auth file should be in result");
        let auth_file = auth_file.unwrap();
        assert!(
            auth_file.symbols.iter().any(|s| s.contains("Authenticate")),
            "symbols should include Authenticate"
        );
    }

    #[test]
    fn understand_caps_symbols_at_eight() {
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[edge("src/big.go", "src/util.go", "imports")])
            .unwrap();

        let opts = SignalOpts::default();
        let reader = |path: &str| -> Option<String> {
            if path.contains("big") {
                let mut src = "package big\n".to_string();
                for i in 0..20 {
                    src.push_str(&format!("func Func{i}() {{}}\n"));
                }
                Some(src)
            } else {
                None
            }
        };

        let result = understand_with_source(&db, "big", &opts, Some(reader)).unwrap();
        let big_file = result
            .files
            .iter()
            .find(|f| f.path.contains("big"))
            .unwrap();
        assert!(
            big_file.symbols.len() <= 8,
            "symbols should be capped at 8, got {}",
            big_file.symbols.len()
        );
    }

    #[test]
    fn understand_output_has_three_tiers_in_harmonic_mode() {
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[
            edge("src/auth/middleware.go", "src/auth/handler.go", "imports"),
            edge("src/auth/handler.go", "src/db/query.go", "imports"),
        ])
        .unwrap();

        let opts = SignalOpts::default();
        let result = understand(&db, "auth", &opts).unwrap();

        assert!(result.text.contains("## MAP"), "should have MAP tier");
        assert!(
            result.text.contains("## SIGNATURES"),
            "should have SIGNATURES tier"
        );
        assert!(result.text.contains("## DETAIL"), "should have DETAIL tier");
    }

    #[test]
    fn understand_tokens_estimate_is_reasonable() {
        let db = GraphDB::open(":memory:").unwrap();
        db.insert_edges(&[edge(
            "src/auth/middleware.go",
            "src/auth/handler.go",
            "imports",
        )])
        .unwrap();

        let opts = SignalOpts {
            token_budget: 6000,
            ..Default::default()
        };
        let result = understand(&db, "auth", &opts).unwrap();

        // tokens_estimate = chars / 4, should be roughly within budget.
        // For a small graph, it should be well under 6000.
        assert!(
            result.tokens_estimate < 6000,
            "estimate should be within budget, got {}",
            result.tokens_estimate
        );
    }

    #[test]
    fn extract_symbols_go_file() {
        let src = r#"package main

import "fmt"

func Authenticate(user string) bool {
    return true
}

type Middleware struct {
    Next http.Handler
}
"#;
        let symbols = extract_symbols("test.go", src);
        assert!(
            symbols.iter().any(|s| s.name.contains("Authenticate")),
            "should extract Authenticate function"
        );
        assert!(
            symbols.iter().any(|s| s.name.contains("Middleware")),
            "should extract Middleware type"
        );
    }

    #[test]
    fn extract_symbols_rust_file() {
        let src = r#"use std::io::Read;

pub fn parse_config(path: &str) -> Config {
    Config::default()
}

struct Config {
    port: u16,
}
"#;
        let symbols = extract_symbols("test.rs", src);
        assert!(
            symbols.iter().any(|s| s.name.contains("parse_config")),
            "should extract parse_config function"
        );
    }

    #[test]
    fn extract_symbols_unsupported_extension_returns_empty() {
        let symbols = extract_symbols("readme.md", "# Hello");
        assert!(symbols.is_empty());
    }

    #[test]
    fn resolution_serde_roundtrip() {
        for r in [Resolution::Harmonic, Resolution::Flat] {
            let json = serde_json::to_string(&r).unwrap();
            let back: Resolution = serde_json::from_str(&json).unwrap();
            assert_eq!(r, back);
        }
    }

    #[test]
    fn tier_serde_roundtrip() {
        for t in [Tier::Map, Tier::Signature, Tier::Detail] {
            let json = serde_json::to_string(&t).unwrap();
            let back: Tier = serde_json::from_str(&json).unwrap();
            assert_eq!(t, back);
        }
    }

    #[test]
    fn understand_respects_max_nodes() {
        let db = GraphDB::open(":memory:").unwrap();
        // Create a graph with many files.
        let edges: Vec<Edge> = (0..50)
            .map(|i| edge(&format!("src/file_{i}.go"), "src/common.go", "imports"))
            .collect();
        db.insert_edges(&edges).unwrap();

        let opts = SignalOpts {
            max_nodes: 10,
            ..Default::default()
        };
        let result = understand(&db, "file", &opts).unwrap();
        assert!(
            result.files.len() <= 10,
            "should cap at max_nodes=10, got {}",
            result.files.len()
        );
    }
}
