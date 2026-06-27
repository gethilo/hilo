//! `hilo plugin` — load and list wasm plugins from .vfs/plugins/.

use anyhow::Result;
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
