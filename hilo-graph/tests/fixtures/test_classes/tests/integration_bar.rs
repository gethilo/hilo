//! Integration-test fixture for the COV-3 test-class taxonomy.
//!
//! Lives in a `tests/` directory, so the class rule (§7 of the documented
//! precedence) maps it to `integration`. Inert — never compiled as a target.

#[test]
fn bar_round_trips() {
    assert_eq!(2 + 2, 4);
}
