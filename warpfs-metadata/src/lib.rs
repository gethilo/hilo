// WarpFS metadata engine — xattr read/write and inventory file I/O.
// See specs/warpfs-spec.md §15-16, §18.1.

pub mod xattr;
pub mod inventory;

pub use xattr::{set_vfs_xattr, get_vfs_xattr, list_vfs_xattrs, remove_vfs_xattr};
pub use inventory::{
    Edge, BackendMount,
    create_vfs_structure, append_edge, append_edges,
    read_mounts, write_mounts, edge_to_jsonl,
};

/// Errors that can arise during metadata operations.
#[derive(Debug, thiserror::Error)]
pub enum MetadataError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("xattr error: {0}")]
    Xattr(String),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_yaml::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("UTF-8 error: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
}
