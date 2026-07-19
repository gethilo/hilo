# hilo-metadata — Metadata Engine

Extended attribute (xattr) read/write and JSONL inventory file I/O. The shared `Edge` type used by all graph operations lives here.

**Crate:** `hilo-metadata`  
**Public modules:** 2

## Public API Surface

### Types

| Type | Description |
|------|-------------|
| `Edge` | Graph edge — `{from, to, rel, provenance, confidence}`. Serializes to/from JSONL. Provenance defaults to `"ast_exact"`, confidence to `1.0`. |
| `BackendMount` | Backend mount entry — `{id, backend_type, config, mount_point}` |
| `MetadataError` | Error type: `Io`, `Xattr`, `Serde`, `Json`, `Utf8` |

### Functions

| Function | Description |
|----------|-------------|
| `Edge::new(from, to, rel) -> Self` | Create edge with default provenance (ast_exact, 1.0) |
| `Edge::with_provenance(from, to, rel, provenance, confidence) -> Self` | Create edge with explicit provenance |
| `get_vfs_xattr(path, key) -> Result<Option<String>>` | Read a `user.vfs.*` xattr from a file |
| `set_vfs_xattr(path, key, value) -> Result<()>` | Write a `user.vfs.*` xattr |
| `list_vfs_xattrs(path) -> Result<Vec<String>>` | List all `user.vfs.*` xattrs on a file |
| `remove_vfs_xattr(path, key) -> Result<()>` | Remove a `user.vfs.*` xattr |
| `append_edge(path, edge) -> Result<()>` | Append one edge to edges.jsonl |
| `append_edges(path, edges) -> Result<()>` | Append multiple edges to edges.jsonl |
| `append_edges_deduped(path, edges) -> Result<()>` | Append edges with dedup (by from+to+rel+provenance) |
| `edge_to_jsonl(edge) -> String` | Serialize edge to JSONL line |
| `create_vfs_structure(root) -> Result<()>` | Create `.vfs/graph/` and `.vfs/backends/` directories |
| `read_mounts(path) -> Result<Vec<BackendMount>>` | Read backend mounts from mounts.yaml |
| `write_mounts(path, mounts) -> Result<()>` | Write backend mounts to mounts.yaml |

## Usage Example

```rust
use hilo_metadata::{Edge, set_vfs_xattr, get_vfs_xattr, append_edge, Provenance};

// Create an edge with provenance
let edge = Edge::with_provenance(
    "src/main.go".into(),
    "pkg:fmt".into(),
    "imports".into(),
    "ast_exact".into(),
    1.0,
);

// Append to edges.jsonl
append_edge(".vfs/graph/edges.jsonl", edge)?;

// Set metadata xattr on a file
set_vfs_xattr("src/auth.rs", "user.vfs.feature", "auth-module")?;

// Read it back
let feature = get_vfs_xattr("src/auth.rs", "user.vfs.feature")?;
// => Some("auth-module")
```
