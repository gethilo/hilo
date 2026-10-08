//! GAP-112: `hilo graph wiring` — silent-fallback conformance detector.
//!
//! Re-uses warm's discovery + parse cache machinery over the same tree, runs
//! [`hilo_graph::detect_wiring`], and reports the three-way verdict per
//! consumed interface: pass / finding / unsupported. Unsupported NEVER
//! renders as pass.

use anyhow::{Context, Result};
use hilo_graph::parser::Language;
use hilo_graph::{detect_wiring, WiringState};
use rayon::prelude::*;

/// The languages with conformance extraction — everything else in a scanned
/// tree contributes an explicit `unsupported` line, never a silent skip.
const SUPPORTED: [Language; 5] = [
    Language::Go,
    Language::Rust,
    Language::Python,
    Language::TypeScript,
    Language::JavaScript,
];

/// `hilo graph wiring [--json] [path]`
pub fn run_wiring(json: bool, path: Option<String>) -> Result<()> {
    let root = match path {
        Some(p) => std::fs::canonicalize(&p).with_context(|| format!("bad path: {p}"))?,
        None => std::env::current_dir().context("failed to determine the current directory")?,
    };

    // Walk the tree for source files (mirrors collect_source_files' pruning
    // by reusing it through warm's walk helper is overkill here — wiring is
    // JIT and self-contained; honor .vfs pruning via hilo-graph's discovery
    // helper used by warm, imported below).
    let mut source_files = Vec::new();
    let mut exclusions = crate::commands::graph::ExclusionReport::default();
    collect(&root, &mut source_files, &mut exclusions)?;

    // Parse each file once (JIT, no cache — wiring is an analysis pass).
    let corpora: Vec<(Language, String, String)> = source_files
        .par_iter()
        .filter_map(|file| {
            let lang = Language::from_path(file)?;
            let rel = file
                .strip_prefix(&root)
                .unwrap_or(file)
                .to_string_lossy()
                .replace('\\', "/");
            let src = std::fs::read_to_string(file).ok()?;
            Some((lang, rel, src))
        })
        .collect();

    // Per-language presence: which languages appear in the tree.
    let seen: std::collections::BTreeSet<Language> = corpora.iter().map(|(l, _, _)| *l).collect();

    // Filter to conformance-capable languages for the detector; every other
    // seen language prints an explicit unsupported line.
    let capable: Vec<(Language, String, String)> = corpora
        .iter()
        .filter(|(l, _, _)| SUPPORTED.contains(l))
        .cloned()
        .collect();

    let results = detect_wiring(&capable);

    let findings: Vec<_> = results
        .iter()
        .filter(|r| r.state == WiringState::Finding)
        .collect();

    if json {
        let doc = serde_json::json!({
            "schema": "hilo.graph.wiring/1",
            "root": root.to_string_lossy(),
            "scanned_files": corpora.len(),
            "languages_unsupported": seen
                .iter()
                .filter(|l| !SUPPORTED.contains(l))
                .map(|l| format!("{l:?}").to_lowercase())
                .collect::<Vec<_>>(),
            "results": results
                .iter()
                .map(|r| serde_json::json!({
                    "interface": r.interface,
                    "state": match r.state {
                        WiringState::Pass => "pass",
                        WiringState::Finding => "finding",
                        WiringState::Unsupported => "unsupported",
                    },
                    "consumers": r.consumers,
                    "satisfiers": r.satisfiers.iter().map(|s| serde_json::json!({
                        "type": s.type_name,
                        "file": s.file,
                        "role": s.role.as_str(),
                    })).collect::<Vec<_>>(),
                }))
                .collect::<Vec<_>>(),
            "finding_count": findings.len(),
        });
        println!("{}", serde_json::to_string_pretty(&doc)?);
    } else {
        for lang in &seen {
            if !SUPPORTED.contains(lang) {
                println!("conformance: unsupported ({:?})", lang);
            }
        }
        if results.is_empty() && seen.iter().all(|l| SUPPORTED.contains(l)) {
            println!("No interface consumption sites found — nothing to check.");
        }
        for r in &results {
            match r.state {
                WiringState::Pass => {
                    println!("pass      {} — non-test satisfier exists", r.interface);
                }
                WiringState::Finding => {
                    println!(
                        "FINDING   {} — consumed from non-test code but satisfied only by:",
                        r.interface
                    );
                    for c in &r.consumers {
                        println!("          consuming site: {c}");
                    }
                    for s in &r.satisfiers {
                        println!(
                            "          satisfier: {} ({}, role: {})",
                            s.type_name,
                            s.file,
                            s.role.as_str()
                        );
                    }
                }
                WiringState::Unsupported => {
                    println!(
                        "unsupported {} — no conformance extraction for this language",
                        r.interface
                    );
                }
            }
        }
        println!(
            "\n{} interface(s) checked, {} finding(s). Heuristic: names are matched within the scanned tree; this is not a type checker.",
            results.len(),
            findings.len()
        );
    }

    // Exit non-zero on findings so CI can gate on it.
    if findings.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("{} wiring finding(s)", findings.len());
    }
}

/// Discover source files under `root` with the same exclusion pruning as
/// warm (delegate to hilo-graph's discovery used by the CLI's warm path).
fn collect(
    root: &std::path::Path,
    out: &mut Vec<std::path::PathBuf>,
    exclusions: &mut crate::commands::graph::ExclusionReport,
) -> Result<()> {
    let cwd = std::path::PathBuf::from(root);
    crate::commands::graph::collect_source_files(
        &cwd,
        std::path::Path::new(""),
        None,
        &[],
        out,
        exclusions,
    )
    .map_err(|e| anyhow::anyhow!("failed to walk {}: {e}", root.display()))
}
