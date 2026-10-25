//! Git hook installation — auto-update Hilo metadata on commit and pull.
//!
//! When `hilo init` runs (without `--no-hooks`), it installs two git hooks
//! into `.git/hooks/`:
//! - **post-commit** — runs `hilo graph warm --changed` (incremental parse)
//!   when Hilo is on `PATH`.
//! - **post-merge** — on pull, checks for `.vfs/.dirty`; if present and Hilo
//!   is installed, runs full `hilo graph warm` and deletes the marker.
//!
//! GAP-087: both hooks are no-ops when the `hilo` executable is absent — they
//! exit 0 and touch nothing. Neither creates a `.vfs/.dirty` marker merely
//! because Hilo is missing: a hook must never fail a commit, and must not
//! leave behind state that only a Hilo which is not installed could clear.
//!
//! DF-WARPFS-83: the `.vfs/.dirty` marker protocol is retired. The old
//! post-merge body waited for a marker that nothing ever wrote (dead code —
//! the README promised a recovery path that could never fire). Staleness is
//! now carried by the `.vfs/graph/state.json` freshness ledger, written by
//! every completed `graph warm`, and post-merge heals a pulled graph by
//! running a full warm itself whenever the pull changed graph-relevant files.
//!
//! Both hooks use `### HILO` / `### /HILO` block markers so they can be safely
//! appended to existing hooks without overwriting content.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

/// Marker delimiting the Hilo block inside a hook file.
const HILO_MARKER_START: &str = "### HILO";
const HILO_MARKER_END: &str = "### /HILO";

/// post-commit hook content — incremental graph warm when Hilo is installed.
///
/// GAP-087: with no `hilo` on `PATH` this is a pure no-op that exits 0. It must
/// not create a `.vfs/.dirty` marker: nothing installed could clear it, and a
/// failed write would make the hook fail the commit.
///
/// DF-WARPFS-83: the `.vfs/graph/state.json` freshness ledger replaces the
/// dead `.vfs/.dirty` marker protocol. A completed warm stamps state.json
/// with the git HEAD it saw, so `graph warm` (and post-merge, via warm) can
/// detect "HEAD moved since the graph was built" without any pre-merge
/// marker writer. This hook never writes the ledger itself — only warm does.
const POST_COMMIT_HOOK: &str = r#"#!/bin/sh
### HILO — auto-update metadata on commit
if command -v hilo >/dev/null 2>&1; then
    hilo graph warm --changed 2>/dev/null || true
fi
### /HILO
"#;

/// post-merge hook content — sync metadata on pull.
///
/// DF-WARPFS-83: a pull that changed graph-relevant state must heal the
/// graph. The old protocol waited for a `.vfs/.dirty` marker that nothing
/// ever wrote (dead code — the consumer could never fire). The freshness
/// ledger `.vfs/graph/state.json` now carries the last-warmed git HEAD, and
/// `graph warm` detects HEAD drift itself, so this hook simply runs a full
/// warm whenever a pull brought in commits that could change the graph
/// (new/changed/deleted source or inventory files). Warm's own staleness
/// pass then rebuilds past the mtime-preserving pull and re-stamps the
/// ledger; an empty pull skips the work via the normal all-cached path.
///
/// GAP-087 still holds: with no `hilo` on `PATH` this exits 0 and touches
/// nothing. The listed paths are the graph's inputs (per the inventory
/// policy, `.vfs/manifest.yaml` and `.vfs/graph/edges.jsonl` are tracked);
/// `--diff-filter` covers added/copied/deleted/modified/renamed/type
/// changes and exits 0 (no output) on an empty diff.
const POST_MERGE_HOOK: &str = r#"#!/bin/sh
### HILO — sync metadata on pull
if command -v hilo >/dev/null 2>&1; then
    if [ -n "$(git diff --diff-filter=ACDMRT --name-only HEAD@{1} HEAD -- '*.rs' '*.go' '*.py' '*.ts' '*.tsx' '*.js' '*.jsx' '*.java' '*.c' '*.h' '*.cpp' '*.hpp' '*.rb' '*.cs' '*.kt' '*.php' '*.swift' '*.ex' '*.exs' '*.hs' '*.erl' '*.scala' '*.zig' '*.lua' '*.dart' '*.clj' '*.ml' '*.r' '*.jl' '*.elm' '*.nim' '*.tf' '.vfs/manifest.yaml' '.vfs/graph/edges.jsonl' 2>/dev/null)" ]; then
        echo "Hilo: graph-relevant changes pulled — updating metadata"
        hilo graph warm 2>/dev/null || true
        echo "Hilo: metadata updated"
    fi
fi
### /HILO
"#;

/// Resolve the hooks directory through Git so linked worktrees, submodules,
/// and `core.hooksPath` are handled like Git itself handles them.
fn resolve_hooks_dir(project_dir: &Path, git_dir: &Path) -> Result<PathBuf> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(project_dir)
        .args(["rev-parse", "--path-format=absolute", "--git-path", "hooks"])
        .output();

    match output {
        Ok(output) if output.status.success() => {
            let path =
                String::from_utf8(output.stdout).context("git returned a non-UTF-8 hooks path")?;
            let path = path.trim();
            if path.is_empty() {
                anyhow::bail!("git returned an empty hooks path");
            }
            let path = PathBuf::from(path);
            Ok(if path.is_absolute() {
                path
            } else {
                project_dir.join(path)
            })
        }
        Ok(_output) if git_dir.is_dir() => {
            // Preserve the traditional `.git/hooks` behavior if Git is
            // unavailable or the directory is a lightweight test fixture.
            eprintln!("warning: could not resolve Git hooks path; using .git/hooks");
            Ok(git_dir.join("hooks"))
        }
        Ok(output) => anyhow::bail!(
            "failed to resolve Git hooks path: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        Err(error) if git_dir.is_dir() => {
            eprintln!("warning: {error}; using .git/hooks");
            Ok(git_dir.join("hooks"))
        }
        Err(error) => Err(error).context("failed to resolve Git hooks path for worktree"),
    }
}

/// Resolve the repository's git COMMON directory (the shared `.git` that
/// linked worktrees borrow from), so a legitimate hooks path inside the
/// repository is not mistaken for a host-wide one.
fn resolve_git_common_dir(project_dir: &Path, git_dir: &Path) -> PathBuf {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(project_dir)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output();

    match output {
        Ok(output) if output.status.success() => {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if path.is_empty() {
                git_dir.to_path_buf()
            } else {
                let path = PathBuf::from(path);
                if path.is_absolute() {
                    path
                } else {
                    project_dir.join(path)
                }
            }
        }
        _ => git_dir.to_path_buf(),
    }
}

/// True when `candidate` is `root` or lies beneath it.
///
/// Both sides are canonicalized when possible so symlinked temp roots
/// (`/tmp` vs `/private/tmp`, `/var/tmp` bind mounts) cannot fake a match.
fn is_within(candidate: &Path, root: &Path) -> bool {
    let candidate = candidate
        .canonicalize()
        .unwrap_or_else(|_| candidate.to_path_buf());
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    candidate.starts_with(&root)
}

/// Copy an existing hook beside itself as `<name>.bak-<epoch-milliseconds>`.
///
/// A hook frequently carries hand-written logic the user (or another tool)
/// put there; rewriting it without keeping the pre-image makes the change
/// unrecoverable. The backup keeps the exact bytes.
///
/// DF-WARPFS-162: the stamp is millisecond precision so two installs in the
/// same second (the norm for back-to-back `hilo init` runs in scripts or
/// tests) cannot overwrite the first generation's pre-image — seconds
/// precision silently clobbered the earlier backup and re-opened exactly the
/// unrecoverable-rewrite defect this backup exists to prevent.
fn backup_hook(hook_path: &Path) -> Result<PathBuf> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let name = hook_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "hook".to_string());
    let backup = hook_path.with_file_name(format!("{name}.bak-{stamp}"));
    std::fs::copy(hook_path, &backup)
        .with_context(|| format!("failed to back up {}", hook_path.display()))?;
    Ok(backup)
}

/// Install both post-commit and post-merge hooks into Git's resolved hooks directory.
///
/// If `.git/` does not exist (not a git repo), prints a warning and returns
/// `Ok(())` — `hilo init` should not fail when run outside a git repo.
///
/// DF-WARPFS-162: a `core.hooksPath` that resolves OUTSIDE the repository is
/// refused. It is a host-wide/shared hook directory (every repository on the
/// machine runs those hooks), so initialising one throwaway checkout must not
/// mutate it. In-repo hooks paths — including `.githooks/` and linked-worktree
/// common dirs — are still honoured.
pub fn install_hooks(project_dir: &Path) -> Result<()> {
    let git_dir = project_dir.join(".git");
    if !git_dir.exists() {
        eprintln!("warning: .git/ not found — skipping git hook installation");
        eprintln!("  Run 'git init' first, then 'hilo init' to enable hooks.");
        return Ok(());
    }

    let hooks_dir = resolve_hooks_dir(project_dir, &git_dir)?;
    let common_dir = resolve_git_common_dir(project_dir, &git_dir);
    if !is_within(&hooks_dir, project_dir) && !is_within(&hooks_dir, &common_dir) {
        eprintln!(
            "warning: git hooks path {} is outside this repository — skipping hook installation",
            hooks_dir.display()
        );
        eprintln!(
            "  core.hooksPath points at a shared hook directory: writing there would change \
             every repository on this machine."
        );
        eprintln!(
            "  Point core.hooksPath inside this repository, unset it, or add the HILO block \
             to {} yourself.",
            hooks_dir.join("post-commit").display()
        );
        return Ok(());
    }

    if !hooks_dir.exists() {
        std::fs::create_dir_all(&hooks_dir)
            .with_context(|| format!("failed to create {}", hooks_dir.display()))?;
    }

    install_hook(&hooks_dir.join("post-commit"), POST_COMMIT_HOOK)?;
    install_hook(&hooks_dir.join("post-merge"), POST_MERGE_HOOK)?;

    println!("Installed git hooks: post-commit, post-merge");
    Ok(())
}

/// Install or update a single hook file.
///
/// - If the file does not exist, write the hook content and make it executable.
/// - If the file exists it is first copied to `<name>.bak-<stamp>`, then the
///   Hilo block is appended (no existing Hilo block) or replaced in place
///   (idempotent — re-running `hilo init` updates stale hooks).
fn install_hook(hook_path: &Path, hook_content: &str) -> Result<()> {
    if hook_path.exists() {
        let existing = std::fs::read_to_string(hook_path)
            .with_context(|| format!("failed to read {}", hook_path.display()))?;

        let backup = backup_hook(hook_path)?;
        println!(
            "  Saved previous hook to {}",
            backup.file_name().unwrap_or_default().to_string_lossy()
        );

        if has_hilo_block(&existing) {
            // Replace the existing Hilo block with fresh content.
            let updated = replace_hilo_block(&existing, hook_content);
            std::fs::write(hook_path, updated)
                .with_context(|| format!("failed to write {}", hook_path.display()))?;
            println!(
                "  Updated Hilo block in {}",
                hook_path.file_name().unwrap_or_default().to_string_lossy()
            );
        } else {
            // Append Hilo block to existing hook.
            let mut new_content = existing;
            if !new_content.ends_with('\n') {
                new_content.push('\n');
            }
            new_content.push('\n');
            new_content.push_str(hook_content);
            std::fs::write(hook_path, new_content)
                .with_context(|| format!("failed to write {}", hook_path.display()))?;
            println!(
                "  Appended Hilo block to {}",
                hook_path.file_name().unwrap_or_default().to_string_lossy()
            );
        }
    } else {
        // Write fresh hook file.
        std::fs::write(hook_path, hook_content)
            .with_context(|| format!("failed to write {}", hook_path.display()))?;
        println!(
            "  Created {}",
            hook_path.file_name().unwrap_or_default().to_string_lossy()
        );
    }

    // Make the hook executable on Unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(hook_path, perms).ok();
    }

    Ok(())
}

/// Check if `content` already contains a Hilo block.
fn has_hilo_block(content: &str) -> bool {
    content.contains(HILO_MARKER_START) && content.contains(HILO_MARKER_END)
}

/// Replace the Hilo block in `existing` with `new_block`.
///
/// `new_block` must contain both markers.
fn replace_hilo_block(existing: &str, new_block: &str) -> String {
    // Find the start and end of the existing Hilo block.
    let start_idx = match existing.find(HILO_MARKER_START) {
        Some(i) => i,
        None => return existing.to_string(),
    };

    // Walk backwards from start_idx to include the shebang or preceding lines
    // that are part of the hook. We only replace from the marker.
    // Find the beginning of the line containing the marker.
    let line_start = existing[..start_idx]
        .rfind('\n')
        .map(|i| i + 1)
        .unwrap_or(0);

    let end_idx = match existing[start_idx..].find(HILO_MARKER_END) {
        Some(i) => start_idx + i + HILO_MARKER_END.len(),
        None => return existing.to_string(),
    };

    // Find the end of the line containing the end marker.
    let block_end = existing[end_idx..]
        .find('\n')
        .map(|i| end_idx + i + 1)
        .unwrap_or(existing.len());

    let before = &existing[..line_start];
    let after = &existing[block_end..];

    let mut result = String::new();
    result.push_str(before);
    result.push_str(new_block);
    result.push_str(after);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Create a temp project dir with a `.git/hooks/` subdirectory.
    ///
    /// DF-WARPFS-162: pin `core.hooksPath` to this fixture's own `.git/hooks`
    /// (absolute) so a host-global `core.hooksPath` cannot leak into the test
    /// and redirect hook installation to a shared directory.
    fn make_temp_git_project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let output = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .arg(dir.path())
            .output()
            .expect("git must be available for hook tests");
        assert!(
            output.status.success(),
            "git init failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let hooks = dir.path().join(".git").join("hooks");
        let pin = std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["config", "core.hooksPath"])
            .arg(&hooks)
            .output()
            .unwrap();
        assert!(
            pin.status.success(),
            "git config core.hooksPath failed: {}",
            String::from_utf8_lossy(&pin.stderr)
        );
        dir
    }

    #[test]
    fn test_install_hooks_creates_post_commit() {
        let dir = make_temp_git_project();
        install_hooks(dir.path()).unwrap();

        let hook = dir.path().join(".git").join("hooks").join("post-commit");
        assert!(hook.exists(), "post-commit hook should exist");
        let content = std::fs::read_to_string(&hook).unwrap();
        assert!(content.contains("### HILO"), "should contain HILO marker");
        assert!(
            content.contains("hilo graph warm --changed"),
            "should call warm --changed"
        );
    }

    #[test]
    fn test_install_hooks_creates_post_merge() {
        let dir = make_temp_git_project();
        install_hooks(dir.path()).unwrap();

        let hook = dir.path().join(".git").join("hooks").join("post-merge");
        assert!(hook.exists(), "post-merge hook should exist");
        let content = std::fs::read_to_string(&hook).unwrap();
        assert!(content.contains("### HILO"), "should contain HILO marker");
        assert!(
            content.contains("HEAD@{1}"),
            "post-merge must diff the pull range (HEAD@{{1}}..HEAD), got: {content}"
        );
        assert!(
            content.contains("graph warm"),
            "post-merge must run a full warm on graph-relevant pulls"
        );
    }

    #[test]
    fn test_install_hooks_appends_to_existing() {
        let dir = make_temp_git_project();
        let hook_path = dir.path().join(".git").join("hooks").join("post-commit");

        // Write existing hook content.
        std::fs::write(&hook_path, "#!/bin/sh\necho 'my hook'\n").unwrap();

        install_hooks(dir.path()).unwrap();

        let content = std::fs::read_to_string(&hook_path).unwrap();
        assert!(
            content.contains("my hook"),
            "existing hook content should be preserved"
        );
        assert!(
            content.contains("### HILO"),
            "Hilo block should be appended"
        );
    }

    #[test]
    fn test_install_hooks_idempotent_replaces_block() {
        let dir = make_temp_git_project();
        let hook_path = dir.path().join(".git").join("hooks").join("post-commit");

        // First install.
        install_hooks(dir.path()).unwrap();
        let first_content = std::fs::read_to_string(&hook_path).unwrap();

        // Second install — should replace the block, not duplicate.
        install_hooks(dir.path()).unwrap();
        let second_content = std::fs::read_to_string(&hook_path).unwrap();

        let first_count = first_content.matches("### HILO").count();
        let second_count = second_content.matches("### HILO").count();
        assert_eq!(
            first_count, 1,
            "first install should have exactly one Hilo block"
        );
        assert_eq!(
            second_count, 1,
            "second install should still have exactly one Hilo block"
        );
    }

    #[test]
    fn test_install_hooks_missing_git_dir_warns() {
        let dir = tempfile::tempdir().unwrap();
        // No .git/ directory.
        let result = install_hooks(dir.path());
        assert!(result.is_ok(), "should not fail when .git/ missing");
        assert!(
            !dir.path().join(".git").exists(),
            "should not create .git/ dir"
        );
    }

    #[test]
    fn test_install_hooks_supports_custom_core_hooks_path() {
        let repo = make_temp_git_project();
        let config = std::process::Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["config", "core.hooksPath", ".hilo-hooks"])
            .output()
            .unwrap();
        assert!(
            config.status.success(),
            "git config failed: {}",
            String::from_utf8_lossy(&config.stderr)
        );

        install_hooks(repo.path()).unwrap();

        assert!(repo.path().join(".hilo-hooks/post-commit").is_file());
        assert!(!repo.path().join(".git/hooks/post-commit").exists());
    }

    // ─────────── DF-WARPFS-162: never mutate a shared hooks directory ───────────

    #[test]
    fn test_install_hooks_refuses_hooks_path_outside_repository() {
        let repo = make_temp_git_project();
        let foreign = tempfile::tempdir().unwrap();
        let shared_hooks = foreign.path().join("shared-hooks");
        std::fs::create_dir_all(&shared_hooks).unwrap();
        let original = "#!/bin/sh\necho 'shared fleet hook'\n";
        std::fs::write(shared_hooks.join("post-commit"), original).unwrap();

        let config = std::process::Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["config", "core.hooksPath"])
            .arg(&shared_hooks)
            .output()
            .unwrap();
        assert!(
            config.status.success(),
            "git config failed: {}",
            String::from_utf8_lossy(&config.stderr)
        );

        install_hooks(repo.path()).unwrap();

        assert_eq!(
            std::fs::read_to_string(shared_hooks.join("post-commit")).unwrap(),
            original,
            "a hook outside the repository must stay byte-identical"
        );
        assert!(
            !shared_hooks.join("post-merge").exists(),
            "no post-merge hook may be created outside the repository"
        );
        assert_eq!(
            std::fs::read_dir(&shared_hooks).unwrap().count(),
            1,
            "the shared hooks directory must gain no files (no hook, no .bak)"
        );
        assert!(
            !repo.path().join(".git/hooks/post-commit").exists(),
            "refusing must not silently fall back to .git/hooks"
        );
    }

    #[test]
    fn test_install_hooks_allows_absolute_hooks_path_inside_repository() {
        let repo = make_temp_git_project();
        let in_repo = repo.path().join("githooks");
        std::fs::create_dir_all(&in_repo).unwrap();

        let config = std::process::Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["config", "core.hooksPath"])
            .arg(&in_repo)
            .output()
            .unwrap();
        assert!(
            config.status.success(),
            "git config failed: {}",
            String::from_utf8_lossy(&config.stderr)
        );

        install_hooks(repo.path()).unwrap();

        assert!(
            in_repo.join("post-commit").is_file(),
            "an absolute hooks path inside the repository stays supported"
        );
        assert!(in_repo.join("post-merge").is_file());
        assert!(!repo.path().join(".git/hooks/post-commit").exists());
    }

    #[test]
    fn test_install_hooks_backs_up_existing_hook_bytes() {
        let repo = make_temp_git_project();
        let hooks = repo.path().join(".git").join("hooks");
        let hook_path = hooks.join("post-commit");
        let original = "#!/bin/sh\necho 'my hook'\n### local tweak\n";
        std::fs::write(&hook_path, original).unwrap();

        install_hooks(repo.path()).unwrap();

        let backups: Vec<std::path::PathBuf> = std::fs::read_dir(&hooks)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().starts_with("post-commit.bak-"))
                    .unwrap_or(false)
            })
            .collect();
        assert_eq!(backups.len(), 1, "exactly one pre-image backup expected");
        assert_eq!(
            std::fs::read_to_string(&backups[0]).unwrap(),
            original,
            "backup must be the byte-identical pre-image"
        );

        let current = std::fs::read_to_string(&hook_path).unwrap();
        assert!(current.contains("my hook"), "existing content preserved");
        assert!(current.contains("### HILO"), "Hilo block installed");
    }

    #[test]
    fn test_install_hooks_backs_up_before_replacing_hilo_block() {
        let repo = make_temp_git_project();
        let hooks = repo.path().join(".git").join("hooks");
        let hook_path = hooks.join("post-commit");

        install_hooks(repo.path()).unwrap();
        let first = std::fs::read_to_string(&hook_path).unwrap();

        install_hooks(repo.path()).unwrap();

        let backups: Vec<std::path::PathBuf> = std::fs::read_dir(&hooks)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().starts_with("post-commit.bak-"))
                    .unwrap_or(false)
            })
            .collect();
        assert_eq!(
            backups.len(),
            1,
            "the rewritten hook must have a pre-image backup"
        );
        assert_eq!(
            std::fs::read_to_string(&backups[0]).unwrap(),
            first,
            "the backup holds the pre-rewrite content"
        );
        assert_eq!(
            std::fs::read_to_string(&hook_path)
                .unwrap()
                .matches("### HILO")
                .count(),
            1,
            "rewrite stays idempotent"
        );
    }

    #[test]
    fn test_install_hooks_supports_linked_worktree_gitfile() {
        let repo = make_temp_git_project();
        let commit = std::process::Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args([
                "-c",
                "user.name=Hilo Test",
                "-c",
                "user.email=hilo-test@example.invalid",
            ])
            .args(["commit", "--allow-empty", "-m", "seed"])
            .output()
            .unwrap();
        assert!(
            commit.status.success(),
            "seed commit failed: {}",
            String::from_utf8_lossy(&commit.stderr)
        );

        let linked = repo.path().join("linked worktree");
        let add = std::process::Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["worktree", "add", "--detach"])
            .arg(&linked)
            .arg("HEAD")
            .output()
            .unwrap();
        assert!(
            add.status.success(),
            "git worktree add failed: {}",
            String::from_utf8_lossy(&add.stderr)
        );
        assert!(
            linked.join(".git").is_file(),
            "worktree .git must be a file"
        );

        install_hooks(&linked).unwrap();

        let resolved = std::process::Command::new("git")
            .arg("-C")
            .arg(&linked)
            .args(["rev-parse", "--path-format=absolute", "--git-path", "hooks"])
            .output()
            .unwrap();
        assert!(resolved.status.success());
        let hooks_dir =
            std::path::PathBuf::from(String::from_utf8(resolved.stdout).unwrap().trim());
        assert!(hooks_dir.join("post-commit").is_file());
        assert!(hooks_dir.join("post-merge").is_file());
    }

    #[test]
    fn test_has_hilo_block_detection() {
        assert!(has_hilo_block("### HILO\nstuff\n### /HILO"));
        assert!(has_hilo_block("before\n### HILO\n### /HILO\nafter"));
        assert!(!has_hilo_block("just some content"));
        assert!(!has_hilo_block("### HILO only start"));
    }

    #[test]
    fn test_replace_hilo_block_preserves_surrounding() {
        let existing = "#!/bin/sh\necho 'before'\n### HILO\nold\n### /HILO\necho 'after'\n";
        let new_block = "### HILO\nnew content\n### /HILO\n";
        let result = replace_hilo_block(existing, new_block);
        assert!(result.contains("echo 'before'"), "before content preserved");
        assert!(result.contains("echo 'after'"), "after content preserved");
        assert!(result.contains("new content"), "new content inserted");
        assert!(!result.contains("old\n"), "old content removed");
    }

    #[test]
    fn test_post_commit_hook_has_no_dirty_side_effect() {
        let dir = make_temp_git_project();
        install_hooks(dir.path()).unwrap();
        let content =
            std::fs::read_to_string(dir.path().join(".git").join("hooks").join("post-commit"))
                .unwrap();
        assert!(
            content.contains("command -v hilo"),
            "should check if hilo is installed"
        );
        // GAP-087: post-commit must never write the dirty marker — a marker
        // created while Hilo is absent can only be cleared by Hilo.
        assert!(
            !content.contains(".vfs/.dirty"),
            "post-commit must not create a dirty marker"
        );
        assert!(
            !content.contains("echo \"stale\""),
            "post-commit must not write the stale marker"
        );
    }

    #[test]
    fn test_post_merge_hook_warms_on_graph_relevant_pull() {
        let dir = make_temp_git_project();
        install_hooks(dir.path()).unwrap();
        let content =
            std::fs::read_to_string(dir.path().join(".git").join("hooks").join("post-merge"))
                .unwrap();
        // DF-WARPFS-83: the dead `.vfs/.dirty` consumer is gone — the hook
        // heals the graph itself by running a full warm over the pulled
        // range. Warm's own freshness pass (state.json ledger) does the
        // staleness detection.
        assert!(
            !content.contains(".vfs/.dirty"),
            "the dead dirty-marker protocol must be gone"
        );
        assert!(
            content.contains("--diff-filter=ACDMRT"),
            "must detect added/changed/deleted/renamed graph inputs"
        );
    }

    #[test]
    fn test_hooks_are_executable_on_unix() {
        let dir = make_temp_git_project();
        install_hooks(dir.path()).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let post_commit = dir.path().join(".git").join("hooks").join("post-commit");
            let perms = std::fs::metadata(&post_commit)
                .unwrap()
                .permissions()
                .mode();
            assert!(
                perms & 0o100 != 0,
                "post-commit should be executable (mode={:o})",
                perms
            );
        }
    }

    #[test]
    fn test_existing_hook_without_newline_before_append() {
        let dir = make_temp_git_project();
        let hook_path = dir.path().join(".git").join("hooks").join("post-commit");

        // Existing hook without trailing newline.
        let mut f = std::fs::File::create(&hook_path).unwrap();
        f.write_all(b"#!/bin/sh\necho hi").unwrap();
        drop(f);

        install_hooks(dir.path()).unwrap();
        let content = std::fs::read_to_string(&hook_path).unwrap();
        assert!(content.contains("echo hi"), "original content preserved");
        assert!(
            content.contains("### HILO"),
            "Hilo block appended correctly"
        );
        // Ensure there's proper newline separation.
        assert!(
            content.contains("hi\n\n#!/bin/sh\n### HILO"),
            "should have newline between existing and Hilo block"
        );
    }

    // ─────────────────── GAP-087: hook runtime behavior ───────────────────
    //
    // These tests execute the installed hook scripts with `/bin/sh`, with
    // `PATH` pointed at a private temp directory so the host's `hilo` (or its
    // absence) can never influence the result.

    /// Temp dir used as the hook's `PATH`, optionally holding a stub `hilo`.
    #[cfg(unix)]
    fn make_path_dir(stub_hilo: Option<&str>) -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        if let Some(body) = stub_hilo {
            let stub = dir.path().join("hilo");
            std::fs::write(&stub, body).unwrap();
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        dir
    }

    /// Run an installed hook with `sh`, in `project`, with `PATH=path`.
    #[cfg(unix)]
    fn run_installed_hook(
        project: &Path,
        hook: &str,
        path: &std::ffi::OsStr,
    ) -> std::process::Output {
        let hook_path = project.join(".git").join("hooks").join(hook);
        std::process::Command::new("/bin/sh")
            .arg(&hook_path)
            .current_dir(project)
            .env("PATH", path)
            .output()
            .unwrap_or_else(|e| panic!("failed to run {hook}: {e}"))
    }

    /// `PATH` for a hook run: `stub_dir` always FIRST, so a stub `hilo` wins
    /// over any host installation, optionally followed by the system binary
    /// directories the hook itself needs (`rm` is not a shell builtin).
    ///
    /// Tests that assert the absent-`hilo` behavior pass `with_system_dirs =
    /// false` — the hook's no-op path needs no external commands at all, so an
    /// empty `PATH` proves the behavior without the host's `hilo` in play.
    #[cfg(unix)]
    fn hook_path(stub_dir: &Path, with_system_dirs: bool) -> std::ffi::OsString {
        let mut path = std::ffi::OsString::from(stub_dir.as_os_str());
        if with_system_dirs {
            path.push(":/usr/local/bin:/usr/bin:/bin");
        }
        path
    }

    /// Body of a stub `hilo` that records its argv into `log`.
    #[cfg(unix)]
    fn stub_hilo_recording(log: &Path) -> String {
        format!("#!/bin/sh\necho \"$@\" >> {}\n", log.display())
    }

    /// A stub `hilo` that always fails — the hook must still exit 0.
    #[cfg(unix)]
    const STUB_HILO_FAILING: &str = "#!/bin/sh\nexit 1\n";

    #[cfg(unix)]
    #[test]
    fn test_post_commit_noop_without_hilo() {
        let dir = make_temp_git_project();
        install_hooks(dir.path()).unwrap();
        // `.vfs/` exists and is writable, so a marker would land here if the
        // hook still wrote one.
        std::fs::create_dir_all(dir.path().join(".vfs")).unwrap();
        let path_dir = make_path_dir(None);

        let out = run_installed_hook(
            dir.path(),
            "post-commit",
            &hook_path(path_dir.path(), false),
        );
        assert_eq!(
            out.status.code(),
            Some(0),
            "post-commit must exit 0 without hilo: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !dir.path().join(".vfs").join(".dirty").exists(),
            "post-commit must not create .vfs/.dirty when hilo is absent"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_post_commit_noop_without_hilo_or_vfs_dir() {
        let dir = make_temp_git_project();
        install_hooks(dir.path()).unwrap();
        let path_dir = make_path_dir(None);

        let out = run_installed_hook(
            dir.path(),
            "post-commit",
            &hook_path(path_dir.path(), false),
        );
        assert_eq!(
            out.status.code(),
            Some(0),
            "post-commit must exit 0 without hilo even when .vfs/ is missing: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !dir.path().join(".vfs").exists(),
            "post-commit must not create .vfs/ state"
        );
    }

    /// Create a commit in the temp project touching one file.
    #[cfg(unix)]
    fn commit_file(project: &Path, rel: &str, content: &str, msg: &str) {
        let path = project.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, content).unwrap();
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(project)
            .args([
                "-c",
                "user.name=Hilo Test",
                "-c",
                "user.email=hilo-test@example.invalid",
            ])
            .args(["add", rel])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git add failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(project)
            .args([
                "-c",
                "user.name=Hilo Test",
                "-c",
                "user.email=hilo-test@example.invalid",
            ])
            .args(["commit", "--quiet", "-m", msg])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git commit failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_post_merge_noop_without_hilo() {
        let dir = make_temp_git_project();
        install_hooks(dir.path()).unwrap();
        // `.vfs/` exists and is writable, so a marker would land here if the
        // hook still wrote one.
        std::fs::create_dir_all(dir.path().join(".vfs")).unwrap();
        let path_dir = make_path_dir(None);

        let out = run_installed_hook(dir.path(), "post-merge", &hook_path(path_dir.path(), false));
        assert_eq!(
            out.status.code(),
            Some(0),
            "post-merge must exit 0 without hilo: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !dir.path().join(".vfs").join(".dirty").exists(),
            "post-merge must not create .vfs/.dirty when hilo is absent"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_post_merge_without_hilo_leaves_preexisting_marker() {
        let dir = make_temp_git_project();
        install_hooks(dir.path()).unwrap();
        let marker = dir.path().join(".vfs").join(".dirty");
        std::fs::create_dir_all(dir.path().join(".vfs")).unwrap();
        std::fs::write(&marker, "stale\n").unwrap();
        let path_dir = make_path_dir(None);

        let out = run_installed_hook(dir.path(), "post-merge", &hook_path(path_dir.path(), false));
        assert_eq!(out.status.code(), Some(0));
        // Only an installed hilo may clear the marker — an absent one must not
        // silently drop the staleness signal.
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap(),
            "stale\n",
            "pre-existing marker must be untouched when hilo is absent"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_post_commit_exits_zero_when_hilo_fails() {
        let dir = make_temp_git_project();
        install_hooks(dir.path()).unwrap();
        let path_dir = make_path_dir(Some(STUB_HILO_FAILING));

        let out = run_installed_hook(dir.path(), "post-commit", &hook_path(path_dir.path(), true));
        assert_eq!(
            out.status.code(),
            Some(0),
            "a failing hilo must not fail the commit"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_post_commit_warms_when_hilo_present() {
        let dir = make_temp_git_project();
        install_hooks(dir.path()).unwrap();
        let log = dir.path().join("hilo-calls.log");
        let path_dir = make_path_dir(Some(&stub_hilo_recording(&log)));

        let out = run_installed_hook(dir.path(), "post-commit", &hook_path(path_dir.path(), true));
        assert_eq!(out.status.code(), Some(0));
        let calls = std::fs::read_to_string(&log).expect("stub hilo should have been invoked");
        assert_eq!(calls.trim(), "graph warm --changed");
    }

    /// DF-WARPFS-83: a pull that adds a source file must heal the graph —
    /// the hook runs a full `hilo graph warm` (simulated here with a stub
    /// that records its argv). The two-commit repo makes `HEAD@{1}` real:
    /// the hook diffs commit1..commit2 and sees the added `lib.rs`.
    #[cfg(unix)]
    #[test]
    fn test_post_merge_warms_when_source_file_pulled() {
        let dir = make_temp_git_project();
        commit_file(dir.path(), "seed.txt", "seed\n", "c1");
        commit_file(
            dir.path(),
            "src/lib.rs",
            "pub fn f() {}\n",
            "c2 adds source",
        );
        install_hooks(dir.path()).unwrap();
        let log = dir.path().join("hilo-calls.log");
        let path_dir = make_path_dir(Some(&stub_hilo_recording(&log)));

        let out = run_installed_hook(dir.path(), "post-merge", &hook_path(path_dir.path(), true));
        assert_eq!(
            out.status.code(),
            Some(0),
            "post-merge must not fail the pull: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let calls = std::fs::read_to_string(&log).expect("a source pull must invoke hilo warm");
        assert_eq!(calls.trim(), "graph warm");
        assert!(
            !dir.path().join(".vfs").join(".dirty").exists(),
            "the dead marker protocol stays dead — no .dirty may be written"
        );
    }

    /// DF-WARPFS-83 negative arm: a pull that touches only docs must NOT
    /// warm — the graph's inputs did not change, so the hook stays silent.
    #[cfg(unix)]
    #[test]
    fn test_post_merge_skips_docs_only_pull() {
        let dir = make_temp_git_project();
        commit_file(dir.path(), "seed.txt", "seed\n", "c1");
        commit_file(dir.path(), "docs/note.md", "docs\n", "c2 docs only");
        install_hooks(dir.path()).unwrap();
        let log = dir.path().join("hilo-calls.log");
        let path_dir = make_path_dir(Some(&stub_hilo_recording(&log)));

        let out = run_installed_hook(dir.path(), "post-merge", &hook_path(path_dir.path(), true));
        assert_eq!(out.status.code(), Some(0));
        assert!(
            !log.exists(),
            "a docs-only pull must not invoke hilo (no graph inputs changed)"
        );
    }

    /// A pull that updates the tracked graph inventory itself must warm:
    /// `.vfs/graph/edges.jsonl` is graph-relevant by definition.
    #[cfg(unix)]
    #[test]
    fn test_post_merge_warms_when_edges_inventory_pulled() {
        let dir = make_temp_git_project();
        commit_file(dir.path(), "seed.txt", "seed\n", "c1");
        commit_file(
            dir.path(),
            ".vfs/graph/edges.jsonl",
            "{\"from\":\"local:a.rs\",\"to\":\"local:b.rs\",\"relation\":\"imports\"}\n",
            "c2 inventory refresh",
        );
        install_hooks(dir.path()).unwrap();
        let log = dir.path().join("hilo-calls.log");
        let path_dir = make_path_dir(Some(&stub_hilo_recording(&log)));

        let out = run_installed_hook(dir.path(), "post-merge", &hook_path(path_dir.path(), true));
        assert_eq!(out.status.code(), Some(0));
        assert_eq!(
            std::fs::read_to_string(&log)
                .expect("inventory pull must invoke hilo warm")
                .trim(),
            "graph warm"
        );
    }

    /// A repo with no reflog entry for `HEAD@{1}` (fresh clone-style state)
    /// must exit 0 and stay silent — the diff gate fails closed to no-op.
    #[cfg(unix)]
    #[test]
    fn test_post_merge_exits_zero_without_reflog_history() {
        let dir = make_temp_git_project();
        install_hooks(dir.path()).unwrap();
        let log = dir.path().join("hilo-calls.log");
        let path_dir = make_path_dir(Some(&stub_hilo_recording(&log)));

        let out = run_installed_hook(dir.path(), "post-merge", &hook_path(path_dir.path(), true));
        assert_eq!(out.status.code(), Some(0));
        assert!(
            !log.exists(),
            "no pull range resolvable — the hook must not guess"
        );
    }
}
