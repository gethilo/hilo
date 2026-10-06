//! Surface enumeration — derive hilo's externally-visible contract points
//! from code (COV-1).
//!
//! Seven kinds, each with a code-derived provider:
//!
//! | kind              | provider                                                       |
//! |-------------------|----------------------------------------------------------------|
//! | `cli_verb`        | clap `CommandFactory` introspection of `hilo-cli/src/cli.rs`   |
//! | `cli_flag`        | same — the `--long` / `-s` args each subcommand registers       |
//! | `mcp_tool`        | `hilo_mcp::tools::list_tools()` — the server's own registry     |
//! | `ffi_export`      | `vfs_*` declarations in `hilo-ffi/src/hilo.udl`                |
//! | `fuse_op`         | `fn` methods of `impl Filesystem for Hilo` in `hilo-fuse/src/ops.rs` |
//! | `config_key`      | top-level `pub` fields of `hilo_core::manifest::Manifest`       |
//! | `public_api_item` | `pub use` re-exports of `hilo-graph/src/lib.rs`                 |
//!
//! The first three are compiled in (in-memory registries); the last four are
//! parsed from the repo's own source files so they are *derived from code*,
//! never hand-written. Every provider reports a [`KindCensus`] —
//! `detected` vs `expected` plus the rule that produced it — so a parser that
//! finds nothing reads as a gap, never as a silent empty success.
//!
//! ## Scope (GAP-112)
//!
//! The compiled providers run **unconditionally**: they describe the `hilo`
//! binary that is executing, not the tree it is pointed at. On a repository
//! that is not Hilo that self-data is not the target's — presenting it as the
//! target's surfaces is a confident wrong answer. So before anything is
//! enumerated the repo is classified ([`detect_scope`]): a tree carrying the
//! Hilo workspace markers (`self`) keeps the full inventory unchanged, and
//! any other tree (`foreign`) gets an empty inventory whose `scope` marker is
//! the entire result — no Hilo-owned row is ever presented as target output,
//! and nothing is written to a foreign tree.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};
use clap::CommandFactory;

use crate::cli::Cli;
use hilo_graph::surfaces::{KindCensus, Surface, SurfaceInventory, SurfaceKind, SurfaceScope};

/// The source file that owns the CLI surface (the clap derive types).
const CLI_OWNER: &str = "hilo-cli/src/cli.rs";
/// The source file that owns the MCP surface (the tool registry).
const MCP_OWNER: &str = "hilo-mcp/src/tools/mod.rs";
/// The library crate root that owns the `public_api_item` surface — the third
/// file (with the two owner constants above) that marks a Hilo workspace.
const GRAPH_OWNER: &str = "hilo-graph/src/lib.rs";

/// Files whose presence identifies a Hilo workspace root.
///
/// All three are provider-owned paths: without them the compiled
/// `cli_verb`/`cli_flag`/`mcp_tool` rows (which the compiled registries emit
/// regardless of the tree) would describe Hilo, not the target (GAP-112).
const SELF_MARKERS: [&str; 3] = [CLI_OWNER, MCP_OWNER, GRAPH_OWNER];

/// True when `dir` carries every Hilo workspace marker.
fn is_hilo_workspace_root(dir: &Path) -> bool {
    SELF_MARKERS.iter().all(|marker| dir.join(marker).is_file())
}

/// Classify the repository containing `start`: `SelfHosted` when a Hilo
/// workspace root is `start` or one of its ancestors, `Foreign` otherwise.
///
/// The search walks **up** the way `cargo` finds `Cargo.toml`, so
/// `graph surfaces` run from a subdirectory of a Hilo checkout still reads as
/// Hilo. It stops at the first repository root (a `.git` entry) that is not
/// Hilo, so a foreign checkout nested under a Hilo tree classifies as foreign
/// rather than inheriting the ancestor's identity.
pub fn detect_scope(start: &Path) -> SurfaceScope {
    let mut dir = Some(start);
    while let Some(d) = dir {
        if is_hilo_workspace_root(d) {
            return SurfaceScope::SelfHosted;
        }
        if d.join(".git").exists() {
            return SurfaceScope::Foreign;
        }
        dir = d.parent();
    }
    SurfaceScope::Foreign
}

/// The empty, self-only inventory a foreign repository gets (GAP-112).
fn foreign_inventory() -> SurfaceInventory {
    SurfaceInventory {
        schema: SurfaceInventory::SCHEMA,
        scope: SurfaceScope::Foreign,
        surfaces: Vec::new(),
        census: Vec::new(),
    }
}

/// Enumerate every surface kind, writing nothing. `root` is the repo root the
/// source-derived providers (ffi/fuse/config/public_api) resolve their files
/// against.
///
/// On a foreign repository this returns [`foreign_inventory`] — the scope
/// marker with zero rows — instead of Hilo's own compiled inventory.
pub fn enumerate(root: &Path) -> SurfaceInventory {
    // GAP-112: decide scope before enumerating anything. The compiled
    // providers cannot tell Hilo's registries from the target's, so without
    // this guard a foreign repo would receive Hilo's own rows as its result.
    if !detect_scope(root).is_self() {
        return foreign_inventory();
    }

    let cli_cmd = Cli::command();

    let (cli_verbs, cli_flags) = collect_cli(&cli_cmd);
    let registered_verbs = count_subcommands(&cli_cmd);
    let registered_flags = count_flags(&cli_cmd);

    let (mcp_surfaces, mcp_census) = mcp_provider();
    let (ffi_surfaces, ffi_census) = ffi_provider(root);
    let (fuse_surfaces, fuse_census) = fuse_provider(root);
    let (config_surfaces, config_census) = config_provider(root);
    let (api_surfaces, api_census) = public_api_provider(root);

    let mut surfaces = Vec::new();
    surfaces.extend(cli_verbs);
    surfaces.extend(cli_flags);
    surfaces.extend(mcp_surfaces);
    surfaces.extend(ffi_surfaces);
    surfaces.extend(fuse_surfaces);
    surfaces.extend(config_surfaces);
    surfaces.extend(api_surfaces);
    // Deterministic order: kind, then stable surface_id — so the JSONL rows
    // and JSON output are byte-stable across runs.
    surfaces
        .sort_by(|a, b| (a.kind.as_str(), &a.surface_id).cmp(&(b.kind.as_str(), &b.surface_id)));

    let census = vec![
        KindCensus {
            kind: SurfaceKind::CliVerb,
            detected: surfaces.iter().filter(|s| s.kind == SurfaceKind::CliVerb).count(),
            expected: registered_verbs,
            rule: format!(
                "clap CommandFactory introspection of {CLI_OWNER} ({registered_verbs} subcommands registered)"
            ),
        },
        KindCensus {
            kind: SurfaceKind::CliFlag,
            detected: surfaces.iter().filter(|s| s.kind == SurfaceKind::CliFlag).count(),
            expected: registered_flags,
            rule: format!(
                "clap CommandFactory introspection of {CLI_OWNER} ({registered_flags} flags registered)"
            ),
        },
        mcp_census,
        ffi_census,
        fuse_census,
        config_census,
        api_census,
    ];

    SurfaceInventory {
        schema: SurfaceInventory::SCHEMA,
        scope: SurfaceScope::SelfHosted,
        surfaces,
        census,
    }
}

/// The `hilo graph surfaces` command: classify the scope, enumerate (only on
/// the Hilo repo itself), append to `.vfs/graph/surfaces.jsonl` (deduped by
/// `surface_id`), and print the per-kind census plus the rows in text or JSON.
///
/// On a foreign repository the command declares its self-only scope and
/// refuses: it prints the `scope: foreign` marker, writes **nothing**, and
/// exits non-zero, so a caller can never read Hilo self-data as the target's
/// surfaces and never gets a success-shaped empty report (GAP-112).
pub fn run(json: bool, kind: Option<&str>) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to determine the current directory")?;

    // A `--kind` typo is a usage error regardless of scope, so validate it
    // before the scope check below can short-circuit.
    let kind_filter = match kind {
        Some(filter) => Some(SurfaceKind::parse(filter).ok_or_else(|| {
            let valid = SurfaceKind::ALL
                .iter()
                .map(|k| k.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::anyhow!("unknown surface kind '{filter}'. Valid kinds: {valid}")
        })?),
        None => None,
    };

    let mut inventory = enumerate(&cwd);

    if !inventory.scope.is_self() {
        report_foreign_scope(&cwd, json);
        anyhow::bail!(
            "hilo graph surfaces only inventories the Hilo repository itself; {} is not a Hilo workspace",
            cwd.display()
        );
    }

    if let Some(parsed) = kind_filter {
        inventory.surfaces.retain(|s| s.kind == parsed);
        inventory.census.retain(|c| c.kind == parsed);
    }

    let path = cwd.join(".vfs").join("graph").join("surfaces.jsonl");
    let appended = hilo_graph::surfaces::append_surfaces_deduped(&path, &inventory.surfaces)
        .context("failed to write surfaces.jsonl")?;

    if json {
        let out = serde_json::to_string_pretty(&inventory)
            .context("failed to serialize surface inventory as JSON")?;
        println!("{out}");
    } else {
        for census in &inventory.census {
            if census.is_gap() {
                println!(
                    "{}: {} detected of {} expected — GAP: {}",
                    census.kind.as_str(),
                    census.detected,
                    census.expected,
                    census.rule
                );
            } else {
                println!(
                    "{}: {} detected of {} expected ({})",
                    census.kind.as_str(),
                    census.detected,
                    census.expected,
                    census.rule
                );
            }
        }
        println!();
        for surface in &inventory.surfaces {
            println!(
                "{}  {}  {}  [{}]",
                surface.kind.as_str(),
                surface.surface_id,
                surface.name,
                if surface.public { "public" } else { "internal" }
            );
        }
    }

    if appended > 0 {
        eprintln!("appended {appended} surface row(s) to {}", path.display());
    }
    Ok(())
}

/// Print the foreign-repo scope declaration — the explicit self-only result
/// (GAP-112). `--json` callers get the machine-readable inventory with an
/// empty `surfaces`/`census` and `scope: "foreign"`; text callers get the
/// same statement in words. Nothing is written to disk.
fn report_foreign_scope(root: &Path, json: bool) {
    let inventory = foreign_inventory();
    if json {
        match serde_json::to_string_pretty(&inventory) {
            Ok(out) => println!("{out}"),
            Err(e) => eprintln!("error: failed to serialize the foreign-scope marker: {e}"),
        }
    } else {
        println!(
            "scope: foreign — {} is not a Hilo workspace",
            root.display()
        );
        println!(
            "hilo graph surfaces inventories the Hilo repository's own contract surfaces only."
        );
        println!("No target-repo surfaces were derived and nothing was written to .vfs/graph/.");
    }
}

// ─────────────────────────── CLI (clap) ───────────────────────────

/// Walk the clap command tree, emitting one `cli_verb` surface per registered
/// subcommand and one `cli_flag` surface per registered flag/option.
fn collect_cli(cmd: &clap::Command) -> (Vec<Surface>, Vec<Surface>) {
    let mut verbs = Vec::new();
    let mut flags = Vec::new();
    collect_cli_node(cmd, "", &mut verbs, &mut flags);
    (verbs, flags)
}

fn collect_cli_node(
    cmd: &clap::Command,
    path: &str,
    verbs: &mut Vec<Surface>,
    flags: &mut Vec<Surface>,
) {
    // Flags/options registered on THIS command node. The owner is the verb
    // path that owns the flag (the root command's own args use "<hilo>").
    for arg in cmd.get_arguments() {
        if let Some(flag) = flag_name(arg) {
            let owner = if path.is_empty() {
                "<hilo>".to_string()
            } else {
                path.to_string()
            };
            flags.push(Surface::new(
                SurfaceKind::CliFlag,
                CLI_OWNER,
                format!("{owner}::{flag}"),
                flag,
                true,
            ));
        }
    }
    for sub in cmd.get_subcommands() {
        let name = sub.get_name().to_string();
        let full = if path.is_empty() {
            name.clone()
        } else {
            format!("{path} {name}")
        };
        verbs.push(Surface::new(
            SurfaceKind::CliVerb,
            CLI_OWNER,
            full.clone(),
            full.clone(),
            true,
        ));
        collect_cli_node(sub, &full, verbs, flags);
    }
}

/// The rendered flag form of a clap arg (`--long` preferred, else `-s`).
/// Positional args (no long/short) yield `None` — they are not flags.
fn flag_name(arg: &clap::Arg) -> Option<String> {
    arg.get_long()
        .map(|l| format!("--{l}"))
        .or_else(|| arg.get_short().map(|s| format!("-{s}")))
}

/// Independently count registered subcommands (the "expected" denominator).
fn count_subcommands(cmd: &clap::Command) -> usize {
    cmd.get_subcommands().count() + cmd.get_subcommands().map(count_subcommands).sum::<usize>()
}

/// Independently count registered flags (the "expected" denominator).
fn count_flags(cmd: &clap::Command) -> usize {
    cmd.get_arguments()
        .filter(|a| flag_name(a).is_some())
        .count()
        + cmd.get_subcommands().map(count_flags).sum::<usize>()
}

// ─────────────────────────── MCP ───────────────────────────

fn mcp_provider() -> (Vec<Surface>, KindCensus) {
    let tools = hilo_mcp::tools::list_tools();
    let surfaces: Vec<Surface> = tools
        .iter()
        .map(|tool| {
            Surface::new(
                SurfaceKind::McpTool,
                MCP_OWNER,
                tool.name.clone(),
                tool.name.clone(),
                true,
            )
        })
        .collect();
    let n = surfaces.len();
    let census = KindCensus {
        kind: SurfaceKind::McpTool,
        detected: n,
        expected: n,
        rule: format!("hilo_mcp::tools::list_tools() registry ({n} tools)"),
    };
    (surfaces, census)
}

// ─────────────────── source-derived providers ───────────────────

/// FFI exports: the `vfs_*` function/method declarations in the UniFFI UDL.
/// The UDL is the source of truth that drives `uniffi-bindgen` codegen, so
/// parsing it is "derived from code", not a hand list.
fn ffi_provider(root: &Path) -> (Vec<Surface>, KindCensus) {
    const FILE: &str = "hilo-ffi/src/hilo.udl";
    let text = match std::fs::read_to_string(root.join(FILE)) {
        Ok(t) => t,
        Err(e) => {
            return (
                Vec::new(),
                KindCensus {
                    kind: SurfaceKind::FfiExport,
                    detected: 0,
                    expected: 0,
                    rule: format!("{FILE} not found — no ffi_export enumeration ({e})"),
                },
            )
        }
    };
    let mut names = BTreeSet::new();
    for line in text.lines() {
        let trimmed = line.trim();
        // A declaration names a `vfs_*` function/method immediately before
        // its `(`. Skip `namespace`/`interface`/`[Throws=...]` lines, which
        // carry no `(` and no `vfs_` token before one.
        if !trimmed.contains("vfs_") || !trimmed.contains('(') {
            continue;
        }
        if let Some(open) = trimmed.find('(') {
            let before = &trimmed[..open];
            if let Some(name) = before
                .split_whitespace()
                .find(|w| w.starts_with("vfs_"))
                .map(|w| w.trim_end_matches(';'))
            {
                names.insert(name.to_string());
            }
        }
    }
    let n = names.len();
    let surfaces = names
        .iter()
        .map(|name| {
            Surface::new(
                SurfaceKind::FfiExport,
                FILE,
                name.clone(),
                name.clone(),
                true,
            )
        })
        .collect();
    let census = KindCensus {
        kind: SurfaceKind::FfiExport,
        detected: n,
        expected: n,
        rule: format!("vfs_* declarations parsed from {FILE} ({n} exports)"),
    };
    (surfaces, census)
}

/// FUSE ops: the `fuser::Filesystem` trait methods hilo implements.
/// Extracted from the `impl Filesystem for Hilo` block, so the enumeration
/// tracks the actual implemented surface rather than a hand list.
fn fuse_provider(root: &Path) -> (Vec<Surface>, KindCensus) {
    const FILE: &str = "hilo-fuse/src/ops.rs";
    let text = match std::fs::read_to_string(root.join(FILE)) {
        Ok(t) => t,
        Err(e) => {
            return (
                Vec::new(),
                KindCensus {
                    kind: SurfaceKind::FuseOp,
                    detected: 0,
                    expected: 0,
                    rule: format!("{FILE} not found — no fuse_op enumeration ({e})"),
                },
            )
        }
    };
    let mut names = BTreeSet::new();
    let mut in_impl = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("impl Filesystem for Hilo") {
            in_impl = true;
            continue;
        }
        if !in_impl {
            continue;
        }
        // The impl block ends at the next `impl` or a column-0 `}`.
        if trimmed.starts_with("impl ") || line.starts_with('}') {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("fn ") {
            if let Some(name) = rest.split('(').next().map(str::trim) {
                if !name.is_empty() {
                    names.insert(name.to_string());
                }
            }
        }
    }
    let n = names.len();
    let surfaces = names
        .iter()
        .map(|name| Surface::new(SurfaceKind::FuseOp, FILE, name.clone(), name.clone(), true))
        .collect();
    let census = KindCensus {
        kind: SurfaceKind::FuseOp,
        detected: n,
        expected: n,
        rule: format!("fuser::Filesystem methods parsed from {FILE} ({n} ops)"),
    };
    (surfaces, census)
}

/// Config keys: the top-level manifest keys, derived from the `Manifest`
/// struct's own `pub` fields (the serde schema every manifest is parsed
/// against).
fn config_provider(root: &Path) -> (Vec<Surface>, KindCensus) {
    const FILE: &str = "hilo-core/src/manifest.rs";
    let text = match std::fs::read_to_string(root.join(FILE)) {
        Ok(t) => t,
        Err(e) => {
            return (
                Vec::new(),
                KindCensus {
                    kind: SurfaceKind::ConfigKey,
                    detected: 0,
                    expected: 0,
                    rule: format!("{FILE} not found — no config_key enumeration ({e})"),
                },
            )
        }
    };
    let mut names = BTreeSet::new();
    let mut in_struct = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("pub struct Manifest") {
            in_struct = true;
            continue;
        }
        if !in_struct {
            continue;
        }
        if line.starts_with('}') {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("pub ") {
            if let Some(field) = rest.split(':').next().map(str::trim) {
                if !field.is_empty() {
                    names.insert(field.to_string());
                }
            }
        }
    }
    let n = names.len();
    let surfaces = names
        .iter()
        .map(|name| {
            Surface::new(
                SurfaceKind::ConfigKey,
                FILE,
                name.clone(),
                name.clone(),
                true,
            )
        })
        .collect();
    let census = KindCensus {
        kind: SurfaceKind::ConfigKey,
        detected: n,
        expected: n,
        rule: format!(
            "top-level pub fields of hilo_core::manifest::Manifest parsed from {FILE} ({n} keys)"
        ),
    };
    (surfaces, census)
}

/// Public API items: the `pub use` re-exports of the graph engine's library
/// crate root — the items downstream consumers can actually name.
fn public_api_provider(root: &Path) -> (Vec<Surface>, KindCensus) {
    const FILE: &str = "hilo-graph/src/lib.rs";
    let text = match std::fs::read_to_string(root.join(FILE)) {
        Ok(t) => t,
        Err(e) => {
            return (
                Vec::new(),
                KindCensus {
                    kind: SurfaceKind::PublicApiItem,
                    detected: 0,
                    expected: 0,
                    rule: format!("{FILE} not found — no public_api_item enumeration ({e})"),
                },
            )
        }
    };
    let names = public_api_names(&text);
    let n = names.len();
    let surfaces = names
        .iter()
        .map(|name| {
            Surface::new(
                SurfaceKind::PublicApiItem,
                FILE,
                name.clone(),
                name.clone(),
                true,
            )
        })
        .collect();
    let census = KindCensus {
        kind: SurfaceKind::PublicApiItem,
        detected: n,
        expected: n,
        rule: format!("pub use re-exports parsed from {FILE} ({n} items)"),
    };
    (surfaces, census)
}

/// Extract every re-exported name from a `lib.rs`-style source. Handles
/// multi-line brace groups (`pub use x::{a, b, c};`), aliases
/// (`pub use x::y as z;`), and single paths (`pub use x::y;`).
fn public_api_names(text: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut rest = text;
    while let Some(idx) = rest.find("pub use ") {
        let after = &rest[idx + "pub use ".len()..];
        let Some(semi) = after.find(';') else {
            break;
        };
        let stmt = &after[..semi];
        parse_pub_use_stmt(stmt, &mut names);
        rest = &after[semi + 1..];
    }
    names
}

fn parse_pub_use_stmt(stmt: &str, names: &mut BTreeSet<String>) {
    let stmt = stmt.trim();
    if let Some(open) = stmt.find('{') {
        let close = stmt.rfind('}').unwrap_or(stmt.len());
        let inner = &stmt[open + 1..close];
        for item in inner.split(',') {
            let name = item.trim();
            if name.is_empty() {
                continue;
            }
            // `x as y` inside a brace group re-exports as `y`.
            let name = name.rsplit(" as ").next().unwrap_or(name).trim();
            names.insert(name.to_string());
        }
    } else if let Some(as_pos) = stmt.find(" as ") {
        names.insert(stmt[as_pos + 4..].trim().to_string());
    } else if let Some(name) = stmt.rsplit("::").next() {
        let name = name.trim();
        if !name.is_empty() {
            names.insert(name.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The workspace root: `CARGO_MANIFEST_DIR` is `…/hilo-cli`, whose parent
    /// is the workspace root. Lets the tests enumerate the REAL repo sources.
    fn repo_root() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("hilo-cli must live directly under the workspace root")
    }

    #[test]
    fn mcp_enumerates_exactly_the_registry() {
        let (surfaces, census) = mcp_provider();
        let registry = hilo_mcp::tools::list_tools();
        assert_eq!(surfaces.len(), 17, "17 registered MCP tools");
        assert_eq!(census.detected, 17);
        assert_eq!(census.expected, 17);
        let mut got: Vec<&str> = surfaces.iter().map(|s| s.name.as_str()).collect();
        let mut want: Vec<&str> = registry.iter().map(|t| t.name.as_str()).collect();
        got.sort_unstable();
        want.sort_unstable();
        assert_eq!(
            got, want,
            "enumeration must match the server's own registry"
        );
    }

    #[test]
    fn cli_enumerates_a_sample_of_verbs_and_is_honest() {
        let cmd = Cli::command();
        let (verbs, flags) = collect_cli(&cmd);
        assert!(!verbs.is_empty(), "clap introspection found no verbs");
        let verb_names: Vec<&str> = verbs.iter().map(|s| s.name.as_str()).collect();
        for expected in [
            "init",
            "meta",
            "serve",
            "graph",
            "graph warm",
            "graph stats",
            "graph surfaces",
            "graph related",
            "backend",
            "workspace",
            "classify",
        ] {
            assert!(
                verb_names.contains(&expected),
                "missing clap verb '{expected}'; got {verb_names:?}"
            );
        }
        assert_eq!(
            verbs.len(),
            count_subcommands(&cmd),
            "detected verbs must equal registered subcommands"
        );
        assert!(!flags.is_empty(), "clap introspection found no flags");
        assert_eq!(
            flags.len(),
            count_flags(&cmd),
            "detected flags must equal registered flags"
        );
    }

    #[test]
    fn source_derived_kinds_enumerate_the_real_repo() {
        let root = repo_root();
        let inv = enumerate(root);

        let ffi = inv
            .surfaces
            .iter()
            .filter(|s| s.kind == SurfaceKind::FfiExport)
            .count();
        let fuse = inv
            .surfaces
            .iter()
            .filter(|s| s.kind == SurfaceKind::FuseOp)
            .count();
        let config = inv
            .surfaces
            .iter()
            .filter(|s| s.kind == SurfaceKind::ConfigKey)
            .count();
        let api = inv
            .surfaces
            .iter()
            .filter(|s| s.kind == SurfaceKind::PublicApiItem)
            .count();

        assert_eq!(ffi, 8, "hilo.udl declares 8 vfs_* exports");
        assert_eq!(fuse, 10, "impl Filesystem for Hilo implements 10 ops");
        assert_eq!(config, 14, "Manifest has 14 top-level keys");
        assert!(api > 0, "hilo-graph/src/lib.rs must re-export public items");

        // No silent empty success anywhere: every kind must report a census row.
        assert_eq!(inv.census.len(), SurfaceKind::ALL.len());
        for census in &inv.census {
            assert_eq!(
                census.detected,
                census.expected,
                "kind {} detected/expected must agree (both derived): {census:?}",
                census.kind.as_str()
            );
            assert!(!census.rule.is_empty(), "every census names its rule");
        }
    }

    #[test]
    fn public_api_names_handles_braces_alias_and_single_path() {
        let src = "pub use classify::{a, b, c};\npub use semantic::tokenize as semantic_tokenize;\npub use hilo_metadata::inventory::Edge;\npub use serde_json;\n";
        let names = public_api_names(src);
        for expected in ["a", "b", "c", "semantic_tokenize", "Edge", "serde_json"] {
            assert!(
                names.contains(expected),
                "missing '{expected}' in {names:?}"
            );
        }
        // A multi-line brace group.
        let multi = "pub use graph::{\n    alpha, beta,\n    gamma,\n};\n";
        let names = public_api_names(multi);
        assert!(names.contains("alpha") && names.contains("beta") && names.contains("gamma"));
    }

    // ─────────────── scope: Hilo-self vs foreign (GAP-112) ───────────────

    /// Build a throwaway tree carrying every Hilo workspace marker so it
    /// classifies as `self` without being the real repository.
    fn hilo_marker_tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        for marker in SELF_MARKERS {
            let path = dir.path().join(marker);
            std::fs::create_dir_all(path.parent().unwrap()).expect("create marker dir");
            std::fs::write(&path, "").expect("write marker");
        }
        dir
    }

    #[test]
    fn detect_scope_classifies_the_hilo_repo_and_a_foreign_tree() {
        // Self: the real repository root.
        assert_eq!(detect_scope(repo_root()), SurfaceScope::SelfHosted);

        // A plain subdirectory of a Hilo checkout still reads as Hilo — the
        // search walks up the way `cargo` finds `Cargo.toml`.
        let hilo = hilo_marker_tree();
        let sub = hilo.path().join("hilo-cli");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(detect_scope(&sub), SurfaceScope::SelfHosted);

        // Foreign: a tree with sources of its own and no Hilo markers.
        let foreign = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(foreign.path().join("src")).unwrap();
        std::fs::write(foreign.path().join("src/main.go"), "package main\n").unwrap();
        assert_eq!(detect_scope(foreign.path()), SurfaceScope::Foreign);

        // A foreign checkout nested under a Hilo tree is still foreign: the
        // `.git` stop beats the ancestor's markers.
        let nested = hilo.path().join("bat");
        std::fs::create_dir_all(nested.join(".git")).unwrap();
        assert_eq!(detect_scope(&nested), SurfaceScope::Foreign);
    }

    #[test]
    fn a_tree_carrying_the_hilo_markers_is_self_scope() {
        let dir = hilo_marker_tree();
        assert_eq!(detect_scope(dir.path()), SurfaceScope::SelfHosted);

        let inv = enumerate(dir.path());
        assert_eq!(inv.scope, SurfaceScope::SelfHosted);
        // The compiled registries do not read the tree, so a self tree still
        // yields its surface rows; the source-derived kinds honestly report
        // their gaps rather than disappearing.
        assert!(
            !inv.surfaces.is_empty(),
            "a self tree must still yield the compiled surface rows"
        );
        assert_eq!(inv.census.len(), SurfaceKind::ALL.len());
        assert_eq!(serde_json::to_value(&inv).unwrap()["scope"], "self");
    }

    #[test]
    fn enumerate_refuses_to_emit_hilo_rows_on_a_foreign_tree() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("README.md"), "# bat\n").unwrap();

        let inv = enumerate(dir.path());
        assert_eq!(inv.scope, SurfaceScope::Foreign);
        assert!(
            inv.surfaces.is_empty(),
            "no Hilo-owned row may be presented as a foreign repo's result: {:?}",
            inv.surfaces
        );
        assert!(inv.census.is_empty());

        // The machine-readable result carries the scope marker explicitly,
        // with no rows a caller could mistake for target coverage.
        let value = serde_json::to_value(&inv).unwrap();
        assert_eq!(value["scope"], "foreign");
        assert_eq!(value["surfaces"].as_array().unwrap().len(), 0);
        assert_eq!(value["census"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn enumerate_preserves_the_hilo_self_inventory_on_the_real_repo() {
        let inv = enumerate(repo_root());
        assert_eq!(inv.scope, SurfaceScope::SelfHosted);
        // The self-audit is unchanged: every kind reports a census, and the
        // compiled kinds are present.
        assert_eq!(inv.census.len(), SurfaceKind::ALL.len());
        assert!(inv.surfaces.iter().any(|s| s.kind == SurfaceKind::McpTool));
        assert!(inv.surfaces.iter().any(|s| s.kind == SurfaceKind::CliVerb));
        assert_eq!(serde_json::to_value(&inv).unwrap()["scope"], "self");
    }

    #[test]
    fn json_shape_is_locked() {
        let inv = enumerate(repo_root());
        let value = serde_json::to_value(&inv).unwrap();
        let obj = value.as_object().unwrap();
        assert_eq!(obj.get("schema").and_then(|v| v.as_u64()), Some(1));
        assert_eq!(obj.get("scope").and_then(|v| v.as_str()), Some("self"));
        assert!(obj.contains_key("surfaces"));
        assert!(obj.contains_key("census"));
        // Each surface row carries exactly the six contract fields.
        let surfaces = obj["surfaces"].as_array().unwrap();
        assert!(!surfaces.is_empty());
        let row = surfaces[0].as_object().unwrap();
        for key in [
            "surface_id",
            "kind",
            "owner_file",
            "owner_symbol",
            "name",
            "public",
        ] {
            assert!(
                row.contains_key(key),
                "surface row missing '{key}': {row:?}"
            );
        }
        // Each census row carries kind/detected/expected/rule.
        let census = obj["census"].as_array().unwrap();
        assert!(!census.is_empty());
        let crow = census[0].as_object().unwrap();
        for key in ["kind", "detected", "expected", "rule"] {
            assert!(
                crow.contains_key(key),
                "census row missing '{key}': {crow:?}"
            );
        }
    }
}
