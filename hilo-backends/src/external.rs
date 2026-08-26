//! ExternalToolDriver — adapts an existing sync CLI (rclone, s3sync, gdrive,
//! onedrive, dropbox) to the [`Backend`](crate::backend::Backend) trait (spec §6.1).
//!
//! Every external call runs with `Command::new(tool).args(...)` under a 30s
//! default timeout (configurable `HILO_TOOL_TIMEOUT_SECS`), stderr captured
//! into `BackendError::ToolFailed`. A missing binary → `BackendError::ToolMissing`
//! before any call.
//!
//! Command table (spec §6.1, exact for rclone/s3sync/gdrive):
//! | Tool | list | get | put | delete | stat |
//! |---|---|---|---|---|---|
//! | rclone | `rclone lsf --json {remote}:{path}/{prefix}` | `rclone copyto {remote}:{path}/{key} {dest}` | `rclone copyto {local} {remote}:{path}/{key}` | `rclone deletefile {remote}:{path}/{key}` | `rclone lsl {remote}:{path}/{key}` |
//! | s3sync | `s3sync list {bucket}/{prefix}` | `s3sync pull {bucket}/{key} {dest}` | `s3sync push {local} {bucket}/{key}` | `s3sync rm {bucket}/{key}` | `s3sync stat {bucket}/{key}` |
//! | gdrive | `gdrive files list --query "'{folder}' in parents"` | `gdrive files download {id} --dest {dest}` | `gdrive files upload {local} --parent {folder}` | `gdrive files delete {id}` | `gdrive files info {id}` |
//!
//! gdrive deviation (documented): v1 adds `--json` to `files list` — the plain
//! table output is not machine-parseable. The gdrive *key* is the file ID
//! (v1 simplification; folder id comes from `BackendConfig.remote`).
//! OneDrive/Dropbox CLIs follow the same pattern; exact flags are added when
//! the official CLIs are pinned (spec marks them "same pattern").

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::backend::{Backend, BackendConfig, BackendEntry, BackendError, BackendKind, SyncMode};
use crate::WriteResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyncTool {
    Rclone,
    S3Sync,
    GDriveCli,
    OneDriveCli,
    DropboxCli,
}

/// External tool driver (spec §6).
#[derive(Debug, Clone)]
pub struct ExternalToolDriver {
    tool: SyncTool,
    /// rclone: `"remote:path"`; s3sync: bucket; gdrive: folder id.
    remote: String,
    /// Sub-path/prefix appended to the remote.
    path: String,
    /// Stored per the spec'd struct shape; consumed by the sync planner (§7),
    /// not by the driver itself in v1.
    #[allow(dead_code)]
    mode: SyncMode,
}

impl ExternalToolDriver {
    pub fn new(cfg: &BackendConfig) -> Result<Self, BackendError> {
        let tool = match cfg.tool {
            crate::backend::SyncTool::Rclone => SyncTool::Rclone,
            crate::backend::SyncTool::S3Sync => SyncTool::S3Sync,
            crate::backend::SyncTool::GDriveCli => SyncTool::GDriveCli,
            crate::backend::SyncTool::OneDriveCli => SyncTool::OneDriveCli,
            crate::backend::SyncTool::DropboxCli => SyncTool::DropboxCli,
            crate::backend::SyncTool::Native => {
                return Err(BackendError::InvalidConfig(
                    "ExternalToolDriver requires an external tool (rclone/s3sync/gdrive/onedrive/dropbox)"
                        .into(),
                ))
            }
        };
        let bin = tool.binary();
        if find_on_path(bin).is_none() {
            return Err(BackendError::ToolMissing(bin.to_string()));
        }
        let remote = match tool {
            SyncTool::S3Sync => cfg.bucket.clone().ok_or_else(|| {
                BackendError::InvalidConfig("s3sync driver needs `bucket`".into())
            })?,
            SyncTool::GDriveCli => cfg.remote.clone().ok_or_else(|| {
                BackendError::InvalidConfig("gdrive driver needs `remote` (folder id)".into())
            })?,
            _ => cfg.remote.clone().ok_or_else(|| {
                BackendError::InvalidConfig(
                    "external driver needs `remote` (\"remote:path\")".into(),
                )
            })?,
        };
        Ok(Self {
            tool,
            remote,
            path: cfg.prefix.clone().unwrap_or_default(),
            mode: cfg.mode,
        })
    }

    /// Compose the `{remote}:{path}/{sub}` argument for rclone-style tools.
    fn remote_arg(&self, sub: &str) -> String {
        let base = if self.path.is_empty() {
            self.remote.clone()
        } else {
            format!(
                "{}/{}",
                self.remote.trim_end_matches('/'),
                self.path.trim_matches('/')
            )
        };
        if sub.is_empty() {
            base
        } else {
            format!("{}/{}", base, sub.trim_matches('/'))
        }
    }

    /// Compose the `{bucket}/{prefix}/{sub}` argument for s3sync.
    fn s3_arg(&self, sub: &str) -> String {
        self.remote_arg(sub)
    }

    /// Run one command with the tool timeout; capture stdout + stderr.
    fn run(&self, cmd: &mut Command) -> Result<String, BackendError> {
        let timeout = std::env::var("HILO_TOOL_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(30);
        let mut child = cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| {
                BackendError::ToolFailed(self.tool.binary().to_string(), None, e.to_string())
            })?;
        let start = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if start.elapsed() > Duration::from_secs(timeout) {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(BackendError::ToolFailed(
                            self.tool.binary().to_string(),
                            None,
                            format!("timed out after {timeout}s"),
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => {
                    return Err(BackendError::ToolFailed(
                        self.tool.binary().to_string(),
                        None,
                        e.to_string(),
                    ))
                }
            }
        };
        let output = child.wait_with_output().map_err(|e| {
            BackendError::ToolFailed(self.tool.binary().to_string(), None, e.to_string())
        })?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if !status.success() {
            return Err(BackendError::ToolFailed(
                self.tool.binary().to_string(),
                status.code(),
                stderr.trim().to_string(),
            ));
        }
        Ok(stdout)
    }

    fn list_rclone(&self, prefix: &str) -> Result<Vec<BackendEntry>, BackendError> {
        let out =
            self.run(Command::new("rclone").args(["lsf", "--json", &self.remote_arg(prefix)]))?;
        #[derive(serde::Deserialize)]
        struct RcloneEntry {
            #[serde(default, rename = "Path")]
            path: String,
            #[serde(default, rename = "Size")]
            size: i64,
            #[serde(default, rename = "IsDir")]
            is_dir: bool,
        }
        let entries: Vec<RcloneEntry> = serde_json::from_str(&out)
            .map_err(|e| BackendError::ToolFailed("rclone".into(), None, e.to_string()))?;
        Ok(entries
            .into_iter()
            .map(|e| BackendEntry {
                key: join_key(prefix, &e.path),
                size: e.size,
                modified: None,
                etag: None,
                is_dir: e.is_dir,
            })
            .collect())
    }

    fn list_s3sync(&self, prefix: &str) -> Result<Vec<BackendEntry>, BackendError> {
        let out = self.run(Command::new("s3sync").args(["list", &self.s3_arg(prefix)]))?;
        Ok(out
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                let key = l.split_whitespace().next().unwrap_or_default().to_string();
                BackendEntry {
                    key,
                    size: 0,
                    modified: None,
                    etag: None,
                    is_dir: false,
                }
            })
            .collect())
    }

    fn list_gdrive(&self, prefix: &str) -> Result<Vec<BackendEntry>, BackendError> {
        // --json deviation: the plain table output is not machine-parseable.
        let query = format!("'{}' in parents", self.remote);
        let out =
            self.run(Command::new("gdrive").args(["files", "list", "--json", "--query", &query]))?;
        #[derive(serde::Deserialize)]
        struct GdriveEntry {
            #[serde(default)]
            id: String,
            #[serde(default)]
            #[allow(dead_code)]
            name: String,
            #[serde(default)]
            size: Option<i64>,
            #[serde(default, rename = "mimeType")]
            mime_type: String,
        }
        let entries: Vec<GdriveEntry> = serde_json::from_str(&out)
            .map_err(|e| BackendError::ToolFailed("gdrive".into(), None, e.to_string()))?;
        Ok(entries
            .into_iter()
            .map(|e| BackendEntry {
                // v1: the key IS the gdrive file id (folder scoping via remote).
                key: join_key(prefix, &e.id),
                size: e.size.unwrap_or(0),
                modified: None,
                etag: None,
                is_dir: e.mime_type == "application/vnd.google-apps.folder",
            })
            .collect())
    }

    fn stat_rclone(&self, key: &str) -> Result<BackendEntry, BackendError> {
        // `rclone lsl` line: "<size> <date> <time> <key>"
        let out = self.run(Command::new("rclone").args(["lsl", &self.remote_arg(key)]))?;
        let line = out
            .lines()
            .next()
            .ok_or_else(|| BackendError::NotFound(key.to_string()))?;
        let mut parts = line.split_whitespace();
        let size = parts
            .next()
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        let date = parts.next().unwrap_or_default();
        let time = parts.next().unwrap_or_default();
        Ok(BackendEntry {
            key: key.to_string(),
            size,
            modified: parse_rclone_datetime(date, time),
            etag: None,
            is_dir: false,
        })
    }

    fn stat_s3sync(&self, key: &str) -> Result<BackendEntry, BackendError> {
        let out = self.run(Command::new("s3sync").args(["stat", &self.s3_arg(key)]))?;
        let line = out
            .lines()
            .next()
            .ok_or_else(|| BackendError::NotFound(key.to_string()))?;
        let mut parts = line.split_whitespace();
        let size = parts
            .next()
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        Ok(BackendEntry {
            key: key.to_string(),
            size,
            modified: None,
            etag: None,
            is_dir: false,
        })
    }

    fn stat_gdrive(&self, key: &str) -> Result<BackendEntry, BackendError> {
        let out = self.run(Command::new("gdrive").args(["files", "info", key]))?;
        // `gdrive files info` prints "Size: 123" / "Name: x" lines.
        let size = out
            .lines()
            .find_map(|l| l.strip_prefix("Size:"))
            .and_then(|s| s.trim().parse::<i64>().ok())
            .unwrap_or(0);
        Ok(BackendEntry {
            key: key.to_string(),
            size,
            modified: None,
            etag: None,
            is_dir: false,
        })
    }
}

fn join_key(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{}/{}", prefix.trim_end_matches('/'), name)
    }
}

/// Parse `2026-08-26` + `12:00:00.123` into unix seconds (civil-date algorithm).
fn parse_rclone_datetime(date: &str, time: &str) -> Option<i64> {
    let mut dp = date.split('-');
    let y: i64 = dp.next()?.parse().ok()?;
    let m: i64 = dp.next()?.parse().ok()?;
    let d: i64 = dp.next()?.parse().ok()?;
    let time = time.split('.').next().unwrap_or(time);
    let mut tp = time.split(':');
    let hh: i64 = tp.next()?.parse().ok()?;
    let mm: i64 = tp.next()?.parse().ok()?;
    let ss: i64 = tp.next()?.parse().ok()?;
    Some(civil_to_unix(y, m, d, hh, mm, ss))
}

/// Days-from-civil (Howard Hinnant) + seconds-of-day.
fn civil_to_unix(y: i64, m: i64, d: i64, hh: i64, mm: i64, ss: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    days * 86400 + hh * 3600 + mm * 60 + ss
}

/// PATH search without invoking the binary.
fn find_on_path(bin: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(bin);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

impl SyncTool {
    fn binary(&self) -> &'static str {
        match self {
            SyncTool::Rclone => "rclone",
            SyncTool::S3Sync => "s3sync",
            SyncTool::GDriveCli => "gdrive",
            SyncTool::OneDriveCli => "onedrive",
            SyncTool::DropboxCli => "dropbox",
        }
    }
}

impl Backend for ExternalToolDriver {
    fn kind(&self) -> BackendKind {
        BackendKind::External
    }

    fn name(&self) -> &str {
        "external"
    }

    fn list(&self, prefix: &str) -> Result<Vec<BackendEntry>, BackendError> {
        match self.tool {
            SyncTool::Rclone => self.list_rclone(prefix),
            SyncTool::S3Sync => self.list_s3sync(prefix),
            SyncTool::GDriveCli => self.list_gdrive(prefix),
            SyncTool::OneDriveCli | SyncTool::DropboxCli => Err(BackendError::InvalidConfig(
                "OneDrive/Dropbox CLI drivers are not implemented in v1 (same pattern as gdrive; exact flags pending official CLI pin)"
                    .into(),
            )),
        }
    }

    fn stat(&self, key: &str) -> Result<BackendEntry, BackendError> {
        match self.tool {
            SyncTool::Rclone => self.stat_rclone(key),
            SyncTool::S3Sync => self.stat_s3sync(key),
            SyncTool::GDriveCli => self.stat_gdrive(key),
            SyncTool::OneDriveCli | SyncTool::DropboxCli => Err(BackendError::InvalidConfig(
                "OneDrive/Dropbox CLI drivers are not implemented in v1".into(),
            )),
        }
    }

    fn get(&self, key: &str, dest: &Path) -> Result<(), BackendError> {
        match self.tool {
            SyncTool::Rclone => {
                self.run(
                    Command::new("rclone")
                        .args(["copyto", &self.remote_arg(key)])
                        .arg(dest),
                )?;
            }
            SyncTool::S3Sync => {
                self.run(
                    Command::new("s3sync")
                        .args(["pull", &self.s3_arg(key)])
                        .arg(dest),
                )?;
            }
            SyncTool::GDriveCli => {
                self.run(
                    Command::new("gdrive")
                        .args(["files", "download", key, "--dest"])
                        .arg(dest),
                )?;
            }
            SyncTool::OneDriveCli | SyncTool::DropboxCli => {
                return Err(BackendError::InvalidConfig(
                    "OneDrive/Dropbox CLI drivers are not implemented in v1".into(),
                ))
            }
        }
        Ok(())
    }

    fn put(&self, local: &Path, key: &str) -> Result<WriteResult, BackendError> {
        match self.tool {
            SyncTool::Rclone => {
                self.run(
                    Command::new("rclone")
                        .arg("copyto")
                        .arg(local)
                        .arg(self.remote_arg(key)),
                )?;
            }
            SyncTool::S3Sync => {
                self.run(
                    Command::new("s3sync")
                        .args(["push"])
                        .arg(local)
                        .arg(self.s3_arg(key)),
                )?;
            }
            SyncTool::GDriveCli => {
                self.run(
                    Command::new("gdrive")
                        .args(["files", "upload"])
                        .arg(local)
                        .args(["--parent", &self.remote]),
                )?;
            }
            SyncTool::OneDriveCli | SyncTool::DropboxCli => {
                return Err(BackendError::InvalidConfig(
                    "OneDrive/Dropbox CLI drivers are not implemented in v1".into(),
                ))
            }
        }
        Ok(WriteResult {
            cache_path: local.to_path_buf(),
            sha256: String::new(),
            etag: None,
        })
    }

    fn delete(&self, key: &str) -> Result<(), BackendError> {
        match self.tool {
            SyncTool::Rclone => {
                self.run(Command::new("rclone").args(["deletefile", &self.remote_arg(key)]))?;
            }
            SyncTool::S3Sync => {
                self.run(Command::new("s3sync").args(["rm", &self.s3_arg(key)]))?;
            }
            SyncTool::GDriveCli => {
                self.run(Command::new("gdrive").args(["files", "delete", key]))?;
            }
            SyncTool::OneDriveCli | SyncTool::DropboxCli => {
                return Err(BackendError::InvalidConfig(
                    "OneDrive/Dropbox CLI drivers are not implemented in v1".into(),
                ))
            }
        }
        Ok(())
    }

    fn walk(&self, prefix: &str) -> Result<Vec<BackendEntry>, BackendError> {
        match self.tool {
            SyncTool::Rclone => self.list_rclone(prefix),
            SyncTool::S3Sync => self.list_s3sync(prefix),
            SyncTool::GDriveCli => self.list_gdrive(prefix),
            SyncTool::OneDriveCli | SyncTool::DropboxCli => Err(BackendError::InvalidConfig(
                "OneDrive/Dropbox CLI drivers are not implemented in v1".into(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;

    /// Serializes PATH-mutating tests (Rust runs tests in parallel threads;
    /// a global PATH swap would race sibling tests).
    static PATH_LOCK: Mutex<()> = Mutex::new(());

    /// Install a fake tool shim in a temp bin dir. The shim derives its log
    /// (`<dir>/../argv.log`) and canned stdout (`<dir>/../stdout.txt`) paths
    /// from its own location, so no env vars are needed.
    fn install_shim(dir: &Path, name: &str, code: i32) -> std::path::PathBuf {
        let bin = dir.join(name);
        let script = format!(
            "#!/bin/bash\nBASE=\"$(dirname \"$0\")/..\"\nprintf '%s\\n' \"$*\" >> \"$BASE/argv.log\"\nif [ -f \"$BASE/stdout.txt\" ]; then printf '%s' \"$(cat \"$BASE/stdout.txt\")\"; fi\nexit {code}\n"
        );
        std::fs::write(&bin, script).unwrap();
        let mut perms = std::fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin, perms).unwrap();
        bin
    }

    fn driver_for(
        tool: crate::backend::SyncTool,
        remote: &str,
        prefix: Option<&str>,
    ) -> ExternalToolDriver {
        ExternalToolDriver {
            tool: match tool {
                crate::backend::SyncTool::Rclone => SyncTool::Rclone,
                crate::backend::SyncTool::S3Sync => SyncTool::S3Sync,
                crate::backend::SyncTool::GDriveCli => SyncTool::GDriveCli,
                other => panic!("unsupported test tool {other:?}"),
            },
            remote: remote.to_string(),
            path: prefix.unwrap_or("").to_string(),
            mode: SyncMode::Mirror,
        }
    }

    /// Run `f` with `bin_dir` prepended to PATH, then restore.
    fn run_with_path<T>(bin_dir: &Path, f: impl FnOnce() -> T) -> T {
        let _guard = PATH_LOCK.lock().unwrap();
        let old = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{}", bin_dir.display(), old));
        let r = f();
        std::env::set_var("PATH", &old);
        r
    }

    #[test]
    fn tool_missing_when_binary_absent() {
        let _guard = PATH_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let empty = tmp.path().join("empty-bin");
        std::fs::create_dir_all(&empty).unwrap();
        let cfg = BackendConfig {
            kind: BackendKind::External,
            name: "x".into(),
            remote: Some("myremote:docs".into()),
            tool: crate::backend::SyncTool::Rclone,
            ..Default::default()
        };
        let old_path = std::env::var("PATH").unwrap();
        std::env::set_var("PATH", &empty);
        let r = ExternalToolDriver::new(&cfg);
        std::env::set_var("PATH", &old_path);
        assert!(matches!(r, Err(BackendError::ToolMissing(_))), "{r:?}");
    }

    #[test]
    fn rclone_commands_are_exact_per_spec_table() {
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let log = tmp.path().join("argv.log");
        install_shim(&bin_dir, "rclone", 0);
        let d = driver_for(
            crate::backend::SyncTool::Rclone,
            "myremote:docs",
            Some("sub"),
        );
        let stdout_file = tmp.path().join("stdout.txt");

        // list
        std::fs::write(
            &stdout_file,
            r#"[{"Path":"a.txt","Name":"a.txt","Size":42,"IsDir":false},{"Path":"dir","Name":"dir","Size":0,"IsDir":true}]"#,
        )
        .unwrap();
        let entries = run_with_path(&bin_dir, || d.list("")).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].key, "a.txt");
        assert_eq!(entries[0].size, 42);
        assert!(entries[1].is_dir);
        // keys are relative to the backend prefix/path (spec §6 BackendEntry)
        assert_eq!(entries[1].key, "dir");
        let argv = std::fs::read_to_string(&log).unwrap();
        assert_eq!(argv.lines().next().unwrap(), "lsf --json myremote:docs/sub");
        std::fs::remove_file(&log).unwrap();

        // get
        let dst = tmp.path().join("out.bin");
        run_with_path(&bin_dir, || d.get("a.txt", &dst)).unwrap();
        let argv = std::fs::read_to_string(&log).unwrap();
        assert_eq!(
            argv.lines().next().unwrap(),
            format!("copyto myremote:docs/sub/a.txt {}", dst.display())
        );
        std::fs::remove_file(&log).unwrap();

        // put (local path is absolute; shim records it verbatim)
        let src = tmp.path().join("in.bin");
        std::fs::write(&src, b"x").unwrap();
        run_with_path(&bin_dir, || d.put(&src, "a.txt")).unwrap();
        let argv = std::fs::read_to_string(&log).unwrap();
        assert_eq!(
            argv.lines().next().unwrap(),
            format!("copyto {} myremote:docs/sub/a.txt", src.display())
        );
        std::fs::remove_file(&log).unwrap();

        // delete
        run_with_path(&bin_dir, || d.delete("a.txt")).unwrap();
        let argv = std::fs::read_to_string(&log).unwrap();
        assert_eq!(
            argv.lines().next().unwrap(),
            "deletefile myremote:docs/sub/a.txt"
        );
        std::fs::remove_file(&log).unwrap();

        // stat (lsl line: "<size> <date> <time> <key>")
        std::fs::write(&stdout_file, "123 2026-08-26 12:34:56.000000000 a.txt\n").unwrap();
        let st = run_with_path(&bin_dir, || d.stat("a.txt")).unwrap();
        let argv = std::fs::read_to_string(&log).unwrap();
        assert_eq!(argv.lines().next().unwrap(), "lsl myremote:docs/sub/a.txt");
        assert_eq!(st.size, 123);
        // 2026-08-26 12:34:56 UTC
        assert_eq!(st.modified, Some(1787_747_696));
    }

    #[test]
    fn s3sync_commands_are_exact_per_spec_table() {
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let log = tmp.path().join("argv.log");
        install_shim(&bin_dir, "s3sync", 0);
        let d = driver_for(
            crate::backend::SyncTool::S3Sync,
            "my-bucket",
            Some("workspace/"),
        );

        let dst = tmp.path().join("out.bin");
        run_with_path(&bin_dir, || d.get("a.txt", &dst)).unwrap();
        let argv = std::fs::read_to_string(&log).unwrap();
        assert_eq!(
            argv.lines().next().unwrap(),
            format!("pull my-bucket/workspace/a.txt {}", dst.display())
        );
        std::fs::remove_file(&log).unwrap();

        run_with_path(&bin_dir, || d.delete("a.txt")).unwrap();
        let argv = std::fs::read_to_string(&log).unwrap();
        assert_eq!(argv.lines().next().unwrap(), "rm my-bucket/workspace/a.txt");
    }

    #[test]
    fn gdrive_list_uses_folder_query_and_json_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let log = tmp.path().join("argv.log");
        install_shim(&bin_dir, "gdrive", 0);
        std::fs::write(
            tmp.path().join("stdout.txt"),
            r#"[{"id":"f1","name":"a.txt","size":7,"mimeType":"text/plain"},{"id":"f2","name":"d","size":0,"mimeType":"application/vnd.google-apps.folder"}]"#,
        )
        .unwrap();
        let d = driver_for(crate::backend::SyncTool::GDriveCli, "folder-42", None);

        let entries = run_with_path(&bin_dir, || d.list("")).unwrap();
        let argv = std::fs::read_to_string(&log).unwrap();
        assert_eq!(
            argv.lines().next().unwrap(),
            "files list --json --query 'folder-42' in parents"
        );
        assert_eq!(entries[0].key, "f1");
        assert_eq!(entries[0].size, 7);
        assert!(entries[1].is_dir);
    }

    #[test]
    fn tool_failed_captures_stderr_and_exit_code() {
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        install_shim(&bin_dir, "rclone", 3);
        std::fs::write(tmp.path().join("stdout.txt"), "").unwrap();
        let d = driver_for(crate::backend::SyncTool::Rclone, "myremote:docs", None);
        let err = run_with_path(&bin_dir, || d.stat("missing.txt")).unwrap_err();
        assert!(
            matches!(err, BackendError::ToolFailed(_, Some(3), _)),
            "{err}"
        );
    }

    #[test]
    fn civil_to_unix_matches_known_epoch() {
        // 1970-01-01 00:00:00 UTC
        assert_eq!(civil_to_unix(1970, 1, 1, 0, 0, 0), 0);
        // 2026-08-26 12:34:56 UTC = 1787747696
        assert_eq!(civil_to_unix(2026, 8, 26, 12, 34, 56), 1787_747_696);
    }
}
