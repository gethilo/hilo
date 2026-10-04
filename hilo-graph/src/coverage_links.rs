//! Evidenced test→surface coverage links (COV-2).
//!
//! A coverage link is a first-class EVIDENCED relation between a test file
//! and a covered surface (a [`crate::surfaces::Surface`] `surface_id` when
//! resolvable, else a module/file path). Every row carries an
//! [`EvidenceKind`] (how the link was established), a `confidence` (0.0–1.0),
//! and a `direction` — no link without provenance. A test that establishes no
//! link does not silently disappear: the surfaces it fails to reach land in
//! the report's unlinked set, each with a cause.
//!
//! Layers are ADDITIVE and ordered: the same (test_file, target) pair can be
//! established by several evidence kinds, but it is ONE link — a later,
//! stronger layer (e.g. `runtime_trace`) RAISES the existing link's
//! confidence and upgrades its evidence kind instead of creating a duplicate
//! row. [`link_id`] is the content-derived one-row-per-pair key.
//!
//! Storage mirrors `surfaces.jsonl`: `.vfs/graph/coverage_links.jsonl`,
//! append-only discipline with dedupe-by-`link_id` — see
//! [`append_coverage_links_deduped`]. This artifact is what coverage scoring
//! consumes; the existing `tested_by` edges and file-level consumers are
//! untouched (COV-2 AC7: additive).

use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// How a test→target link was established. Ordered strongest-first for
/// merge precedence: a link established by a stronger layer wins over a
/// weaker one on the same (test_file, target) pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    /// The test observed the target at runtime (a recorded trace), the
    /// strongest evidence. Rank 5.
    RuntimeTrace,
    /// An explicit human/manifest-declared mapping names the pair. Rank 4.
    DeclaredMapping,
    /// The test file's AST imports the target. Rank 3.
    Import,
    /// The test file's source names the surface (word-boundary match on the
    /// surface's contract name). Rank 2.
    SymbolNameMatch,
    /// The test reaches the target through an in-repo fixture/helper it
    /// imports (one hop). Rank 1 — weakest.
    FixtureOrHelper,
}

impl EvidenceKind {
    /// All kinds strongest-first — merge precedence and census order.
    pub const ALL: [EvidenceKind; 5] = [
        EvidenceKind::RuntimeTrace,
        EvidenceKind::DeclaredMapping,
        EvidenceKind::Import,
        EvidenceKind::SymbolNameMatch,
        EvidenceKind::FixtureOrHelper,
    ];

    /// The snake_case string form used in JSONL rows and `--json` output.
    pub fn as_str(self) -> &'static str {
        match self {
            EvidenceKind::RuntimeTrace => "runtime_trace",
            EvidenceKind::DeclaredMapping => "declared_mapping",
            EvidenceKind::Import => "import",
            EvidenceKind::SymbolNameMatch => "symbol_name_match",
            EvidenceKind::FixtureOrHelper => "fixture_or_helper",
        }
    }

    /// Parse a snake_case kind string (inverse of [`EvidenceKind::as_str`]).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "runtime_trace" => Some(EvidenceKind::RuntimeTrace),
            "declared_mapping" => Some(EvidenceKind::DeclaredMapping),
            "import" => Some(EvidenceKind::Import),
            "symbol_name_match" => Some(EvidenceKind::SymbolNameMatch),
            "fixture_or_helper" => Some(EvidenceKind::FixtureOrHelper),
            _ => None,
        }
    }

    /// Merge precedence: higher wins when the same pair is established twice.
    /// Matches [`EvidenceKind::ALL`] order (strongest first).
    pub fn rank(self) -> u8 {
        match self {
            EvidenceKind::RuntimeTrace => 5,
            EvidenceKind::DeclaredMapping => 4,
            EvidenceKind::Import => 3,
            EvidenceKind::SymbolNameMatch => 2,
            EvidenceKind::FixtureOrHelper => 1,
        }
    }

    /// The default confidence a link carries when established by this kind
    /// and nothing stronger. Deliberately below 1.0 for everything except
    /// `runtime_trace`: only an observed-at-runtime link is certain.
    pub fn default_confidence(self) -> f64 {
        match self {
            EvidenceKind::RuntimeTrace => 1.0,
            EvidenceKind::DeclaredMapping => 0.9,
            EvidenceKind::Import => 0.7,
            EvidenceKind::SymbolNameMatch => 0.5,
            EvidenceKind::FixtureOrHelper => 0.4,
        }
    }
}

/// One evidenced test→target coverage link.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoverageLink {
    /// Content-derived identity of the (test_file, target) pair — see
    /// [`link_id`]. One row per id, always.
    pub link_id: String,
    /// The test file (repo-relative, `/`-separated).
    pub test_file: String,
    /// What the test covers: a `surface_id` from `.vfs/graph/surfaces.jsonl`
    /// when the target resolves to exactly one surface, else the module/file
    /// path it was established against.
    pub target: String,
    /// How the link was established — the provenance. Every link carries
    /// one; there is no link without evidence.
    pub evidence_kind: EvidenceKind,
    /// 0.0–1.0 confidence. A stronger layer RAISES this on an existing link
    /// (never duplicates it).
    pub confidence: f64,
    /// Link direction. `test_to_target` = the test exercises the target;
    /// `target_to_test` = the target names the test as its witness (reserved
    /// for declared mappings written in that direction).
    pub direction: String,
}

impl CoverageLink {
    /// Build a link with the evidence kind's default confidence and the
    /// canonical `test_to_target` direction.
    pub fn new(
        test_file: impl Into<String>,
        target: impl Into<String>,
        evidence_kind: EvidenceKind,
    ) -> Self {
        let test_file = test_file.into();
        let target = target.into();
        CoverageLink {
            link_id: link_id(&test_file, &target),
            test_file,
            target,
            evidence_kind,
            confidence: evidence_kind.default_confidence(),
            direction: "test_to_target".to_string(),
        }
    }
}

/// Derive a link's stable, content-derived identity.
///
/// NUL-framed sha256 over `test_file` and `target` — the same framing
/// [`crate::surfaces::surface_id`] uses, so neither a repo path nor a
/// surface id can collide by concatenation. Nothing else feeds the hash:
/// confidence and evidence kind are ATTRIBUTES of the link, not its
/// identity, so a stronger layer re-deriving the same pair lands on the
/// same id and merges instead of duplicating (AC3).
pub fn link_id(test_file: &str, target: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"hilo-coverage-link-v1");
    hasher.update([0u8]);
    hasher.update(test_file.as_bytes());
    hasher.update([0u8]);
    hasher.update(target.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Merge `new` links into `existing`, keyed on `link_id` (AC3: one link id
/// per (test_file, target), never a duplicate row).
///
/// On a pair collision the stronger evidence wins: higher
/// [`EvidenceKind::rank`] first, then higher confidence as tiebreak. A
/// weaker re-derivation NEVER lowers an existing link's confidence.
/// Returns the merged set in deterministic `link_id` order.
pub fn merge_links(existing: Vec<CoverageLink>, new: Vec<CoverageLink>) -> Vec<CoverageLink> {
    let mut by_id: std::collections::HashMap<String, CoverageLink> = existing
        .into_iter()
        .map(|l| (l.link_id.clone(), l))
        .collect();
    for link in new {
        match by_id.get(&link.link_id) {
            None => {
                by_id.insert(link.link_id.clone(), link);
            }
            Some(current) => {
                let upgrade = (link.evidence_kind.rank(), link.confidence)
                    > (current.evidence_kind.rank(), current.confidence);
                if upgrade {
                    by_id.insert(link.link_id.clone(), link);
                }
            }
        }
    }
    let mut merged: Vec<CoverageLink> = by_id.into_values().collect();
    merged.sort_by(|a, b| a.link_id.cmp(&b.link_id));
    merged
}

/// A surface with no coverage link at all, with WHY (AC5: never a bare
/// empty list — every unlinked surface names its cause).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnlinkedSurface {
    pub surface_id: String,
    pub kind: String,
    pub name: String,
    pub cause: String,
}

/// The full coverage-link report: rows, unlinked surfaces with per-cause
/// attribution, and the census that makes an empty link set distinguishable
/// from an unexercised inference pass.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CoverageLinkReport {
    pub schema: u32,
    pub links: Vec<CoverageLink>,
    pub unlinked: Vec<UnlinkedSurface>,
    /// Per-cause counts over `unlinked`, plus the inference-health rows
    /// (e.g. "inference failed for language X" when a test file's language
    /// has no parser).
    pub cause_census: Vec<(String, usize)>,
    /// The rule(s) the derivation ran under — the gap-signal twin of
    /// `surfaces`' `KindCensus.rule`.
    pub rules: Vec<String>,
}

impl CoverageLinkReport {
    /// The current schema version written to disk and `--json` output.
    pub const SCHEMA: u32 = 1;
}

/// Aggregate `unlinked` into per-cause counts, deterministic by first
/// appearance of each cause in the (sorted) unlinked list.
pub fn cause_census(unlinked: &[UnlinkedSurface]) -> Vec<(String, usize)> {
    let mut census: Vec<(String, usize)> = Vec::new();
    for row in unlinked {
        match census.iter_mut().find(|(c, _)| *c == row.cause) {
            Some((_, n)) => *n += 1,
            None => census.push((row.cause.clone(), 1)),
        }
    }
    census
}

/// Read existing links from a `coverage_links.jsonl`-shaped file.
///
/// Malformed lines are SKIPPED (tolerant read — the append-only file may
/// have interleaved writers), never silently turned into absent links that
/// would then be re-derived: a malformed row degrades to "not seen" and the
/// merge re-establishes the pair with fresh evidence.
pub fn read_links(path: &Path) -> std::io::Result<Vec<CoverageLink>> {
    let mut links = Vec::new();
    if !path.exists() {
        return Ok(links);
    }
    let contents = std::fs::read_to_string(path)?;
    for line in contents.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(link) = serde_json::from_str::<CoverageLink>(line) {
            links.push(link);
        }
    }
    Ok(links)
}

/// Persist `links` to `path`, deduplicated by `link_id` against what is
/// already on disk (AC1/AC3).
///
/// Mirrors `append_surfaces_deduped`'s discipline with one COV-2 addition:
/// because a stronger layer must RAISE an existing row's confidence in
/// place (not append a duplicate), the merged set is rewritten when the
/// merge changes any row. A no-op merge (all pairs already present at
/// equal-or-higher confidence) leaves the bytes untouched. A zero-row
/// persist still materializes the (possibly empty) file, so the artifact's
/// existence is independent of the derivation's yield.
///
/// Returns the number of rows ADDED plus rows UPGRADED (the merge delta).
pub fn append_coverage_links_deduped(
    path: &Path,
    links: &[CoverageLink],
) -> std::io::Result<usize> {
    let existing = read_links(path)?;
    let before: std::collections::HashMap<String, (&'static str, f64)> = existing
        .iter()
        .map(|l| (l.link_id.clone(), (l.evidence_kind.as_str(), l.confidence)))
        .collect();

    let merged = merge_links(existing, links.to_vec());
    let delta = merged
        .iter()
        .filter(|l| before.get(&l.link_id) != Some(&(l.evidence_kind.as_str(), l.confidence)))
        .count();

    if delta == 0 && path.exists() {
        return Ok(0);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut buf = Vec::with_capacity(merged.len() * 224);
    for link in &merged {
        serde_json::to_writer(&mut buf, link).map_err(std::io::Error::other)?;
        buf.push(b'\n');
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)?;
    file.write_all(&buf)?;
    file.flush()?;
    Ok(delta)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn link_id_is_stable_and_framed() {
        // Same pair → same id (the merge key must be stable across runs).
        assert_eq!(
            link_id("tests/a.rs", "surf:1"),
            link_id("tests/a.rs", "surf:1")
        );
        // Framing distinguishes concatenation collisions.
        assert_ne!(
            link_id("tests/a.rs", "surf:1"),
            link_id("tests/a", "rs:surf:1")
        );
        // Swapped operands are a different pair.
        assert_ne!(
            link_id("tests/a.rs", "surf:1"),
            link_id("surf:1", "tests/a.rs")
        );
    }

    #[test]
    fn every_link_carries_evidence_and_confidence() {
        for kind in EvidenceKind::ALL {
            let link = CoverageLink::new("tests/a.rs", "surf:1", kind);
            assert!(!link.link_id.is_empty());
            assert!(!link.direction.is_empty());
            assert!(
                (0.0..=1.0).contains(&link.confidence),
                "{kind:?} confidence must be in [0,1]: {}",
                link.confidence
            );
            // Serialized row carries the full provenance vocabulary.
            let value = serde_json::to_value(&link).unwrap();
            for key in [
                "link_id",
                "test_file",
                "target",
                "evidence_kind",
                "confidence",
                "direction",
            ] {
                assert!(value.get(key).is_some(), "row missing '{key}': {value:?}");
            }
        }
    }

    #[test]
    fn evidence_vocabulary_round_trips() {
        for kind in EvidenceKind::ALL {
            assert_eq!(EvidenceKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(EvidenceKind::parse("telepathy"), None);
        assert_eq!(EvidenceKind::parse(""), None);
    }

    #[test]
    fn merge_never_duplicates_a_pair_and_stronger_layer_raises_confidence() {
        // AC3: import establishes the pair; a later runtime_trace RAISES it.
        let import = CoverageLink::new("tests/a.rs", "surf:1", EvidenceKind::Import);
        let trace = CoverageLink::new("tests/a.rs", "surf:1", EvidenceKind::RuntimeTrace);
        assert_eq!(import.link_id, trace.link_id, "same pair → same id");

        let merged = merge_links(vec![import.clone()], vec![trace.clone()]);
        assert_eq!(merged.len(), 1, "no duplicate row for one pair");
        assert_eq!(merged[0].evidence_kind, EvidenceKind::RuntimeTrace);
        assert_eq!(
            merged[0].confidence, 1.0,
            "stronger layer raised confidence"
        );

        // Weaker re-derivation must NOT lower the raised link.
        let merged = merge_links(merged, vec![import]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].evidence_kind, EvidenceKind::RuntimeTrace);
        assert_eq!(merged[0].confidence, 1.0);
    }

    #[test]
    fn merge_keeps_distinct_pairs_and_is_deterministic() {
        let a = CoverageLink::new("tests/a.rs", "surf:1", EvidenceKind::Import);
        let b = CoverageLink::new("tests/b.rs", "surf:1", EvidenceKind::SymbolNameMatch);
        let merged = merge_links(vec![a.clone(), b.clone()], vec![a, b]);
        assert_eq!(merged.len(), 2, "distinct pairs stay distinct rows");
        let ids: Vec<&str> = merged.iter().map(|l| l.link_id.as_str()).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted, "deterministic link_id order");
    }

    #[test]
    fn append_dedupes_upgrades_and_creates_absent_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("graph").join("coverage_links.jsonl");

        // Zero-row persist still materializes the artifact (empty-but-present).
        assert_eq!(append_coverage_links_deduped(&path, &[]).unwrap(), 0);
        assert!(path.exists());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");

        // First append writes the row; identical re-append is a no-op (delta 0,
        // bytes untouched).
        let import = CoverageLink::new("tests/a.rs", "surf:1", EvidenceKind::Import);
        assert_eq!(
            append_coverage_links_deduped(&path, std::slice::from_ref(&import)).unwrap(),
            1
        );
        let before = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            append_coverage_links_deduped(&path, std::slice::from_ref(&import)).unwrap(),
            0
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

        // A stronger layer on the same pair UPGRADES the row: still one line.
        let trace = CoverageLink::new("tests/a.rs", "surf:1", EvidenceKind::RuntimeTrace);
        assert_eq!(
            append_coverage_links_deduped(&path, std::slice::from_ref(&trace)).unwrap(),
            1
        );
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            contents.lines().count(),
            1,
            "upgrade must not duplicate: {contents}"
        );
        let row: CoverageLink = serde_json::from_str(contents.lines().next().unwrap()).unwrap();
        assert_eq!(row.evidence_kind, EvidenceKind::RuntimeTrace);
        assert_eq!(row.confidence, 1.0);
    }

    #[test]
    fn read_links_tolerates_malformed_lines() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("coverage_links.jsonl");
        let link = CoverageLink::new("tests/a.rs", "surf:1", EvidenceKind::Import);
        std::fs::write(
            &path,
            format!(
                "{{\"garbage\": true}}\n{}\n\nnot json\n",
                serde_json::to_string(&link).unwrap()
            ),
        )
        .unwrap();
        let read = read_links(&path).unwrap();
        assert_eq!(read.len(), 1, "malformed lines skipped, good rows kept");
        assert_eq!(read[0].link_id, link.link_id);
    }

    #[test]
    fn cause_census_counts_every_unlinked_row() {
        let unlinked = vec![
            UnlinkedSurface {
                surface_id: "s1".into(),
                kind: "cli_verb".into(),
                name: "init".into(),
                cause: "no test evidence found".into(),
            },
            UnlinkedSurface {
                surface_id: "s2".into(),
                kind: "mcp_tool".into(),
                name: "vfs_get_metadata".into(),
                cause: "no test evidence found".into(),
            },
            UnlinkedSurface {
                surface_id: "s3".into(),
                kind: "fuse_op".into(),
                name: "lookup".into(),
                cause: "target is not a modeled surface".into(),
            },
        ];
        let census = cause_census(&unlinked);
        assert_eq!(census.len(), 2);
        assert!(census.contains(&("no test evidence found".to_string(), 2)));
        assert!(census.contains(&("target is not a modeled surface".to_string(), 1)));
        // Total equals the unlinked count — no row silently dropped.
        let total: usize = census.iter().map(|(_, n)| n).sum();
        assert_eq!(total, unlinked.len());
    }

    #[test]
    fn report_round_trips_through_json() {
        let report = CoverageLinkReport {
            schema: CoverageLinkReport::SCHEMA,
            links: vec![CoverageLink::new(
                "tests/a.rs",
                "surf:1",
                EvidenceKind::Import,
            )],
            unlinked: vec![UnlinkedSurface {
                surface_id: "s2".into(),
                kind: "cli_verb".into(),
                name: "init".into(),
                cause: "no test evidence found".into(),
            }],
            cause_census: cause_census(&[]),
            rules: vec!["ast import extraction".into()],
        };
        let json = serde_json::to_string(&report).unwrap();
        let back: CoverageLinkReport = serde_json::from_str(&json).unwrap();
        assert_eq!(report, back);
    }
}
