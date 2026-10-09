//! `hilo graph wiring` — the wiring report.
//!
//! Two layers, one command:
//!
//! * **GAP-112** — the silent-fallback conformance detector. An interface
//!   consumed from at least one NON-test site whose satisfiers classify
//!   test-role is a FINDING: tests stay green while every production run
//!   silently falls to a slow/absent path (the TRBL-084 shape). Three-way
//!   verdict per interface — `pass` / `finding` / `unsupported` — never
//!   collapsed, `unsupported` never rendered as `pass`.
//!
//! * **GAP-113** — the aggregate per-module **wiring report** the external
//!   scorer reads: public surfaces, inbound/outbound connection counts,
//!   deltas against a stored baseline, untested surfaces, and a module
//!   classification (`entrypoint` / `service` / `lib` / `test` / `dead`).
//!
//! The report is assembled by [`hilo_graph::wiring_report::build_report`]
//! from four inputs, all of which the CLI owns the IO for:
//!
//! | input                            | artifact                          |
//! |----------------------------------|-----------------------------------|
//! | public surfaces                  | `.vfs/graph/surfaces.jsonl` (COV-1) |
//! | connection counts / edge deltas  | `.vfs/graph/edges.jsonl`          |
//! | tested/untested surfaces         | `.vfs/graph/coverage_links.jsonl` (COV-2) |
//! | module classification            | the JIT source scan itself        |
//!
//! An absent artifact is a named NOTE in the report, never a silent zero:
//! the module rows are still emitted (with zero counts and the reason stated).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result};
use hilo_graph::classify::classify_file;
use hilo_graph::coverage_links::read_links;
use hilo_graph::parser::Language;
use hilo_graph::surfaces::read_surfaces;
use hilo_graph::wiring_report::{
    build_report, module_index, module_of, read_edges, snapshot, BaselineInfo, ExcludedDir,
    InterfaceResult, InterfaceSatisfier, ModuleFile, SurfaceRef, WiringBaseline, WiringInputs,
};
use hilo_graph::{detect_wiring, WiringState};
use rayon::prelude::*;

/// The languages with conformance extraction. Everything else in a scanned
/// tree contributes an explicit `unsupported` line, never a silent skip.
const SUPPORTED: [Language; 5] = [
    Language::Go,
    Language::Rust,
    Language::Python,
    Language::TypeScript,
    Language::JavaScript,
];

/// One source file discovered by the scan.
struct Scanned {
    lang: Language,
    /// Repo-relative, `/`-separated.
    rel: String,
    src: String,
    /// `classify`'s role for the file (`entrypoint`, `library`, `test`, …).
    role: String,
}

/// `hilo graph wiring [--json] [--baseline FILE] [--write-baseline FILE] [path]`
pub fn run_wiring(
    json: bool,
    path: Option<String>,
    baseline: Option<String>,
    write_baseline: Option<String>,
) -> Result<()> {
    let root = match path {
        Some(p) => std::fs::canonicalize(&p).with_context(|| format!("bad path: {p}"))?,
        None => std::env::current_dir().context("failed to determine the current directory")?,
    };

    // ── 1. JIT source scan (discovery pruned exactly like warm) ──────────
    let mut source_files = Vec::new();
    let mut exclusions = crate::commands::graph::ExclusionReport::default();
    collect(&root, &mut source_files, &mut exclusions)?;

    let scanned: Vec<Scanned> = source_files
        .par_iter()
        .filter_map(|file| {
            let lang = Language::from_path(file)?;
            let rel = file
                .strip_prefix(&root)
                .unwrap_or(file)
                .to_string_lossy()
                .replace('\\', "/");
            let src = std::fs::read_to_string(file).ok()?;
            let role = classify_file(lang, &rel, &src)
                .map(|c| c.role)
                .unwrap_or_else(|_| "unknown".to_string());
            Some(Scanned {
                lang,
                rel,
                src,
                role,
            })
        })
        .collect();

    // ── 2. GAP-112 interface verdicts ───────────────────────────────────
    let seen: BTreeSet<Language> = scanned.iter().map(|s| s.lang).collect();
    let capable: Vec<(Language, String, String)> = scanned
        .iter()
        .filter(|s| SUPPORTED.contains(&s.lang))
        .map(|s| (s.lang, s.rel.clone(), s.src.clone()))
        .collect();
    let results = detect_wiring(&capable);
    let finding_count = results
        .iter()
        .filter(|r| r.state == WiringState::Finding)
        .count();
    let interfaces: Vec<InterfaceResult> = results
        .iter()
        .map(|r| InterfaceResult {
            interface: r.interface.clone(),
            state: match r.state {
                WiringState::Pass => "pass",
                WiringState::Finding => "finding",
                WiringState::Unsupported => "unsupported",
            }
            .to_string(),
            consumers: r.consumers.clone(),
            satisfiers: r
                .satisfiers
                .iter()
                .map(|s| InterfaceSatisfier {
                    type_name: s.type_name.clone(),
                    file: s.file.clone(),
                    role: s.role.as_str().to_string(),
                })
                .collect(),
        })
        .collect();

    // ── 3. Module inputs (classification comes from the scan) ───────────
    let mut module_files: BTreeMap<String, Vec<ModuleFile>> = BTreeMap::new();
    for s in &scanned {
        module_files
            .entry(module_of(&s.rel))
            .or_default()
            .push(ModuleFile {
                rel: s.rel.clone(),
                role: s.role.clone(),
            });
    }

    // ── 4. Persisted graph artifacts (absent = a named note) ────────────
    let graph_dir = root.join(".vfs").join("graph");
    let surfaces_path = graph_dir.join("surfaces.jsonl");
    let edges_path = graph_dir.join("edges.jsonl");
    let links_path = graph_dir.join("coverage_links.jsonl");

    let surfaces = read_surfaces(&surfaces_path).context("failed to read surfaces.jsonl")?;
    let edges = read_edges(&edges_path).context("failed to read edges.jsonl")?;
    let links = read_links(&links_path).context("failed to read coverage_links.jsonl")?;

    let mut notes: Vec<String> = Vec::new();
    if !surfaces_path.exists() {
        notes.push(format!(
            "{} absent — public surfaces unavailable; run `hilo graph surfaces`",
            surfaces_path.display()
        ));
    }
    if !edges_path.exists() {
        notes.push(format!(
            "{} absent — connection counts are zero; run `hilo graph warm`",
            edges_path.display()
        ));
    }
    if !links_path.exists() {
        notes.push(format!(
            "{} absent — every surface reported untested; run `hilo graph coverage-links`",
            links_path.display()
        ));
    }

    // ── 5. Baseline read (compare) and write ────────────────────────────
    let baseline_doc: Option<WiringBaseline> = match &baseline {
        Some(p) => {
            let abs = std::fs::canonicalize(p).unwrap_or_else(|_| Path::new(p).to_path_buf());
            let text = std::fs::read_to_string(&abs)
                .with_context(|| format!("failed to read baseline {p}"))?;
            Some(
                WiringBaseline::from_json_str(&text)
                    .map_err(|e| anyhow::anyhow!("baseline {p}: {e}"))?,
            )
        }
        None => None,
    };

    let written: Option<String> = match &write_baseline {
        Some(p) => {
            let index = module_index(&module_files, &surfaces);
            let current = snapshot(&surfaces, &edges, &index);
            let doc = WiringBaseline::from_snapshot(current);
            let text = doc.to_json_pretty().map_err(|e| anyhow::anyhow!("{e}"))?;
            std::fs::write(p, text).with_context(|| format!("failed to write baseline {p}"))?;
            Some(
                std::fs::canonicalize(p)
                    .unwrap_or_else(|_| Path::new(p).to_path_buf())
                    .to_string_lossy()
                    .into_owned(),
            )
        }
        None => None,
    };

    let baseline_info = if baseline.is_some() || write_baseline.is_some() {
        Some(BaselineInfo {
            path: baseline.as_ref().map(|p| {
                std::fs::canonicalize(p)
                    .unwrap_or_else(|_| Path::new(p).to_path_buf())
                    .to_string_lossy()
                    .into_owned()
            }),
            compared: baseline_doc.is_some(),
            written: written.clone(),
        })
    } else {
        None
    };

    // ── 6. Non-code top-level directories (AC5: named, never dropped) ───
    let excluded = excluded_dirs(&root, &module_files)?;

    let scope = crate::commands::surfaces::detect_scope(&root)
        .as_str()
        .to_string();

    let report = build_report(WiringInputs {
        root: root.to_string_lossy().into_owned(),
        scope,
        scanned_files: scanned.len(),
        languages_unsupported: seen
            .iter()
            .filter(|l| !SUPPORTED.contains(l))
            .map(|l| format!("{l:?}").to_lowercase())
            .collect(),
        module_files,
        surfaces: &surfaces,
        edges: &edges,
        links: &links,
        baseline: baseline_doc.as_ref(),
        baseline_info,
        interfaces,
        finding_count,
        excluded,
        notes,
    });

    if json {
        let out = serde_json::to_string_pretty(&report)
            .context("failed to serialize the wiring report as JSON")?;
        println!("{out}");
    } else {
        print_text(&report);
    }

    // Exit non-zero on GAP-112 findings so CI can gate on it.
    if report.finding_count == 0 {
        Ok(())
    } else {
        anyhow::bail!("{} wiring finding(s)", report.finding_count);
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

/// Enumerate the repository's top-level directories and classify each as a
/// module or an explicitly-excluded non-code directory (AC5).
///
/// A directory is a module when the scan found at least one supported source
/// file under it. Everything else carries the reason it is not: hidden
/// (pruned by discovery), a dependency/cache directory (pruned), or simply
/// "no supported source files" (docs, assets, licenses, …).
fn excluded_dirs(
    root: &Path,
    modules: &BTreeMap<String, Vec<ModuleFile>>,
) -> Result<Vec<ExcludedDir>> {
    let mut out = Vec::new();
    let entries =
        std::fs::read_dir(root).with_context(|| format!("failed to read {}", root.display()))?;
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if modules.contains_key(&name) {
            continue; // it is a module
        }
        let reason = if name.starts_with('.') {
            "hidden directory (pruned by discovery)".to_string()
        } else if crate::commands::guard::DEFAULT_PRUNE_DIRS.contains(&name.as_str()) {
            "dependency/cache directory (pruned by discovery)".to_string()
        } else {
            "no supported source files".to_string()
        };
        out.push(ExcludedDir { path: name, reason });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// Human-readable rendering: one line per module, the GAP-112 findings, the
/// excluded directories and the honest notes.
fn print_text(report: &hilo_graph::WiringReport) {
    for line in &report.notes {
        println!("note: {line}");
    }
    if let Some(b) = &report.baseline {
        if let Some(p) = &b.path {
            println!("baseline: compared against {p}");
        }
        if let Some(w) = &b.written {
            println!("baseline: wrote {w}");
        }
    }

    println!(
        "Modules: {}  Surfaces: {}  Untested: {}  Edges in/out: {}/{}",
        report.totals.modules,
        report.totals.surfaces,
        report.totals.untested_surfaces,
        report.totals.inbound_edges,
        report.totals.outbound_edges
    );
    for m in &report.modules {
        println!(
            "{:<10} {:<20} surfaces={:<4} untested={:<4} in={:<5} out={:<5}",
            m.classification.as_str(),
            m.module,
            m.surface_count,
            m.untested_count,
            m.inbound_edges,
            m.outbound_edges
        );
        if let Some(d) = &m.delta {
            let added: Vec<&str> = d
                .surfaces_added
                .iter()
                .map(|s: &SurfaceRef| s.name.as_str())
                .collect();
            let removed: Vec<&str> = d
                .surfaces_removed
                .iter()
                .map(|s: &SurfaceRef| s.name.as_str())
                .collect();
            if !added.is_empty()
                || !removed.is_empty()
                || !d.edges_added.is_empty()
                || !d.edges_removed.is_empty()
            {
                println!(
                    "           delta: +{} surface(s) [{}] -{} surface(s) [{}] edges +{}/-{}",
                    d.surfaces_added.len(),
                    added.join(", "),
                    d.surfaces_removed.len(),
                    removed.join(", "),
                    d.edges_added.len(),
                    d.edges_removed.len()
                );
            }
        }
    }

    if !report.excluded.is_empty() {
        println!("\nExcluded top-level directories:");
        for e in &report.excluded {
            println!("  {} — {}", e.path, e.reason);
        }
    }

    for lang in &report.languages_unsupported {
        println!("conformance: unsupported ({lang})");
    }
    if report.interfaces.is_empty() && report.languages_unsupported.is_empty() {
        println!("No interface consumption sites found — nothing to check.");
    }
    for r in &report.interfaces {
        match r.state.as_str() {
            "pass" => println!("pass      {} — non-test satisfier exists", r.interface),
            "finding" => {
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
                        s.type_name, s.file, s.role
                    );
                }
            }
            _ => println!(
                "unsupported {} — no conformance extraction for this language",
                r.interface
            ),
        }
    }
    println!(
        "\n{} interface(s) checked, {} finding(s). Heuristic: names are matched within the scanned tree; this is not a type checker.",
        report.interfaces.len(),
        report.finding_count
    );
}
