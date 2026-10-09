//! GAP-113: the aggregate **wiring report** — per-module public surfaces,
//! inbound/outbound connection counts, deltas against a stored baseline, and
//! the untested surfaces underneath each module.
//!
//! This is the machine-readable report the external scorer (task-router
//! TR-282) reads. It is deliberately a *pure* layer: every function here
//! takes already-collected facts (the surface inventory from
//! `.vfs/graph/surfaces.jsonl`, the edge set from `.vfs/graph/edges.jsonl`,
//! the coverage links from `.vfs/graph/coverage_links.jsonl`, and the
//! per-module file classifications) and returns a serializable report. The
//! CLI does the discovery/IO; this module owns the model, the identity, the
//! delta arithmetic and the versioned JSON shape.
//!
//! ## What a "module" is
//!
//! A module is the **first path component** of a repo-relative file path —
//! `hilo-graph/src/lib.rs` → `hilo-graph`, `src/main.go` → `src`. Files that
//! live directly at the repository root (no `/`) belong to the synthetic
//! `(root)` module. This makes the report work unchanged on a Rust workspace
//! (crates are top-level directories) and on any other layout.
//!
//! ## What a "public surface" is
//!
//! A row of the COV-1 surface inventory ([`crate::surfaces::Surface`]): a
//! code-derived, externally-visible contract point (`cli_verb`, `mcp_tool`,
//! `public_api_item`, …). Surfaces key on the content-derived
//! [`crate::surfaces::surface_id`], so a delta moves only when a surface's
//! *identity* moves — never because unrelated code shifted.
//!
//! ## Module classification (precedence, first match wins)
//!
//! 1. `test` — every source file in the module is a test file.
//! 2. `entrypoint` — the module declares at least one `entrypoint` file
//!    (`classify`'s role for a `main`/`fn main`/`__main__`/`Program.cs`-shaped
//!    file).
//! 3. `service` — a non-test, non-entrypoint module that at least one OTHER
//!    module imports (inbound edges > 0): it provides an API others consume.
//! 4. `lib` — a non-test, non-entrypoint module nothing else imports yet, but
//!    which still exports surfaces or imports something itself (a leaf
//!    provider).
//! 5. `dead` — a non-test, non-entrypoint module with no inbound edges, no
//!    outbound edges and no public surfaces: nothing links it to the rest of
//!    the tree.
//!
//! The five classes are exhaustive and deterministic — every module lands in
//! exactly one, and the rule that produced it is documented here rather than
//! inferred at read time.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::coverage_links::CoverageLink;
use crate::surfaces::Surface;
use hilo_metadata::inventory::Edge;

/// The versioned id of the report shape. Bumped only on a breaking shape
/// change; the external scorer keys on it.
pub const WIRING_REPORT_VERSION: &str = "hilo.graph.wiring/2";

/// The versioned id of the baseline file shape.
pub const WIRING_BASELINE_VERSION: u32 = 1;

/// The module that owns files sitting directly at the repository root.
pub const ROOT_MODULE: &str = "(root)";

/// The five module classes, in canonical render order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleClass {
    /// An executable entry lives here (`main`, `fn main`, `__main__`, …).
    Entrypoint,
    /// A production module other modules import (inbound edges > 0).
    Service,
    /// A production module nothing imports yet (leaf provider).
    Lib,
    /// Every source file here is a test file.
    Test,
    /// Production, but linked to nothing: no edges in, none out, no surfaces.
    Dead,
}

impl ModuleClass {
    /// Every class in canonical render order.
    pub const ALL: [ModuleClass; 5] = [
        ModuleClass::Entrypoint,
        ModuleClass::Service,
        ModuleClass::Lib,
        ModuleClass::Test,
        ModuleClass::Dead,
    ];

    /// The snake_case wire form used in JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            ModuleClass::Entrypoint => "entrypoint",
            ModuleClass::Service => "service",
            ModuleClass::Lib => "lib",
            ModuleClass::Test => "test",
            ModuleClass::Dead => "dead",
        }
    }

    /// Parse a snake_case class name — the inverse of [`ModuleClass::as_str`].
    /// Returns `None` outside the closed vocabulary.
    pub fn parse(s: &str) -> Option<Self> {
        ModuleClass::ALL.into_iter().find(|c| c.as_str() == s)
    }
}

impl std::fmt::Display for ModuleClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A reference to one public surface — the identity plus the minimum a human
/// (or the scorer) needs to name it. `owner_file` travels with the row so a
/// coverage link keyed on the *file* can still mark the surface as tested.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SurfaceRef {
    /// Content-derived identity ([`crate::surfaces::surface_id`]).
    pub surface_id: String,
    /// The externally-visible name of the surface.
    pub name: String,
    /// The repo-relative file that declares/owns the surface.
    pub owner_file: String,
}

impl SurfaceRef {
    /// Build a reference from an inventory row.
    pub fn from_surface(s: &Surface) -> Self {
        SurfaceRef {
            surface_id: s.surface_id.clone(),
            name: s.name.clone(),
            owner_file: s.owner_file.clone(),
        }
    }
}

/// One module's snapshot: the surfaces it exposes and the identities of the
/// edges that touch it. This is exactly what a baseline stores, so a delta is
/// a set difference over two snapshots — nothing else.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleSnapshot {
    /// Surfaces owned by this module, sorted by `surface_id`.
    pub surfaces: Vec<SurfaceRef>,
    /// Identities (`from|rel|to`) of edges touching this module, sorted.
    pub edges: Vec<String>,
}

/// A stored baseline: one snapshot per module, keyed by module name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WiringBaseline {
    pub version: u32,
    pub modules: BTreeMap<String, ModuleSnapshot>,
}

impl WiringBaseline {
    /// Build a baseline from a live snapshot map.
    pub fn from_snapshot(modules: BTreeMap<String, ModuleSnapshot>) -> Self {
        WiringBaseline {
            version: WIRING_BASELINE_VERSION,
            modules,
        }
    }

    /// Parse a baseline JSON document. A version this build does not
    /// understand is an error — refusing loudly beats misreading a shape.
    pub fn from_json_str(text: &str) -> Result<Self, String> {
        let parsed: WiringBaseline =
            serde_json::from_str(text).map_err(|e| format!("baseline is not valid JSON: {e}"))?;
        if parsed.version != WIRING_BASELINE_VERSION {
            return Err(format!(
                "baseline version {} is not supported (this build writes version {})",
                parsed.version, WIRING_BASELINE_VERSION
            ));
        }
        Ok(parsed)
    }

    /// Serialize as pretty JSON (the on-disk baseline form).
    pub fn to_json_pretty(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|e| format!("failed to serialize baseline: {e}"))
    }
}

/// The change for one module against the baseline. Both directions are always
/// present (possibly empty) so the JSON shape does not move between runs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleDelta {
    /// Surfaces present now that the baseline did not have.
    pub surfaces_added: Vec<SurfaceRef>,
    /// Surfaces the baseline had that are gone now.
    pub surfaces_removed: Vec<SurfaceRef>,
    /// Edge identities present now that the baseline did not have.
    pub edges_added: Vec<String>,
    /// Edge identities the baseline had that are gone now.
    pub edges_removed: Vec<String>,
}

/// One module's wiring report row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleWiring {
    /// The module name (first path component).
    pub module: String,
    /// The module classification (see the module docs for the rule).
    pub classification: ModuleClass,
    /// Public surfaces owned by the module, sorted by `surface_id`.
    pub surfaces: Vec<SurfaceRef>,
    /// `surfaces.len()` — explicit so a consumer need not count.
    pub surface_count: usize,
    /// Edges that point INTO this module from another module.
    pub inbound_edges: usize,
    /// Edges that point OUT of this module into another module or a
    /// dependency pseudo-node (`pkg:`/`sys:`/`external:`).
    pub outbound_edges: usize,
    /// Surfaces owned by the module with no coverage link at all.
    pub untested_surfaces: Vec<SurfaceRef>,
    /// `untested_surfaces.len()`.
    pub untested_count: usize,
    /// Delta against the baseline, or `null` when no baseline was compared.
    pub delta: Option<ModuleDelta>,
}

/// A top-level directory that is not a module, with WHY (AC5: nothing is
/// silently dropped — a non-code directory is named and explained).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExcludedDir {
    /// The directory path, repo-relative.
    pub path: String,
    /// The reason it is not a module.
    pub reason: String,
}

/// One consumed-interface row from the GAP-112 silent-fallback detector,
/// mirrored here so the whole report is one serializable type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterfaceSatisfier {
    /// The satisfying type's name.
    #[serde(rename = "type")]
    pub type_name: String,
    /// The file that defines it.
    pub file: String,
    /// `production` or `test`.
    pub role: String,
}

/// One interface verdict (GAP-112), embedded in the report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterfaceResult {
    pub interface: String,
    /// `pass` | `finding` | `unsupported`.
    pub state: String,
    pub consumers: Vec<String>,
    pub satisfiers: Vec<InterfaceSatisfier>,
}

/// Baseline provenance, echoed into the report so a consumer can tell a
/// compared run from a bare run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineInfo {
    /// Absolute path of the baseline that was read, if any.
    pub path: Option<String>,
    /// Whether a delta was computed against it.
    pub compared: bool,
    /// Absolute path a new baseline was written to (if any).
    pub written: Option<String>,
}

/// Roll-up totals over every module — the numbers the scorer reads first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WiringTotals {
    pub modules: usize,
    pub surfaces: usize,
    pub untested_surfaces: usize,
    pub inbound_edges: usize,
    pub outbound_edges: usize,
    pub findings: usize,
}

/// The full, versioned wiring report — the command's `--json` document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WiringReport {
    /// The versioned shape id (also exposed under `schema` for continuity
    /// with the `hilo.graph.wiring/1` field set GAP-112 documented).
    pub version: String,
    pub schema: String,
    /// Absolute path of the scanned root.
    pub root: String,
    /// `self` when the target is the Hilo workspace itself, else `foreign`.
    pub scope: String,
    /// How many source files were parsed.
    pub scanned_files: usize,
    /// Languages present in the tree with no conformance extraction.
    pub languages_unsupported: Vec<String>,
    /// Per-module rows, sorted by module name.
    pub modules: Vec<ModuleWiring>,
    /// Top-level directories that are not modules, each with a reason.
    pub excluded: Vec<ExcludedDir>,
    /// Baseline provenance, or `null` when no baseline was involved.
    pub baseline: Option<BaselineInfo>,
    /// Honest notes about degraded inputs (absent artifacts, etc.).
    pub notes: Vec<String>,
    /// The GAP-112 interface verdicts.
    pub interfaces: Vec<InterfaceResult>,
    /// Count of `finding` rows among `interfaces`.
    pub finding_count: usize,
    /// Roll-up totals.
    pub totals: WiringTotals,
}

/// A source file discovered in the scan, with the role `classify` gave it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleFile {
    /// Repo-relative, `/`-separated path.
    pub rel: String,
    /// `classify`'s role string (`entrypoint`, `library`, `test`, …).
    pub role: String,
}

/// Everything the report needs, collected by the caller (the CLI owns IO).
pub struct WiringInputs<'a> {
    pub root: String,
    pub scope: String,
    pub scanned_files: usize,
    pub languages_unsupported: Vec<String>,
    /// module name -> its source files (with roles).
    pub module_files: BTreeMap<String, Vec<ModuleFile>>,
    /// The COV-1 surface inventory (may be empty).
    pub surfaces: &'a [Surface],
    /// The dependency edges (may be empty).
    pub edges: &'a [Edge],
    /// The COV-2 coverage links (may be empty).
    pub links: &'a [CoverageLink],
    /// The baseline to compare against, if any.
    pub baseline: Option<&'a WiringBaseline>,
    pub baseline_info: Option<BaselineInfo>,
    pub interfaces: Vec<InterfaceResult>,
    pub finding_count: usize,
    /// Top-level directories that are not modules.
    pub excluded: Vec<ExcludedDir>,
    /// Degraded-input notes.
    pub notes: Vec<String>,
}

/// The module name for a repo-relative path: its first path component, or
/// [`ROOT_MODULE`] for a file that sits directly at the root.
pub fn module_of(rel: &str) -> String {
    match rel.split_once('/') {
        Some((first, _)) if !first.is_empty() => first.to_string(),
        _ => ROOT_MODULE.to_string(),
    }
}

/// Pseudo-node prefixes the graph uses for things that are NOT modules:
/// standard-library / external / resolver-local nodes, plus the bare crate
/// form. `pkg:` is handled separately — it names a workspace crate when the
/// name matches a known module, and an external dependency otherwise.
const NON_MODULE_PREFIXES: [&str; 4] = ["sys:", "external:", "local:", "std:"];

/// Normalize a module/crate name for cross-spelling comparison: the Rust
/// crate `hilo_metadata` and its directory `hilo-metadata` are one module.
fn normalize_module_name(name: &str) -> String {
    name.replace('_', "-").to_lowercase()
}

/// Resolves edge endpoints to modules, **restricted to modules that exist**.
///
/// A `pkg:<crate>` node names a workspace crate when its name matches a known
/// module (normalized: `pkg:hilo_metadata` → `hilo-metadata`) and an external
/// dependency otherwise. `std:`/`sys:`/`external:`/`local:` nodes name no
/// module. Any other endpoint is attributed to its first path component only
/// when that component is a known module — so a resolver pseudo-node can
/// never invent a phantom module row.
#[derive(Debug, Clone, Default)]
pub struct ModuleIndex {
    known: BTreeSet<String>,
    by_normalized: BTreeMap<String, String>,
}

impl ModuleIndex {
    /// Build the index from the modules that exist (the scan's top-level
    /// directories plus every surface owner's module).
    pub fn new(modules: impl IntoIterator<Item = String>) -> Self {
        let known: BTreeSet<String> = modules.into_iter().collect();
        let by_normalized = known
            .iter()
            .map(|m| (normalize_module_name(m), m.clone()))
            .collect();
        ModuleIndex {
            known,
            by_normalized,
        }
    }

    /// The module an edge endpoint belongs to, or `None` when the endpoint
    /// names no existing module (an external crate, `std:`, `local:`, …).
    pub fn resolve(&self, node: &str) -> Option<String> {
        if let Some(rest) = node.strip_prefix("pkg:") {
            let head = rest.split([':', '/']).next().unwrap_or("");
            return self
                .by_normalized
                .get(&normalize_module_name(head))
                .cloned();
        }
        if NON_MODULE_PREFIXES.iter().any(|p| node.starts_with(p)) {
            return None;
        }
        let module = module_of(node);
        self.known.contains(&module).then_some(module)
    }

    /// Whether `name` is a known module.
    pub fn contains(&self, name: &str) -> bool {
        self.known.contains(name)
    }
}

/// The module index for a scan: every module the file scan found, plus every
/// module that owns a public surface (a surface can be declared in a file
/// whose language is not scan-visible, e.g. a UniFFI `.udl`).
pub fn module_index(
    module_files: &BTreeMap<String, Vec<ModuleFile>>,
    surfaces: &[Surface],
) -> ModuleIndex {
    ModuleIndex::new(
        module_files
            .keys()
            .cloned()
            .chain(surfaces.iter().map(|s| module_of(&s.owner_file))),
    )
}

/// A stable identity for one edge — used in baselines and deltas.
pub fn edge_id(e: &Edge) -> String {
    format!("{}|{}|{}", e.from, e.rel, e.to)
}

/// Per-module edge tallies: `(inbound, outbound)` inter-module counts.
/// Self-edges (both endpoints in the same module) are not counted; an
/// endpoint naming no module (an external dependency) counts as an outbound
/// connection from the source module's side only.
fn edge_counts(edges: &[Edge], index: &ModuleIndex) -> BTreeMap<String, (usize, usize)> {
    let mut counts: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for e in edges {
        let from_mod = index.resolve(&e.from);
        let to_mod = index.resolve(&e.to);
        if let Some(from) = &from_mod {
            if to_mod.as_deref() != Some(from.as_str()) {
                counts.entry(from.clone()).or_default().1 += 1;
            }
        }
        if let Some(to) = &to_mod {
            if from_mod.as_deref() != Some(to.as_str()) {
                counts.entry(to.clone()).or_default().0 += 1;
            }
        }
    }
    counts
}

/// Build the per-module snapshot (surfaces + touching edge ids) from the
/// current surfaces and edges — the value a baseline stores and a delta
/// compares.
pub fn snapshot(
    surfaces: &[Surface],
    edges: &[Edge],
    index: &ModuleIndex,
) -> BTreeMap<String, ModuleSnapshot> {
    let mut out: BTreeMap<String, ModuleSnapshot> = BTreeMap::new();

    for s in surfaces {
        let module = module_of(&s.owner_file);
        out.entry(module)
            .or_default()
            .surfaces
            .push(SurfaceRef::from_surface(s));
    }

    for e in edges {
        let id = edge_id(e);
        let modules: BTreeSet<String> = [&e.from, &e.to]
            .into_iter()
            .filter_map(|node| index.resolve(node))
            .collect();
        for module in modules {
            out.entry(module).or_default().edges.push(id.clone());
        }
    }

    for snap in out.values_mut() {
        snap.surfaces.sort();
        snap.surfaces.dedup();
        snap.edges.sort();
        snap.edges.dedup();
    }
    out
}

/// Compute the per-module delta between `baseline` and `current`. Modules
/// present in only one side are delta'd against an empty snapshot, so a
/// wholly-added or wholly-removed module is reported rather than skipped.
pub fn compute_delta(
    baseline: &WiringBaseline,
    current: &BTreeMap<String, ModuleSnapshot>,
) -> BTreeMap<String, ModuleDelta> {
    let empty = ModuleSnapshot::default();
    let mut modules: BTreeSet<&String> = BTreeSet::new();
    modules.extend(baseline.modules.keys());
    modules.extend(current.keys());

    let mut out: BTreeMap<String, ModuleDelta> = BTreeMap::new();
    for module in modules {
        let before = baseline.modules.get(module).unwrap_or(&empty);
        let after = current.get(module).unwrap_or(&empty);

        let before_surfaces: BTreeSet<&SurfaceRef> = before.surfaces.iter().collect();
        let after_surfaces: BTreeSet<&SurfaceRef> = after.surfaces.iter().collect();
        let before_edges: BTreeSet<&String> = before.edges.iter().collect();
        let after_edges: BTreeSet<&String> = after.edges.iter().collect();

        let mut surfaces_added: Vec<SurfaceRef> = after_surfaces
            .difference(&before_surfaces)
            .map(|s| (*s).clone())
            .collect();
        surfaces_added.sort();
        let mut surfaces_removed: Vec<SurfaceRef> = before_surfaces
            .difference(&after_surfaces)
            .map(|s| (*s).clone())
            .collect();
        surfaces_removed.sort();
        let mut edges_added: Vec<String> = after_edges
            .difference(&before_edges)
            .map(|s| (*s).clone())
            .collect();
        edges_added.sort();
        let mut edges_removed: Vec<String> = before_edges
            .difference(&after_edges)
            .map(|s| (*s).clone())
            .collect();
        edges_removed.sort();

        out.insert(
            module.clone(),
            ModuleDelta {
                surfaces_added,
                surfaces_removed,
                edges_added,
                edges_removed,
            },
        );
    }
    out
}

/// Classify a module — the precedence documented on [`ModuleClass`].
///
/// `files` are the module's source files with their `classify` roles,
/// `surfaces` is how many public surfaces the module owns, and the edge
/// tallies are inter-module counts.
///
/// `test` requires at least one scanned file, ALL of which are tests: a
/// module the scan never saw (e.g. one known only through a surface declared
/// in a non-source file) is classified from its surfaces and edges instead —
/// "no files" is not "all files are tests".
pub fn classify_module(
    files: &[ModuleFile],
    surface_count: usize,
    inbound: usize,
    outbound: usize,
) -> ModuleClass {
    let non_test: Vec<&ModuleFile> = files.iter().filter(|f| f.role != "test").collect();
    if !files.is_empty() && non_test.is_empty() {
        return ModuleClass::Test;
    }
    if non_test.iter().any(|f| f.role == "entrypoint") {
        return ModuleClass::Entrypoint;
    }
    if inbound > 0 {
        return ModuleClass::Service;
    }
    if outbound > 0 || surface_count > 0 {
        return ModuleClass::Lib;
    }
    ModuleClass::Dead
}

/// A surface is TESTED when a coverage link names either its identity
/// (`surface_id`) or its owning file. With no links at all nothing is tested,
/// which the report states in its `notes` rather than hiding.
fn is_surface_tested(s: &SurfaceRef, links: &[CoverageLink]) -> bool {
    links
        .iter()
        .any(|l| l.target == s.surface_id || l.target == s.owner_file)
}

/// Assemble the versioned report. Pure: every fact is an input.
pub fn build_report(inputs: WiringInputs<'_>) -> WiringReport {
    let WiringInputs {
        root,
        scope,
        scanned_files,
        languages_unsupported,
        module_files,
        surfaces,
        edges,
        links,
        baseline,
        baseline_info,
        interfaces,
        finding_count,
        excluded,
        notes,
    } = inputs;

    let index = module_index(&module_files, surfaces);
    let snap = snapshot(surfaces, edges, &index);
    let counts = edge_counts(edges, &index);
    let deltas = baseline.map(|b| compute_delta(b, &snap));

    // Every module seen in the file scan OR in the snapshot is a row: a
    // module with files but no surfaces/edges must still be classified.
    let mut module_names: BTreeSet<String> = BTreeSet::new();
    module_names.extend(module_files.keys().cloned());
    module_names.extend(snap.keys().cloned());

    let mut modules: Vec<ModuleWiring> = Vec::with_capacity(module_names.len());
    let mut totals = WiringTotals {
        findings: finding_count,
        ..WiringTotals::default()
    };

    for name in module_names {
        let empty_files: Vec<ModuleFile> = Vec::new();
        let files = module_files.get(&name).unwrap_or(&empty_files);
        let empty_snap = ModuleSnapshot::default();
        let snap_ref = snap.get(&name).unwrap_or(&empty_snap);
        let (inbound, outbound) = counts.get(&name).copied().unwrap_or((0, 0));

        let classification = classify_module(files, snap_ref.surfaces.len(), inbound, outbound);

        let mut untested: Vec<SurfaceRef> = snap_ref
            .surfaces
            .iter()
            .filter(|s| !is_surface_tested(s, links))
            .cloned()
            .collect();
        untested.sort();

        let untested_count = untested.len();
        let delta = deltas.as_ref().and_then(|d| d.get(&name).cloned());

        totals.modules += 1;
        totals.surfaces += snap_ref.surfaces.len();
        totals.untested_surfaces += untested_count;
        totals.inbound_edges += inbound;
        totals.outbound_edges += outbound;

        modules.push(ModuleWiring {
            module: name,
            classification,
            surfaces: snap_ref.surfaces.clone(),
            surface_count: snap_ref.surfaces.len(),
            inbound_edges: inbound,
            outbound_edges: outbound,
            untested_surfaces: untested,
            untested_count,
            delta,
        });
    }

    WiringReport {
        version: WIRING_REPORT_VERSION.to_string(),
        schema: WIRING_REPORT_VERSION.to_string(),
        root,
        scope,
        scanned_files,
        languages_unsupported,
        modules,
        excluded,
        baseline: baseline_info,
        notes,
        interfaces,
        finding_count,
        totals,
    }
}

/// Read edges from an `edges.jsonl`-shaped file. Malformed lines are skipped
/// (the file is append-only and may have interleaved writers), never turned
/// into phantom edges.
pub fn read_edges(path: &Path) -> std::io::Result<Vec<Edge>> {
    let mut edges = Vec::new();
    if !path.exists() {
        return Ok(edges);
    }
    let contents = std::fs::read_to_string(path)?;
    for line in contents.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(edge) = serde_json::from_str::<Edge>(line) {
            edges.push(edge);
        }
    }
    Ok(edges)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coverage_links::{CoverageLink, EvidenceKind};
    use crate::surfaces::{Surface, SurfaceKind};
    use hilo_metadata::inventory::Edge;

    fn edge(from: &str, rel: &str, to: &str) -> Edge {
        Edge {
            from: from.to_string(),
            to: to.to_string(),
            rel: rel.to_string(),
            provenance: "ast_exact".to_string(),
            confidence: 1.0,
        }
    }

    fn surface(kind: SurfaceKind, owner: &str, symbol: &str) -> Surface {
        Surface::new(kind, owner, symbol, symbol, true)
    }

    fn sref(name: &str, owner: &str) -> SurfaceRef {
        SurfaceRef {
            surface_id: format!("id-{name}"),
            name: name.to_string(),
            owner_file: owner.to_string(),
        }
    }

    fn snapshot_of(_name: &str, surfaces: &[SurfaceRef], edges: &[&str]) -> ModuleSnapshot {
        ModuleSnapshot {
            surfaces: surfaces.to_vec(),
            edges: edges.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    #[test]
    fn module_of_uses_the_first_component() {
        assert_eq!(module_of("hilo-graph/src/lib.rs"), "hilo-graph");
        assert_eq!(module_of("src/main.go"), "src");
        assert_eq!(module_of("README.md"), ROOT_MODULE);
        assert_eq!(module_of(""), ROOT_MODULE);
    }

    #[test]
    fn module_index_resolves_crates_and_rejects_pseudo_nodes() {
        let index = ModuleIndex::new(
            ["hilo-graph", "hilo-core", "hilo-metadata", "src"]
                .into_iter()
                .map(str::to_string),
        );
        // A file path resolves to its first component, only when known.
        assert_eq!(
            index.resolve("hilo-graph/src/lib.rs").as_deref(),
            Some("hilo-graph")
        );
        assert_eq!(index.resolve("vendor/other/x.rs"), None);
        // A workspace crate pseudo-node resolves to its directory
        // (`_` vs `-` spelling normalizes).
        assert_eq!(
            index.resolve("pkg:hilo_metadata").as_deref(),
            Some("hilo-metadata")
        );
        assert_eq!(
            index
                .resolve("pkg:hilo_graph::signal::understand")
                .as_deref(),
            Some("hilo-graph")
        );
        // An EXTERNAL crate stays external.
        assert_eq!(index.resolve("pkg:serde"), None);
        assert_eq!(index.resolve("pkg:std::path::PathBuf"), None);
        // Resolver pseudo-prefixes name no module.
        assert_eq!(index.resolve("std:testing"), None);
        assert_eq!(index.resolve("local:./x.rs"), None);
        assert_eq!(index.resolve("sys:unix"), None);
        assert_eq!(index.resolve("external:repo:path"), None);
    }

    #[test]
    fn class_strings_round_trip() {
        for class in ModuleClass::ALL {
            assert_eq!(ModuleClass::parse(class.as_str()), Some(class));
        }
        assert_eq!(ModuleClass::parse("nope"), None);
        // The AC names the vocabulary explicitly.
        for expected in ["entrypoint", "service", "lib", "test", "dead"] {
            assert!(ModuleClass::parse(expected).is_some());
        }
    }

    #[test]
    fn snapshot_groups_surfaces_and_touching_edges() {
        let surfaces = vec![
            surface(
                SurfaceKind::PublicApiItem,
                "hilo-graph/src/lib.rs",
                "GraphDB",
            ),
            surface(
                SurfaceKind::McpTool,
                "hilo-mcp/src/tools/mod.rs",
                "vfs_get_metadata",
            ),
        ];
        let edges = vec![
            edge(
                "hilo-graph/src/lib.rs",
                "imports",
                "hilo-core/src/manifest.rs",
            ),
            edge("hilo-mcp/src/tools/mod.rs", "imports", "pkg:serde"),
        ];
        let index = ModuleIndex::new(
            ["hilo-graph", "hilo-core", "hilo-mcp"]
                .into_iter()
                .map(str::to_string),
        );
        let snap = snapshot(&surfaces, &edges, &index);
        assert_eq!(snap["hilo-graph"].surfaces.len(), 1);
        assert_eq!(snap["hilo-graph"].surfaces[0].name, "GraphDB");
        // The edge is recorded against BOTH of its file endpoints.
        assert_eq!(snap["hilo-graph"].edges.len(), 1);
        assert_eq!(snap["hilo-core"].edges.len(), 1);
        // A `pkg:` pseudo-node creates no phantom module.
        assert!(!snap.contains_key("pkg:serde"));
        assert_eq!(snap["hilo-mcp"].edges.len(), 1);
    }

    #[test]
    fn edge_counts_are_inter_module_only() {
        let edges = vec![
            edge("a/src/x.rs", "imports", "b/src/y.rs"),
            edge("b/src/y.rs", "imports", "a/src/x.rs"),
            edge("a/src/x.rs", "imports", "a/src/z.rs"), // intra-module: no count
            edge("a/src/x.rs", "imports", "pkg:serde"),  // external: outbound only
        ];
        let index = ModuleIndex::new(["a", "b"].into_iter().map(str::to_string));
        let counts = edge_counts(&edges, &index);
        assert_eq!(counts["a"], (1, 2)); // inbound from b; outbound to b + pkg
        assert_eq!(counts["b"], (1, 1));
    }

    #[test]
    fn classify_covers_every_class() {
        let entry = ModuleFile {
            rel: "bin/main.rs".into(),
            role: "entrypoint".into(),
        };
        let lib = ModuleFile {
            rel: "src/lib.rs".into(),
            role: "library".into(),
        };
        let test = ModuleFile {
            rel: "tests/it.rs".into(),
            role: "test".into(),
        };

        // 1. all-test module.
        assert_eq!(
            classify_module(std::slice::from_ref(&test), 0, 0, 0),
            ModuleClass::Test
        );
        // 2. entrypoint wins over everything else.
        assert_eq!(
            classify_module(&[entry.clone(), lib.clone()], 5, 9, 9),
            ModuleClass::Entrypoint
        );
        // 3. consumed production module.
        assert_eq!(
            classify_module(std::slice::from_ref(&lib), 0, 3, 1),
            ModuleClass::Service
        );
        // 4. leaf provider (exports something but nothing imports it).
        assert_eq!(
            classify_module(std::slice::from_ref(&lib), 0, 0, 2),
            ModuleClass::Lib
        );
        assert_eq!(
            classify_module(std::slice::from_ref(&lib), 1, 0, 0),
            ModuleClass::Lib
        );
        // 5. linked to nothing.
        assert_eq!(
            classify_module(std::slice::from_ref(&lib), 0, 0, 0),
            ModuleClass::Dead
        );
        // A module the scan never saw (no files at all) is NOT "all test":
        // it is classified from its surfaces/edges.
        assert_eq!(classify_module(&[], 2, 0, 1), ModuleClass::Lib);
        assert_eq!(classify_module(&[], 0, 0, 0), ModuleClass::Dead);
    }

    /// AC3: on a fixture where one surface is added and another removed, the
    /// exact delta — counts AND names — is asserted.
    #[test]
    fn delta_reports_added_and_removed_surfaces_exactly() {
        let a = sref("alpha", "m/src/a.rs");
        let b = sref("beta", "m/src/b.rs");
        let c = sref("gamma", "m/src/c.rs");

        let baseline = WiringBaseline::from_snapshot(
            [(
                "m".to_string(),
                snapshot_of("m", &[a.clone(), b.clone()], &["e1", "e2"]),
            )]
            .into_iter()
            .collect(),
        );
        let current: BTreeMap<String, ModuleSnapshot> = [(
            "m".to_string(),
            snapshot_of("m", &[b.clone(), c.clone()], &["e2", "e3"]),
        )]
        .into_iter()
        .collect();

        let delta = compute_delta(&baseline, &current);
        let d = &delta["m"];
        assert_eq!(d.surfaces_added.len(), 1);
        assert_eq!(d.surfaces_added[0].name, "gamma");
        assert_eq!(d.surfaces_removed.len(), 1);
        assert_eq!(d.surfaces_removed[0].name, "alpha");
        assert_eq!(d.edges_added, vec!["e3".to_string()]);
        assert_eq!(d.edges_removed, vec!["e1".to_string()]);
    }

    #[test]
    fn delta_reports_a_wholly_added_and_wholly_removed_module() {
        let baseline = WiringBaseline::from_snapshot(
            [(
                "gone".to_string(),
                snapshot_of("gone", &[sref("x", "gone/x.rs")], &[]),
            )]
            .into_iter()
            .collect(),
        );
        let current: BTreeMap<String, ModuleSnapshot> = [(
            "fresh".to_string(),
            snapshot_of("fresh", &[sref("y", "fresh/y.rs")], &[]),
        )]
        .into_iter()
        .collect();
        let delta = compute_delta(&baseline, &current);
        assert_eq!(delta["fresh"].surfaces_added[0].name, "y");
        assert_eq!(delta["gone"].surfaces_removed[0].name, "x");
    }

    #[test]
    fn baseline_round_trips_and_rejects_an_unknown_version() {
        let baseline = WiringBaseline::from_snapshot(
            [(
                "m".to_string(),
                snapshot_of("m", &[sref("alpha", "m/a.rs")], &["e|imports|f"]),
            )]
            .into_iter()
            .collect(),
        );
        let json = baseline.to_json_pretty().unwrap();
        let back = WiringBaseline::from_json_str(&json).unwrap();
        assert_eq!(baseline, back);

        let bad = json.replace("\"version\": 1", "\"version\": 99");
        assert!(WiringBaseline::from_json_str(&bad).is_err());
        assert!(WiringBaseline::from_json_str("not json").is_err());
    }

    #[test]
    fn untested_surfaces_use_coverage_links_on_id_or_file() {
        let surfaces = vec![
            surface(SurfaceKind::PublicApiItem, "m/src/a.rs", "A"),
            surface(SurfaceKind::PublicApiItem, "m/src/b.rs", "B"),
        ];
        let links = vec![
            // Covers A by surface_id.
            CoverageLink::new(
                "m/tests/a_test.rs",
                surfaces[0].surface_id.clone(),
                EvidenceKind::Import,
            ),
            // Covers B by file.
            CoverageLink::new(
                "m/tests/b_test.rs",
                "m/src/b.rs".to_string(),
                EvidenceKind::Import,
            ),
        ];
        let report = build_report(WiringInputs {
            root: "/fixture".into(),
            scope: "self".into(),
            scanned_files: 4,
            languages_unsupported: vec![],
            module_files: [(
                "m".to_string(),
                vec![ModuleFile {
                    rel: "m/src/a.rs".into(),
                    role: "library".into(),
                }],
            )]
            .into_iter()
            .collect(),
            surfaces: &surfaces,
            edges: &[],
            links: &links,
            baseline: None,
            baseline_info: None,
            interfaces: vec![],
            finding_count: 0,
            excluded: vec![],
            notes: vec![],
        });
        assert_eq!(report.totals.modules, 1);
        assert_eq!(report.totals.surfaces, 2);
        assert_eq!(report.totals.untested_surfaces, 0);
        let m = &report.modules[0];
        assert_eq!(m.untested_count, 0);
        assert_eq!(m.classification, ModuleClass::Lib);
    }

    /// AC2: the JSON shape is versioned and locked.
    #[test]
    fn json_shape_is_stable_and_versioned() {
        let surfaces = vec![surface(
            SurfaceKind::CliVerb,
            "hilo-cli/src/cli.rs",
            "graph wiring",
        )];
        let edges = vec![edge(
            "hilo-cli/src/cli.rs",
            "imports",
            "hilo-graph/src/lib.rs",
        )];
        let baseline = WiringBaseline::from_snapshot(
            [("hilo-cli".to_string(), snapshot_of("hilo-cli", &[], &[]))]
                .into_iter()
                .collect(),
        );
        let report = build_report(WiringInputs {
            root: "/x".into(),
            scope: "self".into(),
            scanned_files: 3,
            languages_unsupported: vec!["java".into()],
            module_files: [
                (
                    "hilo-cli".to_string(),
                    vec![ModuleFile {
                        rel: "hilo-cli/src/main.rs".into(),
                        role: "entrypoint".into(),
                    }],
                ),
                (
                    "hilo-graph".to_string(),
                    vec![ModuleFile {
                        rel: "hilo-graph/src/lib.rs".into(),
                        role: "library".into(),
                    }],
                ),
            ]
            .into_iter()
            .collect(),
            surfaces: &surfaces,
            edges: &edges,
            links: &[],
            baseline: Some(&baseline),
            baseline_info: Some(BaselineInfo {
                path: Some("/x/baseline.json".into()),
                compared: true,
                written: None,
            }),
            interfaces: vec![InterfaceResult {
                interface: "Iface".into(),
                state: "pass".into(),
                consumers: vec!["a.rs".into()],
                satisfiers: vec![InterfaceSatisfier {
                    type_name: "Prod".into(),
                    file: "b.rs".into(),
                    role: "production".into(),
                }],
            }],
            finding_count: 0,
            excluded: vec![ExcludedDir {
                path: "docs".into(),
                reason: "no supported source files".into(),
            }],
            notes: vec![],
        });

        let v = serde_json::to_value(&report).unwrap();
        let obj = v.as_object().unwrap();
        // Versioned + the GAP-112 continuity key.
        assert_eq!(obj["version"], WIRING_REPORT_VERSION);
        assert_eq!(obj["schema"], WIRING_REPORT_VERSION);
        for key in [
            "root",
            "scope",
            "scanned_files",
            "languages_unsupported",
            "modules",
            "excluded",
            "baseline",
            "notes",
            "interfaces",
            "finding_count",
            "totals",
        ] {
            assert!(obj.contains_key(key), "report missing '{key}': {obj:?}");
        }

        let module = obj["modules"].as_array().unwrap()[0].as_object().unwrap();
        for key in [
            "module",
            "classification",
            "surfaces",
            "surface_count",
            "inbound_edges",
            "outbound_edges",
            "untested_surfaces",
            "untested_count",
            "delta",
        ] {
            assert!(module.contains_key(key), "module row missing '{key}'");
        }
        // `delta` is always present (null when no baseline) — the shape does
        // not move between a compared and a bare run.
        assert!(module["delta"].is_null() || module["delta"].is_object());

        let totals = obj["totals"].as_object().unwrap();
        for key in [
            "modules",
            "surfaces",
            "untested_surfaces",
            "inbound_edges",
            "outbound_edges",
            "findings",
        ] {
            assert!(totals.contains_key(key), "totals missing '{key}'");
        }
    }

    #[test]
    fn read_edges_skips_malformed_lines_and_absent_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("edges.jsonl");
        assert!(read_edges(&path).unwrap().is_empty());
        std::fs::write(
            &path,
            "{\"from\":\"a\",\"to\":\"b\",\"rel\":\"imports\"}\nnot json\n\n",
        )
        .unwrap();
        let edges = read_edges(&path).unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].from, "a");
    }
}
