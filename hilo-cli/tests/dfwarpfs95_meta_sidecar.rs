//! DF-WARPFS-95: `hilo meta` writes a git-tracked annotation sidecar and
//! `hilo meta apply` restores annotations after a fresh clone.
//!
//! Xattrs do not survive `git push`/`git clone`; `.vfs/annotations.jsonl`
//! does (`.vfs/` is committed). The acceptance chain exercised here, against
//! the real binary:
//!
//! set → sidecar line exists → xattr removed (clone simulation) →
//! list shows `(sidecar)` → apply → xattr restored → apply again = no-op.
//!
//! These tests need a filesystem with `user.*` xattr support (like the
//! existing hilo-metadata unit tests, which write real xattrs in CI).

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const BIN: &str = env!("CARGO_BIN_EXE_hilo");

const RELINK_RETRIES: usize = 3;
const RELINK_RETRY_SLEEP: Duration = Duration::from_secs(2);

/// Start building a `hilo` command (INT-GITREINS-004 relink-safe pattern
/// from `hilo-cli/tests/cli.rs`).
fn hilo_cmd() -> Command {
    Command::new(BIN)
}

/// Classify a spawn result as a concurrent-relink casualty.
fn spawn_hit_relink(out: &Output) -> bool {
    let empty = out.stdout.is_empty() && out.stderr.is_empty();
    empty && !out.status.success() && out.status.code().is_none()
}

/// Run `hilo` to completion, retrying concurrent-relink casualties.
fn run_hilo(cmd: &mut Command) -> Output {
    for attempt in 0..=RELINK_RETRIES {
        match cmd.output() {
            Ok(out) => {
                if attempt < RELINK_RETRIES && spawn_hit_relink(&out) {
                    std::thread::sleep(RELINK_RETRY_SLEEP);
                    continue;
                }
                return out;
            }
            Err(err) => panic!("failed to spawn hilo: {err}"),
        }
    }
    unreachable!("retry loop always returns or panics")
}

fn assert_ok(out: &Output, context: &str) -> String {
    assert!(
        out.status.success(),
        "{context} failed: rc={:?} stdout={} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Unique temp workspace dir (per-process + nanos, like cli.rs).
fn unique_tempdir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock moved backwards")
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("hilo-df95-{label}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&dir).expect("failed to create temp dir");
    dir
}

/// `hilo init --allow-home --no-hooks` inside `dir`.
fn init_workspace(dir: &Path) {
    let out = run_hilo(
        hilo_cmd()
            .arg("init")
            .arg("--allow-home")
            .arg("--no-hooks")
            .current_dir(dir),
    );
    assert_ok(&out, "hilo init");
}

/// Remove the raw `user.vfs.<attr>` xattr (the "fresh clone" simulation).
fn remove_xattr(path: &Path, attr: &str) {
    use std::ffi::CString;
    let c_path = CString::new(path.to_str().expect("non-utf8 path")).unwrap();
    let c_attr = CString::new(attr).unwrap();
    let rc = unsafe { libc::removexattr(c_path.as_ptr(), c_attr.as_ptr()) };
    assert_eq!(rc, 0, "removexattr({attr}) failed");
}

/// AC2 chain: set → sidecar recorded → xattr removed → list merges from
/// sidecar → apply restores the xattr → apply again is a no-op.
#[test]
fn sidecar_records_and_apply_restores_xattr_after_removal() {
    let ws = unique_tempdir("restore");
    init_workspace(&ws);
    let file = ws.join("app.rs");
    fs::write(&file, "fn main() {}\n").unwrap();

    // 1. Set with the path-positional-FIRST arg order.
    let out = run_hilo(
        hilo_cmd()
            .arg("meta")
            .arg("app.rs")
            .arg("--set")
            .arg("role")
            .arg("--value")
            .arg("entrypoint")
            .current_dir(&ws),
    );
    let stdout = assert_ok(&out, "meta --set");
    assert!(
        stdout.contains("Recorded in .vfs/annotations.jsonl"),
        "set output must confirm sidecar recording, got: {stdout}"
    );

    // 2. The sidecar exists with exactly the record, one compact JSON line.
    let sidecar = fs::read_to_string(ws.join(".vfs").join("annotations.jsonl"))
        .expect("sidecar must exist after set");
    assert_eq!(
        sidecar, "{\"rel_path\":\"app.rs\",\"attr\":\"role\",\"value\":\"entrypoint\"}\n",
        "sidecar must hold the record as compact JSONL"
    );

    // 3. Simulate a fresh clone: the xattr does not travel through git.
    remove_xattr(&file, "user.vfs.role");

    // 4. List merges the sidecar record, distinctly marked.
    let out = run_hilo(hilo_cmd().arg("meta").arg("app.rs").current_dir(&ws));
    let stdout = assert_ok(&out, "meta list (xattr missing)");
    assert!(
        stdout.contains("role: entrypoint (sidecar)"),
        "list must show the sidecar-only record marked (sidecar), got: {stdout}"
    );

    // 5. apply restores the xattr.
    let out = run_hilo(hilo_cmd().arg("meta").arg("apply").current_dir(&ws));
    let stdout = assert_ok(&out, "meta apply");
    assert!(
        stdout.contains("applied 1 of 1 records"),
        "apply summary wrong, got: {stdout}"
    );

    // 6. The value is back as a REAL xattr (no (sidecar) marker anymore).
    let out = run_hilo(hilo_cmd().arg("meta").arg("app.rs").current_dir(&ws));
    let stdout = assert_ok(&out, "meta list (after apply)");
    assert!(
        stdout.contains("role: entrypoint"),
        "xattr must be restored, got: {stdout}"
    );
    assert!(
        !stdout.contains("(sidecar)"),
        "after apply the record comes from the xattr, got: {stdout}"
    );

    // 7. Idempotent: a second apply applies nothing.
    let out = run_hilo(hilo_cmd().arg("meta").arg("apply").current_dir(&ws));
    let stdout = assert_ok(&out, "meta apply (second run)");
    assert!(
        stdout.contains("applied 0 of 1 records"),
        "second apply must be a no-op, got: {stdout}"
    );
}

/// A local xattr (any value) is never clobbered by apply.
#[test]
fn apply_never_clobbers_existing_xattr() {
    let ws = unique_tempdir("noclobber");
    init_workspace(&ws);
    let file = ws.join("app.rs");
    fs::write(&file, "fn main() {}\n").unwrap();

    run_hilo(
        hilo_cmd()
            .arg("meta")
            .arg("app.rs")
            .arg("--set")
            .arg("role")
            .arg("--value")
            .arg("recorded-value")
            .current_dir(&ws),
    );
    // Local edit diverges from the sidecar.
    run_hilo(
        hilo_cmd()
            .arg("meta")
            .arg("app.rs")
            .arg("--set")
            .arg("role")
            .arg("--value")
            .arg("local-edit")
            .current_dir(&ws),
    );

    let out = run_hilo(hilo_cmd().arg("meta").arg("apply").current_dir(&ws));
    let stdout = assert_ok(&out, "meta apply (local wins)");
    assert!(
        stdout.contains("applied 0 of 1 records"),
        "apply must not touch an existing xattr, got: {stdout}"
    );
    let out = run_hilo(hilo_cmd().arg("meta").arg("app.rs").current_dir(&ws));
    let stdout = assert_ok(&out, "meta list (local wins)");
    assert!(
        stdout.contains("role: local-edit"),
        "local xattr must survive apply, got: {stdout}"
    );
}

/// --dry-run reports without writing; the real apply then lands the value.
#[test]
fn dry_run_reports_without_writing() {
    let ws = unique_tempdir("dryrun");
    init_workspace(&ws);
    let file = ws.join("app.rs");
    fs::write(&file, "fn main() {}\n").unwrap();

    run_hilo(
        hilo_cmd()
            .arg("meta")
            .arg("app.rs")
            .arg("--set")
            .arg("role")
            .arg("--value")
            .arg("dry-check")
            .current_dir(&ws),
    );
    remove_xattr(&file, "user.vfs.role");

    let out = run_hilo(
        hilo_cmd()
            .arg("meta")
            .arg("apply")
            .arg("--dry-run")
            .current_dir(&ws),
    );
    let stdout = assert_ok(&out, "meta apply --dry-run");
    assert!(
        stdout.contains("would apply user.vfs.role on app.rs = dry-check"),
        "dry run must announce the record, got: {stdout}"
    );
    assert!(
        stdout.contains("would apply 1 of 1 records (dry run)"),
        "dry run summary wrong, got: {stdout}"
    );

    // The xattr is still absent — dry run wrote nothing (list still merges
    // from the sidecar).
    let out = run_hilo(hilo_cmd().arg("meta").arg("app.rs").current_dir(&ws));
    let stdout = assert_ok(&out, "meta list (after dry run)");
    assert!(
        stdout.contains("(sidecar)"),
        "dry run must not have written the xattr, got: {stdout}"
    );
}

/// Missing or empty sidecar is a normal state: exit 0, `no sidecar
/// annotations`.
#[test]
fn apply_without_sidecar_exits_zero() {
    let ws = unique_tempdir("nosidecar");
    init_workspace(&ws);
    let out = run_hilo(hilo_cmd().arg("meta").arg("apply").current_dir(&ws));
    let stdout = assert_ok(&out, "meta apply (no sidecar)");
    assert!(
        stdout.contains("no sidecar annotations"),
        "missing sidecar must print the calm summary, got: {stdout}"
    );
}

/// Outside any Hilo workspace the set is xattr-only: no sidecar line, no
/// `.vfs/` created, no "Recorded" confirmation.
#[test]
fn set_outside_workspace_is_xattr_only() {
    let dir = unique_tempdir("outside");
    fs::write(dir.join("out.rs"), "fn x() {}\n").unwrap();

    let out = run_hilo(
        hilo_cmd()
            .arg("meta")
            .arg("out.rs")
            .arg("--set")
            .arg("role")
            .arg("--value")
            .arg("external")
            .current_dir(&dir),
    );
    let stdout = assert_ok(&out, "meta --set outside workspace");
    assert!(
        stdout.contains("Set user.vfs.role = external"),
        "xattr write must still succeed, got: {stdout}"
    );
    assert!(
        !stdout.contains("Recorded"),
        "no sidecar confirmation outside a workspace, got: {stdout}"
    );
    assert!(
        !dir.join(".vfs").exists(),
        "no .vfs/ must be created outside a workspace"
    );
}
