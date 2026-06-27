//! `hilo mount <mount_point>` — mount a Hilo virtual filesystem via FUSE.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use hilo_fuse::{daemon, FuseConfig, Hilo};
use hilo_triggers::{TriggerConfig, TriggerEngine};

/// Mount a Hilo read-only FUSE filesystem.
///
/// Reads the current directory as the backing root, builds a `FuseConfig`
/// from the CLI arguments, and blocks in `daemon::mount` until unmounted.
///
/// When `--triggers` is set, a background `TriggerEngine` watches the mount
/// point for file events (inotify) and fires registered triggers.  The
/// engine runs in its own tokio runtime on a dedicated OS thread so it
/// does not interfere with the FUSE event loop.
///
/// On `SIGINT` / `SIGTERM` the mount is cleaned up via `daemon::unmount`.
pub fn run_mount(mount_point: &str, triggers: bool, allow_other: bool) -> Result<()> {
    let current_dir =
        std::env::current_dir().context("failed to determine the current directory")?;

    let trigger_handle = if triggers {
        let watch_dir = current_dir.clone();
        let mount_pt = mount_point.to_owned();
        Some(std::thread::spawn(move || {
            start_trigger_engine(&watch_dir, &mount_pt);
        }))
    } else {
        None
    };

    let config = FuseConfig {
        mount_point: PathBuf::from(mount_point),
        allow_other,
        direct_io: false,
        auto_unmount: true,
        attr_timeout: 1.0,
        entry_timeout: 1.0,
        max_read: 131_072,
        max_write: 131_072,
        sandbox: None,
    };

    let fs = Hilo::new(current_dir, config.clone());

    println!(
        "Hilo mounted at {}{}",
        mount_point,
        if triggers { " (triggers enabled)" } else { "" }
    );

    daemon::mount(fs, &config).context("FUSE mount failed")?;

    if let Some(handle) = trigger_handle {
        let _ = handle.join();
    }

    Ok(())
}

/// Start the trigger engine in a dedicated tokio runtime.
fn start_trigger_engine(watch_dir: &Path, mount_desc: &str) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("[trigger-engine] failed to create tokio runtime: {e}");
            return;
        }
    };

    rt.block_on(run_trigger_engine(watch_dir, mount_desc));
}

async fn run_trigger_engine(watch_dir: &Path, mount_desc: &str) {
    let watch_dir = watch_dir.to_path_buf();
    let triggers = load_triggers(&watch_dir);
    eprintln!(
        "[trigger-engine] loaded {} triggers for mount {}",
        triggers.len(),
        mount_desc
    );

    let trigger_count = triggers.len();
    let mut engine = TriggerEngine::new(triggers, 500);

    if let Err(e) = engine.watch_dir(&watch_dir) {
        eprintln!(
            "[trigger-engine] failed to watch {}: {e}",
            watch_dir.display()
        );
        return;
    }

    eprintln!(
        "[trigger-engine] watching {} — {} triggers active",
        watch_dir.display(),
        trigger_count
    );

    if let Err(e) = engine.run().await {
        eprintln!("[trigger-engine] event loop exited: {e}");
    }
}

/// Load trigger definitions from the workspace manifest or defaults.
fn load_triggers(project_dir: &Path) -> Vec<TriggerConfig> {
    for candidate in &[
        ".vfs/manifest.yaml",
        "manifest.yaml",
        ".vfs/manifest.yml",
        "manifest.yml",
    ] {
        let path = project_dir.join(candidate);
        if let Ok(contents) = std::fs::read_to_string(&path) {
            if let Some(configs) = parse_manifest_triggers(&contents) {
                return configs;
            }
        }
    }

    default_triggers()
}

/// Parse `triggers[]` from a manifest YAML document.
fn parse_manifest_triggers(yaml: &str) -> Option<Vec<TriggerConfig>> {
    let doc: serde_yaml::Value = serde_yaml::from_str(yaml).ok()?;
    let triggers_val = doc.get("triggers")?.as_sequence()?;

    let mut configs = Vec::with_capacity(triggers_val.len());
    for t in triggers_val {
        let name = t.get("name")?.as_str()?.to_owned();
        let watch_pattern = t
            .get("when")
            .and_then(|v| v.as_str())
            .unwrap_or("*")
            .to_owned();
        let events: Vec<String> = t
            .get("on")
            .and_then(|v| v.as_sequence())
            .map(|seq| {
                seq.iter()
                    .filter_map(|e| e.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_else(|| vec!["write".into()]);

        let run_value = t.get("run").and_then(|v| v.as_str());
        let builtin = run_value.filter(|s| *s == "parse-and-diff" || *s == "upload-to-backend");
        let command = if builtin.is_some() {
            None
        } else {
            run_value.map(String::from)
        };

        let async_exec = t.get("async").and_then(|v| v.as_bool()).unwrap_or(true);
        let timeout_secs =
            parse_trigger_timeout(t.get("timeout").and_then(|v| v.as_str()).unwrap_or("30s"));
        let debounce_ms = parse_debounce(
            t.get("debounce")
                .and_then(|v| v.as_str())
                .unwrap_or("500ms"),
        );

        configs.push(TriggerConfig {
            name,
            watch_pattern,
            events,
            command,
            builtin: builtin.map(String::from),
            async_exec,
            timeout_secs,
            debounce_ms,
            on_success: None,
            on_failure: None,
        });
    }

    Some(configs)
}

/// Parse a duration string like "5s" or "2m" to seconds.
fn parse_trigger_timeout(s: &str) -> u64 {
    if let Some(rest) = s.strip_suffix("ms") {
        rest.parse::<u64>().unwrap_or(0) / 1000
    } else if let Some(rest) = s.strip_suffix('s') {
        rest.parse::<u64>().unwrap_or(30)
    } else if let Some(rest) = s.strip_suffix('m') {
        rest.parse::<u64>().unwrap_or(1) * 60
    } else {
        s.parse::<u64>().unwrap_or(30)
    }
    .max(1)
}

/// Parse a debounce duration string like "500ms" or "2s" to milliseconds.
fn parse_debounce(s: &str) -> u64 {
    if let Some(rest) = s.strip_suffix("ms") {
        rest.parse::<u64>().unwrap_or(500)
    } else if let Some(rest) = s.strip_suffix('s') {
        rest.parse::<u64>().unwrap_or(1) * 1000
    } else {
        s.parse::<u64>().unwrap_or(500)
    }
    .max(1)
}

/// Sensible default triggers when no manifest is present.
fn default_triggers() -> Vec<TriggerConfig> {
    let source_extensions = &[
        "*.go", "*.rs", "*.py", "*.ts", "*.js", "*.java", "*.c", "*.cpp", "*.rb",
    ];
    source_extensions
        .iter()
        .map(|ext| TriggerConfig {
            name: format!("update-graph-{}", ext.trim_start_matches("*.")),
            watch_pattern: (*ext).to_string(),
            events: vec!["write".into()],
            command: None,
            builtin: Some("parse-and-diff".into()),
            async_exec: true,
            timeout_secs: 5,
            debounce_ms: 500,
            on_success: None,
            on_failure: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_triggers_covers_all_languages() {
        let configs = default_triggers();
        assert_eq!(configs.len(), 9, "one trigger per supported language");
        for cfg in &configs {
            assert!(cfg.watch_pattern.starts_with("*."));
            assert_eq!(cfg.builtin.as_deref(), Some("parse-and-diff"));
            assert!(cfg.async_exec);
        }
    }

    #[test]
    fn test_parse_manifest_triggers_empty_yaml() {
        assert!(parse_manifest_triggers("version: 2").is_none());
    }

    #[test]
    fn test_parse_manifest_triggers_no_triggers_key() {
        assert!(parse_manifest_triggers("project:\n  name: test").is_none());
    }

    #[test]
    fn test_parse_manifest_triggers_single() {
        let yaml = "\
triggers:
  - name: update-graph
    when: \"*\"
    on: [write, delete]
    run: parse-and-diff
    async: true
    timeout: 5s
";
        let configs = parse_manifest_triggers(yaml).unwrap();
        assert_eq!(configs.len(), 1);
        let c = &configs[0];
        assert_eq!(c.name, "update-graph");
        assert_eq!(c.watch_pattern, "*");
        assert_eq!(c.events, vec!["write", "delete"]);
        assert_eq!(c.builtin.as_deref(), Some("parse-and-diff"));
        assert!(c.async_exec);
        assert_eq!(c.timeout_secs, 5);
    }

    #[test]
    fn test_parse_manifest_triggers_command() {
        let yaml = "\
triggers:
  - name: lint-go
    when: \"*.go\"
    on: [write]
    run: \"golangci-lint run {{ .FilePath }}\"
    async: true
    timeout: 30s
    debounce: 2s
";
        let configs = parse_manifest_triggers(yaml).unwrap();
        assert_eq!(configs.len(), 1);
        let c = &configs[0];
        assert_eq!(c.name, "lint-go");
        assert_eq!(c.watch_pattern, "*.go");
        assert!(c.command.is_some());
        assert!(c.builtin.is_none());
        assert_eq!(c.debounce_ms, 2000);
    }

    #[test]
    fn test_parse_trigger_timeout() {
        assert_eq!(parse_trigger_timeout("5s"), 5);
        assert_eq!(parse_trigger_timeout("30s"), 30);
        assert_eq!(parse_trigger_timeout("500ms"), 1); // 500ms floor → 0s, clamped to 1
        assert_eq!(parse_trigger_timeout("1000ms"), 1);
        assert_eq!(parse_trigger_timeout("2m"), 120);
        assert_eq!(parse_trigger_timeout("0s"), 1);
    }

    #[test]
    fn test_parse_debounce() {
        assert_eq!(parse_debounce("500ms"), 500);
        assert_eq!(parse_debounce("2s"), 2000);
        assert_eq!(parse_debounce("1s"), 1000);
    }
}
