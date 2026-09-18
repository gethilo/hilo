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
//! Both hooks use `### HILO` / `### /HILO` block markers so they can be safely
//! appended to existing hooks without overwriting content.

use std::path::Path;

use anyhow::{Context, Result};

/// Marker delimiting the Hilo block inside a hook file.
const HILO_MARKER_START: &str = "### HILO";
const HILO_MARKER_END: &str = "### /HILO";

/// post-commit hook content — incremental graph warm when Hilo is installed.
///
/// GAP-087: with no `hilo` on `PATH` this is a pure no-op that exits 0. It must
/// not create a `.vfs/.dirty` marker: nothing installed could clear it, and a
/// failed write would make the hook fail the commit.
const POST_COMMIT_HOOK: &str = r#"#!/bin/sh
### HILO — auto-update metadata on commit
if command -v hilo >/dev/null 2>&1; then
    hilo graph warm --changed 2>/dev/null || true
fi
### /HILO
"#;

/// post-merge hook content — sync metadata on pull.
///
/// GAP-087: with no `hilo` on `PATH` this exits 0 and leaves `.vfs/.dirty`
/// untouched — only an installed Hilo clears the staleness marker.
const POST_MERGE_HOOK: &str = r#"#!/bin/sh
### HILO — sync metadata on pull
if [ -f .vfs/.dirty ] && command -v hilo >/dev/null 2>&1; then
    echo "Hilo: dirty marker found — updating metadata"
    hilo graph warm 2>/dev/null || true
    rm -f .vfs/.dirty
    echo "Hilo: metadata updated, dirty marker removed"
fi
### /HILO
"#;

/// Install both post-commit and post-merge hooks into `project_dir/.git/hooks/`.
///
/// If `.git/` does not exist (not a git repo), prints a warning and returns
/// `Ok(())` — `hilo init` should not fail when run outside a git repo.
pub fn install_hooks(project_dir: &Path) -> Result<()> {
    let git_dir = project_dir.join(".git");
    if !git_dir.exists() {
        eprintln!("warning: .git/ not found — skipping git hook installation");
        eprintln!("  Run 'git init' first, then 'hilo init' to enable hooks.");
        return Ok(());
    }

    let hooks_dir = git_dir.join("hooks");
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
/// - If the file exists but has no Hilo block, append the hook content.
/// - If the file exists and already has a Hilo block, replace the block with
///   fresh content (idempotent — re-running `hilo init` updates stale hooks).
fn install_hook(hook_path: &Path, hook_content: &str) -> Result<()> {
    if hook_path.exists() {
        let existing = std::fs::read_to_string(hook_path)
            .with_context(|| format!("failed to read {}", hook_path.display()))?;

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
    fn make_temp_git_project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let git_hooks = dir.path().join(".git").join("hooks");
        std::fs::create_dir_all(&git_hooks).unwrap();
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
            content.contains(".vfs/.dirty"),
            "should reference dirty file"
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
    fn test_post_merge_hook_deletes_dirty_after_warm() {
        let dir = make_temp_git_project();
        install_hooks(dir.path()).unwrap();
        let content =
            std::fs::read_to_string(dir.path().join(".git").join("hooks").join("post-merge"))
                .unwrap();
        assert!(
            content.contains("rm -f .vfs/.dirty"),
            "should remove dirty marker after successful warm"
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

    #[cfg(unix)]
    #[test]
    fn test_post_merge_noop_without_hilo() {
        let dir = make_temp_git_project();
        install_hooks(dir.path()).unwrap();
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

    #[cfg(unix)]
    #[test]
    fn test_post_merge_warms_and_clears_marker_when_hilo_present() {
        let dir = make_temp_git_project();
        install_hooks(dir.path()).unwrap();
        let marker = dir.path().join(".vfs").join(".dirty");
        std::fs::create_dir_all(dir.path().join(".vfs")).unwrap();
        std::fs::write(&marker, "stale\n").unwrap();
        let log = dir.path().join("hilo-calls.log");
        let path_dir = make_path_dir(Some(&stub_hilo_recording(&log)));

        let out = run_installed_hook(dir.path(), "post-merge", &hook_path(path_dir.path(), true));
        assert_eq!(out.status.code(), Some(0));
        assert!(
            !marker.exists(),
            "installed hilo must clear the marker after a full warm"
        );
        assert_eq!(std::fs::read_to_string(&log).unwrap().trim(), "graph warm");
    }

    #[cfg(unix)]
    #[test]
    fn test_post_merge_without_marker_does_not_invoke_hilo() {
        let dir = make_temp_git_project();
        install_hooks(dir.path()).unwrap();
        let log = dir.path().join("hilo-calls.log");
        let path_dir = make_path_dir(Some(&stub_hilo_recording(&log)));

        let out = run_installed_hook(dir.path(), "post-merge", &hook_path(path_dir.path(), true));
        assert_eq!(out.status.code(), Some(0));
        assert!(
            !log.exists(),
            "post-merge must not warm unless a dirty marker exists"
        );
    }
}
