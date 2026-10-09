# hilo-plugins — WASM Plugin System

> **NOT IMPLEMENTED — plugin execution does not exist (DF-WARPFS-59).**
> Manifest `plugins:` blocks are accepted for backward compatibility, but
> no code path connects declared hooks to the trigger engine or the FUSE
> daemon: **a declared hook never fires.** `hilo plugin load|list` only
> validate metadata and persist files — they never execute anything.
> Loading a manifest that declares plugins prints a loud warning naming
> each plugin and hook. `hilo init` no longer writes a `plugins:` block.

WASM plugin runtime built on the Extism runtime types. Plugins are `.wasm` modules loaded from `.vfs/plugins/`. Written in any language with an Extism PDK (Rust, Go, Python, JS, C, Zig).

**Status (2026-09-22, DF-WARPFS-22):** header validation and honest metadata are live — a module is accepted only when it carries the `\0asm` magic and version 1, and loaded instances report `0 hooks, 0 edge types` until real hook discovery exists. NOT yet implemented: real wasm execution (`PluginRuntime::dispatch_hook` currently *simulates* results from declared hooks — a plugin declaring `tested_by` yields a canned `AddEdge`), inotify hot-load on manifest change, and host-function registration as callable `extism::Function` instances (see `host_functions.rs`). Sandboxing is inherited from Extism once execution lands.

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

    // Call a plugin hook (simulated execution for now — see status note above)
    let results = runtime.dispatch_hook("on_file_parse", "src/lib.rs", source_bytes);
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

## CLI

The `hilo plugin` subcommands expose the validate-and-persist path from the
shell. They are **metadata operations only** — neither command executes
plugin code (DF-WARPFS-59).

Run them from the workspace root: both resolve `.vfs/plugins/` relative to
the current directory. A ready-made module to try is checked in at
[`examples/plugins/minimal.wasm`](../examples/plugins/README.md).

### `hilo plugin load <file>.wasm`

Load a `.wasm` file, validate its header, and persist it into
`.vfs/plugins/` so `hilo plugin list` can find it.

```bash
hilo plugin load examples/plugins/minimal.wasm
# loaded plugin: minimal
#   path: /abs/path/to/examples/plugins/minimal.wasm
#   hooks: 0
#   edge_types: []
# persisted to: .vfs/plugins/minimal.wasm
```

Behaviour:

- **Validation is a header check, not execution.** The file must start with
  the `\0asm` magic at byte 0 and carry wasm version 1 at bytes 4–8. A text
  file (even one named `.wasm`) is rejected *before* anything is registered:
  `failed to load plugin: invalid wasm module <path>: missing \0asm magic at
  byte 0 (file is N bytes)`. An unsupported version fails with
  `... unsupported version at bytes 4-8`.
- **Pre-flight errors:** `plugin file not found: <path>` when the file is
  missing, and `plugin file must have a .wasm extension: <path>` when the
  extension is not `.wasm`.
- **Persistence:** the file is copied to `.vfs/plugins/<basename>` (a load
  that only registered in memory would be a silent no-op). If the source is
  already inside `.vfs/plugins/`, the copy is skipped and the command prints
  `already in .vfs/plugins: <path>` instead of truncating the file onto
  itself.
- **Honest output:** a validated module reports `hooks: 0` and
  `edge_types: []` — no manifest is parsed yet, so nothing is fabricated
  (DF-WARPFS-22). The plugin name is the file stem (`minimal.wasm` →
  `minimal`).

### `hilo plugin list`

Scan `.vfs/plugins/` (non-recursive) for `.wasm` files and print one line
per discoverable plugin.

```bash
hilo plugin list
# plugins in .vfs/plugins:
#   minimal v? — 0 hooks, 0 edge types
```

Behaviour:

- **Invalid files are skipped, not errors.** Any file failing the same
  header check `load` enforces (bad magic, wrong version, too short) is
  omitted from the listing — the plugins directory may hold
  work-in-progress files, and listing must never report metadata for them.
- **Version is `?`.** No manifest is parsed yet, so the version is always
  the unknown marker `?`; hooks and edge types are likewise reported as
  `0`. Fabricated metadata is never printed.
- An empty (or missing) plugins directory prints
  `no plugins found in .vfs/plugins`. Manifests are sorted by name.
