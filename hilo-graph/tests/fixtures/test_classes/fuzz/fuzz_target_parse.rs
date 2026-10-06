//! Fuzz-target fixture for the COV-3 test-class taxonomy.
//!
//! The `fuzz/` path component and the `fuzz_target!` marker both name the
//! `property_fuzz` class (§2). Inert — never compiled as a target.

fuzz_target!(|data: &[u8]| {
    let _ = data.len();
});
