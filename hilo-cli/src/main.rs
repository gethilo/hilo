mod commands;

use clap::{Parser, Subcommand};

use commands::plugin::PluginCommand;
use commands::{backend, classify, graph, init, meta, mount, plugin, serve, workspace};

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
    /// Run a Hilo server (MCP stub).
    Serve(ServeArgs),
    /// Manage virtual backends (S3, git, remote, local).
    #[command(subcommand)]
    Backend(commands::backend::BackendCommand),
    /// Mount a Hilo virtual filesystem via FUSE.
    Mount(MountArgs),
    /// Manage multi-repo workspace mounts.
    #[command(subcommand)]
    Workspace(WorkspaceCommand),
    /// Auto-classify files with role/status metadata (entrypoint, test, library, etc.).
    Classify(ClassifyArgs),
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
}

#[derive(clap::Args)]
struct InitArgs {}

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
    /// Walk the current directory, parse Go imports, and populate the graph.
    Discover(DiscoverArgs),
    /// Print summary statistics from the discovered dependency graph.
    Stats,
    /// Query graph edges for a specific file.
    Related(RelatedArgs),
    /// Find all files that transitively depend on a given file (impact analysis).
    Impact(ImpactArgs),
    /// List all rules defined in the manifest.
    RuleList,
    /// Execute a named rule query against the dependency graph.
    RuleCheck(RuleCheckArgs),
}

#[derive(clap::Args)]
struct DiscoverArgs {
    /// Detect cross-repo imports using the workspace manifest.
    /// When set, import paths that resolve to files in another workspace
    /// repo are flagged as `external:repo-name:path` edges.
    #[arg(long)]
    workspace: bool,
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
struct ServeArgs {
    /// Run as an MCP server.
    #[arg(long)]
    mcp: bool,
}

#[derive(Subcommand)]
enum WorkspaceCommand {
    /// Mount all repos and backends from the manifest.
    Mount(WorkspaceMountArgs),
    /// Unmount a workspace.
    Unmount(WorkspaceUnmountArgs),
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
}

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Init(_) => init::run(),
        Commands::Meta(args) => meta::run(&args.path, args.set.as_deref(), args.value.as_deref()),
        Commands::Graph(GraphCommand::Discover(args)) => graph::run_discover(args.workspace),
        Commands::Graph(GraphCommand::Stats) => graph::run_stats(),
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
        Commands::Graph(GraphCommand::RuleList) => graph::run_rule_list(),
        Commands::Graph(GraphCommand::RuleCheck(args)) => graph::run_rule_check(&args.name),
        Commands::Serve(args) => serve::run(args.mcp),
        Commands::Backend(commands::backend::BackendCommand::Mount(args)) => {
            backend::run_mount(&args)
        }
        Commands::Backend(commands::backend::BackendCommand::List) => backend::run_list(),
        Commands::Mount(args) => {
            mount::run_mount(&args.mount_point, args.triggers, args.allow_other)
        }
        Commands::Workspace(WorkspaceCommand::Mount(args)) => {
            workspace::run_workspace_mount(&args.manifest, &args.mount_point)
        }
        Commands::Workspace(WorkspaceCommand::Unmount(args)) => {
            workspace::run_workspace_unmount(&args.mount_point)
        }
        Commands::Classify(args) => {
            classify::run_classify(args.dry_run, args.verbose, args.features)
        }
        Commands::Plugin(PluginCommand::Load(args)) => plugin::run_plugin_load(&args.wasm_path),
        Commands::Plugin(PluginCommand::List) => plugin::run_plugin_list(),
    };

    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
