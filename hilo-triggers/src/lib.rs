// Hilo Triggers — inotify-wired event engine
//
// Every file event fires triggers:
//   1. parse-and-diff → tree-sitter AST → edge updates
//   2. User-defined commands with {{ .FilePath }} templating
//   3. upload-to-backend → S3 sync
//
// Features: debouncing, async execution, timeouts, concurrency limiting

pub mod debounce;
pub mod engine;

pub use debounce::Debouncer;
pub use engine::TriggerEngine;

pub use std::path::PathBuf;

/// Trigger configuration from manifest.
#[derive(Clone)]
pub struct TriggerConfig {
    pub name: String,
    pub watch_pattern: String,
    pub events: Vec<String>,     // write, delete
    pub command: Option<String>, // shell command template
    pub builtin: Option<String>, // parse-and-diff, upload-to-backend
    pub async_exec: bool,
    pub timeout_secs: u64,
    pub debounce_ms: u64,
    pub on_success: Option<TriggerAction>,
    pub on_failure: Option<TriggerAction>,
    /// Impact depth for parse-and-diff trigger (default: None → 3).
    pub max_depth: Option<u32>,
    /// Path to DuckDB graph DB (default: None → derived from project root).
    pub graph_db_path: Option<String>,
}

#[derive(Clone)]
pub enum TriggerAction {
    SetXattr { key: String, value_template: String },
    Warn,
    Error,
}

/// A file event received from inotify.
#[derive(Clone)]
pub struct FileEvent {
    pub path: PathBuf,
    pub event_type: EventType,
    pub timestamp: u64, // unix timestamp
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum EventType {
    Write,
    Delete,
    Create,
}

impl Default for TriggerConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            watch_pattern: "*".to_string(),
            events: vec!["write".to_string(), "delete".to_string()],
            command: None,
            builtin: None,
            async_exec: true,
            timeout_secs: 30,
            debounce_ms: 500,
            on_success: None,
            on_failure: None,
            max_depth: None,
            graph_db_path: None,
        }
    }
}

/// Parse duration string to milliseconds: "500ms" -> 500, "2s" -> 2000, "30s" -> 30000
pub fn parse_duration_ms(s: &str) -> u64 {
    if let Some(n) = s.strip_suffix("ms") {
        n.parse::<u64>().unwrap_or(0)
    } else if let Some(n) = s.strip_suffix('s') {
        n.parse::<u64>().unwrap_or(0) * 1000
    } else {
        s.parse::<u64>().unwrap_or(0)
    }
}
