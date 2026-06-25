// Trigger-related FUSE mount configuration.
//
// Bridges FuseConfig and the TriggerEngine, allowing the FUSE daemon to
// start/stop file watching alongside the mount lifecycle.

use std::path::PathBuf;

use crate::FuseConfig;

/// Configuration derived from FuseConfig for the TriggerEngine.
pub struct TriggerEngineConfig {
    pub enabled: bool,
    pub watch_dir: PathBuf,
    pub debounce_ms: u64,
    pub max_concurrent: usize,
}

impl FuseConfig {
    /// Returns true if triggers are enabled (default: true).
    ///
    /// When the `no_triggers` field is added to FuseConfig this will check it;
    /// for now triggers are always enabled.
    pub fn triggers_enabled(&self) -> bool {
        true
    }

    /// Build trigger engine config from FuseConfig.
    ///
    /// Uses the FUSE mount point as the watch directory and sensible defaults
    /// for debounce and concurrency.
    pub fn trigger_config(&self) -> TriggerEngineConfig {
        TriggerEngineConfig {
            enabled: self.triggers_enabled(),
            watch_dir: self.mount_point.clone(),
            debounce_ms: 500,
            max_concurrent: 4,
        }
    }
}
