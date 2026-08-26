//! Backend abstraction — the adapter layer over storage backends (spec §6).
//!
//! Two roles live here:
//! - The [`Backend`] trait: a uniform key-relative API (list/stat/get/put/delete/walk)
//!   implemented by [`S3Driver`](crate::s3::S3Driver), [`ExternalToolDriver`](crate::external::ExternalToolDriver)
//!   and [`LocalDriver`] (reference impl + test double).
//! - [`BackendRegistry`]: name-keyed registry with `from_config` construction and
//!   `load_mounts` parsing of `.vfs/backends/mounts.yaml`.
//!
//! Note on naming: the crate root already exports the legacy `Backend` *enum*
//! (virtual-path resolution). The trait lives at `backend::Backend` to avoid the
//! collision; the registry and structs are re-exported at the crate root.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::WriteResult;

/// Storage backend kinds (spec §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    #[default]
    Local,
    S3,
    GDrive,
    OneDrive,
    Dropbox,
    External,
}

/// Sync modes (spec §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SyncMode {
    Stream,
    #[default]
    Mirror,
}

/// Sync tool used to reach a backend (spec §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SyncTool {
    #[default]
    Native,
    Rclone,
    S3Sync,
    GDriveCli,
    OneDriveCli,
    DropboxCli,
}

/// One entry from a backend listing (spec §6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendEntry {
    /// Remote key, relative to the backend prefix/path.
    pub key: String,
    pub size: i64,
    /// Unix seconds.
    pub modified: Option<i64>,
    pub etag: Option<String>,
    pub is_dir: bool,
}

/// Static configuration for one backend (spec §6 + §11.3).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BackendConfig {
    pub kind: BackendKind,
    /// Registry key + mounts.yaml name.
    pub name: String,
    pub bucket: Option<String>,
    /// S3 key prefix.
    pub prefix: Option<String>,
    pub region: Option<String>,
    /// External tool remote (`"remote:path"`) or gdrive folder id.
    pub remote: Option<String>,
    #[serde(default)]
    pub tool: SyncTool,
    #[serde(default)]
    pub mode: SyncMode,
    pub ignore_file: Option<PathBuf>,
    #[serde(default = "default_poll_secs")]
    pub poll_secs: u64,
    #[serde(default)]
    pub no_default_ignores: bool,
}

fn default_poll_secs() -> u64 {
    60
}

/// Backend errors — exact catalog from spec §12.
#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("key not found: {0}")]
    NotFound(String),
    #[error("bucket operation failed: {0}")]
    BucketError(String),
    #[error("backend is read-only")]
    ReadOnly,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("aws sdk error: {0}")]
    Aws(String),
    #[error("required tool not found on PATH: {0}")]
    ToolMissing(String),
    #[error("tool failed: {0} (exit {1:?}): {2}")]
    ToolFailed(String, Option<i32>, String),
    #[error("backend unreachable: {0}")]
    Unreachable(String),
    #[error("invalid config: {0}")]
    InvalidConfig(String),
}

impl From<crate::S3Error> for BackendError {
    fn from(e: crate::S3Error) -> Self {
        match e {
            crate::S3Error::NotFound(k) => BackendError::NotFound(k),
            crate::S3Error::ReadOnly => BackendError::ReadOnly,
            other => BackendError::Aws(other.to_string()),
        }
    }
}

/// Uniform key-relative backend API (spec §6). Keys are relative to the
/// backend root and must be safe (no absolute paths, no `..`).
pub trait Backend: Send + Sync {
    fn kind(&self) -> BackendKind;
    fn name(&self) -> &str;

    /// Non-recursive listing of a prefix. Keys are relative to the backend root.
    fn list(&self, prefix: &str) -> Result<Vec<BackendEntry>, BackendError>;

    fn stat(&self, key: &str) -> Result<BackendEntry, BackendError>;

    /// Download `key` bytes to `dest` (a full file path; parent exists).
    fn get(&self, key: &str, dest: &Path) -> Result<(), BackendError>;

    /// Upload a local file to `key`. Returns the existing WriteResult shape.
    fn put(&self, local: &Path, key: &str) -> Result<WriteResult, BackendError>;

    fn delete(&self, key: &str) -> Result<(), BackendError>;

    /// Recursive listing (list + descend); used by plan_sync and stream mount.
    fn walk(&self, prefix: &str) -> Result<Vec<BackendEntry>, BackendError>;
}

/// Reject unsafe keys: empty, absolute, or containing `..` traversal.
pub(crate) fn check_key(key: &str) -> Result<(), BackendError> {
    if key.is_empty() {
        return Err(BackendError::InvalidConfig("empty key".into()));
    }
    let p = Path::new(key);
    if p.is_absolute() {
        return Err(BackendError::InvalidConfig(format!(
            "absolute key not allowed: {key}"
        )));
    }
    for comp in p.components() {
        if matches!(
            comp,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            return Err(BackendError::InvalidConfig(format!("unsafe key: {key}")));
        }
    }
    Ok(())
}

/// Reference backend implementation + test double (spec §6): the key is a
/// path relative to `root`. Round-trips through the real filesystem so the
/// contract suite exercises real I/O without any network.
#[derive(Debug, Clone)]
pub struct LocalDriver {
    root: PathBuf,
    /// Stored per the spec'd struct shape; consumed by the sync planner (§7),
    /// not by the driver itself in v1.
    #[allow(dead_code)]
    mode: SyncMode,
}

impl LocalDriver {
    pub fn new(root: PathBuf, mode: SyncMode) -> Self {
        Self { root, mode }
    }

    fn resolve(&self, key: &str) -> Result<PathBuf, BackendError> {
        check_key(key)?;
        Ok(self.root.join(key))
    }

    fn sha256_of(path: &Path) -> Result<String, BackendError> {
        use sha2::{Digest, Sha256};
        let bytes = std::fs::read(path)?;
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        Ok(format!("sha256:{}", hex(&hasher.finalize())))
    }
}

impl Backend for LocalDriver {
    fn kind(&self) -> BackendKind {
        BackendKind::Local
    }

    fn name(&self) -> &str {
        "local"
    }

    fn list(&self, prefix: &str) -> Result<Vec<BackendEntry>, BackendError> {
        let dir = if prefix.is_empty() {
            self.root.clone()
        } else {
            check_key(prefix)?;
            self.root.join(prefix)
        };
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let meta = entry.metadata()?;
            let key = entry
                .path()
                .strip_prefix(&self.root)
                .map_err(|e| BackendError::InvalidConfig(e.to_string()))?
                .to_string_lossy()
                .replace('\\', "/");
            out.push(BackendEntry {
                key,
                size: meta.len() as i64,
                modified: meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64),
                etag: None,
                is_dir: meta.is_dir(),
            });
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }

    fn stat(&self, key: &str) -> Result<BackendEntry, BackendError> {
        let path = self.resolve(key)?;
        let meta = std::fs::metadata(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                BackendError::NotFound(key.to_string())
            } else {
                BackendError::Io(e)
            }
        })?;
        Ok(BackendEntry {
            key: key.to_string(),
            size: meta.len() as i64,
            modified: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64),
            etag: None,
            is_dir: meta.is_dir(),
        })
    }

    fn get(&self, key: &str, dest: &Path) -> Result<(), BackendError> {
        let path = self.resolve(key)?;
        std::fs::copy(&path, dest).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                BackendError::NotFound(key.to_string())
            } else {
                BackendError::Io(e)
            }
        })?;
        Ok(())
    }

    fn put(&self, local: &Path, key: &str) -> Result<WriteResult, BackendError> {
        let path = self.resolve(key)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(local, &path)?;
        Ok(WriteResult {
            cache_path: path,
            sha256: Self::sha256_of(local)?,
            etag: None,
        })
    }

    fn delete(&self, key: &str) -> Result<(), BackendError> {
        let path = self.resolve(key)?;
        std::fs::remove_file(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                BackendError::NotFound(key.to_string())
            } else {
                BackendError::Io(e)
            }
        })
    }

    fn walk(&self, prefix: &str) -> Result<Vec<BackendEntry>, BackendError> {
        let base = if prefix.is_empty() {
            self.root.clone()
        } else {
            check_key(prefix)?;
            self.root.join(prefix)
        };
        let mut out = Vec::new();
        if !base.exists() {
            return Ok(out);
        }
        let mut stack = vec![base];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir)? {
                let entry = entry?;
                let meta = entry.metadata()?;
                let key = entry
                    .path()
                    .strip_prefix(&self.root)
                    .map_err(|e| BackendError::InvalidConfig(e.to_string()))?
                    .to_string_lossy()
                    .replace('\\', "/");
                if meta.is_dir() {
                    out.push(BackendEntry {
                        key,
                        size: meta.len() as i64,
                        modified: None,
                        etag: None,
                        is_dir: true,
                    });
                    stack.push(entry.path());
                } else {
                    out.push(BackendEntry {
                        key,
                        size: meta.len() as i64,
                        modified: meta
                            .modified()
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_secs() as i64),
                        etag: None,
                        is_dir: false,
                    });
                }
            }
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }
}

/// Name-keyed registry of backends (spec §6).
#[derive(Default)]
pub struct BackendRegistry {
    backends: HashMap<String, Arc<dyn Backend>>,
}

impl std::fmt::Debug for BackendRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackendRegistry")
            .field("names", &self.backends.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl BackendRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, name: String, b: Arc<dyn Backend>) {
        self.backends.insert(name, b);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Backend>> {
        self.backends.get(name).cloned()
    }

    /// All registered mount names, sorted for deterministic output.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.backends.keys().cloned().collect();
        names.sort();
        names
    }

    /// Construct a backend from a config. `tool: auto` resolves to the native
    /// driver for S3 and to rclone for every external kind (spec §11.3).
    pub fn from_config(cfg: &BackendConfig) -> Result<Arc<dyn Backend>, BackendError> {
        let tool = match cfg.tool {
            SyncTool::Native => match cfg.kind {
                BackendKind::S3 => SyncTool::Native,
                BackendKind::Local => SyncTool::Native,
                _ => SyncTool::Rclone,
            },
            other => other,
        };
        let backend: Arc<dyn Backend> = match (cfg.kind, tool) {
            (BackendKind::S3, SyncTool::Native) => Arc::new(crate::s3::S3Driver::new(cfg)?),
            (BackendKind::Local, SyncTool::Native) => {
                let root = cfg.prefix.as_ref().map(PathBuf::from).ok_or_else(|| {
                    BackendError::InvalidConfig("LocalDriver needs prefix=root".into())
                })?;
                Arc::new(LocalDriver::new(root, cfg.mode))
            }
            (
                _,
                SyncTool::Rclone
                | SyncTool::S3Sync
                | SyncTool::GDriveCli
                | SyncTool::OneDriveCli
                | SyncTool::DropboxCli,
            ) => {
                // The resolved `tool` may differ from cfg.tool (Native on a
                // non-S3 kind falls back to Rclone); the driver validates its
                // own tool, so hand it the RESOLVED tool, not the raw config.
                let mut resolved = cfg.clone();
                resolved.tool = tool;
                Arc::new(crate::external::ExternalToolDriver::new(&resolved)?)
            }
            (kind, tool) => {
                return Err(BackendError::InvalidConfig(format!(
                    "no driver for kind {kind:?} + tool {tool:?}"
                )))
            }
        };
        Ok(backend)
    }

    /// Parse `.vfs/backends/mounts.yaml` (spec §11.3) and register every entry.
    /// Entries missing the new keys get spec defaults (`tool: native` implied
    /// for s3, `mode: mirror`, `poll_secs: 60`).
    pub fn load_mounts(mounts_yaml: &Path) -> Result<Self, BackendError> {
        let text = std::fs::read_to_string(mounts_yaml).map_err(|e| {
            BackendError::InvalidConfig(format!("cannot read {}: {e}", mounts_yaml.display()))
        })?;
        let entries: Vec<MountEntry> = serde_yaml::from_str(&text)
            .map_err(|e| BackendError::InvalidConfig(format!("bad mounts.yaml: {e}")))?;
        let mut reg = Self::new();
        for entry in entries {
            let kind = BackendKind::from_str(entry.kind.as_str()).ok_or_else(|| {
                BackendError::InvalidConfig(format!("unknown backend type '{}'", entry.kind))
            })?;
            let tool = match entry.tool.as_deref() {
                None | Some("auto") => SyncTool::Native,
                Some("native") => SyncTool::Native,
                Some("rclone") => SyncTool::Rclone,
                Some("s3sync") => SyncTool::S3Sync,
                Some("gdrive") => SyncTool::GDriveCli,
                Some("onedrive") => SyncTool::OneDriveCli,
                Some("dropbox") => SyncTool::DropboxCli,
                Some(other) => {
                    return Err(BackendError::InvalidConfig(format!(
                        "unknown tool '{other}'"
                    )));
                }
            };
            let cfg = BackendConfig {
                kind,
                name: entry.name.clone(),
                bucket: entry.bucket,
                prefix: entry.prefix,
                region: entry.region,
                remote: entry.remote,
                tool,
                mode: match entry.mode.as_deref() {
                    None | Some("mirror") => SyncMode::Mirror,
                    Some("stream") => SyncMode::Stream,
                    Some(other) => {
                        return Err(BackendError::InvalidConfig(format!(
                            "unknown mode '{other}'"
                        )));
                    }
                },
                ignore_file: entry.ignore_file.map(PathBuf::from),
                poll_secs: entry.poll_secs.unwrap_or(60),
                no_default_ignores: entry.no_default_ignores.unwrap_or(false),
            };
            let backend = Self::from_config(&cfg)?;
            reg.register(entry.name, backend);
        }
        Ok(reg)
    }
}

/// YAML shape of one `.vfs/backends/mounts.yaml` entry (spec §11.3).
///
/// Public so the CLI (`hilo backend mount`) can append entries; `at` is the
/// mount point (informational in v1 — the loader does not need it).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountEntry {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bucket: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignore_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poll_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_default_ignores: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
}

impl BackendKind {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "s3" => Some(BackendKind::S3),
            "gdrive" => Some(BackendKind::GDrive),
            "onedrive" => Some(BackendKind::OneDrive),
            "dropbox" => Some(BackendKind::Dropbox),
            "external" => Some(BackendKind::External),
            "local" => Some(BackendKind::Local),
            _ => None,
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In-memory backend (spec §15 test double).
    #[derive(Debug, Default)]
    pub(crate) struct MockBackend {
        pub objects: std::sync::RwLock<HashMap<String, Vec<u8>>>,
    }

    impl Backend for MockBackend {
        fn kind(&self) -> BackendKind {
            BackendKind::External
        }
        fn name(&self) -> &str {
            "mock"
        }
        fn list(&self, prefix: &str) -> Result<Vec<BackendEntry>, BackendError> {
            let objects = self.objects.read().unwrap();
            let mut out: Vec<BackendEntry> = objects
                .keys()
                .filter(|k| k.starts_with(prefix))
                .map(|k| BackendEntry {
                    key: k.clone(),
                    size: objects[k].len() as i64,
                    modified: None,
                    etag: None,
                    is_dir: false,
                })
                .collect();
            out.sort_by(|a, b| a.key.cmp(&b.key));
            Ok(out)
        }
        fn stat(&self, key: &str) -> Result<BackendEntry, BackendError> {
            let objects = self.objects.read().unwrap();
            let data = objects
                .get(key)
                .ok_or_else(|| BackendError::NotFound(key.to_string()))?;
            Ok(BackendEntry {
                key: key.to_string(),
                size: data.len() as i64,
                modified: None,
                etag: None,
                is_dir: false,
            })
        }
        fn get(&self, key: &str, dest: &Path) -> Result<(), BackendError> {
            let objects = self.objects.read().unwrap();
            let data = objects
                .get(key)
                .ok_or_else(|| BackendError::NotFound(key.to_string()))?;
            std::fs::write(dest, data)?;
            Ok(())
        }
        fn put(&self, local: &Path, key: &str) -> Result<WriteResult, BackendError> {
            let data = std::fs::read(local)?;
            self.objects.write().unwrap().insert(key.to_string(), data);
            Ok(WriteResult {
                cache_path: local.to_path_buf(),
                sha256: String::new(),
                etag: None,
            })
        }
        fn delete(&self, key: &str) -> Result<(), BackendError> {
            if self.objects.write().unwrap().remove(key).is_none() {
                return Err(BackendError::NotFound(key.to_string()));
            }
            Ok(())
        }
        fn walk(&self, prefix: &str) -> Result<Vec<BackendEntry>, BackendError> {
            self.list(prefix)
        }
    }

    /// The identical contract suite both MockBackend and LocalDriver must pass.
    pub(crate) fn contract_suite(b: &dyn Backend, tmp: &tempfile::TempDir) {
        // put + stat + get round-trip
        let src = tmp.path().join("src.bin");
        std::fs::write(&src, b"hello backend").unwrap();
        b.put(&src, "a/b.txt").unwrap();
        let st = b.stat("a/b.txt").unwrap();
        assert_eq!(st.key, "a/b.txt");
        assert_eq!(st.size, 13);
        assert!(!st.is_dir);
        let dst = tmp.path().join("dst.bin");
        b.get("a/b.txt", &dst).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"hello backend");

        // list filters by prefix and returns relative keys
        b.put(&src, "a/c.txt").unwrap();
        b.put(&src, "other.txt").unwrap();
        let listed = b.list("a/").unwrap();
        let keys: Vec<&str> = listed.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(keys, vec!["a/b.txt", "a/c.txt"]);

        // walk descends
        let walked = b.walk("").unwrap();
        let walked_keys: Vec<&str> = walked.iter().map(|e| e.key.as_str()).collect();
        for k in ["a/b.txt", "a/c.txt", "other.txt"] {
            assert!(
                walked_keys.contains(&k),
                "walk missing {k}: {walked_keys:?}"
            );
        }

        // delete + stat NotFound
        b.delete("other.txt").unwrap();
        assert!(matches!(
            b.stat("other.txt"),
            Err(BackendError::NotFound(_))
        ));
        assert!(matches!(
            b.get("other.txt", &dst),
            Err(BackendError::NotFound(_))
        ));

        // unicode / emoji keys round-trip (spec §13.4)
        let emoji = "dir/🦀-café.txt";
        b.put(&src, emoji).unwrap();
        let st = b.stat(emoji).unwrap();
        assert_eq!(st.size, 13);
        let dst2 = tmp.path().join("dst2.bin");
        b.get(emoji, &dst2).unwrap();
        assert_eq!(std::fs::read(&dst2).unwrap(), b"hello backend");
        b.delete(emoji).unwrap();
        assert!(matches!(b.stat(emoji), Err(BackendError::NotFound(_))));
    }

    #[test]
    fn mock_backend_passes_contract() {
        let tmp = tempfile::tempdir().unwrap();
        let b = MockBackend::default();
        contract_suite(&b, &tmp);
    }

    #[test]
    fn local_driver_passes_contract() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let b = LocalDriver::new(root, SyncMode::Mirror);
        contract_suite(&b, &tmp);
    }

    #[test]
    fn local_driver_put_returns_sha256_and_writes_real_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let b = LocalDriver::new(root.clone(), SyncMode::Mirror);
        let src = tmp.path().join("payload");
        std::fs::write(&src, b"abc").unwrap();
        let wr = b.put(&src, "x/y.txt").unwrap();
        assert!(wr.sha256.starts_with("sha256:"));
        assert!(root.join("x/y.txt").exists());
        assert_eq!(std::fs::read(root.join("x/y.txt")).unwrap(), b"abc");
    }

    #[test]
    fn local_driver_rejects_unsafe_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let b = LocalDriver::new(root, SyncMode::Mirror);
        for bad in ["../escape", "/abs", "a/../../b", ""] {
            assert!(
                matches!(b.stat(bad), Err(BackendError::InvalidConfig(_))),
                "key {bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn registry_register_get_from_config_local() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let mut reg = BackendRegistry::new();
        let cfg = BackendConfig {
            kind: BackendKind::Local,
            name: "local1".into(),
            prefix: Some(root.display().to_string()),
            ..Default::default()
        };
        let b = BackendRegistry::from_config(&cfg).unwrap();
        reg.register("local1".into(), b);
        assert!(reg.get("local1").is_some());
        assert!(reg.get("nope").is_none());
        // from_config rejects traversal-unsafe LocalDriver configs only at use time;
        // S3 without a bucket is an invalid config
        let bad = BackendConfig {
            kind: BackendKind::S3,
            name: "s3x".into(),
            bucket: None,
            ..Default::default()
        };
        assert!(BackendRegistry::from_config(&bad).is_err());
    }

    #[test]
    fn from_config_resolves_native_to_rclone_for_external_kinds() {
        // A gdrive/onedrive/dropbox/external kind with tool: auto (→ Native)
        // must fall back to the rclone driver (spec §9 auto resolution), which
        // surfaces ToolMissing when rclone is not installed — NOT the
        // InvalidConfig "requires an external tool" error the raw config
        // would produce in ExternalToolDriver.
        let cfg = BackendConfig {
            kind: BackendKind::GDrive,
            name: "gdrive-auto".into(),
            remote: Some("test:path".into()),
            tool: SyncTool::Native,
            ..Default::default()
        };
        let result = BackendRegistry::from_config(&cfg);
        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("expected ToolMissing, got Ok"),
        };
        assert!(
            matches!(err, BackendError::ToolMissing(_)),
            "expected ToolMissing, got {err:?}"
        );
    }

    #[test]
    fn mount_entry_round_trips_with_at() {
        let entry = MountEntry {
            name: "prod-bucket".into(),
            kind: "s3".into(),
            bucket: Some("my-bucket".into()),
            prefix: Some("workspace/".into()),
            region: None,
            remote: None,
            tool: Some("native".into()),
            mode: Some("mirror".into()),
            ignore_file: Some(".hiloignore".into()),
            poll_secs: Some(60),
            no_default_ignores: Some(false),
            at: Some("/mnt/vfs/ws".into()),
        };
        let yaml = serde_yaml::to_string(&entry).unwrap();
        assert!(yaml.contains("type: s3"), "yaml: {yaml}");
        assert!(yaml.contains("at: /mnt/vfs/ws"), "yaml: {yaml}");
        let back: MountEntry = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(back.name, "prod-bucket");
        assert_eq!(back.at.as_deref(), Some("/mnt/vfs/ws"));
        assert_eq!(back.tool.as_deref(), Some("native"));
    }

    #[test]
    fn registry_names_lists_registration_order() {
        let mut reg = BackendRegistry::new();
        assert!(reg.names().is_empty());
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        for n in ["a", "b", "c"] {
            let cfg = BackendConfig {
                kind: BackendKind::Local,
                name: n.into(),
                prefix: Some(root.display().to_string()),
                ..Default::default()
            };
            reg.register(n.into(), BackendRegistry::from_config(&cfg).unwrap());
        }
        assert_eq!(reg.names(), vec!["a", "b", "c"]);
    }

    #[test]
    fn load_mounts_parses_yaml_with_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("local-root");
        std::fs::create_dir_all(&root).unwrap();
        let yaml = format!(
            "- name: prod-bucket\n  type: s3\n  bucket: my-bucket\n  prefix: workspace/\n  at: /mnt/vfs/ws\n  tool: native\n  mode: mirror\n  ignore_file: .hiloignore\n  poll_secs: 60\n  no_default_ignores: false\n- name: local-1\n  type: local\n  prefix: {}\n",
            root.display()
        );
        let f = tmp.path().join("mounts.yaml");
        std::fs::write(&f, yaml).unwrap();
        let reg = BackendRegistry::load_mounts(&f).unwrap();
        assert!(reg.get("prod-bucket").is_some());
        let local = reg.get("local-1").expect("local-1 registered");
        assert_eq!(local.kind(), BackendKind::Local);
        // put through the registry-loaded local driver
        let src = tmp.path().join("p.txt");
        std::fs::write(&src, b"data").unwrap();
        local.put(&src, "p.txt").unwrap();
        assert!(root.join("p.txt").exists());
    }

    #[test]
    fn load_mounts_rejects_bad_yaml() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("mounts.yaml");
        std::fs::write(&f, "- name: x\n  type: s3\n  tool: notatool\n").unwrap();
        let err = BackendRegistry::load_mounts(&f).unwrap_err();
        assert!(matches!(err, BackendError::InvalidConfig(_)), "{err}");
    }
}
