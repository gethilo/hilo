# hilo-core — Configuration & Workspace

Foundation crate for Hilo's configuration, manifest, workspace management, sandboxing, and virtual directory layer.

**Crate:** `hilo-core`  
**Public modules:** 7

## Public API Surface

### Modules

| Module | Description |
|--------|-------------|
| `config` | Global Hilo configuration (paths, defaults) |
| `manifest` | `.vfs/manifest.yaml` parser — YAML schema with backends, permissions, triggers, performance, plugins |
| `sandbox` | Bubblewrap sandbox configuration for agent isolation |
| `virtual_dir` | Virtual directory abstraction over mounted backends |
| `workspace` | Multi-repo workspace management |
| `worktree` | Git worktree operations |
| `logging` | Shared logging utilities |

### Types (manifest module)

| Type | Description |
|------|-------------|
| `Manifest` | Top-level manifest — `{version, project, backends, permissions, triggers, performance, plugins}` |
| `ProjectConfig` | Project metadata — `{name, description}` |
| `BackendConfig` | Backend definition — `{id, type, config}` |
| `S3Config` | S3 backend — `{bucket, region, prefix, writable, cache_dir}` |
| `GitConfig` | Git backend — `{url, ref_name, worktree, writable}` |
| `PermissionConfig` | Permission rules — `{paths, mode, allow_delete}` |
| `TriggerConfig` | Trigger definitions — `{name, watch_pattern, events, command, ...}` |
| `PerformanceConfig` | Performance tuning — `{rate_limit_rps}` |
| `PluginConfig` | Plugin loading — `{enabled, paths}` |
| `ManifestError` | Manifest parse/validation errors |

### Types (sandbox module)

| Type | Description |
|------|-------------|
| `BubblewrapConfig` | bwrap sandbox config — `{rootfs, bind_mounts, network, seccomp}` |

## Usage Example

```rust
use hilo_core::manifest::Manifest;

// Load and parse manifest.yaml
let manifest = Manifest::load(".vfs/manifest.yaml")?;

// Access backends
for backend in &manifest.backends {
    println!("Backend: {} (type: {})", backend.id, backend.config_type());
}

// Access permissions
for rule in &manifest.permissions {
    println!("Permission: {:?} mode={:o}", rule.paths, rule.mode);
}

// Access performance settings
println!("Rate limit: {} req/s", manifest.performance.rate_limit_rps);
```
