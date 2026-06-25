//! Remote Git backend — clone, pull, and worktree management via git2.
//!
//! Clones repos to `~/.hilo/worktrees/<name>/` and exposes them as
//! virtual directories. Supports auto-pull on configurable intervals and
//! read-only enforcement.

use std::path::{Path, PathBuf};

use git2::{Cred, FetchOptions, RemoteCallbacks, Repository};

/// Error type for Git backend operations.
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("git operation failed: {0}")]
    Git(#[from] git2::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("repository not found at {0}")]
    NotFound(PathBuf),
    #[error("repository is read-only")]
    ReadOnly,
}

pub type GitResult<T> = Result<T, GitError>;

/// Configuration for a remote Git backend mount.
pub struct GitBackendConfig {
    pub url: String,
    pub ref_name: String,
    pub at: String,
    pub writable: bool,
    pub auto_pull_secs: Option<u64>,
    pub cache_dir: Option<PathBuf>,
}

/// Manages a remote Git repository clone and its worktree.
pub struct GitBackend {
    config: GitBackendConfig,
    worktree: PathBuf,
    #[allow(dead_code)]
    repo: Repository,
}

impl GitBackend {
    /// Clone (or open existing) a remote Git repository.
    ///
    /// If the worktree already exists and contains a valid repo, opens it
    /// instead of re-cloning. Auto-pulls if `auto_pull_secs` is set and
    /// the clone is older than the configured interval.
    pub fn mount(config: GitBackendConfig) -> GitResult<Self> {
        let worktree = config
            .cache_dir
            .clone()
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
                    .join(".hilo/worktrees")
            })
            .join(sanitize_name(&config.url));

        let repo = if worktree.join(".git").exists() {
            let repo = Repository::open(&worktree)?;

            // Auto-pull if configured and stale.
            if let Some(interval) = config.auto_pull_secs {
                if should_pull(&worktree, interval) {
                    pull_remote(&repo, &config.ref_name)?;
                }
            }

            repo
        } else {
            std::fs::create_dir_all(&worktree)?;
            let repo = Repository::clone(&config.url, &worktree)?;

            // Checkout the requested ref if not default.
            if config.ref_name != "main" && config.ref_name != "master" {
                checkout_ref(&repo, &config.ref_name)?;
            }

            repo
        };

        Ok(GitBackend {
            config,
            worktree,
            repo,
        })
    }

    /// Resolve a virtual path within this backend to the real filesystem path.
    pub fn resolve(&self, virtual_path: &str) -> GitResult<PathBuf> {
        let rel = virtual_path
            .strip_prefix(&self.config.at)
            .unwrap_or(virtual_path);
        let real = self.worktree.join(rel.trim_start_matches('/'));
        if !real.exists() {
            return Err(GitError::NotFound(real));
        }
        Ok(real)
    }

    /// Get backend metadata for reporting.
    pub fn info(&self) -> crate::BackendInfo {
        crate::BackendInfo {
            backend: "git".to_string(),
            real_path: self.worktree.to_string_lossy().to_string(),
            cached: true,
            cache_path: Some(self.worktree.to_string_lossy().to_string()),
            sync_status: "synced".to_string(),
        }
    }

    /// Whether writes are allowed on this backend.
    pub fn writable(&self) -> bool {
        self.config.writable
    }

    /// The virtual mount path for this backend.
    pub fn mount_point(&self) -> &str {
        &self.config.at
    }
}

// ── Helpers ────────────────────────────────────────────────────────

/// Sanitize a repo URL into a safe directory name.
fn sanitize_name(url: &str) -> String {
    url.replace("https://", "")
        .replace("http://", "")
        .replace("git@", "")
        .replace([':', '/', '.'], "_")
}

/// Check if the repo's last fetch is older than `interval_secs`.
fn should_pull(worktree: &Path, interval_secs: u64) -> bool {
    let fetch_head = worktree.join(".git").join("FETCH_HEAD");
    match std::fs::metadata(&fetch_head) {
        Ok(meta) => {
            let age = meta
                .modified()
                .ok()
                .and_then(|t| t.elapsed().ok())
                .map(|d| d.as_secs())
                .unwrap_or(u64::MAX);
            age > interval_secs
        }
        Err(_) => true, // No FETCH_HEAD — needs pull.
    }
}

/// Pull the given ref from origin.
fn pull_remote(repo: &Repository, ref_name: &str) -> GitResult<()> {
    let mut remote = repo.find_remote("origin")?;

    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(|_url, _username, _allowed| {
        // Use SSH agent or default credentials.
        Cred::default()
    });

    let mut fetch_opts = FetchOptions::new();
    fetch_opts.remote_callbacks(callbacks);

    let refspec = format!("+refs/heads/{ref_name}:refs/remotes/origin/{ref_name}");
    remote.fetch(&[&refspec], Some(&mut fetch_opts), None)?;

    Ok(())
}

/// Checkout a specific ref (branch or tag).
fn checkout_ref(repo: &Repository, ref_name: &str) -> GitResult<()> {
    let (object, reference) = repo.revparse_ext(ref_name)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    /// Create a bare Git repo in a temp directory with an initial commit.
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

    #[test]
    fn test_sanitize_name_github_url() {
        let name = sanitize_name("https://github.com/org/repo.git");
        assert_eq!(name, "github_com_org_repo_git");
    }

    #[test]
    fn test_sanitize_name_ssh_url() {
        let name = sanitize_name("git@github.com:org/repo.git");
        assert_eq!(name, "github_com_org_repo_git");
    }

    #[test]
    fn test_git_error_display() {
        assert!(
            GitError::ReadOnly.to_string().contains("read-only"),
            "expected read-only, got: {}",
            GitError::ReadOnly
        );
        assert!(
            GitError::NotFound(PathBuf::from("/tmp/nope"))
                .to_string()
                .contains("/tmp/nope"),
            "expected path in NotFound"
        );
    }

    #[test]
    fn test_git_backend_mount_clones_repo() {
        let (_dir, url) = init_bare_repo();
        let tmp = tempfile::tempdir().unwrap();

        let config = GitBackendConfig {
            url: url.clone(),
            ref_name: "main".to_string(),
            at: "/vfs/repo".to_string(),
            writable: false,
            auto_pull_secs: None,
            cache_dir: Some(tmp.path().to_path_buf()),
        };

        let backend = GitBackend::mount(config).unwrap();
        assert!(backend.worktree.join("README.md").exists());
        assert!(!backend.writable());
        assert_eq!(backend.mount_point(), "/vfs/repo");
    }

    #[test]
    fn test_git_backend_resolve_existing_path() {
        let (_dir, url) = init_bare_repo();
        let tmp = tempfile::tempdir().unwrap();

        let config = GitBackendConfig {
            url: url.clone(),
            ref_name: "main".to_string(),
            at: "/vfs/repo".to_string(),
            writable: true,
            auto_pull_secs: None,
            cache_dir: Some(tmp.path().to_path_buf()),
        };

        let backend = GitBackend::mount(config).unwrap();
        let resolved = backend.resolve("/vfs/repo/README.md").unwrap();
        assert!(resolved.exists());
        assert!(resolved.to_string_lossy().contains("README.md"));
    }

    #[test]
    fn test_git_backend_resolve_missing_path() {
        let (_dir, url) = init_bare_repo();
        let tmp = tempfile::tempdir().unwrap();

        let config = GitBackendConfig {
            url: url.clone(),
            ref_name: "main".to_string(),
            at: "/vfs/repo".to_string(),
            writable: false,
            auto_pull_secs: None,
            cache_dir: Some(tmp.path().to_path_buf()),
        };

        let backend = GitBackend::mount(config).unwrap();
        let result = backend.resolve("/vfs/repo/nonexistent.txt");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("repository not found")
                || err.to_string().contains("not found"),
            "expected NotFound, got: {}",
            err
        );
    }

    #[test]
    fn test_git_backend_info() {
        let (_dir, url) = init_bare_repo();
        let tmp = tempfile::tempdir().unwrap();

        let config = GitBackendConfig {
            url: url.clone(),
            ref_name: "main".to_string(),
            at: "/vfs/repo".to_string(),
            writable: true,
            auto_pull_secs: None,
            cache_dir: Some(tmp.path().to_path_buf()),
        };

        let backend = GitBackend::mount(config).unwrap();
        let info = backend.info();
        assert_eq!(info.backend, "git");
        assert_eq!(info.sync_status, "synced");
        assert!(info.cached);
        assert!(info.cache_path.is_some());
    }

    #[test]
    fn test_git_backend_writable_respects_config() {
        let (_dir, url) = init_bare_repo();
        let tmp = tempfile::tempdir().unwrap();

        let ro_config = GitBackendConfig {
            url: url.clone(),
            ref_name: "main".to_string(),
            at: "/vfs/ro-repo".to_string(),
            writable: false,
            auto_pull_secs: None,
            cache_dir: Some(tmp.path().to_path_buf()),
        };
        let ro = GitBackend::mount(ro_config).unwrap();
        assert!(!ro.writable());

        let rw_config = GitBackendConfig {
            url: url.clone(),
            ref_name: "main".to_string(),
            at: "/vfs/rw-repo".to_string(),
            writable: true,
            auto_pull_secs: None,
            cache_dir: Some(tmp.path().to_path_buf()),
        };
        let rw = GitBackend::mount(rw_config).unwrap();
        assert!(rw.writable());
    }

    #[test]
    fn test_git_backend_mount_reuses_existing_clone() {
        let (_dir, url) = init_bare_repo();
        let tmp = tempfile::tempdir().unwrap();

        let config = GitBackendConfig {
            url: url.clone(),
            ref_name: "main".to_string(),
            at: "/vfs/repo".to_string(),
            writable: false,
            auto_pull_secs: None,
            cache_dir: Some(tmp.path().to_path_buf()),
        };

        let backend1 = GitBackend::mount(config).unwrap();
        drop(backend1);

        let config2 = GitBackendConfig {
            url: url.clone(),
            ref_name: "main".to_string(),
            at: "/vfs/repo".to_string(),
            writable: false,
            auto_pull_secs: None,
            cache_dir: Some(tmp.path().to_path_buf()),
        };
        let backend2 = GitBackend::mount(config2).unwrap();
        assert!(backend2.worktree.join("README.md").exists());
    }

    #[test]
    fn test_should_pull_no_fetch_head() {
        let tmp = tempfile::tempdir().unwrap();
        let worktree = tmp.path().join("no-fetch");
        std::fs::create_dir_all(worktree.join(".git")).unwrap();
        assert!(should_pull(&worktree, 3600));
    }

    #[test]
    fn test_should_pull_returns_true_for_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        let fetch_head = git_dir.join("FETCH_HEAD");
        std::fs::write(&fetch_head, "stale\n").unwrap();

        let two_hours_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(7200);
        filetime::set_file_mtime(&fetch_head, two_hours_ago.into()).unwrap();

        assert!(should_pull(tmp.path(), 3600));
    }

    #[test]
    fn test_should_pull_returns_false_for_fresh() {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        let fetch_head = git_dir.join("FETCH_HEAD");
        std::fs::write(&fetch_head, "fresh\n").unwrap();

        assert!(!should_pull(tmp.path(), 3600));
    }
}
