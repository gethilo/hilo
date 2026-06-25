// Hilo Backends — virtual storage backends
//
// Supported backends:
// - Git (local): normal file write → staged in worktree
// - S3 (read-only): S3 bucket → local cache → read
// - S3 (write-through): write → cache → upload → blob index
// - Remote git: clone → worktree → auto-pull
// - Local path: direct passthrough

pub mod git;
pub mod local;
pub mod s3;

pub use git::{GitBackend, GitBackendConfig, GitError, GitResult};
pub use s3::{S3Client, S3Error, S3Result, WriteResult};

/// Resolve a virtual path to its real storage location.
pub enum Backend {
    S3 {
        bucket: String,
        prefix: String,
        region: String,
        writable: bool,
    },
    Git {
        url: String,
        ref_name: String,
        worktree: String,
        writable: bool,
    },
    Remote {
        url: String,
        ref_name: String,
        writable: bool,
    },
    Local {
        real_path: String,
    },
}

/// Result of resolving a virtual path through the backend layer.
pub struct BackendInfo {
    pub backend: String,
    pub real_path: String,
    pub cached: bool,
    pub cache_path: Option<String>,
    pub sync_status: String,
}
