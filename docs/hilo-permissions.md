# hilo-permissions — Permission Engine

Glob-based path matching with mode bits. Evaluates whether Read, Write, or Execute operations are allowed on a path. Rules iterate in order — first match wins.

## Current enforcement status (read this before relying on permissions)

As of v0.3.0:

- **FUSE enforcement uses hardcoded default protections only.** The FUSE
  engine is built from `default_protections()` (`hilo-fuse/src/ops.rs`);
  it enforces those defaults on `open`, independent of the manifest.
- **Manifest `permissions.rules`, `default_mode`, and `backends` are parsed
  by `hilo-core` but NOT currently consumed by FUSE.** Rules in
  `.vfs/manifest.yaml` have no effect on any surface.
- **MCP permission enforcement is not implemented.** `hilo-mcp` contains no
  permission code; the `PermissionEngine` in this crate is not wired into
  the MCP server.

The standard mount is also read-only (`hilo mount` sets `read_only=true`),
so write protection today comes from the kernel, not from this crate.

**Crate:** `hilo-permissions`  
**Public modules:** 1 (flat crate)

## Public API Surface

### Types

| Type | Description |
|------|-------------|
| `PermissionEngine` | Main engine — load rules, check permissions |
| `PermissionRule` | Glob-based rule — `{paths: Vec<String>, mode: u32, allow_delete: bool}` |
| `BackendPermissionRule` | Backend-level rule — `{name: String, mode: u32}`. Applies to all paths under a mount. |
| `PermissionOp` | Operation enum — `Read`, `Write`, `Execute` |
| `PermissionResult` | Check result — `{mode: u32, readable: bool, writable: bool, executable: bool, deletable: bool}` |
| `PermissionError` | `Denied{path, op, mode}` — includes path, operation, and octal mode |

### PermissionEngine

```rust
pub struct PermissionEngine;

impl PermissionEngine {
    /// Create engine from a list of glob-based rules.
    pub fn from_rules(rules: Vec<PermissionRule>) -> Self;

    /// Create engine from rules + backend-level rules (priority over glob rules).
    pub fn from_rules_with_backend(
        rules: Vec<PermissionRule>,
        backend_rules: Vec<BackendPermissionRule>,
    ) -> Self;

    /// Check if an operation is allowed on a path.
    pub fn check(&self, path: &str, op: PermissionOp) -> Result<PermissionResult, PermissionError>;

    /// Compute the effective mode for a path (without checking a specific operation).
    pub fn effective_mode(&self, path: &str) -> u32;
}
```

## Usage Example

```rust
use hilo_permissions::{PermissionEngine, PermissionRule, PermissionOp};

let rules = vec![
    PermissionRule {
        paths: vec!["src/**".into()],
        mode: 0o644,
        allow_delete: true,
    },
    PermissionRule {
        paths: vec![".vfs/**".into()],
        mode: 0o444,
        allow_delete: false,
    },
];
let engine = PermissionEngine::from_rules(rules);

// Read is allowed on source
assert!(engine.check("src/main.rs", PermissionOp::Read).is_ok());

// Write is allowed on source
assert!(engine.check("src/main.rs", PermissionOp::Write).is_ok());

// Write is DENIED on .vfs
assert!(engine.check(".vfs/manifest.yaml", PermissionOp::Write).is_err());

// Get effective mode
let mode = engine.effective_mode("src/lib.rs");
assert_eq!(mode, 0o644);
```

## Mode Bits

Octal modes follow Unix conventions:

| Mode | Meaning | Typical Use |
|------|---------|-------------|
| `0o444` | Read-only | Dependency repos, .vfs metadata |
| `0o644` | Read-write | User source code |
| `0o755` | Read-write-execute | Scripts, entrypoints |
| `0o000` | No access | Blocked paths |
