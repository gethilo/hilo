//! `hilo graph warm`, `hilo graph stats`, `hilo graph related`, and `hilo graph impact`.
//!
//! Queries (`related`, `impact`, `stats`) are JIT — they auto-parse files on
//! first access. `warm` is an optional batch pre-parse for CI / power users.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result};
use hilo_graph::edges;
use hilo_graph::{GraphDB, ImpactResult, Language, Parser};
use hilo_metadata::inventory::{self, Edge};
use rayon::prelude::*;

/// Directory names to skip when walking for source files.
const SKIP_DIRS: &[&str] = &[
    "target",       // Rust build output
    "node_modules", // JavaScript/TypeScript
    "vendor",       // Go / PHP
    "__pycache__",  // Python cache
    ".venv",        // Python virtualenv
];

/// Walk the current directory for source files in all supported languages,
/// parse their imports, and write the resulting edges to both
/// Pre-compute the full graph — parse ALL source files (optional warmup).
///
/// This is the same batch-parse that was previously called
/// `hilo graph discover`.  It remains useful for CI pipelines or users who
/// want every file cached before running queries.  Day-to-day, queries are
/// JIT (lazy) and do **not** require `warm` first.
///
/// When `language` is `Some`, only files of that language are parsed
/// (e.g. `--language rust`).  Otherwise all supported languages are scanned.
pub fn run_warm(workspace: bool, language: Option<String>, changed: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to determine the current directory")?;

    // Collect every source file under the current directory.
    let mut source_files = Vec::new();
    collect_source_files(&cwd, &mut source_files)
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
            other => anyhow::bail!("unknown language: {other}"),
        };
        source_files.retain(|f| f.extension().and_then(|e| e.to_str()) == Some(ext));
    }

    if source_files.is_empty() {
        println!(
            "No supported source files found. Supported extensions: {}",
            Language::all_extensions().join(", ")
        );
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

    // Parallel parse: create a fresh parser per file since tree_sitter::Parser
    // is not Send.  Each closure runs on a rayon thread, reads the file, and
    // returns the parsed edges (or an empty vec on skip/error).
    let parse_results: Vec<Result<Vec<Edge>>> = source_files
        .par_iter()
        .map(|file| {
            let ext = file.extension().and_then(|e| e.to_str());
            let lang = match ext.and_then(Language::from_extension) {
                Some(l) => l,
                None => return Ok(Vec::new()),
            };

            let source = match std::fs::read_to_string(file) {
                Ok(s) => s,
                Err(_) => return Ok(Vec::new()),
            };

            let rel = file
                .strip_prefix(&cwd)
                .unwrap_or(file)
                .to_string_lossy()
                .into_owned();

            let mut parser = Parser::for_language(lang)
                .with_context(|| format!("failed to initialize {:?} parser", lang))?;

            let count = progress.fetch_add(1, Ordering::Relaxed) + 1;
            if count.is_multiple_of(100) || count == total_files {
                eprintln!("  parsing {count}/{total_files} files...");
            }

            parser
                .parse_imports(&rel, &source)
                .with_context(|| format!("failed to parse {rel}"))
        })
        .collect();

    // Flatten results, propagating the first error.
    let mut all_edges: Vec<Edge> = Vec::new();
    let mut unique_sources: HashSet<String> = HashSet::new();
    for result in parse_results {
        let edges = result?;
        for e in &edges {
            unique_sources.insert(e.from.clone());
        }
        all_edges.extend(edges);
    }

    // Infer `tested_by` and `tests` edges from filename conventions.
    let test_edges = discover_test_associations(&source_files, &cwd);
    all_edges.extend(test_edges);

    // Process graph extensions from manifest — manually declared edge patterns
    // like docs/**/*.md → src/**/*.go with relation "documented_by".
    if let Ok(manifest) = load_manifest() {
        let extension_edges =
            generate_extension_edges(&manifest.graph.extensions, &source_files, &cwd);
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

    // Persist edges to the JSONL inventory file.
    let edges_jsonl = cwd.join(".vfs").join("graph").join("edges.jsonl");
    inventory::append_edges_deduped(&edges_jsonl, &all_edges)
        .context("failed to write edges.jsonl")?;

    // Populate the DuckDB graph database.
    let graph_db = cwd.join(".vfs").join("graph").join("graph.db");
    let graph_db_str = graph_db.to_str().unwrap_or(".vfs/graph/graph.db");
    let graph = GraphDB::open(graph_db_str).context("failed to open DuckDB graph database")?;
    graph
        .insert_edges(&all_edges)
        .context("failed to insert edges into DuckDB")?;

    let n = all_edges.len();
    let m = unique_sources.len();
    let langs = langs_seen.len();
    println!("Discovered {n} edges across {m} files ({langs} languages)");

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
        println!(
            "{}  →  {}  ({})  [{} conf={:.2}]",
            edge.from, edge.to, edge.rel, edge.provenance, edge.confidence
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

/// Print summary statistics from the dependency graph.
///
/// An empty cache is a valid state (not an error) — the graph starts
/// empty after `hilo init` and is populated lazily as files are queried
/// or eagerly via `hilo graph warm`.
pub fn run_stats() -> Result<()> {
    let cwd = std::env::current_dir().context("failed to determine the current directory")?;
    let graph_db = cwd.join(".vfs").join("graph").join("graph.db");

    if !graph_db.exists() {
        println!("Graph cache is empty. Query a file or run `hilo graph warm` to populate.");
        return Ok(());
    }

    let graph_db_str = graph_db.to_str().unwrap_or(".vfs/graph/graph.db");
    let graph = GraphDB::open(graph_db_str).context("failed to open DuckDB graph database")?;
    let stats = graph
        .stats()
        .context("failed to compute graph statistics")?;

    if stats.total_edges == 0 {
        println!("Graph cache is empty. No edges parsed yet.");
        return Ok(());
    }

    println!("Total edges: {}", stats.total_edges);
    println!("Total files: {}", stats.total_files);
    if let Some(ref mc) = stats.most_connected {
        println!("Most connected: {mc}");
    }
    println!("Edge types:");
    for (rel, count) in &stats.edge_types {
        println!("  {rel}: {count}");
    }
    if !stats.orphans.is_empty() {
        println!("Orphans (no incoming edges):");
        for orphan in &stats.orphans {
            println!("  {orphan}");
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
    } else if let Some(stem) = name.strip_prefix("test_") {
        if stem.ends_with(".py")
            || stem.ends_with(".c")
            || stem.ends_with(".clj")
            || stem.ends_with(".cljs")
            || stem.ends_with(".r")
            || stem.ends_with(".jl")
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
    }
    patterns
}

fn collect_source_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if name_str.starts_with('.') || SKIP_DIRS.contains(&name_str.as_ref()) {
            continue;
        }

        let ft = entry.file_type()?;
        if ft.is_dir() {
            collect_source_files(&path, out)?;
        } else if ft.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if Language::from_extension(ext).is_some() {
                    out.push(path);
                }
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

/// `hilo graph rule-check <name>` — execute a named rule against the graph.
pub fn run_rule_check(name: &str) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to determine the current directory")?;
    let graph_db = cwd.join(".vfs").join("graph").join("graph.db");

    if !graph_db.exists() {
        anyhow::bail!("No graph data. Run `hilo graph discover` first.");
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

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
}
