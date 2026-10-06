//! Documentation smoke-test fixture for the COV-3 test-class taxonomy.
//!
//! The file name carries `smoke`, so the class rule (§6) maps it to
//! `doc_smoke` even though it also lives in a `tests/` directory. Inert —
//! never compiled as a target.

#[test]
fn readme_quickstart_snippet_runs() {
    // Exercises the quickstart block from the README/docs verbatim.
    assert!(!env!("CARGO_PKG_NAME").is_empty());
}
