//! Git worktree manager — clone, pull, checkout, list, and remove Git repos
//! under `~/.hilo/worktrees/<name>/`.
//!
//! Uses the `git2` crate for programmatic Git operations. Each managed
//! worktree is a full clone (not a linked worktree) living in its own
//! directory under the manager's base directory.

use std::path::PathBuf;
use std::time::SystemTime;

use thiserror::Error;

/// Error type for worktree operations.
#[derive(Debug, Error)]
pub enum WorktreeError {
    #[error("git operation failed: {0}")]
    Git(#[from] git2::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("worktree not found: {0}")]
    NotFound(String),
    #[error("worktree already exists: {0}")]
    AlreadyExists(String),
}

/// Status of a managed worktree.
#[derive(Debug, Clone)]
pub struct WorktreeStatus {
    pub name: String,
    pub path: PathBuf,
    pub current_ref: String,
    pub last_pull: Option<SystemTime>,
}

/// Manages git worktrees under `~/.hilo/worktrees/<name>/`.
pub struct WorktreeManager {
    base_dir: PathBuf,
}

impl WorktreeManager {
    /// Create a new `WorktreeManager` rooted at `~/.hilo/worktrees/`.
    ///
    /// The base directory is created if it does not already exist.
    pub fn new() -> Result<Self, WorktreeError> {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let base_dir = PathBuf::from(home).join(".hilo").join("worktrees");
        Self::with_base_dir(base_dir)
    }

    /// Create a manager with a custom base directory (primarily for testing).
    ///
    /// The directory is created if it does not already exist.
    pub fn with_base_dir(base_dir: PathBuf) -> Result<Self, WorktreeError> {
        std::fs::create_dir_all(&base_dir)?;
        Ok(Self { base_dir })
    }

    /// Ensure a worktree exists: clone if absent, fetch if present, then
    /// checkout the requested ref.
    ///
    /// Returns the path to the worktree directory.
    pub fn ensure(&self, name: &str, url: &str, ref_name: &str) -> Result<PathBuf, WorktreeError> {
        let worktree_path = self.base_dir.join(name);
        if worktree_path.join(".git").exists() {
            // Already cloned — open, fetch latest, and (re)checkout the ref.
            let repo = git2::Repository::open(&worktree_path)?;
            self.fetch_origin(&repo)?;
            self.checkout_ref(&repo, ref_name)?;
            Ok(worktree_path)
        } else {
            // Fresh clone.
            std::fs::create_dir_all(&worktree_path)?;
            let repo = git2::Repository::clone(url, &worktree_path)?;
            self.checkout_ref(&repo, ref_name)?;
            Ok(worktree_path)
        }
    }

    /// List all worktrees and their status.
    ///
    /// Scans `base_dir` for subdirectories containing a `.git` entry and
    /// reports the current HEAD ref name plus the mtime of `FETCH_HEAD`
    /// (as a proxy for the last pull time).
    pub fn list(&self) -> Result<Vec<WorktreeStatus>, WorktreeError> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.base_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() || !path.join(".git").exists() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let repo = match git2::Repository::open(&path) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let current_ref = repo
                .head()
                .ok()
                .and_then(|h| {
                    h.is_branch()
                        .then(|| h.shorthand().map(|s| s.to_string()))
                        .flatten()
                })
                .unwrap_or_else(|| "HEAD".to_string());
            let last_pull = path
                .join(".git")
                .join("FETCH_HEAD")
                .metadata()
                .and_then(|m| m.modified())
                .ok();
            out.push(WorktreeStatus {
                name,
                path,
                current_ref,
                last_pull,
            });
        }
        Ok(out)
    }

    /// Remove a worktree — deletes the directory and all of its contents.
    pub fn remove(&self, name: &str) -> Result<(), WorktreeError> {
        let path = self.base_dir.join(name);
        if !path.exists() {
            return Err(WorktreeError::NotFound(name.to_string()));
        }
        std::fs::remove_dir_all(&path)?;
        Ok(())
    }

    /// Auto-pull a worktree if `FETCH_HEAD` is older than `interval_secs`.
    ///
    /// Returns `true` if a fetch was performed, `false` if the worktree is
    /// fresh enough. A missing `FETCH_HEAD` is treated as always-stale.
    pub fn auto_pull_if_stale(
        &self,
        name: &str,
        interval_secs: u64,
    ) -> Result<bool, WorktreeError> {
        let worktree_path = self.base_dir.join(name);
        if !worktree_path.join(".git").exists() {
            return Err(WorktreeError::NotFound(name.to_string()));
        }
        let repo = git2::Repository::open(&worktree_path)?;
        let fetch_head = worktree_path.join(".git").join("FETCH_HEAD");
        if !fetch_head.exists() {
            // No FETCH_HEAD yet — needs an initial pull.
            self.fetch_origin(&repo)?;
            return Ok(true);
        }
        let mtime = std::fs::metadata(&fetch_head)?.modified()?;
        let elapsed = SystemTime::now().duration_since(mtime).unwrap_or_default();
        if elapsed.as_secs() >= interval_secs {
            self.fetch_origin(&repo)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    // ── Private helpers ────────────────────────────────────────────────

    /// Fetch all branches from the configured `origin` remote.
    fn fetch_origin(&self, repo: &git2::Repository) -> Result<(), WorktreeError> {
        let mut remote = repo.find_remote("origin")?;
        remote.fetch(&["+refs/heads/*:refs/heads/*"], None, None)?;
        Ok(())
    }

    /// Checkout the requested ref — tries as a direct revparse, then as a
    /// branch (`refs/heads/<ref>`), then as a tag (`refs/tags/<ref>`).
    ///
    /// Tags result in a detached HEAD; branches update HEAD to track the
    /// branch ref.
    fn checkout_ref(&self, repo: &git2::Repository, ref_name: &str) -> Result<(), WorktreeError> {
        let (object, reference) = repo
            .revparse_ext(ref_name)
            .or_else(|_| repo.revparse_ext(&format!("refs/heads/{ref_name}")))
            .or_else(|_| repo.revparse_ext(&format!("refs/tags/{ref_name}")))?;
        repo.checkout_tree(&object, None)?;
        match reference {
            Some(gref) if gref.is_tag() => {
                repo.set_head_detached(object.id())?;
            }
            _ => {
                repo.set_head(&format!("refs/heads/{ref_name}"))?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    /// Create a bare Git repo in a temp directory with an initial commit on
    /// `main`. Returns the temp dir (kept alive for the test) and the
    /// `file://` URL to the bare repo.
    fn init_bare_repo() -> (TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("test-repo.git");
        std::fs::create_dir_all(&repo_path).unwrap();

        let output = Command::new("git")
            .args(["init", "--bare", "-b", "main"])
            .arg(&repo_path)
            .output()
            .unwrap();
        assert!(output.status.success(), "git init failed: {:?}", output);

        let work = dir.path().join("work");
        let url = format!("file://{}", repo_path.display());
        let output = Command::new("git")
            .args(["clone", &url, work.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(output.status.success(), "git clone failed: {:?}", output);

        std::fs::write(work.join("README.md"), "# test\n").unwrap();
        let output = Command::new("git")
            .args(["-C", work.to_str().unwrap(), "add", "README.md"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let output = Command::new("git")
            .args(["-C", work.to_str().unwrap(), "commit", "-m", "initial"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let output = Command::new("git")
            .args(["-C", work.to_str().unwrap(), "push", "origin", "main"])
            .output()
            .unwrap();
        assert!(output.status.success());

        (dir, url)
    }

    // TEST 1: Fresh clone creates worktree
    #[test]
    fn test_ensure_fresh_clone_creates_worktree() {
        let (_dir, url) = init_bare_repo();
        let tmp = tempfile::tempdir().unwrap();
        let mgr = WorktreeManager::with_base_dir(tmp.path().to_path_buf()).unwrap();
        let path = mgr.ensure("my-repo", &url, "main").unwrap();
        assert!(path.join("README.md").exists());
        assert!(path.join(".git").exists());
    }

    // TEST 2: Ensure on existing worktree skips clone (idempotent)
    #[test]
    fn test_ensure_existing_worktree_skips_clone() {
        let (_dir, url) = init_bare_repo();
        let tmp = tempfile::tempdir().unwrap();
        let mgr = WorktreeManager::with_base_dir(tmp.path().to_path_buf()).unwrap();
        let path1 = mgr.ensure("my-repo", &url, "main").unwrap();
        // Second ensure should succeed without error.
        let path2 = mgr.ensure("my-repo", &url, "main").unwrap();
        assert_eq!(path1, path2);
        assert!(path2.join("README.md").exists());
    }

    // TEST 3: Checkout branch (refs/heads/main semantics)
    #[test]
    fn test_ensure_checkout_branch() {
        let (_dir, url) = init_bare_repo();
        let tmp = tempfile::tempdir().unwrap();
        let mgr = WorktreeManager::with_base_dir(tmp.path().to_path_buf()).unwrap();
        let path = mgr.ensure("branch-repo", &url, "main").unwrap();
        assert!(path.join("README.md").exists());
    }

    // TEST 4: List returns all worktrees
    #[test]
    fn test_list_returns_all_worktrees() {
        let (_dir, url) = init_bare_repo();
        let tmp = tempfile::tempdir().unwrap();
        let mgr = WorktreeManager::with_base_dir(tmp.path().to_path_buf()).unwrap();
        mgr.ensure("repo-a", &url, "main").unwrap();
        mgr.ensure("repo-b", &url, "main").unwrap();
        let list = mgr.list().unwrap();
        assert_eq!(list.len(), 2);
        let names: Vec<&str> = list.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"repo-a"));
        assert!(names.contains(&"repo-b"));
    }

    // TEST 5: Auto-pull on stale worktree triggers fetch
    #[test]
    fn test_auto_pull_stale_triggers_fetch() {
        let (_dir, url) = init_bare_repo();
        let tmp = tempfile::tempdir().unwrap();
        let mgr = WorktreeManager::with_base_dir(tmp.path().to_path_buf()).unwrap();
        mgr.ensure("stale-repo", &url, "main").unwrap();
        // Make FETCH_HEAD look old using filetime.
        let fetch_head = tmp
            .path()
            .join("stale-repo")
            .join(".git")
            .join("FETCH_HEAD");
        if fetch_head.exists() {
            let two_hours_ago = SystemTime::now() - std::time::Duration::from_secs(7200);
            filetime::set_file_mtime(&fetch_head, two_hours_ago.into()).unwrap();
        }
        let pulled = mgr.auto_pull_if_stale("stale-repo", 3600).unwrap();
        assert!(pulled);
    }

    // TEST 6: Auto-pull on fresh worktree does nothing
    #[test]
    fn test_auto_pull_fresh_does_nothing() {
        let (_dir, url) = init_bare_repo();
        let tmp = tempfile::tempdir().unwrap();
        let mgr = WorktreeManager::with_base_dir(tmp.path().to_path_buf()).unwrap();
        mgr.ensure("fresh-repo", &url, "main").unwrap();
        let pulled = mgr.auto_pull_if_stale("fresh-repo", 3600).unwrap();
        assert!(!pulled);
    }

    // TEST 7: Remove deletes worktree
    #[test]
    fn test_remove_deletes_worktree() {
        let (_dir, url) = init_bare_repo();
        let tmp = tempfile::tempdir().unwrap();
        let mgr = WorktreeManager::with_base_dir(tmp.path().to_path_buf()).unwrap();
        mgr.ensure("to-remove", &url, "main").unwrap();
        assert!(tmp.path().join("to-remove").exists());
        mgr.remove("to-remove").unwrap();
        assert!(!tmp.path().join("to-remove").exists());
    }

    // TEST 8: Remove nonexistent worktree returns NotFound
    #[test]
    fn test_remove_nonexistent_returns_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = WorktreeManager::with_base_dir(tmp.path().to_path_buf()).unwrap();
        let err = mgr.remove("nonexistent").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not found") || msg.contains("nonexistent"));
    }

    // TEST 9: WorktreeError Display
    #[test]
    fn test_worktree_error_display() {
        assert!(WorktreeError::NotFound("foo".into())
            .to_string()
            .contains("foo"));
        assert!(WorktreeError::AlreadyExists("bar".into())
            .to_string()
            .contains("bar"));
    }

    // TEST 10: Ensure with tag ref
    #[test]
    fn test_ensure_checkout_tag() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("tag-repo.git");
        std::fs::create_dir_all(&repo_path).unwrap();
        Command::new("git")
            .args(["init", "--bare", "-b", "main"])
            .arg(&repo_path)
            .output()
            .unwrap();
        let work = dir.path().join("work");
        let url = format!("file://{}", repo_path.display());
        Command::new("git")
            .args(["clone", &url, work.to_str().unwrap()])
            .output()
            .unwrap();
        std::fs::write(work.join("file.txt"), "v1\n").unwrap();
        Command::new("git")
            .args(["-C", work.to_str().unwrap(), "add", "file.txt"])
            .output()
            .unwrap();
        Command::new("git")
            .args(["-C", work.to_str().unwrap(), "commit", "-m", "v1 commit"])
            .output()
            .unwrap();
        Command::new("git")
            .args(["-C", work.to_str().unwrap(), "tag", "v1.0"])
            .output()
            .unwrap();
        Command::new("git")
            .args([
                "-C",
                work.to_str().unwrap(),
                "push",
                "origin",
                "main",
                "--tags",
            ])
            .output()
            .unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let mgr = WorktreeManager::with_base_dir(tmp.path().to_path_buf()).unwrap();
        let path = mgr.ensure("tag-repo", &url, "v1.0").unwrap();
        assert!(path.join("file.txt").exists());
    }
}
