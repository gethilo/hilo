// Trigger engine — watches directories with inotify, debounces events,
// and fires trigger callbacks (builtin or shell command).
//
// Event flow:
//   inotify event -> mask_to_event_type -> pattern match -> debounce -> execute

use crate::{Debouncer, FileEvent, EventType, TriggerAction, TriggerConfig};
use inotify::{EventMask, Inotify, WatchDescriptor, WatchMask};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::time::timeout;

pub struct TriggerEngine {
    watcher: Inotify,
    debouncer: Debouncer,
    /// Trigger configs loaded from manifest.
    triggers: Vec<TriggerConfig>,
    /// Watch descriptors by path.
    watches: HashMap<PathBuf, WatchDescriptor>,
    /// Global debounce default from manifest (500ms default).
    debounce_default_ms: u64,
    /// Max concurrent trigger executions.
    max_concurrent: usize,
    /// Timeout for async trigger execution.
    trigger_timeout: Duration,
}

impl TriggerEngine {
    /// Create a new engine. Does NOT start watching yet.
    pub fn new(triggers: Vec<TriggerConfig>, debounce_default_ms: u64) -> Self {
        let trigger_timeout = triggers
            .first()
            .map(|t| Duration::from_secs(t.timeout_secs))
            .unwrap_or(Duration::from_secs(30));

        let watcher = Inotify::init()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))
            .expect("TriggerEngine::new: failed to initialize inotify");

        Self {
            watcher,
            debouncer: Debouncer::new(debounce_default_ms),
            triggers,
            watches: HashMap::new(),
            debounce_default_ms,
            max_concurrent: 4,
            trigger_timeout,
        }
    }

    /// Add a directory to watch recursively. Returns count of watches added.
    /// For each directory, adds an IN_CLOSE_WRITE | IN_DELETE | IN_CREATE watch.
    pub fn watch_dir(&mut self, dir: &Path) -> io::Result<usize> {
        let mut count = 0;
        let mask = WatchMask::CLOSE_WRITE | WatchMask::DELETE | WatchMask::CREATE;

        // Add watch on this directory.
        let wd = self
            .watcher
            .watches()
            .add(dir, mask)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
        self.watches.insert(dir.to_path_buf(), wd);
        count += 1;

        // Recurse into subdirectories.
        let entries = std::fs::read_dir(dir)?;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                count += self.watch_dir(&path)?;
            }
        }

        Ok(count)
    }

    /// Run the event loop. Blocks until cancelled.
    ///
    /// For each inotify event:
    /// 1. Convert mask to EventType
    /// 2. Build FileEvent
    /// 3. Match against trigger configs (pattern + event filter)
    /// 4. Debounce per-file
    /// 5. Fire trigger (async spawn or inline)
    pub async fn run(&mut self) -> io::Result<()> {
        let mut buffer = [0u8; 4096];

        loop {
            // Read raw events (blocking — inotify fd is in blocking mode).
            let events = self
                .watcher
                .read_events(&mut buffer)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

            for event in events {
                let mask = event.mask;
                let name = event.name.map(|n| n.to_owned());

                // Map inotify mask to our EventType.
                let event_type = match mask_to_event_type(mask) {
                    Some(et) => et,
                    None => continue,
                };

                // Need a filename for pattern matching.
                let name = match name {
                    Some(n) => n,
                    None => continue,
                };

                let path = PathBuf::from(&name);

                // Build FileEvent.
                let timestamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);

                let file_event = FileEvent {
                    path: path.clone(),
                    event_type: event_type.clone(),
                    timestamp,
                };

                let event_str = event_type_string(&event_type);

                // Check each trigger config.
                for trigger in &self.triggers {
                    // Pattern match.
                    if !matches_pattern(&path, &trigger.watch_pattern) {
                        continue;
                    }

                    // Event-type filter.
                    if !trigger.events.iter().any(|e| e == event_str) {
                        continue;
                    }

                    // Per-file debounce.
                    if !self.debouncer.should_fire_file(&path) {
                        continue;
                    }

                    // Fire trigger.
                    let to = Duration::from_secs(trigger.timeout_secs);

                    if trigger.async_exec {
                        let cfg = trigger.clone();
                        let evt = file_event.clone();
                        tokio::spawn(async move {
                            execute_trigger(&cfg, &evt, to).await;
                        });
                    } else {
                        execute_trigger(trigger, &file_event, to).await;
                    }
                }
            }
        }
    }

    /// Stop the watcher.
    pub fn shutdown(&mut self) {
        // Inotify fd is closed when the engine is dropped.
        // This method is a placeholder for graceful shutdown signalling.
        eprintln!("[trigger-engine] shutdown requested");
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert inotify event mask to our EventType.
fn mask_to_event_type(mask: EventMask) -> Option<EventType> {
    if mask.contains(EventMask::CLOSE_WRITE) {
        Some(EventType::Write)
    } else if mask.contains(EventMask::DELETE) || mask.contains(EventMask::DELETE_SELF) {
        Some(EventType::Delete)
    } else if mask.contains(EventMask::CREATE) {
        Some(EventType::Create)
    } else {
        None
    }
}

/// String representation of EventType for matching against trigger config.
fn event_type_string(et: &EventType) -> &str {
    match et {
        EventType::Write => "write",
        EventType::Delete => "delete",
        EventType::Create => "create",
    }
}

/// Check if a file path matches a trigger's watch_pattern.
///
/// Simple glob:
///   "*"      matches any file
///   "*.go"   matches files ending in ".go"
///   "Makefile"  exact match
fn matches_pattern(path: &Path, pattern: &str) -> bool {
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");

    if pattern == "*" {
        return true;
    }

    if let Some(rest) = pattern.strip_prefix('*') {
        // Glob: *suffix — filename ends with suffix (minus the leading *).
        return filename.ends_with(rest);
    }

    // Exact filename match.
    filename == pattern
}

/// Execute a single trigger for a file event.
///
/// - Built-in triggers (parse-and-diff, upload-to-backend): log and return Ok (stub).
/// - Command triggers: run shell command with `{{ .FilePath }}` substitution.
///
/// Returns on timeout via `tokio::time::timeout`.
/// Errors are logged with `eprintln!`, never panic.
async fn execute_trigger(cfg: &TriggerConfig, event: &FileEvent, timeout_dur: Duration) {
    // Built-in triggers — stub for now.
    if let Some(builtin) = &cfg.builtin {
        eprintln!(
            "[trigger] builtin '{}' fired for '{}' ({})",
            builtin,
            event.path.display(),
            event_type_string(&event.event_type)
        );

        // Handle on_success / on_failure for builtin triggers.
        match builtin.as_str() {
            "parse-and-diff" | "upload-to-backend" => {
                eprintln!("[trigger] builtin '{}' completed (stub)", builtin);
            }
            _ => {
                eprintln!("[trigger] unknown builtin '{}'", builtin);
            }
        }
        return;
    }

    // Command triggers.
    if let Some(command) = &cfg.command {
        let file_path = event.path.display().to_string();
        let cmd = command.replace("{{ .FilePath }}", &file_path);

        eprintln!(
            "[trigger] '{}' executing: {}",
            cfg.name, cmd
        );

        match timeout(timeout_dur, async {
            tokio::process::Command::new("sh")
                .arg("-c")
                .arg(&cmd)
                .output()
                .await
        })
        .await
        {
            Ok(Ok(output)) => {
                if !output.status.success() {
                    eprintln!(
                        "[trigger] '{}' exited with status {}",
                        cfg.name,
                        output.status
                    );
                    if let Some(on_failure) = &cfg.on_failure {
                        log_trigger_action(on_failure, &event.path);
                    }
                } else {
                    if let Some(on_success) = &cfg.on_success {
                        log_trigger_action(on_success, &event.path);
                    }
                }
            }
            Ok(Err(e)) => {
                eprintln!(
                    "[trigger] '{}' command failed: {}",
                    cfg.name, e
                );
                if let Some(on_failure) = &cfg.on_failure {
                    log_trigger_action(on_failure, &event.path);
                }
            }
            Err(_) => {
                eprintln!(
                    "[trigger] '{}' timed out after {:?}",
                    cfg.name, timeout_dur
                );
                if let Some(on_failure) = &cfg.on_failure {
                    log_trigger_action(on_failure, &event.path);
                }
            }
        }
        return;
    }

    // No command or builtin configured.
    eprintln!(
        "[trigger] '{}' has no command or builtin — nothing to execute",
        cfg.name
    );
}

/// Log a TriggerAction (stub — real implementation would set xattrs, etc.).
fn log_trigger_action(action: &TriggerAction, path: &Path) {
    match action {
        TriggerAction::SetXattr { key, value_template } => {
            let value = value_template.replace("{{ .FilePath }}", &path.display().to_string());
            eprintln!("[trigger-action] setxattr {}={} on {}", key, value, path.display());
        }
        TriggerAction::Warn => {
            eprintln!("[trigger-action] warn for {}", path.display());
        }
        TriggerAction::Error => {
            eprintln!("[trigger-action] error for {}", path.display());
        }
    }
}

// Suppress unused-import warning for Instant (kept for API compatibility).
#[allow(dead_code)]
fn _instant_marker() -> Instant {
    Instant::now()
}

#[cfg(test)]
mod tests {
    use super::*;
    use inotify::EventMask;
    use std::path::Path;

    // ── mask_to_event_type ────────────────────────────────────────────

    #[test]
    fn test_mask_to_close_write_is_write() {
        assert_eq!(
            mask_to_event_type(EventMask::CLOSE_WRITE),
            Some(EventType::Write)
        );
    }

    #[test]
    fn test_mask_to_delete_is_delete() {
        assert_eq!(
            mask_to_event_type(EventMask::DELETE),
            Some(EventType::Delete)
        );
    }

    #[test]
    fn test_mask_to_delete_self_is_delete() {
        assert_eq!(
            mask_to_event_type(EventMask::DELETE_SELF),
            Some(EventType::Delete)
        );
    }

    #[test]
    fn test_mask_to_create_is_create() {
        assert_eq!(
            mask_to_event_type(EventMask::CREATE),
            Some(EventType::Create)
        );
    }

    #[test]
    fn test_mask_to_modify_is_none() {
        assert_eq!(mask_to_event_type(EventMask::MODIFY), None);
    }

    #[test]
    fn test_mask_to_empty_is_none() {
        assert_eq!(mask_to_event_type(EventMask::empty()), None);
    }

    // ── event_type_string ─────────────────────────────────────────────

    #[test]
    fn test_event_type_string_write() {
        assert_eq!(event_type_string(&EventType::Write), "write");
    }

    #[test]
    fn test_event_type_string_delete() {
        assert_eq!(event_type_string(&EventType::Delete), "delete");
    }

    #[test]
    fn test_event_type_string_create() {
        assert_eq!(event_type_string(&EventType::Create), "create");
    }

    // ── matches_pattern ───────────────────────────────────────────────

    #[test]
    fn test_matches_pattern_star_matches_anything() {
        assert!(matches_pattern(Path::new("foo.go"), "*"));
        assert!(matches_pattern(Path::new("bar.rs"), "*"));
        assert!(matches_pattern(Path::new("Makefile"), "*"));
    }

    #[test]
    fn test_matches_pattern_extension_glob() {
        assert!(matches_pattern(Path::new("main.go"), "*.go"));
        assert!(matches_pattern(Path::new("test.go"), "*.go"));
        assert!(!matches_pattern(Path::new("main.rs"), "*.go"));
        assert!(!matches_pattern(Path::new("Makefile"), "*.go"));
    }

    #[test]
    fn test_matches_pattern_exact() {
        assert!(matches_pattern(Path::new("Makefile"), "Makefile"));
        assert!(!matches_pattern(Path::new("makefile"), "Makefile"));
        assert!(!matches_pattern(Path::new("Makefile.old"), "Makefile"));
    }

    #[test]
    fn test_matches_pattern_no_match() {
        assert!(!matches_pattern(Path::new("foo.rs"), "*.py"));
        assert!(!matches_pattern(Path::new("bar"), "*.go"));
    }

    #[test]
    fn test_matches_pattern_directory_component() {
        // matches_pattern uses only the filename portion via file_name().
        assert!(matches_pattern(
            Path::new("src/subdir/main.go"),
            "*.go"
        ));
        assert!(!matches_pattern(
            Path::new("src/subdir/main.go"),
            "src/subdir/main.go"
        ));
    }

    // ── log_trigger_action ────────────────────────────────────────────

    #[test]
    fn test_log_trigger_action_setxattr() {
        // Should not panic — writes to stderr.
        log_trigger_action(
            &TriggerAction::SetXattr {
                key: "user.vfs.feature".into(),
                value_template: "{{ .FilePath }} was updated".into(),
            },
            Path::new("test.go"),
        );
    }

    #[test]
    fn test_log_trigger_action_warn() {
        log_trigger_action(&TriggerAction::Warn, Path::new("test.go"));
    }

    #[test]
    fn test_log_trigger_action_error() {
        log_trigger_action(&TriggerAction::Error, Path::new("test.go"));
    }

    // ── match-and-filter logic (unit-testable without running event loop)

    #[test]
    fn test_match_and_filter_write_event_passes() {
        let trigger = TriggerConfig {
            watch_pattern: "*.go".into(),
            events: vec!["write".into()],
            ..TriggerConfig::default()
        };
        let path = Path::new("main.go");
        let event_type = EventType::Write;

        // Replicate the match+filter logic from run().
        let pattern_match = matches_pattern(path, &trigger.watch_pattern);
        let event_match = trigger
            .events
            .iter()
            .any(|e| e == event_type_string(&event_type));

        assert!(pattern_match, "pattern should match *.go");
        assert!(event_match, "write event should pass filter");
    }

    #[test]
    fn test_match_and_filter_wrong_event_type_blocked() {
        let trigger = TriggerConfig {
            watch_pattern: "*.go".into(),
            events: vec!["delete".into()],
            ..TriggerConfig::default()
        };
        let path = Path::new("main.go");
        let event_type = EventType::Write;

        let pattern_match = matches_pattern(path, &trigger.watch_pattern);
        let event_match = trigger
            .events
            .iter()
            .any(|e| e == event_type_string(&event_type));

        assert!(pattern_match, "pattern should match *.go");
        assert!(!event_match, "write event should be blocked by delete-only filter");
    }

    #[test]
    fn test_match_and_filter_wrong_pattern_blocked() {
        let trigger = TriggerConfig {
            watch_pattern: "*.rs".into(),
            events: vec!["write".into()],
            ..TriggerConfig::default()
        };
        let path = Path::new("main.go");
        let event_type = EventType::Write;

        let pattern_match = matches_pattern(path, &trigger.watch_pattern);
        let event_match = trigger
            .events
            .iter()
            .any(|e| e == event_type_string(&event_type));

        assert!(!pattern_match, "pattern *.rs should not match main.go");
        assert!(event_match, "write event should pass if pattern matched");
    }
}
