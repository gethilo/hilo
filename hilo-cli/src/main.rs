mod commands;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use commands::plugin::PluginCommand;
use commands::{backend, classify, graph, ignore, init, meta, mount, plugin, serve, workspace};

/// Shared sync-direction vocabulary (DF-WARPFS-12): `hilo workspace sync`
/// and `hilo backend sync` speak the same --push/--pull/--both flags, and
/// the resolved direction is printed on every run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyncDirectionArg {
    Push,
    Pull,
    Both,
}

impl SyncDirectionArg {
    /// The exact string printed on the run header / plan lines.
    pub(crate) fn label(self) -> &'static str {
        match self {
            SyncDirectionArg::Push => "push",
            SyncDirectionArg::Pull => "pull",
            SyncDirectionArg::Both => "two-way",
        }
    }
}

/// Flag resolution: --push / --pull / --both (default two-way). `_both` is
/// the explicit spelling of the default and carries no extra state — the
/// fallback branch serves both `--both` and no-flag-at-all.
pub(crate) fn sync_direction(push: bool, pull: bool, _both: bool) -> SyncDirectionArg {
    if push {
        SyncDirectionArg::Push
    } else if pull {
        SyncDirectionArg::Pull
    } else {
        SyncDirectionArg::Both
    }
}

/// Hilo command-line interface.
#[derive(Parser)]
#[command(name = "hilo", about = "Hilo CLI", version)]
struct Cli {
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
    /// Run a Hilo server (MCP stdio transport). Exposes 17 vfs_* tools — see README.
    Serve(ServeArgs),
    /// Manage virtual backends (S3, gdrive, onedrive, dropbox, external).
    #[command(subcommand)]
    Backend(commands::backend::BackendCommand),
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
    /// Push local changes to the backend
    #[arg(long, conflicts_with_all = ["pull", "both"])]
    push: bool,
    /// Pull remote changes into the workspace
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

/// Restore the default `SIGPIPE` disposition at process start.
///
/// The Rust runtime installs `SIG_IGN` for `SIGPIPE` before `main` runs, so a
/// write into a closed pipe (`hilo graph stats | head`, `| true`) surfaces as
/// `EPIPE` / "Broken pipe (os error 32)" and the `std` print macros turn that
/// write error into a panic with exit code 101. Every other Unix filter is
/// killed silently by the kernel instead (exit status 141), so restore the
/// default handler and let SIGPIPE do its job. This is the ripgrep/fd
/// behaviour; it belongs to the CLI binary only, never to a library crate.
#[cfg(unix)]
fn reset_sigpipe() {
    // SAFETY: installing a default disposition for a valid signal number
    // cannot violate memory safety; the return value is only `SIG_ERR` for an
    // invalid signal, and `SIGPIPE` is always valid on unix.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

/// No-op on platforms without POSIX signals (Windows has no `SIGPIPE`).
#[cfg(not(unix))]
fn reset_sigpipe() {}

fn main() {
    reset_sigpipe();

    let cli = Cli::parse();

    let result = match cli.command {
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
        Commands::Serve(args) => serve::run(args.mcp),
        Commands::Backend(commands::backend::BackendCommand::Mount(args)) => {
            backend::run_mount(&args)
        }
        Commands::Backend(commands::backend::BackendCommand::List) => backend::run_list(),
        Commands::Backend(commands::backend::BackendCommand::Sync(args)) => {
            backend::run_sync(&args)
        }
        Commands::Backend(commands::backend::BackendCommand::Setup(args)) => {
            backend::run_setup(&args)
        }
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
    };

    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
