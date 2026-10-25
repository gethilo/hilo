//! DF-WARPFS-50 — `hilo workspace mount` accepts BOTH accepted manifest
//! schemas, and rejects anything else with a message that names the schema.
//!
//! The bug (dogfood run 14): `hilo workspace mount --manifest <P>` help said
//! "e.g., .vfs/manifest.yaml", but passing exactly that file — the manifest
//! `hilo init` writes — failed with `unknown field 'version'`, and spec §4's
//! own repo fields (`at:`, boolean `auto_pull:`) were rejected too. Nothing in
//! the error named a schema, so a new user had no way to find the real one.
//!
//! These tests drive the compiled binary, so they pin the real
//! `hilo init` → `hilo workspace mount` cycle and the schema-naming contract.
//! Everything here is filesystem-only: a manifest with no mountable source
//! never reaches FUSE, and the rejection paths fail before any clone.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Path to the compiled CLI binary, injected by Cargo at compile time.
const BIN: &str = env!("CARGO_BIN_EXE_hilo");

/// Retries after a concurrent-relink casualty (INT-GITREINS-004).
const RELINK_RETRIES: usize = 3;
/// Pause between spawn retries.
const RELINK_RETRY_SLEEP: Duration = Duration::from_secs(2);

fn hilo_cmd() -> Command {
    Command::new(BIN)
}

/// Concurrent-relink casualty classifier (INT-GITREINS-004, as in tests/cli.rs).
fn spawn_hit_relink(
    spawn_err: Option<&std::io::Error>,
    output: Option<&std::process::Output>,
) -> bool {
    if let Some(err) = spawn_err {
        if err.kind() == std::io::ErrorKind::NotFound || err.raw_os_error() == Some(26) {
            return true;
        }
    }
    if let Some(out) = output {
        let empty = out.stdout.is_empty() && out.stderr.is_empty();
        let instant_link_death = empty
            && (!out.status.success())
            && (out.status.code().is_none()
                || out.status.code() == Some(132)
                || out.status.code() == Some(134));
        if instant_link_death {
            return true;
        }
    }
    false
}

fn run_hilo_with_retry(cmd: &mut Command) -> Output {
    for attempt in 0..=RELINK_RETRIES {
        match cmd.output() {
            Ok(out) => {
                if attempt < RELINK_RETRIES && spawn_hit_relink(None, Some(&out)) {
                    std::thread::sleep(RELINK_RETRY_SLEEP);
                    continue;
                }
                return out;
            }
            Err(err) => {
                if attempt < RELINK_RETRIES && spawn_hit_relink(Some(&err), None) {
                    std::thread::sleep(RELINK_RETRY_SLEEP);
                    continue;
                }
                panic!("failed to spawn hilo: {err}");
            }
        }
    }
    unreachable!("retry loop always returns or panics")
}

/// Unique temporary directory (the `tempfile` crate is not a dependency here).
fn unique_tempdir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock moved backwards")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "hilo-dfwarpfs50-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("failed to create temp dir");
    dir
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Run `hilo <args>` in `dir`.
fn run_in(dir: &Path, args: &[&str]) -> Output {
    run_hilo_with_retry(hilo_cmd().args(args).current_dir(dir))
}

/// Run `hilo <args>` in `dir` with `HOME` pointed at `home`.
///
/// The worktree manager roots its clones at `$HOME/.hilo/worktrees`, so a test
/// that reaches the clone stage must not read or write the developer's real
/// `$HOME` — and must not pick up a leftover clone that would let the mount
/// proceed into FUSE.
fn run_in_with_home(dir: &Path, home: &Path, args: &[&str]) -> Output {
    run_hilo_with_retry(hilo_cmd().args(args).current_dir(dir).env("HOME", home))
}

/// Write `.vfs/manifest.yaml` in `dir` with the given body.
fn write_manifest(dir: &Path, body: &str) -> PathBuf {
    let vfs = dir.join(".vfs");
    fs::create_dir_all(&vfs).expect("failed to create .vfs");
    let path = vfs.join("manifest.yaml");
    fs::write(&path, body).expect("failed to write manifest");
    path
}

/// AC1 — the exact verification command from the task: `hilo init`, then
/// `hilo workspace mount --manifest .vfs/manifest.yaml`, must exit 0.
#[test]
fn init_manifest_is_accepted_unchanged_by_workspace_mount() {
    let dir = unique_tempdir("init-accepted");

    let init = run_in(&dir, &["init", "--no-hooks"]);
    assert!(
        init.status.success(),
        "hilo init failed: status {:?}\nstdout:\n{}\nstderr:\n{}",
        init.status.code(),
        stdout_of(&init),
        stderr_of(&init)
    );
    let manifest = dir.join(".vfs").join("manifest.yaml");
    assert!(
        manifest.is_file(),
        "hilo init must write .vfs/manifest.yaml"
    );
    let init_manifest_bytes = fs::read(&manifest).expect("read init manifest");

    // Exactly the file `hilo init` wrote, unchanged: no rewriting, no mount
    // point argument (an initialized project declares nothing to mount).
    let mount = run_in(
        &dir,
        &["workspace", "mount", "--manifest", ".vfs/manifest.yaml"],
    );
    assert!(
        mount.status.success(),
        "workspace mount rejected the manifest `hilo init` writes — status {:?}\n\
         stdout:\n{}\nstderr:\n{}",
        mount.status.code(),
        stdout_of(&mount),
        stderr_of(&mount)
    );

    let out = stdout_of(&mount);
    let err = stderr_of(&mount);
    assert!(
        out.contains("project schema"),
        "the run must say which schema it read: {out}"
    );
    assert!(
        out.contains("nothing to mount"),
        "an initialized-but-empty manifest must say it mounts nothing: {out}"
    );
    assert!(
        !err.contains("unknown field") && !err.contains("neither accepted schema"),
        "no schema error may be raised for the init manifest: {err}"
    );
    assert_eq!(
        fs::read(&manifest).expect("re-read init manifest"),
        init_manifest_bytes,
        "workspace mount must not rewrite the manifest"
    );
}

/// AC2 — a manifest that fits neither schema names both schemas, the rejected
/// field, what that field expected, and how to fix it.
#[test]
fn rejected_manifest_error_names_both_schemas_and_the_field() {
    let dir = unique_tempdir("schema-names");
    // Project-schema keys + a workspace-only `mounts:` block: valid in neither.
    write_manifest(
        &dir,
        "version: 2\nproject:\n  name: x\nmounts:\n  - source: a\n    at: /a\n",
    );

    let out = run_in(
        &dir,
        &["workspace", "mount", "--manifest", ".vfs/manifest.yaml"],
    );
    assert!(
        !out.status.success(),
        "a manifest matching neither schema must fail:\n{}",
        stdout_of(&out)
    );

    let err = stderr_of(&out);
    assert!(err.contains("workspace schema"), "names a schema: {err}");
    assert!(err.contains("project schema"), "names both schemas: {err}");
    assert!(err.contains("mounts"), "names the rejected field: {err}");
    assert!(err.contains("expected"), "states what was expected: {err}");
    assert!(err.contains("hint:"), "offers a fix: {err}");
}

/// AC2 (validation half) — a workspace-schema manifest whose repos are never
/// mounted reports the schema and the missing `mounts:` field instead of the
/// old bare "manifest has no mounts defined".
#[test]
fn repos_without_mounts_error_names_the_workspace_schema_and_the_field() {
    let dir = unique_tempdir("repos-no-mounts");
    write_manifest(
        &dir,
        "repos:\n  - name: shared-lib\n    url: git@github.com:org/shared-lib.git\n    ref: main\n",
    );

    let out = run_in(
        &dir,
        &[
            "workspace",
            "mount",
            "--manifest",
            ".vfs/manifest.yaml",
            "./mnt",
        ],
    );
    assert!(
        !out.status.success(),
        "repos[] alone mounts nothing and must be reported as an error"
    );

    let err = stderr_of(&out);
    assert!(err.contains("workspace schema"), "names the schema: {err}");
    assert!(err.contains("mounts"), "names the missing field: {err}");
    assert!(
        err.contains("source:") || err.contains("source"),
        "shows the expected mounts entry: {err}"
    );
    // The pre-fix message gave the user nothing to act on.
    assert!(
        !err.trim_end().ends_with("manifest has no mounts defined"),
        "the bare legacy message must be gone: {err}"
    );
}

/// AC2 — the spec §4 repo fields that used to be rejected in the workspace
/// schema (`at:`, boolean `auto_pull:`) now parse, validate and reach the
/// clone stage.
///
/// The URL does not resolve, so the run stops inside the plan builder — which
/// is exactly the point: getting there proves the manifest was accepted by the
/// workspace schema, that `repos[].at` produced a mount, and that validation
/// passed. No FUSE mount is attempted.
#[test]
fn workspace_schema_accepts_repo_at_and_boolean_auto_pull() {
    let dir = unique_tempdir("ws-at-auto-pull");
    let home = unique_tempdir("ws-at-auto-pull-home");
    write_manifest(
        &dir,
        "repos:\n  - name: shared-lib\n    url: /nonexistent/shared-lib.git\n    ref: main\n    \
         at: mnt/shared-lib\n    auto_pull: true\n",
    );

    let out = run_in_with_home(
        &dir,
        &home,
        &[
            "workspace",
            "mount",
            "--manifest",
            ".vfs/manifest.yaml",
            "./mnt",
        ],
    );

    let out_text = stdout_of(&out);
    let err_text = stderr_of(&out);
    let combined = format!("{out_text}{err_text}");

    // The workspace schema must be recognised, and neither of the two
    // dogfood-run-14 parse errors may reappear.
    assert!(
        out_text.contains("workspace schema"),
        "manifest must be read as the workspace schema: {combined}"
    );
    assert!(
        !combined.contains("unknown field `at`"),
        "`at:` must be accepted in the workspace schema: {combined}"
    );
    assert!(
        !combined.contains("invalid type: boolean"),
        "boolean `auto_pull` must be accepted: {combined}"
    );
    assert!(
        !combined.contains("neither accepted schema"),
        "the manifest must not be rejected as unknown: {combined}"
    );
    assert!(
        err_text.contains("failed to build mount plan"),
        "the run must reach the plan/clone stage (repos[].at mounted), \
         failing on the unresolvable URL instead of on the schema: {combined}"
    );
}
