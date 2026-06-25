// Plugin runtime — loads and manages plugin instances, dispatches hooks.
//
// The runtime maintains a list of loaded PluginInstance objects and a
// HostFunctions store. When a FUSE hook fires (file_write, file_read), the
// daemon calls dispatch_hook with the event name and file path. The runtime
// finds matching plugins (sorted by priority) and returns HookResults.
//
// In the full extism integration, dispatch_hook would call into each plugin's
// .wasm module. For v1, we simulate execution: plugins with edge_types
// containing "tested_by" produce AddEdge results, and all matching plugins
// produce a Warning result. This lets us test the dispatch pipeline without
// real .wasm modules.

use crate::host_functions::HostFunctions;
use crate::{HookConfig, HookResult, PluginInstance};
use std::path::Path;

pub struct PluginRuntime {
    pub plugins: Vec<PluginInstance>,
    pub host_functions: HostFunctions,
    /// Manifest holds the extism configuration. It is populated when real .wasm
    /// plugins are loaded; for now it is reserved for the full extism integration.
    #[allow(dead_code)]
    manifest: extism::Manifest,
}

impl PluginRuntime {
    /// Create a new runtime with an empty plugin list and default host functions.
    pub fn new() -> Self {
        Self {
            plugins: Vec::new(),
            host_functions: HostFunctions::new(),
            manifest: extism::Manifest::default(),
        }
    }

    /// Load a .wasm plugin from disk.
    ///
    /// Reads the wasm bytes, then creates a PluginInstance with default hook
    /// configuration. For real .wasm files the extism Plugin would be created
    /// here; for test stubs (empty / mock bytes) we still register the instance
    /// so the dispatch pipeline can be exercised.
    ///
    /// Returns the plugin name on success.
    pub fn load_plugin(&mut self, wasm_path: &Path) -> Result<String, String> {
        let _wasm_bytes = std::fs::read(wasm_path)
            .map_err(|e| format!("failed to read plugin file {}: {}", wasm_path.display(), e))?;

        let name = wasm_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        // Create a plugin instance with default configuration.
        // Real extism::Plugin::new would be called here for valid wasm;
        // for v1 we register the instance metadata so dispatch works.
        let instance = PluginInstance {
            name: name.clone(),
            wasm_path: wasm_path.to_path_buf(),
            hooks: vec![HookConfig {
                on: "file_write".into(),
                priority: 0,
                languages: vec![],
            }],
            edge_types: vec!["tested_by".into()],
            metadata_namespaces: vec![],
        };

        self.plugins.push(instance);
        Ok(name)
    }

    /// Unload a plugin by name. Returns true if a plugin was removed.
    pub fn unload_plugin(&mut self, name: &str) -> bool {
        let before = self.plugins.len();
        self.plugins.retain(|p| p.name != name);
        self.plugins.len() < before
    }

    /// Get a mutable reference to the host functions store.
    pub fn host_functions_mut(&mut self) -> &mut HostFunctions {
        &mut self.host_functions
    }

    /// Dispatch a hook event to all matching plugins.
    ///
    /// Iterates plugins in priority order. For each plugin whose hooks include
    /// `event`, simulates execution:
    ///   - Plugins with edge_types containing "tested_by" produce AddEdge.
    ///   - All matching plugins produce a Warning result.
    ///
    /// Returns the collected HookResults.
    pub fn dispatch_hook(&self, event: &str, path: &str, _data: &str) -> Vec<HookResult> {
        // Collect (priority, plugin_index) pairs for matching hooks.
        let mut matches: Vec<(u32, usize)> = Vec::new();
        for (idx, plugin) in self.plugins.iter().enumerate() {
            if let Some(min_prio) = plugin
                .hooks
                .iter()
                .filter(|h| h.on == event)
                .map(|h| h.priority)
                .min()
            {
                matches.push((min_prio, idx));
            }
        }

        // Sort by priority (ascending) for deterministic dispatch order.
        matches.sort_by_key(|(prio, _)| *prio);

        let mut results = Vec::new();
        for (_, idx) in matches {
            let plugin = &self.plugins[idx];

            // Simulate: plugins that declare "tested_by" edges add an edge.
            if plugin.edge_types.iter().any(|e| e == "tested_by") {
                results.push(HookResult::AddEdge {
                    from: path.to_string(),
                    to: "test_target".to_string(),
                    relation: "tested_by".to_string(),
                });
            }

            // Simulate: every matching plugin emits a warning.
            results.push(HookResult::Warning {
                path: path.to_string(),
                message: format!("hook '{}' executed by plugin '{}'", event, plugin.name),
            });
        }

        results
    }
}

impl Default for PluginRuntime {
    fn default() -> Self {
        Self::new()
    }
}
