//! Fixture crate: one referenced function, one unreferenced function.

/// The referenced function (see `tests/used.rs`).
pub fn used() {}

/// The unreferenced function.
pub fn never_called() {}
