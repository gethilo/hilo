//! PERF-005: refuse destructive project-root defaults (HOME) unless the user
//! explicitly overrides with `--allow-home`.
//!
//! Both `hilo init` (writes `.vfs/` into the current directory) and
//! `hilo graph warm` (recursively parses everything under the current
//! directory, including dependency/cache trees) are unsafe when the effective
//! project root equals the user's HOME directory. `--allow-home` is the
//! explicit escape hatch that preserves the previous behavior.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

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
}
