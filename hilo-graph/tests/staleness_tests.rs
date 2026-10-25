//! DF-WARPFS-83 — graph staleness detection across pulls.
//!
//! The two-machine failure this suite pins: machine A commits and pushes;
//! machine B pulls. Git preserves file mtimes on pull, so machine B's parse
//! cache stays "valid" while its graph predates the pull — every query
//! answered from stale edges with no signal. The fix is a git-aware
//! freshness ledger (`.vfs/graph/state.json`) written by every completed
//! warm and consulted by `graph warm`.
//!
//! Here that scenario is simulated at the library level with real git
//! commits standing in for the remote pushes; `hilo-cli/tests/
//! dfwarpfs83_staleness.rs` drives the same scenario through the compiled
//! `hilo` binary.

use std::path::Path;
use std::process::Command;

use hilo_graph::freshness::{
    check_staleness, git_head, stamp_after_warm, FreshnessState, FRESHNESS_STATE_FILE,
};

/// A throwaway git repo with identity pre-configured.
struct Repo {
    dir: tempfile::TempDir,
}

impl Repo {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .output()
                .expect("git must be available for staleness tests");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["init", "--quiet"]);
        run(&["config", "user.email", "hilo-test@example.invalid"]);
        run(&["config", "user.name", "Hilo Test"]);
        Self { dir }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn head(&self) -> String {
        git_head(self.path()).expect("repo must have a HEAD")
    }

    /// "Machine A": write a file and commit it (what the remote would carry).
    fn commit(&self, rel: &str, content: &str, msg: &str) {
        let path = self.path().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, content).unwrap();
        let out = Command::new("git")
            .arg("-C")
            .arg(self.path())
            .args(["add", rel])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git add failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let out = Command::new("git")
            .arg("-C")
            .arg(self.path())
            .args(["commit", "--quiet", "-m", msg])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git commit failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// "Machine B clone": the pulled working tree plus the ledger its last warm
/// stamped. Returns the HEAD the ledger now records.
fn warm_stamp(repo: &Repo) -> String {
    stamp_after_warm(repo.path()).unwrap();
    repo.head()
}

fn ledger_state(repo: &Repo) -> FreshnessState {
    FreshnessState::load(&repo.path().join(".vfs").join("graph"))
}

#[test]
fn warm_stamps_ledger_with_current_head() {
    let repo = Repo::new();
    repo.commit("src/main.rs", "fn main() {}\n", "c1");
    assert!(!repo
        .path()
        .join(".vfs")
        .join("graph")
        .join(FRESHNESS_STATE_FILE)
        .exists());

    let stamped = warm_stamp(&repo);
    let state = ledger_state(&repo);
    assert_eq!(state.last_warm_head.as_deref(), Some(stamped.as_str()));
    assert_eq!(
        state.last_warm_head.as_deref(),
        Some(repo.head().as_str()),
        "ledger must record the HEAD warm actually saw"
    );
}

#[test]
fn fresh_graph_after_warm_is_not_stale() {
    let repo = Repo::new();
    repo.commit("src/main.rs", "fn main() {}\n", "c1");
    warm_stamp(&repo);
    assert_eq!(
        check_staleness(repo.path()),
        None,
        "a graph warmed at the current HEAD must not be flagged stale"
    );
}

/// THE two-machine repro: graph warmed at c1, "machine A" commits c2, the
/// tree advances to c2 (a pull). The graph must now be detectably stale,
/// and the reason must name both the new HEAD and the last-warmed HEAD.
#[test]
fn pull_after_warm_is_detected_stale() {
    let repo = Repo::new();
    repo.commit("src/main.rs", "fn main() {}\n", "c1");
    let warm_head = warm_stamp(&repo);

    // Machine A commits; machine B pulls — working tree HEAD advances.
    repo.commit("src/pulled_module.rs", "pub fn pulled() {}\n", "c2");
    let new_head = repo.head();
    assert_ne!(warm_head, new_head, "commit must move HEAD");

    let reason = check_staleness(repo.path()).expect("HEAD moved since last warm — must be stale");
    assert!(
        reason.contains(&new_head[..7.min(new_head.len())]),
        "reason must name the new HEAD: {reason}"
    );
    assert!(
        reason.contains(&warm_head[..7.min(warm_head.len())]),
        "reason must name the last-warm HEAD: {reason}"
    );

    // The healing warm re-stamps; staleness clears.
    warm_stamp(&repo);
    assert_eq!(check_staleness(repo.path()), None);
}

/// An added source file is the pull case that mtime-based `--changed`
/// cannot see (git preserves mtimes): the HEAD comparison is the only
/// signal, so it must fire on ledger CONTENT alone — a freshly-touched
/// ledger file carrying a stale HEAD must not mask the drift.
#[test]
fn stale_detection_is_content_based_not_mtime_based() {
    let repo = Repo::new();
    repo.commit("src/main.rs", "fn main() {}\n", "c1");
    warm_stamp(&repo);

    repo.commit("src/late.rs", "pub fn late() {}\n", "c2");
    // Same ledger bytes, brand-new file mtime — the inverse of a fresh
    // cache with old mtimes. The signal must stay git-derived.
    let ledger = repo
        .path()
        .join(".vfs")
        .join("graph")
        .join(FRESHNESS_STATE_FILE);
    let pre = std::fs::read_to_string(&ledger).unwrap();
    std::fs::write(&ledger, pre).unwrap();

    assert!(
        check_staleness(repo.path()).is_some(),
        "a rewritten-but-stale ledger must still report staleness (signal is content, not mtime)"
    );
}

#[test]
fn no_ledger_yet_resolves_not_stale() {
    // Fresh clone before the first warm: freshness undecidable, never block.
    let repo = Repo::new();
    repo.commit("src/main.rs", "fn main() {}\n", "c1");
    assert_eq!(check_staleness(repo.path()), None);
    // Legacy graph (no ledger file) inside an existing .vfs: same answer.
    std::fs::create_dir_all(repo.path().join(".vfs").join("graph")).unwrap();
    std::fs::write(
        repo.path().join(".vfs").join("graph").join("edges.jsonl"),
        "",
    )
    .unwrap();
    assert_eq!(check_staleness(repo.path()), None);
}

#[test]
fn corrupt_ledger_resolves_not_stale_and_reheals() {
    let repo = Repo::new();
    repo.commit("src/main.rs", "fn main() {}\n", "c1");
    warm_stamp(&repo);
    repo.commit("src/other.rs", "pub fn o() {}\n", "c2");

    let ledger = repo
        .path()
        .join(".vfs")
        .join("graph")
        .join(FRESHNESS_STATE_FILE);
    std::fs::write(&ledger, "{{{corrupt").unwrap();
    assert_eq!(
        check_staleness(repo.path()),
        None,
        "corrupt ledger = freshness undecidable = not stale (never wedge)"
    );

    // The next completed warm re-stamps and decidability returns.
    warm_stamp(&repo);
    assert_eq!(check_staleness(repo.path()), None);
}

#[test]
fn non_git_project_is_never_stale() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".vfs").join("graph")).unwrap();
    stamp_after_warm(dir.path()).unwrap();
    let state = FreshnessState::load(&dir.path().join(".vfs").join("graph"));
    assert_eq!(state.last_warm_head, None);
    assert_eq!(check_staleness(dir.path()), None);
}
