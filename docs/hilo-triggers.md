# hilo-triggers — Event Engine

Inotify-wired event engine. Every file save triggers parse-and-diff (tree-sitter AST → edge updates), user-defined shell commands with `{{ .FilePath }}` templating, and upload-to-backend sync.

**Crate:** `hilo-triggers`  
**Public modules:** 2

## Public API Surface

### Types

| Type | Description |
|------|-------------|
| `TriggerEngine` | Main engine — watch files, dispatch triggers, manage lifecycle |
| `TriggerConfig` | Per-trigger config — `{name, watch_pattern, events, command, builtin, async_exec, timeout_secs, debounce_ms, on_success, on_failure, max_depth, graph_db_path}` |
| `TriggerAction` | On-success/on-failure action — `SetXattr {key, value_template}`, `Warn`, `Error` |
| `FileEvent` | File event — `{path, event_type, timestamp}` |
| `EventType` | `Write`, `Delete`, `Create` |
| `Debouncer` | Debounce logic — group rapid events into one trigger |

### TriggerEngine

```rust
pub struct TriggerEngine {
    pub fn new(configs: Vec<TriggerConfig>, graph_db_path: impl Into<PathBuf>) -> Self;
    pub fn run(&mut self) -> Result<()>;        // Blocking event loop
    pub fn shutdown(&self);                      // Graceful shutdown
}
```

### DeBouncer

```rust
pub struct Debouncer;

impl Debouncer {
    pub fn new(debounce_ms: u64) -> Self;
    pub fn should_fire(&mut self, path: &Path) -> bool;
}
```

### Built-in Triggers

| Builtin | Description |
|---------|-------------|
| `parse-and-diff` | Re-parse changed files, diff edges, update DuckDB |
| `upload-to-backend` | Sync changed files to S3/Git backends |

## Usage Example

```rust
use hilo_triggers::{TriggerEngine, TriggerConfig};

let configs = vec![
    TriggerConfig {
        name: "parse-on-save".into(),
        watch_pattern: "src/**/*.go".into(),
        events: vec!["write".into()],
        builtin: Some("parse-and-diff".into()),
        debounce_ms: 250,
        ..Default::default()
    },
    TriggerConfig {
        name: "sync-docs".into(),
        watch_pattern: "docs/**".into(),
        events: vec!["write".into()],
        command: Some("echo 'Syncing {{ .FilePath }}'".into()),
        async_exec: true,
        ..Default::default()
    },
];

let mut engine = TriggerEngine::new(configs, ".vfs/graph/graph.db");
engine.run()?;  // Blocks until shutdown
```

```bash
# CLI: mount with triggers enabled
hilo mount /mnt/hilo --triggers
```
