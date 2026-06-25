//! FUSE filesystem operations.
//!
//! `Hilo` implements the `fuser::Filesystem` trait, exposing the on-disk
//! repository directory tree (with Hilo metadata xattrs) through a read-only
//! FUSE mount. The kernel enforces permission bits so AI agents can safely use
//! standard tools like `cat`, `ls`, and `getfattr` without special clients.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fuser::{
    FileAttr, FileType, Filesystem, KernelConfig, ReplyAttr, ReplyData, ReplyDirectory, ReplyEmpty,
    ReplyEntry, ReplyOpen, ReplyXattr, Request,
};

use libc::{EACCES, ENODATA, ENOENT};

use crate::permissions::PermissionEngine;
use crate::FuseConfig;

const ROOT_INO: u64 = 1;
const TTL: Duration = Duration::from_secs(1);

// ---------------------------------------------------------------------------
// Inode data model
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct InodeEntry {
    pub path: PathBuf,
    pub kind: InodeKind,
    pub size: u64,
    pub mode: u32,
}

#[derive(Clone, Debug)]
pub enum InodeKind {
    File,
    Directory,
}

// ---------------------------------------------------------------------------
// Hilo filesystem
// ---------------------------------------------------------------------------

pub struct Hilo {
    root: PathBuf,
    files: Arc<RwLock<HashMap<u64, InodeEntry>>>,
    next_inode: AtomicU64,
    config: FuseConfig,
    permissions: PermissionEngine,
}

impl Hilo {
    /// Create a new `Hilo` backed by `root`.
    ///
    /// The root inode (1) is pre-populated as a directory pointing at `root`.
    pub fn new(root: PathBuf, config: FuseConfig) -> Self {
        let permissions = PermissionEngine::from_rules(crate::permissions::default_protections());
        let mut files = HashMap::new();
        files.insert(
            ROOT_INO,
            InodeEntry {
                path: PathBuf::from("/"),
                kind: InodeKind::Directory,
                size: 0,
                mode: 0o755,
            },
        );
        Hilo {
            root,
            files: Arc::new(RwLock::new(files)),
            next_inode: AtomicU64::new(2),
            config,
            permissions,
        }
    }

    /// Allocate the next inode number atomically.
    fn alloc_inode(&self) -> u64 {
        self.next_inode.fetch_add(1, Ordering::SeqCst)
    }

    /// Resolve an inode number to its absolute filesystem path.
    ///
    /// Root (ino 1) resolves to `self.root`. Any other inode's stored `path`
    /// is joined onto `self.root`.
    pub fn resolve_path(&self, ino: u64) -> Option<PathBuf> {
        let files = self.files.read().unwrap();
        let entry = files.get(&ino)?;
        match ino {
            ROOT_INO => Some(self.root.clone()),
            _ => Some(self.root.join(&entry.path)),
        }
    }

    /// Lazily populate inode entries for the children of `dir_ino`.
    ///
    /// Reads the underlying directory on disk and creates inode entries for
    /// each child that does not already have one. This is idempotent.
    fn populate_directory(&self, dir_ino: u64) {
        let dir_path = match self.resolve_path(dir_ino) {
            Some(p) => p,
            None => return,
        };

        let entries = match std::fs::read_dir(&dir_path) {
            Ok(e) => e,
            Err(_) => return,
        };

        let mut files = self.files.write().unwrap();
        let dir_entry = match files.get(&dir_ino) {
            Some(e) => e.clone(),
            None => return,
        };

        for entry in entries.flatten() {
            let name = entry.file_name();
            let child_rel = match dir_ino {
                ROOT_INO => PathBuf::from(&name),
                _ => dir_entry.path.join(&name),
            };

            // Skip if we already have an inode for this relative path.
            let exists = files.values().any(|e| e.path == child_rel);
            if exists {
                continue;
            }

            let metadata = entry.metadata();
            let (kind, size, mode) = match &metadata {
                Ok(m) if m.is_dir() => (InodeKind::Directory, 0, 0o755),
                Ok(m) => (
                    InodeKind::File,
                    m.len(),
                    self.permissions.compute_mode(&child_rel),
                ),
                Err(_) => continue,
            };

            let ino = self.alloc_inode();
            files.insert(
                ino,
                InodeEntry {
                    path: child_rel,
                    kind,
                    size,
                    mode,
                },
            );
        }
    }

    /// Build a `FileAttr` from an `InodeEntry`.
    fn make_attr(&self, ino: u64, entry: &InodeEntry) -> FileAttr {
        let kind = match entry.kind {
            InodeKind::File => FileType::RegularFile,
            InodeKind::Directory => FileType::Directory,
        };
        let now = SystemTime::now();
        FileAttr {
            ino,
            size: entry.size,
            blocks: entry.size.div_ceil(512),
            atime: now,
            mtime: now,
            ctime: now,
            crtime: now,
            kind,
            perm: (entry.mode & 0o7777) as u16,
            nlink: if matches!(entry.kind, InodeKind::Directory) {
                2
            } else {
                1
            },
            uid: 0,
            gid: 0,
            rdev: 0,
            blksize: 512,
            flags: 0,
        }
    }

    /// Reference to the config (used by daemon code).
    pub fn config(&self) -> &FuseConfig {
        &self.config
    }
}

// ---------------------------------------------------------------------------
// Filesystem trait
// ---------------------------------------------------------------------------

impl Filesystem for Hilo {
    fn init(&mut self, _req: &Request, _config: &mut KernelConfig) -> Result<(), std::ffi::c_int> {
        Ok(())
    }

    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        // Ensure the parent directory's children are populated.
        self.populate_directory(parent);

        // Build the expected relative path for this child.
        let expected_path = {
            let files = self.files.read().unwrap();
            match files.get(&parent) {
                Some(_) if parent == ROOT_INO => PathBuf::from(name),
                Some(parent_entry) => parent_entry.path.join(name),
                None => {
                    reply.error(ENOENT);
                    return;
                }
            }
        };

        // Search for the child inode by relative path.
        let files = self.files.read().unwrap();
        for (ino, entry) in files.iter() {
            if entry.path == expected_path {
                let attr = self.make_attr(*ino, entry);
                reply.entry(&TTL, &attr, 0);
                return;
            }
        }
        reply.error(ENOENT);
    }

    fn getattr(&mut self, _req: &Request, ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
        let files = self.files.read().unwrap();
        if let Some(entry) = files.get(&ino) {
            let attr = self.make_attr(ino, entry);
            reply.attr(&TTL, &attr);
        } else {
            reply.error(ENOENT);
        }
    }

    fn readdir(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        if offset == 0 {
            self.populate_directory(ino);
        }

        let files = self.files.read().unwrap();

        // "." entry
        if offset <= 1 {
            if let Some(entry) = files.get(&ino) {
                let kind = match entry.kind {
                    InodeKind::Directory => FileType::Directory,
                    InodeKind::File => FileType::RegularFile,
                };
                if reply.add(ino, 1, kind, ".") {
                    reply.ok();
                    return;
                }
            } else {
                reply.error(ENOENT);
                return;
            }
        }

        // ".." entry
        if offset <= 2 && reply.add(1, 2, FileType::Directory, "..") {
            reply.ok();
            return;
        }

        // Collect children (sorted by name for deterministic ordering).
        let dir_entry = match files.get(&ino) {
            Some(e) => e.clone(),
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        let mut children: Vec<(u64, String, &InodeEntry)> = files
            .iter()
            .filter(|(_, e)| {
                if ino == ROOT_INO {
                    // Direct children of root have single-component relative paths.
                    e.path
                        .parent()
                        .map(|p| p.as_os_str().is_empty())
                        .unwrap_or(false)
                        && e.path != Path::new("/")
                } else {
                    e.path.parent() == Some(&dir_entry.path)
                }
            })
            .map(|(i, e)| {
                (
                    *i,
                    e.path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    e,
                )
            })
            .collect();
        children.sort_by(|a, b| a.1.cmp(&b.1));

        let child_offset = offset.saturating_sub(2) as usize;
        for (idx, (child_ino, child_name, child_entry)) in
            children.iter().skip(child_offset).enumerate()
        {
            let dir_offset = (idx + 3) as i64; // offset starts at 3 after "." and ".."
            let kind = match child_entry.kind {
                InodeKind::Directory => FileType::Directory,
                InodeKind::File => FileType::RegularFile,
            };
            if reply.add(*child_ino, dir_offset, kind, child_name.as_str()) {
                break;
            }
        }
        reply.ok();
    }

    fn read(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        let path = match self.resolve_path(ino) {
            Some(p) => p,
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        let data = match std::fs::read(&path) {
            Ok(d) => d,
            Err(_) => {
                reply.error(ENOENT);
                return;
            }
        };

        let offset = offset as usize;
        if offset >= data.len() {
            reply.data(&[]);
            return;
        }

        let end = (offset + size as usize).min(data.len());
        reply.data(&data[offset..end]);
    }

    fn getxattr(&mut self, _req: &Request, ino: u64, name: &OsStr, size: u32, reply: ReplyXattr) {
        let path = match self.resolve_path(ino) {
            Some(p) => p,
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        let name_str = match name.to_str() {
            Some(s) => s,
            None => {
                reply.error(ENODATA);
                return;
            }
        };

        match hilo_metadata::get_vfs_xattr(&path, name_str) {
            Ok(Some(value)) => {
                if size == 0 {
                    reply.size(value.len() as u32);
                } else {
                    reply.data(value.as_bytes());
                }
            }
            Ok(None) => {
                reply.error(ENODATA);
            }
            Err(_) => {
                reply.error(ENODATA);
            }
        }
    }

    fn listxattr(&mut self, _req: &Request, ino: u64, size: u32, reply: ReplyXattr) {
        let path = match self.resolve_path(ino) {
            Some(p) => p,
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        let attrs = match hilo_metadata::list_vfs_xattrs(&path) {
            Ok(a) => a,
            Err(_) => {
                if size == 0 {
                    reply.size(0);
                } else {
                    reply.data(&[]);
                }
                return;
            }
        };

        let mut list = attrs.join("\0");
        if !list.is_empty() {
            list.push('\0');
        }
        let list_bytes = list.as_bytes();
        let total_len = list_bytes.len() as u32;

        if size == 0 {
            reply.size(total_len);
        } else if total_len <= size {
            reply.data(list_bytes);
        } else {
            reply.error(libc::ERANGE);
        }
    }

    fn open(&mut self, _req: &Request, ino: u64, _flags: i32, reply: ReplyOpen) {
        // Enforce permission check — resolve the path for this inode
        // and verify the operation is allowed by the engine.
        let files = self.files.read().unwrap();
        if let Some(entry) = files.get(&ino) {
            use crate::permissions::PermissionOp;
            let access_mode = _flags & libc::O_ACCMODE;
            let op = if access_mode == libc::O_WRONLY {
                PermissionOp::Write
            } else if access_mode == libc::O_RDWR {
                // Check both read and write; deny if either fails
                if self
                    .permissions
                    .check(&entry.path, PermissionOp::Read)
                    .is_err()
                    || self
                        .permissions
                        .check(&entry.path, PermissionOp::Write)
                        .is_err()
                {
                    reply.error(EACCES);
                    return;
                }
                reply.opened(0, 0);
                return;
            } else {
                PermissionOp::Read
            };
            if self.permissions.check(&entry.path, op).is_err() {
                reply.error(EACCES);
                return;
            }
        }
        reply.opened(0, 0);
    }

    fn release(
        &mut self,
        _req: &Request,
        _ino: u64,
        _fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        reply.ok();
    }
}

/// Suppress unused import warning — `UNIX_EPOCH` is used in attribute helpers
/// when we need sub-second precision in future revisions.
#[allow(dead_code)]
fn _epoch_now() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
}

/// Helper used by tests: look up an inode by relative path.
#[allow(dead_code)]
pub fn inode_for_path(wfs: &Hilo, rel: &str) -> Option<u64> {
    let files = wfs.files.read().unwrap();
    let target = Path::new(rel);
    for (ino, entry) in files.iter() {
        if entry.path == target {
            return Some(*ino);
        }
    }
    None
}

/// Helper used by tests: populate a directory and return child inode count.
#[allow(dead_code)]
pub fn populated_child_count(wfs: &Hilo, dir_ino: u64) -> usize {
    wfs.populate_directory(dir_ino);
    let files = wfs.files.read().unwrap();
    let dir_entry = match files.get(&dir_ino) {
        Some(e) => e.clone(),
        None => return 0,
    };
    let parent_path = &dir_entry.path;
    files
        .values()
        .filter(|e| {
            if dir_ino == ROOT_INO {
                e.path.parent().is_none() && e.path != Path::new("/")
            } else {
                e.path.parent() == Some(parent_path)
            }
        })
        .count()
}
