//! PERF-005: refuse destructive project-root defaults (HOME) unless the user
//! explicitly overrides with `--allow-home`.
//!
//! GAP-086: also refuse to *operate* on a directory that is not a Hilo
//! project. `graph warm` walked any directory and left a partial `.vfs/graph/`
//! (parse cache, `edges.jsonl`, DuckDB cache) behind, and `serve --mcp`
//! happily served a zeroed graph — both produced state or answers from a tree
//! that is not a project, and neither named the fix.
//!
//! Both `hilo init` (writes `.vfs/` into the current directory) and
//! `hilo graph warm` (recursively parses everything under the current
//! directory, including dependency/cache trees) are unsafe when the effective
//! project root equals the user's HOME directory. `--allow-home` is the
//! explicit escape hatch that preserves the previous behavior.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

/// Manifest filenames that make a directory a Hilo project root, in the
/// precedence order used by the graph commands ([`crate::commands::graph`]).
///
/// A root-level `manifest.yaml` is accepted because the CLI has always read
/// manifests from either location; `hilo init` writes the `.vfs/` form (and
/// the standard `.vfs/` layout alongside it).
pub const MANIFEST_PATHS: &[&str] = &["manifest.yaml", ".vfs/manifest.yaml"];

/// Default prune names for graph source discovery (PERF-005).
///
/// Dependency and cache trees — pruning them before descending is what keeps
/// `graph warm` from parsing tens of thousands of third-party sources.
/// `go/pkg/mod` is handled as a path-suffix in the discovery walk (its `go`
/// parent directory is a normal, non-hidden source root).
pub const DEFAULT_PRUNE_DIRS: &[&str] = &[
    "target",        // Rust build output
    "node_modules",  // JavaScript/TypeScript
    "vendor",        // Go / PHP
    "__pycache__",   // Python cache
    "venv",          // Python virtualenv (non-dot spelling)
    ".venv",         // Python virtualenv
    "site-packages", // Python site-packages
    ".cache",        // Generic user/tool cache
    ".rustup",       // Rust toolchain cache
    ".npm",          // npm cache
];

/// The user's HOME directory, matching the convention used elsewhere in the
/// CLI (`std::env::var_os("HOME")`).
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Determine whether the effective project root equals the user's HOME
/// directory.
///
/// Both paths are canonicalized before comparison so symlinked HOMEs and
/// `.` / `..` components can't dodge the guard. The `home` parameter is
/// injectable so tests can exercise refusal and override behavior in a
/// temporary directory without touching the real HOME.
pub fn is_project_root_home(cwd: &Path, home: Option<PathBuf>) -> Result<bool> {
    let home = match home {
        Some(h) => h,
        None => home_dir().context("failed to determine the user home directory")?,
    };
    // Canonicalize when possible (existing, symlinked paths); fall back to the
    // raw path for injected homes that don't exist on disk.
    let home_canon = std::fs::canonicalize(&home).unwrap_or(home);

    let cwd_canon = std::fs::canonicalize(cwd)
        .with_context(|| format!("failed to resolve {}", cwd.display()))?;

    Ok(cwd_canon == home_canon)
}

/// Refuse HOME as the effective project root unless explicitly allowed.
///
/// Error text names `--allow-home` so the fix is actionable. The `home`
/// parameter is injectable so tests can exercise refusal and override
/// behavior without touching the real HOME; production callers pass `None`.
pub fn ensure_not_home_with(cwd: &Path, allow_home: bool, home: Option<PathBuf>) -> Result<()> {
    if allow_home {
        return Ok(());
    }
    if is_project_root_home(cwd, home)? {
        return Err(anyhow!(
            "refusing to operate with HOME ({}) as the project root; \
             pass --allow-home to override if this is really intended",
            home_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<home>".to_string())
        ));
    }
    Ok(())
}

/// The manifest that makes `root` a Hilo project, if one is present.
///
/// Returns the first existing candidate of [`MANIFEST_PATHS`] (root-level
/// `manifest.yaml` before `.vfs/manifest.yaml`, both relative to `root`).
pub fn manifest_path(root: &Path) -> Option<PathBuf> {
    MANIFEST_PATHS
        .iter()
        .map(|rel| root.join(rel))
        .find(|candidate| candidate.exists())
}

/// Refuse to operate on a directory that is not a Hilo project root.
///
/// GAP-086: callers that require a project (`graph warm`, `serve --mcp`)
/// use this so a missing manifest fails loudly, naming `hilo init`, instead
/// of scattering a partial `.vfs/graph/` into an unrelated tree or answering
/// from a zeroed graph. An initialized project with no edges yet is a valid
/// project: only the manifest's presence is checked, never its contents.
pub fn ensure_project_root(root: &Path) -> Result<()> {
    if manifest_path(root).is_some() {
        return Ok(());
    }
    Err(anyhow!(
        "no Hilo project found in {}: expected manifest.yaml or .vfs/manifest.yaml; \
         run `hilo init` first",
        root.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn refusal_when_cwd_equals_injected_home() {
        let home = TempDir::new().unwrap();
        let err =
            ensure_not_home_with(home.path(), false, Some(home.path().to_path_buf())).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("--allow-home"),
            "error must name the flag: {msg}"
        );
        assert!(msg.contains("HOME"), "error must mention HOME: {msg}");
    }

    #[test]
    fn allow_home_overrides_refusal() {
        let home = TempDir::new().unwrap();
        ensure_not_home_with(home.path(), true, Some(home.path().to_path_buf())).unwrap();
    }

    #[test]
    fn normal_project_dir_is_not_refused() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        ensure_not_home_with(project.path(), false, Some(home.path().to_path_buf())).unwrap();
    }

    #[test]
    fn subdir_of_home_is_not_refused() {
        // A project *inside* HOME (e.g. ~/code/proj) must stay allowed; only
        // HOME itself is refused.
        let home = TempDir::new().unwrap();
        let proj = home.path().join("code").join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        ensure_not_home_with(&proj, false, Some(home.path().to_path_buf())).unwrap();
    }

    #[test]
    fn dot_components_cannot_dodge_the_guard() {
        let home = TempDir::new().unwrap();
        let dodge = home.path().join(".");
        let err = ensure_not_home_with(&dodge, false, Some(home.path().to_path_buf())).unwrap_err();
        assert!(format!("{err:#}").contains("--allow-home"));
    }

    #[test]
    fn symlinked_cwd_cannot_dodge_the_guard() {
        let home = TempDir::new().unwrap();
        let link_dir = TempDir::new().unwrap();
        let link = link_dir.path().join("home-link");
        std::os::unix::fs::symlink(home.path(), &link).unwrap();
        let err = ensure_not_home_with(&link, false, Some(home.path().to_path_buf())).unwrap_err();
        assert!(format!("{err:#}").contains("--allow-home"));
    }

    // ======================================================================
    // GAP-086: project-root precondition (manifest presence)
    // ======================================================================

    #[test]
    fn project_root_accepts_vfs_manifest() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".vfs")).unwrap();
        std::fs::write(dir.path().join(".vfs/manifest.yaml"), "version: 2\n").unwrap();
        ensure_project_root(dir.path()).unwrap();
        assert_eq!(
            manifest_path(dir.path()),
            Some(dir.path().join(".vfs/manifest.yaml"))
        );
    }

    #[test]
    fn project_root_accepts_root_level_manifest() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("manifest.yaml"), "version: 2\n").unwrap();
        ensure_project_root(dir.path()).unwrap();
        assert_eq!(
            manifest_path(dir.path()),
            Some(dir.path().join("manifest.yaml"))
        );
    }

    #[test]
    fn project_root_without_manifest_names_init() {
        let dir = TempDir::new().unwrap();
        let err = ensure_project_root(dir.path()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("hilo init"), "error must name the fix: {msg}");
        assert!(
            msg.contains(&dir.path().display().to_string()),
            "error must name the directory it refused: {msg}"
        );
        assert_eq!(manifest_path(dir.path()), None);
    }

    #[test]
    fn project_root_does_not_require_a_vfs_directory() {
        // The precondition is the manifest, not the layout: a project rooted
        // at a tree whose `.vfs/` has not been created yet is still refused,
        // and an initialized-but-empty project is accepted (no edge files).
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "fn lib() {}\n").unwrap();
        assert!(ensure_project_root(dir.path()).is_err());

        std::fs::create_dir_all(dir.path().join(".vfs")).unwrap();
        std::fs::write(dir.path().join(".vfs/manifest.yaml"), "version: 2\n").unwrap();
        ensure_project_root(dir.path()).unwrap();
    }
}
