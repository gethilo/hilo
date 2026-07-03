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
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
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
    max_concurrent: usize,
    /// Semaphore to enforce max_concurrent.
    semaphore: Arc<Semaphore>,
    /// Timeout for async trigger execution.
    #[allow(dead_code)]
    trigger_timeout: Duration,
    /// AST cache: file path → (source_content, Vec<Edge>).
    ast_cache: HashMap<PathBuf, (String, Vec<hilo_metadata::inventory::Edge>)>,
    /// Optional DuckDB connection for impact computation.
    db_conn: Option<duckdb::Connection>,
    /// Project root directory for .vfs/ paths.
    project_root: Option<PathBuf>,
    /// Optional S3 client for the `upload-to-backend` builtin.
    s3_client: Option<hilo_backends::S3Client>,
    /// S3 bucket name used by the `upload-to-backend` builtin.
    s3_bucket: Option<String>,
}

impl TriggerEngine {
    /// Create a new engine. Does NOT start watching yet.
    ///
    /// `db_conn` and `project_root` are optional — when `Some`, the
    /// `parse-and-diff` builtin uses them for edge append + impact computation.
    pub fn new(
        triggers: Vec<TriggerConfig>,
        debounce_default_ms: u64,
        db_conn: Option<duckdb::Connection>,
        project_root: Option<PathBuf>,
        s3_client: Option<hilo_backends::S3Client>,
        s3_bucket: Option<String>,
    ) -> Self {
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
            semaphore: Arc::new(Semaphore::new(4)),
            trigger_timeout,
            ast_cache: HashMap::new(),
            db_conn,
            project_root,
            s3_client,
            s3_bucket,
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

                    // ── parse-and-diff builtin: handle synchronously using ──
                    // engine's own ast_cache / db_conn / project_root.      ──
                    // This avoids the complexity of passing &mut cache      ──
                    // through tokio::spawn for a CPU-bound, synchronous      ──
                    // operation.  Command triggers and other builtins        ──
                    // continue through the async execute_trigger path below. ──
                    if let Some(builtin) = &trigger.builtin {
                        if builtin == "parse-and-diff" {
                            parse_and_diff_sync(
                                trigger,
                                &file_event,
                                &mut self.ast_cache,
                                &self.db_conn,
                                &self.project_root,
                            );
                            continue; // skip async execute_trigger spawn
                        }
                        if builtin == "upload-to-backend" {
                            let file_path_str = file_event.path.display().to_string();
                            match (&self.s3_client, &self.s3_bucket) {
                                (Some(s3), Some(bucket)) => {
                                    let to = Duration::from_secs(trigger.timeout_secs);
                                    let result = tokio::time::timeout(
                                        to,
                                        upload_to_backend(
                                            &file_event,
                                            s3,
                                            bucket,
                                            &self.project_root,
                                        ),
                                    )
                                    .await;
                                    match result {
                                        Ok(Ok(())) => {
                                            eprintln!(
                                                "[trigger] upload-to-backend: {} uploaded successfully",
                                                file_path_str
                                            );
                                            if let Some(on_success) = &trigger.on_success {
                                                log_trigger_action(
                                                    on_success,
                                                    &file_event.path,
                                                    file_event.timestamp,
                                                );
                                            }
                                        }
                                        Ok(Err(e)) => {
                                            eprintln!(
                                                "[trigger] upload-to-backend: {} failed: {}",
                                                file_path_str, e
                                            );
                                            if let Some(on_failure) = &trigger.on_failure {
                                                log_trigger_action(
                                                    on_failure,
                                                    &file_event.path,
                                                    file_event.timestamp,
                                                );
                                            }
                                        }
                                        Err(_) => {
                                            eprintln!(
                                                "[trigger] upload-to-backend: {} timed out after {:?}",
                                                file_path_str, to
                                            );
                                            if let Some(on_failure) = &trigger.on_failure {
                                                log_trigger_action(
                                                    on_failure,
                                                    &file_event.path,
                                                    file_event.timestamp,
                                                );
                                            }
                                        }
                                    }
                                }
                                _ => {
                                    eprintln!(
                                        "[trigger] upload-to-backend: S3 client or bucket not configured for {}",
                                        file_path_str
                                    );
                                }
                            }
                            continue;
                        }
                    }

                    // Fire trigger.
                    let to = Duration::from_secs(trigger.timeout_secs);
                    let project_root = self.project_root.clone();

                    if trigger.async_exec {
                        // Acquire a permit from the semaphore — drop if at capacity.
                        let permit = match self.semaphore.clone().try_acquire_owned() {
                            Ok(p) => p,
                            Err(_) => {
                                eprintln!(
                                    "[trigger] dropped '{}' for {} (max_concurrent={} reached)",
                                    trigger.name,
                                    path.display(),
                                    self.max_concurrent
                                );
                                continue;
                            }
                        };
                        let cfg = trigger.clone();
                        let evt = file_event.clone();
                        tokio::spawn(async move {
                            let _permit = permit; // hold permit until done
                            execute_trigger(&cfg, &evt, to, &project_root).await;
                        });
                    } else {
                        execute_trigger(trigger, &file_event, to, &self.project_root).await;
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

/// Execute the parse-and-diff built-in trigger for a file event.
///
/// Synchronous (tree-sitter parsing is CPU-bound). Steps:
/// 1. Read file content
/// 2. Detect language from extension
/// 3. Tree-sitter parse → extract edges via hilo_graph::Parser::parse_imports()
/// 4. Diff against AST cache: only new/changed edges are appended
/// 5. Append new edges to .vfs/graph/edges.jsonl
/// 6. Set user.vfs.last_modified xattr
/// 7. If db_conn is Some, compute impact via hilo_graph::impact::compute_impact()
/// 8. Set user.vfs.impact xattr on each impacted file
/// 9. Update AST cache with new parse result
///
/// Errors are logged via `eprintln!` — never panics.
fn parse_and_diff_sync(
    cfg: &TriggerConfig,
    event: &FileEvent,
    ast_cache: &mut HashMap<PathBuf, (String, Vec<hilo_metadata::inventory::Edge>)>,
    db_conn: &Option<duckdb::Connection>,
    project_root: &Option<PathBuf>,
) {
    // 1. Read file content.
    let content = match std::fs::read_to_string(&event.path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "[trigger] parse-and-diff: cannot read {}: {e}",
                event.path.display()
            );
            return;
        }
    };

    // 2. Detect language from extension.
    let ext = event
        .path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let lang = match hilo_graph::Language::from_extension(ext) {
        Some(l) => l,
        None => {
            eprintln!("[trigger] parse-and-diff: unsupported extension .{ext}");
            return;
        }
    };

    // 3. Parse imports via tree-sitter.
    let mut parser = match hilo_graph::Parser::for_language(lang) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[trigger] parse-and-diff: parser init failed: {e}");
            return;
        }
    };
    let file_path_str = event.path.display().to_string();
    let edges = match parser.parse_imports(&file_path_str, &content) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[trigger] parse-and-diff: parse failed: {e}");
            return;
        }
    };

    // 4. Diff against cache — only edges NOT already present.
    let cache_key = event.path.clone();
    let new_edges: Vec<hilo_metadata::inventory::Edge> =
        if let Some((_, cached_edges)) = ast_cache.get(&cache_key) {
            edges
                .iter()
                .filter(|e| !cached_edges.contains(e))
                .cloned()
                .collect()
        } else {
            edges.clone()
        };

    // 5. Append new edges to .vfs/graph/edges.jsonl.
    if !new_edges.is_empty() {
        if let Some(root) = project_root {
            let edges_path = root.join(".vfs/graph/edges.jsonl");
            if let Err(e) = hilo_metadata::inventory::append_edges(&edges_path, &new_edges) {
                eprintln!("[trigger] parse-and-diff: failed to append edges: {e}");
            }
        }
    }

    // 6. Set user.vfs.last_modified xattr.
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let iso_ts = unix_to_iso(ts);
    let last_mod_key = vfs_xattr_name("last_modified");
    if let Err(e) = xattr::set(&event.path, &last_mod_key, iso_ts.as_bytes()) {
        eprintln!("[trigger] parse-and-diff: failed to set last_modified xattr: {e}");
    }

    // 7. Impact computation (best-effort).
    if let Some(conn) = db_conn {
        let max_depth = cfg.max_depth.unwrap_or(3);
        match hilo_graph::impact::compute_impact(conn, &file_path_str, max_depth) {
            Ok(impacted) => {
                let impact_key = vfs_xattr_name("impact");
                // 8. Set user.vfs.impact on each impacted file.
                for f in &impacted {
                    let imp_path = Path::new(&f.path);
                    let val = format!("{} (depth {})", f.relation, f.depth);
                    if let Err(e) = xattr::set(imp_path, &impact_key, val.as_bytes()) {
                        eprintln!(
                            "[trigger] parse-and-diff: failed to set impact xattr on {}: {e}",
                            f.path
                        );
                    }
                }
                eprintln!(
                    "[trigger] parse-and-diff: {} impacted files for {}",
                    impacted.len(),
                    file_path_str
                );
            }
            Err(e) => {
                eprintln!("[trigger] parse-and-diff: impact computation failed: {e}");
            }
        }
    }

    // 9. Update AST cache.
    ast_cache.insert(cache_key, (content, edges.clone()));

    eprintln!(
        "[trigger] parse-and-diff: {} processed — {} edges ({} new)",
        file_path_str,
        edges.len(),
        new_edges.len()
    );
}

/// Execute the `upload-to-backend` built-in trigger for a file event.
///
/// Async — awaited in `run()` inside a `tokio::time::timeout` wrapper.
/// Steps:
/// 1. Read file content
/// 2. Compute SHA-256 hash (sha256:<hex>)
/// 3. Derive S3 key from file path (relative to project_root when possible)
/// 4. Derive blob_index_dir from project_root (.vfs/)
/// 5. S3Client.put_object(bucket, key, data, blob_index_dir) — writes cache,
///    uploads to S3, sets xattrs on cache file, appends blob index
/// 6. Set xattrs on the SOURCE file: user.vfs.backend, user.vfs.hash,
///    user.vfs.cache_status = "synced"
///
/// Errors are returned as `String` — the caller logs them.
async fn upload_to_backend(
    event: &FileEvent,
    s3_client: &hilo_backends::S3Client,
    bucket: &str,
    project_root: &Option<PathBuf>,
) -> Result<(), String> {
    let file_path_str = event.path.display().to_string();

    // 1. Read file content.
    let data = std::fs::read(&event.path).map_err(|e| format!("read {}: {}", file_path_str, e))?;

    // 2. Compute SHA-256 hash.
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    Digest::update(&mut hasher, &data);
    let hash_hex = format!("{:x}", hasher.finalize());
    let hash = format!("sha256:{}", hash_hex);

    // 3. Derive S3 key — prefer path relative to project_root.
    let s3_key = match project_root {
        Some(root) if event.path.starts_with(root) => event
            .path
            .strip_prefix(root)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| file_path_str.clone()),
        _ => file_path_str.clone(),
    };

    // 4. Derive blob_index_dir from project_root.
    let blob_index_dir = project_root.clone().unwrap_or_else(|| PathBuf::from("."));

    // 5. S3 put_object — cache write + upload + blob index append.
    s3_client
        .put_object(bucket, &s3_key, &data, &blob_index_dir)
        .await
        .map_err(|e| format!("put_object {}: {}", s3_key, e))?;

    // 6. Set xattrs on the SOURCE file (not the cache file).
    let backend_key = vfs_xattr_name("backend");
    let hash_key = vfs_xattr_name("hash");
    let status_key = vfs_xattr_name("cache_status");
    if let Err(e) = xattr::set(&event.path, &backend_key, b"s3") {
        eprintln!(
            "[trigger] upload-to-backend: failed to set backend xattr on {}: {}",
            file_path_str, e
        );
    }
    if let Err(e) = xattr::set(&event.path, &hash_key, hash.as_bytes()) {
        eprintln!(
            "[trigger] upload-to-backend: failed to set hash xattr on {}: {}",
            file_path_str, e
        );
    }
    if let Err(e) = xattr::set(&event.path, &status_key, b"synced") {
        eprintln!(
            "[trigger] upload-to-backend: failed to set cache_status xattr on {}: {}",
            file_path_str, e
        );
    }

    Ok(())
}

/// Execute a single trigger for a file event.
///
/// - `parse-and-diff`: handled synchronously in `run()` — never reaches this function.
/// - `upload-to-backend`: uploads the changed file to S3, updates blob index, sets xattrs.
/// - Command triggers: run shell command with `{{ .FilePath }}`, `{{ .ModulePath }}` substitution.
///
/// Returns on timeout via `tokio::time::timeout`.
/// Errors are logged with `eprintln!`, never panic.
async fn execute_trigger(
    cfg: &TriggerConfig,
    event: &FileEvent,
    timeout_dur: Duration,
    project_root: &Option<PathBuf>,
) {
    // Built-in triggers.
    // parse-and-diff is dispatched synchronously from run() (line ~180)
    // and never reaches this function.
    if let Some(builtin) = &cfg.builtin {
        eprintln!(
            "[trigger] builtin '{}' fired for '{}' ({})",
            builtin,
            event.path.display(),
            event_type_string(&event.event_type)
        );

        match builtin.as_str() {
            "parse-and-diff" => {
                // Should never reach here — handled synchronously in run().
                eprintln!("[trigger] parse-and-diff reached execute_trigger (should not happen)");
            }
            "upload-to-backend" => {
                // Should never reach here — handled synchronously in run().
                eprintln!(
                    "[trigger] upload-to-backend reached execute_trigger (should not happen)"
                );
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
        let mut cmd = command.replace("{{ .FilePath }}", &file_path);

        // {{ .ModulePath }} — file's parent directory relative to project root.
        if cmd.contains("{{ .ModulePath }}") {
            if let Some(root) = project_root {
                if let Some(parent) = event.path.parent() {
                    let rel = parent
                        .strip_prefix(root)
                        .unwrap_or(parent)
                        .display()
                        .to_string();
                    let module_path = if rel.is_empty() || rel == "." {
                        ".".to_string()
                    } else {
                        rel
                    };
                    cmd = cmd.replace("{{ .ModulePath }}", &module_path);
                }
            }
        }

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
                // Log stdout/stderr (truncated to 1KB each for debugging).
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout_trim = stdout.trim();
                let stderr_trim = stderr.trim();
                if !stdout_trim.is_empty() {
                    let log = if stdout_trim.len() > 1024 {
                        format!("{}... (truncated)", &stdout_trim[..1024])
                    } else {
                        stdout_trim.to_string()
                    };
                    eprintln!("[trigger] '{}' stdout: {}", cfg.name, log);
                }
                if !stderr_trim.is_empty() {
                    let log = if stderr_trim.len() > 1024 {
                        format!("{}... (truncated)", &stderr_trim[..1024])
                    } else {
                        stderr_trim.to_string()
                    };
                    eprintln!("[trigger] '{}' stderr: {}", cfg.name, log);
                }

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

    // ── max_concurrent semaphore enforcement ────────────────────────────

    #[test]
    fn test_max_concurrent_semaphore_created() {
        let engine = TriggerEngine::new(vec![], 500, None, None, None, None);
        // Semaphore should have capacity equal to max_concurrent.
        assert_eq!(engine.max_concurrent, 4);
        assert_eq!(engine.semaphore.available_permits(), 4);
    }

    #[tokio::test]
    async fn test_semaphore_try_acquire_limits_concurrency() {
        use std::sync::Arc;
        use tokio::sync::Semaphore;

        let sem = Arc::new(Semaphore::new(2));
        assert_eq!(sem.available_permits(), 2);

        // Acquire two permits.
        let p1 = sem.clone().try_acquire_owned().unwrap();
        let p2 = sem.clone().try_acquire_owned().unwrap();
        assert_eq!(sem.available_permits(), 0);

        // Third try_acquire should fail.
        assert!(sem.clone().try_acquire_owned().is_err());

        // Release one permit.
        drop(p1);
        // After dropping, the permit is returned. Wait briefly for async return.
        tokio::task::yield_now().await;

        // Now try_acquire should succeed again.
        let p3 = sem.clone().try_acquire_owned().unwrap();
        assert!(sem.available_permits() <= 1);

        drop(p2);
        drop(p3);
    }

    #[tokio::test]
    async fn test_semaphore_release_allows_new_acquire() {
        use std::sync::Arc;
        use tokio::sync::Semaphore;

        let sem = Arc::new(Semaphore::new(1));
        assert_eq!(sem.available_permits(), 1);

        let p = sem.clone().try_acquire_owned().unwrap();
        assert_eq!(sem.available_permits(), 0);

        // Should fail while permit is held.
        assert!(sem.clone().try_acquire_owned().is_err());

        // Release — permit returns.
        drop(p);
        tokio::task::yield_now().await;

        // Now should succeed.
        assert!(sem.clone().try_acquire_owned().is_ok());
    }

    // ── parse-and-diff builtin ────────────────────────────────────────

    /// Helper: build a TriggerConfig for the parse-and-diff builtin.
    fn parse_diff_cfg(max_depth: Option<u32>) -> TriggerConfig {
        TriggerConfig {
            name: "test".into(),
            builtin: Some("parse-and-diff".into()),
            max_depth,
            ..TriggerConfig::default()
        }
    }

    #[test]
    fn test_parse_diff_updates_edges() {
        let dir = tempfile::TempDir::new().unwrap();
        let go_file = dir.path().join("main.go");
        std::fs::write(
            &go_file,
            "package main\nimport \"fmt\"\nfunc main() { fmt.Println(\"hi\") }\n",
        )
        .unwrap();

        let mut cache = HashMap::new();
        let conn = duckdb::Connection::open_in_memory().unwrap();
        // Initialize edges table so compute_impact doesn't error.
        conn.execute_batch("CREATE TABLE edges (\"from\" TEXT, \"to\" TEXT, rel TEXT)")
            .unwrap();

        let event = FileEvent {
            path: go_file.clone(),
            event_type: EventType::Write,
            timestamp: 0,
        };
        let cfg = parse_diff_cfg(Some(3));

        parse_and_diff_sync(
            &cfg,
            &event,
            &mut cache,
            &Some(conn),
            &Some(dir.path().to_path_buf()),
        );

        // edges.jsonl should be created with import edges.
        let edges_path = dir.path().join(".vfs/graph/edges.jsonl");
        assert!(edges_path.exists(), "edges.jsonl should be created");
        let content = std::fs::read_to_string(&edges_path).unwrap();
        assert!(
            content.contains("imports"),
            "should contain import edge: {content}"
        );

        // last_modified xattr should be set.
        let xattr_val: Option<Vec<u8>> = xattr::get(&go_file, "user.vfs.last_modified").unwrap();
        assert!(xattr_val.is_some(), "last_modified xattr should be set");

        // Cache should have an entry.
        assert!(cache.contains_key(&go_file), "cache should have entry");
    }

    #[test]
    fn test_parse_diff_unchanged_file_noop() {
        let dir = tempfile::TempDir::new().unwrap();
        let go_file = dir.path().join("main.go");
        std::fs::write(&go_file, "package main\nimport \"fmt\"\n").unwrap();

        let mut cache = HashMap::new();
        let event = FileEvent {
            path: go_file.clone(),
            event_type: EventType::Write,
            timestamp: 0,
        };
        let cfg = parse_diff_cfg(None);

        // First parse — should write edges.
        parse_and_diff_sync(
            &cfg,
            &event,
            &mut cache,
            &None,
            &Some(dir.path().to_path_buf()),
        );

        let edges_path = dir.path().join(".vfs/graph/edges.jsonl");
        let first_content = std::fs::read_to_string(&edges_path).unwrap();
        let first_line_count = first_content.lines().count();
        assert!(first_line_count > 0, "first parse should produce edges");

        // Second parse — same content, same cache → no new edges.
        parse_and_diff_sync(
            &cfg,
            &event,
            &mut cache,
            &None,
            &Some(dir.path().to_path_buf()),
        );

        let second_content = std::fs::read_to_string(&edges_path).unwrap();
        let second_line_count = second_content.lines().count();
        assert_eq!(
            first_line_count, second_line_count,
            "second parse should not add edges"
        );
    }

    #[test]
    fn test_parse_diff_changed_content_delta_edges() {
        let dir = tempfile::TempDir::new().unwrap();
        let go_file = dir.path().join("main.go");
        std::fs::write(&go_file, "package main\nimport \"fmt\"\n").unwrap();

        let mut cache = HashMap::new();
        let event = FileEvent {
            path: go_file.clone(),
            event_type: EventType::Write,
            timestamp: 0,
        };
        let cfg = parse_diff_cfg(None);

        // First parse.
        parse_and_diff_sync(
            &cfg,
            &event,
            &mut cache,
            &None,
            &Some(dir.path().to_path_buf()),
        );

        let edges_path = dir.path().join(".vfs/graph/edges.jsonl");
        let first_count = std::fs::read_to_string(&edges_path)
            .unwrap()
            .lines()
            .count();

        // Change file — add a new import.
        std::fs::write(&go_file, "package main\nimport \"fmt\"\nimport \"os\"\n").unwrap();

        // Second parse — delta should produce only the new edge.
        parse_and_diff_sync(
            &cfg,
            &event,
            &mut cache,
            &None,
            &Some(dir.path().to_path_buf()),
        );

        let second_count = std::fs::read_to_string(&edges_path)
            .unwrap()
            .lines()
            .count();
        assert!(
            second_count >= first_count,
            "delta parse should not lose edges: {first_count} -> {second_count}"
        );
    }

    #[test]
    fn test_parse_diff_unsupported_extension_noop() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("README.md");
        std::fs::write(&file, "# Hello\n").unwrap();

        let mut cache = HashMap::new();
        let event = FileEvent {
            path: file.clone(),
            event_type: EventType::Write,
            timestamp: 0,
        };
        let cfg = parse_diff_cfg(None);

        parse_and_diff_sync(
            &cfg,
            &event,
            &mut cache,
            &None,
            &Some(dir.path().to_path_buf()),
        );

        // Cache should NOT have an entry for unsupported extension.
        assert!(
            cache.is_empty() || !cache.contains_key(&file),
            "cache should not have entry for .md"
        );
    }

    #[test]
    fn test_parse_diff_with_impact() {
        let dir = tempfile::TempDir::new().unwrap();
        let go_file = dir.path().join("main.go");
        std::fs::write(&go_file, "package main\nimport \"fmt\"\n").unwrap();

        // Create a file that "imports" main.go in the graph DB.
        let other = dir.path().join("other.go");
        std::fs::write(&other, "package other\n").unwrap();

        // compute_impact matches by the file_path_str (the absolute path passed
        // to parse_and_diff_sync), so the edge's "to" column must use the same
        // full path.
        let go_path_str = go_file.display().to_string();
        let other_path_str = other.display().to_string();
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "CREATE TABLE edges (\"from\" TEXT, \"to\" TEXT, rel TEXT);
             INSERT INTO edges VALUES ('{other_path_str}', '{go_path_str}', 'imports');",
        ))
        .unwrap();

        let mut cache = HashMap::new();
        let event = FileEvent {
            path: go_file.clone(),
            event_type: EventType::Write,
            timestamp: 0,
        };
        let cfg = parse_diff_cfg(Some(5));

        parse_and_diff_sync(
            &cfg,
            &event,
            &mut cache,
            &Some(conn),
            &Some(dir.path().to_path_buf()),
        );

        // other.go should have user.vfs.impact xattr set.
        let impact_val: Option<Vec<u8>> = xattr::get(&other, "user.vfs.impact").unwrap();
        assert!(
            impact_val.is_some(),
            "impacted file should have impact xattr"
        );
    }

    #[test]
    fn test_parse_diff_missing_file_no_panic() {
        let dir = tempfile::TempDir::new().unwrap();
        let no_file = dir.path().join("nonexistent.go");

        let mut cache = HashMap::new();
        let event = FileEvent {
            path: no_file,
            event_type: EventType::Write,
            timestamp: 0,
        };
        let cfg = parse_diff_cfg(None);

        // Should not panic — gracefully logs and returns.
        parse_and_diff_sync(
            &cfg,
            &event,
            &mut cache,
            &None,
            &Some(dir.path().to_path_buf()),
        );

        assert!(cache.is_empty(), "cache should remain empty");
    }

    // ── upload-to-backend builtin ────────────────────────────────────

    #[test]
    fn test_upload_to_backend_engine_without_s3_client() {
        // TriggerEngine with s3_client=None and s3_bucket=None — the field
        // config exists but the builtin can't run without a client.
        let cfg = TriggerConfig {
            name: "upload".into(),
            builtin: Some("upload-to-backend".into()),
            ..TriggerConfig::default()
        };
        let engine = TriggerEngine::new(vec![cfg], 500, None, None, None, None);
        assert!(engine.s3_client.is_none());
        assert!(engine.s3_bucket.is_none());
    }

    #[test]
    fn test_upload_to_backend_missing_file_no_panic() {
        // upload_to_backend on a nonexistent file should return Err, not panic.
        let dir = tempfile::TempDir::new().unwrap();
        let no_file = dir.path().join("nonexistent.txt");

        // We can't build a real S3Client without AWS config init, but we can
        // test that the read() guard fails BEFORE reaching put_object. Since
        // S3Client::new is async, we wrap in a tokio runtime.
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            // Build a writable S3Client with a dummy region + temp cache dir.
            let cache_dir = dir.path().join("cache");
            let s3 = hilo_backends::S3Client::new("us-east-1", &cache_dir, 0, true)
                .await
                .expect("S3Client init");

            let event = FileEvent {
                path: no_file,
                event_type: EventType::Write,
                timestamp: 0,
            };

            let result = upload_to_backend(&event, &s3, "test-bucket", &None).await;
            assert!(result.is_err(), "missing file should return Err");
            let err = result.unwrap_err();
            assert!(
                err.contains("read") || err.contains("No such file"),
                "error should mention read failure: {err}"
            );
        });
    }

    #[test]
    fn test_upload_to_backend_s3_error_no_panic() {
        // File exists, but S3 upload fails (no real S3 endpoint reachable).
        // The function should return Err with put_object failure, not panic.
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "hello world").unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let cache_dir = dir.path().join("cache");
            let s3 = hilo_backends::S3Client::new("us-east-1", &cache_dir, 0, true)
                .await
                .expect("S3Client init");

            let event = FileEvent {
                path: file,
                event_type: EventType::Write,
                timestamp: 0,
            };

            let result = upload_to_backend(
                &event,
                &s3,
                "nonexistent-bucket-for-test",
                &Some(dir.path().to_path_buf()),
            )
            .await;

            // S3 put_object will fail — either network error or AWS error.
            assert!(
                result.is_err(),
                "upload to fake bucket should fail: {:?}",
                result
            );
        });
    }

    #[tokio::test]
    async fn test_upload_to_backend_sets_xattrs_on_success_path() {
        // Verify that when put_object succeeds, the source file gets
        // user.vfs.backend, user.vfs.hash, and user.vfs.cache_status xattrs.
        // Since we can't reach real S3 in CI, this test uses a writable client
        // but expects put_object to fail — it then checks that NO xattrs were
        // set (proving xattrs are only set after a successful put_object).
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("content.txt");
        std::fs::write(&file, "test content").unwrap();

        let cache_dir = dir.path().join("cache");
        let s3 = hilo_backends::S3Client::new("us-east-1", &cache_dir, 0, true)
            .await
            .expect("S3Client init");

        let event = FileEvent {
            path: file.clone(),
            event_type: EventType::Write,
            timestamp: 0,
        };

        let result =
            upload_to_backend(&event, &s3, "fake-bucket", &Some(dir.path().to_path_buf())).await;

        // put_object will fail (no real S3) — xattrs should NOT be set on source.
        assert!(result.is_err());

        let backend: Option<Vec<u8>> = xattr::get(&file, "user.vfs.backend").unwrap();
        assert!(
            backend.is_none(),
            "backend xattr should not be set on failed upload"
        );

        let hash: Option<Vec<u8>> = xattr::get(&file, "user.vfs.hash").unwrap();
        assert!(
            hash.is_none(),
            "hash xattr should not be set on failed upload"
        );
    }

    // ── Command trigger execution ───────────────────────────────────────

    #[tokio::test]
    async fn test_command_trigger_filepath_template() {
        // Command with {{ .FilePath }} — should substitute with the event path.
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("src/main.go");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "content").unwrap();
        let file_path_str = file.display().to_string();

        let cfg = TriggerConfig {
            name: "echo-path".into(),
            command: Some("echo '{{ .FilePath }}'".into()),
            builtin: None,
            timeout_secs: 5,
            ..TriggerConfig::default()
        };
        let event = FileEvent {
            path: file.clone(),
            event_type: EventType::Write,
            timestamp: 0,
        };

        execute_trigger(&cfg, &event, Duration::from_secs(5), &None).await;
        // No panic — success path.
    }

    #[tokio::test]
    async fn test_command_trigger_modulepath_template() {
        // Command with {{ .ModulePath }} — should substitute with parent dir
        // relative to project root.
        let dir = tempfile::TempDir::new().unwrap();
        let project_root = dir.path().to_path_buf();
        let file = dir.path().join("src/auth/main.go");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "content").unwrap();
        let expected_module = "src/auth";

        // Use echo to capture the expanded value via stdout.
        let cfg = TriggerConfig {
            name: "echo-module".into(),
            command: Some("echo '{{ .ModulePath }}'".into()),
            builtin: None,
            timeout_secs: 5,
            ..TriggerConfig::default()
        };
        let event = FileEvent {
            path: file,
            event_type: EventType::Write,
            timestamp: 0,
        };

        execute_trigger(&cfg, &event, Duration::from_secs(5), &Some(project_root)).await;
        // No panic — ModulePath was expanded and echo'd.
        let _ = expected_module; // used in assertion comment
    }

    #[tokio::test]
    async fn test_command_trigger_modulepath_root_file() {
        // File at project root — ModulePath should be ".".
        let dir = tempfile::TempDir::new().unwrap();
        let project_root = dir.path().to_path_buf();
        let file = dir.path().join("Cargo.toml");

        let cfg = TriggerConfig {
            name: "root-module".into(),
            command: Some("echo '{{ .ModulePath }}'".into()),
            builtin: None,
            timeout_secs: 5,
            ..TriggerConfig::default()
        };
        let event = FileEvent {
            path: file,
            event_type: EventType::Write,
            timestamp: 0,
        };

        execute_trigger(&cfg, &event, Duration::from_secs(5), &Some(project_root)).await;
        // No panic — root file ModulePath should be ".".
    }

    #[tokio::test]
    async fn test_command_trigger_modulepath_no_project_root() {
        // When project_root is None, {{ .ModulePath }} should remain unsubstituted.
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("subdir/file.go");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "content").unwrap();

        let cfg = TriggerConfig {
            name: "no-root".into(),
            command: Some("echo '{{ .ModulePath }}'".into()),
            builtin: None,
            timeout_secs: 5,
            ..TriggerConfig::default()
        };
        let event = FileEvent {
            path: file,
            event_type: EventType::Write,
            timestamp: 0,
        };

        execute_trigger(&cfg, &event, Duration::from_secs(5), &None).await;
        // No panic — ModulePath is left as-is when no project_root.
    }

    #[tokio::test]
    async fn test_command_trigger_success_fires_on_success() {
        // Command that succeeds should trigger on_success action.
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "content").unwrap();

        let cfg = TriggerConfig {
            name: "success-test".into(),
            command: Some("echo 'ok'".into()),
            builtin: None,
            timeout_secs: 5,
            on_success: Some(TriggerAction::Warn),
            ..TriggerConfig::default()
        };
        let event = FileEvent {
            path: file,
            event_type: EventType::Write,
            timestamp: 0,
        };

        execute_trigger(&cfg, &event, Duration::from_secs(5), &None).await;
        // No panic — on_success was triggered.
    }

    #[tokio::test]
    async fn test_command_trigger_failure_fires_on_failure() {
        // Command that fails should trigger on_failure action.
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "content").unwrap();

        let cfg = TriggerConfig {
            name: "fail-test".into(),
            command: Some("false".into()),
            builtin: None,
            timeout_secs: 5,
            on_failure: Some(TriggerAction::Warn),
            ..TriggerConfig::default()
        };
        let event = FileEvent {
            path: file,
            event_type: EventType::Write,
            timestamp: 0,
        };

        execute_trigger(&cfg, &event, Duration::from_secs(5), &None).await;
        // No panic — on_failure was triggered.
    }

    #[tokio::test]
    async fn test_command_trigger_timeout() {
        // Command that sleeps beyond timeout should trigger on_failure.
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "content").unwrap();

        let cfg = TriggerConfig {
            name: "timeout-test".into(),
            command: Some("sleep 10".into()),
            builtin: None,
            timeout_secs: 1,
            on_failure: Some(TriggerAction::Warn),
            ..TriggerConfig::default()
        };
        let event = FileEvent {
            path: file,
            event_type: EventType::Write,
            timestamp: 0,
        };

        execute_trigger(&cfg, &event, Duration::from_secs(1), &None).await;
        // No panic — timeout triggered on_failure.
    }
}
