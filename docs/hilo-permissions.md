# hilo-permissions — Permission Engine

Glob-based path matching with mode bits. Evaluates whether Read, Write, or Execute operations are allowed on a path. Rules iterate in order — first match wins. Used by FUSE for kernel-level enforcement and by the MCP server for agent access control.

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
