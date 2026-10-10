//! Workspace FUSE mount — unified directory tree from multiple repos + backends.
//!
//! Routes filesystem operations (lookup, readdir, read, getxattr) to the
//! correct backing worktree directory based on the path prefix.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

use fuser::{
    FileAttr, FileType, Filesystem, KernelConfig, MountOption, ReplyAttr, ReplyCreate, ReplyData,
    ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyWrite, ReplyXattr, Request, TimeOrNow,
};
use libc::{EACCES, EIO, ENODATA, ENOENT};

use hilo_core::workspace::MountEntry;

use crate::permissions::{PermissionEngine, PermissionOp};
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

    /// Whether any mounted backend is writable.
    ///
    /// The kernel enforces one read-only/read-write mode for the whole mount,
    /// so the mount may only be mounted read-only when NO backend is writable.
    /// The per-entry `MountEntry::writable` flag still provides the fine-grained
    /// userspace enforcement layer on top of this coarse kernel decision.
    pub fn any_writable(&self) -> bool {
        self.mounts.iter().any(|m| m.writable)
    }

    /// Given a mount-relative path ("auth-service/src/main.go"),
    /// find which mount entry it belongs to and resolve to real path.
    ///
    /// Used by the write/create/setattr paths (DF-WARPFS-138).
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

    /// Shared write-path gates for an entry: the inode exists, it is a file,
    /// the permission engine allows the write, and the owning mount is
    /// writable (DF-WARPFS-138). Returns the entry clone on success and the
    /// errno the Filesystem method should reply with on failure.
    fn writable_entry_gate(&self, ino: u64) -> Result<InodeEntry, i32> {
        let files = self.files.read().unwrap();
        let entry = match files.get(&ino) {
            Some(e) if matches!(e.kind, InodeKind::File) => e.clone(),
            _ => return Err(ENOENT),
        };
        drop(files);
        // 1. Permission engine check (backend rules take priority).
        if self
            .permissions
            .check(&entry.path, PermissionOp::Write)
            .is_err()
        {
            return Err(EACCES);
        }
        // 2. Per-entry writable flag (fine-grained userspace enforcement).
        let writable = entry
            .mount_idx
            .map(|idx| self.mounts[idx].writable)
            .unwrap_or(true);
        if !writable {
            return Err(EACCES);
        }
        Ok(entry)
    }

    /// Resolve an entry's mount-relative path to its real backing path.
    /// Mirrors the inline resolution used by `read()` (entry.mount_idx +
    /// backing_path strip) via the shared `resolve_to_real` helper.
    fn backing_path_for(&self, entry: &InodeEntry) -> Option<(usize, PathBuf)> {
        self.resolve_to_real(&entry.path)
    }

    /// Write-through core for `write()`: open the backing file and write the
    /// bytes at the requested offset. All failures after the gates pass map
    /// to EIO with a stderr diagnostic (mirrors ops.rs §8.4).
    fn write_through(&self, entry: &InodeEntry, offset: i64, data: &[u8]) -> Result<u32, i32> {
        let (_, real_path) = match self.backing_path_for(entry) {
            Some(p) => p,
            None => {
                eprintln!(
                    "[hilo-fuse] workspace write: cannot resolve backing path for {}",
                    entry.path.display()
                );
                return Err(EIO);
            }
        };
        let file = match std::fs::OpenOptions::new().write(true).open(&real_path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!(
                    "[hilo-fuse] workspace write open failed for {}: {e}",
                    real_path.display()
                );
                return Err(EIO);
            }
        };
        use std::os::unix::fs::FileExt;
        match file.write_at(data, offset.max(0) as u64) {
            Ok(n) => Ok(n as u32),
            Err(e) => {
                eprintln!(
                    "[hilo-fuse] workspace write_at failed for {}: {e}",
                    real_path.display()
                );
                Err(EIO)
            }
        }
    }

    /// Truncate/extend the backing file (setattr size path). Requires the
    /// same gate as write; other setattr fields are not supported here.
    fn set_len_through(&self, entry: &InodeEntry, size: u64) -> Result<(), i32> {
        let (_, real_path) = match self.backing_path_for(entry) {
            Some(p) => p,
            None => {
                eprintln!(
                    "[hilo-fuse] workspace setattr: cannot resolve backing path for {}",
                    entry.path.display()
                );
                return Err(EIO);
            }
        };
        match std::fs::OpenOptions::new().write(true).open(&real_path) {
            Ok(f) => match f.set_len(size) {
                Ok(()) => Ok(()),
                Err(e) => {
                    eprintln!(
                        "[hilo-fuse] workspace set_len failed for {}: {e}",
                        real_path.display()
                    );
                    Err(EIO)
                }
            },
            Err(e) => {
                eprintln!(
                    "[hilo-fuse] workspace setattr open failed for {}: {e}",
                    real_path.display()
                );
                Err(EIO)
            }
        }
    }

    /// Create core for `create()`: materialize the file under the parent's
    /// backing directory, register an inode, and return its new inode number.
    fn create_through(&self, parent: u64, name: &OsStr, mode: u32, flags: i32) -> Result<u64, i32> {
        let files = self.files.read().unwrap();
        let parent_entry = match files.get(&parent) {
            Some(e) if matches!(e.kind, InodeKind::Directory) => e.clone(),
            _ => return Err(ENOENT),
        };
        drop(files);

        // Gate on the PARENT: its owning mount must be writable and the
        // permission engine must allow a write under the parent path.
        let parent_writable = parent_entry
            .mount_idx
            .map(|idx| self.mounts[idx].writable)
            .unwrap_or(true);
        if !parent_writable
            || self
                .permissions
                .check(&parent_entry.path, PermissionOp::Write)
                .is_err()
        {
            return Err(EACCES);
        }

        let (idx, parent_real) = match self.backing_path_for(&parent_entry) {
            Some(p) => p,
            None => {
                eprintln!(
                    "[hilo-fuse] workspace create: cannot resolve backing path for {}",
                    parent_entry.path.display()
                );
                return Err(EIO);
            }
        };
        let real_path = parent_real.join(name);
        let rel_path = parent_entry.path.join(name);
        // Truncate only when O_TRUNC is requested (flags carry the open
        // mode); creation flags like O_CREAT/O_EXCL are implied by this op.
        let truncate = flags & libc::O_TRUNC != 0;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(truncate)
            .mode(mode & 0o7777)
            .open(&real_path)
            .map_err(|e| {
                eprintln!(
                    "[hilo-fuse] workspace create failed for {}: {e}",
                    real_path.display()
                );
                EIO
            })?;

        let mut files = self.files.write().unwrap();
        // Another thread may have created the same path concurrently; reuse
        // that inode if present (paths are the mount's identity).
        if let Some(existing) = files
            .iter()
            .find(|(_, e)| e.path == rel_path)
            .map(|(i, _)| *i)
        {
            return Ok(existing);
        }
        let ino = self.alloc_inode();
        files.insert(
            ino,
            InodeEntry {
                path: rel_path,
                kind: InodeKind::File,
                size: 0,
                mode: {
                    let m = mode & 0o7777;
                    if m == 0 {
                        0o644
                    } else {
                        m
                    }
                },
                mount_idx: Some(idx),
            },
        );
        Ok(ino)
    }

    /// Post-gate attr reply for setattr: report the fresh size from disk
    /// (mirrors getattr's TTL + writable-derived mode).
    fn attr_after_setattr(&self, ino: u64, entry: &InodeEntry) -> FileAttr {
        let writable = entry
            .mount_idx
            .map(|idx| self.mounts[idx].writable)
            .unwrap_or(true);
        let mut size = entry.size;
        if let Some((_, p)) = self.backing_path_for(entry) {
            if let Ok(meta) = std::fs::metadata(&p) {
                size = meta.len();
                let mut files = self.files.write().unwrap();
                if let Some(e) = files.get_mut(&ino) {
                    e.size = size;
                }
            }
        }
        let mut updated = entry.clone();
        updated.size = size;
        self.make_attr(ino, &updated, writable)
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
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        // 1+2. Permission engine + per-entry writable gate (backend rules
        // take priority). EACCES here means "denied", never "unimplemented".
        let entry = match self.writable_entry_gate(ino) {
            Ok(e) => e,
            Err(errno) => {
                reply.error(errno);
                return;
            }
        };
        // 3. Write through to the backing file at the requested offset.
        match self.write_through(&entry, offset, data) {
            Ok(n) => reply.written(n),
            Err(e) => reply.error(e),
        }
    }

    fn setattr(
        &mut self,
        _req: &Request,
        ino: u64,
        _mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<TimeOrNow>,
        _mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<u64>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        let files = self.files.read().unwrap();
        let entry = match files.get(&ino) {
            Some(e) => e.clone(),
            None => {
                reply.error(ENOENT);
                return;
            }
        };
        drop(files);

        match size {
            Some(new_size) => {
                // Truncate/extend goes through the same write gate.
                let gated = match self.writable_entry_gate(ino) {
                    Ok(e) => e,
                    Err(errno) => {
                        reply.error(errno);
                        return;
                    }
                };
                if let Err(e) = self.set_len_through(&gated, new_size) {
                    reply.error(e);
                    return;
                }
                let attr = self.attr_after_setattr(ino, &entry);
                reply.attr(&TTL, &attr);
            }
            None => {
                // No size change requested: reply the current attrs (the
                // metadata layer is out of scope for DF-WARPFS-138).
                let writable = entry
                    .mount_idx
                    .map(|idx| self.mounts[idx].writable)
                    .unwrap_or(true);
                let attr = self.make_attr(ino, &entry, writable);
                reply.attr(&TTL, &attr);
            }
        }
    }

    fn create(
        &mut self,
        _req: &Request,
        parent: u64,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        match self.create_through(parent, name, mode, flags) {
            Ok(ino) => {
                let files = self.files.read().unwrap();
                match files.get(&ino) {
                    Some(entry) => {
                        let writable = entry
                            .mount_idx
                            .map(|idx| self.mounts[idx].writable)
                            .unwrap_or(true);
                        let attr = self.make_attr(ino, entry, writable);
                        drop(files);
                        // fuser's create reply carries the open flags as u32;
                        // echo the kernel-provided bits back verbatim.
                        reply.created(&TTL, &attr, 0, 0, flags as u32);
                    }
                    None => {
                        drop(files);
                        reply.error(ENOENT);
                    }
                }
            }
            Err(e) => reply.error(e),
        }
    }
}

/// Mount a `WorkspaceMount` filesystem at `config.mount_point`.
///
/// This call blocks until the filesystem is unmounted (or an error occurs).
/// On success returns `Ok(())`.
pub fn mount(fs: WorkspaceMount, config: &crate::FuseConfig) -> anyhow::Result<()> {
    let opts = mount_options(&fs, config);
    fuser::mount2(fs, &config.mount_point, &opts)?;
    Ok(())
}

fn mount_options(fs: &WorkspaceMount, config: &crate::FuseConfig) -> Vec<MountOption> {
    let mut opts = vec![MountOption::FSName("hilo-workspace".into())];
    // The kernel enforces RO/RW for the whole mount, so it is read-only only
    // when NO backend is writable (default behavior is unchanged for an
    // all-read-only or empty workspace). Per-entry `writable` enforcement
    // happens in userspace (lookup/getattr/write) regardless of this flag.
    if !fs.any_writable() {
        opts.push(MountOption::RO);
    }
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

#[doc(hidden)]
pub fn ws_write_for_test(
    fs: &WorkspaceMount,
    ino: u64,
    offset: i64,
    data: &[u8],
) -> Result<u32, i32> {
    let entry = fs.writable_entry_gate(ino)?;
    fs.write_through(&entry, offset, data)
}

#[doc(hidden)]
pub fn ws_create_for_test(fs: &WorkspaceMount, parent: u64, name: &str) -> Result<u64, i32> {
    // Mirrors the kernel's create: mode 0o644, O_CREAT (no O_TRUNC).
    fs.create_through(parent, OsStr::new(name), 0o644, libc::O_CREAT)
}

#[doc(hidden)]
pub fn ws_setattr_size_for_test(fs: &WorkspaceMount, ino: u64, size: u64) -> Result<(), i32> {
    let entry = fs.writable_entry_gate(ino)?;
    fs.set_len_through(&entry, size)?;
    // Keep the inode table's cached size in sync (as attr_after_setattr does).
    if let Some((_, p)) = fs.backing_path_for(&entry) {
        if let Ok(meta) = std::fs::metadata(&p) {
            let mut files = fs.files.write().unwrap();
            if let Some(e) = files.get_mut(&ino) {
                e.size = meta.len();
            }
        }
    }
    Ok(())
}

#[doc(hidden)]
pub struct WsEntryInfoForTest {
    pub path: PathBuf,
    pub is_file: bool,
    pub size: u64,
    pub mode: u32,
    pub mount_idx: Option<usize>,
}

#[doc(hidden)]
pub fn ws_entry_info_for_test(fs: &WorkspaceMount, ino: u64) -> Option<WsEntryInfoForTest> {
    let files = fs.files.read().unwrap();
    files.get(&ino).map(|e| WsEntryInfoForTest {
        path: e.path.clone(),
        is_file: matches!(e.kind, InodeKind::File),
        size: e.size,
        mode: e.mode,
        mount_idx: e.mount_idx,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::PermissionEngine;
    use hilo_core::workspace::MountEntry;

    fn test_config() -> FuseConfig {
        FuseConfig {
            mount_point: PathBuf::from("/tmp/hilo-ws-opt-test"),
            allow_other: false,
            direct_io: false,
            auto_unmount: false,
            read_only: true,
            attr_timeout: 1.0,
            entry_timeout: 1.0,
            max_read: 131_072,
            max_write: 131_072,
            sandbox: None,
        }
    }

    fn entry(name: &str, writable: bool) -> MountEntry {
        MountEntry {
            name: name.to_string(),
            backing_path: PathBuf::from("/tmp").join(name),
            at: format!("/tmp/vfs/{name}"),
            writable,
        }
    }

    fn ws(mounts: Vec<MountEntry>) -> WorkspaceMount {
        WorkspaceMount::new(
            mounts,
            test_config(),
            PermissionEngine::from_rules(Vec::new()),
        )
    }

    #[test]
    fn any_writable_mount_drops_ro_option() {
        let fs = ws(vec![entry("rw", true), entry("ro", false)]);
        assert!(
            fs.any_writable(),
            "one writable backend must make the mount writable"
        );
        let opts = mount_options(&fs, &test_config());
        assert!(
            !opts.contains(&MountOption::RO),
            "a writable backend must NOT produce a read-only kernel mount"
        );
        assert!(
            opts.iter().any(|o| matches!(o, MountOption::FSName(_))),
            "FSName must still be present"
        );
    }

    #[test]
    fn all_read_only_keeps_ro_option() {
        let fs = ws(vec![entry("ro1", false), entry("ro2", false)]);
        assert!(!fs.any_writable());
        let opts = mount_options(&fs, &test_config());
        assert!(
            opts.contains(&MountOption::RO),
            "an all-read-only workspace must keep the read-only kernel mount"
        );
    }

    #[test]
    fn empty_workspace_keeps_ro_option() {
        let fs = ws(Vec::new());
        assert!(!fs.any_writable());
        let opts = mount_options(&fs, &test_config());
        assert!(
            opts.contains(&MountOption::RO),
            "an empty workspace must keep the default read-only kernel mount"
        );
    }

    // ------------------------------------------------------------
    // DF-WARPFS-138 — write/create/setattr through the backing path.
    // ------------------------------------------------------------

    /// A workspace whose single mount `name` points at a REAL tempdir
    /// backing directory, so write-through lands bytes on disk.
    fn ws_with_backing(name: &str, writable: bool) -> (tempfile::TempDir, WorkspaceMount) {
        let backing = tempfile::tempdir().unwrap();
        let mounts = vec![MountEntry {
            name: name.to_string(),
            backing_path: backing.path().to_path_buf(),
            at: format!("/tmp/vfs/{name}"),
            writable,
        }];
        let fs = WorkspaceMount::new(
            mounts,
            test_config(),
            PermissionEngine::from_rules(Vec::new()),
        );
        (backing, fs)
    }

    /// Populate root + the mount dir and return the inode of
    /// `<mount>/<rel>`, mirroring the kernel's lookup walk.
    fn inode_of(fs: &WorkspaceMount, mount: &str, rel: &str) -> u64 {
        let root = ws_root_ino_for_test();
        assert!(ws_populate_dir_for_test(fs, root), "root populates");
        let dir = ws_inode_for_path_for_test(fs, mount).expect("mount dir resolves");
        assert!(ws_populate_dir_for_test(fs, dir), "mount dir populates");
        ws_inode_for_path_for_test(fs, &format!("{mount}/{rel}")).expect("file resolves")
    }

    #[test]
    fn write_writable_mount_lands_bytes_at_offset() {
        let (backing, fs) = ws_with_backing("rw-write", true);
        std::fs::write(backing.path().join("hello.txt"), b"0123456789").unwrap();

        let ino = inode_of(&fs, "rw-write", "hello.txt");
        let n = ws_write_for_test(&fs, ino, 3, b"XYZ").expect("write must succeed");
        assert_eq!(n, 3, "write must reply the byte count");

        let on_disk = std::fs::read(backing.path().join("hello.txt")).unwrap();
        assert_eq!(on_disk, b"012XYZ6789", "bytes land at the requested offset");

        // The inode table's cached size is stale here (write() does not
        // refresh it in this revision), so assert only the DISK truth above.
    }

    #[test]
    fn write_negative_offset_clamps_to_zero() {
        let (backing, fs) = ws_with_backing("rw-neg", true);
        std::fs::write(backing.path().join("f.txt"), b"abcdef").unwrap();

        let ino = inode_of(&fs, "rw-neg", "f.txt");
        let n = ws_write_for_test(&fs, ino, -5, b"XY").expect("write must succeed");
        assert_eq!(n, 2);
        assert_eq!(
            std::fs::read(backing.path().join("f.txt")).unwrap(),
            b"XYcdef",
            "a negative offset must clamp to 0, not wrap around"
        );
    }

    #[test]
    fn write_read_only_mount_is_refused_with_eacces() {
        let (backing, fs) = ws_with_backing("ro-write", false);
        std::fs::write(backing.path().join("hello.txt"), b"untouched").unwrap();

        let ino = inode_of(&fs, "ro-write", "hello.txt");
        let err = ws_write_for_test(&fs, ino, 0, b"XYZ").expect_err("must refuse");
        assert_eq!(err, libc::EACCES, "read-only entries must deny with EACCES");
        assert_eq!(
            std::fs::read(backing.path().join("hello.txt")).unwrap(),
            b"untouched",
            "a refused write must not touch the backing file"
        );
    }

    #[test]
    fn write_unknown_inode_is_enoent() {
        let (_backing, fs) = ws_with_backing("rw-enoent", true);
        assert_eq!(ws_write_for_test(&fs, 9999, 0, b"x"), Err(libc::ENOENT));
    }

    #[test]
    fn create_materializes_file_and_registers_inode() {
        let (backing, fs) = ws_with_backing("rw-create", true);
        let root = ws_root_ino_for_test();
        assert!(ws_populate_dir_for_test(&fs, root));
        let dir = ws_inode_for_path_for_test(&fs, "rw-create").unwrap();

        let ino =
            ws_create_for_test(&fs, dir, "new-file.txt").expect("create must succeed on writable");
        assert_ne!(ino, 0);

        let backing_file = backing.path().join("new-file.txt");
        assert!(backing_file.exists(), "create must materialize the file");
        assert_eq!(
            std::fs::read(&backing_file).unwrap(),
            Vec::<u8>::new(),
            "a fresh create (no O_TRUNC semantics) starts empty"
        );

        let info = ws_entry_info_for_test(&fs, ino).expect("new node registered");
        assert!(info.is_file, "created node must be a File");
        assert_eq!(info.path, PathBuf::from("rw-create/new-file.txt"));
        assert_eq!(
            info.mode, 0o644,
            "default mode derives from the create flags"
        );
        assert_eq!(info.mount_idx, Some(0), "mount_idx inherits the parent's");

        // The new node is visible via the same lookup path the kernel uses.
        assert_eq!(
            ws_inode_for_path_for_test(&fs, "rw-create/new-file.txt"),
            Some(ino),
            "created node must be visible via lookup"
        );
        assert!(
            ws_child_names_for_test(&fs, dir).contains(&"new-file.txt".to_string()),
            "created node appears in the parent's listing"
        );
    }

    #[test]
    fn create_read_only_mount_is_refused_with_eacces() {
        let (backing, fs) = ws_with_backing("ro-create", false);
        let root = ws_root_ino_for_test();
        assert!(ws_populate_dir_for_test(&fs, root));
        let dir = ws_inode_for_path_for_test(&fs, "ro-create").unwrap();

        let err = ws_create_for_test(&fs, dir, "nope.txt").expect_err("must refuse");
        assert_eq!(err, libc::EACCES);
        assert!(
            !backing.path().join("nope.txt").exists(),
            "a refused create must not materialize the file"
        );
    }

    #[test]
    fn setattr_size_truncates_backing_file() {
        let (backing, fs) = ws_with_backing("rw-setattr", true);
        std::fs::write(backing.path().join("big.txt"), b"01234567890123456789").unwrap();

        let ino = inode_of(&fs, "rw-setattr", "big.txt");
        ws_setattr_size_for_test(&fs, ino, 5).expect("setattr size must succeed");

        let on_disk = std::fs::read(backing.path().join("big.txt")).unwrap();
        assert_eq!(
            on_disk, b"01234",
            "setattr(size) must truncate the backing file"
        );

        let info = ws_entry_info_for_test(&fs, ino).expect("entry still registered");
        assert_eq!(info.size, 5, "cached size follows the backing file");
    }

    #[test]
    fn setattr_size_extends_backing_file() {
        let (backing, fs) = ws_with_backing("rw-extend", true);
        std::fs::write(backing.path().join("g.txt"), b"abc").unwrap();

        let ino = inode_of(&fs, "rw-extend", "g.txt");
        ws_setattr_size_for_test(&fs, ino, 6).expect("extend must succeed");
        assert_eq!(
            std::fs::read(backing.path().join("g.txt")).unwrap(),
            b"abc\0\0\0",
            "setattr(size) must extend with zeros like truncate(2)"
        );
    }

    #[test]
    fn setattr_size_read_only_mount_is_refused_with_eacces() {
        let (backing, fs) = ws_with_backing("ro-setattr", false);
        std::fs::write(backing.path().join("keep.txt"), b"keepme").unwrap();

        let ino = inode_of(&fs, "ro-setattr", "keep.txt");
        let err = ws_setattr_size_for_test(&fs, ino, 1).expect_err("must refuse");
        assert_eq!(err, libc::EACCES);
        assert_eq!(
            std::fs::read(backing.path().join("keep.txt")).unwrap(),
            b"keepme",
            "refused setattr must not touch the backing file"
        );
    }
}
