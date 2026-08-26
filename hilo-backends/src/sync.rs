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

/// Where an ignore rule came from (backend-backed-workspace-spec §4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IgnoreSource {
    /// Compile-time built-in defaults (spec §4.2).
    Builtin,
    /// The workspace root `.hiloignore` (or an explicit `--ignore-file`).
    RootFile,
    /// A `.hiloignore` discovered in a subdirectory (relative to the root).
    NestedFile(PathBuf),
}

/// One parsed ignore pattern.
#[derive(Debug)]
struct IgnorePattern {
    regex: Regex,
    negated: bool,
    /// trailing `/` — matches directories only (the dir itself, or a
    /// subtree below it)
    dir_only: bool,
    /// The raw line the pattern was parsed from (for reporting).
    source: String,
    /// Which ignore file the pattern came from.
    source_kind: IgnoreSource,
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
#[derive(Default, Debug)]
pub struct IgnoreMatcher {
    /// Root-level patterns: built-in defaults, then the root `.hiloignore`,
    /// then the optional extra file. Ordered; the last match wins.
    patterns: Vec<IgnorePattern>,
    /// Nested `.hiloignore` files: (directory relative to root, patterns
    /// scoped to that directory). Rules apply only to paths under the dir.
    nested: Vec<(PathBuf, Vec<IgnorePattern>)>,
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
            if let Some(p) = parse_line(line, IgnoreSource::RootFile) {
                patterns.push(p);
            }
        }
        Self {
            patterns,
            ..Self::default()
        }
    }

    /// Load patterns from a file. A missing file yields an empty matcher.
    pub fn from_file(path: &Path) -> std::io::Result<Self> {
        if !path.exists() {
            return Ok(Self::empty());
        }
        let text = std::fs::read_to_string(path)?;
        Ok(Self::parse(&text))
    }

    /// Compile-time built-in defaults (spec §4.2): prepended to every `load`
    /// matcher unless `no_defaults`; any user rule overrides them (last
    /// match wins).
    const BUILTIN_DEFAULTS: &'static str = "target/\nnode_modules/\n.venv/\nvenv/\n__pycache__/\ndist/\nbuild/\n.next/\n.cargo/\n.vfs/\n.git/\n*.o\n*.pyc\n*.class\n.DS_Store\n*.log\n.hiloignore\n.hiloephemeral\n";

    /// Loads rules: built-in defaults (unless `no_defaults`), the root
    /// `.hiloignore`, an optional extra file (`--ignore-file`), then
    /// discovers nested `.hiloignore` files under `root` (depth-first, never
    /// descending into `.vfs`/`.git`). Rules from nested files apply relative
    /// to their own directory; the nearest file wins.
    pub fn load(
        root: &Path,
        extra_file: Option<&Path>,
        no_defaults: bool,
    ) -> std::io::Result<Self> {
        let mut patterns = Vec::new();
        if !no_defaults {
            for line in Self::BUILTIN_DEFAULTS.lines() {
                if let Some(p) = parse_line(line, IgnoreSource::Builtin) {
                    patterns.push(p);
                }
            }
        }
        let root_file = root.join(".hiloignore");
        if root_file.is_file() {
            for line in std::fs::read_to_string(&root_file)?.lines() {
                if let Some(p) = parse_line(line, IgnoreSource::RootFile) {
                    patterns.push(p);
                }
            }
        }
        if let Some(extra) = extra_file {
            if !extra.is_file() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("ignore file not found: {}", extra.display()),
                ));
            }
            for line in std::fs::read_to_string(extra)?.lines() {
                if let Some(p) = parse_line(line, IgnoreSource::RootFile) {
                    patterns.push(p);
                }
            }
        }
        let mut nested: Vec<(PathBuf, Vec<IgnorePattern>)> = Vec::new();
        for entry in walkdir::WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                let name = e.file_name().to_string_lossy();
                !(e.depth() > 0 && (name == ".vfs" || name == ".git"))
            })
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() || entry.file_name() != ".hiloignore" {
                continue;
            }
            let dir_rel = entry
                .path()
                .parent()
                .and_then(|p| p.strip_prefix(root).ok())
                .map(|p| p.to_path_buf())
                .unwrap_or_default();
            if dir_rel.as_os_str().is_empty() {
                continue; // the root file is handled above
            }
            let mut pats = Vec::new();
            for line in std::fs::read_to_string(entry.path())?.lines() {
                if let Some(p) = parse_line(line, IgnoreSource::NestedFile(dir_rel.clone())) {
                    pats.push(p);
                }
            }
            nested.push((dir_rel, pats));
        }
        // Deeper files win on ties: evaluate shallower scopes first.
        nested.sort_by_key(|(dir, _)| dir.components().count());
        Ok(Self { patterns, nested })
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

    /// Git-exact match: like `is_ignored`, but a directory-only pattern
    /// (trailing `/`) matches a path that *is* the directory only when
    /// `is_dir` is true; paths below the directory still match regardless.
    pub fn matches(&self, rel_path: &str, is_dir: bool) -> bool {
        let mut best = Self::decide_scope(&self.patterns, rel_path, Some(is_dir));
        for (dir, pats) in &self.nested {
            let prefix = format!("{}/", dir.to_string_lossy());
            if let Some(sub) = rel_path.strip_prefix(&prefix) {
                let scoped = Self::decide_scope(pats, sub, Some(is_dir));
                if scoped.rule.is_some() {
                    best = scoped;
                }
            }
        }
        // Any ignored ancestor directory excludes the whole subtree. The
        // ancestors are directories by construction, so is_dir is true.
        let mut idx = 0;
        while let Some(slash) = rel_path[idx..].find('/') {
            idx += slash + 1;
            if self.matches(&rel_path[..idx - 1], true) {
                return true;
            }
        }
        best.ignored
    }

    /// Full ignore decision for `rel_path`: whether it is excluded, the raw
    /// ignore-file line responsible (the last matching pattern, or the
    /// ancestor-directory pattern that excludes the subtree), and the source
    /// ignore file. `rule`/`source` are `None` when no pattern matches.
    ///
    /// A negated (`!`) pattern that is the last match reports as not ignored
    /// with its rule still shown; an excluded ancestor directory wins over
    /// any re-inclusion below it (gitignore rule).
    pub fn decision(&self, rel_path: &str) -> IgnoreDecision {
        // Root-level scope (builtins + root file + extra file).
        let mut best = Self::decide_scope(&self.patterns, rel_path, None);
        // Nested scopes: the deepest applicable .hiloignore wins (nearest
        // file), and a nested negation can re-include what a root pattern
        // excluded (unless an excluded ancestor directory blocks it below).
        for (dir, pats) in &self.nested {
            let prefix = format!("{}/", dir.to_string_lossy());
            if let Some(sub) = rel_path.strip_prefix(&prefix) {
                let scoped = Self::decide_scope(pats, sub, None);
                if scoped.rule.is_some() {
                    best = scoped;
                }
            }
        }
        // Any ignored ancestor directory excludes the whole subtree.
        let mut idx = 0;
        while let Some(slash) = rel_path[idx..].find('/') {
            idx += slash + 1;
            let ancestor = &rel_path[..idx - 1];
            if self.is_ignored(ancestor) {
                let d = self.decision(ancestor);
                return IgnoreDecision {
                    ignored: true,
                    rule: d.rule.clone(),
                    source: d.source.clone(),
                };
            }
        }
        best
    }

    /// The last pattern matching `rel_path` (with optional directory-only
    /// enforcement), if any.
    fn last_match_rule<'a>(
        patterns: &'a [IgnorePattern],
        rel_path: &str,
        is_dir: Option<bool>,
    ) -> Option<&'a IgnorePattern> {
        let mut last: Option<&IgnorePattern> = None;
        for p in patterns {
            let matched = match is_dir {
                Some(is_dir) => pattern_matches(p, rel_path, is_dir),
                None => p.regex.is_match(rel_path),
            };
            if matched {
                last = Some(p);
            }
        }
        last
    }

    /// Decide against a single scope's pattern list (root-level or one
    /// nested file). `rel_path` is relative to that scope's directory.
    /// `is_dir: None` keeps the legacy behavior where directory-only
    /// patterns also match a path that is exactly the directory name.
    fn decide_scope(
        patterns: &[IgnorePattern],
        rel_path: &str,
        is_dir: Option<bool>,
    ) -> IgnoreDecision {
        let direct = Self::last_match_rule(patterns, rel_path, is_dir);
        if let Some(p) = direct {
            if !p.negated {
                return IgnoreDecision {
                    ignored: true,
                    rule: Some(p.source.clone()),
                    source: Some(p.source_kind.clone()),
                };
            }
        }
        // Any ignored ancestor directory excludes the whole subtree.
        let mut idx = 0;
        while let Some(slash) = rel_path[idx..].find('/') {
            idx += slash + 1;
            if let Some(p) = Self::last_match_rule(patterns, &rel_path[..idx - 1], Some(true)) {
                if !p.negated {
                    return IgnoreDecision {
                        ignored: true,
                        rule: Some(p.source.clone()),
                        source: Some(p.source_kind.clone()),
                    };
                }
            }
        }
        // Not ignored: report the last matching rule (e.g. a negation) if any.
        IgnoreDecision {
            ignored: false,
            rule: direct.map(|p| p.source.clone()),
            source: direct.map(|p| p.source_kind.clone()),
        }
    }
}

/// Whether `p` matches `rel_path` when directory-only patterns are enforced:
/// a `dir_only` pattern matches a path that is exactly the directory only
/// when `is_dir` is true; a path below the directory matches regardless.
fn pattern_matches(p: &IgnorePattern, rel_path: &str, is_dir: bool) -> bool {
    if !p.regex.is_match(rel_path) {
        return false;
    }
    if !p.dir_only || is_dir {
        return true;
    }
    // Directory-only pattern, path is a file: it matches only when the path
    // sits below the directory (some ancestor prefix is the directory).
    let mut idx = 0;
    while let Some(slash) = rel_path[idx..].find('/') {
        idx += slash + 1;
        if p.regex.is_match(&rel_path[..idx - 1]) {
            return true;
        }
    }
    false
}

/// Result of an ignore lookup: whether the path is excluded and which rule
/// (raw ignore-file line) decided it, plus where that rule came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IgnoreDecision {
    pub ignored: bool,
    /// Raw source line of the deciding pattern; `None` when nothing matches.
    pub rule: Option<String>,
    /// The ignore file the deciding pattern came from.
    pub source: Option<IgnoreSource>,
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
fn parse_line(line: &str, source_kind: IgnoreSource) -> Option<IgnorePattern> {
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
    let dir_only = body.ends_with('/');
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
        dir_only,
        source: trimmed.to_string(),
        source_kind,
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
pub(crate) fn is_never_synced(rel_path: &str) -> bool {
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

    // ---------- load(): builtin defaults, nested files, sources ----------

    fn write_fs(p: &Path, s: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    #[test]
    fn load_applies_builtin_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let m = IgnoreMatcher::load(tmp.path(), None, false).unwrap();
        assert!(m.is_ignored("target/artifact.bin"));
        assert!(m.is_ignored("node_modules/pkg/index.js"));
        assert!(m.is_ignored("sub/venv/bin/python"));
        assert!(m.is_ignored("build/out.o"));
        assert!(m.is_ignored("src/debug.log"));
        assert!(m.is_ignored(".vfs/graph/edges.jsonl"));
        assert!(!m.is_ignored("src/main.rs"));
        assert!(!m.is_ignored("README.md"));
        let d = m.decision("target/artifact.bin");
        assert_eq!(d.source, Some(IgnoreSource::Builtin));
        assert_eq!(d.rule.as_deref(), Some("target/"));
    }

    #[test]
    fn load_no_defaults_skips_builtins() {
        let tmp = tempfile::tempdir().unwrap();
        let m = IgnoreMatcher::load(tmp.path(), None, true).unwrap();
        assert!(!m.is_ignored("target/artifact.bin"));
        assert!(!m.is_ignored("src/debug.log"));
    }

    #[test]
    fn load_user_rule_overrides_builtin() {
        let tmp = tempfile::tempdir().unwrap();
        write_fs(&tmp.path().join(".hiloignore"), "!target/\n");
        let m = IgnoreMatcher::load(tmp.path(), None, false).unwrap();
        assert!(
            !m.is_ignored("target/artifact.bin"),
            "negation overrides builtin"
        );
        assert!(
            m.is_ignored("node_modules/x/index.js"),
            "other builtins stay"
        );
        let d = m.decision("target/artifact.bin");
        assert_eq!(d.source, Some(IgnoreSource::RootFile));
    }

    #[test]
    fn load_nested_ignore_scoped_to_its_directory() {
        let tmp = tempfile::tempdir().unwrap();
        write_fs(&tmp.path().join("sub/.hiloignore"), "cache.txt\n");
        let m = IgnoreMatcher::load(tmp.path(), None, false).unwrap();
        assert!(m.is_ignored("sub/cache.txt"));
        assert!(!m.is_ignored("cache.txt"), "root path unaffected");
        assert!(!m.is_ignored("other/cache.txt"), "other dirs unaffected");
        let d = m.decision("sub/cache.txt");
        assert_eq!(
            d.source,
            Some(IgnoreSource::NestedFile(PathBuf::from("sub")))
        );
        let d = m.decision("target/artifact.bin");
        assert_eq!(d.source, Some(IgnoreSource::Builtin));
    }

    #[test]
    fn load_nested_negation_reincludes_over_root_pattern() {
        let tmp = tempfile::tempdir().unwrap();
        write_fs(&tmp.path().join(".hiloignore"), "*.tmp\n");
        write_fs(&tmp.path().join("sub/.hiloignore"), "!keep.tmp\n");
        let m = IgnoreMatcher::load(tmp.path(), None, false).unwrap();
        assert!(m.is_ignored("sub/other.tmp"));
        assert!(!m.is_ignored("sub/keep.tmp"), "nested negation wins");
        assert!(m.is_ignored("keep.tmp"), "root path stays ignored");
    }

    #[test]
    fn load_nested_negation_blocked_by_excluded_ancestor_dir() {
        let tmp = tempfile::tempdir().unwrap();
        write_fs(&tmp.path().join(".hiloignore"), "sub/\n");
        write_fs(&tmp.path().join("sub/.hiloignore"), "!cache.txt\n");
        let m = IgnoreMatcher::load(tmp.path(), None, false).unwrap();
        assert!(
            m.is_ignored("sub/cache.txt"),
            "cannot re-include under excluded dir"
        );
        assert!(m.is_ignored("sub/anything.txt"));
    }

    #[test]
    fn load_missing_extra_file_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let err =
            IgnoreMatcher::load(tmp.path(), Some(&tmp.path().join("nope")), false).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn matches_honors_dir_only_with_is_dir() {
        let m = IgnoreMatcher::parse("target/\n");
        assert!(
            !m.matches("target", false),
            "file named target is not a dir"
        );
        assert!(m.matches("target", true), "the directory itself");
        assert!(
            m.matches("target/x.rs", false),
            "below the dir matches files"
        );
        assert!(m.matches("a/b/target/x", false));
        assert!(!m.matches("target.txt", false));
        // Legacy is_ignored keeps its looser behavior (locked by tests above).
        assert!(m.is_ignored("target"));
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
