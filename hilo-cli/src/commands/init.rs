//! `hilo init` — create the `.vfs/` directory tree and a default manifest.

use anyhow::{Context, Result};
use hilo_core::manifest::{Manifest, Project};
use hilo_metadata::inventory;

use crate::commands::hooks;

/// Create the `.vfs/` structure and a minimal `manifest.yaml` in the current
/// directory.
///
/// Idempotent: if `.vfs/manifest.yaml` already exists it is left untouched.
pub fn run() -> Result<()> {
    let cwd = std::env::current_dir().context("failed to determine the current directory")?;

    // Create the .vfs/ directory tree (idempotent — safe to call repeatedly).
    inventory::create_vfs_structure(&cwd).context("failed to create .vfs directory structure")?;

    let manifest_path = cwd.join(".vfs").join("manifest.yaml");

    // Idempotent: never overwrite an existing manifest.
    if manifest_path.exists() {
        println!("Initialized Hilo in {}", cwd.display());
        return Ok(());
    }

    // Derive the project name from the current directory's name.
    let dir_name = cwd
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "hilo-project".to_string());

    // Manifest does not derive Default, so construct it field by field.
    // Every sub-struct implements Default (verified in hilo-core/manifest.rs).
    let manifest = Manifest {
        version: 2,
        project: Project {
            name: dir_name,
            description: String::new(),
        },
        interfaces: Default::default(),
        repos: Vec::new(),
        backends: Default::default(),
        metadata: Default::default(),
        graph: Default::default(),
        permissions: Default::default(),
        triggers: Vec::new(),
        rules: Vec::new(),
        plugins: Vec::new(),
        discovery: Default::default(),
        sandbox: Default::default(),
        performance: Default::default(),
    };

    let yaml = serde_yaml::to_string(&manifest).context("failed to serialize manifest to YAML")?;
    std::fs::write(&manifest_path, yaml)
        .with_context(|| format!("failed to write {}", manifest_path.display()))?;

    println!("Initialized Hilo in {}", cwd.display());

    // Install git hooks for auto-metadata-update on commit and pull.
    hooks::install_hooks(&cwd).context("failed to install git hooks")?;

    Ok(())
}
