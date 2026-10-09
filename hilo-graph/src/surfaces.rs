//! First-class surface inventory — one row per externally-visible contract
//! point of the codebase (`cli_verb`, `cli_flag`, `mcp_tool`, `ffi_export`,
//! `fuse_op`, `config_key`, `public_api_item`), derived from code, never
//! hand-written.
//!
//! Rows are persisted to `.vfs/graph/surfaces.jsonl` with the same
//! append-only discipline as `edges.jsonl`: one JSON object per line, keyed
//! on a content-derived [`Surface::surface_id`] that stays stable when
//! unrelated code moves — COV-2/COV-5 and GAP-113 deltas key on it.
//!
//! This module owns the *model* and the *storage*: what a surface is, how its
//! identity is derived, and how the inventory file is appended. The
//! enumeration itself (the providers that walk clap, the MCP registry, the
//! UDL, the FUSE trait impl, the manifest schema, and the library re-exports)
//! lives in the caller that has those dependencies (`hilo-cli`), because
//! `hilo-graph` is a dependency of every crate those surfaces describe and
//! cannot depend back on them.

use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The externally-visible contract-point kinds a surface can be.
///
/// Serialized snake_case so the JSONL rows and the `--json` output carry the
/// same vocabulary as the task schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceKind {
    /// A top-level or nested CLI subcommand verb (e.g. `graph`, `graph warm`).
    CliVerb,
    /// A CLI flag/option (e.g. `--json`, `--max-depth`).
    CliFlag,
    /// An MCP tool name (e.g. `vfs_get_metadata`).
    McpTool,
    /// An FFI export (UniFFI interface method / namespace function).
    FfiExport,
    /// A FUSE filesystem operation (`fuser::Filesystem` trait method).
    FuseOp,
    /// A manifest configuration key.
    ConfigKey,
    /// A public library API item (a re-exported type/function).
    PublicApiItem,
}

impl SurfaceKind {
    /// Every kind in a fixed order — the order the per-kind census renders in,
    /// so `detected`/`expected` lines are deterministic across runs.
    pub const ALL: [SurfaceKind; 7] = [
        SurfaceKind::CliVerb,
        SurfaceKind::CliFlag,
        SurfaceKind::McpTool,
        SurfaceKind::FfiExport,
        SurfaceKind::FuseOp,
        SurfaceKind::ConfigKey,
        SurfaceKind::PublicApiItem,
    ];

    /// The snake_case string form used in JSONL rows and the `--kind` filter.
    pub fn as_str(self) -> &'static str {
        match self {
            SurfaceKind::CliVerb => "cli_verb",
            SurfaceKind::CliFlag => "cli_flag",
            SurfaceKind::McpTool => "mcp_tool",
            SurfaceKind::FfiExport => "ffi_export",
            SurfaceKind::FuseOp => "fuse_op",
            SurfaceKind::ConfigKey => "config_key",
            SurfaceKind::PublicApiItem => "public_api_item",
        }
    }

    /// Parse a snake_case kind string (the inverse of [`SurfaceKind::as_str`]).
    /// Returns `None` for anything outside the closed vocabulary.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "cli_verb" => Some(SurfaceKind::CliVerb),
            "cli_flag" => Some(SurfaceKind::CliFlag),
            "mcp_tool" => Some(SurfaceKind::McpTool),
            "ffi_export" => Some(SurfaceKind::FfiExport),
            "fuse_op" => Some(SurfaceKind::FuseOp),
            "config_key" => Some(SurfaceKind::ConfigKey),
            "public_api_item" => Some(SurfaceKind::PublicApiItem),
            _ => None,
        }
    }
}

/// One externally-visible contract point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Surface {
    /// Content-derived identity: sha256 of `kind`, `owner_file`, and
    /// `owner_symbol`. Stable across runs and unaffected by unrelated code
    /// moves — see [`surface_id`].
    pub surface_id: String,
    pub kind: SurfaceKind,
    /// The source file that declares/owns this surface (repo-relative,
    /// `/`-separated).
    pub owner_file: String,
    /// The code-level symbol that owns it (the clap variant, the MCP dispatch
    /// arm, the trait method name, the manifest field, the re-exported name).
    pub owner_symbol: String,
    /// The externally-visible name of the surface (usually equal to
    /// `owner_symbol`, but distinct for surfaces whose contract name differs
    /// from their owner symbol — e.g. a `graph warm` verb whose owner is the
    /// `GraphCommand::Warm` variant).
    pub name: String,
    /// Whether the surface is public (true for everything in the closed
    /// vocabulary today; reserved for future non-public internal surfaces).
    pub public: bool,
}

impl Surface {
    pub fn new(
        kind: SurfaceKind,
        owner_file: impl Into<String>,
        owner_symbol: impl Into<String>,
        name: impl Into<String>,
        public: bool,
    ) -> Self {
        let owner_file = owner_file.into();
        let owner_symbol = owner_symbol.into();
        let name = name.into();
        let surface_id = surface_id(kind, &owner_file, &owner_symbol);
        Surface {
            surface_id,
            kind,
            owner_file,
            owner_symbol,
            name,
            public,
        }
    }
}

/// Derive a surface's stable, content-derived identity.
///
/// The hash folds `kind`, `owner_file`, and `owner_symbol` with explicit
/// length framing (NUL separators — neither a repo path nor a symbol name can
/// contain NUL) so two different triples can never collide by concatenation.
/// Nothing else feeds the hash: in particular `name` and `public` do not, so
/// renaming a surface's display name or flipping a flag does NOT churn its id.
/// This is the property COV-2/COV-5 and GAP-113 depend on — a delta keyed on
/// `surface_id` must only move when a surface's *identity* (what it is, and
/// where it is declared) moves.
pub fn surface_id(kind: SurfaceKind, owner_file: &str, owner_symbol: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"hilo-surface-v1");
    hasher.update([0u8]);
    hasher.update(kind.as_str().as_bytes());
    hasher.update([0u8]);
    hasher.update(owner_file.as_bytes());
    hasher.update([0u8]);
    hasher.update(owner_symbol.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// The per-kind census: how many surfaces a rule detected vs how many it
/// expected to find.
///
/// `detected == 0` is the explicit "gap" signal (AC5): a parser that finds
/// nothing must read as a gap, never as an empty success, so the report and
/// the JSON both carry the `rule` that produced the zero.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KindCensus {
    pub kind: SurfaceKind,
    pub detected: usize,
    pub expected: usize,
    /// Human-readable description of the rule that produced `detected`
    /// (e.g. "clap CommandFactory introspection of hilo-cli/src/cli.rs",
    /// "hilo_mcp::tools::list_tools() registry",
    /// "hilo-ffi/src/hilo.udl not found — no ffi_export enumeration").
    pub rule: String,
}

impl KindCensus {
    /// True when the rule detected nothing — the loud-gap condition.
    pub fn is_gap(&self) -> bool {
        self.detected == 0
    }
}

/// The scope an inventory covers — whether the rows describe the Hilo
/// repository itself, or a repository that is *not* Hilo.
///
/// `hilo graph surfaces` derives the `cli_verb` / `cli_flag` / `mcp_tool`
/// rows from Hilo's **own** compiled registries, and the source-derived
/// kinds from Hilo's own files, so on a *foreign* repository none of that
/// describes the target. Rather than present Hilo self-data as target
/// coverage, a foreign run carries this marker and **zero** rows: the marker
/// is the whole result (GAP-112).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SurfaceScope {
    /// The inventory covers the Hilo repository itself.
    #[default]
    #[serde(rename = "self")]
    SelfHosted,
    /// The inventory covers a foreign repository: surface enumeration is
    /// unsupported there, so `surfaces` and `census` are empty and this
    /// marker is the entire result.
    #[serde(rename = "foreign")]
    Foreign,
}

impl SurfaceScope {
    /// The snake_case wire form (`self` / `foreign`).
    pub fn as_str(self) -> &'static str {
        match self {
            SurfaceScope::SelfHosted => "self",
            SurfaceScope::Foreign => "foreign",
        }
    }

    /// True when the inventory describes the Hilo repository itself.
    pub fn is_self(self) -> bool {
        matches!(self, SurfaceScope::SelfHosted)
    }
}

/// The full surface inventory: the rows plus the per-kind census that makes a
/// zero count distinguishable from an unexercised parser.
///
/// `schema` is a version number so a future reader can reject a shape it does
/// not understand instead of misreading it. `scope` says whether the rows
/// describe the Hilo repository itself or a foreign repository — on a foreign
/// repository `surfaces` and `census` are empty and `scope` is the entire
/// answer (GAP-112).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SurfaceInventory {
    pub schema: u32,
    /// Whether these rows describe the Hilo repository itself (`self`) or a
    /// repository that is not Hilo (`foreign`) — see [`SurfaceScope`].
    pub scope: SurfaceScope,
    pub surfaces: Vec<Surface>,
    pub census: Vec<KindCensus>,
}

impl SurfaceInventory {
    /// The current schema version written to disk and to `--json` output.
    pub const SCHEMA: u32 = 1;
}

/// Append `surfaces` to `surfaces.jsonl`, deduplicating by `surface_id`.
///
/// Mirrors `inventory::append_edges_deduped`'s append-only discipline: the
/// file is opened once in append mode, existing rows are read to build a seen
/// set keyed on `surface_id`, and only genuinely-new rows are written. A
/// zero-row append still materializes the (possibly empty) file, so the
/// artifact's existence is independent of the enumeration's yield — the same
/// "empty-but-present is the honest state" rule edges.jsonl follows.
///
/// Returns the number of rows actually appended.
pub fn append_surfaces_deduped(path: &Path, surfaces: &[Surface]) -> std::io::Result<usize> {
    let mut seen = std::collections::HashSet::new();
    if path.exists() {
        let contents = std::fs::read_to_string(path)?;
        for line in contents.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(surface) = serde_json::from_str::<Surface>(line) {
                seen.insert(surface.surface_id);
            }
        }
    }

    let new: Vec<&Surface> = surfaces
        .iter()
        .filter(|s| !seen.contains(&s.surface_id))
        .collect();

    if new.is_empty() {
        if !path.exists() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::File::create(path)?;
        }
        return Ok(0);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut buf = Vec::with_capacity(new.len() * 192);
    for surface in &new {
        serde_json::to_writer(&mut buf, surface).map_err(std::io::Error::other)?;
        buf.push(b'\n');
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(&buf)?;
    Ok(new.len())
}

/// Read surfaces from a `surfaces.jsonl`-shaped file (the append-only COV-1
/// inventory). Malformed lines are SKIPPED — the file may have interleaved
/// writers — so a torn row degrades to "not seen" rather than aborting the
/// read. A missing file is an empty inventory, not an error.
///
/// Used by the GAP-113 wiring report, which groups the inventory by owning
/// module and diffs it against a baseline.
pub fn read_surfaces(path: &Path) -> std::io::Result<Vec<Surface>> {
    let mut surfaces = Vec::new();
    if !path.exists() {
        return Ok(surfaces);
    }
    let contents = std::fs::read_to_string(path)?;
    for line in contents.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(surface) = serde_json::from_str::<Surface>(line) {
            surfaces.push(surface);
        }
    }
    Ok(surfaces)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample() -> Surface {
        Surface::new(
            SurfaceKind::McpTool,
            "hilo-mcp/src/tools/mod.rs",
            "vfs_get_metadata",
            "vfs_get_metadata",
            true,
        )
    }

    #[test]
    fn surface_id_is_stable_for_identical_identity() {
        let a = sample();
        let b = Surface::new(
            SurfaceKind::McpTool,
            "hilo-mcp/src/tools/mod.rs",
            "vfs_get_metadata",
            "vfs_get_metadata",
            true,
        );
        assert_eq!(a.surface_id, b.surface_id);
    }

    #[test]
    fn surface_id_changes_only_on_identity_not_display() {
        let a = sample();
        // Same identity, different display name and public flag — the id must
        // NOT move, because COV deltas key on identity, not presentation.
        let renamed = Surface::new(
            SurfaceKind::McpTool,
            "hilo-mcp/src/tools/mod.rs",
            "vfs_get_metadata",
            "get_metadata",
            false,
        );
        assert_eq!(a.surface_id, renamed.surface_id);

        // A moved owner symbol IS a new identity.
        let moved = Surface::new(
            SurfaceKind::McpTool,
            "hilo-mcp/src/tools/mod.rs",
            "vfs_get_metadata_v2",
            "vfs_get_metadata",
            true,
        );
        assert_ne!(a.surface_id, moved.surface_id);

        // A different kind IS a new identity.
        let different_kind = Surface::new(
            SurfaceKind::CliVerb,
            "hilo-mcp/src/tools/mod.rs",
            "vfs_get_metadata",
            "vfs_get_metadata",
            true,
        );
        assert_ne!(a.surface_id, different_kind.surface_id);
    }

    #[test]
    fn surface_id_framing_is_unambiguous() {
        // The NUL framing must distinguish these two (which would collide
        // under naive string concatenation):
        let x = surface_id(SurfaceKind::CliVerb, "a", "bc");
        let y = surface_id(SurfaceKind::CliVerb, "ab", "c");
        assert_ne!(x, y);
        let x2 = surface_id(SurfaceKind::CliVerb, "a", "b");
        let y2 = surface_id(SurfaceKind::CliFlag, "a", "b");
        assert_ne!(x2, y2);
    }

    #[test]
    fn kind_parse_round_trips_and_rejects_unknown() {
        for kind in SurfaceKind::ALL {
            assert_eq!(SurfaceKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(SurfaceKind::parse("nope"), None);
        assert_eq!(SurfaceKind::parse(""), None);
    }

    #[test]
    fn census_gap_is_detected_zero() {
        let gap = KindCensus {
            kind: SurfaceKind::FfiExport,
            detected: 0,
            expected: 0,
            rule: "not found".into(),
        };
        assert!(gap.is_gap());
        let ok = KindCensus {
            kind: SurfaceKind::McpTool,
            detected: 17,
            expected: 17,
            rule: "registry".into(),
        };
        assert!(!ok.is_gap());
    }

    #[test]
    fn scope_serializes_self_and_foreign_and_defaults_to_self() {
        assert_eq!(
            serde_json::to_string(&SurfaceScope::SelfHosted).unwrap(),
            "\"self\""
        );
        assert_eq!(
            serde_json::to_string(&SurfaceScope::Foreign).unwrap(),
            "\"foreign\""
        );
        assert_eq!(SurfaceScope::default(), SurfaceScope::SelfHosted);
        assert!(SurfaceScope::SelfHosted.is_self());
        assert!(!SurfaceScope::Foreign.is_self());
        assert_eq!(SurfaceScope::SelfHosted.as_str(), "self");
        assert_eq!(SurfaceScope::Foreign.as_str(), "foreign");
    }

    #[test]
    fn inventory_round_trips_through_json() {
        let inv = SurfaceInventory {
            schema: SurfaceInventory::SCHEMA,
            scope: SurfaceScope::Foreign,
            surfaces: vec![sample()],
            census: vec![KindCensus {
                kind: SurfaceKind::McpTool,
                detected: 1,
                expected: 1,
                rule: "registry".into(),
            }],
        };
        let json = serde_json::to_string(&inv).unwrap();
        let back: SurfaceInventory = serde_json::from_str(&json).unwrap();
        assert_eq!(inv, back);
        assert_eq!(back.scope, SurfaceScope::Foreign);
    }

    #[test]
    fn append_surfaces_dedupes_by_id_and_creates_absent_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("graph").join("surfaces.jsonl");

        // Empty append materializes the file.
        assert_eq!(append_surfaces_deduped(&path, &[]).unwrap(), 0);
        assert!(path.exists());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");

        // One real surface, appended once.
        let s = sample();
        assert_eq!(
            append_surfaces_deduped(&path, std::slice::from_ref(&s)).unwrap(),
            1
        );
        // Idempotent: same identity appends nothing.
        assert_eq!(
            append_surfaces_deduped(&path, std::slice::from_ref(&s)).unwrap(),
            0
        );
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents.lines().count(), 1, "no duplicate rows: {contents}");
        let row: Surface = serde_json::from_str(contents.lines().next().unwrap()).unwrap();
        assert_eq!(row.surface_id, s.surface_id);
    }
}
