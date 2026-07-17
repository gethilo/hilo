// Plugin registry — discovers .wasm plugin files on disk.
//
// Scans a plugins directory (typically `.vfs/plugins/`) for .wasm files and
// produces PluginManifest entries. Each manifest describes what hooks the
// plugin registers and at what priority. In a full implementation the manifest
// would be embedded in the .wasm metadata; for v1 we use sensible defaults.

use std::path::{Path, PathBuf};

/// Metadata for a discovered plugin, derived from the filesystem.
pub struct PluginManifest {
    pub name: String,
    pub wasm_path: PathBuf,
    pub version: String,
    pub hooks: Vec<HookRef>,
    pub edge_types: Vec<String>,
}

/// A hook reference inside a manifest.
pub struct HookRef {
    pub on: String,
    pub priority: u32,
}

/// Stateless scanner for plugin directories.
pub struct PluginRegistry;

impl PluginRegistry {
    /// Discover all .wasm files in `plugins_dir`.
    ///
    /// Returns an empty vec if the directory does not exist (hot-load friendly).
    pub fn discover(plugins_dir: &Path) -> Result<Vec<PluginManifest>, String> {
        if !plugins_dir.exists() {
            return Ok(Vec::new());
        }

        let wasm_files = Self::scan_directory(plugins_dir)?;

        let manifests = wasm_files
            .into_iter()
            .map(|path| {
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
                    .to_string();
                PluginManifest {
                    name,
                    wasm_path: path,
                    version: "0.2.0".into(),
                    hooks: vec![HookRef {
                        on: "file_write".into(),
                        priority: 0,
                    }],
                    edge_types: Vec::new(),
                }
            })
            .collect();

        Ok(manifests)
    }

    /// Scan a single directory for .wasm files (non-recursive).
    fn scan_directory(dir: &Path) -> Result<Vec<PathBuf>, String> {
        let entries = std::fs::read_dir(dir)
            .map_err(|e| format!("failed to read plugin directory {}: {}", dir.display(), e))?;

        let mut wasm_files = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| format!("failed to read directory entry: {}", e))?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("wasm") {
                wasm_files.push(path);
            }
        }

        // Sort for deterministic ordering.
        wasm_files.sort();

        Ok(wasm_files)
    }
}
