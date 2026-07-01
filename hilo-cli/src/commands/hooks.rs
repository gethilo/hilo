//! Git hook installation — auto-update Hilo metadata on commit and pull.
//!
//! When `hilo init` runs, it installs two git hooks into `.git/hooks/`:
//! - **post-commit** — runs `hilo graph warm --changed` (incremental parse)
//!   when Hilo is available, or writes a `.vfs/.dirty` marker when it is not.
//! - **post-merge** — on pull, checks for `.vfs/.dirty`; if present and Hilo is
//!   installed, runs full `hilo graph warm` and deletes the marker.
//!
//! Both hooks use `### HILO` / `### /HILO` block markers so they can be safely
//! appended to existing hooks without overwriting content.

use std::path::Path;

use anyhow::{Context, Result};

/// Marker delimiting the Hilo block inside a hook file.
const HILO_MARKER_START: &str = "### HILO";
const HILO_MARKER_END: &str = "### /HILO";

/// post-commit hook content — incremental graph warm or dirty marker.
const POST_COMMIT_HOOK: &str = r#"#!/bin/sh
### HILO — auto-update metadata on commit
if command -v hilo >/dev/null 2>&1; then
    hilo graph warm --changed 2>/dev/null || true
else
    # Hilo not installed — mark dirty
    echo "Hilo: not installed — marking metadata as dirty"
    echo "stale" > .vfs/.dirty
fi
### /HILO
"#;

/// post-merge hook content — sync metadata on pull.
const POST_MERGE_HOOK: &str = r#"#!/bin/sh
### HILO — sync metadata on pull
if [ -f .vfs/.dirty ]; then
    if command -v hilo >/dev/null 2>&1; then
        echo "Hilo: dirty marker found — updating metadata"
        hilo graph warm 2>/dev/null || true
        rm -f .vfs/.dirty
        echo "Hilo: metadata updated, dirty marker removed"
    else
        echo "Hilo: not installed — leaving dirty marker"
    fi
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
    fn test_post_commit_hook_contains_dirty_logic() {
        let dir = make_temp_git_project();
        install_hooks(dir.path()).unwrap();
        let content =
            std::fs::read_to_string(dir.path().join(".git").join("hooks").join("post-commit"))
                .unwrap();
        assert!(
            content.contains("command -v hilo"),
            "should check if hilo is installed"
        );
        assert!(
            content.contains(".vfs/.dirty"),
            "should write dirty marker when hilo not installed"
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
}
