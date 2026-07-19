# hilo-fuse — FUSE Virtual Filesystem

Kernel-level virtual filesystem that mounts repositories and backends as a unified directory tree. Files appear as regular files — agents use `cat`, `ls`, and `getfattr`.

**Crate:** `hilo-fuse`  
**Public modules:** 5

## Public API Surface

### Types

| Type | Description |
|------|-------------|
| `Hilo` | Main FUSE filesystem implementation — implements `fuser::Filesystem` |
| `InodeEntry` | In-memory inode — `{inode, path, kind, children, xattrs}` |
| `InodeKind` | `File` or `Directory` |
| `FuseConfig` | Mount config — `{mount_point, allow_other, direct_io, auto_unmount, attr_timeout, entry_timeout, max_read, max_write, sandbox}` |

### Re-exports

| Type | Source | Description |
|------|--------|-------------|
| `BubblewrapConfig` | `hilo_core::sandbox` | Agent sandbox config for bwrap |
| `PermissionRule` | `hilo_permissions` | Glob-based file permission rules |

### Modules

| Module | Description |
|--------|-------------|
| `daemon` | Daemon lifecycle — mount, unmount, signal handling |
| `ops` | FUSE operations — `getattr`, `readdir`, `lookup`, `read`, `getxattr`, `listxattr` |
| `permissions` | Permission enforcement — mode bits from manifest rules |
| `triggers` | FUSE-triggered file watchers |
| `workspace_mount` | Multi-repo workspace mounting |

## Usage Example

```rust
use hilo_fuse::{Hilo, FuseConfig};

let config = FuseConfig {
    mount_point: "/mnt/hilo".into(),
    allow_other: false,
    auto_unmount: true,
    ..Default::default()
};

let fs = Hilo::new(manifest, graph_db)?;
fs.mount(config)?;

// Filesystem is now mounted — agents can:
//   cat /mnt/hilo/src/main.rs
//   ls /mnt/hilo/pkg/
//   getfattr -n user.vfs.role /mnt/hilo/src/auth.rs
```

```bash
# CLI mount
hilo mount /mnt/hilo --triggers

# With workspace
hilo workspace mount
```
