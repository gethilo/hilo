// Hilo FUSE — kernel-level virtual filesystem
//
// Mounts repos + backends as a unified directory tree.
// The Linux kernel enforces permissions (mode bits).
// Agents use cat/ls/getfattr — no special tools needed.

pub mod daemon;
pub mod ops;
pub mod permissions;
pub mod triggers;
pub mod workspace_mount;

use std::path::PathBuf;

pub use hilo_core::sandbox::BubblewrapConfig;
pub use ops::{Hilo, InodeEntry, InodeKind};

/// FUSE mount configuration from manifest.
#[derive(Clone)]
pub struct FuseConfig {
    pub mount_point: PathBuf,
    pub allow_other: bool,
    pub direct_io: bool,
    pub auto_unmount: bool,
    pub attr_timeout: f64,
    pub entry_timeout: f64,
    pub max_read: u32,
    pub max_write: u32,
    /// Bubblewrap sandbox configuration for agent isolation (§14.3).
    /// When `Some(...)`, agent process execution is sandboxed via bwrap.
    pub sandbox: Option<BubblewrapConfig>,
}

pub use hilo_permissions::PermissionRule;
