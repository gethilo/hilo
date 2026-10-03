//! DF-WARPFS-59: `hilo init` must not write a `plugins:` block to
//! `.vfs/manifest.yaml`.
//!
//! Plugin execution is NOT implemented — a fresh manifest must not advertise
//! a feature that never fires. The unit-level serialization assertion lives
//! in `hilo-core`; this test exercises the real `hilo init` binary
//! end-to-end (AC2).

use std::fs;
use std::process::Command;
use std::time::Duration;

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

fn run_init(dir: &std::path::Path) -> std::process::Output {
    let mut last_err: Option<std::io::Error> = None;
    for _ in 0..RELINK_RETRIES {
        let result = hilo_cmd()
            .arg("init")
            .arg("--allow-home")
            .arg("--no-hooks")
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

/// AC2: `hilo init` writes a manifest WITHOUT a `plugins:` block.
#[test]
fn init_manifest_has_no_plugins_block() {
    let tmp = std::env::temp_dir().join(format!("dfwarpfs59-init-{}", std::process::id()));
    fs::create_dir_all(&tmp).expect("temp project dir");

    run_init(&tmp);

    let manifest_path = tmp.join(".vfs").join("manifest.yaml");
    let manifest = fs::read_to_string(&manifest_path)
        .unwrap_or_else(|e| panic!("manifest must exist after init: {e}"));

    assert!(
        !manifest.contains("plugins"),
        "init-written manifest must not contain a plugins block; got:\n{manifest}"
    );
    assert!(
        manifest.contains("project:"),
        "sanity: manifest must still carry the project block; got:\n{manifest}"
    );
}
