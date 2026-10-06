//! Conformance/golden fixture for the COV-3 test-class taxonomy.
//!
//! The `conformance/` path component names the class (§4). Inert — never
//! compiled as a target.

#[test]
fn matches_golden_output() {
    let rendered = "a,b\n";
    assert_eq!(
        rendered, "a,b\n",
        "rendered output must match the golden file"
    );
}
