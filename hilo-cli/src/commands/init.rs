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
///
/// GAP-087: `no_hooks` (the `--no-hooks` flag) skips git hook installation
/// entirely — see [`run_in`].
pub fn run(allow_home: bool, no_hooks: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to determine the current directory")?;
    run_in(&cwd, allow_home, None, no_hooks)
}

/// Injectable core of [`run`] (PERF-005): root and HOME are parameters so the
/// refusal/override contract is testable in a temporary directory without
/// touching the real HOME or the process working directory.
///
/// When `no_hooks` is true the `.vfs/` tree and manifest are still created, but
/// nothing under `.git/hooks/` is created or modified: pre-existing hook files
/// stay byte-identical and `.git/hooks/` is not even created. Hook installation
/// only ever accompanies a fresh manifest, so the opt-out is honored on an
/// already-initialized project too.
pub fn run_in(
    root: &Path,
    allow_home: bool,
    home: Option<std::path::PathBuf>,
    no_hooks: bool,
) -> Result<()> {
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
    //
    // GAP-087: `--no-hooks` returns before `.git/hooks/` is touched at all, so
    // an opt-out can neither create nor modify a hook file.
    if no_hooks {
        println!("Skipped git hook installation (--no-hooks)");
        return Ok(());
    }

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
        let err = run_in(home.path(), false, Some(home.path().to_path_buf()), false).unwrap_err();
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
        run_in(home.path(), true, Some(home.path().to_path_buf()), false).unwrap();
        assert!(
            home.path().join(".vfs").join("manifest.yaml").exists(),
            "--allow-home init must proceed"
        );
    }

    #[test]
    fn init_normal_project_still_works() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        run_in(
            project.path(),
            false,
            Some(home.path().to_path_buf()),
            false,
        )
        .unwrap();
        assert!(project.path().join(".vfs").join("manifest.yaml").exists());
    }

    // ─────────────────── GAP-087: --no-hooks opt-out ───────────────────

    /// Snapshot `<root>/.git/hooks` as sorted (name, bytes) pairs.
    ///
    /// Returns an empty vec when the directory does not exist — an absent
    /// `.git/hooks/` and an empty one are both "nothing for Hilo to touch".
    fn snapshot_hooks(root: &Path) -> Vec<(String, Vec<u8>)> {
        let hooks_dir = root.join(".git").join("hooks");
        if !hooks_dir.exists() {
            return Vec::new();
        }
        let mut entries: Vec<(String, Vec<u8>)> = std::fs::read_dir(&hooks_dir)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                let name = entry.file_name().to_string_lossy().into_owned();
                let bytes = std::fs::read(entry.path()).unwrap();
                (name, bytes)
            })
            .collect();
        entries.sort();
        entries
    }

    /// Create `<root>/.git/hooks/post-commit` with caller-supplied content.
    fn write_existing_hook(root: &Path, content: &str) {
        let hooks_dir = root.join(".git").join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        std::fs::write(hooks_dir.join("post-commit"), content).unwrap();
    }

    #[test]
    fn init_no_hooks_leaves_existing_hooks_byte_identical() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        let custom = "#!/bin/sh\necho 'pre-existing project hook'\n";
        write_existing_hook(project.path(), custom);
        let before = snapshot_hooks(project.path());

        run_in(project.path(), false, Some(home.path().to_path_buf()), true).unwrap();

        assert!(
            project.path().join(".vfs").join("manifest.yaml").exists(),
            "opt-out must still create the manifest"
        );
        assert_eq!(
            snapshot_hooks(project.path()),
            before,
            ".git/hooks must be byte-identical after --no-hooks"
        );
        assert_eq!(
            std::fs::read_to_string(project.path().join(".git/hooks/post-commit")).unwrap(),
            custom,
            "pre-existing hook content must be untouched"
        );
        assert!(
            !project.path().join(".git/hooks/post-merge").exists(),
            "--no-hooks must not create a post-merge hook"
        );
    }

    #[test]
    fn init_no_hooks_does_not_create_git_hooks_dir() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();

        run_in(project.path(), false, Some(home.path().to_path_buf()), true).unwrap();

        assert!(project.path().join(".vfs").join("manifest.yaml").exists());
        assert!(
            !project.path().join(".git").exists(),
            "--no-hooks must not create .git/ (or .git/hooks/) at all"
        );
    }

    #[test]
    fn init_no_hooks_honors_opt_out_when_manifest_exists() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();

        // Pre-existing project: manifest already present, plus custom hook
        // content. Re-running with --no-hooks must not touch either.
        std::fs::create_dir_all(project.path().join(".vfs")).unwrap();
        std::fs::write(
            project.path().join(".vfs").join("manifest.yaml"),
            "version: 2\nproject:\n  name: existing\n",
        )
        .unwrap();
        let custom = "#!/bin/sh\necho 'custom hook'\n";
        write_existing_hook(project.path(), custom);
        let manifest_before =
            std::fs::read(project.path().join(".vfs").join("manifest.yaml")).unwrap();
        let hooks_before = snapshot_hooks(project.path());

        run_in(project.path(), false, Some(home.path().to_path_buf()), true).unwrap();

        assert_eq!(
            std::fs::read(project.path().join(".vfs").join("manifest.yaml")).unwrap(),
            manifest_before,
            "existing manifest must stay byte-identical"
        );
        assert_eq!(
            snapshot_hooks(project.path()),
            hooks_before,
            "opt-out on an initialized project must leave hooks untouched"
        );
        assert!(!project.path().join(".git/hooks/post-merge").exists());
    }

    #[test]
    fn init_without_no_hooks_still_installs_hooks() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        write_existing_hook(project.path(), "#!/bin/sh\necho 'keep me'\n");

        run_in(
            project.path(),
            false,
            Some(home.path().to_path_buf()),
            false,
        )
        .unwrap();

        let post_commit =
            std::fs::read_to_string(project.path().join(".git/hooks/post-commit")).unwrap();
        assert!(
            post_commit.contains("echo 'keep me'"),
            "existing hook content must be preserved on install"
        );
        assert!(
            post_commit.contains("### HILO"),
            "default init must still install the Hilo block"
        );
        assert!(
            project.path().join(".git/hooks/post-merge").exists(),
            "default init must still install post-merge"
        );
    }

    #[test]
    fn init_no_hooks_still_preserves_home_guard() {
        let home = TempDir::new().unwrap();
        let err = run_in(home.path(), false, Some(home.path().to_path_buf()), true).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("--allow-home"),
            "error must name the flag: {msg}"
        );
        assert!(!home.path().join(".vfs").exists());
    }
}
