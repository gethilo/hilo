// Host functions exposed to plugins.
//
// In the full extism integration these would be registered as extism::Function
// instances so .wasm modules can call them via the extism PDK. For v1 we expose
// a dispatch method (call_host_function) that lets tests and the runtime invoke
// host logic without a real .wasm module. The struct fields double as
// accumulator stores that mirror what real plugin execution would produce.

use std::collections::HashMap;

/// Accumulator stores for host-function side-effects.
///
/// When a real .wasm plugin calls `add_edge`, the edge lands in `edges`.
/// When it calls `set_xattr`, the value lands in `xattr_store`.
/// Tests can inspect these directly to verify plugin behaviour.
pub struct HostFunctions {
    /// Fake file store (plugins call get_file_content).
    pub file_store: HashMap<String, String>,
    /// Fake xattr store keyed by (path, key).
    pub xattr_store: HashMap<(String, String), String>,
    /// Edge accumulator (plugins call add_edge).
    pub edges: Vec<(String, String, String)>,
    /// Warning accumulator (plugins call emit_warning).
    pub warnings: Vec<(String, String)>,
}

impl HostFunctions {
    /// Create a new HostFunctions with all stores empty.
    pub fn new() -> Self {
        Self {
            file_store: HashMap::new(),
            xattr_store: HashMap::new(),
            edges: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// Dispatch to the correct mock host function by name.
    ///
    /// This is the v1 simplified interface. In the full extism integration,
    /// each case here maps to an `extism::Function` registered on the plugin.
    pub fn call_host_function(&mut self, name: &str, args: &[String]) -> Result<String, String> {
        match name {
            "get_file_content" => {
                let path = args.first().ok_or("get_file_content: missing path")?;
                Ok(self.file_store.get(path).cloned().unwrap_or_default())
            }
            "get_xattr" => {
                let path = args.first().ok_or("get_xattr: missing path")?;
                let key = args.get(1).ok_or("get_xattr: missing key")?;
                Ok(self
                    .xattr_store
                    .get(&(path.clone(), key.clone()))
                    .cloned()
                    .unwrap_or_default())
            }
            "set_xattr" => {
                let path = args.first().ok_or("set_xattr: missing path")?;
                let key = args.get(1).ok_or("set_xattr: missing key")?;
                let value = args.get(2).ok_or("set_xattr: missing value")?;
                self.xattr_store
                    .insert((path.clone(), key.clone()), value.clone());
                Ok("ok".into())
            }
            "add_edge" => {
                let from = args.first().ok_or("add_edge: missing from")?;
                let to = args.get(1).ok_or("add_edge: missing to")?;
                let relation = args.get(2).ok_or("add_edge: missing relation")?;
                self.edges
                    .push((from.clone(), to.clone(), relation.clone()));
                Ok("ok".into())
            }
            "query_graph" => {
                // Stub — real graph integration comes in a later phase.
                Ok("[]".into())
            }
            "emit_warning" => {
                let path = args.first().ok_or("emit_warning: missing path")?;
                let message = args.get(1).ok_or("emit_warning: missing message")?;
                self.warnings.push((path.clone(), message.clone()));
                Ok("ok".into())
            }
            _ => Err(format!("unknown host function: {}", name)),
        }
    }
}

impl Default for HostFunctions {
    fn default() -> Self {
        Self::new()
    }
}
