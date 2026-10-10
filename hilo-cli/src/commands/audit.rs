//! COV-5: `hilo graph audit` — symbol-level connection + test audit.
//!
//! Answers, per public function/surface, two questions the file-scoped
//! primitives (`impact`, `related`, `untested`) cannot: is it reachable from a
//! declared entrypoint, and does it carry a coverage link. The output is the
//! NAMED list partitioned into three buckets (`unreachable`, `unlinked`, `ok`),
//! never a score — the derivation and its exact rules live in
//! [`hilo_graph::audit`].
//!
//! The command is a REPORT, not a gate: it always exits 0 (a zero bucket is a
//! real result, and the report always carries the rule that produced it), so
//! piping `--json` into `jq` never mixes an error frame into the document.

use anyhow::{Context, Result};
use hilo_graph::audit::{self, AuditBucket, AuditOptions, AuditReport};
use hilo_graph::coverage_links::read_links;

/// How many named members of each bucket the text view prints before it
/// elides the tail (the JSON view is never truncated).
const TEXT_LIMIT: usize = 40;

/// `hilo graph audit [--json] [path]`
pub fn run(json: bool, path: Option<String>) -> Result<()> {
    let root = match path {
        Some(p) => std::fs::canonicalize(&p).with_context(|| format!("bad path: {p}"))?,
        None => std::env::current_dir().context("failed to determine the current directory")?,
    };
    let root_str = root.to_string_lossy().to_string();

    let corpus = audit::collect_corpus(&root);

    // COV-2 links on disk (`.vfs/graph/coverage_links.jsonl`) enrich the
    // test-link evidence when present. Absent/foreign is not an error: the
    // corpus-derived test-name evidence stands on its own.
    let links_path = root.join(".vfs").join("graph").join("coverage_links.jsonl");
    let coverage_links = read_links(&links_path).unwrap_or_default();

    let opts = AuditOptions {
        max_evidence: 10,
        coverage_links,
    };
    let report = audit::audit(&corpus, &root_str, &opts);

    if json {
        let out = serde_json::to_string_pretty(&report)
            .context("failed to serialize the audit report as JSON")?;
        println!("{out}");
    } else {
        print_text(&report);
    }
    Ok(())
}

fn print_text(report: &AuditReport) {
    println!(
        "audit: {} public function(s) over {} file(s) in {}",
        report.public_symbols, report.scanned_files, report.root
    );
    println!();
    println!(
        "entrypoints: {} declared — {}",
        report.entrypoints.count, report.entrypoints.rule
    );
    for file in &report.entrypoints.files {
        println!("    {file}");
    }
    println!();

    for bucket in AuditBucket::ALL {
        let b = report.buckets.get(bucket);
        println!("{}: {}", bucket.as_str(), b.count);
        println!("    rule: {}", b.rule);
        for symbol in b.symbols.iter().take(TEXT_LIMIT) {
            println!("    - {}  {}:{}", symbol.name, symbol.file, symbol.line);
        }
        if b.symbols.len() > TEXT_LIMIT {
            println!(
                "    … {} more (use --json for the full list)",
                b.symbols.len() - TEXT_LIMIT
            );
        }
        println!();
    }

    println!(
        "unknown: {} — {}",
        report.unknown.count, report.unknown.rule
    );
    for file in report.unknown.files.iter().take(TEXT_LIMIT) {
        println!("    - {file}");
    }
    if report.unknown.files.len() > TEXT_LIMIT {
        println!(
            "    … {} more (use --json for the full list)",
            report.unknown.files.len() - TEXT_LIMIT
        );
    }
    println!();

    println!("census:");
    for row in &report.census {
        println!(
            "    {}: {} file(s), {} public fn — {}",
            row.language, row.files, row.public_symbols, row.extractor
        );
    }
    println!();
    println!("rules:");
    for rule in &report.rules {
        println!("    - {rule}");
    }
}
