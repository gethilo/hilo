//! DF-WARPFS-162: `hilo init` hook installation must stay repo-local and must
//! never destroy pre-existing hook content.
//!
//! Incident: a throwaway clone's `hilo init` honoured the host-wide
//! `core.hooksPath` and rewrote `/home/kara/.hermes/hooks/{post-commit,
//! post-merge}` — a directory every repository on the machine reads —
//! replacing ~109 bytes of prior hook content with NO backup.
//!
//! These integration tests drive the real `hilo init` binary end-to-end:
//!
//! 1. AC-1/AC-4a — `core.hooksPath` resolving OUTSIDE the repository is
//!    refused (nothing written there), the resolved path is named, and
//!    `hilo init` still completes its repo-local work.
//! 2. AC-3 — with no hooks-path configuration at all, hooks are installed
//!    into the repo's own `.git/hooks/`.
//! 3. AC-2/AC-4b — an existing hook file is backed up byte-identically
//!    beside itself before any overwrite, and the backup path is printed.
//! 4. AC-2 round-trip — a second `hilo init` keeps the whole pre-image chain
//!    recoverable (the first backup still holds the original bytes).
//!
//! Every fixture pins `core.hooksPath` repo-locally (or clears git's global
//! config via `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM`) so the tests are
//! deterministic on hosts that carry a global `core.hooksPath`.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};

const BIN: &str = env!("CARGO_BIN_EXE_hilo");

const RELINK_RETRIES: usize = 3;
const RELINK_RETRY_SLEEP: Duration = Duration::from_secs(2);

/// Start building a `hilo` command.
fn hilo_cmd() -> Command {
    Command::new(BIN)
}

/// Classify a spawn result as a concurrent-relink casualty
/// (INT-GITREINS-004 pattern from `hilo-cli/tests/cli.rs`): ENOENT /
/// ETXTBSY / instant link death while another build relinks the binary.
fn spawn_hit_relink(
    spawn_err: Option<&std::io::Error>,
    output: Option<&std::process::Output>,
) -> bool {
    if let Some(err) = spawn_err {
        if err.kind() == std::io::ErrorKind::NotFound {
            return true;
        }
        if err.raw_os_error() == Some(26) {
            return true; // ETXTBSY
        }
    }
    if let Some(out) = output {
        let empty = out.stdout.is_empty() && out.stderr.is_empty();
        return empty && !out.status.success() && out.status.code().is_none();
    }
    false
}

/// Run `hilo init --allow-home` (hooks ENABLED) in `dir`, retrying
/// relink casualties from concurrent builds of the same binary.
fn run_init(dir: &Path) -> std::process::Output {
    let mut last_err: Option<std::io::Error> = None;
    for _ in 0..RELINK_RETRIES {
        let result = hilo_cmd()
            .arg("init")
            .arg("--allow-home")
            .current_dir(dir)
            .output();
        match result {
            Ok(out) if out.status.success() => return out,
            Ok(out) if spawn_hit_relink(None, Some(&out)) => {
                std::thread::sleep(RELINK_RETRY_SLEEP);
                continue;
            }
            Ok(out) => {
                panic!("hilo init failed: {}", String::from_utf8_lossy(&out.stderr));
            }
            Err(err) => {
                if spawn_hit_relink(Some(&err), None) {
                    std::thread::sleep(RELINK_RETRY_SLEEP);
                    last_err = Some(err);
                    continue;
                }
                panic!("hilo init spawn failed: {err}");
            }
        }
    }
    panic!(
        "hilo init hit relink casualties {} times (last: {:?})",
        RELINK_RETRIES, last_err
    );
}

/// Run a git subcommand in `dir`, panicking on failure.
fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git must be available for hook tests");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Fresh git repository in its own tempdir, removed on drop.
fn scratch_git_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp project dir");
    git(dir.path(), &["init", "--quiet", "-b", "main"]);
    dir
}

/// Flat snapshot of a directory: sorted (name, bytes, mtime) triples.
///
/// Used to prove a directory that must not be mutated is byte- and
/// mtime-identical across an `hilo init` run.
fn snapshot_dir(dir: &Path) -> Vec<(String, Vec<u8>, SystemTime)> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display())) {
        let entry = entry.expect("dir entry");
        let meta = entry.metadata().expect("entry metadata");
        assert!(
            meta.is_file(),
            "snapshot_dir expects a flat dir; {entry:?} is not a file"
        );
        let name = entry.file_name().to_string_lossy().into_owned();
        let bytes = fs::read(entry.path())
            .unwrap_or_else(|e| panic!("read {}: {e}", entry.path().display()));
        entries.push((name, bytes, meta.modified().expect("mtime")));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
}

/// The legacy post-commit content used as the "pre-existing hook" fixture —
/// stands in for the ~109 bytes of foreign hook logic the incident destroyed.
const LEGACY_HOOK: &str = "#!/bin/sh\n# legacy-guard: keep me\necho legacy-guard ran\nexit 0\n";

/// Write `content` to `path` and make it executable.
fn seed_hook(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("hooks parent dir");
    }
    fs::write(path, content).expect("seed hook");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod hook");
    }
}

/// AC-1 / AC-4a: `core.hooksPath` pointing OUTSIDE the repository is refused.
///
/// The resolved path is named, the outside directory gains no file and keeps
/// byte-identical content + mtimes, the repo-local `.git/hooks/` gains no
/// hook either, and `hilo init` still succeeds at its repo-local work.
///
/// The fixture pins the outside path repo-locally (repo-local overrides
/// global) so the test reproduces the incident deterministically even on
/// hosts without a global `core.hooksPath`.
#[test]
fn init_refuses_hookspath_outside_repo_and_mutates_nothing() {
    let proj = scratch_git_project();
    let outside = tempfile::tempdir().expect("temp outside dir");
    let shared_hooks = outside.path().join("shared-hooks");
    seed_hook(
        &shared_hooks.join("pre-commit"),
        "#!/bin/sh\n# pre-existing guard\n",
    );

    // Repo-local override pointing at a shared, out-of-repo hooks dir.
    let hooks_path = shared_hooks.display().to_string();
    git(proj.path(), &["config", "core.hooksPath", &hooks_path]);

    let before = snapshot_dir(&shared_hooks);
    assert!(
        before.iter().any(|(name, _, _)| name == "pre-commit"),
        "premise: sentinel hook must exist in the shared dir before init"
    );

    let out = run_init(proj.path());

    // Refusal is a skip, not a command failure: the repo-local .vfs work
    // still completed. If this is ever changed to a hard error, update the
    // hooks.rs refusal unit test too.
    assert!(
        out.status.success(),
        "hilo init should complete (skipping hooks); stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("outside this repository"),
        "refusal must name the problem; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains(&hooks_path),
        "refusal must name the RESOLVED hooks path ({hooks_path}); stderr:\n{stderr}"
    );

    // The shared directory is untouched: same files, same bytes, same mtimes,
    // nothing new (no hook, no .bak).
    let after = snapshot_dir(&shared_hooks);
    assert_eq!(
        before, after,
        "shared hooks dir must be byte- and mtime-identical"
    );

    // And the repo-local hooks dir gained nothing either.
    let repo_hooks = proj.path().join(".git").join("hooks");
    let installed: Vec<String> = fs::read_dir(&repo_hooks)
        .expect(".git/hooks exists after git init")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .filter(|n| n == "post-commit" || n == "post-merge")
        .collect();
    assert!(
        installed.is_empty(),
        "no hook may be installed when the resolved hooks dir is outside the repo; found {installed:?}"
    );

    // Repo-local work still happened.
    assert!(
        proj.path().join(".vfs").join("manifest.yaml").exists(),
        "hilo init must still create the manifest when hooks are refused"
    );
}

/// AC-3: with no hooks-path configuration at all (git global config cleared
/// via env so a host-wide `core.hooksPath` cannot leak in), hooks land in the
/// repo's own `.git/hooks/`.
#[test]
fn init_default_install_goes_to_repo_local_git_hooks() {
    let proj = scratch_git_project();
    // Premise: no hooks-path config anywhere for this fixture.
    let check = Command::new("git")
        .arg("-C")
        .arg(proj.path())
        .args(["config", "--show-origin", "--get", "core.hooksPath"])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git must be available");
    assert!(
        !check.status.success(),
        "premise: fixture must have no core.hooksPath configured"
    );

    let mut cmd = hilo_cmd();
    cmd.arg("init").arg("--allow-home").current_dir(proj.path());
    // Hermetic git for the hilo process's own `git rev-parse` calls.
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null");
    let out = cmd.output().expect("spawn hilo");
    assert!(
        out.status.success(),
        "hilo init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let hooks = proj.path().join(".git").join("hooks");
    for name in ["post-commit", "post-merge"] {
        let hook = hooks.join(name);
        assert!(hook.exists(), "{name} must be installed into .git/hooks/");
        let content = fs::read_to_string(&hook).expect("read installed hook");
        assert!(
            content.contains("### HILO"),
            "{name} must carry the HILO block"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&hook)
                .expect("hook metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111, "{name} must be executable");
        }
    }

    // Fresh install overwrites nothing, so there is no backup yet.
    let baks: Vec<String> = fs::read_dir(&hooks)
        .expect("hooks dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".bak-"))
        .collect();
    assert!(
        baks.is_empty(),
        "fresh install must not create backups; found {baks:?}"
    );
}

/// AC-2 / AC-4b: an existing hook file is backed up byte-identically beside
/// itself BEFORE the overwrite, and the backup path is printed.
#[test]
fn init_backs_up_existing_hook_bytes_before_overwrite() {
    let proj = scratch_git_project();
    // Pin hooks INSIDE the repo (absolute) so the host global hooksPath
    // cannot redirect the install.
    let hooks = proj.path().join(".git").join("hooks");
    let pin = hooks.display().to_string();
    git(proj.path(), &["config", "core.hooksPath", &pin]);
    seed_hook(&hooks.join("post-commit"), LEGACY_HOOK);

    let out = run_init(proj.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // Exactly one pre-image backup beside the hook.
    let mut baks: Vec<std::path::PathBuf> = fs::read_dir(&hooks)
        .expect("hooks dir")
        .map(|e| e.expect("entry").path())
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().starts_with("post-commit.bak-"))
                .unwrap_or(false)
        })
        .collect();
    baks.sort();
    assert_eq!(
        baks.len(),
        1,
        "exactly one pre-image backup expected; got {baks:?}"
    );

    // The backup holds the exact pre-image bytes.
    let backup_bytes = fs::read(&baks[0]).expect("read backup");
    assert_eq!(
        String::from_utf8_lossy(&backup_bytes),
        LEGACY_HOOK,
        "backup must be the byte-identical pre-image"
    );

    // The hook now carries the Hilo block appended after the legacy logic.
    let updated = fs::read_to_string(hooks.join("post-commit")).expect("read hook");
    assert!(
        updated.contains("### HILO"),
        "hook must gain the HILO block"
    );
    assert!(
        updated.contains("legacy-guard ran"),
        "pre-existing hook logic must survive the update"
    );

    // The backup path is printed (canonical AC: backup printed beside the hook).
    let backup_name = baks[0].file_name().unwrap().to_string_lossy().into_owned();
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("Saved previous hook") && combined.contains(&backup_name),
        "init must print the backup path ({backup_name}); stdout:\n{stdout}stderr:\n{stderr}"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(hooks.join("post-commit"))
            .expect("hook metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "rewritten hook must stay executable");
    }
}

/// AC-2 round-trip: a second `hilo init` keeps the whole pre-image chain
/// recoverable — the FIRST backup still holds the original bytes and the
/// second holds the intermediate state, with distinct stamps.
#[test]
fn init_update_round_trip_keeps_preimage_chain_recoverable() {
    let proj = scratch_git_project();
    let hooks = proj.path().join(".git").join("hooks");
    let pin = hooks.display().to_string();
    git(proj.path(), &["config", "core.hooksPath", &pin]);
    seed_hook(&hooks.join("post-commit"), LEGACY_HOOK);

    let _first = run_init(proj.path());
    // Backup names are stamped; sleep so the second run cannot share a stamp
    // with the first (defense against same-second clobber).
    std::thread::sleep(Duration::from_millis(1100));
    let _second = run_init(proj.path());

    let mut baks: Vec<std::path::PathBuf> = fs::read_dir(&hooks)
        .expect("hooks dir")
        .map(|e| e.expect("entry").path())
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().starts_with("post-commit.bak-"))
                .unwrap_or(false)
        })
        .collect();
    baks.sort();
    assert_eq!(
        baks.len(),
        2,
        "each overwrite must leave its own backup; got {baks:?}"
    );
    assert_ne!(
        fs::read(&baks[0]).expect("first backup"),
        fs::read(&baks[1]).expect("second backup"),
        "the two backups must capture different generations"
    );

    // Generation 1: the original legacy bytes survive both inits.
    assert_eq!(
        String::from_utf8_lossy(&fs::read(&baks[0]).expect("first backup")),
        LEGACY_HOOK,
        "the original pre-image must survive the init/update round-trip"
    );
    // Generation 2: the intermediate post-first-init content.
    let second_bytes = fs::read(&baks[1]).expect("second backup");
    let second = String::from_utf8_lossy(&second_bytes);
    assert!(
        second.contains("legacy-guard ran") && second.contains("### HILO"),
        "second backup must hold the intermediate (legacy + block) content"
    );

    // Idempotent update: exactly one Hilo block after two inits.
    let final_hook = fs::read_to_string(hooks.join("post-commit")).expect("read hook");
    assert_eq!(
        final_hook
            .matches("### HILO — auto-update metadata on commit")
            .count(),
        1,
        "re-running init must replace, not duplicate, the Hilo block"
    );
}
