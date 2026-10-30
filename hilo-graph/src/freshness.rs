//! Graph freshness ledger — `.vfs/graph/state.json`.
//!
//! DF-WARPFS-83: `graph warm` used to leave no durable record of WHAT it had
//! seen, so a second machine that `git pull`s kept answering queries from a
//! graph that predated the pull — silently. This module gives the graph a
//! git-aware staleness signal in three pieces:
//!
//! 1. [`FreshnessState`] — the ledger: the git HEAD `graph warm` last saw,
//!    stamped by warm itself on every completed run (both the fast-path and
//!    the full-write exit).
//! 2. [`git_head`] — the repo's current HEAD, read through Git so linked
//!    worktrees and custom `core.worktree` resolve the way Git itself
//!    resolves them.
//! 3. [`check_staleness`] — the comparison: `Some(reason)` when the working
//!    tree's HEAD differs from the ledger's `last_warm_head` (or the ledger
//!    never recorded a HEAD while a repo exists), `None` when the graph is
//!    provably fresh or freshness is undecidable (not a git repo, git
//!    unavailable, corrupt/absent ledger).
//!
//! Design rules inherited from the marker history (GAP-087): the ledger is
//! written ONLY by a completed warm (the repair already happened), it lives
//! beside the other rebuildable caches in `.vfs/graph/`, and every unknown
//! state resolves to "not stale" so the check can never wedge a query path.
//! `warm` is the only writer; the post-merge hook consumes the signal
//! indirectly by unconditionally warming graph-relevant pulls, and the
//! warn-on-warm pass gives any other caller the same answer for free.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Ledger file name, inside `.vfs/graph/`.
pub const FRESHNESS_STATE_FILE: &str = "state.json";

/// Current ledger schema version. Bump when the shape changes in a way old
/// readers must not misread.
pub const FRESHNESS_STATE_VERSION: u32 = 1;

/// What the last completed `graph warm` observed, persisted at
/// `.vfs/graph/state.json`.
///
/// Rebuildable runtime state: the ledger is re-stamped by the next warm when
/// absent, and a missing/corrupt ledger resolves to "freshness unknown",
/// never to a hard error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FreshnessState {
    /// Ledger schema version.
    pub version: u32,
    /// Git HEAD commit id the last completed `graph warm` ran against.
    /// `None` when the last warm happened outside a git repository (or git
    /// was unavailable) — no staleness signal can be derived in that state.
    #[serde(rename = "lastWarmHead")]
    pub last_warm_head: Option<String>,
}

impl Default for FreshnessState {
    fn default() -> Self {
        Self {
            version: FRESHNESS_STATE_VERSION,
            last_warm_head: None,
        }
    }
}

impl FreshnessState {
    /// A ledger stamped for `head` as observed by a completed warm.
    #[must_use]
    pub fn stamped(head: Option<String>) -> Self {
        Self {
            version: FRESHNESS_STATE_VERSION,
            last_warm_head: head,
        }
    }

    /// Read the ledger from `<graph_dir>/state.json`.
    ///
    /// Fail-open: a missing, unparsable, or wrong-version ledger yields the
    /// default (no recorded HEAD). `warm` re-stamps the file on its next
    /// completed run, so a corrupt ledger heals itself instead of wedging.
    #[must_use]
    pub fn load(graph_dir: &Path) -> Self {
        let path = graph_dir.join(FRESHNESS_STATE_FILE);
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match serde_json::from_str::<FreshnessState>(&raw) {
            Ok(state) if state.version == FRESHNESS_STATE_VERSION => state,
            _ => Self::default(),
        }
    }

    /// Write the ledger to `<graph_dir>/state.json`, creating `graph_dir`
    /// when missing. Returns the write error so callers can warn without
    /// failing the warm that just finished.
    pub fn write(&self, graph_dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(graph_dir)?;
        let raw = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string());
        std::fs::write(graph_dir.join(FRESHNESS_STATE_FILE), raw)
    }
}

/// Resolve the project's git dir through Git so linked worktrees (`.git` as
/// a file) and custom `core.worktree` resolve the way Git itself resolves
/// them. Returns `None` outside a git repository.
fn resolve_git_dir(project_dir: &Path) -> Option<std::path::PathBuf> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(project_dir)
        .args(["rev-parse", "--path-format=absolute", "--git-dir"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        None
    } else {
        Some(Path::new(&path).to_path_buf())
    }
}

/// The repository's current HEAD commit id, or `None` when the directory is
/// not a git repository, git is unavailable, or HEAD is unborn.
///
/// `git rev-parse HEAD` exits non-zero on an unborn branch (fresh `git init`
/// before the first commit) — that is the honest "no HEAD yet" answer: with
/// no commits there is nothing for a pull to advance past, so `None` maps to
/// "freshness unknown" downstream.
#[must_use]
pub fn git_head(project_dir: &Path) -> Option<String> {
    resolve_git_dir(project_dir)?;
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(project_dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let head = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if head.is_empty() {
        None
    } else {
        Some(head)
    }
}

/// Staleness verdict returned by [`check_staleness`]: the human-readable
/// reason the graph is presumed out of date. `None` means fresh (or
/// freshness undecidable — not a git repo, no git binary, corrupt ledger):
/// those states must never block a caller.
pub type StaleReason = String;

/// Compare the project's current git HEAD against the HEAD recorded by the
/// last completed `graph warm`.
///
/// - `Some(reason)` — HEAD moved since the last warm: the graph was built
///   from older source. `warm` must not take cache fast paths in this state.
/// - `None` — the graph is fresh (HEAD matches the ledger), or freshness is
///   undecidable: not a git repository, git unavailable, unborn HEAD, or a
///   missing/corrupt ledger. Undecidable states resolve to "not stale" so
///   the check can never wedge a warm; the next completed warm stamps the
///   ledger and future comparisons become decidable.
#[must_use]
pub fn check_staleness(project_dir: &Path) -> Option<StaleReason> {
    let head = git_head(project_dir)?;
    let graph_dir = project_dir.join(".vfs").join("graph");
    let state = FreshnessState::load(&graph_dir);
    match state.last_warm_head {
        Some(last) if last != head => Some(format!(
            "git HEAD is {head} but the last graph warm ran at {last} — pull? run a full `hilo graph warm`"
        )),
        // Same HEAD, or the ledger predates stamping (last_warm_head: None):
        // no evidence of staleness. The warn path stays silent; warm stamps
        // the ledger so the NEXT pull is detectable.
        _ => None,
    }
}

/// Stamp the ledger with the project's current HEAD after a completed warm.
///
/// Called on both warm exits (full write and "all cached" fast path) so the
/// ledger always answers for the graph as it exists RIGHT NOW. Failures are
/// returned for the caller to warn about — a stamp failure must not fail a
/// warm that already updated the graph.
pub fn stamp_after_warm(project_dir: &Path) -> std::io::Result<()> {
    let head = git_head(project_dir);
    let graph_dir = project_dir.join(".vfs").join("graph");
    FreshnessState::stamped(head).write(&graph_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tempdir guaranteed to sit outside any git repository.
    ///
    /// `tempfile::tempdir()` follows TMPDIR, which can live inside a repo
    /// (e.g. an agent scratch dir under a home meta-repo) and would make
    /// "outside repo" fixtures observe that repository's HEAD.
    fn outside_repo_tempdir() -> tempfile::TempDir {
        let mut roots = vec![std::env::temp_dir()];
        roots.push(std::path::PathBuf::from("/var/tmp"));
        roots.push(std::path::PathBuf::from("/tmp"));
        for base in &roots {
            if let Ok(dir) = tempfile::tempdir_in(base) {
                if git_head(dir.path()).is_none() {
                    return dir;
                }
            }
        }
        panic!(
            "no temp root outside a git repository (TMPDIR contamination: {:?})",
            std::env::temp_dir()
        );
    }

    /// A project dir with `.vfs/graph/` (no git — plain fixture).
    fn plain_project() -> tempfile::TempDir {
        let dir = outside_repo_tempdir();
        std::fs::create_dir_all(dir.path().join(".vfs").join("graph")).unwrap();
        dir
    }

    /// A temp git repo with one commit; returns (dir, head-sha).
    fn git_project() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .output()
                .expect("git must be available for freshness tests");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        run(&["init", "--quiet"]);
        run(&["config", "user.email", "hilo-test@example.invalid"]);
        run(&["config", "user.name", "Hilo Test"]);
        std::fs::write(dir.path().join("seed.txt"), "seed\n").unwrap();
        run(&["add", "seed.txt"]);
        run(&["commit", "--quiet", "-m", "seed"]);
        let head = run(&["rev-parse", "HEAD"]);
        (dir, head)
    }

    #[test]
    fn load_missing_ledger_is_default() {
        let dir = plain_project();
        let state = FreshnessState::load(&dir.path().join(".vfs").join("graph"));
        assert_eq!(state, FreshnessState::default());
        assert_eq!(state.last_warm_head, None);
    }

    #[test]
    fn load_corrupt_ledger_is_default() {
        let dir = plain_project();
        let graph = dir.path().join(".vfs").join("graph");
        std::fs::write(graph.join(FRESHNESS_STATE_FILE), "not json at all{{{").unwrap();
        assert_eq!(FreshnessState::load(&graph), FreshnessState::default());
    }

    #[test]
    fn load_wrong_version_is_default() {
        let dir = plain_project();
        let graph = dir.path().join(".vfs").join("graph");
        std::fs::write(
            graph.join(FRESHNESS_STATE_FILE),
            r#"{"version":999,"lastWarmHead":"abc"}"#,
        )
        .unwrap();
        assert_eq!(FreshnessState::load(&graph), FreshnessState::default());
    }

    #[test]
    fn write_then_load_roundtrips() {
        let dir = plain_project();
        let graph = dir.path().join(".vfs").join("graph");
        FreshnessState::stamped(Some("abc123".to_string()))
            .write(&graph)
            .unwrap();
        let state = FreshnessState::load(&graph);
        assert_eq!(state.version, FRESHNESS_STATE_VERSION);
        assert_eq!(state.last_warm_head.as_deref(), Some("abc123"));
    }

    #[test]
    fn write_creates_missing_graph_dir() {
        let dir = tempfile::tempdir().unwrap();
        let graph = dir.path().join(".vfs").join("graph");
        assert!(!graph.exists());
        FreshnessState::stamped(Some("abc".to_string()))
            .write(&graph)
            .unwrap();
        assert!(graph.join(FRESHNESS_STATE_FILE).is_file());
    }

    #[test]
    fn git_head_none_outside_repo() {
        let dir = outside_repo_tempdir();
        assert_eq!(git_head(dir.path()), None);
    }

    #[test]
    fn git_head_reads_real_repo() {
        let (dir, head) = git_project();
        assert_eq!(git_head(dir.path()).as_deref(), Some(head.as_str()));
    }

    #[test]
    fn check_staleness_none_without_ledger_stamp() {
        // A repo whose warm never stamped a HEAD: freshness undecidable
        // (last_warm_head None) — resolve to not-stale, never block.
        let (dir, _head) = git_project();
        std::fs::create_dir_all(dir.path().join(".vfs").join("graph")).unwrap();
        assert_eq!(check_staleness(dir.path()), None);
    }

    #[test]
    fn check_staleness_none_when_fresh() {
        let (dir, head) = git_project();
        let graph = dir.path().join(".vfs").join("graph");
        std::fs::create_dir_all(&graph).unwrap();
        FreshnessState::stamped(Some(head)).write(&graph).unwrap();
        assert_eq!(check_staleness(dir.path()), None);
    }

    #[test]
    fn check_staleness_some_when_head_moved() {
        let (dir, head) = git_project();
        let graph = dir.path().join(".vfs").join("graph");
        std::fs::create_dir_all(&graph).unwrap();
        // Ledger remembers the PRE-pull HEAD.
        FreshnessState::stamped(Some("0000000000000000000000000000000000000000".to_string()))
            .write(&graph)
            .unwrap();
        let reason = check_staleness(dir.path()).expect("HEAD moved since last warm");
        assert!(
            reason.contains(head.trim().get(..7).unwrap_or("")),
            "reason must name the new HEAD: {reason}"
        );
        assert!(
            reason.contains("0000000"),
            "reason must name the last-warm HEAD: {reason}"
        );
    }

    #[test]
    fn check_staleness_none_outside_git_repo() {
        let dir = plain_project();
        assert_eq!(check_staleness(dir.path()), None);
    }

    #[test]
    fn stamp_after_warm_records_head() {
        let (dir, head) = git_project();
        stamp_after_warm(dir.path()).unwrap();
        let state = FreshnessState::load(&dir.path().join(".vfs").join("graph"));
        assert_eq!(state.last_warm_head.as_deref(), Some(head.as_str()));
    }

    #[test]
    fn stamp_after_warm_outside_git_records_none() {
        let dir = plain_project();
        stamp_after_warm(dir.path()).unwrap();
        let state = FreshnessState::load(&dir.path().join(".vfs").join("graph"));
        assert_eq!(state.last_warm_head, None);
    }
}
