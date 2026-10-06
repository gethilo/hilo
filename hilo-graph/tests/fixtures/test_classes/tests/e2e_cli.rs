//! End-to-end process fixture for the COV-3 test-class taxonomy.
//!
//! The file name carries `e2e` and the body spawns a child process, so both
//! the path rule (§5) and the `e2e_process` marker layer agree. Inert — never
//! compiled as a target.

#[test]
fn e2e_runs_the_binary() {
    let out = std::process::Command::new("true").output().unwrap();
    assert!(out.status.success());
}
