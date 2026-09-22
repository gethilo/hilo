//! Build metadata for `hilo --version` — provenance, not just the crate number.
//!
//! Fleet review item 47 (2026-09-22): the installed `hilo` printed `0.3.0`
//! both for an 11-day-old artifact and for a HEAD build 11 commits past the
//! `v0.3.0` tag. A binary that cannot identify its own provenance is how a
//! fleet ends up testing the wrong thing. This script stamps the git
//! describe string and the build time; `hilo --main.rs long_version` prints
//! them. All values fall back to `unknown` so tarball/reproducible builds
//! without a `.git` directory still compile.

use std::process::Command;

fn main() {
    // Re-run when HEAD moves: the HEAD file alone does not change content on
    // a new commit to a branch, so track the resolved ref file too. If the
    // ref is packed away (rare here), the next change to any tracked input
    // refreshes the stamp — a worst-case slightly stale describe, never a
    // wrong one once rebuilt.
    println!("cargo:rerun-if-changed=.git/HEAD");
    if let Ok(head) = std::fs::read_to_string(".git/HEAD") {
        let head = head.trim();
        if let Some(refpath) = head.strip_prefix("ref: ") {
            println!("cargo:rerun-if-changed=.git/{refpath}");
        }
    }

    let describe = Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // Short commit sha = last dash-separated component of the describe
    // ("v0.3.0-11-gc90d2f6" -> "c90d2f6"; a bare sha describe is itself).
    let commit = describe
        .rsplit('-')
        .next()
        .unwrap_or("unknown")
        .trim_start_matches('g')
        .to_string();

    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    println!("cargo:rustc-env=HILO_BUILD_DESCRIBE={describe}");
    println!("cargo:rustc-env=HILO_BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=HILO_BUILD_TIME_UTC={}", iso8601_utc(secs));
}

/// Unix seconds -> `YYYY-MM-DDTHH:MM:SSZ` (no chrono dependency; Howard
/// Hinnant's civil-from-days algorithm).
fn iso8601_utc(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Shift epoch from 1970-01-01 to 0000-03-01 (the algorithm's era base).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

#[cfg(test)]
mod tests {
    use super::iso8601_utc;

    #[test]
    fn epoch() {
        assert_eq!(iso8601_utc(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn known_instants() {
        // 2026-09-22T21:30:00Z == 1790112600
        assert_eq!(iso8601_utc(1_790_112_600), "2026-09-22T21:30:00Z");
        // Leap-year boundary: 2024-02-29T12:00:00Z == 1709208000
        assert_eq!(iso8601_utc(1_709_208_000), "2024-02-29T12:00:00Z");
    }
}
