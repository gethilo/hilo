//! DF-WARPFS-83 — end-to-end staleness detection through the `hilo` binary.
//!
//! Simulates the two-machine flow with real git commits and the compiled
//! CLI: warm at c1 (ledger stamped), "machine A" commits c2, the tree
//! advances (a pull), and the next `hilo graph warm` must WARN that the
//! graph is out of date — then re-stamp the ledger so the warning clears.
//! Library-level units live in `hilo-graph/tests/staleness_tests.rs`; the
//! git-hook producer/consumer arms live in `hilo-cli/src/commands/hooks.rs`
//! (run with `/bin/sh` against stub `hilo` binaries).

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_hilo");

/// A throwaway git repo prepared as a Hilo project (manifest + one source
/// commit), with an injected HOME so the warm HOME-guard never trips.
struct Project {
    dir: tempfile::TempDir,
    home: tempfile::TempDir,
}

impl Project {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args([
                    "-c",
                    "user.name=Hilo Test",
                    "-c",
                    "user.email=hilo-test@example.invalid",
                ])
                .args(args)
                .output()
                .expect("git must be available for staleness tests");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "--quiet"]);
        // Hilo project manifest — the same shape `hilo init` writes.
        std::fs::create_dir_all(dir.path().join(".vfs")).unwrap();
        std::fs::write(
            dir.path().join(".vfs").join("manifest.yaml"),
            "version: 2\nproject:\n  name: staleness-fixture\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        Self { dir, home }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn head(&self) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(self.path())
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        assert!(out.status.success(), "repo must have a HEAD");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn commit(&self, rel: &str, content: &str, msg: &str) {
        let path = self.path().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, content).unwrap();
        let out = Command::new("git")
            .arg("-C")
            .arg(self.path())
            .args([
                "-c",
                "user.name=Hilo Test",
                "-c",
                "user.email=hilo-test@example.invalid",
            ])
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
            .args([
                "-c",
                "user.name=Hilo Test",
                "-c",
                "user.email=hilo-test@example.invalid",
            ])
            .args(["commit", "--quiet", "-m", msg])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git commit failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn ledger_path(&self) -> PathBuf {
        self.path().join(".vfs").join("graph").join("state.json")
    }

    fn ledger_head(&self) -> Option<String> {
        let raw = std::fs::read_to_string(self.ledger_path()).ok()?;
        let v: Value = serde_json::from_str(&raw).ok()?;
        v.get("lastWarmHead")
            .and_then(|h| h.as_str())
            .map(|s| s.to_string())
    }

    /// Run `hilo graph warm` in the project (HOME injected).
    fn warm(&self) -> std::process::Output {
        Command::new(BIN)
            .args(["graph", "warm"])
            .current_dir(self.path())
            .env("HOME", self.home.path())
            .output()
            .expect("hilo binary must spawn")
    }
}

#[test]
fn warm_stamps_ledger_then_warns_after_pull_then_heals() {
    let project = Project::new();
    project.commit("src/main.rs", "fn main() {}\n", "c1 seed");

    // Warm #1 (full write path — first warm, nothing cached).
    let out = project.warm();
    assert!(
        out.status.success(),
        "first warm failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let head1 = project.head();
    assert_eq!(
        project.ledger_head().as_deref(),
        Some(head1.as_str()),
        "first warm must stamp the ledger with HEAD"
    );

    // Warm #2 at the same HEAD (fast path) — ledger stays truthful.
    let out = project.warm();
    assert!(out.status.success());
    assert_eq!(project.ledger_head().as_deref(), Some(head1.as_str()));

    // "Machine A" commits; "machine B" pulls — tree HEAD advances.
    project.commit("src/pulled.rs", "pub fn pulled() {}\n", "c2 pulled");
    let head2 = project.head();
    assert_ne!(head1, head2);

    // Warm #3 must WARN (stderr) and still succeed.
    let out = project.warm();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("graph is out of date"),
        "warm must warn on HEAD drift after a pull, stderr was: {stderr}"
    );
    assert!(
        stderr.contains(&head2[..7]),
        "warning must name the new HEAD: {stderr}"
    );

    // The same warm re-stamped the ledger: a follow-up warm is quiet.
    assert_eq!(project.ledger_head().as_deref(), Some(head2.as_str()));
    let out = project.warm();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("graph is out of date"),
        "warning must clear once the graph is warmed at the new HEAD: {stderr}"
    );
}

#[test]
fn changed_incremental_warm_never_warns_and_still_stamps() {
    let project = Project::new();
    project.commit("src/main.rs", "fn main() {}\n", "c1 seed");
    // --changed (the post-commit hook's invocation) bypasses the staleness
    // check but must still stamp the ledger so later checks are decidable.
    let out = Command::new(BIN)
        .args(["graph", "warm", "--changed"])
        .current_dir(project.path())
        .env("HOME", project.home.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "--changed warm failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        project.ledger_head().as_deref(),
        Some(project.head().as_str())
    );

    // DF-WARPFS-167: --changed observes only the new file, but coverage.json
    // is corpus-wide. A changed warm must not replace the full ledger with a
    // one-file snapshot or make stats report a fabricated stale census.
    let coverage_path = project.path().join(".vfs/graph/coverage.json");
    let before: Value =
        serde_json::from_str(&std::fs::read_to_string(&coverage_path).unwrap()).unwrap();
    let discovered = before
        .get("discovered_files")
        .and_then(Value::as_u64)
        .unwrap();
    assert!(discovered >= 1);
    std::fs::write(project.path().join("src/new.rs"), "pub fn new_file() {}\n").unwrap();
    let changed = Command::new(BIN)
        .args(["graph", "warm", "--changed"])
        .current_dir(project.path())
        .env("HOME", project.home.path())
        .output()
        .unwrap();
    assert!(
        changed.status.success(),
        "--changed warm failed: {}",
        String::from_utf8_lossy(&changed.stderr)
    );
    let after: Value =
        serde_json::from_str(&std::fs::read_to_string(&coverage_path).unwrap()).unwrap();
    assert_eq!(
        after.get("discovered_files").and_then(Value::as_u64),
        Some(discovered)
    );
    let stats = Command::new(BIN)
        .args(["graph", "stats"])
        .current_dir(project.path())
        .env("HOME", project.home.path())
        .output()
        .unwrap();
    assert!(
        stats.status.success(),
        "stats failed: {}",
        String::from_utf8_lossy(&stats.stderr)
    );
    let stats_stdout = String::from_utf8_lossy(&stats.stdout);
    assert!(
        !stats_stdout.contains("warm last counted 1 discovered"),
        "incremental ledger clobbered: {stats_stdout}"
    );
    assert!(
        !stats_stdout.contains("means the graph predates the last warm"),
        "false stale census after incremental warm: {stats_stdout}"
    );
}

#[test]
fn non_git_project_warms_without_warning_or_ledger_head() {
    let dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".vfs")).unwrap();
    std::fs::write(
        dir.path().join(".vfs").join("manifest.yaml"),
        "version: 2\nproject:\n  name: no-git-fixture\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();

    let out = Command::new(BIN)
        .args(["graph", "warm"])
        .current_dir(dir.path())
        .env("HOME", home.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "warm outside git failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("graph is out of date"),
        "non-git projects must never warn: {stderr}"
    );
    let ledger =
        std::fs::read_to_string(dir.path().join(".vfs").join("graph").join("state.json")).unwrap();
    let v: Value = serde_json::from_str(&ledger).unwrap();
    assert_eq!(
        v.get("lastWarmHead").and_then(|h| h.as_str()),
        None,
        "outside git there is no HEAD to record"
    );
}
