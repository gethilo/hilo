//! Git worktree manager — clone, pull, checkout, list, and remove Git repos
//! under `~/.hilo/worktrees/<name>/`.
//!
//! Uses the `git2` crate for programmatic Git operations. Each managed
//! worktree is a full clone (not a linked worktree) living in its own
//! directory under the manager's base directory.

use std::path::{Path, PathBuf};
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
    #[error("unresolved ref '{ref_name}': tried {forms_tried}; {available_refs}")]
    UnresolvedRef {
        ref_name: String,
        forms_tried: String,
        available_refs: String,
    },
}

/// Cap on how many branch names the unresolved-ref error lists.
const MAX_LISTED_BRANCHES: usize = 10;

/// List available branch names (local + remote-tracking), deduplicated and
/// capped at [`MAX_LISTED_BRANCHES`], for the unresolved-ref error message.
fn available_branches(repo: &git2::Repository) -> String {
    let mut names: Vec<String> = Vec::new();
    let mut truncated = false;
    if let Ok(branches) = repo.branches(None) {
        for (branch, _kind) in branches.flatten() {
            if names.len() >= MAX_LISTED_BRANCHES {
                truncated = true;
                break;
            }
            if let Ok(Some(name)) = branch.name() {
                if !names.iter().any(|n| n == name) {
                    names.push(name.to_string());
                }
            }
        }
    }
    if names.is_empty() {
        return "available branches: none".to_string();
    }
    let list = names.join(", ");
    if truncated {
        format!("available branches (first {MAX_LISTED_BRANCHES}): {list}, ...")
    } else {
        format!("available branches: {list}")
    }
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
            // Fresh clone. A plain clone only materializes the remote's
            // default branch, so a manifest `ref` that names any OTHER branch
            // (e.g. `main` against a repo whose default is `master`) has no
            // local ref, and `checkout_ref` falls through to the tags arm and
            // dies. Fetch every branch (mirroring the already-cloned path) so
            // the requested ref resolves against fetched refs.
            std::fs::create_dir_all(&worktree_path)?;
            let result = self.clone_and_checkout(url, &worktree_path, ref_name);
            if let Err(err) = result {
                // A failed clone/checkout must not leave an empty (or
                // half-cloned) directory behind: remove it so a subsequent
                // mount retries from scratch instead of wedging on a dir with
                // no `.git`. Cleanup is best-effort; the original error wins.
                let _ = std::fs::remove_dir_all(&worktree_path);
                return Err(err);
            }
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

    /// Clone `url` into `worktree_path` and checkout `ref_name`.
    ///
    /// The clone is followed by a full branch fetch (`fetch_origin`) before
    /// checkout: a plain `git2::Repository::clone` only materializes the
    /// remote's default branch, so resolving a non-default branch ref requires
    /// fetching the branch set first.
    fn clone_and_checkout(
        &self,
        url: &str,
        worktree_path: &Path,
        ref_name: &str,
    ) -> Result<(), WorktreeError> {
        let repo = git2::Repository::clone(url, worktree_path)?;
        self.fetch_origin(&repo)?;
        self.checkout_ref(&repo, ref_name)?;
        Ok(())
    }

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
        // Fresh clones only carry refs the fetch materialized; when nothing
        // resolves, the failure must name every form tried and what the
        // remote actually offers instead of a bare libgit2 revspec error.
        let candidate_forms: [String; 3] = [
            ref_name.to_string(),
            format!("refs/heads/{ref_name}"),
            format!("refs/tags/{ref_name}"),
        ];
        for form in &candidate_forms {
            if let Ok((object, reference)) = repo.revparse_ext(form) {
                repo.checkout_tree(&object, None)?;
                match reference {
                    Some(gref) if gref.is_tag() => {
                        repo.set_head_detached(object.id())?;
                    }
                    _ => {
                        repo.set_head(&format!("refs/heads/{ref_name}"))?;
                    }
                }
                return Ok(());
            }
        }
        Err(WorktreeError::UnresolvedRef {
            ref_name: ref_name.to_string(),
            forms_tried: candidate_forms
                .iter()
                .map(|f| format!("'{f}'"))
                .collect::<Vec<_>>()
                .join(", "),
            available_refs: available_branches(repo),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    /// Run a git command with CI-safe identity injection.
    /// GitHub Actions runners do not configure a global git identity by
    /// default, causing `git commit` to fail with "Author identity unknown".
    /// This helper prepends `-c user.name=... -c user.email=... -c
    /// commit.gpgSign=false` so tests are hermetic.
    fn git_cmd(args: &[&str]) {
        let prefix = &[
            "-c",
            "user.name=Hilo Test",
            "-c",
            "user.email=test@hilo.test",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "commit.gpgSign=false",
        ];
        let all_args: Vec<&str> = prefix.iter().chain(args.iter()).copied().collect();
        let output = Command::new("git").args(&all_args).output().unwrap();
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Create a bare Git repo in a temp directory with an initial commit on
    /// `main`. Returns the temp dir (kept alive for the test) and the
    /// `file://` URL to the bare repo.
    fn init_bare_repo() -> (TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("test-repo.git");
        std::fs::create_dir_all(&repo_path).unwrap();

        git_cmd(&["init", "--bare", "-b", "main", repo_path.to_str().unwrap()]);

        let work = dir.path().join("work");
        let url = format!("file://{}", repo_path.display());

        git_cmd(&["clone", &url, work.to_str().unwrap()]);

        std::fs::write(work.join("README.md"), "# test\n").unwrap();
        git_cmd(&["-C", work.to_str().unwrap(), "add", "README.md"]);
        git_cmd(&["-C", work.to_str().unwrap(), "commit", "-m", "initial"]);
        git_cmd(&["-C", work.to_str().unwrap(), "push", "origin", "main"]);

        (dir, url)
    }

    /// Create a bare Git repo whose default branch is `master` AND which also
    /// carries a separate `main` branch. This reproduces the DF-WARPFS-128
    /// fixture: a manifest `ref: main` against a repo whose clone-default is
    /// `master` must still resolve `main`. Returns the temp dir and the
    /// `file://` URL.
    fn init_bare_repo_master_with_main() -> (TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("test-repo.git");
        std::fs::create_dir_all(&repo_path).unwrap();

        // Default branch is `master` (the `-b master` flag overrides the
        // `init.defaultBranch=main` injected by `git_cmd`).
        git_cmd(&[
            "init",
            "--bare",
            "-b",
            "master",
            repo_path.to_str().unwrap(),
        ]);

        let work = dir.path().join("work");
        let url = format!("file://{}", repo_path.display());
        git_cmd(&["clone", &url, work.to_str().unwrap()]);

        std::fs::write(work.join("README.md"), "# test\n").unwrap();
        git_cmd(&["-C", work.to_str().unwrap(), "add", "README.md"]);
        git_cmd(&["-C", work.to_str().unwrap(), "commit", "-m", "initial"]);
        git_cmd(&["-C", work.to_str().unwrap(), "push", "origin", "master"]);

        // A `main` branch that is NOT the remote default.
        git_cmd(&["-C", work.to_str().unwrap(), "checkout", "-b", "main"]);
        git_cmd(&["-C", work.to_str().unwrap(), "push", "origin", "main"]);

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
        git_cmd(&["init", "--bare", "-b", "main", repo_path.to_str().unwrap()]);
        let work = dir.path().join("work");
        let url = format!("file://{}", repo_path.display());
        git_cmd(&["clone", &url, work.to_str().unwrap()]);
        std::fs::write(work.join("file.txt"), "v1\n").unwrap();
        git_cmd(&["-C", work.to_str().unwrap(), "add", "file.txt"]);
        git_cmd(&["-C", work.to_str().unwrap(), "commit", "-m", "v1 commit"]);
        git_cmd(&["-C", work.to_str().unwrap(), "tag", "v1.0"]);
        git_cmd(&[
            "-C",
            work.to_str().unwrap(),
            "push",
            "origin",
            "main",
            "--tags",
        ]);

        let tmp = tempfile::tempdir().unwrap();
        let mgr = WorktreeManager::with_base_dir(tmp.path().to_path_buf()).unwrap();
        let path = mgr.ensure("tag-repo", &url, "v1.0").unwrap();
        assert!(path.join("file.txt").exists());
    }

    // TEST 11: Fresh clone resolves a non-default branch ref (DF-WARPFS-128).
    //
    // The remote's default branch is `master`; the manifest requests `main`.
    // A plain clone only materializes `master`, so the old code fell through
    // to the tags arm and died with `revspec 'refs/tags/main' not found`.
    #[test]
    fn test_ensure_resolves_non_default_branch_on_fresh_clone() {
        let (_dir, url) = init_bare_repo_master_with_main();
        let tmp = tempfile::tempdir().unwrap();
        let mgr = WorktreeManager::with_base_dir(tmp.path().to_path_buf()).unwrap();
        let path = mgr.ensure("repo-a", &url, "main").unwrap();
        assert!(path.join("README.md").exists());
        // HEAD must track the requested branch, not the clone default.
        let repo = git2::Repository::open(&path).unwrap();
        assert_eq!(repo.head().unwrap().shorthand().unwrap(), "main");
    }

    // TEST 12: A failed fresh clone leaves no worktree directory behind.
    //
    // DF-WARPFS-128 hardening: `ensure` must not leave an empty (or
    // half-cloned) directory when the clone fails, so a later mount retries
    // from scratch instead of wedging on a dir with no `.git`.
    #[test]
    fn test_ensure_failed_clone_leaves_no_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = WorktreeManager::with_base_dir(tmp.path().to_path_buf()).unwrap();
        // A `file://` URL to a path that is not a Git repo makes the clone
        // fail deterministically (no network involved).
        let url = format!("file://{}", tmp.path().join("not-a-repo").display());
        let err = mgr.ensure("bad-repo", &url, "main").unwrap_err();
        assert!(
            !tmp.path().join("bad-repo").exists(),
            "worktree dir left behind after failed clone: {err}"
        );
    }

    // TEST 13: An unresolvable ref error names every tried form and the
    // available branches (DF-WARPFS-132).
    //
    // `checkout_ref` tries the ref as-is, then `refs/heads/<ref>`, then
    // `refs/tags/<ref>`; the final error must say so and list what the
    // remote actually offers instead of a bare libgit2 revspec error.
    #[test]
    fn test_ensure_unresolvable_ref_error_names_forms_and_branches() {
        let (_dir, url) = init_bare_repo_master_with_main();
        let tmp = tempfile::tempdir().unwrap();
        let mgr = WorktreeManager::with_base_dir(tmp.path().to_path_buf()).unwrap();
        let err = mgr.ensure("repo-a", &url, "no-such-branch").unwrap_err();
        let msg = err.to_string();
        // Names the requested ref and every candidate form tried.
        assert!(msg.contains("no-such-branch"), "names the ref: {msg}");
        assert!(msg.contains("tried"), "lists tried forms: {msg}");
        assert!(msg.contains("'no-such-branch'"), "tried bare form: {msg}");
        assert!(
            msg.contains("'refs/heads/no-such-branch'"),
            "tried branch form: {msg}"
        );
        assert!(
            msg.contains("'refs/tags/no-such-branch'"),
            "tried tag form: {msg}"
        );
        // Lists the branches the remote actually offers.
        assert!(
            msg.contains("available branches"),
            "lists available branches: {msg}"
        );
        assert!(msg.contains("master"), "available branch master: {msg}");
        assert!(msg.contains("main"), "available branch main: {msg}");
    }

    // TEST 14: The available-branch list is capped (DF-WARPFS-132).
    //
    // A repo with more than MAX_LISTED_BRANCHES branches must not dump all
    // of them into the error: the message notes the cap (first 10) and
    // truncates.
    #[test]
    fn test_unresolved_ref_branch_list_is_capped() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("many-branches.git");
        std::fs::create_dir_all(&repo_path).unwrap();
        git_cmd(&["init", "--bare", "-b", "main", repo_path.to_str().unwrap()]);
        let work = dir.path().join("work");
        let url = format!("file://{}", repo_path.display());
        git_cmd(&["clone", &url, work.to_str().unwrap()]);
        std::fs::write(work.join("README.md"), "# test\n").unwrap();
        git_cmd(&["-C", work.to_str().unwrap(), "add", "README.md"]);
        git_cmd(&["-C", work.to_str().unwrap(), "commit", "-m", "initial"]);
        git_cmd(&["-C", work.to_str().unwrap(), "push", "origin", "main"]);
        for i in 1..=12 {
            let branch = format!("b{i:02}");
            git_cmd(&["-C", work.to_str().unwrap(), "branch", &branch]);
        }
        git_cmd(&["-C", work.to_str().unwrap(), "push", "origin", "--all"]);

        let tmp = tempfile::tempdir().unwrap();
        let mgr = WorktreeManager::with_base_dir(tmp.path().to_path_buf()).unwrap();
        let err = mgr.ensure("capped", &url, "nope").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("first 10"), "cap noted: {msg}");
        assert!(msg.contains("..."), "truncation marker: {msg}");
        // Early branches always sit inside the first 10 (the clone default
        // `main` sorts last alphabetically, so b01..b10 lead the list).
        assert!(msg.contains("b01"), "early branch listed: {msg}");
        assert!(msg.contains("b05"), "early branch listed: {msg}");
        assert!(!msg.contains("b11"), "beyond-cap branch omitted: {msg}");
        assert!(!msg.contains("b12"), "beyond-cap branch omitted: {msg}");
    }
}
