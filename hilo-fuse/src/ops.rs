//! FUSE filesystem operations.
//!
//! `Hilo` implements the `fuser::Filesystem` trait, exposing the on-disk
//! repository directory tree (with Hilo metadata xattrs) through a read-only
//! FUSE mount. The kernel enforces permission bits so AI agents can safely use
//! standard tools like `cat`, `ls`, and `getfattr` without special clients.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fuser::{
    FileAttr, FileType, Filesystem, KernelConfig, ReplyAttr, ReplyData, ReplyDirectory, ReplyEmpty,
    ReplyEntry, ReplyOpen, ReplyWrite, ReplyXattr, Request,
};

use libc::{EACCES, EIO, ENODATA, ENOENT};

use crate::permissions::PermissionEngine;
use crate::stream::StreamState;
use crate::FuseConfig;
use hilo_backends::IgnoreMatcher;

const ROOT_INO: u64 = 1;
const TTL: Duration = Duration::from_secs(1);

/// DF-WARPFS-5: build the entries a `readdir` at `offset` must return.
///
/// Offsets are FUSE cookies: entry N is emitted at offset N+1 and a resume
/// passes the LAST cookie it saw. `.`=1 and `..`=2 are therefore emitted only
/// when the resume offset is BELOW them.
///
/// This used to compare with `<=` (`if offset <= 1` for `.`, `offset <= 2` for
/// `..`), which is correct only while some later entry raises the cookie past 2.
/// On a directory with no children the last cookie the kernel ever sees is 2,
/// so the next `readdir(offset=2)` re-emitted `..` with cookie 2 — and the
/// kernel asked again, forever. That is the reported hang: `ls` on the empty
/// dir never returned, `find -type f` over the mount produced nothing and hung,
/// while every non-empty sibling answered in ~110 ms (a child's cookie is >= 3,
/// so the next call matched nothing and terminated the stream).
///
/// The rule is "emit entry k iff offset < k", which returns an empty batch for
/// offset >= 3 and lets the resume terminate.
fn readdir_entries(
    offset: i64,
    children: &[(u64, String, bool)], // (ino, name, is_dir)
) -> Vec<(u64, i64, String, bool)> {
    let mut out = Vec::new();
    // "." at cookie 1
    if offset < 1 {
        out.push((ROOT_INO, 1, ".".to_string(), true));
    }
    // ".." at cookie 2
    if offset < 2 {
        out.push((ROOT_INO, 2, "..".to_string(), true));
    }
    // children start at cookie 3
    let skip = offset.saturating_sub(2).max(0) as usize;
    for (idx, (ino, name, is_dir)) in children.iter().skip(skip).enumerate() {
        let cookie = (idx + 3) as i64;
        out.push((*ino, cookie, name.clone(), *is_dir));
    }
    out
}

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

/// DF-WARPFS-135: the inode map plus a parent→children index, kept behind ONE
/// lock so they can never diverge.
///
/// Before this, `readdir` filtered the ENTIRE inode map and evaluated
/// `Path::parent()` on every entry per listing, `populate_directory` scanned
/// every entry to detect an already-known child, and `lookup` scanned by
/// relative path — all O(all inodes) per call. On a large mount (tens of
/// thousands of inodes) that O(n)-per-readdir is the reported FUSE readdir
/// performance regression.
///
/// `children[parent_ino][name] = child_ino` is written in the same critical
/// section as `files`, so an inode always has exactly its index slot. The
/// key is the raw `OsString` file name as produced by `read_dir` at creation
/// time; every inode in `files` was inserted by `populate_directory` under the
/// directory whose own relative path is `entry.path.parent()`, which is the
/// same directory `populate_directory(dir_ino)` would derive from — so the
/// index cannot miss a previously-created child (duplicate detection, lookup,
/// and readdir enumeration all see the identical set the old full-map scans
/// computed), and nothing evicts inodes, so it cannot go stale.
#[derive(Default)]
struct InodeStore {
    files: HashMap<u64, InodeEntry>,
    /// parent inode number → child name → child inode number
    children: HashMap<u64, HashMap<OsString, u64>>,
}

impl InodeStore {
    /// Indexed child lookup: replaces the old full-map path scan.
    fn child_ino(&self, parent: u64, name: &OsStr) -> Option<u64> {
        self.children.get(&parent)?.get(name).copied()
    }

    /// Insert a freshly-created inode and index it under `parent` atomically
    /// (same write-lock hold as the caller's duplicate check).
    fn insert_child(&mut self, parent: u64, name: OsString, ino: u64, entry: InodeEntry) {
        self.files.insert(ino, entry);
        self.children.entry(parent).or_default().insert(name, ino);
    }
}

// ---------------------------------------------------------------------------
// Hilo filesystem
// ---------------------------------------------------------------------------

pub struct Hilo {
    root: PathBuf,
    /// DF-WARPFS-135: inode map + parent→children index behind one lock.
    files: Arc<RwLock<InodeStore>>,
    next_inode: AtomicU64,
    config: FuseConfig,
    permissions: PermissionEngine,
    /// Stream-mode state (§8): placeholder plan + backend for lazy
    /// materialization. `None` on plain read-only mounts.
    stream: Option<Arc<StreamState>>,
    /// DF-WARPFS-7: the workspace ignore stack. The mount previously walked the
    /// tree with a raw `read_dir` and never consulted it, so `.git/`, `.vfs/`
    /// and a 130 GB `target/` were served through the mount even though
    /// `hilo ignore check target/` reported `ignored: true`. `None` = no
    /// filtering (used by tests and by callers that opt out explicitly).
    ignores: Option<Arc<IgnoreMatcher>>,
}

impl Hilo {
    /// Create a new `Hilo` backed by `root`.
    ///
    /// The root inode (1) is pre-populated as a directory pointing at `root`.
    pub fn new(root: PathBuf, config: FuseConfig) -> Self {
        let permissions = PermissionEngine::from_rules(crate::permissions::default_protections());
        let mut store = InodeStore::default();
        store.files.insert(
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
            files: Arc::new(RwLock::new(store)),
            next_inode: AtomicU64::new(2),
            config,
            permissions,
            stream: None,
            ignores: None,
        }
    }

    /// DF-WARPFS-7: attach the workspace ignore stack so the mount honours the
    /// same ignore policy every other path already enforces (`hilo ignore
    /// check`, backend sync, ephemeral overlay). Without this the mount is the
    /// one surface that serves `.git/`, `.vfs/` and `target/`.
    pub fn with_ignores(mut self, matcher: IgnoreMatcher) -> Self {
        self.ignores = Some(Arc::new(matcher));
        self
    }

    /// Whether `rel` is excluded by the ignore stack. A missing matcher means
    /// "no filtering", so an unconfigured mount keeps its previous behaviour
    /// rather than silently hiding the tree.
    fn is_ignored(&self, rel: &Path) -> bool {
        let Some(matcher) = &self.ignores else {
            return false;
        };
        let posix = rel.to_string_lossy().replace('\\', "/");
        if posix.is_empty() || posix == "/" {
            return false;
        }
        // The root itself is never ignorable, and a trailing slash is how the
        // stack spells a directory rule (target/ vs a file named target).
        matcher.is_ignored(&posix)
    }

    /// Attach stream-mode state (spec §8): the placeholder plan and backend
    /// used to materialize lazily on open. The caller (the mount command)
    /// creates the placeholder files on disk before the filesystem starts
    /// serving.
    pub fn with_stream(mut self, stream: Arc<StreamState>) -> Self {
        self.stream = Some(stream);
        self
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
        let store = self.files.read().unwrap();
        let entry = store.files.get(&ino)?;
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

        let mut store = self.files.write().unwrap();
        let dir_entry = match store.files.get(&dir_ino) {
            Some(e) => e.clone(),
            None => return,
        };

        for entry in entries.flatten() {
            let name = entry.file_name();
            let child_rel = match dir_ino {
                ROOT_INO => PathBuf::from(&name),
                _ => dir_entry.path.join(&name),
            };

            // DF-WARPFS-135: skip if we already have an inode for this child.
            // Indexed by parent+name — replaces `files.values().any(...)`,
            // which was O(all inodes) per child per populate call.
            if store.child_ino(dir_ino, &name).is_some() {
                continue;
            }

            // DF-WARPFS-7: ...and skip anything the ignore stack excludes, so
            // the mount agrees with `hilo ignore check`. Checked before the
            // metadata call so an ignored 130 GB target/ is never even stat'd.
            if self.is_ignored(&child_rel) {
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
            // Creation-time indexing: the inode and its index slot land in the
            // same write-lock hold, so the two views cannot diverge.
            store.insert_child(
                dir_ino,
                name,
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

    /// DF-WARPFS-135: sorted (ino, name, is_dir) children of `dir_ino`, read
    /// from the parent→children index. Shared by `readdir` (full listing) and
    /// the test helpers so enumerated order matches the FUSE reply exactly.
    ///
    /// This replaces the per-listing `files.iter().filter(|_| e.path.parent()
    /// == ...)` pass, which reparsed every inode in the filesystem for each
    /// directory read.
    fn indexed_children(&self, dir_ino: u64) -> Vec<(u64, String, bool)> {
        let store = self.files.read().unwrap();
        let Some(map) = store.children.get(&dir_ino) else {
            return Vec::new();
        };
        let mut children: Vec<(u64, String, bool)> = map
            .iter()
            .filter_map(|(name, &ino)| {
                let entry = store.files.get(&ino)?;
                let is_dir = matches!(entry.kind, InodeKind::Directory);
                Some((ino, name.to_string_lossy().into_owned(), is_dir))
            })
            .collect();
        children.sort_by(|a, b| a.1.cmp(&b.1));
        children
    }

    /// Build a `FileAttr` from an `InodeEntry`.
    fn make_attr(&self, ino: u64, entry: &InodeEntry) -> FileAttr {
        let kind = match entry.kind {
            InodeKind::File => FileType::RegularFile,
            InodeKind::Directory => FileType::Directory,
        };
        // §8.2: on stream mounts an unmaterialized placeholder reports the
        // remote size from the walk listing; once materialized (or for any
        // non-placeholder) the on-disk size is authoritative — read it
        // fresh so writes through the mount are reflected.
        let size = if let Some(stream) = &self.stream {
            let full = if ino == ROOT_INO {
                self.root.clone()
            } else {
                self.root.join(&entry.path)
            };
            match stream.remote_size(&entry.path, &full) {
                Some(remote) => remote,
                None if matches!(entry.kind, InodeKind::File) => std::fs::metadata(&full)
                    .map(|m| m.len())
                    .unwrap_or(entry.size),
                None => entry.size,
            }
        } else {
            entry.size
        };
        let now = SystemTime::now();
        // DF-WARPFS-8: report the BACKING file's real timestamps.
        //
        // These four fields used to be `now` — the moment of the stat call — so
        // every file through the mount claimed the current time. Measured:
        // README.md disk 20:13:55 vs mount 20:47:12; Makefile disk 2026-07-19 vs
        // mount 20:47:12; and two sessions minutes apart reported different
        // values for the same unchanged files. Sizes and content were already
        // byte-exact, so this was purely metadata dishonesty — and it silently
        // broke every mtime-based tool (cargo/rsync/make/`git status`) run over
        // a mount, since nothing ever looked modified.
        //
        // A failed stat falls back to `now`: an approximate time is better than
        // an error here, and the caller cannot act on a getattr failure anyway.
        let (atime, mtime, ctime, crtime) = match self.backing_metadata(ino, entry) {
            Some(md) => (
                md.accessed().unwrap_or(now),
                md.modified().unwrap_or(now),
                // std exposes no birth/change time portably; modified time is
                // the closest honest substitute for ctime, and real ctime is
                // used for crtime where the platform provides it.
                md.modified().unwrap_or(now),
                md.created()
                    .unwrap_or_else(|_| md.modified().unwrap_or(now)),
            ),
            None => (now, now, now, now),
        };
        FileAttr {
            ino,
            size,
            blocks: size.div_ceil(512),
            atime,
            mtime,
            ctime,
            crtime,
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

    /// The on-disk metadata for an inode's backing path (DF-WARPFS-8).
    /// `None` when the inode is the root, has no backing file, or cannot be
    /// stat'd — callers then fall back rather than inventing a value.
    fn backing_metadata(&self, ino: u64, entry: &InodeEntry) -> Option<std::fs::Metadata> {
        let path = if ino == ROOT_INO {
            self.root.clone()
        } else {
            self.root.join(&entry.path)
        };
        std::fs::metadata(path).ok()
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

        // DF-WARPFS-135: resolve parent+name through the index — replaces the
        // full-map scan by relative path. The index is populated at inode
        // creation under the same parent `populate_directory` just walked, so
        // it sees exactly the children the old path-equality scan did.
        let store = self.files.read().unwrap();
        if !store.files.contains_key(&parent) {
            reply.error(ENOENT);
            return;
        }
        match store.child_ino(parent, name) {
            Some(ino) => {
                let attr = self.make_attr(ino, &store.files[&ino]);
                reply.entry(&TTL, &attr, 0);
            }
            None => reply.error(ENOENT),
        }
    }

    fn getattr(&mut self, _req: &Request, ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
        let store = self.files.read().unwrap();
        if let Some(entry) = store.files.get(&ino) {
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

        // A directory we do not know about is an error; resolve that BEFORE
        // emitting anything so the reply is unambiguous.
        {
            let store = self.files.read().unwrap();
            if !store.files.contains_key(&ino) {
                reply.error(ENOENT);
                return;
            }
        }

        // DF-WARPFS-135: children come from the parent→children index
        // (sorted by name for deterministic ordering). The old path filtered
        // the entire inode map and ran `Path::parent()` on every entry for
        // every listing.
        let children = self.indexed_children(ino);

        // DF-WARPFS-5: entry list is computed in one place, and the resume rule
        // is `offset < cookie`. Emitting `.`/`..` on `offset <= k` re-sent `..`
        // forever on an EMPTY directory (whose last cookie is 2), which hung
        // every `ls`/`find` that touched one. See `readdir_entries`.
        for (entry_ino, cookie, name, is_dir) in readdir_entries(offset, &children) {
            let kind = if is_dir {
                FileType::Directory
            } else {
                FileType::RegularFile
            };
            if reply.add(entry_ino, cookie, kind, name.as_str()) {
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
        use crate::permissions::PermissionOp;
        // §8.3/§8.4: opening a stream placeholder materializes it before the
        // kernel proceeds (read or write open). Metadata-only operations
        // (getxattr/listxattr) never materialize (§13.14).
        if let Some(stream) = self.stream.clone() {
            let rel = self
                .files
                .read()
                .unwrap()
                .files
                .get(&ino)
                .map(|e| e.path.clone());
            if let Some(rel) = rel {
                if let Some(full) = self.resolve_path(ino) {
                    if let Err(e) = stream.materialize(&rel, &full) {
                        eprintln!(
                            "[hilo-fuse] stream materialize failed for {}: {e}",
                            full.display()
                        );
                        reply.error(EIO);
                        return;
                    }
                }
            }
        }
        // Enforce permission check — resolve the path for this inode
        // and verify the operation is allowed by the engine.
        let store = self.files.read().unwrap();
        if let Some(entry) = store.files.get(&ino) {
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
        // §8.4: write-through — a placeholder was materialized at open, so
        // the write applies to the backing file and the inotify/sync-hook
        // dirty→push flow picks it up. The mount exposes no create/mkdir/
        // unlink, so the tree structure stays read-only.
        let path = match self.resolve_path(ino) {
            Some(p) => p,
            None => {
                reply.error(ENOENT);
                return;
            }
        };
        let file = match std::fs::OpenOptions::new().write(true).open(&path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("[hilo-fuse] write open failed for {}: {e}", path.display());
                reply.error(EIO);
                return;
            }
        };
        use std::os::unix::fs::FileExt;
        match file.write_at(data, offset.max(0) as u64) {
            Ok(n) => reply.written(n as u32),
            Err(e) => {
                eprintln!("[hilo-fuse] write_at failed for {}: {e}", path.display());
                reply.error(EIO);
            }
        }
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
    let store = wfs.files.read().unwrap();
    let target = Path::new(rel);
    for (ino, entry) in store.files.iter() {
        if entry.path == target {
            return Some(*ino);
        }
    }
    None
}

/// Test-only: the readdir entry builder, so the resume/cookie semantics are
/// provable without a kernel mount (DF-WARPFS-5).
#[doc(hidden)]
pub fn readdir_entries_for_test(
    offset: i64,
    children: &[(u64, String, bool)],
) -> Vec<(u64, i64, String, bool)> {
    readdir_entries(offset, children)
}

/// Test-only: the `FileAttr` the mount would report for an inode (DF-WARPFS-8),
/// so metadata honesty is provable without a kernel mount.
#[doc(hidden)]
pub fn attr_for_test(wfs: &Hilo, ino: u64) -> Option<FileAttr> {
    let store = wfs.files.read().unwrap();
    let entry = store.files.get(&ino)?;
    Some(wfs.make_attr(ino, entry))
}

/// Test-only: does a bare `Hilo` (no stack attached) filter anything? (DF-7)
#[doc(hidden)]
pub fn is_ignored_for_test_absent() -> bool {
    let tmp = std::env::temp_dir();
    let cfg = crate::FuseConfig {
        mount_point: tmp.clone(),
        allow_other: false,
        direct_io: false,
        auto_unmount: false,
        read_only: true,
        attr_timeout: 1.0,
        entry_timeout: 1.0,
        max_read: 131_072,
        max_write: 131_072,
        sandbox: None,
    };
    let fs = Hilo::new(tmp, cfg);
    fs.is_ignored(Path::new("target"))
}

/// Test-only: the ignore predicate the mount applies to a relative path
/// (DF-WARPFS-7), so the agreement with `hilo ignore check` is provable.
#[doc(hidden)]
pub fn is_ignored_for_test(matcher: IgnoreMatcher, rel: &str) -> bool {
    let tmp = std::env::temp_dir();
    let cfg = crate::FuseConfig {
        mount_point: tmp.clone(),
        allow_other: false,
        direct_io: false,
        auto_unmount: false,
        read_only: true,
        attr_timeout: 1.0,
        entry_timeout: 1.0,
        max_read: 131_072,
        max_write: 131_072,
        sandbox: None,
    };
    let fs = Hilo::new(tmp, cfg).with_ignores(matcher);
    fs.is_ignored(Path::new(rel))
}

/// Helper used by tests: populate a directory and return child inode count.
#[allow(dead_code)]
pub fn populated_child_count(wfs: &Hilo, dir_ino: u64) -> usize {
    wfs.populate_directory(dir_ino);
    // DF-WARPFS-135: counted through the parent→children index. The index
    // count is what readdir enumerates, so this helper stays consistent with
    // the hot path it is used alongside.
    let store = wfs.files.read().unwrap();
    if !store.files.contains_key(&dir_ino) {
        return 0;
    }
    store.children.get(&dir_ino).map(|m| m.len()).unwrap_or(0)
}

/// Test-only: the readdir entries the mount would produce for `dir_ino` at
/// `offset` — populate + indexed child enumeration + the shared cookie
/// builder, i.e. exactly the pipeline `Filesystem::readdir` runs, minus the
/// kernel reply object (DF-WARPFS-135).
#[doc(hidden)]
pub fn readdir_listing_for_test(
    wfs: &Hilo,
    dir_ino: u64,
    offset: i64,
) -> Vec<(u64, i64, String, bool)> {
    wfs.populate_directory(dir_ino);
    let children = wfs.indexed_children(dir_ino);
    readdir_entries(offset, &children)
}

/// Test-only: indexed child-membership probe — `Some(ino)` iff the
/// parent→children index holds `name` under `parent` (DF-WARPFS-135).
#[doc(hidden)]
pub fn child_ino_for_test(wfs: &Hilo, parent: u64, name: &OsStr) -> Option<u64> {
    let store = wfs.files.read().unwrap();
    store.child_ino(parent, name)
}
