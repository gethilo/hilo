//! `hilo graph test-classes` — the test-class taxonomy and per-surface class
//! mix (COV-3).
//!
//! Reads the COV-1 surface inventory (`.vfs/graph/surfaces.jsonl`) and the
//! COV-2 coverage links (`.vfs/graph/coverage_links.jsonl`), classifies every
//! test-bearing source file in the repo, and joins the two: each surface
//! reports the set of test classes that reach it, and a surface reached by a
//! single class is flagged `class_gap` with the missing classes named.
//!
//! Both inputs are required and each is named in the error when absent — a
//! silent empty report would be indistinguishable from a repo with no tests.

use std::path::Path;

use anyhow::{Context, Result};

use hilo_graph::coverage_links::read_links;
use hilo_graph::surfaces::Surface;
use hilo_graph::test_classes::{derive, enumerate, TestClassReport};

/// Run the command in the process's current directory.
pub fn run(json: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to determine the current directory")?;
    let graph_dir = cwd.join(".vfs").join("graph");

    let surfaces = read_surfaces(&graph_dir.join("surfaces.jsonl"))?;

    let links_path = graph_dir.join("coverage_links.jsonl");
    if !links_path.exists() {
        anyhow::bail!(
            "no coverage links at {} — run `hilo graph coverage-links` first (COV-2)",
            links_path.display()
        );
    }
    let links = read_links(&links_path).context("failed to read coverage_links.jsonl")?;

    let files = enumerate(&cwd);
    let report = derive(&surfaces, &links, &files);

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .context("failed to serialize test-class report as JSON")?
        );
    } else {
        print_text(&report);
    }
    Ok(())
}

fn read_surfaces(path: &Path) -> Result<Vec<Surface>> {
    if !path.exists() {
        anyhow::bail!(
            "no surface inventory at {} — run `hilo graph surfaces` first (COV-1)",
            path.display()
        );
    }
    let mut surfaces = Vec::new();
    for line in std::fs::read_to_string(path)
        .context("failed to read surfaces.jsonl")?
        .lines()
    {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(s) = serde_json::from_str::<Surface>(line) {
            surfaces.push(s);
        }
    }
    Ok(surfaces)
}

fn print_text(report: &TestClassReport) {
    for rule in &report.rules {
        println!("rule: {rule}");
    }
    println!();

    println!("TEST-CLASS TOTALS ({} class(es)):", report.totals.len());
    for total in &report.totals {
        println!(
            "  {:<20} files={:<6} functions={}",
            total.class.as_str(),
            total.files,
            total.functions
        );
    }
    println!();

    println!(
        "PER-SURFACE CLASS MIX ({} surface(s)):",
        report.surfaces.len()
    );
    for row in &report.surfaces {
        let classes = if row.classes.is_empty() {
            "(none)".to_string()
        } else {
            row.classes
                .iter()
                .map(|c| c.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let flag = if row.class_gap { "  CLASS-GAP" } else { "" };
        println!(
            "  {:<16} {:<28} classes: {}{}",
            row.kind, row.name, classes, flag
        );
        if row.class_gap {
            let missing = row
                .missing_classes
                .iter()
                .map(|c| c.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            println!("      missing classes: {missing}");
        }
    }
    println!();

    println!(
        "CLASS-GAP surfaces ({}): {}",
        report.class_gap_surfaces.len(),
        if report.class_gap_surfaces.is_empty() {
            "none".to_string()
        } else {
            report
                .class_gap_surfaces
                .iter()
                .map(|id| short_id(id))
                .collect::<Vec<_>>()
                .join(", ")
        }
    );
    println!(
        "surfaces with no linked test ({}): {}",
        report.uncovered_surfaces.len(),
        if report.uncovered_surfaces.is_empty() {
            "none".to_string()
        } else {
            report
                .uncovered_surfaces
                .iter()
                .map(|id| short_id(id))
                .collect::<Vec<_>>()
                .join(", ")
        }
    );
}

/// A surface_id is a 64-char hex digest; the text output shows a short prefix
/// (the per-surface rows already carry the kind + name).
fn short_id(id: &str) -> String {
    id.chars().take(12).collect()
}
