//! `hilo plugin` — load and list wasm plugins from .vfs/plugins/.

use anyhow::{Context, Result};
use clap::Subcommand;
use hilo_plugins::{PluginRegistry, PluginRuntime};
use std::path::Path;

#[derive(Subcommand)]
pub enum PluginCommand {
    /// Load a .wasm plugin and register it in the runtime.
    Load(LoadArgs),
    /// List plugins discovered in .vfs/plugins/.
    List,
}

#[derive(clap::Args)]
pub struct LoadArgs {
    /// Path to the .wasm plugin file.
    pub wasm_path: String,
}

/// Load a .wasm plugin from disk and register it in the runtime.
pub fn run_plugin_load(wasm_path: &str) -> Result<()> {
    let path = Path::new(wasm_path);
    if !path.exists() {
        anyhow::bail!("plugin file not found: {}", wasm_path);
    }
    if path.extension().and_then(|e| e.to_str()) != Some("wasm") {
        anyhow::bail!("plugin file must have a .wasm extension: {}", wasm_path);
    }

    let mut runtime = PluginRuntime::new();
    let name = runtime
        .load_plugin(path)
        .map_err(|e| anyhow::anyhow!("failed to load plugin: {}", e))?;

    println!("loaded plugin: {}", name);
    println!(
        "  path: {}",
        path.canonicalize().unwrap_or(path.to_path_buf()).display()
    );
    println!(
        "  hooks: {}",
        runtime.plugins.last().map(|p| p.hooks.len()).unwrap_or(0)
    );
    println!(
        "  edge_types: {:?}",
        runtime
            .plugins
            .last()
            .map(|p| &p.edge_types)
            .unwrap_or(&vec![])
    );

    // DF-WARPFS-22: persist the plugin so `hilo plugin list` (which scans
    // .vfs/plugins/) actually sees what was loaded — a load that registers
    // nothing on disk is a silent no-op.
    let plugins_dir = Path::new(".vfs").join("plugins");
    std::fs::create_dir_all(&plugins_dir)
        .with_context(|| format!("failed to create {}", plugins_dir.display()))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("plugin path has no file name: {}", wasm_path))?;
    let dest = plugins_dir.join(file_name);

    // DF-WARPFS-58: when the source already lives inside .vfs/plugins, the
    // persist step would copy the file onto itself — std::fs::copy opens the
    // destination with O_TRUNC before reading, destroying the plugin (0 bytes)
    // while the load still reports success. Skip the copy when source and
    // destination are the same file, and fail loudly if a copy ever comes up
    // short so data loss can never look like a successful load again.
    let same_file = match (path.canonicalize(), dest.canonicalize()) {
        (Ok(src), Ok(d)) => src == d,
        // Destination not created yet can never be the source; canonicalize
        // failure on an existing source is a real probe error.
        (Ok(_), Err(_)) => false,
        (Err(e), _) => {
            return Err(e)
                .with_context(|| format!("failed to resolve plugin path {}", path.display()));
        }
    };
    if !same_file {
        let copied = std::fs::copy(path, &dest)
            .with_context(|| format!("failed to persist plugin to {}", dest.display()))?;
        let expected = std::fs::metadata(path)
            .with_context(|| format!("failed to stat plugin {}", path.display()))?
            .len();
        if copied != expected {
            anyhow::bail!(
                "persisted plugin is truncated: copied {} of {} bytes to {}",
                copied,
                expected,
                dest.display()
            );
        }
        println!("persisted to: {}", dest.display());
    } else {
        println!("already in .vfs/plugins: {}", dest.display());
    }

    Ok(())
}

/// List plugins discovered in `.vfs/plugins/`.
pub fn run_plugin_list() -> Result<()> {
    let plugins_dir = Path::new(".vfs").join("plugins");
    let manifests = PluginRegistry::discover(&plugins_dir)
        .map_err(|e| anyhow::anyhow!("failed to discover plugins: {}", e))?;

    if manifests.is_empty() {
        println!("no plugins found in {}", plugins_dir.display());
        return Ok(());
    }

    println!("plugins in {}:", plugins_dir.display());
    for m in &manifests {
        println!(
            "  {} v{} — {} hooks, {} edge types",
            m.name,
            m.version,
            m.hooks.len(),
            m.edge_types.len()
        );
        for hook in &m.hooks {
            println!("    hook: on={} priority={}", hook.on, hook.priority);
        }
    }

    Ok(())
}
