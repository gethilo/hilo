//! `warpfs graph discover` and `warpfs graph stats`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use warpfs_graph::{GraphDB, Language, Parser};
use warpfs_metadata::inventory::{self, Edge};

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
/// `.vfs/graph/edges.jsonl` and `.vfs/graph/graph.db`.
pub fn run_discover() -> Result<()> {
    let cwd = std::env::current_dir().context("failed to determine the current directory")?;

    // Collect every source file under the current directory.
    let mut source_files = Vec::new();
    collect_source_files(&cwd, &mut source_files)
        .context("failed to walk directory tree for source files")?;

    if source_files.is_empty() {
        println!("No supported source files found. Supported extensions: {}",
            Language::all_extensions().join(", "));
        return Ok(());
    }

    // Group files by language so we can reuse one parser per language.
    let mut by_lang: std::collections::BTreeMap<Language, Vec<PathBuf>> =
        std::collections::BTreeMap::new();
    for file in &source_files {
        if let Some(ext) = file.extension().and_then(|e| e.to_str()) {
            if let Some(lang) = Language::from_extension(ext) {
                by_lang.entry(lang).or_default().push(file.clone());
            }
        }
    }

    let mut all_edges: Vec<Edge> = Vec::new();
    let mut unique_sources: HashSet<String> = HashSet::new();

    for (language, files) in &by_lang {
        if files.is_empty() { continue; }
        let lang_name = format!("{:?}", language);
        let mut parser = Parser::for_language(*language)
            .with_context(|| format!("failed to initialize tree-sitter {lang_name} parser"))?;

        for file in files {
            let source = match std::fs::read_to_string(file) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let rel = file
                .strip_prefix(&cwd)
                .unwrap_or(file)
                .to_string_lossy()
                .into_owned();

            let edges = parser
                .parse_imports(&rel, &source)
                .with_context(|| format!("failed to parse {rel}"))?;

            for e in &edges {
                unique_sources.insert(e.from.clone());
            }
            all_edges.extend(edges);
        }
    }

    // Persist edges to the JSONL inventory file.
    let edges_jsonl = cwd.join(".vfs").join("graph").join("edges.jsonl");
    inventory::append_edges(&edges_jsonl, &all_edges)
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
    let langs = by_lang.len();
    println!("Discovered {n} edges across {m} files ({langs} languages)");
    Ok(())
}

/// Print summary statistics from the DuckDB graph database.
pub fn run_stats() -> Result<()> {
    let cwd = std::env::current_dir().context("failed to determine the current directory")?;
    let graph_db = cwd.join(".vfs").join("graph").join("graph.db");

    if !graph_db.exists() {
        println!("No graph data. Run `warpfs graph discover` first.");
        return Ok(());
    }

    let graph_db_str = graph_db.to_str().unwrap_or(".vfs/graph/graph.db");
    let graph = GraphDB::open(graph_db_str).context("failed to open DuckDB graph database")?;
    let stats = graph.stats().context("failed to compute graph statistics")?;

    if stats.total_edges == 0 {
        println!("No graph data. Run `warpfs graph discover` first.");
        return Ok(());
    }

    println!("Total edges: {}", stats.total_edges);
    println!("Unique source files: {}", stats.unique_files);
    println!("Unique dependencies: {}", stats.unique_dependencies);
    println!("Top dependencies:");
    for (dep, count) in &stats.top_dependencies {
        println!("  {dep}: {count}");
    }

    Ok(())
}

/// Recursively collect source files for all supported languages.
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
