//! `hilo graph warm`, `hilo graph stats`, `hilo graph related`, and `hilo graph impact`.
//!
//! Queries (`related`, `impact`, `stats`) are JIT — they auto-parse files on
//! first access. `warm` is an optional batch pre-parse for CI / power users.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result};
use hilo_graph::edges;
use hilo_graph::{GraphDB, ImpactResult, Language, Parser};
use hilo_metadata::inventory::{self, Edge};
use rayon::prelude::*;
use regex::Regex;

use crate::commands::guard;

/// PERF-005: `go/pkg/mod` (the Go module cache) is pruned as a path suffix —
/// its parents (`go`, `go/pkg`) are ordinary source directories that must
/// remain walkable so an explicit `include_paths` entry can reopen a single
/// vendored module.
const GO_PKG_MOD: &str = "go/pkg/mod";

/// Walk the current directory for source files in all supported languages,
/// parse their imports, and write the resulting edges to both
/// Pre-compute the full graph — parse ALL source files (optional warmup).
///
/// This is the same batch-parse that was previously called
/// `hilo graph warm`.  It remains useful for CI pipelines or users who
/// want every file cached before running queries.  Day-to-day, queries are
/// JIT (lazy) and do **not** require `warm` first.
///
/// When `language` is `Some`, only files of that language are parsed
/// (e.g. `--language rust`).  Otherwise all supported languages are scanned.
/// PERF-002: per-file parse cache — relpath -> {mtime_nanos, size, edges}.
/// Lets a no-change `graph warm` skip ALL tree-sitter parsing.
type ParseCache = std::collections::HashMap<String, serde_json::Value>;

fn parse_cache_path(cwd: &Path) -> PathBuf {
    cwd.join(".vfs").join("graph").join(".parse_cache.json")
}

fn load_parse_cache(cwd: &Path) -> ParseCache {
    std::fs::read_to_string(parse_cache_path(cwd))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_parse_cache(cwd: &Path, cache: &ParseCache) -> Result<()> {
    let p = parse_cache_path(cwd);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let json = serde_json::to_string(cache).context("failed to serialize parse cache")?;
    std::fs::write(&p, json).context("failed to write parse cache")?;
    Ok(())
}

fn file_fingerprint(p: &Path) -> Option<(u128, u64)> {
    let m = std::fs::metadata(p).ok()?;
    let nanos = m
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some((nanos, m.len()))
}

/// GAP-065: per-file warm outcome so every discovered source file is
/// accounted for exactly once in the end-of-warm coverage summary.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FileOutcome {
    /// Parsed fine and produced at least one edge.
    Contributes,
    /// Zero edges from an `__init__.py` — a package facade, expected.
    PackageFacade,
    /// Zero edges from any other file — no imports found.
    NoImports,
    /// `fs::read_to_string` failed (permissions, I/O, non-UTF8).
    UnreadableSource { path: String },
    /// The extension is not a supported source language.
    UnsupportedExtension { path: String },
}

impl FileOutcome {
    /// Classify a successfully parsed file by its edge count and file name.
    /// Cache hits run through the same classifier, so an `__init__.py` served
    /// from `.parse_cache.json` still counts as a facade.
    fn from_edges(file: &Path, edges: &[Edge]) -> Self {
        if !edges.is_empty() {
            return Self::Contributes;
        }
        if file.file_name().and_then(|n| n.to_str()) == Some("__init__.py") {
            return Self::PackageFacade;
        }
        Self::NoImports
    }
}

/// Maximum number of unreadable/unsupported paths printed in full before the
/// summary truncates to `... and N more`.
const COVERAGE_PATH_LIST_CAP: usize = 20;

/// End-of-warm accounting (GAP-065): classifies every discovered source file
/// into exactly one outcome.  The printed verdict line closes the arithmetic
/// `verdict files = contributes + facades + no_imports + unreadable +
/// unsupported`, so a deliberate zero-edge file is distinguishable from a file
/// we failed to read or parse.
#[derive(Debug, Default)]
struct CoverageReport {
    contributes: usize,
    facades: usize,
    no_imports: usize,
    unreadable: Vec<String>,
    unsupported: Vec<String>,
}

impl CoverageReport {
    fn record(&mut self, outcome: FileOutcome) {
        match outcome {
            FileOutcome::Contributes => self.contributes += 1,
            FileOutcome::PackageFacade => self.facades += 1,
            FileOutcome::NoImports => self.no_imports += 1,
            FileOutcome::UnreadableSource { path } => self.unreadable.push(path),
            FileOutcome::UnsupportedExtension { path } => self.unsupported.push(path),
        }
    }

    /// Build the summary block: the verdict line (whose arithmetic closes
    /// against the discovered-file count) plus the capped path lists.
    fn render(&self) -> String {
        let unreadable_n = self.unreadable.len();
        let unsupported_n = self.unsupported.len();
        let verdict =
            self.contributes + self.facades + self.no_imports + unreadable_n + unsupported_n;
        let mut lines = vec![format!(
            "Coverage: {verdict} files = {} contribute edges + {} package facades (__init__.py) + {} no imports + {unreadable_n} unreadable + {unsupported_n} unsupported extension",
            self.contributes, self.facades, self.no_imports
        )];
        for (label, paths) in [
            ("unreadable source", &self.unreadable),
            ("unsupported extension", &self.unsupported),
        ] {
            lines.extend(coverage_path_lines(label, paths));
        }
        lines.join("\n")
    }

    fn print(&self) {
        println!("{}", self.render());
    }
}

/// Render one failure category's path list: up to [`COVERAGE_PATH_LIST_CAP`]
/// full paths, then an ellipsis line.
fn coverage_path_lines(label: &str, paths: &[String]) -> Vec<String> {
    let mut lines: Vec<String> = paths
        .iter()
        .take(COVERAGE_PATH_LIST_CAP)
        .map(|path| format!("  {label}: {path}"))
        .collect();
    if paths.len() > COVERAGE_PATH_LIST_CAP {
        lines.push(format!(
            "  ... and {} more {label}",
            paths.len() - COVERAGE_PATH_LIST_CAP
        ));
    }
    lines
}

pub fn run_warm(
    workspace: bool,
    language: Option<String>,
    changed: bool,
    allow_home: bool,
) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to determine the current directory")?;
    run_warm_in(
        &cwd,
        workspace,
        language,
        changed,
        allow_home,
        None,
        &|_| load_manifest(),
    )
}

/// Injectable core of [`run_warm`] (PERF-005): the walk root and HOME are
/// parameters and manifest loading goes through `load_manifest_fn`, so
/// command-level refusal/override and manifest-driven re-include behavior
/// are testable in a temp dir without touching the real HOME or the process
/// working directory. `load_manifest_fn` receives the walk root (production
/// passes a closure that ignores it and reads from the process cwd).
pub fn run_warm_in(
    root: &Path,
    workspace: bool,
    language: Option<String>,
    changed: bool,
    allow_home: bool,
    home: Option<PathBuf>,
    load_manifest_fn: &dyn Fn(&Path) -> Result<hilo_core::manifest::Manifest>,
) -> Result<()> {
    let cwd = root;

    // PERF-005: never treat HOME as a project root without an explicit
    // opt-in — warm would otherwise parse (and later JIT-parse) the entire
    // home directory including dependency and cache trees.
    guard::ensure_not_home_with(cwd, allow_home, home)?;

    // PERF-005: explicit per-path re-include overrides from the manifest
    // (`graph.include_paths`). No manifest (or no field) = empty list, i.e.
    // pure default behavior.
    let include_paths: Vec<String> = match load_manifest_fn(cwd) {
        Ok(m) => m.graph.include_paths,
        Err(_) => Vec::new(),
    };

    // Collect every source file under the current directory.
    let mut source_files = Vec::new();
    let mut exclusions = ExclusionReport::default();
    collect_source_files(
        cwd,
        Path::new(""),
        None,
        &include_paths,
        &mut source_files,
        &mut exclusions,
    )
    .context("failed to walk directory tree for source files")?;

    // When --changed is set, filter to files modified since the last warm.
    // The timestamp is stored in `.vfs/graph/.last_warm`.
    if changed {
        let warm_marker = cwd.join(".vfs").join("graph").join(".last_warm");
        if let Some(cutoff) = read_last_warm_mtime(&warm_marker) {
            let before = source_files.len();
            source_files.retain(|f| {
                std::fs::metadata(f)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .map(|mtime| mtime > cutoff)
                    .unwrap_or(true) // keep files we can't stat
            });
            eprintln!(
                "  --changed: {}/{} files modified since last warm",
                source_files.len(),
                before
            );
        } else {
            // No marker — parse everything (first warm).
            eprintln!("  --changed: no previous warm marker, parsing all files");
        }
    }

    // Optional language filter (e.g. --language rust).
    if let Some(ref lang_str) = language {
        let ext = match lang_str.as_str() {
            "go" => "go",
            "python" | "py" => "py",
            "typescript" | "ts" => "ts",
            "rust" | "rs" => "rs",
            "javascript" | "js" => "js",
            "java" => "java",
            "c" => "c",
            "cpp" | "c++" | "cxx" => "cpp",
            "ruby" | "rb" => "rb",
            "csharp" | "cs" | "c#" => "cs",
            "kotlin" | "kt" => "kt",
            "php" => "php",
            "swift" => "swift",
            "elixir" | "ex" | "exs" => "ex",
            "haskell" | "hs" => "hs",
            "erlang" | "erl" => "erl",
            "scala" => "scala",
            "zig" => "zig",
            "lua" => "lua",
            "dart" => "dart",
            "clojure" | "clj" => "clj",
            "ocaml" | "ml" => "ml",
            "r" => "r",
            "julia" | "jl" => "jl",
            "elm" => "elm",
            "nim" => "nim",
            other => anyhow::bail!("unknown language: {other}"),
        };
        source_files.retain(|f| f.extension().and_then(|e| e.to_str()) == Some(ext));
    }

    if source_files.is_empty() {
        println!(
            "No supported source files found. Supported extensions: {}",
            Language::all_extensions().join(", ")
        );
        exclusions.print();
        // GAP-065: vacuously closes the arithmetic (0 files discovered).
        CoverageReport::default().print();
        return Ok(());
    }

    // Count languages for summary output.
    let mut langs_seen: HashSet<Language> = HashSet::new();
    for file in &source_files {
        if let Some(ext) = file.extension().and_then(|e| e.to_str()) {
            if let Some(lang) = Language::from_extension(ext) {
                langs_seen.insert(lang);
            }
        }
    }

    let total_files = source_files.len();
    let progress = AtomicUsize::new(0);

    // PERF-002: load per-file parse cache (mtime+size keyed).
    let cache = load_parse_cache(cwd);
    let cached_files = AtomicUsize::new(0);
    let new_entries: std::sync::Mutex<ParseCache> = std::sync::Mutex::new(ParseCache::new());

    // Parallel parse: create a fresh parser per file since tree_sitter::Parser
    // is not Send.  Each closure runs on a rayon thread, reads the file, and
    // returns the parsed edges (or an empty vec on skip/error).
    // GAP-065: per-file outcomes, order-aligned with `source_files`
    // (`par_iter().map().collect()` preserves input order).
    let parse_results: Vec<Result<(Vec<Edge>, FileOutcome)>> = source_files
        .par_iter()
        .map(|file| {
            let ext = file.extension().and_then(|e| e.to_str());
            let lang = match ext.and_then(Language::from_extension) {
                Some(l) => l,
                None => {
                    let rel = file
                        .strip_prefix(cwd)
                        .unwrap_or(file)
                        .to_string_lossy()
                        .into_owned();
                    return Ok((Vec::new(), FileOutcome::UnsupportedExtension { path: rel }));
                }
            };

            let rel = file
                .strip_prefix(cwd)
                .unwrap_or(file)
                .to_string_lossy()
                .into_owned();

            let count = progress.fetch_add(1, Ordering::Relaxed) + 1;
            if count.is_multiple_of(100) || count == total_files {
                eprintln!("  parsing {count}/{total_files} files...");
            }

            // PERF-002: cache hit (same mtime+size) -> reuse edges, skip parse.
            if let Some((nanos, size)) = file_fingerprint(file) {
                if let Some(entry) = cache.get(&rel) {
                    if entry.get("m").and_then(|v| v.as_u64()) == Some(nanos as u64)
                        && entry.get("s").and_then(|v| v.as_u64()) == Some(size)
                    {
                        if let Some(arr) = entry.get("edges").and_then(|v| v.as_array()) {
                            let edges: Vec<Edge> = arr
                                .iter()
                                .filter_map(|e| serde_json::from_value(e.clone()).ok())
                                .collect();
                            cached_files.fetch_add(1, Ordering::Relaxed);
                            // GAP-065: classify cache hits with the same
                            // rules as fresh parses (a cached __init__.py is
                            // still a facade).
                            let outcome = FileOutcome::from_edges(file, &edges);
                            return Ok((edges, outcome));
                        }
                    }
                }
            }

            let source = match std::fs::read_to_string(file) {
                Ok(s) => s,
                Err(err) => {
                    // GAP-065: an unreadable file (permissions, I/O,
                    // non-UTF8) is no longer silently scored as a zero-edge
                    // file — it is named in the coverage summary.
                    eprintln!("  unreadable source file, classifying as failed: {rel} ({err})");
                    let outcome = FileOutcome::UnreadableSource { path: rel };
                    return Ok((Vec::new(), outcome));
                }
            };

            let mut parser = Parser::for_language(lang)
                .with_context(|| format!("failed to initialize {:?} parser", lang))?;

            let edges = parser
                .parse_imports(&rel, &source)
                .with_context(|| format!("failed to parse {rel}"))?;

            // record fingerprint + serialized edges for next run
            let entry = serde_json::json!({
                "m": file_fingerprint(file).map(|(n, _)| n as u64).unwrap_or(0),
                "s": file_fingerprint(file).map(|(_, s)| s).unwrap_or(0),
                "edges": edges,
            });
            new_entries.lock().unwrap().insert(rel.clone(), entry);
            let outcome = FileOutcome::from_edges(file, &edges);
            Ok((edges, outcome))
        })
        .collect();

    // PERF-002: persist updated cache entries (fresh parses only).
    {
        let mut new_entries = new_entries.lock().unwrap();
        if !new_entries.is_empty() {
            let mut merged = cache.clone();
            for (k, v) in new_entries.drain() {
                merged.insert(k, v);
            }
            // drop entries for files no longer present
            let live: std::collections::HashSet<String> = source_files
                .iter()
                .map(|f| {
                    f.strip_prefix(cwd)
                        .unwrap_or(f)
                        .to_string_lossy()
                        .into_owned()
                })
                .collect();
            merged.retain(|k, _| live.contains(k));
            save_parse_cache(cwd, &merged)?;
        }
    }
    let cached_n = cached_files.load(Ordering::Relaxed);
    if cached_n > 0 {
        eprintln!("  parse cache: {cached_n}/{total_files} files skipped (unchanged)");
    }

    // Flatten results, propagating the first error.
    let mut all_edges: Vec<Edge> = Vec::new();
    let mut unique_sources: HashSet<String> = HashSet::new();
    // GAP-065: end-of-warm accounting — every discovered file lands in
    // exactly one outcome bucket.
    let mut coverage = CoverageReport::default();
    for result in parse_results {
        let (edges, outcome) = result?;
        coverage.record(outcome);
        for e in &edges {
            unique_sources.insert(e.from.clone());
        }
        all_edges.extend(edges);
    }

    let t_assoc = std::time::Instant::now();
    // Infer `tested_by` and `tests` edges from filename conventions.
    let test_edges = discover_test_associations(&source_files, cwd);
    all_edges.extend(test_edges);
    if std::env::var("HILO_WARM_TIMING").is_ok() {
        eprintln!("  [timing] test associations: {:?}", t_assoc.elapsed());
    }

    let t_service = std::time::Instant::now();
    // GAP-081: infer `service_call` edges from Go gRPC client/server call
    // sites — the service dependency dimension an import-only graph cannot
    // see. Same merge point as the associations above, so the edges flow
    // through the shared dedupe/append and the DuckDB reconcile.
    let service_edges = discover_service_calls(&source_files, cwd);
    all_edges.extend(service_edges.iter().cloned());
    if std::env::var("HILO_WARM_TIMING").is_ok() {
        eprintln!("  [timing] service calls: {:?}", t_service.elapsed());
    }

    // Process graph extensions from manifest — manually declared edge patterns
    // like docs/**/*.md → src/**/*.go with relation "documented_by".
    if let Ok(manifest) = load_manifest() {
        let extension_edges =
            generate_extension_edges(&manifest.graph.extensions, &source_files, cwd);
        if !extension_edges.is_empty() {
            println!(
                "Generated {} edge(s) from {} manifest extension(s)",
                extension_edges.len(),
                manifest.graph.extensions.len()
            );
        }
        all_edges.extend(extension_edges);
    }

    // Detect cross-repo external edges when --workspace is set.
    if workspace {
        if let Ok(ws) = load_workspace_manifest() {
            let pairs: Vec<(String, String)> = ws
                .mounts
                .iter()
                .map(|m| (m.source.clone(), m.at.clone()))
                .collect();
            let repo_mounts = edges::build_repo_mounts(&pairs);

            let mut external_count = 0;
            for edge in all_edges.iter_mut() {
                if let Some((repo, path)) = edges::find_external_repo(&edge.to, &repo_mounts) {
                    edge.to = edges::format_external_edge(&repo, &path);
                    external_count += 1;
                }
            }

            if external_count > 0 {
                println!("Flagged {external_count} cross-repo edge(s) as external:repo:path");
            }
        }
    }

    let t_jsonl = std::time::Instant::now();
    // Persist edges to the JSONL inventory file.
    let edges_jsonl = cwd.join(".vfs").join("graph").join("edges.jsonl");
    // PERF-002 fast path: full cache hit => edges identical => skip the
    // jsonl rewrite AND the DuckDB insert entirely (row-point INSERTs are
    // DuckDB's anti-pattern; the only zero-cost insert is no insert).
    //
    // GAP-081 trap: "edges identical" only holds once the `service_call`
    // edges this pass computes are ALREADY present in edges.jsonl. A corpus
    // warmed before the pass existed has a valid parse cache and no
    // service_call lines, so taking the fast path there would strand the new
    // dimension forever (the cache would never invalidate). Fall through to
    // the append + DuckDB open while any computed service edge is absent; a
    // re-warm of a corpus that already carries them still skips.
    let full_cache_hit = cached_n == total_files && total_files > 0;
    if full_cache_hit && count_new_service_edges(&edges_jsonl, &service_edges) == 0 {
        if std::env::var("HILO_WARM_TIMING").is_ok() {
            eprintln!("  [timing] full cache hit — skipping jsonl+db write");
        }
        let n = all_edges.len();
        let m = unique_sources.len();
        let langs = langs_seen.len();
        println!("Discovered {n} edges across {m} files ({langs} languages) [all cached, graph unchanged]");
        exclusions.print();
        coverage.print();
        let _ = t_jsonl; // timing span unused on this path
        return Ok(());
    }
    inventory::append_edges_deduped(&edges_jsonl, &all_edges)
        .context("failed to write edges.jsonl")?;
    if std::env::var("HILO_WARM_TIMING").is_ok() {
        eprintln!("  [timing] jsonl dedup write: {:?}", t_jsonl.elapsed());
    }

    // Populate the DuckDB graph database.
    let t_db = std::time::Instant::now();
    let graph_db = cwd.join(".vfs").join("graph").join("graph.db");
    let graph_db_str = graph_db.to_str().unwrap_or(".vfs/graph/graph.db");
    let graph = GraphDB::open(graph_db_str).context("failed to open DuckDB graph database")?;
    if std::env::var("HILO_WARM_TIMING").is_ok() {
        eprintln!(
            "  [timing] GraphDB::open (incl reconcile): {:?}",
            t_db.elapsed()
        );
    }
    let t_ins = std::time::Instant::now();
    // PERF-002 delta insert: only edges from re-parsed files (+ assoc and
    // manifest-extension edges, which are cheap to reinsert with OR IGNORE).
    let reparsed: std::collections::HashSet<String> = {
        let ne = new_entries.lock().unwrap();
        ne.keys().cloned().collect()
    };
    let delta: Vec<Edge> = all_edges
        .iter()
        .filter(|e| reparsed.contains(&e.from) || e.provenance == "heuristic")
        .cloned()
        .collect();
    let db_edges = if cached_n > 0 && delta.len() < all_edges.len() / 4 {
        // small delta beats 7k+ row-point inserts
        graph
            .insert_edges(&delta)
            .context("failed to insert edge delta into DuckDB")?;
        eprintln!(
            "  delta insert: {} new/changed-file edges (skipped {} unchanged)",
            delta.len(),
            all_edges.len() - delta.len()
        );
        delta.len()
    } else {
        graph
            .insert_edges(&all_edges)
            .context("failed to insert edges into DuckDB")?;
        all_edges.len()
    };
    if std::env::var("HILO_WARM_TIMING").is_ok() {
        eprintln!("  [timing] insert_edges: {:?}", t_ins.elapsed());
    }

    let n = all_edges.len();
    let _ = db_edges;
    let m = unique_sources.len();
    let langs = langs_seen.len();
    println!("Discovered {n} edges across {m} files ({langs} languages)");
    exclusions.print();
    coverage.print();

    // Update the last-warm marker so --changed knows the cutoff.
    let warm_marker = cwd.join(".vfs").join("graph").join(".last_warm");
    if let Err(e) = write_last_warm_marker(&warm_marker) {
        eprintln!("warning: failed to write warm marker: {e}");
    }

    Ok(())
}

/// Read the modification time of the `.last_warm` marker file.
/// Returns `None` if the file doesn't exist (first warm run).
fn read_last_warm_mtime(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

/// Write the `.last_warm` marker file (touch — just needs to exist with
/// current mtime).
fn write_last_warm_marker(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    std::fs::write(path, "").with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

/// Query all edges for a file path, optionally filtered by relation type and
/// direction.
///
/// - Default (no `--direction` or `--direction forward`): outgoing edges
///   (WHERE "from" = ?).
/// - `--direction reverse`: incoming edges (WHERE "to" = ?), e.g.
///   `imported_by`, `tested_by`.
///
/// JIT: on first access the file is parsed on-the-fly and cached in DuckDB —
/// no `hilo graph warm` pre-requisite needed.
pub fn run_related(path: &str, relation: Option<&str>, direction: Option<&str>) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to determine the current directory")?;
    let graph_db = cwd.join(".vfs").join("graph").join("graph.db");

    // Ensure the .vfs/graph directory exists (create on first use).
    if let Some(parent) = graph_db.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let graph_db_str = graph_db.to_str().unwrap_or(".vfs/graph/graph.db");
    let graph = GraphDB::open(graph_db_str).context("failed to open DuckDB graph database")?;

    let dir = direction
        .map(hilo_graph::Direction::parse)
        .unwrap_or(hilo_graph::Direction::Forward);

    // JIT: parse on cache miss, query on cache hit.
    let edges = graph
        .related_or_parse(path, relation, dir)
        .context("failed to query related edges")?;

    if edges.is_empty() {
        let label = match dir {
            hilo_graph::Direction::Forward => "outgoing",
            hilo_graph::Direction::Reverse => "incoming",
        };
        if let Some(rel) = relation {
            println!("No {rel} ({label}) edges found for '{path}'.");
        } else {
            println!("No {label} edges for '{path}'.");
        }
        return Ok(());
    }

    for edge in &edges {
        // GAP-045: pkg:* targets are external-package pseudo-nodes, not
        // resolvable file paths — label them so agents don't try to pass
        // them to meta/impact (GAP-038 covered stats/search only).
        let external = if edge.from.starts_with("pkg:") || edge.to.starts_with("pkg:") {
            " [external package]"
        } else {
            ""
        };
        println!(
            "{}  →  {}  ({})  [{} conf={:.2}]{}",
            edge.from, edge.to, edge.rel, edge.provenance, edge.confidence, external
        );
    }

    Ok(())
}

/// Compute transitive impact: find all files that depend on `path`, directly
/// or transitively, up to `max_depth` hops.
///
/// When `format` is `"json"`, prints the result as pretty-printed JSON.
/// Otherwise prints each dependent file in human-readable text.
///
/// When `external` is `true`, also follows `external:repo:path` cross-repo edges.
///
/// JIT: on first access the start file is parsed on-the-fly and cached —
/// no `hilo graph warm` pre-requisite needed.
pub fn run_impact(path: &str, max_depth: u32, format: Option<&str>, external: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to determine the current directory")?;
    let graph_db = cwd.join(".vfs").join("graph").join("graph.db");

    // Ensure the .vfs/graph directory exists.
    if let Some(parent) = graph_db.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let graph_db_str = graph_db.to_str().unwrap_or(".vfs/graph/graph.db");
    let graph = GraphDB::open(graph_db_str).context("failed to open DuckDB graph database")?;

    let results = if external {
        // GAP-039: same node-existence check for the external path —
        // unknown paths must fail loudly, not look like zero dependents.
        if !graph.file_in_graph(path)? && !Path::new(path).exists() {
            anyhow::bail!("'{path}' is not in the graph (no such file and no matching graph node)");
        }
        // For external: parse start file first, then use cross-repo BFS.
        graph.ensure_parsed(path)?;
        hilo_graph::impact::compute_impact_with_external(graph.conn(), path, max_depth, true)
            .context("failed to compute impact with external edges")?
    } else {
        // JIT: parse start file on cache miss, then BFS over cache.
        graph
            .impact_or_parse(path, max_depth)
            .context("failed to compute impact")?
    };

    match format {
        Some("json") => {
            let result = ImpactResult { files: results };
            let json = hilo_graph::serde_json::to_string_pretty(&result)
                .context("failed to serialize impact results as JSON")?;
            println!("{json}");
        }
        _ => {
            if results.is_empty() {
                println!("No dependents found for '{path}'.");
            } else {
                for file in &results {
                    let prov = file.provenance.as_deref().unwrap_or("ast_exact");
                    let conf = file.confidence.unwrap_or(1.0);
                    println!(
                        "{}  ←  {}  (depth: {})  [{} conf={:.2}]",
                        file.path, file.relation, file.depth, prov, conf
                    );
                }
            }
        }
    }

    Ok(())
}

/// Count non-empty lines in `.vfs/graph/edges.jsonl` (the raw edge records
/// before DuckDB dedup). `None` when the file doesn't exist (JIT-only graph).
fn raw_edges_jsonl_count(cwd: &std::path::Path) -> Option<usize> {
    let path = cwd.join(".vfs").join("graph").join("edges.jsonl");
    let content = std::fs::read_to_string(path).ok()?;
    Some(content.lines().filter(|l| !l.trim().is_empty()).count())
}

/// PERF-001 companion: a missing `graph.db` is no longer a hard error when a
/// sibling `edges.jsonl` exists — `GraphDB::open` reconciles the full jsonl
/// into a fresh DB (cheap since PERF-001: single prepared-statement
/// transaction) and stamps it, so the next command opens instantly.
/// Returns the DB path to open, or None when there is truly no data.
fn resolve_graph_db_path(cwd: &std::path::Path) -> Option<std::path::PathBuf> {
    let graph_db = cwd.join(".vfs").join("graph").join("graph.db");
    if graph_db.exists() {
        return Some(graph_db);
    }
    let jsonl = cwd.join(".vfs").join("graph").join("edges.jsonl");
    if jsonl.exists() {
        return Some(graph_db); // open() will rebuild from edges.jsonl
    }
    None
}

/// Print summary statistics from the dependency graph.
///
/// An empty cache is a valid state (not an error) — the graph starts
/// empty after `hilo init` and is populated lazily as files are queried
/// or eagerly via `hilo graph warm`.
pub fn run_stats(limit: usize) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to determine the current directory")?;
    let Some(graph_db) = resolve_graph_db_path(&cwd) else {
        println!("Graph cache is empty. Query a file or run `hilo graph warm` to populate.");
        return Ok(());
    };

    let graph_db_str = graph_db.to_str().unwrap_or(".vfs/graph/graph.db");
    let graph = GraphDB::open(graph_db_str).context("failed to open DuckDB graph database")?;
    let stats = graph
        .stats()
        .context("failed to compute graph statistics")?;

    if stats.total_edges == 0 {
        println!("Graph cache is empty. No edges parsed yet.");
        return Ok(());
    }

    // GAP-038: DuckDB dedupes multi-provenance edges, so the distinct edge
    // count can be lower than the raw edges.jsonl line count — always
    // surface both so the delta is explained instead of looking like a bug.
    match raw_edges_jsonl_count(&cwd) {
        Some(raw) => println!(
            "Total edges: {} distinct / {} raw (edges.jsonl)",
            stats.total_edges, raw
        ),
        None => println!("Total edges: {}", stats.total_edges),
    }
    println!("Total files: {}", stats.total_files);
    if let Some(ref mc) = stats.most_connected {
        println!("Most connected: {mc}");
    }
    println!("Edge types:");
    // BTree ordering: stats.edge_types is a HashMap, whose iteration order is
    // randomized per process — unsorted print made `graph stats` output
    // non-deterministic across identical runs (broke before/after diffing).
    let mut edge_types_sorted: Vec<_> = stats.edge_types.iter().collect();
    edge_types_sorted.sort();
    for (rel, count) in edge_types_sorted {
        println!("  {rel}: {count}");
    }
    if !stats.orphans.is_empty() {
        println!("Orphans (no incoming edges):");
        // GAP-080: monorepos can have thousands of orphans — cap the list,
        // summarize the remainder, honor --limit 0 as unlimited.
        let shown = if limit == 0 {
            stats.orphans.len()
        } else {
            limit.min(stats.orphans.len())
        };
        for orphan in &stats.orphans[..shown] {
            println!("  {orphan}");
        }
        let remaining = stats.orphans.len() - shown;
        if remaining > 0 {
            println!("  ... {remaining} more orphans (use --limit 0 to show all)");
        }
    }
    println!("Top dependencies:");
    for (dep, count) in &stats.top_dependencies {
        println!("  {dep}: {count}");
    }

    Ok(())
}

/// Infer `tested_by` and `tests` edges from common filename conventions across
/// all 9 supported languages.
///
/// - `*_test.go` -> tested_by -> `*.go` (and reverse: `*.go` -> tests -> `*_test.go`)
/// - `test_*.py` -> tested_by -> `*.py`
/// - `*.test.ts` -> tested_by -> `*.ts`
/// - `*.spec.ts` -> tested_by -> `*.ts`
/// - `*_test.rs` -> tested_by -> `*.rs`
/// - `*Test.java` -> tested_by -> `*.java`
/// - `test_*.c` -> tested_by -> `*.c`
/// - `*_test.cpp` -> tested_by -> `*.cpp`
/// - `*_test.rb` -> tested_by -> `*.rb`
fn discover_test_associations(source_files: &[PathBuf], cwd: &Path) -> Vec<Edge> {
    let mut edges = Vec::new();
    let stem_set: HashSet<String> = source_files
        .iter()
        .map(|p| {
            let rel = p.strip_prefix(cwd).unwrap_or(p);
            rel.to_string_lossy().into_owned()
        })
        .collect();

    for file in source_files {
        let rel = file.strip_prefix(cwd).unwrap_or(file);
        let file_name = rel.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let file_str = rel.to_string_lossy();

        // Check if this is a test file -> generate tested_by edge
        if let Some(source_stem) = test_to_source(file_name) {
            let parent = rel.parent().unwrap_or(Path::new(""));
            let source_path = parent.join(&source_stem);
            let source_str = source_path.to_string_lossy().into_owned();
            if stem_set.contains(&source_str) || file_name == source_stem {
                edges.push(Edge {
                    from: file_str.clone().into_owned(),
                    to: source_str,
                    rel: "tested_by".to_string(),
                    provenance: "heuristic".to_string(),
                    confidence: 0.8,
                });
            }
        }

        // Check if this is a source file that has a corresponding test file -> tests edge
        for test_stem in source_to_test_patterns(file_name) {
            let parent = rel.parent().unwrap_or(Path::new(""));
            let test_path = parent.join(&test_stem);
            let test_str = test_path.to_string_lossy().into_owned();
            if stem_set.contains(&test_str) {
                edges.push(Edge {
                    from: file_str.to_string(),
                    to: test_str,
                    rel: "tests".to_string(),
                    provenance: "heuristic".to_string(),
                    confidence: 0.8,
                });
            }
        }
    }

    edges
}

// ──────────────────────── GAP-081: service_call edges ────────────────────────

/// Relation emitted by [`discover_service_calls`].
///
/// Additive: nothing enumerates `rel` values, and traversal/impact filter on
/// `from`/`to` only, so consumers pick the new dimension up for free.
const SERVICE_CALL_REL: &str = "service_call";

/// Provenance stamp for the gRPC call-site pass.
///
/// Deliberately a fixed string — `(from, to, rel, provenance)` is the dedupe
/// key used by `inventory::append_edges_deduped`, so a stable provenance makes
/// the pass idempotent across re-warms for free.
const SERVICE_CALL_PROVENANCE: &str = "grpc_ast";

/// Header markers of protoc-gen-go output (`protoc-gen-go` and
/// `protoc-gen-go-grpc` both start their banner this way).
const GO_GENERATED_MARKER: &str = "Code generated by protoc-gen-go";
const GO_DO_NOT_EDIT_MARKER: &str = "DO NOT EDIT";

/// How many leading lines are searched for the generated marker — the banner
/// sits in the file header, but after a license block, so line 1 is not enough.
const GO_GENERATED_HEAD_LINES: usize = 60;

/// True when `text` looks like protoc-gen-go generated code.
///
/// This is the discriminator that makes provider resolution correct: every
/// `*/genproto/*_grpc.pb.go` in a repo declares `New<X>ServiceClient` AND
/// `Register<X>ServiceServer` for EVERY service in the project (not only the
/// one its directory serves), so counting generated files would attach each
/// caller to every service's stub file instead of the file that serves it.
fn is_generated_go(text: &str) -> bool {
    text.lines()
        .take(GO_GENERATED_HEAD_LINES)
        .any(|line| line.contains(GO_GENERATED_MARKER) || line.contains(GO_DO_NOT_EDIT_MARKER))
}

/// GAP-081: infer `service_call` edges from Go gRPC client/server call sites.
///
/// Import edges cannot see RPC: in a microservice repo the dependency between
/// two Go services is a proto contract plus a generated client stub, never an
/// import statement — the importing file imports the stub, which every service
/// in the repo ships a copy of. The consequence is that `graph impact` answers
/// "No dependents found" for a service entry point, i.e. the blast radius of a
/// service is empty by construction.
///
/// For every discovered, non-generated `.go` file:
/// - `New<Stem>ServiceClient(` marks a **caller** of `<Stem>Service`
/// - `Register<Stem>ServiceServer(` marks a **provider** of `<Stem>Service`
///
/// and each caller is linked to every provider of the same service:
/// `rel = "service_call"`, `provenance = "grpc_ast"`, `confidence = 1.0`,
/// paths repo-relative like every other edge in this file. One edge per
/// distinct (caller, provider) pair, emitted in sorted order so the output is
/// deterministic.
///
/// Only `source_files` (the discovered set) is scanned — this pass never walks
/// the tree itself, so the repo's exclusion rules and `include_paths` apply.
fn discover_service_calls(source_files: &[PathBuf], cwd: &Path) -> Vec<Edge> {
    // `[A-Za-z][A-Za-z0-9]*` keeps the stem an identifier: no leading digit,
    // no greedy match across a dotted selector.
    let caller_re = Regex::new(r"New([A-Za-z][A-Za-z0-9]*)ServiceClient\(")
        .expect("static caller regex must compile");
    let provider_re = Regex::new(r"Register([A-Za-z][A-Za-z0-9]*)ServiceServer\(")
        .expect("static provider regex must compile");

    // service name -> files (callers / providers)
    let mut callers: HashMap<String, HashSet<String>> = HashMap::new();
    let mut providers: HashMap<String, HashSet<String>> = HashMap::new();

    for file in source_files {
        let rel = file.strip_prefix(cwd).unwrap_or(file);
        if rel.extension().and_then(|e| e.to_str()) != Some("go") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        if is_generated_go(&text) {
            continue;
        }
        let rel_str = rel.to_string_lossy().into_owned();
        for line in text.lines() {
            // A definition is never a call site — belt and braces with the
            // generated-file check above, so a hand-written (or unrecognised)
            // stub file cannot turn its own declarations into dependents.
            if line.trim_start().starts_with("func ") {
                continue;
            }
            for cap in caller_re.captures_iter(line) {
                let service = format!("{}Service", &cap[1]);
                callers.entry(service).or_default().insert(rel_str.clone());
            }
            for cap in provider_re.captures_iter(line) {
                let service = format!("{}Service", &cap[1]);
                providers
                    .entry(service)
                    .or_default()
                    .insert(rel_str.clone());
            }
        }
    }

    // Link every caller of a service to every provider of that same service.
    // The pair is the identity of the edge in this schema (the dedupe key is
    // from/to/rel/provenance), so the set is keyed on the pair: repeated call
    // sites of one service in one file, and two services served by the same
    // file, both collapse to a single edge.
    let mut pairs: BTreeSet<(String, String)> = BTreeSet::new();
    for (service, caller_files) in &callers {
        let Some(provider_files) = providers.get(service) else {
            continue;
        };
        for caller in caller_files {
            for provider in provider_files {
                // A file that serves and calls the same service is not a
                // dependent of itself.
                if caller != provider {
                    pairs.insert((caller.clone(), provider.clone()));
                }
            }
        }
    }

    pairs
        .into_iter()
        .map(|(from, to)| Edge {
            from,
            to,
            rel: SERVICE_CALL_REL.to_string(),
            provenance: SERVICE_CALL_PROVENANCE.to_string(),
            confidence: 1.0,
        })
        .collect()
}

/// GAP-081: how many of `service_edges` are missing from `edges_jsonl`.
///
/// Uses the same `(from, to, rel, provenance)` identity as
/// `inventory::append_edges_deduped`, so "0 missing" means the append would
/// write nothing — the only condition under which the PERF-002 full-cache-hit
/// fast path is safe once this pass exists. A missing/unreadable file counts
/// every edge as new (there is nothing to trust).
fn count_new_service_edges(edges_jsonl: &Path, service_edges: &[Edge]) -> usize {
    if service_edges.is_empty() {
        return 0;
    }
    let mut seen: HashSet<(String, String, String, String)> = HashSet::new();
    if let Ok(contents) = std::fs::read_to_string(edges_jsonl) {
        for line in contents.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(edge) = serde_json::from_str::<Edge>(line) {
                seen.insert((edge.from, edge.to, edge.rel, edge.provenance));
            }
        }
    }
    service_edges
        .iter()
        .filter(|e| {
            !seen.contains(&(
                e.from.clone(),
                e.to.clone(),
                e.rel.clone(),
                e.provenance.clone(),
            ))
        })
        .count()
}

/// If `file_name` is a test file, return the source file stem it tests.
fn test_to_source(name: &str) -> Option<String> {
    if let Some(stem) = name.strip_suffix("_test.go") {
        Some(format!("{stem}.go"))
    } else if let Some(stem) = name.strip_suffix("_test.rs") {
        Some(format!("{stem}.rs"))
    } else if let Some(stem) = name.strip_suffix("_test.cpp") {
        Some(format!("{stem}.cpp"))
    } else if let Some(stem) = name.strip_suffix("_test.rb") {
        Some(format!("{stem}.rb"))
    } else if let Some(stem) = name.strip_suffix("Test.cs") {
        Some(format!("{stem}.cs"))
    } else if let Some(stem) = name.strip_suffix("Tests.cs") {
        Some(format!("{stem}.cs"))
    } else if let Some(stem) = name.strip_suffix("Test.kt") {
        Some(format!("{stem}.kt"))
    } else if let Some(stem) = name.strip_suffix("Tests.kt") {
        Some(format!("{stem}.kt"))
    } else if let Some(stem) = name.strip_suffix("Test.php") {
        Some(format!("{stem}.php"))
    } else if let Some(stem) = name.strip_suffix("Tests.php") {
        Some(format!("{stem}.php"))
    } else if let Some(stem) = name.strip_suffix("Test.swift") {
        Some(format!("{stem}.swift"))
    } else if let Some(stem) = name.strip_suffix("Tests.swift") {
        Some(format!("{stem}.swift"))
    } else if let Some(stem) = name.strip_suffix("_test.exs") {
        Some(format!("{stem}.ex"))
    } else if let Some(stem) = name.strip_suffix("Spec.hs") {
        Some(format!("{stem}.hs"))
    } else if let Some(stem) = name.strip_suffix("Test.hs") {
        Some(format!("{stem}.hs"))
    } else if let Some(stem) = name.strip_suffix("Tests.hs") {
        Some(format!("{stem}.hs"))
    } else if let Some(stem) = name.strip_suffix("_SUITE.erl") {
        Some(format!("{stem}.erl"))
    } else if let Some(stem) = name.strip_suffix("Test.scala") {
        Some(format!("{stem}.scala"))
    } else if let Some(stem) = name.strip_suffix("Tests.scala") {
        Some(format!("{stem}.scala"))
    } else if let Some(stem) = name.strip_suffix("Spec.scala") {
        Some(format!("{stem}.scala"))
    } else if let Some(stem) = name.strip_suffix("_test.zig") {
        Some(format!("{stem}.zig"))
    } else if let Some(stem) = name.strip_suffix("_test.lua") {
        Some(format!("{stem}.lua"))
    } else if let Some(stem) = name.strip_suffix("_test.dart") {
        Some(format!("{stem}.dart"))
    } else if let Some(stem) = name.strip_suffix("_test.clj") {
        Some(format!("{stem}.clj"))
    } else if let Some(stem) = name.strip_suffix("_test.cljs") {
        Some(format!("{stem}.cljs"))
    } else if let Some(stem) = name.strip_suffix("_test.ml") {
        Some(format!("{stem}.ml"))
    } else if let Some(stem) = name.strip_suffix("_test.mli") {
        Some(format!("{stem}.mli"))
    } else if let Some(stem) = name.strip_suffix("_test.jl") {
        Some(format!("{stem}.jl"))
    } else if let Some(stem) = name.strip_suffix("Test.elm") {
        Some(format!("{stem}.elm"))
    } else if let Some(stem) = name.strip_suffix("Tests.elm") {
        Some(format!("{stem}.elm"))
    } else if let Some(stem) = name.strip_suffix("_test.nim") {
        Some(format!("{stem}.nim"))
    } else if let Some(stem) = name.strip_prefix("test_") {
        if stem.ends_with(".py")
            || stem.ends_with(".c")
            || stem.ends_with(".clj")
            || stem.ends_with(".cljs")
            || stem.ends_with(".r")
            || stem.ends_with(".jl")
            || stem.ends_with(".nim")
        {
            Some(stem.to_string())
        } else {
            None
        }
    } else if let Some(stem) = name.strip_prefix("test-") {
        if stem.ends_with(".r") {
            Some(stem.to_string())
        } else {
            None
        }
    } else if let Some(stem) = name.strip_suffix(".test.ts") {
        Some(format!("{stem}.ts"))
    } else if let Some(stem) = name.strip_suffix(".spec.ts") {
        Some(format!("{stem}.ts"))
    } else {
        name.strip_suffix("Test.java")
            .map(|stem| format!("{stem}.java"))
    }
}

/// Return possible test file names for a given source file.
fn source_to_test_patterns(name: &str) -> Vec<String> {
    let mut patterns = Vec::new();
    if let Some(stem) = name.strip_suffix(".go") {
        patterns.push(format!("{stem}_test.go"));
    } else if let Some(stem) = name.strip_suffix(".py") {
        patterns.push(format!("test_{stem}.py"));
    } else if let Some(stem) = name.strip_suffix(".ts") {
        patterns.push(format!("{stem}.test.ts"));
        patterns.push(format!("{stem}.spec.ts"));
    } else if let Some(stem) = name.strip_suffix(".rs") {
        patterns.push(format!("{stem}_test.rs"));
    } else if let Some(stem) = name.strip_suffix(".java") {
        patterns.push(format!("{stem}Test.java"));
    } else if let Some(stem) = name.strip_suffix(".c") {
        patterns.push(format!("test_{stem}.c"));
    } else if let Some(stem) = name.strip_suffix(".cpp") {
        patterns.push(format!("{stem}_test.cpp"));
    } else if let Some(stem) = name.strip_suffix(".rb") {
        patterns.push(format!("{stem}_test.rb"));
    } else if let Some(stem) = name.strip_suffix(".cs") {
        patterns.push(format!("{stem}Test.cs"));
        patterns.push(format!("{stem}Tests.cs"));
    } else if let Some(stem) = name.strip_suffix(".kt") {
        patterns.push(format!("{stem}Test.kt"));
        patterns.push(format!("{stem}Tests.kt"));
    } else if let Some(stem) = name.strip_suffix(".php") {
        patterns.push(format!("{stem}Test.php"));
        patterns.push(format!("{stem}Tests.php"));
    } else if let Some(stem) = name.strip_suffix(".swift") {
        patterns.push(format!("{stem}Test.swift"));
        patterns.push(format!("{stem}Tests.swift"));
    } else if let Some(stem) = name.strip_suffix(".ex") {
        patterns.push(format!("{stem}_test.exs"));
    } else if let Some(stem) = name.strip_suffix(".exs") {
        patterns.push(format!("{stem}_test.exs"));
    } else if let Some(stem) = name.strip_suffix(".hs") {
        patterns.push(format!("{stem}Spec.hs"));
        patterns.push(format!("{stem}Test.hs"));
    } else if let Some(stem) = name.strip_suffix(".lhs") {
        patterns.push(format!("{stem}Spec.lhs"));
    } else if let Some(stem) = name.strip_suffix(".erl") {
        patterns.push(format!("{stem}_SUITE.erl"));
    } else if let Some(stem) = name.strip_suffix(".scala") {
        patterns.push(format!("{stem}Test.scala"));
        patterns.push(format!("{stem}Spec.scala"));
    } else if let Some(stem) = name.strip_suffix(".sc") {
        patterns.push(format!("{stem}Test.sc"));
    } else if let Some(stem) = name.strip_suffix(".zig") {
        patterns.push(format!("{stem}_test.zig"));
    } else if let Some(stem) = name.strip_suffix(".lua") {
        patterns.push(format!("{stem}_test.lua"));
        patterns.push(format!("{stem}_spec.lua"));
    } else if let Some(stem) = name.strip_suffix(".dart") {
        patterns.push(format!("{stem}_test.dart"));
    } else if let Some(stem) = name.strip_suffix(".clj") {
        patterns.push(format!("{stem}_test.clj"));
        patterns.push(format!("test_{stem}.clj"));
    } else if let Some(stem) = name.strip_suffix(".cljs") {
        patterns.push(format!("{stem}_test.cljs"));
    } else if let Some(stem) = name.strip_suffix(".ml") {
        patterns.push(format!("{stem}_test.ml"));
    } else if let Some(stem) = name.strip_suffix(".mli") {
        patterns.push(format!("{stem}_test.mli"));
    } else if let Some(stem) = name.strip_suffix(".r") {
        patterns.push(format!("test_{stem}.r"));
        patterns.push(format!("test-{stem}.r"));
    } else if let Some(stem) = name.strip_suffix(".jl") {
        patterns.push(format!("{stem}_test.jl"));
        patterns.push(format!("test_{stem}.jl"));
    } else if let Some(stem) = name.strip_suffix(".elm") {
        patterns.push(format!("{stem}Test.elm"));
        patterns.push(format!("{stem}Tests.elm"));
    } else if let Some(stem) = name.strip_suffix(".nim") {
        patterns.push(format!("{stem}_test.nim"));
        patterns.push(format!("test_{stem}.nim"));
    }
    patterns
}

/// Stable, user-facing labels for discovery exclusion categories.
const EXCLUSION_CATEGORY_ORDER: &[&str] = &[
    "vendor",
    "go/pkg/mod",
    "node_modules",
    "virtualenv/cache",
    "dependency/cache",
    "hidden",
];

#[derive(Debug, Default, PartialEq, Eq)]
struct ExclusionReport {
    counts: BTreeMap<&'static str, usize>,
}

impl ExclusionReport {
    fn record_file(&mut self, path: &Path, category: &'static str) {
        if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| Language::from_extension(extension).is_some())
        {
            *self.counts.entry(category).or_default() += 1;
        }
    }

    /// Count a pruned tree in one pass without following symlinks.
    ///
    /// A pruned tree was intentionally not part of the old walk's error
    /// surface, so unreadable descendants are best-effort for reporting and
    /// must not make an otherwise successful warm fail.
    fn count_tree(&mut self, dir: &Path, category: &'static str) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                self.count_tree(&path, category);
            } else if file_type.is_file() {
                self.record_file(&path, category);
            }
        }
    }

    fn print(&self) {
        let total: usize = self.counts.values().sum();
        if total == 0 {
            return;
        }

        let categories = EXCLUSION_CATEGORY_ORDER
            .iter()
            .filter_map(|category| {
                self.counts
                    .get(category)
                    .filter(|count| **count > 0)
                    .map(|count| format!("{category}: {count}"))
            })
            .collect::<Vec<_>>();
        println!(
            "Excluded {total} supported source files ({})",
            categories.join(", ")
        );
    }
}

/// Return the stable report category for a default-excluded entry.
fn exclusion_category(rel: &str, file_name: &str) -> Option<&'static str> {
    // Hidden takes precedence so paths such as `.venv` are reported as hidden,
    // rather than being silently folded into another pruning category.
    if file_name.starts_with('.') {
        return Some("hidden");
    }
    if rel == GO_PKG_MOD || rel.ends_with(&format!("/{GO_PKG_MOD}")) {
        return Some("go/pkg/mod");
    }
    match file_name {
        "vendor" => Some("vendor"),
        "node_modules" => Some("node_modules"),
        "venv" | "site-packages" => Some("virtualenv/cache"),
        name if guard::DEFAULT_PRUNE_DIRS.contains(&name) => Some("dependency/cache"),
        _ => None,
    }
}

/// Default-exclusion test for a discovery entry (PERF-005).
///
/// `rel` is the entry's path relative to the walk root with `/` separators.
/// An entry is default-excluded when it is hidden, its name matches the
/// prune list, or it is (or lives under) a `go/pkg/mod` module cache.
fn entry_excluded(rel: &str, file_name: &str) -> bool {
    exclusion_category(rel, file_name).is_some()
}

/// True when `rel` IS a manifest `include_paths` entry or lives inside one —
/// the entry and its whole subtree are open for discovery.
fn is_included(rel: &str, include_paths: &[String]) -> bool {
    include_paths.iter().any(|inc| {
        let inc = inc.trim_end_matches('/');
        rel == inc || rel.starts_with(&format!("{inc}/"))
    })
}

/// True when `rel` is an ANCESTOR of some `include_paths` entry — the walk
/// must pass through it, but its non-included siblings/children stay gated.
fn is_include_ancestor(rel: &str, include_paths: &[String]) -> bool {
    include_paths
        .iter()
        .any(|inc| inc.trim_end_matches('/').starts_with(&format!("{rel}/")))
}

fn collect_source_files(
    dir: &Path,
    rel: &Path,
    excluded_category: Option<&'static str>,
    include_paths: &[String],
    out: &mut Vec<PathBuf>,
    exclusions: &mut ExclusionReport,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        let entry_rel = rel.join(&*name_str);
        let rel_str = entry_rel.to_string_lossy().replace('\\', "/");

        let ft = entry.file_type()?;
        if ft.is_dir() {
            let entry_category = exclusion_category(&rel_str, &name_str);
            let included = is_included(&rel_str, include_paths);
            // Descend when explicitly re-included, when on the path to a
            // re-included subtree, or when the default rules never matched.
            let descend = included
                || is_include_ancestor(&rel_str, include_paths)
                || (!entry_excluded(&rel_str, &name_str) && excluded_category.is_none());
            if !descend {
                // The subtree is deliberately pruned. Count it here, rather
                // than walking it once for discovery and again for reporting.
                let category = excluded_category
                    .or(entry_category)
                    .unwrap_or("dependency/cache");
                exclusions.count_tree(&path, category);
                continue;
            }
            // Inside a re-included subtree everything is open; otherwise the
            // exclusion state carries down (so `vendor/other` stays out when
            // only `vendor/critical` was re-included).
            let child_category = if included {
                None
            } else {
                excluded_category.or(entry_category)
            };
            collect_source_files(
                &path,
                &entry_rel,
                child_category,
                include_paths,
                out,
                exclusions,
            )?;
        } else if ft.is_file() {
            let entry_category = exclusion_category(&rel_str, &name_str);
            let take = is_included(&rel_str, include_paths)
                || (entry_category.is_none() && excluded_category.is_none());
            if take {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    if Language::from_extension(ext).is_some() {
                        out.push(path);
                    }
                }
            } else if let Some(category) = excluded_category.or(entry_category) {
                exclusions.record_file(&path, category);
            }
        }
    }
    Ok(())
}

/// Generate edges from graph extensions declared in the manifest.
///
/// Each extension has a pattern like `"docs/**/*.md → src/**/*.go"` and a
/// relation like `"documented_by"`.  The left-hand glob matches source files;
/// the right-hand glob matches target files.  Every matching (from, to) pair
/// produces an edge with the declared relation.
fn generate_extension_edges(
    extensions: &[hilo_core::manifest::GraphExtension],
    source_files: &[PathBuf],
    cwd: &Path,
) -> Vec<Edge> {
    let mut edges = Vec::new();

    for ext in extensions {
        // Parse the pattern "from_glob → to_glob".
        let parts: Vec<&str> = ext.pattern.splitn(2, "→").map(str::trim).collect();
        if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
            eprintln!(
                "[warn] graph extension '{}' has malformed pattern '{}' — skipping",
                ext.name, ext.pattern
            );
            continue;
        }
        let from_glob = parts[0];
        let to_glob = parts[1];

        // Match source files against the from-glob.
        let matched_from: Vec<PathBuf> = source_files
            .iter()
            .filter(|f| {
                let rel = f.strip_prefix(cwd).unwrap_or(f);
                glob_matches(from_glob, &rel.to_string_lossy())
            })
            .cloned()
            .collect();

        // Match source files against the to-glob.
        let matched_to: Vec<PathBuf> = source_files
            .iter()
            .filter(|f| {
                let rel = f.strip_prefix(cwd).unwrap_or(f);
                glob_matches(to_glob, &rel.to_string_lossy())
            })
            .cloned()
            .collect();

        // Generate a cross-product of edges.
        for from_file in &matched_from {
            for to_file in &matched_to {
                let from_rel = from_file
                    .strip_prefix(cwd)
                    .unwrap_or(from_file)
                    .to_string_lossy()
                    .into_owned();
                let to_rel = to_file
                    .strip_prefix(cwd)
                    .unwrap_or(to_file)
                    .to_string_lossy()
                    .into_owned();

                // Skip self-edges.
                if from_rel == to_rel {
                    continue;
                }

                edges.push(Edge {
                    from: from_rel,
                    to: to_rel,
                    rel: ext.relation.clone(),
                    provenance: "heuristic".to_string(),
                    confidence: 0.5,
                });
            }
        }
    }

    edges
}

/// Check whether a path matches a glob pattern.  Falls back to simple
/// substring/suffix matching when glob::Pattern::new fails on complex patterns.
fn glob_matches(pattern: &str, path: &str) -> bool {
    match glob::Pattern::new(pattern) {
        Ok(p) => p.matches(path),
        Err(_) => {
            // Fallback: simple ** suffix matching
            if let Some(suffix) = pattern.strip_prefix("**/") {
                path.ends_with(suffix) || path.contains(suffix)
            } else {
                path.contains(pattern)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Rule engine — manifest-driven SQL queries against the graph
// ---------------------------------------------------------------------------

/// Default manifest paths (relative to CWD).
const MANIFEST_PATHS: &[&str] = &["manifest.yaml", ".vfs/manifest.yaml"];

/// Load the manifest from the first available path.
fn load_manifest() -> Result<hilo_core::manifest::Manifest> {
    for path in MANIFEST_PATHS {
        if std::path::Path::new(path).exists() {
            return Ok(hilo_core::manifest::Manifest::from_file(path)?);
        }
    }
    anyhow::bail!(
        "No manifest found. Create a manifest.yaml or .vfs/manifest.yaml file with `hilo init`."
    );
}

/// Load the workspace manifest from the first available path.
fn load_workspace_manifest() -> Result<hilo_core::workspace::WorkspaceManifest> {
    for path in MANIFEST_PATHS {
        if std::path::Path::new(path).exists() {
            return Ok(hilo_core::workspace::WorkspaceManifest::load(path)?);
        }
    }
    anyhow::bail!("No workspace manifest found");
}

/// `hilo graph rule-list` — print all rules from the manifest.
pub fn run_rule_list() -> Result<()> {
    let manifest = load_manifest()?;

    if manifest.rules.is_empty() {
        println!("No rules defined in the manifest.");
        return Ok(());
    }

    println!("Rules defined in manifest:");
    for rule in &manifest.rules {
        println!("  {} — {}", rule.name, rule.description);
    }
    Ok(())
}

/// `hilo graph understand <task>` — multi-resolution harmonic context output.
///
/// Runs the signal engine against the graph and prints the 3-tier harmonic
/// output (MAP → SIGNATURES → DETAIL). See `hilo-graph/src/signal.rs`.
pub fn run_understand(task: &str, budget: Option<usize>) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to determine the current directory")?;
    let Some(graph_db) = resolve_graph_db_path(&cwd) else {
        anyhow::bail!("No graph data. Run `hilo graph warm` first.");
    };

    let graph_db_str = graph_db.to_str().unwrap_or(".vfs/graph/graph.db");
    let graph = GraphDB::open(graph_db_str).context("failed to open DuckDB graph database")?;

    let mut opts = hilo_graph::signal::SignalOpts::default();
    if let Some(b) = budget {
        opts.token_budget = b;
    }

    let result = hilo_graph::signal::understand(&graph, task, &opts)
        .context("failed to run signal engine")?;

    println!("{}", result.text);
    Ok(())
}

/// `hilo graph search <query>` — deterministic semantic code search.
///
/// Uses TF-IDF + BM25 + Reciprocal Rank Fusion over all graph nodes.
/// Zero external APIs, fully deterministic. See `hilo-graph/src/semantic.rs`.
pub fn run_search(query: &str, limit: Option<usize>, no_symbols: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to determine the current directory")?;
    let Some(graph_db) = resolve_graph_db_path(&cwd) else {
        anyhow::bail!("No graph data. Run `hilo graph warm` first.");
    };

    let graph_db_str = graph_db.to_str().unwrap_or(".vfs/graph/graph.db");
    let graph = GraphDB::open(graph_db_str).context("failed to open DuckDB graph database")?;

    let mut opts = hilo_graph::semantic::SearchOpts::default();
    if let Some(l) = limit {
        opts.limit = l;
    }
    // GAP-077: CLI search opts into symbol indexing by default so exact
    // symbol names ("url_for") find their defining file; --no-symbols
    // restores the cheap path-only index.
    opts.index_symbols = !no_symbols;

    let results = hilo_graph::semantic::search(&graph, query, &opts)
        .context("failed to run semantic search")?;

    if results.is_empty() {
        println!("No results found for '{}'.", query);
        return Ok(());
    }

    for r in &results {
        println!("{:.4}  {}  [{}]", r.score, r.file_path, r.provenance);
        if !r.symbols.is_empty() {
            println!("         symbols: {}", r.symbols.join(", "));
        }
    }

    Ok(())
}

/// `hilo graph module <prefix>` — per-module statistics.
///
/// Returns file list, edge count, and test coverage for files under a
/// directory prefix (e.g. "hilo-graph/src"). See `GraphDB::module_files_at`.
pub fn run_module(module_name: &str) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to determine the current directory")?;
    let Some(graph_db) = resolve_graph_db_path(&cwd) else {
        anyhow::bail!("No graph data. Run `hilo graph warm` first.");
    };

    let graph_db_str = graph_db.to_str().unwrap_or(".vfs/graph/graph.db");
    let graph = GraphDB::open(graph_db_str).context("failed to open DuckDB graph database")?;

    let stats = graph
        .module_files_at(&cwd, module_name)
        .context("failed to query module stats")?;

    println!("Module: {}", stats.module);
    println!("Files:  {}", stats.files.len());
    println!("Edges:  {}", stats.edges_count);
    println!("Tests:  {:.1}%", stats.test_coverage_pct);

    if !stats.files.is_empty() {
        println!("── Files ──");
        for f in &stats.files {
            println!("  {f}");
        }
    }

    Ok(())
}

/// `hilo graph untested` — list source files with no test coverage.
///
/// Queries all files that have `imports` edges but no `tested_by` edges —
/// neither on the file itself nor on the `pkg:` node it resolves to.
/// See `GraphDB::untested_files_at`.
pub fn run_untested() -> Result<()> {
    let cwd = std::env::current_dir().context("failed to determine the current directory")?;
    let Some(graph_db) = resolve_graph_db_path(&cwd) else {
        anyhow::bail!("No graph data. Run `hilo graph warm` first.");
    };

    let graph_db_str = graph_db.to_str().unwrap_or(".vfs/graph/graph.db");
    let graph = GraphDB::open(graph_db_str).context("failed to open DuckDB graph database")?;

    let files = graph
        .untested_files_at(&cwd)
        .context("failed to query untested files")?;

    if files.is_empty() {
        println!("All files have test coverage.");
        return Ok(());
    }

    println!("{} untested file(s):", files.len());
    for f in &files {
        println!("  {f}");
    }

    Ok(())
}

/// `hilo graph rule-check <name>` — execute a named rule against the graph.
pub fn run_rule_check(name: &str) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to determine the current directory")?;
    let Some(graph_db) = resolve_graph_db_path(&cwd) else {
        anyhow::bail!("No graph data. Run `hilo graph warm` first.");
    };

    let manifest = load_manifest()?;

    let query_rule = manifest
        .rules
        .iter()
        .find(|r| r.name == name)
        .ok_or_else(|| {
            let available: Vec<&str> = manifest.rules.iter().map(|r| r.name.as_str()).collect();
            anyhow::anyhow!(
                "Rule '{}' not found in manifest. Available: {}",
                name,
                available.join(", ")
            )
        })?;

    let rule = hilo_graph::Rule {
        name: query_rule.name.clone(),
        description: query_rule.description.clone(),
        query: query_rule.query.clone(),
    };

    let graph_db_str = graph_db.to_str().unwrap_or(".vfs/graph/graph.db");
    let graph = GraphDB::open(graph_db_str).context("failed to open DuckDB graph database")?;

    match hilo_graph::RuleEngine::check(graph.conn(), &rule) {
        Ok(result) => {
            if result.matches.is_empty() {
                println!("No matches for rule '{}'.", name);
            } else {
                println!("Rule '{}' — {} match(es):", name, result.total);
                for row in &result.matches {
                    println!("  {}", row.join(" | "));
                }
            }
            Ok(())
        }
        Err(err) => {
            // Return structured error — never panic.
            anyhow::bail!("Rule '{}' failed: {}", err.rule, err.error);
        }
    }
}

/// `hilo graph clean` — delete the cached dependency graph.
///
/// Removes `.vfs/graph/edges.jsonl`, `.vfs/graph/graph.db`, the
/// `.parse_cache.json` per-file parse cache, and the `.last_warm` marker so the
/// next `warm` (or JIT parse) rebuilds the graph from scratch. Use this after
/// crate renames or file moves leave stale edges in the cache (e.g.
/// `warpfs-*` entries after the rename to `hilo-*`).
pub fn run_clean() -> Result<()> {
    let cwd = std::env::current_dir().context("failed to determine the current directory")?;
    let removed = clean_graph_dir(&cwd)?;
    if removed == 0 {
        println!(
            "Graph cache already clean ({})",
            cwd.join(".vfs").join("graph").display()
        );
    } else {
        println!(
            "Graph cache cleaned ({removed} file(s) removed). Run `hilo graph warm` to rebuild."
        );
    }
    Ok(())
}

/// Remove the graph cache files under `cwd/.vfs/graph/`; returns the number
/// of files removed. Missing files are not an error.
fn clean_graph_dir(cwd: &Path) -> Result<usize> {
    let graph_dir = cwd.join(".vfs").join("graph");
    let mut removed = 0;
    for name in ["edges.jsonl", "graph.db", ".parse_cache.json", ".last_warm"] {
        let path = graph_dir.join(name);
        match std::fs::remove_file(&path) {
            Ok(()) => {
                println!("removed {}", path.display());
                removed += 1;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(
                    anyhow::Error::new(e).context(format!("failed to remove {}", path.display()))
                )
            }
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn clean_removes_graph_cache() {
        let dir = TempDir::new().unwrap();
        let graph_dir = dir.path().join(".vfs").join("graph");
        fs::create_dir_all(&graph_dir).unwrap();
        for name in ["edges.jsonl", "graph.db", ".parse_cache.json", ".last_warm"] {
            fs::write(graph_dir.join(name), "stale").unwrap();
        }

        let removed = clean_graph_dir(dir.path()).unwrap();
        assert_eq!(removed, 4);
        assert!(!graph_dir.join("edges.jsonl").exists());
        assert!(!graph_dir.join("graph.db").exists());
        assert!(!graph_dir.join(".parse_cache.json").exists());
        assert!(!graph_dir.join(".last_warm").exists());

        // Second run: nothing to remove, not an error.
        let removed = clean_graph_dir(dir.path()).unwrap();
        assert_eq!(removed, 0);
    }

    #[test]
    fn glob_matches_exact() {
        assert!(glob_matches("src/main.rs", "src/main.rs"));
        assert!(!glob_matches("src/main.rs", "src/lib.rs"));
    }

    #[test]
    fn glob_matches_wildcard() {
        assert!(glob_matches("src/**/*.rs", "src/auth/login.rs"));
        assert!(glob_matches("src/**/*.rs", "src/main.rs"));
        assert!(!glob_matches("src/**/*.rs", "tests/test_auth.rs"));
    }

    #[test]
    fn glob_matches_suffix() {
        assert!(glob_matches("*.md", "README.md"));
        assert!(glob_matches("*.md", "docs/guide.md"));
        assert!(!glob_matches("*.md", "src/main.rs"));
    }

    #[test]
    fn generate_extensions_empty() {
        let dir = TempDir::new().unwrap();
        let extensions: Vec<hilo_core::manifest::GraphExtension> = vec![];
        let source_files: Vec<PathBuf> = vec![];
        let edges = generate_extension_edges(&extensions, &source_files, dir.path());
        assert!(edges.is_empty());
    }

    #[test]
    fn generate_extensions_single_pattern() {
        let dir = TempDir::new().unwrap();
        // Create some dummy files
        let docs_dir = dir.path().join("docs");
        fs::create_dir_all(&docs_dir).unwrap();
        fs::write(docs_dir.join("guide.md"), "# Guide").unwrap();
        let src_dir = dir.path().join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(src_dir.join("main.rs"), "fn main() {}").unwrap();

        let extensions = vec![hilo_core::manifest::GraphExtension {
            name: "docs".to_string(),
            pattern: "docs/**/*.md → src/**/*.rs".to_string(),
            relation: "documented_by".to_string(),
        }];

        let source_files = vec![docs_dir.join("guide.md"), src_dir.join("main.rs")];

        let edges = generate_extension_edges(&extensions, &source_files, dir.path());
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].from, "docs/guide.md");
        assert_eq!(edges[0].to, "src/main.rs");
        assert_eq!(edges[0].rel, "documented_by");
    }

    #[test]
    fn generate_extensions_no_match() {
        let dir = TempDir::new().unwrap();
        let src_dir = dir.path().join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(src_dir.join("main.rs"), "fn main() {}").unwrap();

        let extensions = vec![hilo_core::manifest::GraphExtension {
            name: "docs".to_string(),
            pattern: "docs/**/*.md → src/**/*.rs".to_string(),
            relation: "documented_by".to_string(),
        }];

        // No docs/ files exist — from-glob matches nothing
        let source_files = vec![src_dir.join("main.rs")];
        let edges = generate_extension_edges(&extensions, &source_files, dir.path());
        assert!(edges.is_empty());
    }

    #[test]
    fn generate_extensions_self_edge_skipped() {
        let dir = TempDir::new().unwrap();
        let src_dir = dir.path().join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(src_dir.join("lib.rs"), "pub fn x() {}").unwrap();

        // Pattern matches same file against itself — should skip
        let extensions = vec![hilo_core::manifest::GraphExtension {
            name: "self-ref".to_string(),
            pattern: "src/lib.rs → src/lib.rs".to_string(),
            relation: "tests".to_string(),
        }];

        let source_files = vec![src_dir.join("lib.rs")];
        let edges = generate_extension_edges(&extensions, &source_files, dir.path());
        assert!(edges.is_empty(), "self-edges should be skipped");
    }

    #[test]
    fn generate_extensions_multi_pattern() {
        let dir = TempDir::new().unwrap();
        let docs_dir = dir.path().join("docs");
        fs::create_dir_all(&docs_dir).unwrap();
        fs::write(docs_dir.join("api.md"), "# API").unwrap();
        fs::write(docs_dir.join("guide.md"), "# Guide").unwrap();
        let src_dir = dir.path().join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(src_dir.join("main.rs"), "fn main() {}").unwrap();
        fs::write(src_dir.join("lib.rs"), "pub fn x() {}").unwrap();

        let extensions = vec![hilo_core::manifest::GraphExtension {
            name: "docs".to_string(),
            pattern: "docs/**/*.md → src/**/*.rs".to_string(),
            relation: "documented_by".to_string(),
        }];

        let source_files = vec![
            docs_dir.join("api.md"),
            docs_dir.join("guide.md"),
            src_dir.join("main.rs"),
            src_dir.join("lib.rs"),
        ];

        let edges = generate_extension_edges(&extensions, &source_files, dir.path());
        // 2 docs × 2 src = 4 edges
        assert_eq!(edges.len(), 4);
        // All should have the documented_by relation
        assert!(edges.iter().all(|e| e.rel == "documented_by"));
    }

    // ======================================================================
    // PERF-005: discovery exclusion contract
    // ======================================================================

    /// Every dependency/cache tree that must be pruned by default.
    const PRUNE_FIXTURE_DIRS: &[&str] = &[
        "go/pkg/mod",
        "vendor",
        "node_modules",
        "venv",
        ".venv",
        "site-packages",
        "target",
        ".cache",
        ".rustup",
        ".npm",
    ];

    /// Build the full exclusion fixture: a source file inside every pruned
    /// tree, plus control files in ordinary directories.
    fn build_prune_fixture(root: &Path) -> Vec<PathBuf> {
        use std::fs;
        let excluded_sources = [
            "go/pkg/mod/github.com/some/module@v1.0.0/lib.go",
            "vendor/github.com/dep/dep.go",
            "node_modules/pkg/index.js",
            "venv/lib/mod.py",
            ".venv/lib/mod.py",
            "site-packages/pkg/mod.py",
            "target/debug/build.rs",
            ".cache/toolgen/out.rs",
            ".rustup/toolchains/stable-x86_64/lib/rustlib.rs",
            ".npm/_cacache/entry.js",
        ];
        for rel in excluded_sources {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, "fn placeholder() {}\n").unwrap();
        }
        let controls = ["src/main.rs", "src/lib.rs", "cmd/server/main.go"];
        for rel in controls {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, "fn placeholder() {}\n").unwrap();
        }
        let mut all: Vec<PathBuf> = excluded_sources
            .iter()
            .chain(controls.iter())
            .map(|r| root.join(r))
            .collect();
        all.sort();
        all
    }

    fn rels_of(root: &Path, found: &[PathBuf]) -> Vec<String> {
        let mut v: Vec<String> = found
            .iter()
            .map(|p| {
                p.strip_prefix(root)
                    .unwrap_or(p)
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        v.sort();
        v
    }

    fn contains_component(rel: &str, name: &str) -> bool {
        rel.split('/').any(|c| c == name)
    }

    #[test]
    fn entry_excluded_covers_full_prune_contract() {
        // Full default-prune list, by name.
        for name in [
            "target",
            "node_modules",
            "vendor",
            "__pycache__",
            "venv",
            ".venv",
            "site-packages",
            ".cache",
            ".rustup",
            ".npm",
        ] {
            assert!(
                entry_excluded(name, name),
                "{name} must be excluded by default"
            );
        }
        // Hidden entries (dot dirs/files) stay excluded.
        assert!(entry_excluded(".hidden", ".hidden"));
        assert!(entry_excluded(".config/app.rs", ".config"));
        // Go module cache as a path suffix (directory level — children are
        // never visited because the walk prunes the dir first).
        assert!(entry_excluded("go/pkg/mod", GO_PKG_MOD));
        assert!(entry_excluded("tools/go/pkg/mod", "mod"));
        // Boundary: a sibling like `go/pkg/mod-alt` is NOT the module cache.
        assert!(!entry_excluded("go/pkg/mod-alt/x.go", "x.go"));
        // Ordinary entries stay included.
        assert!(!entry_excluded("src", "src"));
        assert!(!entry_excluded("src/main.rs", "main.rs"));
        assert!(!entry_excluded("cmd/server/main.go", "main.go"));
        // `go` and `go/pkg` themselves stay walkable (only `go/pkg/mod`).
        assert!(!entry_excluded("go", "go"));
        assert!(!entry_excluded("go/pkg", "pkg"));
    }

    #[test]
    fn include_boundary_checks_do_not_leak_between_prefixes() {
        let inc = vec!["vendor/critical".to_string()];
        assert!(is_included("vendor/critical", &inc));
        assert!(is_included("vendor/critical/a.rs", &inc));
        assert!(
            !is_included("vendor/critical2", &inc),
            "prefix must respect path boundaries"
        );
        assert!(!is_included("vendor/other", &inc));
        assert!(is_include_ancestor("vendor", &inc));
        assert!(
            !is_include_ancestor("vendor2", &inc),
            "prefix must respect path boundaries"
        );
        // Sibling inside an excluded parent stays gated by in_excluded_tree.
        let inc2 = vec!["go/pkg/mod/github.com/keep/keep@v1.0.0".to_string()];
        assert!(is_included("go/pkg/mod/github.com/keep/keep@v1.0.0", &inc2));
        assert!(!is_included("go/pkg/mod/github.com/other/x@v1.0.0", &inc2));
    }

    #[test]
    fn discovery_reports_supported_exclusions_by_category() {
        let dir = TempDir::new().unwrap();
        build_prune_fixture(dir.path());

        // A hidden source is reportable, while ordinary files are not.
        fs::create_dir_all(dir.path().join(".hidden")).unwrap();
        fs::write(dir.path().join(".hidden/secret.rs"), "fn secret() {}\n").unwrap();
        fs::write(dir.path().join("vendor/README.txt"), "documentation\n").unwrap();
        fs::write(dir.path().join("vendor/asset.bin"), [0_u8, 1, 2]).unwrap();

        let mut found = Vec::new();
        let mut exclusions = ExclusionReport::default();
        collect_source_files(
            dir.path(),
            Path::new(""),
            None,
            &[],
            &mut found,
            &mut exclusions,
        )
        .unwrap();

        assert_eq!(exclusions.counts.get("go/pkg/mod"), Some(&1));
        assert_eq!(exclusions.counts.get("vendor"), Some(&1));
        assert_eq!(exclusions.counts.get("node_modules"), Some(&1));
        assert_eq!(exclusions.counts.get("virtualenv/cache"), Some(&2));
        assert_eq!(exclusions.counts.get("dependency/cache"), Some(&1));
        assert_eq!(exclusions.counts.get("hidden"), Some(&5));
        assert_eq!(exclusions.counts.values().sum::<usize>(), 11);
        assert!(!exclusions.counts.contains_key("README.txt"));
        assert!(!exclusions.counts.contains_key("asset.bin"));
    }

    #[test]
    fn discovery_prunes_all_dependency_trees_by_default() {
        let dir = TempDir::new().unwrap();
        build_prune_fixture(dir.path());

        let mut found = Vec::new();
        let mut exclusions = ExclusionReport::default();
        collect_source_files(
            dir.path(),
            Path::new(""),
            None,
            &[],
            &mut found,
            &mut exclusions,
        )
        .unwrap();
        let found = rels_of(dir.path(), &found);

        assert!(
            found.contains(&"src/main.rs".to_string()),
            "control file missing: {found:?}"
        );
        assert!(
            found.contains(&"cmd/server/main.go".to_string()),
            "ordinary go/ paths must stay walkable: {found:?}"
        );
        for name in PRUNE_FIXTURE_DIRS {
            let leaked: Vec<&String> = found
                .iter()
                .filter(|r| contains_component(r, name.trim_start_matches("go/pkg/")))
                .collect();
            // `go/pkg/mod` is a suffix rule, checked separately below.
            if *name == "go/pkg/mod" {
                assert!(
                    !found.iter().any(|r| r.contains("go/pkg/mod")),
                    "go/pkg/mod leaked: {found:?}"
                );
                continue;
            }
            assert!(
                leaked.is_empty(),
                "{name} leaked into discovery: {leaked:?}"
            );
        }
        assert_eq!(
            found,
            vec!["cmd/server/main.go", "src/lib.rs", "src/main.rs"]
        );
    }

    #[test]
    fn discovery_include_override_reopens_one_path_only() {
        let dir = TempDir::new().unwrap();
        build_prune_fixture(dir.path());

        // Re-include exactly one vendored path; everything else stays out.
        let include_paths = vec!["vendor/critical".to_string()];
        let keep = dir.path().join("vendor/critical");
        std::fs::create_dir_all(&keep).unwrap();
        std::fs::write(keep.join("vendored.rs"), "fn vendored() {}\n").unwrap();
        std::fs::write(dir.path().join("vendor/other.rs"), "fn other() {}\n").unwrap();

        let mut found = Vec::new();
        let mut exclusions = ExclusionReport::default();
        collect_source_files(
            dir.path(),
            Path::new(""),
            None,
            &include_paths,
            &mut found,
            &mut exclusions,
        )
        .unwrap();
        let found = rels_of(dir.path(), &found);

        assert!(
            found.contains(&"vendor/critical/vendored.rs".to_string()),
            "explicitly re-included path must be discovered: {found:?}"
        );
        assert!(
            !found
                .iter()
                .any(|r| r.starts_with("vendor/") && r != "vendor/critical/vendored.rs"),
            "no other vendored path may leak in: {found:?}"
        );
        assert!(
            !found.iter().any(|r| contains_component(r, "node_modules")),
            "unrelated excluded trees stay pruned: {found:?}"
        );
        assert_eq!(exclusions.counts.get("vendor"), Some(&2));
        assert_eq!(exclusions.counts.values().sum::<usize>(), 11);
        assert!(found.contains(&"src/main.rs".to_string()));
    }

    #[test]
    fn discovery_include_override_can_reopen_a_go_module() {
        let dir = TempDir::new().unwrap();
        build_prune_fixture(dir.path());

        let include_paths = vec!["go/pkg/mod/github.com/keep/keep@v1.0.0".to_string()];
        let keep = dir.path().join("go/pkg/mod/github.com/keep/keep@v1.0.0");
        std::fs::create_dir_all(&keep).unwrap();
        std::fs::write(keep.join("keep.go"), "package keep\n").unwrap();

        let mut found = Vec::new();
        let mut exclusions = ExclusionReport::default();
        collect_source_files(
            dir.path(),
            Path::new(""),
            None,
            &include_paths,
            &mut found,
            &mut exclusions,
        )
        .unwrap();
        let found = rels_of(dir.path(), &found);

        assert!(
            found.contains(&"go/pkg/mod/github.com/keep/keep@v1.0.0/keep.go".to_string()),
            "re-included module must be discovered: {found:?}"
        );
        assert!(
            found
                .iter()
                .filter(|r| r.contains("go/pkg/mod"))
                .all(|r| r.starts_with("go/pkg/mod/github.com/keep/keep@v1.0.0/")),
            "only the re-included module may leak: {found:?}"
        );
        assert_eq!(exclusions.counts.get("go/pkg/mod"), Some(&1));
        assert_eq!(exclusions.counts.values().sum::<usize>(), 10);
    }

    // ======================================================================
    // PERF-005: command-level HOME guard (injectable root + HOME)
    // ======================================================================

    fn no_manifest(_root: &Path) -> Result<hilo_core::manifest::Manifest> {
        anyhow::bail!("no manifest in fixture")
    }

    fn manifest_with_include(paths: &[&str]) -> hilo_core::manifest::Manifest {
        let mut yaml = String::from("project:\n  name: fixture\ngraph:\n  include_paths:\n");
        for p in paths {
            yaml.push_str(&format!("    - {p}\n"));
        }
        hilo_core::manifest::Manifest::parse(&yaml).unwrap()
    }

    /// Parse-cache keys are exactly the rel paths warm discovered and
    /// parsed — the most direct observable for "was this file seen?".
    fn warm_cache_keys(root: &Path) -> Vec<String> {
        let cache_path = root.join(".vfs").join("graph").join(".parse_cache.json");
        let raw = std::fs::read_to_string(&cache_path)
            .unwrap_or_else(|_| panic!("parse cache must exist at {}", cache_path.display()));
        let cache: std::collections::HashMap<String, serde_json::Value> =
            serde_json::from_str(&raw).unwrap();
        let mut keys: Vec<String> = cache.into_keys().collect();
        keys.sort();
        keys
    }

    #[test]
    fn warm_refuses_home_root() {
        let home = TempDir::new().unwrap();
        let err = run_warm_in(
            home.path(),
            false,
            None,
            false,
            false,
            Some(home.path().to_path_buf()),
            &no_manifest,
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("--allow-home"),
            "error must name the flag: {msg}"
        );
        assert!(
            !home.path().join(".vfs").exists(),
            "refused warm must not create state"
        );
    }

    #[test]
    fn warm_allow_home_overrides() {
        let home = TempDir::new().unwrap();
        std::fs::write(home.path().join("keep.rs"), "fn keep() {}\n").unwrap();

        run_warm_in(
            home.path(),
            false,
            None,
            false,
            true, // --allow-home
            Some(home.path().to_path_buf()),
            &no_manifest,
        )
        .unwrap();
        assert_eq!(warm_cache_keys(home.path()), vec!["keep.rs".to_string()]);
    }

    #[test]
    fn warm_normal_project_remains_green() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("lib.rs"), "fn lib() {}\n").unwrap();

        run_warm_in(
            project.path(),
            false,
            None,
            false,
            false,
            Some(home.path().to_path_buf()),
            &no_manifest,
        )
        .unwrap();
        assert_eq!(warm_cache_keys(project.path()), vec!["lib.rs".to_string()]);
    }

    #[test]
    fn warm_manifest_include_paths_reach_discovery() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        std::fs::create_dir_all(project.path().join("vendor/critical")).unwrap();
        std::fs::create_dir_all(project.path().join("node_modules")).unwrap();
        std::fs::write(project.path().join("lib.rs"), "fn lib() {}\n").unwrap();
        std::fs::write(
            project.path().join("vendor/critical/vendored.rs"),
            "fn vendored() {}\n",
        )
        .unwrap();
        std::fs::write(
            project.path().join("node_modules/index.js"),
            "const x = 1;\n",
        )
        .unwrap();

        let manifest = manifest_with_include(&["vendor/critical"]);
        let loader = move |_root: &Path| Ok(manifest.clone());

        run_warm_in(
            project.path(),
            false,
            None,
            false,
            false,
            Some(home.path().to_path_buf()),
            &loader,
        )
        .unwrap();

        let keys = warm_cache_keys(project.path());
        assert!(
            keys.contains(&"vendor/critical/vendored.rs".to_string()),
            "manifest re-include must reach discovery: {keys:?}"
        );
        assert!(
            !keys.contains(&"node_modules/index.js".to_string()),
            "non-included excluded trees stay out: {keys:?}"
        );
        assert!(
            keys.contains(&"lib.rs".to_string()),
            "control file: {keys:?}"
        );
    }

    // ======================================================================
    // GAP-065: warm coverage accounting
    // ======================================================================

    fn edge_of(from: &str) -> Edge {
        Edge {
            from: from.to_string(),
            to: "sys:target".to_string(),
            rel: "imports".to_string(),
            provenance: "ast_exact".to_string(),
            confidence: 1.0,
        }
    }

    #[test]
    fn coverage_classifier_distinguishes_contributes_facade_and_no_imports() {
        let contributing = Path::new("pkg/app.py");
        let facade = Path::new("pkg/__init__.py");
        let plain = Path::new("pkg/constants.py");

        assert_eq!(
            FileOutcome::from_edges(contributing, &[edge_of("pkg/app.py")]),
            FileOutcome::Contributes
        );
        // Zero edges + facade name => package facade (never "no imports").
        assert_eq!(
            FileOutcome::from_edges(facade, &[]),
            FileOutcome::PackageFacade
        );
        // Zero edges + ordinary name => no imports found.
        assert_eq!(FileOutcome::from_edges(plain, &[]), FileOutcome::NoImports);
    }

    #[test]
    fn coverage_report_render_closes_arithmetic_and_caps_paths() {
        // Zero report: the verdict line still closes (0 = 0+0+0+0+0).
        let empty = CoverageReport::default();
        assert_eq!(
            empty.render(),
            "Coverage: 0 files = 0 contribute edges + 0 package facades (__init__.py) \
             + 0 no imports + 0 unreadable + 0 unsupported extension"
        );

        let mut report = CoverageReport::default();
        report.record(FileOutcome::Contributes);
        report.record(FileOutcome::PackageFacade);
        report.record(FileOutcome::NoImports);
        for i in 0..(COVERAGE_PATH_LIST_CAP + 3) {
            report.record(FileOutcome::UnreadableSource {
                path: format!("src/locked{i}.py"),
            });
        }
        report.record(FileOutcome::UnsupportedExtension {
            path: "Makefile".to_string(),
        });

        let rendered = report.render();
        let verdict = rendered.lines().next().unwrap();
        assert_eq!(
            verdict,
            "Coverage: 27 files = 1 contribute edges + 1 package facades (__init__.py) \
             + 1 no imports + 23 unreadable + 1 unsupported extension"
        );
        let named: Vec<&str> = rendered
            .lines()
            .filter(|line| line.starts_with("  unreadable source: "))
            .collect();
        assert_eq!(
            named.len(),
            COVERAGE_PATH_LIST_CAP,
            "path list must cap at the limit: {rendered}"
        );
        assert!(
            rendered.contains("  ... and 3 more unreadable source"),
            "cap overflow must be reported: {rendered}"
        );
        assert!(
            rendered.contains("  unsupported extension: Makefile"),
            "unsupported files must be named: {rendered}"
        );
    }

    // ======================================================================
    // GAP-081: service_call edges from Go gRPC call sites
    // ======================================================================

    /// Every `service_call` edge in a warmed fixture as sorted (from, to).
    fn service_call_pairs(root: &Path) -> Vec<(String, String)> {
        let edges = root.join(".vfs").join("graph").join("edges.jsonl");
        let raw = std::fs::read_to_string(&edges)
            .unwrap_or_else(|_| panic!("edges.jsonl must exist at {}", edges.display()));
        let mut pairs: Vec<(String, String)> = raw
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<Edge>(l).ok())
            .filter(|e| e.rel == "service_call")
            .map(|e| (e.from, e.to))
            .collect();
        pairs.sort();
        pairs
    }

    /// GAP-081 fixture — the four shapes that decide the edge set:
    /// - `src/foo/server.go` registers `FooService` (the only provider);
    /// - `src/foo/client_wrapper.go` only DEFINES the client constructor
    ///   (`func NewFooServiceClient(`), so it must never be a caller;
    /// - `src/client1|2/main.go` construct a `FooService` client (the callers),
    ///   plus a `BarService` client with no provider anywhere (dead service —
    ///   must emit nothing);
    /// - `src/bar/genproto/demo_grpc.pb.go` is the protoc-gen-go decoy: it
    ///   carries both anchor forms (definitions AND an indented registration
    ///   inside a helper body), which is exactly how every real
    ///   `*/genproto/*_grpc.pb.go` declares every service in the project.
    fn write_grpc_fixture(root: &Path) {
        fs::create_dir_all(root.join("src/foo")).unwrap();
        fs::write(
            root.join("src/foo/server.go"),
            "package main\n\nfunc serve(s grpc.ServiceRegistrar, svc FooServiceServer) {\n\t\
             pb.RegisterFooServiceServer(s, svc)\n}\n",
        )
        .unwrap();
        fs::write(
            root.join("src/foo/client_wrapper.go"),
            "package main\n\nfunc NewFooServiceClient(cc grpc.ClientConnInterface) FooServiceClient {\n\t\
             return &fooServiceClient{cc}\n}\n",
        )
        .unwrap();

        fs::create_dir_all(root.join("src/bar/genproto")).unwrap();
        fs::write(
            root.join("src/bar/genproto/demo_grpc.pb.go"),
            "// Code generated by protoc-gen-go-grpc. DO NOT EDIT.\n\npackage genproto\n\n\
             func NewFooServiceClient(cc grpc.ClientConnInterface) FooServiceClient {\n\t\
             return &fooServiceClient{cc}\n}\n\n\
             func RegisterFooServiceServer(s grpc.ServiceRegistrar, srv FooServiceServer) {\n\t\
             RegisterFooServiceServer(s, srv)\n}\n",
        )
        .unwrap();

        for dir in ["src/client1", "src/client2"] {
            fs::create_dir_all(root.join(dir)).unwrap();
            fs::write(
                root.join(dir).join("main.go"),
                "package main\n\nfunc dial(conn grpc.ClientConnInterface) {\n\t\
                 cl := pb.NewFooServiceClient(conn)\n\t\
                 _ = pb.NewBarServiceClient(conn)\n\t_ = cl\n}\n",
            )
            .unwrap();
        }
    }

    fn warm_fixture(root: &Path, home: &Path) {
        run_warm_in(
            root,
            false,
            None,
            false,
            false,
            Some(home.to_path_buf()),
            &no_manifest,
        )
        .unwrap();
    }

    fn expected_foo_service_pairs() -> Vec<(String, String)> {
        vec![
            (
                "src/client1/main.go".to_string(),
                "src/foo/server.go".to_string(),
            ),
            (
                "src/client2/main.go".to_string(),
                "src/foo/server.go".to_string(),
            ),
        ]
    }

    /// Falsification: drop the generated-file exclusion and this assertion must
    /// fail with the decoy (`src/bar/genproto/demo_grpc.pb.go`) among the
    /// endpoints; drop the `func ` definition skip and it must fail with
    /// `src/foo/client_wrapper.go` as an extra caller.
    #[test]
    fn warm_emits_service_call_edges_from_go_grpc_call_sites() {
        let root = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        write_grpc_fixture(root.path());
        warm_fixture(root.path(), home.path());

        let pairs = service_call_pairs(root.path());
        assert_eq!(
            pairs,
            expected_foo_service_pairs(),
            "exactly the two caller->provider service_call edges are expected \
             (no decoy endpoint, no definition-line caller, no edge for the \
             provider-less BarService)"
        );

        let raw = std::fs::read_to_string(root.path().join(".vfs/graph/edges.jsonl")).unwrap();
        let decoy_lines: Vec<&str> = raw
            .lines()
            .filter(|l| l.contains("service_call") && l.contains("genproto"))
            .collect();
        assert!(
            decoy_lines.is_empty(),
            "generated decoys must never be service_call endpoints: {decoy_lines:?}"
        );
    }

    /// PERF-002 trap: a corpus warmed before this pass has a FULL parse cache
    /// and an edges.jsonl without `service_call` lines. The fast path must not
    /// skip the append in that state, and a re-warm that already carries the
    /// lines must stay idempotent.
    #[test]
    fn warm_adds_service_call_edges_when_parse_cache_is_full() {
        let root = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        write_grpc_fixture(root.path());
        warm_fixture(root.path(), home.path());
        assert_eq!(
            service_call_pairs(root.path()),
            expected_foo_service_pairs()
        );

        // Simulate the pre-change corpus: strip the service_call lines, keep
        // the parse cache (still complete — no file changed).
        let edges = root.path().join(".vfs").join("graph").join("edges.jsonl");
        let kept: String = std::fs::read_to_string(&edges)
            .unwrap()
            .lines()
            .filter(|l| !l.contains("\"service_call\""))
            .map(|l| format!("{l}\n"))
            .collect();
        std::fs::write(&edges, kept).unwrap();
        assert!(
            service_call_pairs(root.path()).is_empty(),
            "precondition: the stripped corpus carries no service_call edge"
        );

        // Full cache hit + missing service edges => must fall through.
        warm_fixture(root.path(), home.path());
        assert_eq!(
            service_call_pairs(root.path()),
            expected_foo_service_pairs(),
            "a full parse cache must not strand the service_call dimension"
        );

        // Third warm: everything present => still exactly the same two edges.
        warm_fixture(root.path(), home.path());
        assert_eq!(
            service_call_pairs(root.path()),
            expected_foo_service_pairs()
        );
    }

    #[test]
    fn generated_go_marker_is_found_in_the_file_head_not_just_line_one() {
        let licensed = format!(
            "{}// Code generated by protoc-gen-go-grpc. DO NOT EDIT.\npackage genproto\n",
            "// Copyright 2020 Google LLC\n".repeat(4)
        );
        assert!(is_generated_go(&licensed));
        assert!(is_generated_go(
            "// Code generated by protoc-gen-go. DO NOT EDIT.\npackage pb\n"
        ));
        assert!(!is_generated_go("package main\n\nfunc serve() {}\n"));
    }
}
