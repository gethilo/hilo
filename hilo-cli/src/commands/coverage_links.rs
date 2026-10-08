//! Coverage-link derivation — link the repo's tests to its surfaces with
//! evidence (COV-2).
//!
//! Derivation layers, ADDITIVE and ordered (weakest runs first; a stronger
//! layer merges over the pair instead of duplicating it):
//!
//! 1. `import` — the test file's AST imports the target (reuses the same
//!    `Parser` the graph itself uses, so `tested_by` edges and coverage
//!    links always agree on what an import IS).
//! 2. `symbol_name_match` — the test's source names the surface's contract
//!    name on a word boundary (covers test surfaces reachable without a
//!    direct import, e.g. a CLI verb exercised via the compiled binary).
//! 3. `runtime_trace` — reserved in-process (none recorded today); the
//!    merge accepts links of this kind from a recorded trace file, which
//!    RAISES confidence on an existing import link to 1.0.
//!
//! Every surface that no test reaches lands in the report's unlinked set
//! WITH a cause — never a bare empty list (AC5). A test that establishes no
//! link does not silently disappear either: it lands in the `no_test_evidence`
//! census alongside the surfaces it failed to reach.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use hilo_graph::coverage_links::{
    cause_census, CoverageLink, CoverageLinkReport, EvidenceKind, UnlinkedSurface,
};
use hilo_graph::surfaces::{Surface, SurfaceKind};
use hilo_graph::{Language, Parser, PkgResolver};

use anyhow::Context as _;
use hilo_graph::classify::is_test_file;

/// Directory of a recorded runtime trace (optional input). A file named
/// `runtime_trace.jsonl` under `.vfs/graph/` may carry links produced by an
/// external execution tracer; each row needs only `test_file` and `target`.
const RUNTIME_TRACE_FILE: &str = "runtime_trace.jsonl";

/// Derive coverage links for `surfaces` from the repo rooted at `root`.
///
/// Returns the merged link set plus the report scaffolding (unlinked
/// surfaces with causes, per-cause census, derivation rules). Writes
/// nothing.
pub fn derive(root: &Path, surfaces: &[Surface]) -> CoverageLinkReport {
    // surfaces.jsonl keyed for resolution: surface_id by (kind, owner_file,
    // owner_symbol) and by name — a test import edge target is a FILE path,
    // so the primary join is owner_file → every surface that file owns.
    let mut by_owner_file: BTreeMap<&str, Vec<&Surface>> = BTreeMap::new();
    let mut by_name: BTreeMap<&str, Vec<&Surface>> = BTreeMap::new();
    for s in surfaces {
        by_owner_file
            .entry(s.owner_file.as_str())
            .or_default()
            .push(s);
        by_name.entry(s.name.as_str()).or_default().push(s);
    }

    // 1. Collect the repo's test files (same extension discovery the graph
    //    uses: any file whose language is parseable AND that the graph's own
    //    classifier calls a test file — so coverage links and `tested_by`
    //    edges always agree on what a test IS).
    let mut test_files: Vec<String> = Vec::new();
    collect_files(root, Path::new(""), &mut test_files);
    test_files.sort();

    // 2. import layer — parse each test file once, read its import edges,
    //    resolve edge targets to surfaces via owner_file AND via the same
    //    pkg-node resolution `untested_files_at` uses (GAP-057/064/066
    //    parity): a Python/Go test imports `pkg:<module>`, a Rust test
    //    imports its crate root — neither lands on the owner file directly.
    //    The PkgResolver maps each owner file to the node forms its
    //    importers would actually name, and the reverse index joins edges.
    let mut links: Vec<CoverageLink> = Vec::new();
    let mut parser_failures: BTreeMap<String, usize> = BTreeMap::new();
    let mut import_targets: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut pkg_resolver = PkgResolver::new();
    let mut node_to_owners: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for s in surfaces {
        // The resolver anchors its walk at the PROCESS CWD, so the
        // repo-relative `owner_file` must be made absolute against `root`
        // for the node to resolve — then stored back in the same
        // repo-relative/`pkg:` vocabulary the import edges emit.
        let abs_owner = root.join(&s.owner_file).to_string_lossy().into_owned();
        let mut nodes = vec![s.owner_file.clone()];
        if let Some(pkg) = pkg_resolver.pkg_node(&abs_owner) {
            nodes.push(pkg);
        }
        for node in nodes {
            node_to_owners
                .entry(node)
                .or_default()
                .push(s.owner_file.clone());
        }
    }
    for tf in &test_files {
        let Ok(source) = std::fs::read_to_string(root.join(tf)) else {
            *parser_failures
                .entry("unreadable source".to_string())
                .or_default() += 1;
            continue;
        };
        let Some(lang) = Language::from_extension(
            Path::new(tf)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or(""),
        ) else {
            // A test-like file in a language without a parser is a named
            // inference failure, not silence (AC5).
            *parser_failures
                .entry(format!(
                    "inference failed for language: unknown extension .{}",
                    Path::new(tf)
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("")
                ))
                .or_default() += 1;
            continue;
        };
        let mut parser = match Parser::for_language(lang) {
            Ok(p) => p,
            Err(e) => {
                *parser_failures
                    .entry(format!(
                        "inference failed for language: {}",
                        lang_debug(lang)
                    ))
                    .or_default() += 1;
                eprintln!("coverage-links: parser unavailable for {tf}: {e}");
                continue;
            }
        };
        // COV-2 CWD-PARITY: the parser resolves Rust module paths (and the
        // PkgResolver resolves pkg: nodes) against the process CWD, so a
        // repo-relative `tf` would resolve against the WRONG tree whenever
        // the CLI runs from a subdirectory — or in-process, at all. Parse
        // with an ABSOLUTE path so module resolution anchors at the root's
        // real files, then relativize the emitted targets back to
        // repo-relative form so they join `owner_file` (which is stored
        // repo-relative).
        let abs_path = root.join(tf);
        let abs_str = abs_path.to_string_lossy().into_owned();
        let edges = match parser.parse_imports(&abs_str, &source) {
            Ok(e) => e,
            Err(e) => {
                *parser_failures
                    .entry(format!(
                        "inference failed for language: {}",
                        lang_debug(lang)
                    ))
                    .or_default() += 1;
                eprintln!("coverage-links: parse failed for {tf}: {e}");
                continue;
            }
        };
        for edge in &edges {
            // sys:/std:/external: pseudo-nodes are never in-repo surfaces;
            // pkg:/local:/bare file paths resolve via the node index.
            if edge.to.starts_with("sys:")
                || edge.to.starts_with("std:")
                || edge.to.starts_with("external:")
            {
                continue;
            }
            // Relativize absolute targets to repo-relative (the node index's
            // key space). pkg:/local: pass through untouched.
            let target = if edge.to.starts_with("pkg:") || edge.to.starts_with("local:") {
                edge.to.clone()
            } else {
                Path::new(&edge.to)
                    .strip_prefix(root)
                    .unwrap_or(Path::new(&edge.to))
                    .to_string_lossy()
                    .replace('\\', "/")
            };
            import_targets
                .entry(tf.clone())
                .or_default()
                .push(target.clone());
            if let Some(owners) = node_to_owners.get(&target) {
                for owner in owners {
                    if let Some(hit) = by_owner_file.get(owner.as_str()) {
                        for surface in hit {
                            links.push(CoverageLink::new(
                                tf.clone(),
                                surface.surface_id.clone(),
                                EvidenceKind::Import,
                            ));
                        }
                    }
                }
            }
        }
    }

    // 3. symbol_name_match layer — the test names the surface's contract
    //    name on a word boundary (import edges cannot see binary-driven or
    //    reflection-driven coverage). Only surfaces NOT already linked by
    //    import from that test get a symbol match, so the merge has nothing
    //    to downgrade (AC3: stronger never demoted; weaker never duplicates).
    let linked_pairs: BTreeSet<(String, String)> = links
        .iter()
        .map(|l| (l.test_file.clone(), l.target.clone()))
        .collect();
    for tf in &test_files {
        let Ok(source) = std::fs::read_to_string(root.join(tf)) else {
            continue;
        };
        for (name, hit) in &by_name {
            if name.len() < 4 {
                // Too short to be a meaningful word-boundary match.
                continue;
            }
            if !source_contains_word(&source, name) {
                continue;
            }
            for surface in hit {
                let pair = (tf.clone(), surface.surface_id.clone());
                if linked_pairs.contains(&pair) {
                    continue;
                }
                links.push(CoverageLink::new(
                    tf.clone(),
                    surface.surface_id.clone(),
                    EvidenceKind::SymbolNameMatch,
                ));
            }
        }
    }

    // 4. runtime_trace layer — accept recorded traces, the strongest
    //    evidence. The merge raises existing links (never duplicates).
    let trace_path = root.join(".vfs").join("graph").join(RUNTIME_TRACE_FILE);
    if trace_path.exists() {
        match std::fs::read_to_string(&trace_path) {
            Ok(text) => {
                let mut traced = 0usize;
                for line in text.lines() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    #[derive(serde::Deserialize)]
                    struct TraceRow {
                        test_file: String,
                        target: String,
                    }
                    let Ok(row) = serde_json::from_str::<TraceRow>(line) else {
                        continue;
                    };
                    // A trace may name a surface_id directly or a file that
                    // owns surfaces — both resolve to real links.
                    if let Some(hit) = by_owner_file.get(row.target.as_str()) {
                        for surface in hit {
                            links.push(CoverageLink::new(
                                row.test_file.clone(),
                                surface.surface_id.clone(),
                                EvidenceKind::RuntimeTrace,
                            ));
                            traced += 1;
                        }
                    } else if surfaces.iter().any(|s| s.surface_id == row.target) {
                        links.push(CoverageLink::new(
                            row.test_file.clone(),
                            row.target.clone(),
                            EvidenceKind::RuntimeTrace,
                        ));
                        traced += 1;
                    }
                }
                let _ = traced;
            }
            Err(e) => eprintln!("coverage-links: trace unreadable: {e}"),
        }
    }

    // Merge — one row per (test_file, target); stronger layer raises.
    let mut links = hilo_graph::coverage_links::merge_links(Vec::new(), links);
    links.sort_by(|a, b| (&a.test_file, &a.target).cmp(&(&b.test_file, &b.target)));

    // 5. Unlinked set — every surface with NO link at all, each with a
    //    cause derived from what the derivation actually saw (AC5).
    let linked_targets: BTreeSet<&str> = links.iter().map(|l| l.target.as_str()).collect();
    let mut unlinked: Vec<UnlinkedSurface> = Vec::new();
    for s in surfaces {
        if linked_targets.contains(s.surface_id.as_str()) {
            continue;
        }
        let cause = unlinked_cause(s, &import_targets, &test_files);
        unlinked.push(UnlinkedSurface {
            surface_id: s.surface_id.clone(),
            kind: s.kind.as_str().to_string(),
            name: s.name.clone(),
            cause,
        });
    }

    // A test that establishes no link must not silently disappear: count
    // test files with zero derived links into the census (AC2).
    let no_test_evidence = test_files
        .iter()
        .filter(|tf| !links.iter().any(|l| &l.test_file == *tf))
        .count();

    let mut census = cause_census(&unlinked);
    if no_test_evidence > 0 {
        census.push((
            "test files with no derived link".to_string(),
            no_test_evidence,
        ));
    }
    for (cause, n) in &parser_failures {
        census.push((cause.clone(), *n));
    }
    census.sort();

    let mut rules = vec![
        format!(
            "import layer: tree-sitter import edges from {} test file(s) resolved against surfaces.jsonl owner_file",
            test_files.len()
        ),
        "symbol_name_match layer: word-boundary match of surface contract name in test source (name >= 4 chars, import-linked pairs excluded)".to_string(),
    ];
    if trace_path.exists() {
        rules.push(format!(
            "runtime_trace layer: rows from {} (strongest evidence; raises existing links)",
            trace_path.display()
        ));
    } else {
        rules.push(
            "runtime_trace layer: no trace recorded (runtime_trace.jsonl absent)".to_string(),
        );
    }

    CoverageLinkReport {
        schema: CoverageLinkReport::SCHEMA,
        links,
        unlinked,
        cause_census: census,
        rules,
    }
}

/// Cause for a surface having no link, derived from the derivation's own
/// observations — never a bare "unknown".
fn unlinked_cause(
    surface: &Surface,
    import_targets: &BTreeMap<String, Vec<String>>,
    test_files: &[String],
) -> String {
    if test_files.is_empty() {
        return "no test files found in repo".to_string();
    }
    // Did any test import the surface's owner file but the surface itself
    // not resolve? Then the target IS a modeled surface and genuinely
    // unexercised.
    let owner_imported = import_targets
        .values()
        .any(|targets| targets.contains(&surface.owner_file));
    if owner_imported {
        return "owner file imported by a test but no surface-level evidence".to_string();
    }
    match surface.kind {
        // These surfaces live in files tests cannot import (the CLI entry,
        // the UDL, the manifest struct) — a whole edge class the old
        // import-only proxy silently missed.
        SurfaceKind::CliVerb
        | SurfaceKind::CliFlag
        | SurfaceKind::FfiExport
        | SurfaceKind::ConfigKey => {
            "owner file not imported by any test (declaration-only surface)".to_string()
        }
        _ => "no test file imports or names this surface".to_string(),
    }
}

/// Word-boundary containment: `needle` must appear in `haystack` bounded by
/// non-identifier characters on both sides.
fn source_contains_word(haystack: &str, needle: &str) -> bool {
    let bytes = haystack.as_bytes();
    let nb = needle.as_bytes();
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(needle) {
        let abs = start + pos;
        let end = abs + nb.len();
        let left_ok = abs == 0 || !is_ident_byte(bytes[abs - 1]);
        let right_ok = end >= bytes.len() || !is_ident_byte(bytes[end]);
        if left_ok && right_ok {
            return true;
        }
        start = abs + 1;
        if start >= haystack.len() {
            break;
        }
    }
    false
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Debug label for a language in the census (the `Language` enum has no
/// Display; its kind identity is all the report needs).
fn lang_debug(lang: Language) -> &'static str {
    match lang {
        Language::Go => "go",
        Language::Python => "python",
        Language::TypeScript => "typescript",
        Language::Rust => "rust",
        Language::JavaScript => "javascript",
        Language::Java => "java",
        Language::C => "c",
        Language::Cpp => "cpp",
        Language::Ruby => "ruby",
        Language::CSharp => "csharp",
        Language::Kotlin => "kotlin",
        Language::Php => "php",
        Language::Swift => "swift",
        Language::Elixir => "elixir",
        Language::Haskell => "haskell",
        Language::Erlang => "erlang",
        Language::Scala => "scala",
        Language::Zig => "zig",
        Language::Lua => "lua",
        Language::Dart => "dart",
        Language::Clojure => "clojure",
        Language::OCaml => "ocaml",
        Language::R => "r",
        Language::Julia => "julia",
        Language::Elm => "elm",
        Language::Nim => "nim",
        Language::Terraform => "terraform",
    }
}

fn collect_files(dir: &Path, rel: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') || name_str == "target" || name_str == "node_modules" {
            continue;
        }
        let entry_rel = rel.join(&*name_str);
        let path = dir.join(&*name_str);
        if path.is_dir() {
            collect_files(&path, &entry_rel, out);
        } else if Language::from_extension(
            Path::new(name_str.as_ref())
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or(""),
        )
        .is_some()
            && is_test_file(&entry_rel.to_string_lossy().replace('\\', "/"))
        {
            out.push(entry_rel.to_string_lossy().replace('\\', "/"));
        }
    }
}

/// The `hilo graph coverage-links` command: derive, persist to
/// `.vfs/graph/coverage_links.jsonl` (deduped by `link_id`, upgrades in
/// place), and print links + unlinked-with-cause in text or JSON.
/// DF-WARPFS-113: an unknown surface id/name is almost always a typo — fail
/// loudly (mirrors the GAP-085 unknown-module contract in graph.rs) instead of
/// printing a success-shaped empty report. A KNOWN surface with zero links
/// keeps the empty-output-with-cause path.
fn ensure_surface_known(
    surfaces: &[Surface],
    filter: &str,
    surfaces_path: &Path,
) -> anyhow::Result<()> {
    let known = surfaces
        .iter()
        .any(|s| s.surface_id == filter || s.name == filter);
    if !known {
        anyhow::bail!(
            "unknown surface '{filter}': not found in {} (surface_id or name) — check `hilo graph surfaces` for valid names",
            surfaces_path.display()
        );
    }
    Ok(())
}

pub fn run(json: bool, surface: Option<&str>, unlinked_only: bool) -> anyhow::Result<()> {
    let cwd = std::env::current_dir().context("failed to determine the current directory")?;

    let surfaces_path = cwd.join(".vfs").join("graph").join("surfaces.jsonl");
    if !surfaces_path.exists() {
        anyhow::bail!(
            "no surface inventory at {} — run `hilo graph surfaces` first (COV-1)",
            surfaces_path.display()
        );
    }
    let mut surfaces: Vec<Surface> = Vec::new();
    for line in std::fs::read_to_string(&surfaces_path)
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

    let mut report = derive(&cwd, &surfaces);

    if let Some(filter) = surface {
        // DF-WARPFS-113: a typo'd surface id/name must fail loudly, never
        // masquerade as a resolved-but-unlinked surface.
        ensure_surface_known(&surfaces, filter, &surfaces_path)?;
        // Accept either a full surface_id or a surface name.
        report.links.retain(|l| {
            l.target == filter
                || surfaces
                    .iter()
                    .any(|s| s.surface_id == l.target && s.name == filter)
        });
        report
            .unlinked
            .retain(|u| u.surface_id == filter || u.name == filter);
        report.cause_census = cause_census(&report.unlinked);
    }
    if unlinked_only {
        report.links.clear();
    }

    let path = cwd.join(".vfs").join("graph").join("coverage_links.jsonl");
    if !unlinked_only {
        let delta = hilo_graph::coverage_links::append_coverage_links_deduped(&path, &report.links)
            .context("failed to write coverage_links.jsonl")?;
        if delta > 0 {
            eprintln!(
                "wrote {delta} new/updated link row(s) to {}",
                path.display()
            );
        }
    }

    if json {
        let out = serde_json::to_string_pretty(&report)
            .context("failed to serialize coverage-link report as JSON")?;
        println!("{out}");
    } else {
        for rule in &report.rules {
            println!("rule: {rule}");
        }
        println!();
        for link in &report.links {
            println!(
                "LINK  {}  ->  {}  [{}] confidence {:.2}",
                link.test_file,
                link.target,
                link.evidence_kind.as_str(),
                link.confidence
            );
        }
        if report.links.is_empty() {
            println!("LINK  (none)");
        }
        println!();
        if report.unlinked.is_empty() {
            println!("UNLINKED: none — every surface has at least one evidenced link");
        } else {
            println!("UNLINKED: {} surface(s)", report.unlinked.len());
            for row in &report.unlinked {
                println!(
                    "  {}  {}  [{}] cause: {}",
                    row.kind, row.name, row.surface_id, row.cause
                );
            }
            println!();
            println!("CAUSES:");
            for (cause, n) in &report.cause_census {
                println!("  {n}\t{cause}");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hilo_graph::coverage_links::link_id as hilo_link_id;
    use hilo_graph::surfaces::{surface_id, SurfaceKind};
    use tempfile::tempdir;

    /// The AC6 fixture: a scratch repo with a deliberately-linked surface
    /// (a module a test imports and a surface owned by that module) and a
    /// deliberately-unlinked surface (owned by a file no test touches).
    #[derive(Debug)]
    struct Fixture {
        root: tempfile::TempDir,
        linked_surface: Surface,
        unlinked_surface: Surface,
    }

    fn build_fixture() -> Fixture {
        let root = tempdir().unwrap();
        let base = root.path();
        std::fs::create_dir_all(base.join("src")).unwrap();
        std::fs::create_dir_all(base.join("tests")).unwrap();
        // The linked module + its test: a real crate layout where the test
        // IMPORTS the module through the crate root (`use crate::widget;`)
        // — the shape the Rust parser resolves to a file→file edge
        // (GAP-043), the exact evidence the import layer reads. The crate
        // also needs a Cargo.toml so PkgResolver resolves the crate root.
        std::fs::write(
            base.join("Cargo.toml"),
            "[package]\nname = \"widgetcrate\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(base.join("src/widget.rs"), "pub fn spin() -> u32 { 1 }\n").unwrap();
        std::fs::write(base.join("src/lib.rs"), "pub mod widget;\n").unwrap();
        std::fs::write(
            base.join("tests/widget_test.rs"),
            "use crate::widget;\n#[test]\nfn spins() { assert_eq!(widget::spin(), 1); }\n",
        )
        .unwrap();
        // The unlinked module: real code, zero tests.
        std::fs::write(base.join("src/orphan.rs"), "pub fn lonely() -> u32 { 2 }\n").unwrap();
        let linked_surface = Surface::new(
            SurfaceKind::PublicApiItem,
            "src/widget.rs",
            "spin",
            "spin",
            true,
        );
        let unlinked_surface = Surface::new(
            SurfaceKind::PublicApiItem,
            "src/orphan.rs",
            "lonely",
            "lonely",
            true,
        );
        Fixture {
            root,
            linked_surface,
            unlinked_surface,
        }
    }

    #[test]
    fn fixture_proves_both_directions_linked_with_evidence_unlinked_with_cause() {
        let fx = build_fixture();
        let report = derive(
            fx.root.path(),
            &[fx.linked_surface.clone(), fx.unlinked_surface.clone()],
        );

        // Direction 1: the linked surface reports its link WITH evidence.
        assert_eq!(report.links.len(), 1, "exactly one link: {report:?}");
        let link = &report.links[0];
        assert_eq!(link.target, fx.linked_surface.surface_id);
        assert_eq!(link.evidence_kind, EvidenceKind::Import);
        assert!(link.confidence > 0.0, "every link carries confidence");
        // Only the ORPHAN surface may be unlinked — the linked one must not.
        assert_eq!(
            report
                .unlinked
                .iter()
                .map(|u| u.surface_id.as_str())
                .collect::<Vec<_>>(),
            vec![fx.unlinked_surface.surface_id.as_str()],
            "exactly the orphan surface is unlinked"
        );

        // Direction 2: the unlinked surface reports unlinked WITH a cause.
        let full = derive(
            fx.root.path(),
            &[fx.linked_surface.clone(), fx.unlinked_surface.clone()],
        );
        assert!(full
            .unlinked
            .iter()
            .all(|u| u.surface_id != fx.linked_surface.surface_id));
    }

    #[test]
    fn unlinked_surface_causes_are_never_bare_empty() {
        let fx = build_fixture();
        let report = derive(fx.root.path(), std::slice::from_ref(&fx.unlinked_surface));
        assert_eq!(report.unlinked.len(), 1, "the orphan surface is unlinked");
        let row = &report.unlinked[0];
        assert_eq!(row.surface_id, fx.unlinked_surface.surface_id);
        assert!(
            !row.cause.is_empty() && row.cause != "unknown",
            "cause must be a real attribution: '{}'",
            row.cause
        );
        // Per-cause breakdown covers every unlinked row (AC5).
        let total: usize = report.cause_census.iter().map(|(_, n)| n).sum();
        assert!(
            total >= report.unlinked.len(),
            "census must cover every unlinked row: {report:?}"
        );
    }

    #[test]
    fn symbol_name_match_links_surface_without_import() {
        let root = tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::create_dir_all(root.path().join("tests")).unwrap();
        std::fs::write(
            root.path().join("src/binary_surface.rs"),
            "pub fn main() {}\n",
        )
        .unwrap();
        std::fs::write(
            root.path().join("tests/e2e_test.rs"),
            "// exercises the --verbose flag end to end\n#[test]\nfn e2e() {}\n",
        )
        .unwrap();
        let surface = Surface::new(
            SurfaceKind::CliFlag,
            "src/binary_surface.rs",
            "<hilo>::--verbose",
            "--verbose",
            true,
        );
        let report = derive(root.path(), std::slice::from_ref(&surface));
        assert_eq!(report.links.len(), 1, "name match must link: {report:?}");
        assert_eq!(report.links[0].evidence_kind, EvidenceKind::SymbolNameMatch);
        assert_eq!(report.links[0].confidence, 0.5);
    }

    #[test]
    fn runtime_trace_raises_existing_import_link_without_duplicating() {
        let fx = build_fixture();
        let graph_dir = fx.root.path().join(".vfs").join("graph");
        std::fs::create_dir_all(&graph_dir).unwrap();
        std::fs::write(
            graph_dir.join("runtime_trace.jsonl"),
            format!(
                "{{\"test_file\": \"tests/widget_test.rs\", \"target\": \"{}\"}}\n",
                fx.linked_surface.owner_file
            ),
        )
        .unwrap();
        let report = derive(
            fx.root.path(),
            &[fx.linked_surface.clone(), fx.unlinked_surface.clone()],
        );
        let ids: BTreeSet<&str> = report.links.iter().map(|l| l.link_id.as_str()).collect();
        let import_id = hilo_link_id("tests/widget_test.rs", &fx.linked_surface.surface_id);
        assert!(ids.contains(import_id.as_str()), "same pair, same id");
        assert_eq!(
            report.links.len(),
            1,
            "trace raised the link, not duplicated: {report:?}"
        );
        assert_eq!(report.links[0].evidence_kind, EvidenceKind::RuntimeTrace);
        assert_eq!(report.links[0].confidence, 1.0, "strongest layer wins");
    }

    #[test]
    fn no_test_files_is_a_named_cause_not_silence() {
        let root = tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(root.path().join("src/lib.rs"), "pub fn f() {}\n").unwrap();
        let surface = Surface::new(SurfaceKind::PublicApiItem, "src/lib.rs", "f", "f", true);
        let report = derive(root.path(), &[surface]);
        assert!(report.links.is_empty());
        assert_eq!(report.unlinked.len(), 1);
        assert_eq!(report.unlinked[0].cause, "no test files found in repo");
    }

    #[test]
    fn json_shape_is_locked() {
        let fx = build_fixture();
        let report = derive(
            fx.root.path(),
            &[fx.linked_surface.clone(), fx.unlinked_surface.clone()],
        );
        let value = serde_json::to_value(&report).unwrap();
        let obj = value.as_object().unwrap();
        assert_eq!(obj.get("schema").and_then(|v| v.as_u64()), Some(1));
        for key in ["links", "unlinked", "cause_census", "rules"] {
            assert!(obj.contains_key(key), "report missing '{key}'");
        }
        if let Some(link) = obj["links"].as_array().and_then(|a| a.first()) {
            for key in [
                "link_id",
                "test_file",
                "target",
                "evidence_kind",
                "confidence",
                "direction",
            ] {
                assert!(
                    link.get(key).is_some(),
                    "link row missing '{key}': {link:?}"
                );
            }
        }
        if let Some(row) = obj["unlinked"].as_array().and_then(|a| a.first()) {
            for key in ["surface_id", "kind", "name", "cause"] {
                assert!(
                    row.get(key).is_some(),
                    "unlinked row missing '{key}': {row:?}"
                );
            }
        }
    }

    #[test]
    fn word_boundary_match_is_not_a_substring_match() {
        assert!(source_contains_word("uses --verbose flag", "--verbose"));
        assert!(!source_contains_word(
            "uses --verbose_extra flag",
            "--verbose"
        ));
        assert!(source_contains_word(
            "let get_metadata = 1;",
            "get_metadata"
        ));
        assert!(!source_contains_word(
            "let my_get_metadata = 1;",
            "get_metadata"
        ));
    }

    // Silence the unused-surface_id import when tests run: the fixture
    // derives ids through Surface::new, but the test module imports the
    // function for direct comparisons.
    #[test]
    fn surface_id_helper_is_the_one_links_resolve_against() {
        let s = Surface::new(SurfaceKind::McpTool, "f.rs", "tool", "tool", true);
        assert_eq!(
            s.surface_id,
            surface_id(SurfaceKind::McpTool, "f.rs", "tool")
        );
    }

    /// DF-WARPFS-113: an unknown surface id/name must be a loud error, never a
    /// success-shaped empty report; a KNOWN surface (by id or by name) passes.
    #[test]
    fn unknown_surface_filter_is_a_loud_error_known_surfaces_pass() {
        let s = Surface::new(SurfaceKind::PublicApiItem, "src/lib.rs", "f", "f", true);
        let path = Path::new("/tmp/does-not-matter/surfaces.jsonl");

        ensure_surface_known(std::slice::from_ref(&s), &s.surface_id, path)
            .expect("full surface_id must resolve");
        ensure_surface_known(std::slice::from_ref(&s), "f", path)
            .expect("surface name must resolve");

        let err = ensure_surface_known(std::slice::from_ref(&s), "does-not-exist-xyz", path)
            .expect_err("unknown surface must error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("does-not-exist-xyz") && msg.contains("unknown surface"),
            "error must name the filter and say 'unknown surface': {msg}"
        );
    }
}
