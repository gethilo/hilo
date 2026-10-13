//! Path confinement for the MCP tool surface (DF-WARPFS-139).
//!
//! The MCP server is an agent-facing trust boundary: every path argument that
//! reaches the filesystem must resolve INSIDE the workspace root (the process
//! working directory) or a mounted backend root. Dogfood run 40 proved the
//! opposite: `vfs_get_metadata{path:"/etc/passwd"}` returned real host
//! metadata and `vfs_list_directory{path:"/home/kara"}` listed the whole home
//! directory.
//!
//! Two-layer defence, mirroring the skill doctrine for boundary validation:
//!
//! 1. **Lexical fold** — `.` and `..` components are collapsed on the
//!    *requested* string first, so a `../` escape that would only materialize
//!    at the kernel is rejected (or folded) before any syscall. This also
//!    gives stable rejections for hostile RELATIVE inputs regardless of where
//!    symlinks point.
//! 2. **Symlink-resolved prefix check** — the folded path is canonicalized
//!    (`fs::canonicalize`, which resolves symlinks AND must exist) and the
//!    result must be inside the canonicalized workspace root. A symlink that
//!    jumps out of the tree is therefore caught even when its name looks
//!    innocent.
//!
//! Existence: tools that need a live file (`vfs_get_metadata`,
//! `vfs_set_metadata`, `vfs_list_directory`'s filesystem fallback) get the
//! strict canonicalizing check. Tools that must answer for not-yet-on-disk
//! paths (`vfs_resolve_path`, `vfs_backend_status`, `vfs_sync_backend` report
//! `cached: false` / "not found on disk") get the same boundaries applied
//! lexically plus a best-effort canonicalization when the path exists — a
//! missing in-workspace path stays answerable, an out-of-workspace path is
//! always rejected.

use std::path::{Component, Path, PathBuf};

use crate::error::{McpError, McpResult};

/// Fold `.` and `..` components lexically (no syscalls).
///
/// Leading `..` components on a relative path cannot be folded against
/// anything — they are returned separately so the caller can reject the path
/// as an escape attempt rather than silently guessing a base.
///
/// On Windows, a `Prefix` component (e.g. `C:`) is preserved so absolute
/// drive paths keep working; every other component passes through untouched.
pub fn fold_lexical(path: &Path) -> (PathBuf, usize) {
    let mut leading_dotdot = 0usize;
    let mut parts: Vec<Component> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match parts.last() {
                Some(Component::Normal(_)) => {
                    parts.pop();
                }
                _ => {
                    leading_dotdot += 1;
                }
            },
            other => parts.push(other),
        }
    }
    let mut folded = PathBuf::new();
    for component in parts {
        folded.push(component.as_os_str());
    }
    (folded, leading_dotdot)
}

/// Canonical workspace root: the process working directory.
///
/// The MCP server is launched at the workspace root and every existing
/// surface (graph DB path, manifest, mounts.yaml, ignore matching) resolves
/// relative to it, so it is the confinement boundary. Mounted backend roots
/// need no separate allowlist: `resolve_backend` rewrites mount-owned paths
/// under the working directory BEFORE the prefix check.
pub fn workspace_root() -> McpResult<PathBuf> {
    std::env::current_dir().map_err(McpError::Io)
}

/// Reject `path` unless it resolves inside the workspace root.
///
/// Returns the canonical path on success. The error names BOTH the rejected
/// path and the allowed root so an agent can self-correct.
pub fn ensure_inside_workspace<P: AsRef<Path>>(requested: P) -> McpResult<PathBuf> {
    let requested = requested.as_ref();
    let root = workspace_root()?;
    match ensure_inside_root_impl(requested, &root, Requirement::Exists) {
        Ok(path) => Ok(path),
        Err(mut err) => {
            annotate_with_root(&mut err, &root);
            Err(err)
        }
    }
}

/// Best-effort confinement for tools that answer non-existent paths.
///
/// Identical boundaries to [`ensure_inside_workspace`], but a path that does
/// not exist is still accepted when its lexical fold lands inside the root
/// (the caller reports it as not-found / uncached). Returns the path to use
/// for filesystem access when it exists, or the folded absolute path when it
/// does not.
pub fn ensure_inside_workspace_lenient<P: AsRef<Path>>(requested: P) -> McpResult<PathBuf> {
    let requested = requested.as_ref();
    let root = workspace_root()?;
    match ensure_inside_root_impl(requested, &root, Requirement::BestEffort) {
        Ok(path) => Ok(path),
        Err(mut err) => {
            annotate_with_root(&mut err, &root);
            Err(err)
        }
    }
}

#[derive(Clone, Copy)]
enum Requirement {
    /// Path must exist: canonicalize errors propagate.
    Exists,
    /// Missing paths are allowed through with the folded absolute form.
    BestEffort,
}

fn escape_error(requested: &Path) -> McpError {
    McpError::Protocol(format!(
        "path escapes the workspace: '{}'",
        requested.display()
    ))
}

/// Canonical form of the deepest ancestor of `path` that exists on disk.
///
/// `path` itself usually does not exist (lenient-branch callers); the walk
/// starts at its parent and climbs. At least one ancestor always exists in
/// practice (the workspace root), so the `None` arm is a defensive fallback.
fn canonicalize_deepest_existing(path: &Path) -> PathBuf {
    let mut current = path.parent().map(Path::to_path_buf).unwrap_or_default();
    loop {
        match current.canonicalize() {
            Ok(canonical) => return canonical,
            Err(_) => {
                if !current.pop() {
                    return path.to_path_buf();
                }
            }
        }
    }
}

fn ensure_inside_root_impl(
    requested: &Path,
    root: &Path,
    requirement: Requirement,
) -> McpResult<PathBuf> {
    // ── Layer 1: lexical fold (rejects '../' escapes before any syscall) ──
    let (folded, leading_dotdot) = fold_lexical(requested);
    if leading_dotdot > 0 {
        return Err(escape_error(requested));
    }

    let absolute = if folded.is_absolute() {
        folded
    } else {
        root.join(folded)
    };

    // ── Layer 2: canonicalize (resolves symlinks) + prefix check ──
    let canonical_root = root.canonicalize().map_err(McpError::Io)?;
    let canonical = match absolute.canonicalize() {
        Ok(path) => path,
        Err(err) => match requirement {
            Requirement::Exists => return Err(McpError::Io(err)),
            Requirement::BestEffort => {
                // Path does not exist (yet). Compare against the canonical
                // form of the deepest EXISTING ancestor: comparing the raw
                // join against the canonicalized root false-positives when a
                // workspace component itself is a symlink (e.g. scratch
                // dirs under a symlinked /var/tmp). A deep symlink chain
                // under a missing leaf cannot escape (the leaf is missing),
                // and the strict check above covers every existing path.
                let best = canonicalize_deepest_existing(&absolute);
                if !best.starts_with(&canonical_root) {
                    return Err(escape_error(requested));
                }
                return Ok(absolute);
            }
        },
    };

    if !canonical.starts_with(&canonical_root) {
        return Err(escape_error(requested));
    }
    Ok(canonical)
}

fn annotate_with_root(err: &mut McpError, root: &Path) {
    if let McpError::Protocol(message) = err {
        message.push_str(&format!(
            " (MCP filesystem access is confined to the workspace root '{}')",
            root.display()
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap()
    }

    struct Cwd {
        _guard: std::sync::MutexGuard<'static, ()>,
        prev: PathBuf,
        dir: tempfile::TempDir,
    }

    impl Cwd {
        fn new() -> Self {
            let guard = lock();
            let prev = std::env::current_dir().unwrap();
            let dir = tempfile::TempDir::new().unwrap();
            std::env::set_current_dir(dir.path()).unwrap();
            Self {
                _guard: guard,
                prev,
                dir,
            }
        }

        fn path(&self) -> &Path {
            self.dir.path()
        }
    }

    impl Drop for Cwd {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.prev).ok();
        }
    }

    #[test]
    fn folds_dots_and_parent_components() {
        let (folded, leading) = fold_lexical(Path::new("src/./a/../b.rs"));
        assert_eq!(folded, PathBuf::from("src/b.rs"));
        assert_eq!(leading, 0);

        let (folded, leading) = fold_lexical(Path::new("../../etc/passwd"));
        assert_eq!(folded, PathBuf::from("etc/passwd"));
        assert_eq!(leading, 2);
    }

    #[test]
    fn accepts_in_tree_relative_and_absolute_paths() {
        let fx = Cwd::new();
        std::fs::write(fx.path().join("in_tree.rs"), "x").unwrap();

        let canonical = ensure_inside_workspace("in_tree.rs").unwrap();
        assert_eq!(
            canonical,
            fx.path().join("in_tree.rs").canonicalize().unwrap()
        );

        let absolute = fx.path().join("in_tree.rs");
        let canonical = ensure_inside_workspace(absolute.to_str().unwrap()).unwrap();
        assert_eq!(canonical, absolute.canonicalize().unwrap());
    }

    #[test]
    fn rejects_out_of_tree_absolute_and_dotdot_escapes_with_root_named() {
        let fx = Cwd::new();

        for hostile in [
            "/etc/passwd".to_string(),
            "../outside.txt".to_string(),
            "src/../../../etc/passwd".to_string(),
        ] {
            let err = ensure_inside_workspace(&hostile).unwrap_err();
            let message = err.to_string();
            assert!(
                message.contains("path escapes the workspace"),
                "hostile path {hostile} must be rejected as an escape: {message}"
            );
            assert!(
                message.contains(&fx.path().to_string_lossy().to_string()),
                "rejection of {hostile} must name the workspace root: {message}"
            );
        }
    }

    #[test]
    fn symlink_pointing_outside_the_tree_is_rejected() {
        let fx = Cwd::new();
        let outside = tempfile::TempDir::new().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            outside.path().join("secret.txt"),
            fx.path().join("innocent"),
        )
        .unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(
            outside.path().join("secret.txt"),
            fx.path().join("innocent"),
        )
        .unwrap();

        let err = ensure_inside_workspace("innocent").unwrap_err();
        assert!(
            err.to_string().contains("path escapes the workspace"),
            "symlink escape must be rejected: {err}"
        );
    }

    #[test]
    fn lenient_missing_in_tree_path_passes_and_out_of_tree_still_rejected() {
        let fx = Cwd::new();

        let missing = ensure_inside_workspace_lenient("not/on/disk/yet.rs").unwrap();
        assert_eq!(missing, fx.path().join("not/on/disk/yet.rs"));

        let err = ensure_inside_workspace_lenient("/etc/passwd").unwrap_err();
        assert!(err.to_string().contains("path escapes the workspace"));

        let err = ensure_inside_workspace_lenient("../../../etc/passwd").unwrap_err();
        assert!(err.to_string().contains("path escapes the workspace"));
    }

    #[test]
    fn missing_relative_path_errors_on_strict_canonicalization() {
        let _fx = Cwd::new();
        let err = ensure_inside_workspace("not/on/disk.rs").unwrap_err();
        assert!(
            !err.to_string().contains("path escapes the workspace"),
            "a missing in-tree path must surface the canonicalize error, got: {err}"
        );
    }
}
