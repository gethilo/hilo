// Hilo metadata engine — xattr read/write and inventory file I/O.
// See specs/hilo-spec.md §15-16, §18.1.

pub mod inventory;
pub mod xattr;

pub use inventory::{
    append_edge, append_edges, append_edges_deduped, create_vfs_structure, edge_to_jsonl,
    read_mounts, write_mounts, BackendMount, Edge,
};
pub use xattr::{get_vfs_xattr, list_vfs_xattrs, remove_vfs_xattr, set_vfs_xattr};

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
