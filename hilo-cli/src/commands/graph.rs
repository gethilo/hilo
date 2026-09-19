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

    // GAP-086: warm requires a Hilo project root. Discovery used to run in
    // any directory and leave a partial `.vfs/graph/` behind (parse cache,
    // edges.jsonl, DuckDB cache) with no manifest and none of the standard
    // `.vfs/` layout — a tree that looks initialized but is not. Refuse
    // instead, naming the fix; `hilo init` creates the full layout.
    guard::ensure_project_root(cwd)?;

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

            // Parse with the ABSOLUTE path: rust/python import resolution
            // walks up from the given path (Cargo.toml / package __init__)
            // and must anchor at the warm root, not the process cwd — warm
            // with cwd != root used to resolve against the wrong tree and
            // silently yield 0 edges (found by the INV-001 regression test).
            let abs = file.canonicalize().unwrap_or_else(|_| file.clone());
            let mut edges = parser
                .parse_imports(&abs.to_string_lossy(), &source)
                .with_context(|| format!("failed to parse {rel}"))?;
            // Edge endpoints are graph-stable REL paths; the absolute parse
            // path leaks into from/to, so strip the root back off.
            for e in edges.iter_mut() {
                if let Ok(stripped) = Path::new(&e.from).strip_prefix(cwd) {
                    e.from = stripped.to_string_lossy().into_owned();
                }
                if let Ok(stripped) = Path::new(&e.to).strip_prefix(cwd) {
                    e.to = stripped.to_string_lossy().into_owned();
                }
            }

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

    let t_contract = std::time::Instant::now();
    // GAP-081 phase 2a: `service_contract` edges from the same callers to the
    // `.proto` that declares the service — the contract dimension that keeps
    // callers of an OUT-OF-REPO provider (C#, Node.js, Java, Python services)
    // visible, where phase 1's Go-provider resolution has nothing to point at.
    let contract_edges = discover_service_contracts(&source_files, cwd, &include_paths);
    all_edges.extend(contract_edges.iter().cloned());
    if std::env::var("HILO_WARM_TIMING").is_ok() {
        eprintln!("  [timing] service contracts: {:?}", t_contract.elapsed());
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
    // GAP-081 trap: "edges identical" only holds once the derived
    // `service_call` AND `service_contract` edges this pass computes are
    // ALREADY present in edges.jsonl. A corpus warmed before either pass
    // existed has a valid parse cache and none of those lines, so taking the
    // fast path there would strand the dimension forever (the cache would
    // never invalidate). Fall through to the append + DuckDB open while ANY
    // computed derived edge is absent; a re-warm of a corpus that already
    // carries them still skips.
    let derived_edges: [&[Edge]; 2] = [&service_edges, &contract_edges];
    let full_cache_hit = cached_n == total_files && total_files > 0;
    if full_cache_hit && count_new_service_edges(&edges_jsonl, &derived_edges) == 0 {
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

    // GAP-083: a reverse query on a FILE also matches dependents of the file's
    // crate `pkg:<name>` node (GAP-034). Those rows are crate-level, not
    // file-level, and without a summary "0 files import this" is
    // indistinguishable from "31 files import this file's crate". File rows
    // keep their existing rendering byte-for-byte; crate rows are re-labelled.
    let reverse_file_query = matches!(dir, hilo_graph::Direction::Reverse)
        && !path.starts_with("pkg:")
        && !path.starts_with("sys:");
    if reverse_file_query {
        let file_level = edges.iter().filter(|e| !e.to.starts_with("pkg:")).count();
        let mut pkgs: Vec<&str> = edges
            .iter()
            .filter(|e| e.to.starts_with("pkg:"))
            .map(|e| e.to.as_str())
            .collect();
        pkgs.sort_unstable();
        pkgs.dedup();
        let via = if pkgs.is_empty() {
            String::new()
        } else {
            format!(" via {}", pkgs.join(", "))
        };
        println!(
            "{file_level} direct (file-level) dependents; {crate_level} crate-level dependents{via}",
            crate_level = edges.len() - file_level
        );
    }

    for edge in &edges {
        // GAP-045: pkg:* targets are external-package pseudo-nodes, not
        // resolvable file paths — label them so agents don't try to pass
        // them to meta/impact (GAP-038 covered stats/search only).
        // GAP-083: in a reverse query on a file, a row pointing at the file's
        // `pkg:<crate>` node is CRATE-level (GAP-034 resolution), not an
        // external package — the `[external package]` label hid that and made
        // the row read as a file-level dependent.
        let label = if reverse_file_query && edge.to.starts_with("pkg:") {
            format!(" [crate-level {}]", edge.to)
        } else if edge.from.starts_with("pkg:") || edge.to.starts_with("pkg:") {
            " [external package]".to_string()
        } else {
            String::new()
        };
        println!(
            "{}  →  {}  ({})  [{} conf={:.2}]{}",
            edge.from, edge.to, edge.rel, edge.provenance, edge.confidence, label
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
                    // GAP-083: every row states its scope, so a crate-level
                    // match can never be read as a file-level dependent.
                    let via = file
                        .via
                        .as_deref()
                        .map(|v| format!(", via {v}"))
                        .unwrap_or_default();
                    println!(
                        "{}  ←  {}  (depth: {}, scope={}{})  [{} conf={:.2}]",
                        file.path, file.relation, file.depth, file.scope, via, prov, conf
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

/// Relation emitted by [`discover_service_contracts`] (GAP-081 phase 2a).
///
/// A distinct claim from `service_call`: it says "this file calls a service
/// whose contract is declared here", and asserts nothing about any in-repo
/// file serving it.
const SERVICE_CONTRACT_REL: &str = "service_contract";

/// Provenance stamp for the proto-contract pass (GAP-081 phase 2a).
///
/// Fixed for the same reason as [`SERVICE_CALL_PROVENANCE`] —
/// `(from, to, rel, provenance)` is the dedupe key of
/// `inventory::append_edges_deduped`, so a stable stamp makes the pass
/// idempotent across re-warms. It is also the discriminator a consumer uses to
/// tell a contract-only link (`grpc_proto`) apart from a resolved in-repo
/// provider link (`grpc_ast`).
const SERVICE_CONTRACT_PROVENANCE: &str = "grpc_proto";

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

/// Header markers of a generated protobuf/gRPC stub in ANY language
/// (GAP-081 phase 2b/2c). Case-sensitive, searched in the file head.
///
/// `<auto-generated>` is the canonical C#/VB.NET codegen banner: protoc's C#
/// output (`*.cs`, and therefore a vendored `*Grpc.cs`) carries it and does
/// NOT necessarily carry `DO NOT EDIT`. `classify.rs` already keys on the bare
/// `auto-generated` substring for the same reason — the angle brackets are
/// what makes this check specific to a banner and not to prose that happens to
/// mention the phrase.
const STUB_GENERATED_MARKERS: [&str; 6] = [
    "Code generated by protoc-gen-go",
    "DO NOT EDIT",
    "Generated by the protocol buffer compiler",
    "Generated by the gRPC Python protocol compiler plugin",
    "@generated",
    "<auto-generated>",
];

/// How many leading lines are searched for a generated-stub banner — same
/// reasoning as [`GO_GENERATED_HEAD_LINES`]: in most checked-in stubs the
/// banner follows a license block, so line 1 alone is not enough.
const STUB_GENERATED_HEAD_LINES: usize = 60;

/// File-name suffixes of generated stubs, one per protoc/gRPC codegen plugin.
///
/// The NAME is the second, independent discriminator: a vendored
/// `*_pb2_grpc.py` whose banner was stripped (or reworded) is still generated
/// code. `*.pb.go` is in the table for completeness — `.go` files never reach
/// [`is_generated_stub`] ([`is_generated_go`] governs them) — so that row is
/// pinned by a unit test rather than exercised by the scan.
const STUB_GENERATED_NAME_SUFFIXES: [&str; 8] = [
    "_pb2_grpc.py",
    "_pb2.py",
    "_grpc_pb.js",
    "_pb.js",
    "ServiceClientPb.ts",
    "ServiceClientPb.js",
    "Grpc.java",
    ".pb.go",
];

/// True when `rel` looks like a generated protobuf/gRPC stub.
///
/// Either discriminator is sufficient: the banner catches a generated file
/// under a name this table does not know, the file name catches a vendored
/// stub whose banner was stripped. Both are real cases, which is why the check
/// is an `or` and not an `and`.
fn is_generated_stub(rel: &Path, text: &str) -> bool {
    let named = rel
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| {
            STUB_GENERATED_NAME_SUFFIXES
                .iter()
                .any(|suffix| name.ends_with(suffix))
        })
        .unwrap_or(false);
    named
        || text.lines().take(STUB_GENERATED_HEAD_LINES).any(|line| {
            STUB_GENERATED_MARKERS
                .iter()
                .any(|marker| line.contains(marker))
        })
}

/// GAP-081 phase 2b: the generated-stub anchor form a non-Go language spells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StubAnchorForm {
    /// Python: `demo_pb2_grpc.EmailServiceStub(channel)`.
    PythonStub,
    /// Java/Kotlin: `AdServiceGrpc.newBlockingStub(channel)` — also `.newStub(`
    /// and `.newFutureStub(`.
    JavaGrpcFactory,
    /// C#/TS/JS: `new CartServiceClient(channel)` — the C# constructor form,
    /// which TypeScript/JavaScript spellings of the gRPC-web generated client
    /// also use (`.ts`, `.tsx`, `.js`, `.jsx`, and since phase 2c the ESM/CJS
    /// module flavours `.mjs`, `.cjs`).
    NewServiceClient,
}

/// Which anchor forms apply to a scanned extension — `&[]` for an extension
/// outside the phase 2b language set.
///
/// The form/language pairing is load-bearing, not cosmetic. The Java field
/// declaration `hipstershop.AdServiceGrpc.AdServiceBlockingStub blockingStub;`
/// matches NO form (each form requires the factory/constructor call — the
/// declaration is not a call site), and `new AdServiceClient(host, port)` —
/// the call site of a Java wrapper class that is not a stub — matches
/// [`StubAnchorForm::NewServiceClient`] only, so registering that form for
/// `.java` would count wrapper constructors as callers of `AdService`.
///
/// GAP-081 phase 2c: `.mjs`/`.cjs` take the C#/TS/JS form because they are
/// JavaScript — the same `new <Name>ServiceClient(` gRPC-web call site its
/// `.js` twin spells. The pairing is unchanged; only the extension set grew,
/// in lockstep with `Language::from_extension`, which now admits those files
/// to discovery at all (before, this table could not be reached for them
/// because the file was never in `source_files`).
fn stub_anchor_forms(ext: &str) -> &'static [StubAnchorForm] {
    match ext {
        "py" => &[StubAnchorForm::PythonStub],
        "java" | "kt" => &[StubAnchorForm::JavaGrpcFactory],
        "cs" | "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => &[StubAnchorForm::NewServiceClient],
        _ => &[],
    }
}

/// Whole-line comment prefixes of the phase 2b languages — `#` (Python),
/// `//` (all of them), `/*` and the `*` continuation of a C-family block
/// comment.
///
/// A commented-out call site is not a call site: excluding it is the same
/// false-attribution guard as the generated-file predicate and the Go `func `
/// skip. Only whole lines are tested, so a trailing comment after real code
/// cannot hide an anchor.
const NON_GO_COMMENT_PREFIXES: [&str; 4] = ["#", "//", "/*", "*"];

fn is_non_go_comment(line: &str) -> bool {
    let trimmed = line.trim_start();
    NON_GO_COMMENT_PREFIXES
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
}

/// The compiled phase 2b anchor regexes.
///
/// Every capture group 1 is the FULL service name as spelled in the call:
/// `([A-Za-z][A-Za-z0-9]*Service)` plus the form's own suffix. That is
/// deliberately NOT the Go shape — the Go forms capture a stem that EXCLUDES
/// `Service` and append it (`New<Stem>ServiceClient(` -> `<Stem>Service`),
/// while `<Name>ServiceStub(` already ends in `Service`. Reusing the Go shape
/// for these forms yields `RecommendationServiceService`, which matches no
/// declared service and silently turns the whole pass into a no-op on exactly
/// the polyglot repos it exists for.
struct NonGoAnchorRes {
    /// `<Name>ServiceStub(`
    python_stub: Regex,
    /// `<Name>ServiceGrpc.new(Blocking|Future)?Stub(`
    java_factory: Regex,
    /// `new <Name>ServiceClient(`
    new_client: Regex,
}

impl NonGoAnchorRes {
    fn new() -> Self {
        Self {
            python_stub: Regex::new(r"([A-Za-z][A-Za-z0-9]*Service)Stub\(")
                .expect("static python stub-anchor regex must compile"),
            java_factory: Regex::new(
                r"([A-Za-z][A-Za-z0-9]*Service)Grpc\.new(?:Blocking|Future)?Stub\(",
            )
            .expect("static java stub-anchor regex must compile"),
            new_client: Regex::new(r"\bnew\s+([A-Za-z][A-Za-z0-9]*Service)Client\(")
                .expect("static C#/TS/JS stub-anchor regex must compile"),
        }
    }

    fn re(&self, form: StubAnchorForm) -> &Regex {
        match form {
            StubAnchorForm::PythonStub => &self.python_stub,
            StubAnchorForm::JavaGrpcFactory => &self.java_factory,
            StubAnchorForm::NewServiceClient => &self.new_client,
        }
    }
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
    let sites = collect_service_call_sites(source_files, cwd);
    let callers = sites.callers;
    let providers = sites.providers;

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

/// GAP-081: the gRPC call sites one scan of `source_files` yields.
///
/// Both dimensions of GAP-081 are derived from this map — phase 1
/// ([`discover_service_calls`]) links a caller to the in-repo provider of the
/// same service, phase 2a ([`discover_service_contracts`]) links the same
/// caller to the `.proto` declaring that service — so the scan is written
/// once. Two independent walks would be two chances for the dimensions to
/// disagree about who calls what.
#[derive(Debug, Default)]
struct ServiceCallSites {
    /// service name (e.g. `CartService`) -> files that call it.
    callers: HashMap<String, HashSet<String>>,
    /// service name -> files that serve it.
    providers: HashMap<String, HashSet<String>>,
}

/// GAP-081: scan the discovered files for gRPC call-site anchors.
///
/// Two families, one walk, one map:
///
/// - **Go** (`grpc_ast`, phase 1): `New<Stem>ServiceClient(` marks a caller of
///   `<Stem>Service`, `Register<Stem>ServiceServer(` marks a provider of it.
///   Definitions are never call sites (see the skip below), and `protoc-gen-go`
///   output is excluded entirely — it declares both anchors for EVERY service
///   in the project, so counting it would attach every caller to every stub
///   file.
/// - **Non-Go** (phase 2b/2c): the generated-stub caller anchor of `py`,
///   `java`, `kt`, `cs`, `ts`, `tsx`, `js`, `jsx`, `mjs`, `cjs` — see
///   [`StubAnchorForm`] for the forms and [`stub_anchor_forms`] for which
///   language spells which form.
///
/// Non-Go anchors yield CALLERS ONLY, never providers. The file that serves a
/// non-Go service declares `<X>ServiceServicer` (Python) or a
/// `Register<X>ServiceServer` equivalent, and every such declaration in a repo
/// like microservices-demo lives in the generated stub file this scan excludes
/// — so a non-Go provider claim would have no evidence behind it. Only the Go
/// family emits providers.
///
/// Both families write into the SAME [`ServiceCallSites::callers`] map, which
/// is the point of that type: phase 1 ([`discover_service_calls`]) and the
/// contract pass ([`discover_service_contracts`]) become polyglot with no
/// further wiring, and cannot disagree about who calls what. Paths stay
/// repo-relative, the emitted provenance stamps stay fixed constants, and the
/// scan reads each file once — the two families are disjoint by extension, so
/// no path is ever read twice.
fn collect_service_call_sites(source_files: &[PathBuf], cwd: &Path) -> ServiceCallSites {
    // `[A-Za-z][A-Za-z0-9]*` keeps the stem an identifier: no leading digit,
    // no greedy match across a dotted selector.
    let caller_re = Regex::new(r"New([A-Za-z][A-Za-z0-9]*)ServiceClient\(")
        .expect("static caller regex must compile");
    let provider_re = Regex::new(r"Register([A-Za-z][A-Za-z0-9]*)ServiceServer\(")
        .expect("static provider regex must compile");
    let non_go = NonGoAnchorRes::new();

    let mut sites = ServiceCallSites::default();

    for file in source_files {
        let rel = file.strip_prefix(cwd).unwrap_or(file);
        let Some(ext) = rel.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        let is_go = ext == "go";
        let forms = stub_anchor_forms(ext);
        if !is_go && forms.is_empty() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        let rel_str = rel.to_string_lossy().into_owned();

        if !is_go {
            // A generated stub declares `<X>ServiceStub` for EVERY service in
            // the project, so counting it would make it a dependent of all of
            // them — the same false-positive class the Go skip above kills.
            if is_generated_stub(rel, &text) {
                continue;
            }
            for line in text.lines() {
                if is_non_go_comment(line) {
                    continue;
                }
                for form in forms {
                    for cap in non_go.re(*form).captures_iter(line) {
                        sites
                            .callers
                            .entry(cap[1].to_string())
                            .or_default()
                            .insert(rel_str.clone());
                    }
                }
            }
            continue;
        }

        if is_generated_go(&text) {
            continue;
        }
        for line in text.lines() {
            // A definition is never a call site — belt and braces with the
            // generated-file check above, so a hand-written (or unrecognised)
            // stub file cannot turn its own declarations into dependents.
            if line.trim_start().starts_with("func ") {
                continue;
            }
            for cap in caller_re.captures_iter(line) {
                let service = format!("{}Service", &cap[1]);
                sites
                    .callers
                    .entry(service)
                    .or_default()
                    .insert(rel_str.clone());
            }
            for cap in provider_re.captures_iter(line) {
                let service = format!("{}Service", &cap[1]);
                sites
                    .providers
                    .entry(service)
                    .or_default()
                    .insert(rel_str.clone());
            }
        }
    }

    sites
}

/// GAP-081: how many of the derived service-dimension edges are missing from
/// `edges_jsonl`.
///
/// `edge_sets` carries one slice per derived dimension — phase 1's
/// `service_call` edges and phase 2a's `service_contract` edges — and EVERY
/// dimension counts: the PERF-002 fast path may only be taken when the append
/// would write nothing for ANY of them (see the caller).
///
/// Uses the same `(from, to, rel, provenance)` identity as
/// `inventory::append_edges_deduped`, so "0 missing" means the append would
/// write nothing — the only condition under which the PERF-002 full-cache-hit
/// fast path is safe once these passes exist. A missing/unreadable file counts
/// every edge as new (there is nothing to trust).
fn count_new_service_edges(edges_jsonl: &Path, edge_sets: &[&[Edge]]) -> usize {
    let derived: Vec<&Edge> = edge_sets.iter().flat_map(|set| set.iter()).collect();
    if derived.is_empty() {
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
    derived
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

/// GAP-081 phase 2a: `service <Name>` declarations in the repo's `.proto`
/// files, keyed by service name (value = the declaring proto paths, repo
/// relative with `/` separators, like every other edge path here).
///
/// `.proto` is not one of the 26 AST languages, so a proto file is never part
/// of `source_files` (`collect_source_files` filters on
/// `Language::from_extension`); this walk is the only place they are found.
/// It therefore repeats that walk's rules rather than inventing its own: it
/// descends with the same `exclusion_category` / `entry_excluded` /
/// `is_included` / `is_include_ancestor` semantics, so the contract dimension
/// can never see a file the rest of the warm deliberately pruned — hidden
/// entries included, which is what keeps `.vfs`/`.git` (and therefore the
/// graph's own bookkeeping) out of the walk.
fn discover_proto_services(
    cwd: &Path,
    include_paths: &[String],
) -> BTreeMap<String, BTreeSet<String>> {
    // Matched per line (no multiline flag), so `^` is the line start: the
    // declaration must OPEN the line, modulo indentation. `[A-Za-z_]` first
    // keeps the name an identifier.
    let service_re = Regex::new(r"^\s*service\s+([A-Za-z_][A-Za-z0-9_]*)\s*\{")
        .expect("static service-declaration regex must compile");

    let mut protos: Vec<PathBuf> = Vec::new();
    if collect_proto_files(cwd, Path::new(""), None, include_paths, &mut protos).is_err() {
        // An unreadable tree yields no contracts, never a failed warm: this
        // dimension is additive and must not be able to fail the pass.
        return BTreeMap::new();
    }
    protos.sort();

    let mut declared: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for proto in &protos {
        let Ok(text) = std::fs::read_to_string(proto) else {
            continue;
        };
        let rel = proto
            .strip_prefix(cwd)
            .unwrap_or(proto)
            .to_string_lossy()
            .replace('\\', "/");
        for line in text.lines() {
            // A commented-out declaration is not a declaration.
            if line.trim_start().starts_with("//") {
                continue;
            }
            if let Some(cap) = service_re.captures(line) {
                declared
                    .entry(cap[1].to_string())
                    .or_default()
                    .insert(rel.clone());
            }
        }
    }

    declared
}

/// GAP-081 phase 2a: collect `*.proto` files under `dir` with the SAME pruning
/// semantics as [`collect_source_files`] — same descend rule, same take rule,
/// same `include_paths` re-include, differing only in which extensions are
/// kept (`proto` instead of the 26 AST languages).
fn collect_proto_files(
    dir: &Path,
    rel: &Path,
    excluded_category: Option<&'static str>,
    include_paths: &[String],
    out: &mut Vec<PathBuf>,
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
                continue;
            }
            let child_category = if included {
                None
            } else {
                excluded_category.or(entry_category)
            };
            collect_proto_files(&path, &entry_rel, child_category, include_paths, out)?;
        } else if ft.is_file() {
            if path.extension().and_then(|e| e.to_str()) != Some("proto") {
                continue;
            }
            let entry_category = exclusion_category(&rel_str, &name_str);
            let take = is_included(&rel_str, include_paths)
                || (entry_category.is_none() && excluded_category.is_none());
            if take {
                out.push(path);
            }
        }
    }
    Ok(())
}

/// GAP-081 phase 2a: emit `service_contract` edges from RPC callers to the
/// `.proto` file that declares the service they call.
///
/// Why this dimension exists: a `service_call` edge (phase 1) needs a provider
/// file IN THIS REPO. In a polyglot microservice corpus that is the exception,
/// not the rule — microservices-demo's CartService is served by C#,
/// CurrencyService and PaymentService by Node.js, AdService by Java, and
/// RecommendationService / EmailService by Python — so phase 1 emits ZERO
/// edges for every caller of those services and `graph impact <service>` still
/// answers "No dependents found".
///
/// An import edge cannot express the relation either: the C# provider is not a
/// node in this repo. What both sides DO share, and what IS in the repo, is the
/// contract — the `.proto` file that declares `service X`, i.e. the head of the
/// generated stub both sides compile against. The caller is therefore recorded
/// as a dependent OF THE CONTRACT:
///
/// ```text
/// from = caller file, to = declaring .proto, rel = "service_contract",
/// provenance = "grpc_proto", confidence = 1.0
/// ```
///
/// which is what makes `hilo graph impact <proto file>` answer "who calls this
/// service" for a provider that lives outside the repo. `grpc_proto` is also
/// how a consumer tells a contract-only link apart from a resolved provider
/// link (`grpc_ast`); the two edges coexist when the provider IS in the repo,
/// because they make different claims.
///
/// The caller map is phase 1's, reused verbatim (same scan, same
/// `New<Stem>ServiceClient(` anchor — plus, since phase 2b, the generated-stub
/// anchors of the non-Go languages, which is what makes this dimension
/// polyglot: the C#/Java/Python/Node service in that corpus is served OUTSIDE
/// this repo, so its callers only ever had a contract to point at).
/// Only a service with at least one caller
/// yields edges — a declared service nobody calls is never given a fabricated
/// dependent. When several proto files declare the same service, EVERY
/// declaring file gets an edge: all of them really do declare it (checked-in
/// copies are the norm — `./protos/demo.proto` plus
/// `src/{currencyservice,paymentservice,adservice}/**/demo.proto`), and
/// inventing a "canonical copy" preference would pick a winner the repo does
/// not name. One edge per distinct (caller, proto) pair, emitted in sorted
/// order, like every other pass in this file.
fn discover_service_contracts(
    source_files: &[PathBuf],
    cwd: &Path,
    include_paths: &[String],
) -> Vec<Edge> {
    let sites = collect_service_call_sites(source_files, cwd);
    if sites.callers.is_empty() {
        return Vec::new();
    }
    let declared = discover_proto_services(cwd, include_paths);

    let mut pairs: BTreeSet<(String, String)> = BTreeSet::new();
    for (service, caller_files) in &sites.callers {
        let Some(proto_files) = declared.get(service) else {
            continue;
        };
        for caller in caller_files {
            for proto in proto_files {
                pairs.insert((caller.clone(), proto.clone()));
            }
        }
    }

    pairs
        .into_iter()
        .map(|(from, to)| Edge {
            from,
            to,
            rel: SERVICE_CONTRACT_REL.to_string(),
            provenance: SERVICE_CONTRACT_PROVENANCE.to_string(),
            confidence: 1.0,
        })
        .collect()
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

/// Load the manifest from the first available path.
///
/// The candidate list is shared with the project precondition
/// ([`guard::ensure_project_root`]) so a directory warm accepts can never
/// disagree with a directory the loader can read.
fn load_manifest() -> Result<hilo_core::manifest::Manifest> {
    for path in guard::MANIFEST_PATHS {
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
    for path in guard::MANIFEST_PATHS {
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
///
/// GAP-085: an unknown prefix fails loudly, mirroring the GAP-039 contract in
/// [`run_impact`]. A prefix that is neither a directory on disk nor named by
/// the graph is almost always an agent typo, and the old success-shaped
/// `Files: 0` report was indistinguishable from a real but empty module — the
/// command now exits non-zero with an error naming the prefix. The
/// classification runs BEFORE the "No graph data" path, so a fresh, unwarmed
/// directory gets the honest error rather than a missing-graph message.
/// Tolerance: a prefix the warmed graph still names (e.g. the directory was
/// deleted after a `warm`) keeps reporting normally. A real directory with no
/// warmed coverage stays exit 0 but always states the situation explicitly —
/// never a bare `Files: 0` report.
pub fn run_module(module_name: &str) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to determine the current directory")?;

    // GAP-085: classify the prefix against the filesystem first — this must
    // not sit behind the graph-db resolution, or an unwarmed directory answers
    // a typo with "No graph data".
    let empty = classify_empty_module(&cwd, module_name);

    let Some(graph_db) = resolve_graph_db_path(&cwd) else {
        if empty == EmptyModule::UnknownPrefix {
            return Err(unknown_module_error(module_name));
        }
        anyhow::bail!("No graph data. Run `hilo graph warm` first.");
    };

    let graph_db_str = graph_db.to_str().unwrap_or(".vfs/graph/graph.db");
    let graph = GraphDB::open(graph_db_str).context("failed to open DuckDB graph database")?;

    let stats = graph
        .module_files_at(&cwd, module_name)
        .context("failed to query module stats")?;

    // GAP-085: an unknown prefix must never print a success-shaped empty
    // report. A prefix the graph still names is reported normally (tolerance
    // for a directory deleted after a warm).
    if stats.files.is_empty() && empty == EmptyModule::UnknownPrefix {
        return Err(unknown_module_error(module_name));
    }

    println!("Module: {}", stats.module);
    println!("Files:  {}", stats.files.len());
    println!("Edges:  {}", stats.edges_count);
    println!("Tests:  {:.1}%", stats.test_coverage_pct);

    if stats.files.is_empty() {
        // GAP-085: state-independent empty report — the directory exists but
        // nothing under it is in the graph yet, so this can never read as an
        // empty module.
        println!(
            "No files in the graph under '{module_name}' — the directory exists but no warmed graph entry covers it. Run `hilo graph warm`."
        );
        return Ok(());
    }

    println!("── Files ──");
    for f in &stats.files {
        println!("  {f}");
    }

    Ok(())
}

/// GAP-085: why `hilo graph module <prefix>` found no files in the graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EmptyModule {
    /// The prefix is not a directory on disk and the graph cannot answer for
    /// it either — an unknown prefix, i.e. almost always a typo. Loud failure.
    UnknownPrefix,
    /// The directory exists but no warmed graph entry covers it. Honest empty
    /// report, exit 0.
    UncoveredDirectory,
}

/// GAP-085: classify an empty `hilo graph module` result WITHOUT touching the
/// graph, so the decision is unit-testable without DuckDB.
///
/// `root` is the resolution root the prefix is interpreted against (the
/// process cwd for the CLI).
fn classify_empty_module(root: &Path, module_name: &str) -> EmptyModule {
    if root.join(module_name).is_dir() {
        EmptyModule::UncoveredDirectory
    } else {
        EmptyModule::UnknownPrefix
    }
}

/// GAP-085: the loud failure for an unknown module prefix, naming the prefix.
fn unknown_module_error(module_name: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "'{module_name}' is not in the graph (no such directory and no matching module prefix)"
    )
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

    /// GAP-085: a prefix that is neither a directory on disk nor named by the
    /// graph is an unknown prefix — classifiable without DuckDB, so the
    /// command can fail loudly before it queries anything.
    #[test]
    fn classify_empty_module_unknown_prefix() {
        let dir = TempDir::new().unwrap();

        assert_eq!(
            classify_empty_module(dir.path(), "no/such/dir"),
            EmptyModule::UnknownPrefix
        );

        let err = unknown_module_error("no/such/dir");
        assert!(
            err.to_string().contains("no/such/dir"),
            "error must name the prefix, got: {err}"
        );
    }

    /// GAP-085: a real directory with no warmed graph entries is NOT an
    /// unknown prefix — the directory itself is not an error, it just has no
    /// coverage yet.
    #[test]
    fn classify_empty_module_uncovered_directory() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("src").join("nested")).unwrap();

        assert_eq!(
            classify_empty_module(dir.path(), "src"),
            EmptyModule::UncoveredDirectory
        );
        assert_eq!(
            classify_empty_module(dir.path(), "src/nested"),
            EmptyModule::UncoveredDirectory
        );
        // The root itself is a directory too — never classified as unknown.
        assert_eq!(
            classify_empty_module(dir.path(), "."),
            EmptyModule::UncoveredDirectory
        );
    }

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

    /// GAP-086: warm requires a project root, so every fixture that expects
    /// warm to proceed must look like a project. Writes the manifest `hilo
    /// init` produces (`.vfs/manifest.yaml`) and lets the code under test
    /// create the rest of the `.vfs/` layout.
    fn write_fixture_manifest(root: &Path) {
        std::fs::create_dir_all(root.join(".vfs")).unwrap();
        std::fs::write(
            root.join(".vfs").join("manifest.yaml"),
            "version: 2\nproject:\n  name: fixture\n",
        )
        .unwrap();
    }

    /// Warm a fixture project that already has a manifest on disk.
    fn warm_project_fixture(
        root: &Path,
        home: &Path,
        load_manifest_fn: &dyn Fn(&Path) -> Result<hilo_core::manifest::Manifest>,
    ) -> Result<()> {
        write_fixture_manifest(root);
        run_warm_in(
            root,
            false,
            None,
            false,
            false,
            Some(home.to_path_buf()),
            load_manifest_fn,
        )
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
        write_fixture_manifest(home.path());

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

        warm_project_fixture(project.path(), home.path(), &no_manifest).unwrap();
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

        warm_project_fixture(project.path(), home.path(), &loader).unwrap();

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
    // GAP-086: warm requires a project root (manifest precondition)
    // ======================================================================

    #[test]
    fn warm_without_manifest_refuses_and_creates_no_state() {
        let home = TempDir::new().unwrap();
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "fn lib() {}\n").unwrap();

        let err = run_warm_in(
            dir.path(),
            false,
            None,
            false,
            false,
            Some(home.path().to_path_buf()),
            &no_manifest,
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("hilo init"), "error must name the fix: {msg}");
        assert!(
            !dir.path().join(".vfs").exists(),
            "a refused warm must not scatter .vfs state into a non-project tree"
        );
    }

    #[test]
    fn warm_allow_home_does_not_bypass_the_project_gate() {
        // --allow-home overrides the HOME refusal only; a directory that is
        // not a project is still refused, with the same actionable error.
        let home = TempDir::new().unwrap();
        std::fs::write(home.path().join("keep.rs"), "fn keep() {}\n").unwrap();

        let err = run_warm_in(
            home.path(),
            false,
            None,
            false,
            true, // --allow-home
            Some(home.path().to_path_buf()),
            &no_manifest,
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("hilo init"), "error must name the fix: {msg}");
        assert!(
            !msg.contains("--allow-home"),
            "the project gate, not the HOME gate, must be the failure here: {msg}"
        );
        assert!(!home.path().join(".vfs").exists());
    }

    #[test]
    fn warm_accepts_root_level_manifest_project() {
        // A root-level `manifest.yaml` (no `.vfs/` yet) is a valid project —
        // it is the older layout the CLI's manifest lookup still honors.
        let home = TempDir::new().unwrap();
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "fn lib() {}\n").unwrap();
        std::fs::write(
            dir.path().join("manifest.yaml"),
            "version: 2\nproject:\n  name: legacy\n",
        )
        .unwrap();

        run_warm_in(
            dir.path(),
            false,
            None,
            false,
            false,
            Some(home.path().to_path_buf()),
            &no_manifest,
        )
        .unwrap();
        assert_eq!(warm_cache_keys(dir.path()), vec!["lib.rs".to_string()]);
    }

    // ======================================================================
    // INV-001: the tracked `.vfs/graph/edges.jsonl` inventory contract
    // ======================================================================

    /// Sorted non-empty JSONL lines of the fixture's edge inventory (order is
    /// not part of the contract — warm parses in parallel).
    fn edges_jsonl_lines(root: &Path) -> Vec<String> {
        let path = root.join(".vfs").join("graph").join("edges.jsonl");
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("edges.jsonl must exist at {}", path.display()));
        let mut lines: Vec<String> = raw
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(str::to_string)
            .collect();
        lines.sort();
        lines
    }

    /// INV-001 contract, pinned against the real warm path:
    ///
    /// 1. A warm that parses anything writes the discovered, deduplicated
    ///    edges into `.vfs/graph/edges.jsonl` — the tracked inventory, not a
    ///    throwaway cache.
    /// 2. A fully-cached warm (nothing re-parsed, no missing derived edges)
    ///    leaves the inventory untouched.
    /// 3. A warm with a parse-cache miss appends to whatever is already in
    ///    the inventory: pre-existing (e.g. hand-maintained) lines survive
    ///    verbatim, newly discovered edges are added once, and no line is
    ///    ever duplicated — the append-only dedup by
    ///    `(from, to, rel, provenance)` is what keeps a committed inventory
    ///    drift-free across repeated warms.
    #[test]
    fn warm_refreshes_tracked_edges_jsonl_append_only_and_rewarm_adds_no_duplicates() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        // A minimal Cargo package so warm discovers a real `imports` edge:
        // rust import resolution (RustModuleCtx) only builds inside a
        // package root with src/main.rs collecting `mod` declarations —
        // bare files at a temp root yield "no imports" (verified live),
        // which made this test's step (1) unsatisfiable as first written.
        std::fs::write(
            project.path().join("Cargo.toml"),
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(project.path().join("src")).unwrap();
        std::fs::write(
            project.path().join("src").join("lib.rs"),
            "pub fn lib() {}\n",
        )
        .unwrap();
        std::fs::write(
            project.path().join("src").join("main.rs"),
            "mod lib;\nuse crate::lib::lib;\nfn main() { lib(); }\n",
        )
        .unwrap();

        warm_project_fixture(project.path(), home.path(), &no_manifest).unwrap();

        // (0) INV-001: the tracked inventory exists after a parsing warm,
        // even if this fixture yielded zero edges.
        assert!(
            project
                .path()
                .join(".vfs")
                .join("graph")
                .join("edges.jsonl")
                .exists(),
            "a parsing warm must materialize the tracked edges.jsonl inventory"
        );

        // (1) The tracked inventory now carries the warm-discovered edges.
        let after_first = edges_jsonl_lines(project.path());
        assert!(
            after_first
                .iter()
                .any(|l| l.contains("\"rel\":\"imports\"")),
            "first warm must persist discovered edges into the tracked inventory: {after_first:?}"
        );

        // (2) A fully-cached re-warm must not touch the inventory.
        warm_project_fixture(project.path(), home.path(), &no_manifest).unwrap();
        assert_eq!(
            edges_jsonl_lines(project.path()),
            after_first,
            "a fully-cached warm must leave the tracked inventory unchanged"
        );

        // (3a) A parse-cache miss re-parses and appends — pre-existing lines
        // survive verbatim and nothing duplicates. Seed the inventory with a
        // single hand-written sentinel line (the drifted/hand-maintained
        // shape), wipe only the parse cache, and warm again.
        let edges = project
            .path()
            .join(".vfs")
            .join("graph")
            .join("edges.jsonl");
        let sentinel = serde_json::to_string(&Edge {
            from: "main.rs".to_string(),
            to: "sys:manual".to_string(),
            rel: "imports".to_string(),
            provenance: "manual".to_string(),
            confidence: 1.0,
        })
        .unwrap();
        std::fs::write(&edges, format!("{sentinel}\n")).unwrap();
        std::fs::remove_file(
            project
                .path()
                .join(".vfs")
                .join("graph")
                .join(".parse_cache.json"),
        )
        .unwrap();

        warm_project_fixture(project.path(), home.path(), &no_manifest).unwrap();

        let mut expected = after_first.clone();
        expected.push(sentinel);
        expected.sort();
        assert_eq!(
            edges_jsonl_lines(project.path()),
            expected,
            "cache-miss warm must re-append the discovered edges next to the \
             surviving sentinel line, exactly once each"
        );

        // (3b) And it stays idempotent: another cache-miss warm must add no
        // duplicate lines to the tracked inventory.
        std::fs::remove_file(
            project
                .path()
                .join(".vfs")
                .join("graph")
                .join(".parse_cache.json"),
        )
        .unwrap();
        warm_project_fixture(project.path(), home.path(), &no_manifest).unwrap();
        assert_eq!(
            edges_jsonl_lines(project.path()),
            expected,
            "repeated cache-miss warms must not duplicate tracked inventory lines"
        );

        // The rebuildable cache artifacts warm leaves behind are the
        // gitignored companions of the tracked inventory (never committed).
        assert!(
            project
                .path()
                .join(".vfs")
                .join("graph")
                .join(".parse_cache.json")
                .exists(),
            "parse cache is a warm cache artifact"
        );
        assert!(
            project
                .path()
                .join(".vfs")
                .join("graph")
                .join(".last_warm")
                .exists(),
            "last-warm marker is a warm cache artifact"
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
        // GAP-086: warm requires a project root; these fixtures are projects.
        write_fixture_manifest(root);
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

    // ======================================================================
    // GAP-081 phase 2a: service_contract edges from proto service declarations
    // ======================================================================

    /// Every `service_contract` edge in a warmed fixture as sorted (from, to).
    fn service_contract_pairs(root: &Path) -> Vec<(String, String)> {
        let edges = root.join(".vfs").join("graph").join("edges.jsonl");
        let raw = std::fs::read_to_string(&edges)
            .unwrap_or_else(|_| panic!("edges.jsonl must exist at {}", edges.display()));
        let mut pairs: Vec<(String, String)> = raw
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<Edge>(l).ok())
            .filter(|e| e.rel == "service_contract")
            .map(|e| (e.from, e.to))
            .collect();
        pairs.sort();
        pairs
    }

    /// Write a `.proto` file (creating parents) under the fixture root.
    fn write_proto(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
    }

    /// The two contract edges implied by the phase-1 Go fixture: both callers
    /// of `FooService` point at the proto that declares it. `BarService` has
    /// callers but no declaration anywhere, so it can never produce one.
    fn expected_foo_contract_pairs(proto: &str) -> Vec<(String, String)> {
        vec![
            ("src/client1/main.go".to_string(), proto.to_string()),
            ("src/client2/main.go".to_string(), proto.to_string()),
        ]
    }

    /// Falsification: drop the `//` comment skip and the `BarService` pair
    /// appears; drop the caller-map reuse and every expected pair vanishes;
    /// count a declaration per line without the anchor form and `service Foo`
    /// inside the rpc body would leak in.
    #[test]
    fn warm_emits_service_contract_edges_for_proto_service_declarations() {
        let root = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        write_grpc_fixture(root.path());
        write_proto(
            root.path(),
            "protos/demo.proto",
            "syntax = \"proto3\";\n\npackage demo;\n\n\
             // service CommentedOut {\n\
             service FooService {\n  rpc GetFoo(FooRequest) returns (FooResponse);\n}\n",
        );
        warm_fixture(root.path(), home.path());

        assert_eq!(
            service_contract_pairs(root.path()),
            expected_foo_contract_pairs("protos/demo.proto"),
            "one contract edge per (caller, declaring proto) pair"
        );

        // The phase-1 dimension is untouched by the new one.
        assert_eq!(
            service_call_pairs(root.path()),
            expected_foo_service_pairs(),
            "adding the contract dimension must not change service_call edges"
        );

        // Provenance is the fixed stamp that keeps the pass idempotent.
        let raw = std::fs::read_to_string(root.path().join(".vfs/graph/edges.jsonl")).unwrap();
        let stamped = raw
            .lines()
            .filter(|l| l.contains("\"service_contract\"") && l.contains("\"grpc_proto\""))
            .count();
        assert_eq!(stamped, 2, "every contract edge carries grpc_proto: {raw}");
    }

    #[test]
    fn commented_out_service_declaration_emits_no_contract_edge() {
        let root = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        write_grpc_fixture(root.path());
        // `BarService` HAS callers in the fixture (client1/client2 call
        // `NewBarServiceClient`), so a commented-out declaration that was
        // counted would show up as an extra pair.
        write_proto(
            root.path(),
            "protos/demo.proto",
            "syntax = \"proto3\";\n\n\
             // service BarService {\n\
             //   rpc GetBar(BarRequest) returns (BarResponse);\n\
             // }\n\n\
             service FooService {\n  rpc GetFoo(FooRequest) returns (FooResponse);\n}\n",
        );
        warm_fixture(root.path(), home.path());

        assert_eq!(
            service_contract_pairs(root.path()),
            expected_foo_contract_pairs("protos/demo.proto"),
            "a commented-out declaration is not a declaration"
        );
    }

    #[test]
    fn declared_service_with_no_caller_emits_no_contract_edge() {
        let root = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        write_grpc_fixture(root.path());
        // `BazService` is declared and never called: no fabricated dependent.
        write_proto(
            root.path(),
            "protos/demo.proto",
            "syntax = \"proto3\";\n\n\
             service FooService {\n  rpc GetFoo(FooRequest) returns (FooResponse);\n}\n\n\
             service BazService {\n  rpc GetBaz(BazRequest) returns (BazResponse);\n}\n",
        );
        warm_fixture(root.path(), home.path());

        let pairs = service_contract_pairs(root.path());
        assert_eq!(pairs, expected_foo_contract_pairs("protos/demo.proto"));
        assert!(
            !pairs.iter().any(|(_, to)| to.is_empty()),
            "no edge may be emitted for the caller-less service: {pairs:?}"
        );
    }

    #[test]
    fn duplicate_proto_declarations_each_get_an_edge() {
        let root = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        write_grpc_fixture(root.path());
        let decl = "syntax = \"proto3\";\n\nservice FooService {\n  rpc GetFoo(FooRequest) returns (FooResponse);\n}\n";
        // Checked-in copies of the same contract, as microservices-demo ships
        // them (./protos/demo.proto plus per-service src/<svc>/proto copies).
        write_proto(root.path(), "protos/demo.proto", decl);
        write_proto(root.path(), "src/cartservice/src/protos/Foo.proto", decl);
        warm_fixture(root.path(), home.path());

        let mut expected = expected_foo_contract_pairs("protos/demo.proto");
        expected.extend(expected_foo_contract_pairs(
            "src/cartservice/src/protos/Foo.proto",
        ));
        expected.sort();
        assert_eq!(
            service_contract_pairs(root.path()),
            expected,
            "every declaring file gets an edge — no canonical-copy preference"
        );
    }

    #[test]
    fn proto_files_in_pruned_directories_are_ignored() {
        let root = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        write_grpc_fixture(root.path());
        let decl = "syntax = \"proto3\";\n\nservice FooService {\n  rpc GetFoo(FooRequest) returns (FooResponse);\n}\n";
        write_proto(root.path(), "node_modules/vendor-lib/dep.proto", decl);
        write_proto(root.path(), "vendor/legacy/old.proto", decl);
        warm_fixture(root.path(), home.path());

        assert!(
            fs::read_to_string(root.path().join(".vfs/graph/edges.jsonl"))
                .unwrap()
                .lines()
                .all(|l| !l.contains("\"service_contract\"")),
            "a proto under a pruned directory must never become an edge endpoint"
        );
        assert!(service_contract_pairs(root.path()).is_empty());
    }

    /// PERF-002 trap, contract dimension: a corpus with a FULL parse cache whose
    /// edges.jsonl predates this pass must not take the fast path, and a re-warm
    /// that already carries the lines must stay idempotent.
    #[test]
    fn warm_adds_service_contract_edges_when_parse_cache_is_full() {
        let root = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        write_grpc_fixture(root.path());
        write_proto(
            root.path(),
            "protos/demo.proto",
            "syntax = \"proto3\";\n\nservice FooService {\n  rpc GetFoo(FooRequest) returns (FooResponse);\n}\n",
        );
        warm_fixture(root.path(), home.path());
        let expected = expected_foo_contract_pairs("protos/demo.proto");
        assert_eq!(service_contract_pairs(root.path()), expected);

        // Simulate the pre-pass corpus: strip the contract lines, keep the
        // parse cache (still complete — no file changed).
        let edges = root.path().join(".vfs").join("graph").join("edges.jsonl");
        let kept: String = std::fs::read_to_string(&edges)
            .unwrap()
            .lines()
            .filter(|l| !l.contains("\"service_contract\""))
            .map(|l| format!("{l}\n"))
            .collect();
        std::fs::write(&edges, kept).unwrap();
        assert!(
            service_contract_pairs(root.path()).is_empty(),
            "precondition: the stripped corpus carries no service_contract edge"
        );

        warm_fixture(root.path(), home.path());
        assert_eq!(
            service_contract_pairs(root.path()),
            expected,
            "a full parse cache must not strand the service_contract dimension"
        );

        warm_fixture(root.path(), home.path());
        assert_eq!(
            service_contract_pairs(root.path()),
            expected,
            "re-warm must be idempotent"
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

    // ======================================================================
    // GAP-081 phase 2b: non-Go client anchors (Python, Java, C#/TS/JS)
    // ======================================================================

    /// A fixture body from lines — avoids string-continuation escaping and
    /// keeps each fixture readable in the test that owns it.
    fn fixture(lines: &[&str]) -> String {
        let mut body = lines.join("\n");
        body.push('\n');
        body
    }

    /// Write a fixture file (creating parents). `write_proto` stays for the
    /// proto fixtures; these are `.py`/`.java`/`.cs`/`.ts`/`.js`.
    fn write_source(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
    }

    /// One proto declaring every named service, so a phase 2b caller fixture
    /// that resolves to the right service name yields exactly one contract
    /// edge (to `protos/demo.proto`) and a caller that resolves to a wrong
    /// name yields none.
    fn write_phase2b_proto(root: &Path, services: &[&str]) {
        let mut body = String::from("syntax = \"proto3\";\n\npackage demo;\n\n");
        for service in services {
            body.push_str(&format!(
                "service {service} {{\n  rpc Call{service}(DemoRequest) returns (DemoResponse);\n}}\n\n"
            ));
        }
        write_proto(root, "protos/demo.proto", &body);
    }

    /// The contract edge every phase 2b caller fixture produces: the file the
    /// call site lives in -> the proto that declares the service it calls.
    fn phase2b_pair(from: &str) -> (String, String) {
        (from.to_string(), "protos/demo.proto".to_string())
    }

    /// (a) Python: `<Name>ServiceStub(` is a caller of `<Name>Service`.
    /// Falsification: reuse the Go shape for this form (capture a stem, then
    /// `format!("{stem}Service")`) and the pair below becomes
    /// `EmailServiceService` -> no declared service -> the assertion fails.
    #[test]
    fn python_service_stub_anchor_records_caller() {
        let root = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        write_source(
            root.path(),
            "src/pyservice/client.py",
            &fixture(&[
                "import demo_pb2_grpc",
                "",
                "",
                "def make_stub(channel):",
                "    stub = demo_pb2_grpc.EmailServiceStub(channel)",
                "    return stub",
            ]),
        );
        write_phase2b_proto(root.path(), &["EmailService"]);
        warm_fixture(root.path(), home.path());

        assert_eq!(
            service_contract_pairs(root.path()),
            vec![phase2b_pair("src/pyservice/client.py")],
            "a Python `<Name>ServiceStub(` call site is a dependent of the contract"
        );
    }

    /// (b) Java/Kotlin: `<Name>ServiceGrpc.newBlockingStub(|newStub(|newFutureStub(`
    /// all resolve to `<Name>Service` — and two java lines that are NOT call
    /// sites must not: the generated-stub FIELD declaration and the
    /// constructor of a hand-written wrapper class (`new AdServiceClient(`,
    /// which is only an anchor form in the C#/TS/JS languages).
    /// Falsification: drop the `(?:Blocking|Future)?` alternation and
    /// `src/javaservice/AdStubClient.java` (newBlockingStub) disappears from
    /// the assertion; register `NewServiceClient` for `.java` and
    /// `src/javaservice/WrapperClient.java` appears in it.
    #[test]
    fn java_grpc_stub_anchor_records_caller() {
        let root = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        write_source(
            root.path(),
            "src/javaservice/AdStubClient.java",
            &fixture(&[
                "package hipstershop;",
                "",
                "public class AdStubClient {",
                "  private final hipstershop.AdServiceGrpc.AdServiceBlockingStub blockingStub;",
                "",
                "  AdStubClient(ManagedChannel channel) {",
                "    blockingStub = hipstershop.AdServiceGrpc.newBlockingStub(channel);",
                "  }",
                "}",
            ]),
        );
        write_source(
            root.path(),
            "src/javaservice/CartStubClient.java",
            &fixture(&[
                "package hipstershop;",
                "",
                "public class CartStubClient {",
                "  CartStubClient(ManagedChannel channel) {",
                "    hipstershop.CartServiceGrpc.newStub(channel);",
                "  }",
                "}",
            ]),
        );
        write_source(
            root.path(),
            "src/javaservice/ShippingStubClient.java",
            &fixture(&[
                "package hipstershop;",
                "",
                "public class ShippingStubClient {",
                "  ShippingStubClient(ManagedChannel channel) {",
                "    hipstershop.ShippingServiceGrpc.newFutureStub(channel);",
                "  }",
                "}",
            ]),
        );
        // Field declaration only — no factory/constructor call anywhere.
        write_source(
            root.path(),
            "src/javaservice/FieldOnly.java",
            &fixture(&[
                "package hipstershop;",
                "",
                "public class FieldOnly {",
                "  private final hipstershop.AdServiceGrpc.AdServiceBlockingStub blockingStub;",
                "}",
            ]),
        );
        // A wrapper class's own constructor: matches the C#/TS/JS form, not
        // the Java one.
        write_source(
            root.path(),
            "src/javaservice/WrapperClient.java",
            &fixture(&[
                "package hipstershop;",
                "",
                "public class WrapperClient {",
                "  static WrapperClient of(String host, int port) {",
                "    AdServiceClient client = new AdServiceClient(host, port);",
                "    return client;",
                "  }",
                "}",
            ]),
        );
        write_phase2b_proto(
            root.path(),
            &["AdService", "CartService", "ShippingService"],
        );
        warm_fixture(root.path(), home.path());

        let mut expected = vec![
            phase2b_pair("src/javaservice/AdStubClient.java"),
            phase2b_pair("src/javaservice/CartStubClient.java"),
            phase2b_pair("src/javaservice/ShippingStubClient.java"),
        ];
        expected.sort();
        assert_eq!(
            service_contract_pairs(root.path()),
            expected,
            "the three java stub factories are callers; the field declaration \
             and the wrapper constructor are not"
        );
    }

    /// (c) C#/TS/JS: `new <Name>ServiceClient(` is a caller of `<Name>Service`.
    /// Falsification: drop the `new_client` form from `stub_anchor_forms` (or
    /// from `NonGoAnchorRes`) and every expected pair below vanishes.
    #[test]
    fn csharp_or_ts_new_client_anchor_records_caller() {
        let root = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        write_source(
            root.path(),
            "src/cartservice/tests/CartServiceTests.cs",
            &fixture(&[
                "namespace CartService.Tests;",
                "",
                "public class CartServiceTests {",
                "    public void AddItem(Channel channel) {",
                "        var cartClient = new CartServiceClient(channel);",
                "    }",
                "}",
            ]),
        );
        write_source(
            root.path(),
            "src/tsservice/client.ts",
            &fixture(&[
                "export function makeClient(addr: string) {",
                "  const emailClient = new EmailServiceClient(addr);",
                "  return emailClient;",
                "}",
            ]),
        );
        write_source(
            root.path(),
            "src/jsservice/client.js",
            &fixture(&[
                "function makeClient(addr) {",
                "  const adClient = new AdServiceClient(addr);",
                "  return adClient;",
                "}",
            ]),
        );
        write_phase2b_proto(root.path(), &["CartService", "EmailService", "AdService"]);

        // The mapping itself, asserted directly (a mutation that drops the
        // form shows up here as a precise missing-pair diff rather than as a
        // fixture that produced no edge at all).
        let sources: Vec<PathBuf> = [
            "src/cartservice/tests/CartServiceTests.cs",
            "src/tsservice/client.ts",
            "src/jsservice/client.js",
        ]
        .iter()
        .map(|rel| root.path().join(rel))
        .collect();
        let sites = collect_service_call_sites(&sources, root.path());
        let mut found: Vec<(String, String)> = sites
            .callers
            .iter()
            .flat_map(|(service, files)| {
                files
                    .iter()
                    .map(move |file| (service.clone(), file.clone()))
            })
            .collect();
        found.sort();
        assert_eq!(
            found,
            vec![
                (
                    "AdService".to_string(),
                    "src/jsservice/client.js".to_string()
                ),
                (
                    "CartService".to_string(),
                    "src/cartservice/tests/CartServiceTests.cs".to_string()
                ),
                (
                    "EmailService".to_string(),
                    "src/tsservice/client.ts".to_string()
                ),
            ],
            "each C#/TS/JS form maps to the service name spelled in the call"
        );

        warm_fixture(root.path(), home.path());

        let mut expected = vec![
            phase2b_pair("src/cartservice/tests/CartServiceTests.cs"),
            phase2b_pair("src/tsservice/client.ts"),
            phase2b_pair("src/jsservice/client.js"),
        ];
        expected.sort();
        assert_eq!(
            service_contract_pairs(root.path()),
            expected,
            "a C#/TS/JS `new <Name>ServiceClient(` call site is a dependent of \
             the contract"
        );
    }

    /// (c-2c) GAP-081 phase 2c: the ESM/CJS module flavours of the same
    /// JavaScript language reach the anchor pass — a `.mjs`/`.cjs` gRPC-web
    /// caller is a dependent of the contract exactly like its `.js` twin —
    /// while a `.mjs` file with no form match stays out.
    ///
    /// Two independent halves, both load-bearing:
    /// - DISCOVERY: before phase 2c `Language::from_extension` returned `None`
    ///   for `mjs`/`cjs`, so `collect_source_files` never put those paths in
    ///   `source_files` and this scan never saw the file at all. Falsification:
    ///   drop `"mjs" | "cjs"` from `from_extension` and the two expected pairs
    ///   below vanish from `service_contract_pairs` even though
    ///   `stub_anchor_forms` still names the extensions.
    /// - ANCHOR FORM: `stub_anchor_forms` must map those extensions to
    ///   [`StubAnchorForm::NewServiceClient`], because the discovery half alone
    ///   would only give the file an AST (import edges), never a service edge.
    ///   Falsification: drop `"mjs" | "cjs"` from that arm and the same two
    ///   pairs vanish from the anchors while the files stay discovered.
    #[test]
    fn mjs_and_cjs_new_client_anchor_records_caller() {
        let root = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        write_source(
            root.path(),
            "src/esm/client.mjs",
            &fixture(&[
                "import { CartServiceClient } from './gen/cart_grpc_web_pb.js';",
                "",
                "export function makeClient(addr) {",
                "  return new CartServiceClient(addr);",
                "}",
            ]),
        );
        write_source(
            root.path(),
            "src/cjs/client.cjs",
            &fixture(&[
                "const { AdServiceClient } = require('./gen/ad_grpc_web_pb.js');",
                "",
                "module.exports.makeClient = function (addr) {",
                "  return new AdServiceClient(addr);",
                "};",
            ]),
        );
        // No `new <Name>ServiceClient(` anywhere: a plain module in the same
        // corpus must contribute no anchor (the assertion below cannot pass
        // merely because every `.mjs` file became a caller).
        write_source(
            root.path(),
            "src/esm/plain.mjs",
            &fixture(&[
                "import { readFile } from 'node:fs/promises';",
                "",
                "export const load = (path) => readFile(path, 'utf8');",
            ]),
        );
        write_phase2b_proto(root.path(), &["CartService", "AdService"]);

        // The mapping itself, asserted directly: a mutation that drops the
        // form shows up as a precise missing-pair diff rather than as a
        // fixture that produced no edge at all.
        let sources: Vec<PathBuf> = [
            "src/esm/client.mjs",
            "src/cjs/client.cjs",
            "src/esm/plain.mjs",
        ]
        .iter()
        .map(|rel| root.path().join(rel))
        .collect();
        let sites = collect_service_call_sites(&sources, root.path());
        let mut found: Vec<(String, String)> = sites
            .callers
            .iter()
            .flat_map(|(service, files)| files.iter().map(move |f| (service.clone(), f.clone())))
            .collect();
        found.sort();
        assert_eq!(
            found,
            vec![
                ("AdService".to_string(), "src/cjs/client.cjs".to_string()),
                ("CartService".to_string(), "src/esm/client.mjs".to_string()),
            ],
            "each `.mjs`/`.cjs` form maps to the service name spelled in the \
             call; the anchor-less module contributes nothing"
        );

        warm_fixture(root.path(), home.path());

        let mut expected = vec![
            phase2b_pair("src/cjs/client.cjs"),
            phase2b_pair("src/esm/client.mjs"),
        ];
        expected.sort();
        assert_eq!(
            service_contract_pairs(root.path()),
            expected,
            "a `.mjs`/`.cjs` `new <Name>ServiceClient(` call site is a dependent \
             of the contract — the file must reach the warm at all, and the \
             form must be registered for its extension"
        );
    }

    /// (f) GAP-081 phase 2c: the C# codegen banner. protoc's C# output carries
    /// `<auto-generated>` and does not necessarily carry `DO NOT EDIT`, and no
    /// name row covers `*.cs` — so before phase 2c a generated `*Grpc.cs` under
    /// a name the table does not know was counted as a caller of every service
    /// it declares.
    ///
    /// The decoy below carries NO other marker (no `DO NOT EDIT`, no
    /// `@generated`, no known name suffix), so the exclusion is attributable to
    /// `<auto-generated>` alone — falsification: delete that one row from
    /// [`STUB_GENERATED_MARKERS`] and the decoy becomes a caller. The live
    /// hand-written `.cs` caller in the same fixture pins the other half: a
    /// `.cs` file with no banner is still counted (the guard is banner-driven,
    /// not extension-driven).
    ///
    /// The decoy deliberately does NOT live under a `vendor/`-named directory:
    /// that name is in `DEFAULT_PRUNE_DIRS`, so a decoy placed there would be
    /// pruned before the scan and the scan assertion would pass vacuously. The
    /// discovery premise is asserted explicitly below, from the same walk the
    /// warm uses.
    #[test]
    fn csharp_auto_generated_banner_excludes_vendored_grpc_cs() {
        const DECOY: &str = "src/generated/hipstershop/CartServiceGrpc.cs";
        let root = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        write_source(
            root.path(),
            DECOY,
            &fixture(&[
                "// <auto-generated>",
                "namespace Hipstershop {",
                "  public static class CartServiceClientFactory {",
                "    public static CartServiceClient Make(Channel channel) {",
                "      return new CartServiceClient(channel);",
                "    }",
                "  }",
                "}",
            ]),
        );
        write_source(
            root.path(),
            "src/cartservice/RealCaller.cs",
            &fixture(&[
                "namespace CartService;",
                "",
                "public class RealCaller {",
                "    public void AddItem(Channel channel) {",
                "        var cartClient = new CartServiceClient(channel);",
                "    }",
                "}",
            ]),
        );
        write_phase2b_proto(root.path(), &["CartService"]);

        // Discovery premise, from the SAME walk `graph warm` uses: the decoy
        // path must actually reach `source_files`. Without this the scan
        // assertion below could pass because the file was pruned, not because
        // the banner excluded it.
        let mut discovered: Vec<PathBuf> = Vec::new();
        collect_source_files(
            root.path(),
            Path::new(""),
            None,
            &[],
            &mut discovered,
            &mut ExclusionReport::default(),
        )
        .unwrap();
        let discovered: Vec<String> = discovered
            .iter()
            .map(|p| {
                p.strip_prefix(root.path())
                    .unwrap_or(p)
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert!(
            discovered.iter().any(|p| p == DECOY),
            "premise: the decoy must survive discovery, or the exclusion below \
             is untested (discovered: {discovered:?})"
        );

        // Direct predicate check: the banner alone decides, with no other row
        // of either table able to answer for this file name.
        let decoy = std::fs::read_to_string(root.path().join(DECOY)).unwrap();
        assert!(
            is_generated_stub(Path::new(DECOY), &decoy),
            "`<auto-generated>` on its own must mark a generated `*Grpc.cs` as \
             generated — no name row covers `*.cs`"
        );
        assert!(
            STUB_GENERATED_NAME_SUFFIXES
                .iter()
                .all(|suffix| !"CartServiceGrpc.cs".ends_with(suffix)),
            "premise: the decoy's NAME is not in the generated-name table, so \
             the exclusion can only come from the banner"
        );

        let sources: Vec<PathBuf> = [DECOY, "src/cartservice/RealCaller.cs"]
            .iter()
            .map(|rel| root.path().join(rel))
            .collect();
        let sites = collect_service_call_sites(&sources, root.path());
        let mut found: Vec<(String, String)> = sites
            .callers
            .iter()
            .flat_map(|(service, files)| files.iter().map(move |f| (service.clone(), f.clone())))
            .collect();
        found.sort();
        assert_eq!(
            found,
            vec![(
                "CartService".to_string(),
                "src/cartservice/RealCaller.cs".to_string()
            )],
            "the generated `*Grpc.cs` is excluded; the hand-written caller \
             with no banner is still counted"
        );

        warm_fixture(root.path(), home.path());
        assert_eq!(
            service_contract_pairs(root.path()),
            vec![phase2b_pair("src/cartservice/RealCaller.cs")],
            "only the hand-written C# caller is a dependent of the contract"
        );

        let raw = std::fs::read_to_string(root.path().join(".vfs/graph/edges.jsonl")).unwrap();
        let decoys: Vec<&str> = raw
            .lines()
            .filter(|l| l.contains("CartServiceGrpc.cs"))
            .filter(|l| l.contains("\"service_contract\"") || l.contains("\"service_call\""))
            .collect();
        assert!(
            decoys.is_empty(),
            "the generated C# stub must never be a service endpoint: {decoys:?}"
        );
    }

    /// (d) The generated-stub trap: a `*_pb2_grpc.py` declares
    /// `<X>ServiceStub` for EVERY service in the project, so counting it
    /// would make it a dependent of all of them. All three real shapes are
    /// excluded — banner + generated name, stripped banner with a generated
    /// name, and a banner under a name no table knows — while a live caller
    /// in the same fixture still produces its edge (the assertion cannot pass
    /// vacuously).
    /// Falsification: delete the `is_generated_stub` guard and the three decoy
    /// files each become callers of every declared service.
    #[test]
    fn generated_stub_file_declaring_every_stub_is_never_a_caller() {
        let root = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        write_source(
            root.path(),
            "src/emailservice/demo_pb2_grpc.py",
            &fixture(&[
                "# Copyright 2018 Google LLC",
                "#",
                "# Generated by the gRPC Python protocol compiler plugin. DO NOT EDIT!",
                "\"\"\"Client and server classes corresponding to protobuf-defined services.\"\"\"",
                "import grpc",
                "",
                "",
                "class EmailServiceStub(object):",
                "    def __init__(self, channel):",
                "        self.Send = channel.unary_unary('/demo.EmailService/Send')",
                "",
                "",
                "class CartServiceStub(object):",
                "    def __init__(self, channel):",
                "        self.Get = channel.unary_unary('/demo.CartService/Get')",
                "",
                "",
                "class AdServiceStub(object):",
                "    def __init__(self, channel):",
                "        self.Get = channel.unary_unary('/demo.AdService/Get')",
            ]),
        );
        // Vendored stub whose banner was stripped: the file NAME is the only
        // discriminator left.
        write_source(
            root.path(),
            "src/vendor/checked_in_pb2_grpc.py",
            &fixture(&[
                "class ProductCatalogServiceStub(object):",
                "    def __init__(self, channel):",
                "        self.List = channel.unary_unary('/demo.ProductCatalogService/List')",
            ]),
        );
        // Banner under a name no generated-name table knows: the BANNER is the
        // only discriminator left.
        write_source(
            root.path(),
            "src/legacy/handwritten_looking.py",
            &fixture(&[
                "# Generated by the gRPC Python protocol compiler plugin. DO NOT EDIT!",
                "def make(channel):",
                "    return demo_pb2_grpc.CurrencyServiceStub(channel)",
            ]),
        );
        // The live caller that proves the scan is still running.
        write_source(
            root.path(),
            "src/recommendationservice/client.py",
            &fixture(&[
                "import demo_pb2_grpc",
                "",
                "",
                "def make_stub(channel):",
                "    stub = demo_pb2_grpc.EmailServiceStub(channel)",
                "    return stub",
            ]),
        );
        write_phase2b_proto(
            root.path(),
            &[
                "EmailService",
                "CartService",
                "AdService",
                "ProductCatalogService",
                "CurrencyService",
            ],
        );
        warm_fixture(root.path(), home.path());

        assert_eq!(
            service_contract_pairs(root.path()),
            vec![phase2b_pair("src/recommendationservice/client.py")],
            "only the live caller is a dependent; a generated stub that declares \
             every stub in the project is a dependent of none"
        );

        let raw = std::fs::read_to_string(root.path().join(".vfs/graph/edges.jsonl")).unwrap();
        let decoys: Vec<&str> = raw
            .lines()
            .filter(|l| l.contains("demo_pb2_grpc") || l.contains("checked_in_pb2_grpc"))
            .filter(|l| l.contains("\"service_contract\"") || l.contains("\"service_call\""))
            .collect();
        assert!(
            decoys.is_empty(),
            "generated stubs must never be service endpoints: {decoys:?}"
        );
    }

    /// (e) An anchor for a service nothing declares emits nothing, and the
    /// reason is the missing declaration — not a missed anchor. Both halves
    /// are asserted: the scan really did record the caller, and the contract
    /// pass refused to fabricate a dependent for it.
    /// Falsification: make `discover_service_contracts` emit for a caller-less
    /// declaration lookup (e.g. fall back to a default proto path) and the
    /// empty expectation fails; break the anchor regex and the scan assertion
    /// fails.
    #[test]
    fn non_go_anchor_with_no_declared_service_emits_no_edge() {
        let root = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        write_source(
            root.path(),
            "src/pyservice/client.py",
            &fixture(&[
                "import demo_pb2_grpc",
                "",
                "",
                "def make_stub(channel):",
                "    stub = demo_pb2_grpc.MissingServiceStub(channel)",
                "    return stub",
            ]),
        );
        write_phase2b_proto(root.path(), &["EmailService"]);
        warm_fixture(root.path(), home.path());

        // Half one: the anchor was seen (the scan is not silently skipping the
        // file or the form).
        let files: Vec<PathBuf> = vec![root.path().join("src/pyservice/client.py")];
        let sites = collect_service_call_sites(&files, root.path());
        assert_eq!(
            sites
                .callers
                .get("MissingService")
                .map(|files| files.iter().cloned().collect::<Vec<_>>()),
            Some(vec!["src/pyservice/client.py".to_string()]),
            "the non-Go anchor must be recorded in the shared caller map"
        );

        // Half two: no declaration anywhere => no edge, not a fabricated one.
        assert!(
            service_contract_pairs(root.path()).is_empty(),
            "a service nothing declares must not receive a dependent"
        );
    }

    /// (f) The two families share ONE caller map: a non-Go caller of the
    /// service the Go fixture serves gains a phase 1 `service_call` edge to
    /// that Go provider, and the Go edge set itself is unchanged (it is
    /// asserted exactly, against the pinned Go expectation).
    /// Falsification: write the non-Go anchors into a second map instead of
    /// `sites.callers` and the extra `service_call` pair disappears; drop the
    /// `import_edges`-visible Go skip and the Go pairs break.
    #[test]
    fn non_go_caller_joins_the_go_provider_via_the_shared_caller_map() {
        let root = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        write_grpc_fixture(root.path());
        write_phase2b_proto(root.path(), &["FooService"]);
        write_source(
            root.path(),
            "src/pyservice/foo_client.py",
            &fixture(&[
                "import demo_pb2_grpc",
                "",
                "",
                "def make_stub(channel):",
                "    stub = demo_pb2_grpc.FooServiceStub(channel)",
                "    return stub",
            ]),
        );
        warm_fixture(root.path(), home.path());

        // Go semantics unchanged (the pinned Go pairs) plus exactly one new
        // caller->provider pair, contributed by the Python call site.
        let mut expected_calls = expected_foo_service_pairs();
        expected_calls.push((
            "src/pyservice/foo_client.py".to_string(),
            "src/foo/server.go".to_string(),
        ));
        expected_calls.sort();
        assert_eq!(
            service_call_pairs(root.path()),
            expected_calls,
            "a non-Go caller of a Go-served service resolves through phase 1"
        );

        let mut expected_contracts = expected_foo_contract_pairs("protos/demo.proto");
        expected_contracts.push(phase2b_pair("src/pyservice/foo_client.py"));
        expected_contracts.sort();
        assert_eq!(
            service_contract_pairs(root.path()),
            expected_contracts,
            "the same caller is a dependent of the declaring proto"
        );
    }

    /// (g) A commented-out call site is not a call site — `#`, `//` and the
    /// C-family `/*`, in the three languages the corpus anchors live in. The
    /// live caller in the same fixture keeps this from passing vacuously.
    /// Falsification: remove the `is_non_go_comment` skip and every commented
    /// file below becomes a dependent of a declared service.
    #[test]
    fn commented_out_non_go_anchor_emits_no_edge() {
        let root = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        let files = [
            (
                "src/pyservice/client.py",
                "# stub = demo_pb2_grpc.EmailServiceStub(channel)",
            ),
            (
                "src/javaservice/Client.java",
                "// hipstershop.AdServiceGrpc.newBlockingStub(channel);",
            ),
            (
                "src/javaservice/Block.java",
                "/* hipstershop.CartServiceGrpc.newStub(channel); */",
            ),
            (
                "src/tsservice/client.ts",
                "// const c = new ShippingServiceClient(addr);",
            ),
        ];
        for (rel, line) in files {
            write_source(root.path(), rel, &fixture(&[line]));
        }
        // Live caller: also the only thing that gives the fixture an edge at
        // all (a corpus with zero edges has no edges.jsonl to assert against).
        write_source(
            root.path(),
            "src/liveservice/live_client.py",
            &fixture(&[
                "import demo_pb2_grpc",
                "",
                "",
                "def make_stub(channel):",
                "    stub = demo_pb2_grpc.EmailServiceStub(channel)",
                "    return stub",
            ]),
        );
        write_phase2b_proto(
            root.path(),
            &[
                "EmailService",
                "AdService",
                "CartService",
                "ShippingService",
            ],
        );
        warm_fixture(root.path(), home.path());

        assert_eq!(
            service_contract_pairs(root.path()),
            vec![phase2b_pair("src/liveservice/live_client.py")],
            "commented-out anchors must not create dependents"
        );

        let sources: Vec<PathBuf> = files.map(|(rel, _)| root.path().join(rel)).to_vec();
        let sites = collect_service_call_sites(&sources, root.path());
        assert!(
            sites.callers.is_empty(),
            "no commented-out line may reach the caller map: {:?}",
            sites.callers
        );
    }

    /// The generated-stub predicate's two discriminators, including the rows
    /// the scan itself cannot exercise (`*.pb.go` never reaches it) and the
    /// head window (a banner past line 60 is not a banner).
    #[test]
    fn generated_stub_discriminators_cover_banner_and_file_name() {
        // Every banner row, on its own, in a file whose name no row knows.
        for marker in STUB_GENERATED_MARKERS {
            assert!(
                is_generated_stub(
                    Path::new("src/legacy/unknown_name.py"),
                    &fixture(&[marker, "x = 1"])
                ),
                "banner marker must be recognised on its own: {marker}"
            );
        }
        // Hardcoded corpus banner: this (not the loop above) pins the CONTENT
        // of the table — deleting a row would leave the loop passing.
        let banner = "# Generated by the protocol buffer compiler.  DO NOT EDIT!";
        assert!(is_generated_stub(
            Path::new("src/legacy/unknown_name.py"),
            &fixture(&[banner])
        ));
        // GAP-081 phase 2c, pinned as CONTENT for the same reason: the
        // canonical C#/VB.NET codegen banner, alone (no `DO NOT EDIT`, no
        // `@generated`), on a file name no row of either table knows.
        let csharp_banner = "// <auto-generated>";
        assert!(is_generated_stub(
            Path::new("src/legacy/unknown_name.cs"),
            &fixture(&[csharp_banner, "public class Foo {}"])
        ));
        // ... and it is the bracketed banner, not the bare phrase, that the
        // table keys on: prose mentioning the words is not a generated file.
        assert!(!is_generated_stub(
            Path::new("src/live/notes.py"),
            &fixture(&["# this file is not auto-generated by protoc"])
        ));
        // Every generated-name row, with no banner at all.
        for suffix in STUB_GENERATED_NAME_SUFFIXES {
            let name = format!("src/vendor/generated{suffix}");
            assert!(
                is_generated_stub(
                    Path::new(&name),
                    &fixture(&["class FooServiceStub(object):"])
                ),
                "generated file name must be recognised on its own: {suffix}"
            );
        }
        assert!(is_generated_stub(
            Path::new("src/vendor/vendored_pb2_grpc.py"),
            &fixture(&["class FooServiceStub(object):"])
        ));
        assert!(!is_generated_stub(
            Path::new("src/live/client.py"),
            &fixture(&[
                "import demo_pb2_grpc",
                "stub = demo_pb2_grpc.EmailServiceStub(channel)"
            ])
        ));

        let mut long = fixture(&["# filler"; 60]);
        long.push_str(&fixture(&[banner]));
        assert!(
            !is_generated_stub(Path::new("src/live/long_header.py"), &long),
            "the banner window is the file HEAD, not the whole file"
        );
    }
}
