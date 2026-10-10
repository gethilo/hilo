//! The `hilo` command-line interface definition.
//!
//! The clap derive types live in the library (not the binary) so the surface
//! enumeration can introspect them via [`Cli::command()`] (clap's
//! `CommandFactory`) and derive `cli_verb` / `cli_flag` surfaces from the
//! actual registered command tree — never from a hand-written list (COV-1).
//!
//! The binary (`src/main.rs`) is a thin wrapper that restores the default
//! `SIGPIPE` disposition and calls [`run`].

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::commands::plugin::PluginCommand;
use crate::commands::{
    audit, backend, classify, coverage_links, graph, ignore, init, meta, mount, plugin, rollup,
    serve,
    surfaces, test_classes, wiring, workspace,
};
use crate::sync_direction;

/// Hilo command-line interface.
/// Provenance for `hilo --version` (item 47): the crate number alone made an
/// 11-day-old artifact indistinguishable from a HEAD build, so a fleet could
/// test the wrong binary. The extra lines are baked by hilo-cli/build.rs
/// (git describe + UTC build time; "unknown" outside a git checkout).
#[derive(Parser)]
#[command(name = "hilo", about = "Hilo CLI", version)]
#[command(
    long_version = concat!(
        env!("CARGO_PKG_VERSION"),
        "\nbuild: ",
        env!("HILO_BUILD_DESCRIBE"),
        "\nbuilt: ",
        env!("HILO_BUILD_TIME_UTC")
    )
)]
pub struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize a Hilo project in the current directory.
    Init(InitArgs),
    /// Show Hilo extended attributes for a file.
    Meta(MetaArgs),
    /// Dependency-graph discovery, statistics, and impact analysis.
    #[command(subcommand)]
    Graph(GraphCommand),
    /// Run a Hilo server (MCP stdio transport). Exposes 18 vfs_* tools — see README.
    Serve(ServeArgs),
    /// Manage virtual backends (S3, gdrive, onedrive, dropbox, external).
    #[command(subcommand)]
    Backend(backend::BackendCommand),
    /// Mount a Hilo virtual filesystem via FUSE.
    Mount(MountArgs),
    /// Manage multi-repo workspace mounts.
    #[command(subcommand)]
    Workspace(WorkspaceCommand),
    /// Auto-classify files with role/status metadata (entrypoint, test, library, etc.).
    Classify(ClassifyArgs),
    /// Check whether a path is ignored by the workspace ignore file.
    #[command(subcommand)]
    Ignore(ignore::IgnoreCommand),
    /// Load and manage wasm plugins.
    #[command(subcommand)]
    Plugin(PluginCommand),
}

#[derive(clap::Args)]
struct MountArgs {
    /// Directory to mount the filesystem at.
    mount_point: String,
    /// Enable trigger engine (file watchers).
    #[arg(long)]
    triggers: bool,
    /// Allow other users to access the mount.
    #[arg(long)]
    allow_other: bool,
    /// Run in the background: detach from the terminal and return immediately.
    #[arg(long)]
    daemon: bool,
}

#[derive(clap::Args)]
struct InitArgs {
    /// Allow HOME (or its canonical equivalent) as the project root.
    /// Without this flag `hilo init` refuses to create `.vfs/` inside HOME.
    #[arg(long)]
    allow_home: bool,

    /// Do not install git hooks.
    ///
    /// Normal `hilo init` appends a marked `### HILO` block to
    /// `.git/hooks/post-commit` and `.git/hooks/post-merge` (existing hook
    /// content is preserved). With this flag nothing under `.git/hooks/` is
    /// created or modified, so hook setup can stay owned by other tooling.
    #[arg(long)]
    no_hooks: bool,
}

#[derive(clap::Args)]
struct MetaArgs {
    /// The file whose Hilo metadata to inspect or set.
    path: String,

    /// Set a Hilo extended attribute (e.g. `user.vfs.feature`).
    #[arg(long)]
    set: Option<String>,

    /// Value for --set. Accepts literal `\n` for multiline values.
    #[arg(long, requires = "set")]
    value: Option<String>,
}

#[derive(Subcommand)]
enum GraphCommand {
    /// Pre-compute the dependency graph by parsing all source files.
    ///
    /// Optional batch warmup for CI or power users. Queries (`related`,
    /// `impact`) are JIT — they auto-parse files on first access and do
    /// NOT require `warm` first.
    ///
    /// Discovery prunes dependency, cache, vendor, and hidden paths by
    /// default. Override a pruning decision with `graph.include_paths` in
    /// `.vfs/manifest.yaml` or `manifest.yaml`. `.hiloignore` controls backend
    /// sync and does not override graph discovery.
    ///
    /// Requires a Hilo project root: run `hilo init` first if this directory
    /// has no manifest. Without one, warm exits non-zero naming that command
    /// instead of leaving a partial `.vfs/graph/` behind.
    #[clap(alias = "discover")]
    Warm(WarmArgs),
    /// Print summary statistics from the dependency graph.
    Stats(StatsArgs),
    /// Query graph edges for a specific file (auto-parses on first access).
    Related(RelatedArgs),
    /// Find all files that transitively depend on a given file (impact analysis).
    Impact(ImpactArgs),
    /// Multi-resolution harmonic context output for a natural-language task.
    Understand(UnderstandArgs),
    /// Deterministic semantic code search (TF-IDF + BM25).
    Search(SearchArgs),
    /// Per-module statistics and test coverage.
    Module(ModuleArgs),
    /// List source files with no test coverage.
    Untested,
    /// List all rules defined in the manifest.
    RuleList,
    /// Execute a named rule query against the dependency graph.
    RuleCheck(RuleCheckArgs),
    /// Delete the cached dependency graph (edges.jsonl + DuckDB cache) so
    /// the next `graph warm` re-parses every source file from scratch.
    Clean,
    /// Enumerate the codebase's externally-visible contract surfaces — CLI
    /// verbs/flags, MCP tools, FFI exports, FUSE ops, config keys, and public
    /// API items — and write them to `.vfs/graph/surfaces.jsonl` (COV-1).
    Surfaces(SurfacesArgs),
    /// Derive evidenced test→surface coverage links (COV-2): one row per
    /// (test_file, target) with evidence kind, confidence, and direction,
    /// written to `.vfs/graph/coverage_links.jsonl`. `--unlinked` lists
    /// surfaces with no link at all, each with a cause.
    CoverageLinks(CoverageLinksArgs),
    /// Report the test-class taxonomy (COV-3): totals per class and, per
    /// surface, the set of test classes that reach it (joined through the
    /// COV-2 coverage links). A surface reached by a single class is flagged
    /// `class_gap` with the missing classes named — the signal that a surface
    /// has forty unit tests and zero integration/e2e/conformance coverage.
    ///
    /// Requires `.vfs/graph/surfaces.jsonl` (run `hilo graph surfaces`) and
    /// `.vfs/graph/coverage_links.jsonl` (run `hilo graph coverage-links`).
    TestClasses(TestClassesArgs),
    /// Group surfaces into features/components and roll coverage + class mix
    /// up to the group level (COV-4), so "is the workspace-mount feature
    /// covered?" is one query rather than a per-ask re-derivation.
    ///
    /// A surface's group is chosen by one documented precedence — explicit
    /// `user.vfs.feature` / `user.vfs.component` xattr, then the crate
    /// boundary (nearest project manifest), then the module path prefix — and
    /// the chosen source is recorded on the surface's own row, so an
    /// annotated group is always distinguishable from a structural fallback.
    /// An un-annotated repo still groups (the fallback is stated in the
    /// output), so the report is never empty.
    ///
    /// Requires `.vfs/graph/surfaces.jsonl` (run `hilo graph surfaces`,
    /// COV-1). `.vfs/graph/coverage_links.jsonl` (COV-2) supplies the
    /// coverage counts and the class mix; without it every surface reports
    /// uncovered, with that absence named in the report.
    ///
    /// `--group <name>` answers for one group and lists its gaps.
    Rollup(RollupArgs),
    /// GAP-112/GAP-113: the wiring report. Detects silent-fallback wiring
    /// (interfaces consumed from non-test code whose ONLY satisfiers are
    /// test-role types — the TRBL-084 shape) as `pass` / `finding` /
    /// `unsupported`, and emits the aggregate per-module wiring report:
    /// public surfaces, inbound/outbound connection counts, deltas against a
    /// stored baseline, untested surfaces, and a module classification
    /// (entrypoint/service/lib/test/dead). (feat(graph): COV-4 surface grouping + rollup)
    Wiring(WiringArgs),
    /// COV-5: the symbol-level connection + test audit. Answers, per public
    /// function/surface, whether it is reachable from a declared entrypoint
    /// and whether it carries a test link, and partitions the NAMED list into
    /// three buckets — `unreachable`, `unlinked`, `ok` — each with a count and
    /// the rule that produced it. Languages without a public-function
    /// extractor are named as `unknown` rather than collapsed into `ok`.
    Audit(AuditArgs),
}

#[derive(clap::Args)]
struct AuditArgs {
    /// Print the report as JSON (locked shape — see README "graph audit").
    #[arg(long)]
    json: bool,

    /// Optional project root to audit (defaults to the current directory).
    path: Option<String>,
}

#[derive(clap::Args)]
struct WiringArgs {
    /// Print the report as JSON (versioned field set — see docs/cli-reference.md
    /// "wiring", schema `hilo.graph.wiring/2`).
    #[arg(long)]
    json: bool,

    /// Optional project root to scan (defaults to the current directory).
    path: Option<String>,

    /// Compare the current module snapshots against a stored baseline file
    /// and report the added/removed surfaces and edges per module.
    #[arg(long, value_name = "FILE")]
    baseline: Option<String>,

    /// Write the CURRENT module snapshots to FILE as a baseline, for a later
    /// `--baseline FILE` comparison.
    #[arg(long, value_name = "FILE")]
    write_baseline: Option<String>,
}

#[derive(clap::Args)]
struct TestClassesArgs {
    /// Print the report as JSON (locked shape) instead of text.
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args)]
struct RollupArgs {
    /// Grouping dimension: `feature` (explicit xattr annotation first, then
    /// the crate boundary, then the module path prefix — the documented
    /// precedence), `crate`, or `module`.
    ///
    /// Explicit `user.vfs.*` annotations take precedence in every mode; the
    /// flag biases which structural fallback is used when a surface carries
    /// none.
    #[arg(long, default_value = "feature", value_name = "DIMENSION")]
    by: String,

    /// Print the report as JSON (locked shape) instead of text.
    #[arg(long)]
    json: bool,

    /// Restrict the report to one group (by name) and answer with that
    /// group's own gap list: its uncovered surfaces and its class-gap
    /// surfaces. An unknown name is a loud error.
    #[arg(long, value_name = "NAME")]
    group: Option<String>,
}

#[derive(clap::Args)]
struct CoverageLinksArgs {
    /// Print the report as JSON (locked shape) instead of text.
    #[arg(long)]
    json: bool,

    /// Restrict output to one surface (by surface_id or name) — answers
    /// "what tests cover this surface".
    #[arg(long)]
    surface: Option<String>,

    /// Show only the unlinked set (surfaces with no link at all).
    #[arg(long)]
    unlinked: bool,
}

#[derive(clap::Args)]
/// Pre-compute the dependency graph for all discovered source files.
///
/// Discovery prunes dependency, cache, vendor, and hidden paths by default.
/// To override a pruning decision, set `graph.include_paths` in
/// `.vfs/manifest.yaml` or `manifest.yaml`. `.hiloignore` controls backend
/// sync and does not override graph discovery.
///
/// Requires a Hilo project (`.vfs/manifest.yaml`, or a root-level
/// `manifest.yaml`): run `hilo init` in a directory without one.
struct WarmArgs {
    /// Detect cross-repo imports using the workspace manifest.
    /// When set, import paths that resolve to files in another workspace
    /// repo are flagged as `external:repo-name:path` edges.
    #[arg(long)]
    workspace: bool,

    /// Only parse files of a specific language (e.g. "rust", "python", "go").
    /// When omitted, all supported languages are scanned.
    #[arg(long)]
    language: Option<String>,

    /// Only parse files changed since the last `graph warm` (mtime-based).
    /// Used by the post-commit hook for incremental updates.
    #[arg(long)]
    changed: bool,

    /// Allow HOME (or its canonical equivalent) as the project root.
    /// Without this flag `graph warm` refuses to walk the home directory.
    #[arg(long)]
    allow_home: bool,
}

#[derive(clap::Args)]
struct RelatedArgs {
    /// The file whose graph edges to query.
    path: String,

    /// Filter edges by relation type (e.g., "imports", "calls").
    #[arg(long)]
    relation: Option<String>,

    /// Query direction: "forward" (outgoing edges, default) or "reverse"
    /// (incoming edges, e.g. "imported_by", "tested_by").
    #[arg(long)]
    direction: Option<String>,
}

#[derive(clap::Args)]
struct ImpactArgs {
    /// The file whose transitive dependents to find.
    path: String,

    /// Maximum depth of transitive traversal (default: 10).
    ///
    /// Depth counts HOPS between graph nodes, and dependency chains pass
    /// through the `pkg:<crate>` pseudo-node: the parser emits edges that
    /// target `pkg:<crate>` (and, for named imports, `pkg:<crate>::<item>`)
    /// rather than file→file edges, so a file-form query reaches that file's
    /// real importers as `file → pkg:<crate> → importer` — two hops.
    ///
    /// `--max-depth 1` therefore reports only edges that target the queried
    /// file itself (or a `local:` node resolving to it). For sources whose
    /// imports resolve to `pkg:` nodes — Rust, Java — that is usually ZERO
    /// rows even when the file has many importers, so use `--max-depth 2` or
    /// more for a file-form query. Rows reached through a crate node print
    /// `scope=crate` and `via pkg:<crate>`; true file-level rows print
    /// `scope=file`.
    ///
    /// A crate's pkg FAMILY is matched too (GAP-048): member nodes
    /// `pkg:<crate>::<item>` and underscore-sibling crates
    /// `pkg:<crate>_<sibling>` (e.g. `serde_derive` for `serde`). Because
    /// its public surface re-exports them, the reported count can exceed the
    /// number of direct importers of the queried file — see
    /// docs/cli-reference.md (`hilo graph impact`, Family expansion) for the
    /// tradeoff.
    #[arg(long, default_value = "10")]
    max_depth: u32,

    /// Output format: "text" (default) or "json".
    #[arg(long)]
    format: Option<String>,

    /// Include external cross-repo edges in impact traversal.
    /// When set, `external:repo-name:path` edges are also followed.
    #[arg(long)]
    external: bool,
}

#[derive(clap::Args)]
struct RuleCheckArgs {
    /// Name of the rule to execute (e.g., "stale-files").
    name: String,
}

#[derive(clap::Args)]
struct UnderstandArgs {
    /// Natural-language description of what you need to understand.
    task: String,
    /// Token budget override (default: 6000).
    #[arg(long)]
    budget: Option<usize>,
}

#[derive(clap::Args)]
struct SearchArgs {
    /// Semantic search query.
    query: String,
    /// Max results to return (default: 20).
    #[arg(long)]
    limit: Option<usize>,
    /// Skip symbol indexing (faster, but exact symbol names may not match
    /// their defining file). Default: symbols indexed (GAP-077).
    #[arg(long)]
    no_symbols: bool,
}

#[derive(clap::Args)]
struct ModuleArgs {
    /// Directory prefix for the module (e.g. "hilo-graph/src").
    prefix: String,
}

#[derive(clap::Args)]
struct SurfacesArgs {
    /// Print the inventory as JSON (locked shape) instead of text.
    #[arg(long)]
    json: bool,

    /// Restrict enumeration to one surface kind
    /// (cli_verb | cli_flag | mcp_tool | ffi_export | fuse_op | config_key |
    /// public_api_item). Without it, all kinds are enumerated.
    #[arg(long)]
    kind: Option<String>,
}

#[derive(clap::Args)]
struct ServeArgs {
    /// Run as an MCP server (required — the only implemented server mode).
    ///
    /// Requires a Hilo project: run `hilo init` in the directory first.
    /// Without a manifest the server exits non-zero, naming that command,
    /// rather than serving tools that can only answer from an empty graph.
    #[arg(long, required = true)]
    mcp: bool,
}

#[derive(Subcommand)]
enum WorkspaceCommand {
    /// Mount all repos and backends from the manifest.
    Mount(WorkspaceMountArgs),
    /// Unmount a workspace.
    Unmount(WorkspaceUnmountArgs),
    /// Two-way sync a local directory against an S3 prefix.
    ///
    /// Non-ignored files are mirrored in both directions; files matched by
    /// the ignore file (git-ignore style, ".vfsignore") stay local-only and
    /// are never transferred.
    Sync(WorkspaceSyncArgs),
    /// List ephemeral (rebuildable/redownloadable) files in the workspace.
    Ephemeral(WorkspaceEphemeralArgs),
    /// Plan or apply a wipe of ephemeral files.
    Wipe(WorkspaceWipeArgs),
}

#[derive(clap::Args)]
struct WorkspaceMountArgs {
    /// Path to the workspace manifest YAML (e.g., .vfs/manifest.yaml).
    #[arg(long, default_value = ".vfs/manifest.yaml")]
    manifest: String,
    /// Directory to mount the workspace at.
    mount_point: String,
}

#[derive(clap::Args)]
struct WorkspaceUnmountArgs {
    /// Directory to unmount.
    mount_point: String,
}

#[derive(clap::Args)]
struct WorkspaceSyncArgs {
    /// S3 bucket name.
    #[arg(long)]
    bucket: String,
    /// S3 key prefix within the bucket (default: bucket root).
    #[arg(long, default_value = "")]
    prefix: String,
    /// Local directory to sync.
    #[arg(long)]
    at: String,
    /// Ignore file (git-ignore style). Defaults to <at>/.hiloignore.
    #[arg(long)]
    ignore: Option<String>,
    /// AWS region (default: us-east-1).
    #[arg(long, default_value = "us-east-1")]
    region: String,
    /// Explicit S3-compatible endpoint URL (MinIO et al.). Beats the
    /// AWS_ENDPOINT_URL environment variable; when omitted, the resolved
    /// endpoint is disclosed on the run header (DF-WARPFS-12).
    #[arg(long)]
    endpoint: Option<String>,
    /// Push local changes only — never download (remote-only files are
    /// skipped and reported).
    #[arg(long, conflicts_with_all = ["pull", "both"])]
    push: bool,
    /// Pull remote changes only — never upload (local-only files are
    /// skipped and reported).
    #[arg(long, conflicts_with_all = ["push", "both"])]
    pull: bool,
    /// Two-way sync (default)
    #[arg(long, conflicts_with_all = ["push", "pull"])]
    both: bool,
    /// Print the sync plan without transferring any files.
    #[arg(long)]
    dry_run: bool,
}

#[derive(clap::Args)]
struct WorkspaceEphemeralArgs {
    /// Subtrees to list (default: the whole workspace root).
    #[arg(value_name = "PATH")]
    paths: Vec<PathBuf>,
}

#[derive(clap::Args)]
struct WorkspaceWipeArgs {
    /// Wipe ephemeral files (required — the only wipe mode).
    #[arg(long, required = true)]
    ephemeral: bool,
    /// Actually delete ephemeral files (default: dry-run plan only).
    #[arg(long)]
    apply: bool,
}

#[derive(clap::Args)]
struct StatsArgs {
    /// Max list entries to print (orphans). 0 = unlimited. Default 25.
    #[arg(long, default_value_t = 25)]
    limit: usize,
}

#[derive(clap::Args)]
struct ClassifyArgs {
    /// Dry run — print classifications without writing xattrs.
    #[arg(long)]
    dry_run: bool,
    /// Verbose output — show every file classification.
    #[arg(short, long)]
    verbose: bool,
    /// Enable feature inference — set user.vfs.feature xattrs from directory structure.
    #[arg(long)]
    features: bool,
    /// Max per-file lines to print in dry-run/verbose mode. 0 = unlimited. Default 50.
    #[arg(long, default_value_t = 50)]
    limit: usize,
}

/// Parse the process arguments and dispatch to the matching command handler.
///
/// The binary (`src/main.rs`) owns process-level concerns (SIGPIPE restore)
/// and calls this; everything else lives here so the command tree is
/// introspectable for surface enumeration.
pub fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init(args) => init::run(args.allow_home, args.no_hooks),
        Commands::Meta(args) => meta::run(&args.path, args.set.as_deref(), args.value.as_deref()),
        Commands::Graph(GraphCommand::Warm(args)) => {
            graph::run_warm(args.workspace, args.language, args.changed, args.allow_home)
        }
        Commands::Graph(GraphCommand::Stats(args)) => graph::run_stats(args.limit),
        Commands::Graph(GraphCommand::Related(args)) => graph::run_related(
            &args.path,
            args.relation.as_deref(),
            args.direction.as_deref(),
        ),
        Commands::Graph(GraphCommand::Impact(args)) => graph::run_impact(
            &args.path,
            args.max_depth,
            args.format.as_deref(),
            args.external,
        ),
        Commands::Graph(GraphCommand::Understand(args)) => {
            graph::run_understand(&args.task, args.budget)
        }
        Commands::Graph(GraphCommand::Search(args)) => {
            graph::run_search(&args.query, args.limit, args.no_symbols)
        }
        Commands::Graph(GraphCommand::Module(args)) => graph::run_module(&args.prefix),
        Commands::Graph(GraphCommand::Untested) => graph::run_untested(),
        Commands::Graph(GraphCommand::RuleList) => graph::run_rule_list(),
        Commands::Graph(GraphCommand::RuleCheck(args)) => graph::run_rule_check(&args.name),
        Commands::Graph(GraphCommand::Clean) => graph::run_clean(),
        Commands::Graph(GraphCommand::Surfaces(args)) => {
            surfaces::run(args.json, args.kind.as_deref())
        }
        Commands::Graph(GraphCommand::CoverageLinks(args)) => {
            coverage_links::run(args.json, args.surface.as_deref(), args.unlinked)
        }
        Commands::Graph(GraphCommand::Wiring(args)) => wiring::run_wiring(
            args.json,
            args.path.clone(),
            args.baseline.clone(),
            args.write_baseline.clone(),
        ),
        Commands::Graph(GraphCommand::TestClasses(args)) => test_classes::run(args.json),
        Commands::Graph(GraphCommand::Rollup(args)) => {
            rollup::run(args.json, &args.by, args.group.as_deref())
        }
        Commands::Graph(GraphCommand::Audit(args)) => audit::run(args.json, args.path.clone()),
        Commands::Serve(args) => serve::run(args.mcp),
        Commands::Backend(backend::BackendCommand::Mount(args)) => backend::run_mount(&args),
        Commands::Backend(backend::BackendCommand::List) => backend::run_list(),
        Commands::Backend(backend::BackendCommand::Sync(args)) => backend::run_sync(&args),
        Commands::Backend(backend::BackendCommand::Setup(args)) => backend::run_setup(&args),
        Commands::Mount(args) => mount::run_mount(
            &args.mount_point,
            args.triggers,
            args.allow_other,
            args.daemon,
        ),
        Commands::Workspace(WorkspaceCommand::Mount(args)) => {
            workspace::run_workspace_mount(&args.manifest, &args.mount_point)
        }
        Commands::Workspace(WorkspaceCommand::Unmount(args)) => {
            workspace::run_workspace_unmount(&args.mount_point)
        }
        Commands::Workspace(WorkspaceCommand::Sync(args)) => workspace::run_workspace_sync(
            &args.bucket,
            &args.prefix,
            &args.at,
            args.ignore.as_deref(),
            &args.region,
            args.endpoint.as_deref(),
            sync_direction(args.push, args.pull, args.both),
            args.dry_run,
        ),
        Commands::Workspace(WorkspaceCommand::Ephemeral(args)) => {
            workspace::run_workspace_ephemeral(&args.paths)
        }
        Commands::Workspace(WorkspaceCommand::Wipe(args)) => {
            workspace::run_workspace_wipe(args.apply)
        }
        Commands::Classify(args) => {
            classify::run_classify(args.dry_run, args.verbose, args.features, args.limit)
        }
        Commands::Ignore(ignore::IgnoreCommand::Check(args)) => ignore::run_ignore_check(args),
        Commands::Plugin(PluginCommand::Load(args)) => plugin::run_plugin_load(&args.wasm_path),
        Commands::Plugin(PluginCommand::List) => plugin::run_plugin_list(),
    }
}
