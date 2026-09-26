//! Workspace FUSE mount — unified directory tree from multiple repos + backends.
//!
//! Routes filesystem operations (lookup, readdir, read, getxattr) to the
//! correct backing worktree directory based on the path prefix.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use fuser::{
    FileAttr, FileType, Filesystem, KernelConfig, MountOption, ReplyAttr, ReplyData,
    ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyWrite, ReplyXattr, Request,
};
use libc::{EACCES, ENODATA, ENOENT};

use hilo_core::workspace::MountEntry;

use crate::permissions::PermissionEngine;
use crate::FuseConfig;

const ROOT_INO: u64 = 1;
const TTL: Duration = Duration::from_secs(1);

#[derive(Clone, Debug)]
struct InodeEntry {
    pub path: PathBuf,
    pub kind: InodeKind,
    pub size: u64,
    pub mode: u32,
    /// Which mount entry index owns this inode (None for root)
    pub mount_idx: Option<usize>,
}

#[derive(Clone, Debug)]
enum InodeKind {
    File,
    Directory,
}

/// Multi-root workspace FUSE filesystem.
///
/// At the root directory, lists all mounted repos/backends as subdirectories.
/// Below root, routes operations to the correct backing worktree directory.
pub struct WorkspaceMount {
    mounts: Vec<MountEntry>,
    files: Arc<RwLock<HashMap<u64, InodeEntry>>>,
    next_inode: AtomicU64,
    permissions: PermissionEngine,
    #[allow(dead_code)]
    config: FuseConfig,
}

impl WorkspaceMount {
    /// Create a new `WorkspaceMount` from a mount plan.
    ///
    /// `permissions` controls mode enforcement for backend-mounted paths.
    /// Backend rules (by mount name) take priority over glob-based rules.
    pub fn new(mounts: Vec<MountEntry>, config: FuseConfig, permissions: PermissionEngine) -> Self {
        let mut files = HashMap::new();
        files.insert(
            ROOT_INO,
            InodeEntry {
                path: PathBuf::from("/"),
                kind: InodeKind::Directory,
                size: 0,
                mode: 0o755,
                mount_idx: None,
            },
        );
        WorkspaceMount {
            mounts,
            files: Arc::new(RwLock::new(files)),
            next_inode: AtomicU64::new(2),
            permissions,
            config,
        }
    }

    fn alloc_inode(&self) -> u64 {
        self.next_inode.fetch_add(1, Ordering::SeqCst)
    }

    /// Given a mount-relative path ("auth-service/src/main.go"),
    /// find which mount entry it belongs to and resolve to real path.
    #[allow(dead_code)]
    fn resolve_to_real(&self, rel_path: &Path) -> Option<(usize, PathBuf)> {
        let path_str = rel_path.to_string_lossy();
        // Strip leading "/" if present
        let path_str = path_str.strip_prefix('/').unwrap_or(&path_str);
        for (idx, mount) in self.mounts.iter().enumerate() {
            // Mount "at" path like "/mnt/vfs/auth-service/" — extract component "auth-service"
            let mount_name = Path::new(&mount.at)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            // Check if path starts with this mount name
            if path_str == mount_name {
                return Some((idx, mount.backing_path.clone()));
            }
            if let Some(rest) = path_str.strip_prefix(&format!("{}/", mount_name)) {
                return Some((idx, mount.backing_path.join(rest)));
            }
        }
        None
    }

    fn make_attr(&self, ino: u64, entry: &InodeEntry, writable: bool) -> FileAttr {
        let kind = match entry.kind {
            InodeKind::File => FileType::RegularFile,
            InodeKind::Directory => FileType::Directory,
        };
        let now = std::time::SystemTime::now();
        let mode = if !writable && matches!(entry.kind, InodeKind::File) {
            entry.mode & 0o555 // read-only for files
        } else {
            entry.mode
        };
        FileAttr {
            ino,
            size: entry.size,
            blocks: entry.size.div_ceil(512),
            atime: now,
            mtime: now,
            ctime: now,
            crtime: now,
            kind,
            perm: (mode & 0o7777) as u16,
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

    fn populate_root_children(&self) {
        let mut files = self.files.write().unwrap();
        for (idx, mount) in self.mounts.iter().enumerate() {
            let mount_name = Path::new(&mount.at)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| mount.name.clone());
            let child_path = PathBuf::from(&mount_name);
            // Skip if already exists
            if files.values().any(|e| e.path == child_path) {
                continue;
            }
            let ino = self.alloc_inode();
            files.insert(
                ino,
                InodeEntry {
                    path: child_path,
                    kind: InodeKind::Directory,
                    size: 0,
                    mode: 0o755,
                    mount_idx: Some(idx),
                },
            );
        }
    }

    /// Populate the direct children of the directory inode `dir_ino` from its
    /// backing directory.
    ///
    /// The root lists one entry per mount (its mount name); every other
    /// directory maps its workspace-relative path (e.g. `demo/src`) onto the
    /// owning mount's backing directory and lists exactly one level there, so
    /// nested lookups resolve instead of re-listing the repo root.
    /// Returns `false` when `dir_ino` is not a known directory (ENOENT).
    fn populate_dir_children(&self, dir_ino: u64) -> bool {
        if dir_ino == ROOT_INO {
            self.populate_root_children();
            return true;
        }
        let (mount_idx, dir_rel) = {
            let files = self.files.read().unwrap();
            match files.get(&dir_ino) {
                Some(e) if matches!(e.kind, InodeKind::Directory) => match e.mount_idx {
                    Some(idx) => (idx, e.path.clone()),
                    None => return true, // known directory outside any mount
                },
                _ => return false,
            }
        };
        let mount = &self.mounts[mount_idx];
        let mount_name = Path::new(&mount.at)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| mount.name.clone());
        let backing_dir = {
            let empty = PathBuf::new();
            let rest = dir_rel.strip_prefix(&mount_name).unwrap_or(&empty);
            mount.backing_path.join(rest)
        };
        let dir_entries = match std::fs::read_dir(&backing_dir) {
            Ok(e) => e,
            Err(_) => return true,
        };
        let mut files = self.files.write().unwrap();
        for entry in dir_entries.flatten() {
            let name = entry.file_name();
            let child_path = dir_rel.join(&name);
            if files.values().any(|e| e.path == child_path) {
                continue;
            }
            let metadata = entry.metadata();
            let (kind, size, mode) = match &metadata {
                Ok(m) if m.is_dir() => (InodeKind::Directory, 0, 0o755),
                Ok(m) => (InodeKind::File, m.len(), 0o644),
                Err(_) => continue,
            };
            let ino = self.alloc_inode();
            files.insert(
                ino,
                InodeEntry {
                    path: child_path,
                    kind,
                    size,
                    mode,
                    mount_idx: Some(mount_idx),
                },
            );
        }
        true
    }
}

impl Filesystem for WorkspaceMount {
    fn init(&mut self, _req: &Request, _config: &mut KernelConfig) -> Result<(), std::ffi::c_int> {
        Ok(())
    }

    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        if !self.populate_dir_children(parent) {
            reply.error(ENOENT);
            return;
        }

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

        let files = self.files.read().unwrap();
        for (ino, entry) in files.iter() {
            if entry.path == expected_path {
                let writable = entry
                    .mount_idx
                    .map(|idx| self.mounts[idx].writable)
                    .unwrap_or(true);
                let attr = self.make_attr(*ino, entry, writable);
                reply.entry(&TTL, &attr, 0);
                return;
            }
        }
        // Not in the populated table: the backing filesystem had no such
        // entry at population time (population failures surface as the same
        // ENOENT; the kernel retries after entry_timeout).
        reply.error(ENOENT);
    }

    fn getattr(&mut self, _req: &Request, ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
        let files = self.files.read().unwrap();
        match files.get(&ino) {
            Some(entry) => {
                let writable = entry
                    .mount_idx
                    .map(|idx| self.mounts[idx].writable)
                    .unwrap_or(true);
                let attr = self.make_attr(ino, entry, writable);
                reply.attr(&TTL, &attr);
            }
            None => reply.error(ENOENT),
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
        // Populate one level from the backing directory so a first readdir of
        // a nested directory lists its real children (lookup alone populates
        // only the ancestors it walks through).
        if !self.populate_dir_children(ino) {
            reply.error(ENOENT);
            return;
        }
        let files = self.files.read().unwrap();
        let dir_entry = match files.get(&ino) {
            Some(e) if matches!(e.kind, InodeKind::Directory) => e.clone(),
            _ => {
                reply.error(ENOENT);
                return;
            }
        };

        let dir_path = dir_entry.path.clone();
        let entries: Vec<(u64, PathBuf)> = files
            .iter()
            .filter(|(_, e)| {
                if ino == ROOT_INO {
                    // Root children: direct children (no "/" in path after stripping root)
                    e.path
                        .parent()
                        .map(|p| p.as_os_str().is_empty())
                        .unwrap_or(false)
                } else if e.path.starts_with(&dir_path) && e.path != dir_path {
                    // Children: one level deeper
                    e.path.parent().map(|p| p == dir_path).unwrap_or(false)
                } else {
                    false
                }
            })
            .map(|(i, e)| (*i, e.path.clone()))
            .collect();

        // Add . and ..
        if offset == 0 {
            let _ = reply.add(ino, 1, FileType::Directory, ".");
            let parent_ino = if ino == ROOT_INO {
                ROOT_INO
            } else {
                // Find parent or default to root
                1u64
            };
            let _ = reply.add(parent_ino, 2, FileType::Directory, "..");
        }

        let mut idx = 2i64;
        for (child_ino, child_path) in entries.iter() {
            if idx < offset {
                idx += 1;
                continue;
            }
            let name = child_path.file_name().unwrap_or_default();
            let file_type = match files.get(child_ino) {
                Some(e) if matches!(e.kind, InodeKind::Directory) => FileType::Directory,
                _ => FileType::RegularFile,
            };
            if reply.add(
                *child_ino,
                idx + 1,
                file_type,
                name.to_string_lossy().as_ref(),
            ) {
                break;
            }
            idx += 1;
        }
        reply.ok();
    }

    fn read(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        _size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        let files = self.files.read().unwrap();
        let entry = match files.get(&ino) {
            Some(e) if matches!(e.kind, InodeKind::File) => e.clone(),
            _ => {
                reply.error(ENOENT);
                return;
            }
        };

        let real_path = match entry.mount_idx {
            Some(idx) => {
                let mount_name = Path::new(&self.mounts[idx].at)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let empty = PathBuf::new();
                let rest = entry.path.strip_prefix(&mount_name).unwrap_or(&empty);
                self.mounts[idx].backing_path.join(rest)
            }
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        let data = match std::fs::read(&real_path) {
            Ok(d) => d,
            Err(_) => {
                reply.error(ENOENT);
                return;
            }
        };

        let start = offset as usize;
        if start >= data.len() {
            reply.data(&[]);
        } else {
            reply.data(&data[start..]);
        }
    }

    fn open(&mut self, _req: &Request, ino: u64, _flags: i32, reply: ReplyOpen) {
        // Enforce permission check for backend mounts
        let files = self.files.read().unwrap();
        if let Some(entry) = files.get(&ino) {
            use crate::permissions::PermissionOp;
            let access_mode = _flags & libc::O_ACCMODE;
            if access_mode == libc::O_WRONLY {
                if self
                    .permissions
                    .check(&entry.path, PermissionOp::Write)
                    .is_err()
                {
                    reply.error(EACCES);
                    return;
                }
            } else if access_mode == libc::O_RDWR
                && (self
                    .permissions
                    .check(&entry.path, PermissionOp::Read)
                    .is_err()
                    || self
                        .permissions
                        .check(&entry.path, PermissionOp::Write)
                        .is_err())
            {
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

    fn getxattr(
        &mut self,
        _req: &Request,
        _ino: u64,
        _name: &OsStr,
        _size: u32,
        reply: ReplyXattr,
    ) {
        reply.error(ENODATA);
    }

    fn listxattr(&mut self, _req: &Request, _ino: u64, _size: u32, reply: ReplyXattr) {
        reply.error(ENODATA);
    }

    fn write(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        _offset: i64,
        _data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        // Check backend permission first
        let files = self.files.read().unwrap();
        let entry = match files.get(&ino) {
            Some(e) => e.clone(),
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        // 1. Permission engine check (backend rules take priority)
        use crate::permissions::PermissionOp;
        if self
            .permissions
            .check(&entry.path, PermissionOp::Write)
            .is_err()
        {
            reply.error(EACCES);
            return;
        }

        // 2. Writable flag check
        let writable = entry
            .mount_idx
            .map(|idx| self.mounts[idx].writable)
            .unwrap_or(true);
        if !writable {
            reply.error(EACCES);
            return;
        }
        // Fall through to write implementation if writable
        reply.error(EACCES);
    }
}

/// Mount a `WorkspaceMount` filesystem at `config.mount_point`.
///
/// This call blocks until the filesystem is unmounted (or an error occurs).
/// On success returns `Ok(())`.
pub fn mount(fs: WorkspaceMount, config: &crate::FuseConfig) -> anyhow::Result<()> {
    let opts = mount_options(config);
    fuser::mount2(fs, &config.mount_point, &opts)?;
    Ok(())
}

fn mount_options(config: &crate::FuseConfig) -> Vec<MountOption> {
    let mut opts = vec![
        MountOption::RO,
        MountOption::FSName("hilo-workspace".into()),
    ];
    if config.allow_other {
        opts.push(MountOption::AllowOther);
    }
    if config.auto_unmount {
        opts.push(MountOption::AutoUnmount);
    }
    opts
}

// ============================================================
// TEST SEAMS (doc(hidden)) — used by tests/fuse_test.rs
// ============================================================

#[doc(hidden)]
pub fn ws_populate_dir_for_test(fs: &WorkspaceMount, dir_ino: u64) -> bool {
    fs.populate_dir_children(dir_ino)
}

#[doc(hidden)]
pub fn ws_inode_for_path_for_test(fs: &WorkspaceMount, rel: &str) -> Option<u64> {
    let files = fs.files.read().unwrap();
    files.iter().find(|(_, e)| e.path == *rel).map(|(i, _)| *i)
}

#[doc(hidden)]
pub fn ws_child_names_for_test(fs: &WorkspaceMount, dir_ino: u64) -> Vec<String> {
    let files = fs.files.read().unwrap();
    let dir_path = match files.get(&dir_ino) {
        Some(e) => e.path.clone(),
        None => return Vec::new(),
    };
    let mut names: Vec<String> = files
        .iter()
        .filter(|(_, e)| {
            e.path != dir_path && e.path.parent().map(|p| p == dir_path).unwrap_or(false)
        })
        .map(|(_, e)| {
            e.path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        })
        .collect();
    names.sort();
    names
}

#[doc(hidden)]
pub fn ws_backing_file_for_test(fs: &WorkspaceMount, rel: &str) -> Option<PathBuf> {
    let files = fs.files.read().unwrap();
    let entry = files
        .iter()
        .find(|(_, e)| e.path == *rel && matches!(e.kind, InodeKind::File))?
        .1
        .clone();
    let idx = entry.mount_idx?;
    let mount = fs.mounts.get(idx)?;
    let mount_name = Path::new(&mount.at)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let empty = PathBuf::new();
    let rest = entry.path.strip_prefix(&mount_name).unwrap_or(&empty);
    Some(mount.backing_path.join(rest))
}

#[doc(hidden)]
pub fn ws_root_ino_for_test() -> u64 {
    ROOT_INO
}
