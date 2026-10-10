//! `hilo graph rollup` — surface grouping + rollup (COV-4).
//!
//! Reads the COV-1 surface inventory (`.vfs/graph/surfaces.jsonl`) and, when
//! present, the COV-2 evidenced links (`.vfs/graph/coverage_links.jsonl`),
//! resolves every surface into a group by the documented precedence
//! (explicit `user.vfs.feature` / `user.vfs.component` xattr → crate boundary
//! → module path prefix), and rolls coverage and the COV-3 class mix up to
//! that group level.
//!
//! The grouping model and the arithmetic live in [`hilo_graph::rollup`];
//! this module owns the I/O (which artifacts to read, how a missing one is
//! reported) and the rendering.

use std::path::Path;

use anyhow::{Context, Result};

use hilo_graph::coverage_links::read_links;
use hilo_graph::rollup::{GroupBy, RollupReport};
use hilo_graph::surfaces::Surface;
use hilo_graph::test_classes::{derive, enumerate};
use hilo_metadata::xattr::group_annotations;

/// Maximum group members named inline before a list truncates to
/// `... and N more` (the same cap convention `graph warm` uses for paths).
const NAME_LIST_CAP: usize = 20;

/// `hilo graph rollup [--by feature|crate|module] [--json] [--group <name>]`
pub fn run(json: bool, by: &str, group: Option<&str>) -> Result<()> {
    // A `--by` typo is a usage error, so it fails before any artifact is read.
    let by = GroupBy::parse(by).ok_or_else(|| {
        let valid = GroupBy::ALL
            .iter()
            .map(|b| b.as_str())
            .collect::<Vec<_>>()
            .join(" | ");
        anyhow::anyhow!("unknown grouping dimension '{by}'. Valid dimensions: {valid}")
    })?;

    let cwd = std::env::current_dir().context("failed to determine the current directory")?;
    let graph_dir = cwd.join(".vfs").join("graph");

    let surfaces = read_surfaces(&graph_dir.join("surfaces.jsonl"))?;

    // COV-2 is the coverage evidence. Its absence is a NAMED state — the
    // rollup still answers (grouping needs no coverage), but every surface
    // reads uncovered *with the reason stated*, never as a silent zero.
    let links_path = graph_dir.join("coverage_links.jsonl");
    let (links, coverage_evidence) = if links_path.exists() {
        let links = read_links(&links_path).context("failed to read coverage_links.jsonl")?;
        let link_count = links.len();
        (
            links,
            format!(
                "{} ({} link row(s) joined on surface_id)",
                links_path.display(),
                link_count
            ),
        )
    } else {
        (
            Vec::new(),
            format!(
                "absent: no {} — every surface reports uncovered until \
                 `hilo graph coverage-links` (COV-2) runs",
                links_path.display()
            ),
        )
    };

    // The class mix is COV-3's own derivation, so rollup and test-classes can
    // never disagree about what "covered" means.
    let files = enumerate(&cwd);
    let class_report = derive(&surfaces, &links, &files);

    // Explicit annotations are read from each surface owner file on disk.
    // `group_annotations` is best-effort: an unreadable/unset xattr means
    // "no explicit group", and the structural fallback covers it.
    let annotations = |owner_file: &str| group_annotations(&cwd.join(owner_file));

    let mut report = hilo_graph::rollup::rollup(
        &cwd,
        &surfaces,
        &class_report.surfaces,
        &annotations,
        by,
        &coverage_evidence,
    );

    if let Some(name) = group {
        // AC5: an unknown group is a loud error, never a success-shaped
        // empty report (the DF-WARPFS-113 contract).
        if !report.retain_group(name) {
            anyhow::bail!(
                "unknown group '{name}': not among the {} group(s) of this rollup — \
                 run `hilo graph rollup --by {}` to list them",
                report.group_count,
                by.as_str()
            );
        }
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .context("failed to serialize rollup report as JSON")?
        );
    } else {
        print_text(&report);
    }
    Ok(())
}

/// Read the COV-1 inventory, failing loudly when it is absent: an empty
/// rollup would be indistinguishable from a repo with no surfaces.
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

fn print_text(report: &RollupReport) {
    for rule in &report.rules {
        println!("rule: {rule}");
    }
    println!();
    println!(
        "grouping: --by {} — {} surface(s) in {} group(s)",
        report.by, report.total_surfaces, report.group_count
    );
    println!("fallback: {}", report.fallback_note);
    println!("coverage: {}", report.coverage_evidence);
    println!(
        "sources: {}",
        join_counts(
            report
                .source_census
                .iter()
                .map(|r| (r.source.as_str(), r.count))
        )
    );
    println!();

    println!("GROUPS ({}):", report.groups.len());
    for group in &report.groups {
        println!(
            "  {:<24} surfaces={} covered={} uncovered={} class-gap={}",
            group.group, group.surface_count, group.covered, group.uncovered, group.class_gap_count,
        );
        println!(
            "      class mix: {}",
            join_counts(
                group
                    .class_mix
                    .iter()
                    .map(|c| (c.class.as_str(), c.surfaces))
            )
        );
        println!(
            "      sources: {}",
            join_counts(group.sources.iter().map(|s| (s.source.as_str(), s.count)))
        );
        print_name_list("uncovered", &group.uncovered_names);
        print_name_list("class-gap", &group.class_gap_names);
    }
    println!();

    println!("SURFACE GROUPING ({}):", report.surfaces.len());
    for row in &report.surfaces {
        println!(
            "  {:<16} {:<28} -> {}  [{}]",
            row.kind,
            row.name,
            row.group,
            row.source.as_str()
        );
    }

    // AC5: a named group answers with its own gap list.
    if let (Some(name), Some(group)) = (&report.group_filter, report.groups.first()) {
        println!();
        println!(
            "GAP LIST for group '{name}' ({} surface(s): {} covered, {} uncovered, {} class-gap):",
            group.surface_count, group.covered, group.uncovered, group.class_gap_count
        );
        println!(
            "  uncovered surfaces ({}): {}",
            group.uncovered,
            join_or_none(&group.uncovered_names)
        );
        println!(
            "  class-gap surfaces ({}): {}",
            group.class_gap_count,
            join_or_none(&group.class_gap_names)
        );
    }
}

/// `key=count` pairs, dropping zero counts but never the keys that matter:
/// a source or class at zero is already visible on the row's own counts, and
/// the class mix's full zero-row set is what `--json` carries.
fn join_counts<I: Iterator<Item = (&'static str, usize)>>(counts: I) -> String {
    let rendered: Vec<String> = counts
        .filter(|(_, n)| *n > 0)
        .map(|(name, n)| format!("{name}={n}"))
        .collect();
    if rendered.is_empty() {
        "none".to_string()
    } else {
        rendered.join(" ")
    }
}

fn print_name_list(label: &str, names: &[String]) {
    if names.is_empty() {
        return;
    }
    println!(
        "      {label} ({}): {}",
        names.len(),
        capped_list(names, NAME_LIST_CAP)
    );
}

fn capped_list(names: &[String], cap: usize) -> String {
    let shown = names
        .iter()
        .take(cap)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if names.len() > cap {
        format!("{shown} ... and {} more", names.len() - cap)
    } else {
        shown
    }
}

fn join_or_none(names: &[String]) -> String {
    if names.is_empty() {
        "none".to_string()
    } else {
        capped_list(names, NAME_LIST_CAP)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hilo_graph::rollup::{GroupSource, SurfaceGrouping};

    #[test]
    fn unknown_by_dimension_fails_before_touching_the_filesystem() {
        let err = run(false, "crates", None).expect_err("a typo must be a usage error");
        let msg = format!("{err:#}");
        assert!(msg.contains("unknown grouping dimension 'crates'"), "{msg}");
        assert!(msg.contains("feature | crate | module"), "{msg}");
    }

    #[test]
    fn name_lists_cap_and_never_render_empty_as_none_in_a_row() {
        let names: Vec<String> = (0..5).map(|i| format!("s{i}")).collect();
        assert_eq!(capped_list(&names, 20), "s0, s1, s2, s3, s4");
        assert_eq!(capped_list(&names, 2), "s0, s1 ... and 3 more");
        assert_eq!(join_or_none(&[]), "none");
        assert_eq!(join_or_none(&names), "s0, s1, s2, s3, s4");
    }

    #[test]
    fn count_renderer_drops_zeroes_but_names_the_all_zero_case() {
        assert_eq!(join_counts(std::iter::empty()), "none");
        assert_eq!(
            join_counts([("crate", 3usize), ("module", 0usize)].into_iter()),
            "crate=3"
        );
    }

    #[test]
    fn surface_row_renders_the_recorded_source() {
        // AC1: the chosen source is on the row, so rendering never has to
        // re-infer it.
        let row = SurfaceGrouping {
            surface_id: "id".into(),
            kind: "cli_verb".into(),
            name: "init".into(),
            group: "hilo-cli".into(),
            source: GroupSource::Crate,
        };
        assert_eq!(row.source.as_str(), "crate");
    }
}
