//! Surface grouping + rollup (COV-4).
//!
//! COV-1 enumerates individual contract surfaces and COV-3 gives each one a
//! test-class mix, but the question a human actually asks is coarser than a
//! single function and finer than a whole repo: *is the workspace-mount
//! feature covered?* This module owns the grouping model and the arithmetic
//! that rolls coverage and class mix **up** to that level.
//!
//! ## What a group is (AC1)
//!
//! A surface's group is chosen by one documented precedence chain, first
//! match wins:
//!
//! 1. **explicit annotation** — `user.vfs.feature`, else `user.vfs.component`
//!    (the human's own words for the feature; read by
//!    `hilo_metadata::xattr::group_annotations`).
//! 2. **crate boundary** — the nearest ancestor directory carrying a project
//!    manifest ([`CRATE_MANIFESTS`]: `Cargo.toml`, `go.mod`, `package.json`,
//!    …), so this works on the polyglot corpora the graph walks, not only on
//!    Cargo workspaces.
//! 3. **module path prefix** — the file's module path, with the `src/`
//!    source-root component and the file name dropped
//!    (`hilo-cli/src/commands/graph.rs` → `hilo_cli::commands`).
//!
//! The chosen [`GroupSource`] is recorded on each surface's own row
//! ([`SurfaceGrouping`]) — never re-inferred at read time — so a reader can
//! always tell an annotated group from a structural stand-in.
//!
//! `--by crate` / `--by module` bias the structural fallback; explicit
//! annotations still win, because precedence (1) is unconditional. On a repo
//! with no annotations `--by feature` therefore degrades to the crate
//! boundary, which is the same grouping `--by crate` produces: in an
//! un-annotated tree a crate IS the coarsest structural stand-in for a
//! feature. `--by module` is the genuinely different dimension.
//!
//! ## Never empty (AC3)
//!
//! Structural fallback means an un-annotated repo still groups: the report
//! always carries at least one group, and both the group rows and
//! [`RollupReport::fallback_note`] state **which** fallback produced them.
//!
//! ## Arithmetic (AC4)
//!
//! Every surface is assigned to exactly one group, so
//! `sum(group.surface_count) == report.surfaces.len() == report.total_surfaces`
//! on a whole-repo rollup — no double counting, no dropped surfaces. The
//! invariant survives [`RollupReport::retain_group`] because that restriction
//! filters the group rows and the surface rows together.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::classify::TestClass;
use crate::surfaces::Surface;
use crate::test_classes::SurfaceClassMix;

use hilo_metadata::xattr::GroupAnnotations;

/// Manifest file names that mark a *crate / project* boundary — the second
/// level of the grouping precedence.
///
/// Deliberately polyglot: the graph walks 26 languages, so "crate boundary"
/// must mean "nearest project root" (`Cargo.toml` on Rust, `go.mod` on Go,
/// `package.json` on Node, `pyproject.toml`/`setup.py` on Python, `pom.xml` /
/// `build.gradle*` on JVM, and so on) rather than a Cargo-only concept.
///
/// `gemspec` files are NOT listed: they match by glob, not by fixed name, and
/// a glob cannot be probed with `is_file()`.
pub const CRATE_MANIFESTS: &[&str] = &[
    "Cargo.toml",
    "go.mod",
    "package.json",
    "pyproject.toml",
    "setup.py",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "CMakeLists.txt",
    "Gemfile",
    "composer.json",
    "mix.exs",
    "Package.swift",
    "pubspec.yaml",
];

/// The grouping dimension `hilo graph rollup --by <dim>` selects.
///
/// The flag chooses the **structural** fallback; explicit xattr annotations
/// always take precedence (AC1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupBy {
    /// Annotation-first, then crate boundary, then module path prefix — the
    /// documented precedence chain. The default.
    #[default]
    Feature,
    /// Bias the structural fallback to the crate boundary.
    Crate,
    /// Bias the structural fallback to the module path prefix.
    Module,
}

impl GroupBy {
    /// Every dimension, in precedence order — the census rendering order and
    /// the accepted `--by` vocabulary.
    pub const ALL: [GroupBy; 3] = [GroupBy::Feature, GroupBy::Crate, GroupBy::Module];

    /// The dimension used when `--by` is not given.
    pub const DEFAULT: GroupBy = GroupBy::Feature;

    /// The wire/CLI name.
    pub fn as_str(self) -> &'static str {
        match self {
            GroupBy::Feature => "feature",
            GroupBy::Crate => "crate",
            GroupBy::Module => "module",
        }
    }

    /// Parse a `--by` value. `None` for anything outside the closed
    /// vocabulary, so a typo is a loud usage error rather than a silent
    /// fall-back to the default.
    pub fn parse(s: &str) -> Option<Self> {
        GroupBy::ALL.into_iter().find(|b| b.as_str() == s)
    }
}

impl std::fmt::Display for GroupBy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How one surface's group was chosen — recorded on the surface row (AC1).
///
/// Ordered most-explicit first; [`GroupSource::is_explicit`] separates the
/// human's annotation from the structural stand-in, which is what AC3's
/// "the fallback used" statement counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupSource {
    /// `user.vfs.feature` on the surface's owner file.
    XattrFeature,
    /// `user.vfs.component` on the surface's owner file.
    XattrComponent,
    /// Nearest ancestor carrying a project manifest ([`CRATE_MANIFESTS`]).
    Crate,
    /// The file's module path prefix.
    Module,
}

impl GroupSource {
    /// Every source, most-explicit first — census order.
    pub const ALL: [GroupSource; 4] = [
        GroupSource::XattrFeature,
        GroupSource::XattrComponent,
        GroupSource::Crate,
        GroupSource::Module,
    ];

    /// The wire name used in JSON and the text report.
    pub fn as_str(self) -> &'static str {
        match self {
            GroupSource::XattrFeature => "xattr_feature",
            GroupSource::XattrComponent => "xattr_component",
            GroupSource::Crate => "crate",
            GroupSource::Module => "module",
        }
    }

    /// True when the group came from an explicit `user.vfs.*` annotation —
    /// false when it is a structural fallback (AC3).
    pub fn is_explicit(self) -> bool {
        matches!(
            self,
            GroupSource::XattrFeature | GroupSource::XattrComponent
        )
    }
}

/// One surface's resolved group assignment (AC1: the chosen source is stored
/// on the surface row, not re-derived when the report is read).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceGrouping {
    pub surface_id: String,
    /// The surface kind (`cli_verb`, `mcp_tool`, …).
    pub kind: String,
    pub name: String,
    /// The group this surface belongs to.
    pub group: String,
    /// Which precedence level chose `group`.
    pub source: GroupSource,
}

/// One bucket of a [`RollupReport`]'s census: how many surfaces a given
/// [`GroupSource`] (or a given group) accounts for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCount {
    pub source: GroupSource,
    pub count: usize,
}

/// How many surfaces of a group are reached by a given test class — one row
/// per [`TestClass::ALL`], always all eight, so a class with zero reach
/// reports `0` and still names itself rather than vanishing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassCount {
    pub class: TestClass,
    /// Surfaces in the group reached by at least one test of this class.
    pub surfaces: usize,
}

/// One rolled-up group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupRollup {
    pub group: String,
    /// Member surfaces (AC2).
    pub surface_count: usize,
    /// Members with at least one evidenced coverage link.
    pub covered: usize,
    /// Members with no coverage link at all — the gap list's first half.
    pub uncovered: usize,
    /// The class mix (AC2): per test class, how many members it reaches.
    /// Always one row per [`TestClass::ALL`].
    pub class_mix: Vec<ClassCount>,
    /// The classes with a non-zero reach, in [`TestClass::ALL`] order.
    pub classes_present: Vec<TestClass>,
    /// Members reached by exactly ONE class — COV-3's `class_gap` shape,
    /// rolled up. The gap list's second half.
    pub class_gap_count: usize,
    pub class_gap_surfaces: Vec<String>,
    pub class_gap_names: Vec<String>,
    pub uncovered_surfaces: Vec<String>,
    pub uncovered_names: Vec<String>,
    /// Which [`GroupSource`]s produced this group's membership (a group can
    /// mix an annotated majority with structural joiners).
    pub sources: Vec<SourceCount>,
}

/// The full `hilo graph rollup` report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RollupReport {
    pub schema: u32,
    pub by: GroupBy,
    /// Groups in this report (every group of the whole-repo rollup, or the
    /// single group `--group` was given).
    pub groups: Vec<GroupRollup>,
    /// Every grouped surface, sorted by (group, kind, name, surface_id).
    pub surfaces: Vec<SurfaceGrouping>,
    /// Surfaces in the WHOLE rollup. `sum(group.surface_count)` equals
    /// `surfaces.len()` (and `total_surfaces` on an unfiltered report).
    pub total_surfaces: usize,
    /// How many groups the whole rollup produced (even when `--group`
    /// restricted `groups` to one).
    pub group_count: usize,
    /// The `--group` restriction this report was narrowed to, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_filter: Option<String>,
    /// Per-source census over every surface (AC3: explicit vs fallback).
    pub source_census: Vec<SourceCount>,
    /// The plain-language statement of which fallback produced the grouping
    /// (AC3) — present even when no fallback was needed.
    pub fallback_note: String,
    /// Where the coverage/class-mix numbers came from, or why they are
    /// absent. Never a bare empty.
    pub coverage_evidence: String,
    /// The rules the derivation ran under (the gap-signal twin of
    /// `surfaces`' `KindCensus.rule`).
    pub rules: Vec<String>,
}

impl RollupReport {
    /// The current schema version written to `--json` output.
    pub const SCHEMA: u32 = 1;

    /// Restrict the report to one group (AC5), keeping the arithmetic
    /// internally consistent: the group rows and the surface rows are
    /// narrowed TOGETHER, so `sum(group.surface_count) == surfaces.len()`
    /// still holds afterwards. `total_surfaces` keeps describing the whole
    /// rollup.
    ///
    /// Returns `false` when no group has that name — the caller turns that
    /// into a loud error, never a success-shaped empty report.
    pub fn retain_group(&mut self, name: &str) -> bool {
        if !self.groups.iter().any(|g| g.group == name) {
            return false;
        }
        self.groups.retain(|g| g.group == name);
        self.surfaces.retain(|s| s.group == name);
        self.group_filter = Some(name.to_string());
        true
    }
}

/// The nearest ancestor of `owner_file` (including its own directory) that
/// carries a project manifest, expressed repo-relative.
///
/// `None` when no ancestor carries a manifest — the caller then falls through
/// to the module path prefix, so the report is never empty.
pub fn crate_of(root: &Path, owner_file: &str) -> Option<String> {
    let path = Path::new(owner_file);
    let mut dir = path.parent();
    while let Some(d) = dir {
        let abs = if d.as_os_str().is_empty() {
            root.to_path_buf()
        } else {
            root.join(d)
        };
        if CRATE_MANIFESTS.iter().any(|m| abs.join(m).is_file()) {
            return Some(if d.as_os_str().is_empty() {
                root_label(root)
            } else {
                d.to_string_lossy().replace('\\', "/")
            });
        }
        if d.as_os_str().is_empty() {
            break;
        }
        dir = d.parent();
    }
    None
}

/// The label for a repo whose crate boundary IS the root (a single-package
/// repo): the root directory's own name, or `<root>` when it has none.
fn root_label(root: &Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "<root>".to_string())
}

/// The module path prefix of `owner_file`: the file's directories with the
/// `src/` source-root component dropped, joined by `::`, hyphen-free.
///
/// The FIRST `src` component is the crate's source root, so everything after
/// it is the module path *within* the crate — dropping it (and the file name)
/// is what makes `hilo-cli/src/commands/graph.rs` group with the rest of
/// `hilo-cli`'s command module rather than with a phantom `hilo-cli::src`
/// prefix. A path with no `src` component keeps its whole directory chain.
///
/// - `hilo-cli/src/commands/graph.rs` → `hilo_cli::commands`
/// - `hilo-graph/src/lib.rs` → `hilo_graph` (the crate root module)
/// - `src/widget.rs` → `widget` (a top-level module of a single-crate repo)
/// - `pkg/lib/store.go` → `pkg::lib` (no `src` component to drop)
///
/// Always non-empty, so it is a total fallback.
pub fn module_of(owner_file: &str) -> String {
    let parts: Vec<&str> = owner_file.split('/').filter(|p| !p.is_empty()).collect();
    let Some((file, dirs)) = parts.split_last() else {
        return "<root>".to_string();
    };
    let mut comps: Vec<&str> = dirs.to_vec();
    if let Some(pos) = comps.iter().position(|c| *c == "src") {
        comps.remove(pos);
    }
    if comps.is_empty() {
        // No directory left after dropping the source root: the module IS
        // the file.
        let stem = file.rsplit_once('.').map(|(s, _)| s).unwrap_or(file);
        return normalize_module(stem);
    }
    comps
        .iter()
        .copied()
        .map(normalize_module)
        .collect::<Vec<_>>()
        .join("::")
}

/// Rust module path components cannot carry a hyphen, so a crate directory
/// name (`hilo-cli`) renders as the module it actually is (`hilo_cli`).
fn normalize_module(component: &str) -> String {
    component.replace('-', "_")
}

/// Resolve one surface's group and the precedence level that chose it (AC1).
pub fn resolve_group(
    root: &Path,
    owner_file: &str,
    annotations: &GroupAnnotations,
    by: GroupBy,
) -> (String, GroupSource) {
    // (1) explicit annotation — unconditional, in every `--by` mode.
    if let Some(feature) = non_empty(annotations.feature.as_deref()) {
        return (feature.to_string(), GroupSource::XattrFeature);
    }
    if let Some(component) = non_empty(annotations.component.as_deref()) {
        return (component.to_string(), GroupSource::XattrComponent);
    }
    // (2)/(3) structural fallback, biased by `--by`.
    let crate_group = crate_of(root, owner_file);
    match by {
        GroupBy::Module => (module_of(owner_file), GroupSource::Module),
        GroupBy::Feature | GroupBy::Crate => match crate_group {
            Some(crate_group) => (crate_group, GroupSource::Crate),
            None => (module_of(owner_file), GroupSource::Module),
        },
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

/// Accumulator for one group while the surface rows are walked.
#[derive(Debug, Default)]
struct GroupAccumulator {
    surface_count: usize,
    covered: usize,
    /// class → how many members this class reaches.
    class_hits: BTreeMap<TestClass, usize>,
    class_gap: Vec<(String, String)>,
    uncovered: Vec<(String, String)>,
    sources: BTreeMap<GroupSource, usize>,
}

/// Roll surfaces up into groups (AC2) — the whole COV-4 model.
///
/// * `root` anchors the crate-boundary lookup (a repo-relative `owner_file`
///   is resolved against it).
/// * `class_mix` is COV-3's per-surface class mix
///   ([`crate::test_classes::SurfaceClassMix`]); the join key is
///   `surface_id`. A surface with no row (or an empty `classes`) is
///   **uncovered**, which is exactly COV-3's own definition — the two
///   commands can never disagree about what "covered" means.
/// * `annotations` maps a repo-relative owner file to its explicit
///   `user.vfs.*` grouping. Injected rather than read here so the derivation
///   is testable without filesystem xattr support; the CLI passes a closure
///   over `hilo_metadata::xattr::group_annotations`.
/// * `coverage_evidence` states where the coverage numbers came from (or why
///   they are absent) and is copied onto the report verbatim.
pub fn rollup(
    root: &Path,
    surfaces: &[Surface],
    class_mix: &[SurfaceClassMix],
    annotations: &dyn Fn(&str) -> GroupAnnotations,
    by: GroupBy,
    coverage_evidence: &str,
) -> RollupReport {
    let mix_by_id: BTreeMap<&str, &SurfaceClassMix> = class_mix
        .iter()
        .map(|m| (m.surface_id.as_str(), m))
        .collect();

    let mut rows: Vec<SurfaceGrouping> = Vec::with_capacity(surfaces.len());
    let mut by_group: BTreeMap<String, GroupAccumulator> = BTreeMap::new();
    let mut source_census: BTreeMap<GroupSource, usize> = BTreeMap::new();

    for surface in surfaces {
        let ann = annotations(&surface.owner_file);
        let (group, source) = resolve_group(root, &surface.owner_file, &ann, by);
        *source_census.entry(source).or_default() += 1;

        let mix = mix_by_id.get(surface.surface_id.as_str()).copied();
        let classes: &[TestClass] = mix.map(|m| m.classes.as_slice()).unwrap_or(&[]);
        let covered = !classes.is_empty();

        let acc = by_group.entry(group.clone()).or_default();
        acc.surface_count += 1;
        *acc.sources.entry(source).or_default() += 1;
        if covered {
            acc.covered += 1;
        } else {
            acc.uncovered
                .push((surface.surface_id.clone(), surface.name.clone()));
        }
        for class in TestClass::ALL {
            if classes.contains(&class) {
                *acc.class_hits.entry(class).or_default() += 1;
            }
        }
        if mix.is_some_and(|m| m.class_gap) {
            acc.class_gap
                .push((surface.surface_id.clone(), surface.name.clone()));
        }

        rows.push(SurfaceGrouping {
            surface_id: surface.surface_id.clone(),
            kind: surface.kind.as_str().to_string(),
            name: surface.name.clone(),
            group,
            source,
        });
    }

    // Deterministic: grouped by name, then kind, then display name, then id.
    rows.sort_by(|a, b| {
        (&a.group, &a.kind, &a.name, &a.surface_id).cmp(&(
            &b.group,
            &b.kind,
            &b.name,
            &b.surface_id,
        ))
    });

    let groups: Vec<GroupRollup> = by_group
        .into_iter()
        .map(|(group, acc)| {
            // Gap lists are sorted by (name, surface_id) so the answer for a
            // group does not depend on the order the inventory happened to
            // list its surfaces in.
            let mut uncovered = acc.uncovered;
            uncovered.sort_by(|a, b| (&a.1, &a.0).cmp(&(&b.1, &b.0)));
            let mut class_gap = acc.class_gap;
            class_gap.sort_by(|a, b| (&a.1, &a.0).cmp(&(&b.1, &b.0)));
            GroupRollup {
                group,
                surface_count: acc.surface_count,
                covered: acc.covered,
                uncovered: uncovered.len(),
                class_mix: TestClass::ALL
                    .iter()
                    .map(|&class| ClassCount {
                        class,
                        surfaces: acc.class_hits.get(&class).copied().unwrap_or(0),
                    })
                    .collect(),
                classes_present: TestClass::ALL
                    .iter()
                    .copied()
                    .filter(|c| acc.class_hits.get(c).copied().unwrap_or(0) > 0)
                    .collect(),
                class_gap_count: class_gap.len(),
                class_gap_surfaces: class_gap.iter().map(|(id, _)| id.clone()).collect(),
                class_gap_names: class_gap.iter().map(|(_, n)| n.clone()).collect(),
                uncovered_surfaces: uncovered.iter().map(|(id, _)| id.clone()).collect(),
                uncovered_names: uncovered.iter().map(|(_, n)| n.clone()).collect(),
                sources: source_count_rows(&acc.sources),
            }
        })
        .collect();

    let source_census = source_count_rows(&source_census);
    let fallback_note = fallback_note(&source_census, surfaces.len());

    RollupReport {
        schema: RollupReport::SCHEMA,
        by,
        group_count: groups.len(),
        total_surfaces: surfaces.len(),
        groups,
        surfaces: rows,
        group_filter: None,
        source_census,
        fallback_note,
        coverage_evidence: coverage_evidence.to_string(),
        rules: rule_lines(by, surfaces.len()),
    }
}

/// One `SourceCount` row per [`GroupSource`], in `ALL` order, zeroes kept so
/// a source that contributed nothing still names itself.
fn source_count_rows(counts: &BTreeMap<GroupSource, usize>) -> Vec<SourceCount> {
    GroupSource::ALL
        .iter()
        .map(|&source| SourceCount {
            source,
            count: counts.get(&source).copied().unwrap_or(0),
        })
        .collect()
}

/// The AC3 statement: which fallback (if any) produced this report's grouping.
fn fallback_note(census: &[SourceCount], total: usize) -> String {
    let explicit: usize = census
        .iter()
        .filter(|row| row.source.is_explicit())
        .map(|row| row.count)
        .sum();
    let structural = total - explicit;
    if total == 0 {
        return "no surfaces to group".to_string();
    }
    if structural == 0 {
        return format!(
            "every one of the {total} surface(s) carries an explicit xattr grouping \
             (user.vfs.feature|component) — no structural fallback was used"
        );
    }
    let breakdown = census
        .iter()
        .filter(|row| !row.source.is_explicit() && row.count > 0)
        .map(|row| format!("{} ({})", row.source.as_str(), row.count))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "structural fallback used for {structural} of {total} surface(s) — {breakdown}; \
         {explicit} surface(s) grouped by explicit xattr"
    )
}

/// The derivation rules, including the explicit omission of the GAP-113
/// wiring rollup (AC2: roll it up *if GAP-113 landed; otherwise omit* — the
/// omission carries its reason rather than a silent absent field).
fn rule_lines(by: GroupBy, surfaces: usize) -> Vec<String> {
    vec![
        "group rule (precedence, first match wins): (1) explicit xattr user.vfs.feature, \
         (2) explicit xattr user.vfs.component, (3) structural fallback — crate boundary \
         (nearest ancestor carrying a project manifest: Cargo.toml, go.mod, package.json, \
         pyproject.toml, …), then module path prefix (the file's module path, with the file \
         name and the `src/` source root dropped)"
            .to_string(),
        format!(
            "grouping dimension: --by {by}; explicit xattr annotations take precedence in every \
             mode, and {surfaces} surface(s) were grouped"
        ),
        "coverage: a group member is COVERED when COV-3's per-surface class mix carries at least \
         one class for it (i.e. at least one evidenced coverage link reaches it), and uncovered \
         otherwise — the same definition `hilo graph test-classes` reports"
            .to_string(),
        "wiring rollup: omitted — GAP-113 (the aggregate wiring report: per-module public \
         surfaces, inbound/outbound connection counts, baseline deltas, untested surfaces) has \
         not landed, so there are no wiring numbers to roll up. It is not reported as zero."
            .to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::TestClass;
    use crate::surfaces::{Surface, SurfaceKind};
    use crate::test_classes::SurfaceClassMix;
    use std::path::PathBuf;

    /// A fixture workspace root: `crate-a/` and `crate-b/` each carry a
    /// manifest; `plain/` carries none.
    fn fixture_root() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for crate_dir in ["crate-a", "crate-b"] {
            let d = dir.path().join(crate_dir);
            std::fs::create_dir_all(d.join("src/sub")).unwrap();
            std::fs::write(d.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        }
        std::fs::create_dir_all(dir.path().join("plain/src")).unwrap();
        dir
    }

    fn no_annotations(_: &str) -> GroupAnnotations {
        GroupAnnotations::default()
    }

    fn surface(kind: SurfaceKind, file: &str, name: &str) -> Surface {
        Surface::new(kind, file, name, name, true)
    }

    fn mix(surface_id: &str, classes: &[TestClass], class_gap: bool) -> SurfaceClassMix {
        SurfaceClassMix {
            surface_id: surface_id.to_string(),
            kind: "mcp_tool".to_string(),
            name: "n".to_string(),
            classes: classes.to_vec(),
            class_gap,
            missing_classes: Vec::new(),
            test_files: classes.len(),
        }
    }

    #[test]
    fn crate_boundary_is_the_nearest_manifest_ancestor() {
        let root = fixture_root();
        assert_eq!(
            crate_of(root.path(), "crate-a/src/sub/deep.rs").as_deref(),
            Some("crate-a")
        );
        assert_eq!(
            crate_of(root.path(), "crate-b/src/lib.rs").as_deref(),
            Some("crate-b")
        );
        // No manifest anywhere above `plain/` → the module fallback must run.
        assert_eq!(crate_of(root.path(), "plain/src/x.rs"), None);
    }

    #[test]
    fn crate_boundary_generalizes_across_language_manifests() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("svc/src")).unwrap();
        std::fs::write(dir.path().join("svc/go.mod"), "module svc\n").unwrap();
        assert_eq!(
            crate_of(dir.path(), "svc/src/main.go").as_deref(),
            Some("svc")
        );
    }

    #[test]
    fn module_prefix_drops_file_name_and_trailing_src() {
        assert_eq!(
            module_of("hilo-cli/src/commands/graph.rs"),
            "hilo_cli::commands"
        );
        assert_eq!(module_of("hilo-graph/src/lib.rs"), "hilo_graph");
        assert_eq!(module_of("hilo-mcp/src/tools/mod.rs"), "hilo_mcp::tools");
        // A single-crate repo: a top-level src file is its own module.
        assert_eq!(module_of("src/widget.rs"), "widget");
        // No `src` component anywhere: the whole directory chain is kept.
        assert_eq!(module_of("pkg/lib/store.go"), "pkg::lib");
        assert_eq!(module_of(""), "<root>");
    }

    #[test]
    fn precedence_is_xattr_then_crate_then_module() {
        let root = fixture_root();

        // (1) feature annotation beats the crate boundary.
        let ann = GroupAnnotations {
            feature: Some("workspace-mount".into()),
            component: Some("fuse".into()),
        };
        let (group, source) =
            resolve_group(root.path(), "crate-a/src/lib.rs", &ann, GroupBy::Crate);
        assert_eq!(
            (group.as_str(), source),
            ("workspace-mount", GroupSource::XattrFeature)
        );

        // (1b) component is consulted only without a feature.
        let ann = GroupAnnotations {
            feature: None,
            component: Some("backends".into()),
        };
        let (group, source) =
            resolve_group(root.path(), "crate-a/src/lib.rs", &ann, GroupBy::Crate);
        assert_eq!(
            (group.as_str(), source),
            ("backends", GroupSource::XattrComponent)
        );

        // A blank annotation is not an annotation.
        let ann = GroupAnnotations {
            feature: Some("   ".into()),
            component: None,
        };
        let (group, source) =
            resolve_group(root.path(), "crate-a/src/lib.rs", &ann, GroupBy::Crate);
        assert_eq!((group.as_str(), source), ("crate-a", GroupSource::Crate));

        // (2) crate boundary when nothing is annotated.
        let (group, source) = resolve_group(
            root.path(),
            "crate-a/src/sub/x.rs",
            &no_annotations(""),
            GroupBy::Feature,
        );
        assert_eq!((group.as_str(), source), ("crate-a", GroupSource::Crate));

        // (3) module prefix when no manifest ancestor exists at all.
        let (group, source) = resolve_group(
            root.path(),
            "plain/src/deep.rs",
            &no_annotations(""),
            GroupBy::Feature,
        );
        assert_eq!((group.as_str(), source), ("plain", GroupSource::Module));

        // --by module uses the module dimension for a file that HAS a crate.
        let (group, source) = resolve_group(
            root.path(),
            "crate-a/src/sub/x.rs",
            &no_annotations(""),
            GroupBy::Module,
        );
        assert_eq!(
            (group.as_str(), source),
            ("crate_a::sub", GroupSource::Module)
        );

        // ...but an explicit annotation still wins in module mode.
        let ann = GroupAnnotations {
            feature: Some("workspace-mount".into()),
            component: None,
        };
        let (group, source) =
            resolve_group(root.path(), "crate-a/src/sub/x.rs", &ann, GroupBy::Module);
        assert_eq!(
            (group.as_str(), source),
            ("workspace-mount", GroupSource::XattrFeature)
        );
    }

    #[test]
    fn by_parse_round_trips_and_rejects_unknown() {
        for by in GroupBy::ALL {
            assert_eq!(GroupBy::parse(by.as_str()), Some(by));
        }
        assert_eq!(GroupBy::parse("crates"), None);
        assert_eq!(GroupBy::parse(""), None);
        assert_eq!(GroupBy::DEFAULT, GroupBy::Feature);
    }

    #[test]
    fn group_source_explicitness_partitions_the_sources() {
        assert!(GroupSource::XattrFeature.is_explicit());
        assert!(GroupSource::XattrComponent.is_explicit());
        assert!(!GroupSource::Crate.is_explicit());
        assert!(!GroupSource::Module.is_explicit());
    }

    /// AC4 / AC3: the rollup arithmetic closes and the fallback is stated on
    /// an entirely un-annotated repo (so the report is never empty).
    #[test]
    fn unannotated_repo_still_groups_and_the_arithmetic_closes() {
        let root = fixture_root();
        let surfaces = vec![
            surface(SurfaceKind::McpTool, "crate-a/src/lib.rs", "a_lib"),
            surface(SurfaceKind::McpTool, "crate-a/src/sub/deep.rs", "a_deep"),
            surface(SurfaceKind::CliVerb, "crate-b/src/cli.rs", "b_cli"),
            surface(SurfaceKind::CliVerb, "plain/src/x.rs", "plain_x"),
        ];
        let mixes = vec![
            mix(&surfaces[0].surface_id, &[TestClass::Unit], true),
            mix(
                &surfaces[2].surface_id,
                &[TestClass::Unit, TestClass::Integration],
                false,
            ),
        ];

        let report = rollup(
            root.path(),
            &surfaces,
            &mixes,
            &no_annotations,
            GroupBy::Feature,
            "coverage_links.jsonl (2 link rows)",
        );

        // Never empty: 3 groups (crate-a, crate-b, plain).
        assert_eq!(report.group_count, 3);
        assert!(!report.groups.is_empty());
        assert_eq!(report.total_surfaces, 4);

        // AC4: the arithmetic closes — no double count, no dropped surface.
        let summed: usize = report.groups.iter().map(|g| g.surface_count).sum();
        assert_eq!(summed, report.total_surfaces);
        assert_eq!(summed, report.surfaces.len());
        assert_eq!(
            report
                .surfaces
                .iter()
                .map(|s| s.group.as_str())
                .collect::<Vec<_>>(),
            vec!["crate-a", "crate-a", "crate-b", "plain"],
        );

        // AC3: the fallback is stated, and no surface was annotated.
        assert!(
            report
                .fallback_note
                .contains("structural fallback used for 4 of 4"),
            "{}",
            report.fallback_note
        );
        assert!(report.fallback_note.contains("crate"));
        let module_row = report
            .source_census
            .iter()
            .find(|r| r.source == GroupSource::Module)
            .unwrap();
        assert_eq!(
            module_row.count, 1,
            "only `plain/` needed the module fallback"
        );

        // Coverage rolls up per group.
        let crate_a = report.groups.iter().find(|g| g.group == "crate-a").unwrap();
        assert_eq!(
            (crate_a.surface_count, crate_a.covered, crate_a.uncovered),
            (2, 1, 1)
        );
        assert_eq!(crate_a.class_mix.len(), TestClass::ALL.len());
        assert_eq!(crate_a.classes_present, vec![TestClass::Unit]);
        assert_eq!(crate_a.class_gap_count, 1);
        assert_eq!(crate_a.class_gap_names, vec!["a_lib"]);
        assert_eq!(crate_a.uncovered_names, vec!["a_deep"]);

        let crate_b = report.groups.iter().find(|g| g.group == "crate-b").unwrap();
        assert_eq!(
            (crate_b.surface_count, crate_b.covered, crate_b.uncovered),
            (1, 1, 0)
        );
        assert_eq!(
            crate_b.classes_present,
            vec![TestClass::Unit, TestClass::Integration]
        );
        assert_eq!(crate_b.class_gap_count, 0);
    }

    /// A group can mix annotated and structural members, and the census says so.
    #[test]
    fn annotated_and_structural_members_are_both_recorded_per_surface() {
        let root = fixture_root();
        let surfaces = vec![
            surface(SurfaceKind::McpTool, "crate-a/src/lib.rs", "a_lib"),
            surface(SurfaceKind::McpTool, "crate-b/src/lib.rs", "b_lib"),
        ];
        let annotations = |file: &str| {
            if file.starts_with("crate-a") {
                GroupAnnotations {
                    feature: Some("workspace-mount".into()),
                    component: None,
                }
            } else {
                GroupAnnotations::default()
            }
        };
        let report = rollup(
            root.path(),
            &surfaces,
            &[],
            &annotations,
            GroupBy::Feature,
            "absent: no coverage_links.jsonl",
        );

        let a = report.surfaces.iter().find(|s| s.name == "a_lib").unwrap();
        assert_eq!(
            (a.group.as_str(), a.source),
            ("workspace-mount", GroupSource::XattrFeature)
        );
        let b = report.surfaces.iter().find(|s| s.name == "b_lib").unwrap();
        assert_eq!(
            (b.group.as_str(), b.source),
            ("crate-b", GroupSource::Crate)
        );

        assert!(report
            .fallback_note
            .contains("structural fallback used for 1 of 2"));
        // GAP-113 has not landed: the wiring rollup is omitted, with the
        // reason stated rather than a fabricated zero.
        assert!(report
            .rules
            .iter()
            .any(|r| r.contains("wiring rollup: omitted") && r.contains("GAP-113")));
    }

    /// AC5: naming a group answers with its own gap list, and the arithmetic
    /// stays closed after the restriction.
    #[test]
    fn retain_group_answers_with_gap_list_and_keeps_arithmetic_closed() {
        let root = fixture_root();
        let surfaces = vec![
            surface(SurfaceKind::McpTool, "crate-a/src/lib.rs", "a_lib"),
            surface(SurfaceKind::McpTool, "crate-a/src/sub/deep.rs", "a_deep"),
            surface(SurfaceKind::CliVerb, "crate-b/src/cli.rs", "b_cli"),
        ];
        let mixes = vec![mix(&surfaces[0].surface_id, &[TestClass::Unit], true)];
        let mut report = rollup(
            root.path(),
            &surfaces,
            &mixes,
            &no_annotations,
            GroupBy::Feature,
            "coverage_links.jsonl (1 link row)",
        );

        assert!(report.retain_group("crate-a"));
        assert_eq!(report.group_filter.as_deref(), Some("crate-a"));
        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.surfaces.len(), 2);
        assert_eq!(report.total_surfaces, 3, "the whole-rollup total is kept");
        assert_eq!(
            report.group_count, 2,
            "the whole-rollup group count is kept"
        );

        let g = &report.groups[0];
        assert_eq!(g.uncovered_names, vec!["a_deep"]);
        assert_eq!(g.class_gap_names, vec!["a_lib"]);
        let summed: usize = report.groups.iter().map(|g| g.surface_count).sum();
        assert_eq!(
            summed,
            report.surfaces.len(),
            "restriction keeps the invariant"
        );

        // An unknown group is a miss, never a success-shaped empty report.
        assert!(!report.retain_group("nope"));
        assert_eq!(report.groups.len(), 1, "a miss changes nothing");
    }

    #[test]
    fn report_json_shape_is_locked() {
        let root = fixture_root();
        let surfaces = vec![surface(SurfaceKind::McpTool, "crate-a/src/lib.rs", "a_lib")];
        let mixes = vec![mix(&surfaces[0].surface_id, &[TestClass::Unit], true)];
        let report = rollup(
            root.path(),
            &surfaces,
            &mixes,
            &no_annotations,
            GroupBy::Crate,
            "coverage_links.jsonl (1 link row)",
        );
        let value = serde_json::to_value(&report).unwrap();
        let obj = value.as_object().unwrap();
        assert_eq!(obj.get("schema").and_then(|v| v.as_u64()), Some(1));
        assert_eq!(obj.get("by").and_then(|v| v.as_str()), Some("crate"));
        for key in [
            "groups",
            "surfaces",
            "total_surfaces",
            "group_count",
            "source_census",
            "fallback_note",
            "coverage_evidence",
            "rules",
        ] {
            assert!(obj.contains_key(key), "report missing '{key}': {obj:?}");
        }
        // `--group` is the only optional key: absent until a restriction runs.
        assert!(!obj.contains_key("group_filter"));
        // GAP-113 has not landed, so no wiring section is emitted at all.
        assert!(!obj.contains_key("wiring"));

        let row = obj["surfaces"][0].as_object().unwrap();
        for key in ["surface_id", "kind", "name", "group", "source"] {
            assert!(row.contains_key(key), "surface row missing '{key}'");
        }
        assert_eq!(row.get("source").and_then(|v| v.as_str()), Some("crate"));

        let group = obj["groups"][0].as_object().unwrap();
        for key in [
            "group",
            "surface_count",
            "covered",
            "uncovered",
            "class_mix",
            "classes_present",
            "class_gap_count",
            "class_gap_surfaces",
            "class_gap_names",
            "uncovered_surfaces",
            "uncovered_names",
            "sources",
        ] {
            assert!(group.contains_key(key), "group row missing '{key}'");
        }
        assert_eq!(
            group["class_mix"].as_array().unwrap().len(),
            TestClass::ALL.len(),
            "the class mix names every class, zeroes included"
        );
        assert_eq!(group["class_mix"][0]["class"].as_str(), Some("unit"));
        // The source census is a report-level array of named rows, and the
        // per-surface row is where the chosen source is recorded (AC1).
        assert!(obj["source_census"].is_array());
        assert_eq!(
            obj["source_census"].as_array().unwrap().len(),
            GroupSource::ALL.len()
        );
    }

    #[test]
    fn surfaces_without_a_class_mix_row_are_uncovered() {
        let root = fixture_root();
        let surfaces = vec![surface(SurfaceKind::McpTool, "crate-a/src/lib.rs", "a_lib")];
        // No class-mix rows at all (COV-3 not derived): the report must say
        // uncovered rather than invent coverage.
        let report = rollup(
            root.path(),
            &surfaces,
            &[],
            &no_annotations,
            GroupBy::Feature,
            "absent: no coverage_links.jsonl — run `hilo graph coverage-links` (COV-2)",
        );
        assert_eq!(report.groups[0].uncovered, 1);
        assert_eq!(report.groups[0].covered, 0);
        assert!(report.groups[0].classes_present.is_empty());
        assert!(report.coverage_evidence.contains("absent"));
    }

    #[test]
    fn empty_inventory_is_an_explicit_zero_not_a_panic() {
        let root = PathBuf::from("/nonexistent");
        let report = rollup(
            &root,
            &[],
            &[],
            &no_annotations,
            GroupBy::Feature,
            "absent: no coverage_links.jsonl",
        );
        assert_eq!(report.total_surfaces, 0);
        assert_eq!(report.group_count, 0);
        assert!(report.groups.is_empty());
        assert_eq!(report.fallback_note, "no surfaces to group");
        assert_eq!(
            report.source_census.iter().map(|r| r.count).sum::<usize>(),
            0
        );
    }
}
