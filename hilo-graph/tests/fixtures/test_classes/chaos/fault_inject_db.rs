//! Chaos/fault-injection fixture for the COV-3 test-class taxonomy.
//!
//! The `chaos/` path component names the `chaos_fault` class (§3). Inert —
//! never compiled as a target.

#[test]
fn survives_fault_inject() {
    // fault-injection harness placeholder: the DB is failed mid-write and the
    // test asserts the reader recovers.
    assert!(1 + 1 == 2);
}
