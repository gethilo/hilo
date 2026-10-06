//! Unit-test fixture for the COV-3 test-class taxonomy.
//!
//! This file is one of eight fixture files — one per test class — under
//! `hilo-graph/tests/fixtures/test_classes/`. It is deliberately inert: it is
//! never compiled as a cargo target (it lives in a nested fixture directory)
//! and exists only so `classify_test_file` has a real in-source unit test to
//! classify.

/// Production-looking surface the unit test below exercises.
pub fn spin() -> u32 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spins_once() {
        assert_eq!(spin(), 1);
    }
}
