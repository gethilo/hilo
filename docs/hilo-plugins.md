# hilo-plugins — WASM Plugin System

Extism-based WASM plugin runtime. Plugins are `.wasm` modules loaded from `.vfs/plugins/`. Written in any language with an Extism PDK (Rust, Go, Python, JS, C, Zig). Hot-loaded on manifest change. Sandboxed — no filesystem access except host functions.

**Crate:** `hilo-plugins`  
**Public modules:** 3

## Public API Surface

### Types

| Type | Description |
|------|-------------|
| `PluginRegistry` | Plugin discovery and loading — scans `.vfs/plugins/` for `.wasm` files |
| `PluginManifest` | Plugin metadata — `{name, version, hooks, edge_types, metadata_namespaces}` |
| `PluginRuntime` | Extism runtime — load WASM, call hooks, dispatch results |
| `HostFunctions` | Host functions available to plugins — `add_edge`, `set_xattr`, `get_file`, `query_graph`, `warn` |
| `PluginInstance` | Loaded plugin — `{name, wasm_path, hooks, edge_types, metadata_namespaces}` |
| `HookConfig` | Hook registration — `{on, priority, languages}` |
| `HookRef` | Reference to a hook — `{plugin, hook_name, priority}` |
| `HookResult` | Hook return value: `AddEdge{from, to, relation}`, `SetXattr{path, key, value}`, `Warning{path, message}` |

### PluginRegistry

```rust
pub struct PluginRegistry;

impl PluginRegistry {
    pub fn new(plugins_dir: impl Into<PathBuf>) -> Self;
    pub fn discover(&self) -> Result<Vec<PluginManifest>>;
    pub fn load(&self, manifest: &PluginManifest) -> Result<PluginInstance>;
}
```

### PluginRuntime

```rust
pub struct PluginRuntime;

impl PluginRuntime {
    pub fn new() -> Self;
    pub fn load(&mut self, instance: &PluginInstance) -> Result<()>;
    pub fn dispatch_hook(&mut self, hook_name: &str, data: &[u8]) -> Result<Vec<HookResult>>;
    pub fn call_function(&mut self, name: &str, data: &[u8]) -> Result<Vec<u8>>;
}
```

### HostFunctions

```rust
pub struct HostFunctions;

impl HostFunctions {
    pub fn add_edge(plugin: &mut CurrentPlugin, from: &str, to: &str, rel: &str);
    pub fn set_xattr(plugin: &mut CurrentPlugin, path: &str, key: &str, value: &str);
    pub fn get_file(plugin: &mut CurrentPlugin, path: &str) -> Vec<u8>;
    pub fn query_graph(plugin: &mut CurrentPlugin, query: &str) -> Vec<Edge>;
    pub fn warn(plugin: &mut CurrentPlugin, message: &str);
}
```

## Usage Example

```rust
use hilo_plugins::{PluginRegistry, PluginRuntime};

let registry = PluginRegistry::new(".vfs/plugins");
let manifests = registry.discover()?;

for manifest in manifests {
    let instance = registry.load(&manifest)?;
    let mut runtime = PluginRuntime::new();
    runtime.load(&instance)?;

    // Call a plugin hook
    let results = runtime.dispatch_hook("on_file_parse", source_bytes)?;
    for result in results {
        match result {
            HookResult::AddEdge { from, to, relation } => {
                println!("Plugin added edge: {} → {} ({})", from, to, relation);
            }
            HookResult::Warning { path, message } => {
                eprintln!("Plugin warning: {} - {}", path, message);
            }
            _ => {}
        }
    }
}
```
