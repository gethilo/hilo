//! Local path backend — direct filesystem passthrough.
//!
//! Maps a host filesystem directory into the virtual filesystem as-is.
//! No caching, no cloning, no network. Always writable.

use std::path::PathBuf;

/// Error type for local backend operations.
#[derive(Debug, thiserror::Error)]
pub enum LocalError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("path not found: {0}")]
    NotFound(PathBuf),
}

pub type LocalResult<T> = Result<T, LocalError>;

/// Configuration for a local path backend mount.
#[derive(Debug)]
pub struct LocalBackendConfig {
    pub real_path: PathBuf,
    pub at: String,
}

/// Manages a local filesystem directory as a backend.
#[derive(Debug)]
pub struct LocalBackend {
    config: LocalBackendConfig,
    /// Canonicalized real path.
    real_path: PathBuf,
}

impl LocalBackend {
    /// Mount a local directory. Canonicalizes the path and verifies it exists.
    pub fn mount(config: LocalBackendConfig) -> LocalResult<Self> {
        let real_path = config
            .real_path
            .canonicalize()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => LocalError::NotFound(config.real_path.clone()),
                _ => LocalError::Io(e),
            })?;

        Ok(LocalBackend { config, real_path })
    }

    /// Resolve a virtual path within this backend to the real filesystem path.
    pub fn resolve(&self, virtual_path: &str) -> LocalResult<PathBuf> {
        let rel = virtual_path
            .strip_prefix(&self.config.at)
            .unwrap_or(virtual_path);
        let real = self.real_path.join(rel.trim_start_matches('/'));
        if !real.exists() {
            return Err(LocalError::NotFound(real));
        }
        Ok(real)
    }

    /// Get backend metadata for reporting.
    pub fn info(&self) -> crate::BackendInfo {
        crate::BackendInfo {
            backend: "local".to_string(),
            real_path: self.real_path.to_string_lossy().to_string(),
            cached: false,
            cache_path: None,
            sync_status: "direct".to_string(),
        }
    }

    /// Local paths are always writable.
    pub fn writable(&self) -> bool {
        true
    }

    /// The virtual mount path for this backend.
    pub fn mount_point(&self) -> &str {
        &self.config.at
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_mount_valid_path() {
        let tmp = TempDir::new().unwrap();
        let config = LocalBackendConfig {
            real_path: tmp.path().to_path_buf(),
            at: "/mnt/local".to_string(),
        };
        let backend = LocalBackend::mount(config).unwrap();
        assert_eq!(backend.mount_point(), "/mnt/local");
        assert!(backend.writable());
    }

    #[test]
    fn test_mount_nonexistent_path() {
        let config = LocalBackendConfig {
            real_path: PathBuf::from("/tmp/hilo-definitely-nonexistent-12345"),
            at: "/mnt/local".to_string(),
        };
        let err = LocalBackend::mount(config).unwrap_err();
        assert!(
            matches!(err, LocalError::NotFound(_)),
            "expected NotFound, got: {err}"
        );
    }

    #[test]
    fn test_resolve_found_and_missing() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("hello.txt"), "world").unwrap();

        let config = LocalBackendConfig {
            real_path: tmp.path().to_path_buf(),
            at: "/mnt/local".to_string(),
        };
        let backend = LocalBackend::mount(config).unwrap();

        // Found.
        let resolved = backend.resolve("/mnt/local/hello.txt").unwrap();
        assert!(resolved.ends_with("hello.txt"));

        // Missing.
        let err = backend.resolve("/mnt/local/nope.txt").unwrap_err();
        assert!(
            matches!(err, LocalError::NotFound(_)),
            "expected NotFound, got: {err}"
        );
    }

    #[test]
    fn test_info_fields() {
        let tmp = TempDir::new().unwrap();
        let config = LocalBackendConfig {
            real_path: tmp.path().to_path_buf(),
            at: "/mnt/local".to_string(),
        };
        let backend = LocalBackend::mount(config).unwrap();
        let info = backend.info();
        assert_eq!(info.backend, "local");
        assert!(!info.cached);
        assert!(info.cache_path.is_none());
        assert_eq!(info.sync_status, "direct");
    }

    #[test]
    fn test_resolve_without_at_prefix() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("data.txt"), "ok").unwrap();

        let config = LocalBackendConfig {
            real_path: tmp.path().to_path_buf(),
            at: "/mnt/local".to_string(),
        };
        let backend = LocalBackend::mount(config).unwrap();

        // Bare filename — should resolve relative to real_path.
        let resolved = backend.resolve("data.txt").unwrap();
        assert!(resolved.ends_with("data.txt"));
    }

    #[test]
    fn test_local_error_display() {
        assert!(
            LocalError::NotFound(PathBuf::from("/tmp/nope"))
                .to_string()
                .contains("/tmp/nope"),
            "expected path in NotFound error message"
        );
    }
}
