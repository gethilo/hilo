// Trigger engine — watches directories with inotify, debounces events,
// and fires trigger callbacks (builtin or shell command).
//
// Event flow:
//   inotify event -> mask_to_event_type -> pattern match -> debounce -> execute

use crate::{Debouncer, EventType, FileEvent, TriggerAction, TriggerConfig};
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
    #[allow(dead_code)]
    debounce_default_ms: u64,
    /// Max concurrent trigger executions.
    #[allow(dead_code)]
    max_concurrent: usize,
    /// Timeout for async trigger execution.
    #[allow(dead_code)]
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
            .map_err(|e| io::Error::other(e.to_string()))
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
            .map_err(|e| io::Error::other(e.to_string()))?;
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
                .map_err(|e| io::Error::other(e.to_string()))?;

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
    let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

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

        eprintln!("[trigger] '{}' executing: {}", cfg.name, cmd);

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
                        cfg.name, output.status
                    );
                    if let Some(on_failure) = &cfg.on_failure {
                        log_trigger_action(on_failure, &event.path, event.timestamp);
                    }
                } else {
                    if let Some(on_success) = &cfg.on_success {
                        log_trigger_action(on_success, &event.path, event.timestamp);
                    }
                }
            }
            Ok(Err(e)) => {
                eprintln!("[trigger] '{}' command failed: {}", cfg.name, e);
                if let Some(on_failure) = &cfg.on_failure {
                    log_trigger_action(on_failure, &event.path, event.timestamp);
                }
            }
            Err(_) => {
                eprintln!("[trigger] '{}' timed out after {:?}", cfg.name, timeout_dur);
                if let Some(on_failure) = &cfg.on_failure {
                    log_trigger_action(on_failure, &event.path, event.timestamp);
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

/// Execute a TriggerAction — set xattrs, log warnings, etc.
///
/// For `SetXattr`, actually calls `xattr::set()` with `user.vfs.` prefix.
/// Template variables `{{ .FilePath }}` and `{{ .Timestamp }}` are expanded.
/// Errors are logged via `eprintln!` — trigger actions are best-effort.
fn log_trigger_action(action: &TriggerAction, path: &Path, timestamp: u64) {
    match action {
        TriggerAction::SetXattr {
            key,
            value_template,
        } => {
            let mut value = value_template.replace("{{ .FilePath }}", &path.display().to_string());
            // Expand {{ .Timestamp }} — format as ISO 8601 with Z suffix.
            if value.contains("{{ .Timestamp }}") {
                // Convert Unix timestamp to ISO 8601 datetime string.
                let ts_str = unix_to_iso(timestamp);
                value = value.replace("{{ .Timestamp }}", &ts_str);
            }
            // Build full xattr name with user.vfs. prefix (idempotent).
            let full_key = vfs_xattr_name(key);
            match xattr::set(path, &full_key, value.as_bytes()) {
                Ok(()) => {
                    eprintln!(
                        "[trigger-action] setxattr {}={} on {}",
                        full_key,
                        value,
                        path.display()
                    );
                }
                Err(e) => {
                    eprintln!(
                        "[trigger-action] setxattr {} failed on {}: {}",
                        full_key,
                        path.display(),
                        e
                    );
                }
            }
        }
        TriggerAction::Warn => {
            eprintln!("[trigger-action] warn for {}", path.display());
        }
        TriggerAction::Error => {
            eprintln!("[trigger-action] error for {}", path.display());
        }
    }
}

/// Build the full xattr name: `user.vfs.<name>`.
/// Idempotent — stripping an existing `user.vfs.` prefix first.
fn vfs_xattr_name(name: &str) -> String {
    let stripped = name.strip_prefix("user.vfs.").unwrap_or(name);
    format!("user.vfs.{}", stripped)
}

/// Convert a Unix timestamp (u64 seconds) to an ISO 8601 string with `Z` suffix.
/// Returns "1970-01-01T00:00:00Z" for timestamp 0.
fn unix_to_iso(timestamp: u64) -> String {
    use std::time::{Duration, UNIX_EPOCH};
    let dt = UNIX_EPOCH + Duration::from_secs(timestamp);
    match dt.duration_since(UNIX_EPOCH) {
        Ok(d) => {
            let secs = d.as_secs();
            let days = secs / 86400;
            let remaining = secs % 86400;
            let hours = remaining / 3600;
            let minutes = (remaining % 3600) / 60;
            let secs_rem = remaining % 60;
            // Manually compute year/month/day from days since epoch for portability.
            let (year, month, day) = days_to_ymd(days as i64);
            format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
                year, month, day, hours, minutes, secs_rem
            )
        }
        Err(_) => "1970-01-01T00:00:00Z".to_string(),
    }
}

/// Convert days since Unix epoch to (year, month, day).
/// Algorithm: civil_from_days from Howard Hinnant's date library.
fn days_to_ymd(days: i64) -> (i64, u32, u32) {
    // Shift epoch from 1970-01-01 to 0000-03-01 (März).
    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = (z - era * 146097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m as u32, d as u32)
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
        assert!(matches_pattern(Path::new("src/subdir/main.go"), "*.go"));
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
            0,
        );
    }

    #[test]
    fn test_log_trigger_action_warn() {
        log_trigger_action(&TriggerAction::Warn, Path::new("test.go"), 0);
    }

    #[test]
    fn test_log_trigger_action_error() {
        log_trigger_action(&TriggerAction::Error, Path::new("test.go"), 0);
    }

    // ── log_trigger_action — actual xattr writes ───────────────────────

    #[test]
    fn test_setxattr_writes_xattr_to_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        // Write some content so the file exists.
        std::fs::write(path, "hello").unwrap();

        log_trigger_action(
            &TriggerAction::SetXattr {
                key: "feature".into(),
                value_template: "auth-module".into(),
            },
            path,
            0,
        );

        // Verify the xattr was set.
        let val: Option<Vec<u8>> = xattr::get(path, "user.vfs.feature").unwrap();
        assert_eq!(val, Some(b"auth-module".to_vec()));
    }

    #[test]
    fn test_setxattr_template_filepath() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        std::fs::write(path, "content").unwrap();

        log_trigger_action(
            &TriggerAction::SetXattr {
                key: "last_modified_by".into(),
                value_template: "File: {{ .FilePath }}".into(),
            },
            path,
            0,
        );

        let val: Option<Vec<u8>> = xattr::get(path, "user.vfs.last_modified_by").unwrap();
        assert!(val.is_some());
        let val_str = String::from_utf8(val.unwrap()).unwrap();
        assert!(
            val_str.contains("File:"),
            "should contain expanded path: {}",
            val_str
        );
    }

    #[test]
    fn test_setxattr_template_timestamp() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        std::fs::write(path, "content").unwrap();

        // Use a recent timestamp — should produce a 2026-* date.
        let known_ts: u64 = 1782787200;
        log_trigger_action(
            &TriggerAction::SetXattr {
                key: "last_tested".into(),
                value_template: "{{ .Timestamp }}".into(),
            },
            path,
            known_ts,
        );

        let val: Option<Vec<u8>> = xattr::get(path, "user.vfs.last_tested").unwrap();
        assert!(val.is_some());
        let val_str = String::from_utf8(val.unwrap()).unwrap();
        // Must be ISO 8601 format: YYYY-MM-DDTHH:MM:SSZ
        assert!(
            val_str.len() == 20 && val_str.ends_with('Z'),
            "expected ISO 8601 format (20 chars ending in Z), got: {}",
            val_str
        );
        // Verify ISO pattern with regex-free check.
        let chars: Vec<char> = val_str.chars().collect();
        assert!(chars[4] == '-', "expected YYYY-MM-DD, got: {}", val_str);
        assert!(chars[7] == '-', "expected YYYY-MM-DD, got: {}", val_str);
        assert!(chars[10] == 'T', "expected T separator, got: {}", val_str);
        assert!(chars[13] == ':', "expected HH:MM, got: {}", val_str);
        assert!(chars[16] == ':', "expected MM:SS, got: {}", val_str);
        // Year should be 2026+.
        let year: u32 = val_str[..4].parse().unwrap();
        assert!(year >= 2026, "expected year >= 2026, got: {}", year);
    }

    #[test]
    fn test_setxattr_prefix_idempotent() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        std::fs::write(path, "content").unwrap();

        // Passing key WITH user.vfs. prefix.
        log_trigger_action(
            &TriggerAction::SetXattr {
                key: "user.vfs.risk".into(),
                value_template: "critical-path".into(),
            },
            path,
            0,
        );

        // Must NOT be doubled to user.vfs.user.vfs.risk
        let val = xattr::get(path, "user.vfs.risk").unwrap();
        assert_eq!(val, Some(b"critical-path".to_vec()));

        // Doubled prefix should NOT exist.
        assert!(xattr::get(path, "user.vfs.user.vfs.risk")
            .unwrap()
            .is_none());
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
        assert!(
            !event_match,
            "write event should be blocked by delete-only filter"
        );
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
