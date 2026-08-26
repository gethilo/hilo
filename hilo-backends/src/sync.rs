//! Two-way sync between a local workspace directory and an S3 prefix.
//!
//! Implements the "upstream ignore" pattern: a git-ignore-style `.vfsignore`
//! file keeps build artifacts, binaries, and caches local-only. The same
//! ignore format is shared by all remote backends (S3 today; Google Drive,
//! OneDrive, Dropbox in the future) — see docs/ignore-file.md.
//!
//! Sync semantics (last-writer-wins, no deletes):
//! - local-only non-ignored files are uploaded
//! - remote-only non-ignored objects are downloaded
//! - files on both sides: the side with the newer mtime/last-modified wins;
//!   equal timestamps are left unchanged
//! - `.vfs/` metadata is NEVER transferred in either direction
//! - ignored paths are NEVER transferred in either direction

use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use tokio::fs;

use crate::s3::{S3Client, S3Result};

/// One parsed ignore pattern.
struct IgnorePattern {
    regex: Regex,
    negated: bool,
    /// The raw line the pattern was parsed from (for reporting).
    source: String,
}

/// A git-ignore-style matcher ("upstream ignore").
///
/// Supported syntax (subset of gitignore):
/// - blank lines and `#` comments are skipped
/// - `!` prefix re-includes a path (last matching pattern wins)
/// - trailing `/` marks a directory-only pattern
/// - leading `/` anchors the pattern to the workspace root
/// - a pattern containing a `/` (after anchoring) is root-relative;
///   otherwise it matches the basename at any depth
/// - `*` matches any run of characters except `/`, `?` matches one
/// - `**` matches across directory boundaries
///
/// A pattern that matches a directory also matches everything below it,
/// mirroring gitignore's "cannot re-include a file if a parent directory
/// is excluded" rule.
#[derive(Default)]
pub struct IgnoreMatcher {
    patterns: Vec<IgnorePattern>,
}

impl IgnoreMatcher {
    /// An empty matcher that ignores nothing.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Parse git-ignore-style text. Invalid/comment/blank lines are skipped.
    pub fn parse(text: &str) -> Self {
        let mut patterns = Vec::new();
        for line in text.lines() {
            if let Some(p) = parse_line(line) {
                patterns.push(p);
            }
        }
        Self { patterns }
    }

    /// Load patterns from a file. A missing file yields an empty matcher.
    pub fn from_file(path: &Path) -> std::io::Result<Self> {
        if !path.exists() {
            return Ok(Self::empty());
        }
        let text = std::fs::read_to_string(path)?;
        Ok(Self::parse(&text))
    }

    /// Whether `rel_path` (POSIX-style, relative to the workspace root)
    /// is ignored. The last matching pattern wins.
    ///
    /// Mirrors the gitignore rule "it is not possible to re-include a file
    /// if a parent directory of that file is excluded": a path whose any
    /// ancestor directory is ignored stays ignored.
    pub fn is_ignored(&self, rel_path: &str) -> bool {
        self.decision(rel_path).ignored
    }

    /// Full ignore decision for `rel_path`: whether it is excluded, and the
    /// raw ignore-file line responsible (the last matching pattern, or the
    /// ancestor-directory pattern that excludes the subtree). `rule` is
    /// `None` when no pattern matches.
    ///
    /// A negated (`!`) pattern that is the last match reports as not ignored
    /// with its rule still shown; an excluded ancestor directory wins over
    /// any re-inclusion below it (gitignore rule).
    pub fn decision(&self, rel_path: &str) -> IgnoreDecision {
        let direct = self.last_match_rule(rel_path);
        if let Some(p) = direct {
            if !p.negated {
                return IgnoreDecision {
                    ignored: true,
                    rule: Some(p.source.clone()),
                };
            }
        }
        // Any ignored ancestor directory excludes the whole subtree.
        let mut idx = 0;
        while let Some(slash) = rel_path[idx..].find('/') {
            idx += slash + 1;
            if let Some(p) = self.last_match_rule(&rel_path[..idx - 1]) {
                if !p.negated {
                    return IgnoreDecision {
                        ignored: true,
                        rule: Some(p.source.clone()),
                    };
                }
            }
        }
        // Not ignored: report the last matching rule (e.g. a negation) if any.
        IgnoreDecision {
            ignored: false,
            rule: direct.map(|p| p.source.clone()),
        }
    }

    /// The last pattern matching `rel_path`, if any.
    fn last_match_rule(&self, rel_path: &str) -> Option<&IgnorePattern> {
        let mut last: Option<&IgnorePattern> = None;
        for p in &self.patterns {
            if p.regex.is_match(rel_path) {
                last = Some(p);
            }
        }
        last
    }
}

/// Result of an ignore lookup: whether the path is excluded and which rule
/// (raw ignore-file line) decided it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IgnoreDecision {
    pub ignored: bool,
    /// Raw source line of the deciding pattern; `None` when nothing matches.
    pub rule: Option<String>,
}

/// Translate a gitignore glob body (no leading `/`, no trailing `/`) into a
/// regex fragment matching path segments.
fn translate_glob(pattern: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' => {
                if i + 1 < chars.len() && chars[i + 1] == '*' {
                    // `**`
                    if i + 2 < chars.len() && chars[i + 2] == '/' {
                        out.push_str("(?:.*/)?");
                        i += 3;
                    } else {
                        out.push_str(".*");
                        i += 2;
                    }
                } else {
                    out.push_str("[^/]*");
                    i += 1;
                }
            }
            '?' => {
                out.push_str("[^/]");
                i += 1;
            }
            c => {
                out.push_str(&regex::escape(&c.to_string()));
                i += 1;
            }
        }
    }
    out
}

/// Parse a single gitignore-style line into a pattern, if meaningful.
fn parse_line(line: &str) -> Option<IgnorePattern> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let mut body = trimmed;
    let mut negated = false;
    if let Some(rest) = body.strip_prefix('!') {
        negated = true;
        body = rest;
    }
    // trailing `/` = directory-only (parity with gitignore; the regex below
    // matches dirs and everything below them either way)
    if let Some(rest) = body.strip_suffix('/') {
        body = rest;
    }
    if body.is_empty() {
        return None;
    }
    let anchored = body.starts_with('/');
    if anchored {
        body = &body[1..];
    }
    let has_slash = body.contains('/');

    let glob = translate_glob(body);
    let mut re = String::new();
    if anchored || has_slash {
        re.push('^');
        re.push_str(&glob);
    } else {
        // basename match at any depth
        re.push_str("(?:^|/)");
        re.push_str(&glob);
    }
    // A matching directory excludes its whole subtree (gitignore rule).
    re.push_str("(?:/.*)?$");

    Some(IgnorePattern {
        regex: Regex::new(&re).expect("translated glob is a valid regex"),
        negated,
        source: trimmed.to_string(),
    })
}

/// A local file discovered during the workspace walk.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalFile {
    /// POSIX-style path relative to the workspace root.
    pub rel_path: String,
    pub size: u64,
    /// mtime as UNIX epoch seconds.
    pub mtime_unix: u64,
}

/// A remote object listed from S3.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteObject {
    /// POSIX-style path relative to the sync prefix.
    pub rel_path: String,
    pub size: i64,
    /// LastModified as UNIX epoch seconds (0 when unknown).
    pub last_modified_unix: u64,
}

/// Result of comparing local vs remote state.
#[derive(Debug, Default)]
pub struct SyncPlan {
    pub uploads: Vec<LocalFile>,
    pub downloads: Vec<RemoteObject>,
    pub unchanged: usize,
    pub ignored_local: usize,
    pub ignored_remote: usize,
}

/// Two-way sync engine between a local directory and an S3 bucket prefix.
pub struct SyncEngine {
    client: S3Client,
    bucket: String,
    prefix: String,
    local_dir: PathBuf,
    ignore: IgnoreMatcher,
}

/// Whether a relative path is part of Hilo's own metadata or an ignore
/// definition and must never sync. `.hiloignore`/`.hiloephemeral` are
/// always local-only (spec: backend-backed-workspace-spec.md §13.13);
/// `.vfsignore` is accepted as a legacy alias name.
fn is_never_synced(rel_path: &str) -> bool {
    rel_path == ".vfs"
        || rel_path.starts_with(".vfs/")
        || rel_path == ".hiloignore"
        || rel_path == ".hiloephemeral"
        || rel_path == ".vfsignore"
}

impl SyncEngine {
    pub fn new(
        client: S3Client,
        bucket: String,
        prefix: String,
        local_dir: PathBuf,
        ignore: IgnoreMatcher,
    ) -> Self {
        Self {
            client,
            bucket,
            prefix: prefix.trim_matches('/').to_string(),
            local_dir,
            ignore,
        }
    }

    /// Build the transfer plan for a local file set and a remote object set.
    /// Ignores are enforced here (defense in depth: callers may pre-filter
    /// for efficiency, but the plan is the source of truth).
    pub fn build_plan(&self, local: &[LocalFile], remote: &[RemoteObject]) -> SyncPlan {
        let mut plan = SyncPlan::default();
        let remote_map: HashMap<&str, &RemoteObject> =
            remote.iter().map(|r| (r.rel_path.as_str(), r)).collect();
        let local_set: HashSet<&str> = local.iter().map(|l| l.rel_path.as_str()).collect();

        for lf in local {
            if is_never_synced(&lf.rel_path) || self.ignore.is_ignored(&lf.rel_path) {
                plan.ignored_local += 1;
                continue;
            }
            match remote_map.get(lf.rel_path.as_str()) {
                None => plan.uploads.push(lf.clone()),
                Some(ro) => {
                    if lf.mtime_unix > ro.last_modified_unix {
                        plan.uploads.push(lf.clone());
                    } else if ro.last_modified_unix > lf.mtime_unix {
                        plan.downloads.push((*ro).clone());
                    } else {
                        plan.unchanged += 1;
                    }
                }
            }
        }

        for ro in remote {
            if is_never_synced(&ro.rel_path) || self.ignore.is_ignored(&ro.rel_path) {
                plan.ignored_remote += 1;
                continue;
            }
            if !local_set.contains(ro.rel_path.as_str()) {
                plan.downloads.push(ro.clone());
            }
        }

        plan
    }

    /// Compute the plan without transferring anything.
    pub async fn plan(&self) -> S3Result<SyncPlan> {
        let local = self.walk_local();
        let remote = self.list_remote().await?;
        Ok(self.build_plan(&local, &remote))
    }

    /// Compute the plan and execute all transfers.
    ///
    /// After every transfer the local file's mtime is aligned to the remote
    /// object's LastModified so a repeated sync is a no-op (without this,
    /// the side that just transferred would always look newer and the two
    /// sides would ping-pong forever).
    pub async fn sync(&self) -> S3Result<SyncPlan> {
        let plan = self.plan().await?;

        for lf in &plan.uploads {
            let src = self.local_dir.join(&lf.rel_path);
            let data = fs::read(&src).await?;
            self.client
                .upload_bytes(&self.bucket, &self.remote_key(&lf.rel_path), &data)
                .await?;
            // Align local mtime to the remote's LastModified.
            let key = self.remote_key(&lf.rel_path);
            if let Some(lm) = self
                .client
                .head_object_last_modified(&self.bucket, &key)
                .await?
            {
                let _ = set_mtime(&src, lm);
            }
        }

        for ro in &plan.downloads {
            let dest = self.local_dir.join(&ro.rel_path);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).await?;
            }
            self.client
                .download_to(&self.bucket, &self.remote_key(&ro.rel_path), &dest)
                .await?;
            if ro.last_modified_unix > 0 {
                let _ = set_mtime(&dest, ro.last_modified_unix);
            }
        }

        Ok(plan)
    }

    /// The full S3 key for a relative path.
    fn remote_key(&self, rel_path: &str) -> String {
        if self.prefix.is_empty() {
            rel_path.to_string()
        } else {
            format!("{}/{}", self.prefix, rel_path)
        }
    }

    /// A relative path from an S3 key under the sync prefix.
    fn rel_from_key(&self, key: &str) -> Option<String> {
        let rel = if self.prefix.is_empty() {
            key.to_string()
        } else {
            key.strip_prefix(&format!("{}/", self.prefix))?.to_string()
        };
        if rel.is_empty() {
            None
        } else {
            Some(rel.to_string())
        }
    }

    /// Recursively walk the local directory, skipping `.vfs/` and ignored
    /// directories (pruned at the walk level for efficiency; `build_plan`
    /// re-checks every file for correctness).
    fn walk_local(&self) -> Vec<LocalFile> {
        let mut files = Vec::new();
        let walker = walkdir::WalkDir::new(&self.local_dir)
            .follow_links(false)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|e| {
                if e.depth() == 0 {
                    return true;
                }
                let rel = rel_path(e.path(), &self.local_dir);
                if is_never_synced(&rel) {
                    return false;
                }
                if e.file_type().is_dir() {
                    return !self.ignore.is_ignored(&rel);
                }
                true
            });

        for entry in walker {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let rel = rel_path(entry.path(), &self.local_dir);
            // Ignored files are collected anyway; build_plan filters them and
            // counts them, so the plan remains the single source of truth.
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            files.push(LocalFile {
                rel_path: rel,
                size: meta.len(),
                mtime_unix: mtime,
            });
        }
        files
    }

    /// List remote objects under the sync prefix with metadata.
    async fn list_remote(&self) -> S3Result<Vec<RemoteObject>> {
        let objects = self
            .client
            .list_objects_with_meta(&self.bucket, &self.prefix)
            .await?;
        let mut out = Vec::new();
        for obj in objects {
            if let Some(rel) = self.rel_from_key(&obj.key) {
                out.push(RemoteObject {
                    rel_path: rel,
                    size: obj.size,
                    last_modified_unix: obj.last_modified_unix,
                });
            }
        }
        Ok(out)
    }
}

/// POSIX-style relative path of `path` within `base`.
fn rel_path(path: &Path, base: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Set a file's mtime to a UNIX epoch timestamp.
fn set_mtime(path: &Path, unix_secs: u64) -> std::io::Result<()> {
    let file = std::fs::OpenOptions::new().write(true).open(path)?;
    let times = std::fs::FileTimes::new()
        .set_modified(UNIX_EPOCH + std::time::Duration::from_secs(unix_secs));
    file.set_times(times)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- IgnoreMatcher ----------

    fn matcher(text: &str) -> IgnoreMatcher {
        IgnoreMatcher::parse(text)
    }

    #[test]
    fn ignore_blank_and_comment_lines() {
        let m = matcher("# comment\n\n  \n*.log\n");
        assert!(!m.is_ignored("src/main.rs"));
        assert!(m.is_ignored("src/debug.log"));
        assert!(m.is_ignored("a/b/c.log"));
    }

    #[test]
    fn ignore_basename_matches_at_any_depth() {
        let m = matcher("target\n");
        assert!(m.is_ignored("target"));
        assert!(m.is_ignored("target/foo.rs"));
        assert!(m.is_ignored("src/target"));
        assert!(m.is_ignored("src/target/x/y.rs"));
        assert!(!m.is_ignored("targets.rs"));
        assert!(!m.is_ignored("src/main.rs"));
    }

    #[test]
    fn ignore_wildcard_and_single_char() {
        let m = matcher("*.log\nfile?.txt\n");
        assert!(m.is_ignored("debug.log"));
        assert!(m.is_ignored("a/b/debug.log"));
        assert!(!m.is_ignored("debug.log.bak"));
        assert!(m.is_ignored("file1.txt"));
        assert!(m.is_ignored("sub/fileA.txt"));
        assert!(!m.is_ignored("file12.txt"));
    }

    #[test]
    fn ignore_dir_only_pattern() {
        let m = matcher("target/\n");
        assert!(m.is_ignored("target"));
        assert!(m.is_ignored("target/foo"));
        assert!(m.is_ignored("a/b/target/x.rs"));
        assert!(!m.is_ignored("target.txt"));
    }

    #[test]
    fn ignore_anchored_pattern() {
        let m = matcher("/build\n");
        assert!(m.is_ignored("build"));
        assert!(m.is_ignored("build/out.bin"));
        assert!(!m.is_ignored("src/build"));
    }

    #[test]
    fn ignore_middle_slash_is_root_relative() {
        let m = matcher("docs/private\n");
        assert!(m.is_ignored("docs/private"));
        assert!(m.is_ignored("docs/private/note.md"));
        assert!(!m.is_ignored("src/docs/private"));
    }

    #[test]
    fn ignore_double_star() {
        let m = matcher("**/cache\n");
        assert!(m.is_ignored("cache"));
        assert!(m.is_ignored("cache/x"));
        assert!(m.is_ignored("a/b/cache/y"));
        let m2 = matcher("a/**/b\n");
        assert!(m2.is_ignored("a/b"));
        assert!(m2.is_ignored("a/x/b"));
        assert!(m2.is_ignored("a/x/y/b/z"));
        assert!(!m2.is_ignored("c/a/b"));
    }

    #[test]
    fn ignore_negation_last_match_wins() {
        let m = matcher("*.log\n!important.log\n");
        assert!(m.is_ignored("debug.log"));
        assert!(!m.is_ignored("important.log"));
        assert!(!m.is_ignored("sub/important.log"));
    }

    #[test]
    fn ignore_negation_cannot_reinclude_under_ignored_dir() {
        // gitignore rule: a re-include under an excluded directory has no
        // effect because the directory itself stays excluded.
        let m = matcher("cache/\n!cache/keep.txt\n");
        assert!(m.is_ignored("cache"));
        assert!(m.is_ignored("cache/keep.txt"));
    }

    #[test]
    fn ignore_empty_and_missing_files() {
        let m = IgnoreMatcher::empty();
        assert!(!m.is_ignored("anything"));
        let tmp = tempfile::tempdir().unwrap();
        let m = IgnoreMatcher::from_file(&tmp.path().join("nope")).unwrap();
        assert!(!m.is_ignored("anything"));
    }

    // ---------- build_plan ----------

    fn lf(rel: &str, size: u64, mtime: u64) -> LocalFile {
        LocalFile {
            rel_path: rel.to_string(),
            size,
            mtime_unix: mtime,
        }
    }

    fn ro(rel: &str, size: i64, mtime: u64) -> RemoteObject {
        RemoteObject {
            rel_path: rel.to_string(),
            size,
            last_modified_unix: mtime,
        }
    }

    async fn engine(ignore_text: &str) -> SyncEngine {
        SyncEngine::new(
            // Client is never used by build_plan.
            S3Client::new("us-east-1", Path::new("/tmp/none"), 0, true)
                .await
                .expect("client"),
            "bucket".to_string(),
            "prefix".to_string(),
            PathBuf::from("/tmp/ws"),
            IgnoreMatcher::parse(ignore_text),
        )
    }

    #[test]
    fn decision_reports_matching_rule() {
        let m = IgnoreMatcher::parse("*.bin\nbuild/\n");
        let d = m.decision("a.bin");
        assert!(d.ignored);
        assert_eq!(d.rule.as_deref(), Some("*.bin"));
        let d = m.decision("build/cache.o");
        assert!(d.ignored);
        assert_eq!(d.rule.as_deref(), Some("build/"));
        let d = m.decision("keep.txt");
        assert!(!d.ignored);
        assert_eq!(d.rule, None);
    }

    #[test]
    fn decision_negation_reports_rule_not_ignored() {
        let m = IgnoreMatcher::parse("*.log\n!important.log\n");
        let d = m.decision("important.log");
        assert!(!d.ignored);
        assert_eq!(d.rule.as_deref(), Some("!important.log"));
        let d = m.decision("other.log");
        assert!(d.ignored);
        assert_eq!(d.rule.as_deref(), Some("*.log"));
    }

    #[test]
    fn decision_ancestor_dir_exclusion_wins_over_reinclude() {
        // gitignore: cannot re-include a file inside an excluded directory.
        let m = IgnoreMatcher::parse("out/\n!out/keep.txt\n");
        let d = m.decision("out/keep.txt");
        assert!(d.ignored);
        assert_eq!(d.rule.as_deref(), Some("out/"));
    }

    #[test]
    fn decision_parity_with_is_ignored() {
        let m = IgnoreMatcher::parse("*.tmp\ncache/\n!cache/keep.tmp\n");
        for p in [
            "a.tmp",
            "cache/x.tmp",
            "cache/keep.tmp",
            "sub/cache/keep.tmp",
            "plain.txt",
        ] {
            assert_eq!(m.is_ignored(p), m.decision(p).ignored, "parity for {p}");
        }
    }

    #[tokio::test]
    async fn plan_uploads_local_only() {
        let e = engine("").await;
        let local = vec![lf("a.txt", 10, 100)];
        let plan = e.build_plan(&local, &[]);
        assert_eq!(plan.uploads.len(), 1);
        assert_eq!(plan.uploads[0].rel_path, "a.txt");
        assert!(plan.downloads.is_empty());
    }

    #[tokio::test]
    async fn plan_downloads_remote_only() {
        let e = engine("").await;
        let remote = vec![ro("b.txt", 10, 100)];
        let plan = e.build_plan(&[], &remote);
        assert_eq!(plan.downloads.len(), 1);
        assert_eq!(plan.downloads[0].rel_path, "b.txt");
        assert!(plan.uploads.is_empty());
    }

    #[tokio::test]
    async fn plan_equal_timestamps_unchanged() {
        let e = engine("").await;
        let local = vec![lf("c.txt", 10, 100)];
        let remote = vec![ro("c.txt", 10, 100)];
        let plan = e.build_plan(&local, &remote);
        assert!(plan.uploads.is_empty());
        assert!(plan.downloads.is_empty());
        assert_eq!(plan.unchanged, 1);
    }

    #[tokio::test]
    async fn plan_local_newer_uploads() {
        let e = engine("").await;
        let local = vec![lf("c.txt", 10, 200)];
        let remote = vec![ro("c.txt", 10, 100)];
        let plan = e.build_plan(&local, &remote);
        assert_eq!(plan.uploads.len(), 1);
        assert!(plan.downloads.is_empty());
    }

    #[tokio::test]
    async fn plan_remote_newer_downloads() {
        let e = engine("").await;
        let local = vec![lf("c.txt", 10, 100)];
        let remote = vec![ro("c.txt", 10, 200)];
        let plan = e.build_plan(&local, &remote);
        assert!(plan.uploads.is_empty());
        assert_eq!(plan.downloads.len(), 1);
    }

    #[tokio::test]
    async fn plan_ignored_never_transferred_either_way() {
        let e = engine("*.bin\n").await;
        // keep.txt: local newer than remote -> upload; a.bin/old.bin ignored.
        let local = vec![lf("a.bin", 10, 100), lf("keep.txt", 10, 200)];
        let remote = vec![ro("old.bin", 10, 100), ro("keep.txt", 10, 100)];
        let plan = e.build_plan(&local, &remote);
        assert_eq!(plan.uploads.len(), 1);
        assert_eq!(plan.uploads[0].rel_path, "keep.txt");
        assert!(plan.downloads.is_empty());
        assert_eq!(plan.ignored_local, 1);
        assert_eq!(plan.ignored_remote, 1);
    }

    #[tokio::test]
    async fn plan_vfs_never_transferred() {
        let e = engine("").await;
        let local = vec![
            lf(".vfs/manifest.yaml", 10, 100),
            lf(".hiloignore", 10, 100),
        ];
        let remote = vec![
            ro(".vfs/edges.jsonl", 10, 100),
            ro(".hiloephemeral", 10, 100),
        ];
        let plan = e.build_plan(&local, &remote);
        assert!(plan.uploads.is_empty());
        assert!(plan.downloads.is_empty());
        assert_eq!(plan.ignored_local, 2);
        assert_eq!(plan.ignored_remote, 2);
    }

    #[tokio::test]
    async fn plan_mixed_state() {
        let e = engine("target/\n").await;
        let local = vec![
            lf("src/main.rs", 100, 1000),
            lf("target/debug/app", 999, 999),
            lf("both.txt", 10, 100),
        ];
        let remote = vec![
            ro("remote_only.txt", 5, 50),
            ro("both.txt", 10, 100),
            ro("target/remote.bin", 9, 90),
        ];
        let plan = e.build_plan(&local, &remote);
        assert_eq!(plan.uploads.len(), 1);
        assert_eq!(plan.uploads[0].rel_path, "src/main.rs");
        assert_eq!(plan.downloads.len(), 1);
        assert_eq!(plan.downloads[0].rel_path, "remote_only.txt");
        assert_eq!(plan.unchanged, 1);
        assert_eq!(plan.ignored_local, 1);
        assert_eq!(plan.ignored_remote, 1);
    }

    #[tokio::test]
    async fn rel_from_key_respects_prefix() {
        let e = engine("").await;
        assert_eq!(e.rel_from_key("prefix/a.txt"), Some("a.txt".to_string()));
        assert_eq!(e.rel_from_key("prefix"), None);
        assert_eq!(e.rel_from_key("other/a.txt"), None);
    }

    #[test]
    fn set_mtime_aligns_file_timestamp() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("f.txt");
        std::fs::write(&f, b"x").unwrap();
        set_mtime(&f, 1_700_000_000).unwrap();
        let mtime = std::fs::metadata(&f).unwrap().modified().unwrap();
        let secs = mtime.duration_since(UNIX_EPOCH).unwrap().as_secs();
        assert_eq!(secs, 1_700_000_000);
    }
}
