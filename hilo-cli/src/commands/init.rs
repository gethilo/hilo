//! `hilo init` — create the `.vfs/` directory tree and a default manifest.

use std::path::Path;

use anyhow::{Context, Result};
use hilo_core::manifest::{Manifest, Project};
use hilo_metadata::inventory;

use crate::commands::guard;
use crate::commands::hooks;

/// Create the `.vfs/` structure and a minimal `manifest.yaml` in the current
/// directory.
///
/// PERF-005: refuses to run when the current directory is the user's HOME —
/// an accidental `hilo init` there scatters `.vfs/` state across the home
/// directory. `--allow-home` restores the previous behavior explicitly.
///
/// Idempotent: if `.vfs/manifest.yaml` already exists it is left untouched.
pub fn run(allow_home: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to determine the current directory")?;
    run_in(&cwd, allow_home, None)
}

/// Injectable core of [`run`] (PERF-005): root and HOME are parameters so the
/// refusal/override contract is testable in a temporary directory without
/// touching the real HOME or the process working directory.
pub fn run_in(root: &Path, allow_home: bool, home: Option<std::path::PathBuf>) -> Result<()> {
    let cwd = root;

    guard::ensure_not_home_with(cwd, allow_home, home)?;

    // Create the .vfs/ directory tree (idempotent — safe to call repeatedly).
    inventory::create_vfs_structure(cwd).context("failed to create .vfs directory structure")?;

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
    hooks::install_hooks(cwd).context("failed to install git hooks")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn init_refuses_home_root() {
        let home = TempDir::new().unwrap();
        let err = run_in(home.path(), false, Some(home.path().to_path_buf())).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("--allow-home"),
            "error must name the flag: {msg}"
        );
        assert!(
            !home.path().join(".vfs").exists(),
            "refused init must not create .vfs in HOME"
        );
    }

    #[test]
    fn init_allow_home_overrides() {
        let home = TempDir::new().unwrap();
        run_in(home.path(), true, Some(home.path().to_path_buf())).unwrap();
        assert!(
            home.path().join(".vfs").join("manifest.yaml").exists(),
            "--allow-home init must proceed"
        );
    }

    #[test]
    fn init_normal_project_still_works() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        run_in(project.path(), false, Some(home.path().to_path_buf())).unwrap();
        assert!(project.path().join(".vfs").join("manifest.yaml").exists());
    }
}
