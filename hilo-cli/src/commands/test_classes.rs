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
    // DF-WARPFS-159: the COV chain is self-hosted-only. Refuse HERE on a
    // foreign tree, naming that limitation, before any artifact check can
    // send the user to `hilo graph surfaces` — a command that refuses on
    // this very tree.
    crate::commands::surfaces::require_self_scope(&cwd, "graph test-classes")?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// DF-WARPFS-159 regression: on a WARMed foreign repo (its own `.git` plus
    /// planted COV artifacts) `graph test-classes` must refuse at the scope
    /// precondition with the self-only limitation NAMED — not the
    /// missing-artifact instruction that would send the user to `hilo graph
    /// surfaces`, a command that refuses on this very tree — and must leave
    /// every planted artifact byte-identical (no foreign writes, no new
    /// artifacts). Unit-level: this exercises the exact `run` path the CLI
    /// dispatches (cli.rs `test_classes::run`), not an external process.
    #[test]
    fn warmed_foreign_repo_gets_the_named_scope_error_and_no_artifact_writes() {
        let foreign = crate::commands::surfaces::scope_fixtures::warmed_foreign_repo();
        let before = crate::commands::surfaces::scope_fixtures::snapshot_tree(foreign.path());

        // `run` reads the process CWD (the CLI contract), so switch into the
        // foreign repo for the call and restore it on every exit path — the
        // discipline the cwd-touching graph.rs suites document. The lock
        // serializes the COV scope tests against each other: the cwd is
        // process-global and the harness runs tests on parallel threads.
        let _guard = crate::commands::surfaces::scope_fixtures::cwd_lock();
        let prev_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(foreign.path()).unwrap();
        let result = run(false);
        std::env::set_current_dir(prev_cwd).unwrap();
        drop(_guard);

        let err = result.expect_err("a warmed foreign repo must be refused");
        let msg = format!("{err:#}");
        assert!(msg.contains("graph test-classes"), "{msg}");
        assert!(msg.contains("Hilo repository itself only"), "{msg}");
        assert!(msg.contains("scope: foreign"), "{msg}");
        assert!(
            !msg.contains("run `hilo graph surfaces` first"),
            "the foreign refusal must not instruct the user to run a command \
             that refuses on this very tree: {msg}"
        );
        assert_eq!(
            before,
            crate::commands::surfaces::scope_fixtures::snapshot_tree(foreign.path()),
            "planted foreign artifacts must be neither modified nor extended"
        );
    }
}
