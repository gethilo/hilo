//! Integration tests for hilo_permissions — exercising the public API
//! from outside the crate boundary (no access to internal helpers).

use hilo_permissions::{
    default_protections, BackendPermissionRule, PermissionEngine, PermissionOp, PermissionRule,
};
use std::path::Path;

// ── Rule matching ──────────────────────────────────────────────────────────

#[test]
fn rule_matching_glob_patterns() {
    // Verifies glob-based rules match correctly at the integration level.
    let rules = vec![
        PermissionRule {
            paths: vec!["src/**".into()],
            mode: 0o644,
            allow_delete: true,
        },
        PermissionRule {
            paths: vec![".vfs/**".into()],
            mode: 0o444,
            allow_delete: false,
        },
    ];
    let engine = PermissionEngine::from_rules(rules);

    assert!(engine.check("src/main.rs", PermissionOp::Read).is_ok());
    assert!(engine.check("src/main.rs", PermissionOp::Write).is_ok());
    assert!(engine.check("src/lib.rs", PermissionOp::Write).is_ok());

    assert!(engine
        .check(".vfs/manifest.yaml", PermissionOp::Read)
        .is_ok());
    assert!(engine
        .check(".vfs/manifest.yaml", PermissionOp::Write)
        .is_err());
    assert!(engine
        .check(".vfs/deep/nested/file", PermissionOp::Write)
        .is_err());
}

#[test]
fn rule_ordering_first_match_wins() {
    // A broader pattern first should shadow a more specific one later.
    let rules = vec![
        PermissionRule {
            paths: vec!["**/*.rs".into()],
            mode: 0o444,
            allow_delete: false,
        },
        PermissionRule {
            paths: vec!["src/main.rs".into()],
            mode: 0o644,
            allow_delete: true,
        },
    ];
    let engine = PermissionEngine::from_rules(rules);

    // The first rule (**/*.rs → 0o444) matches before the second — so writes are denied.
    assert!(engine.check("src/main.rs", PermissionOp::Read).is_ok());
    assert!(engine.check("src/main.rs", PermissionOp::Write).is_err());
}

// ── Mode computation ───────────────────────────────────────────────────────

#[test]
fn mode_computation_with_backend_rules() {
    // Backend rules take priority over glob rules.
    let rules = vec![PermissionRule {
        paths: vec!["src/**".into()],
        mode: 0o644,
        allow_delete: true,
    }];
    let backends = vec![BackendPermissionRule {
        name: "readonly-dep".into(),
        mode: 0o444,
    }];
    let engine = PermissionEngine::new_with_backends(rules, backends, 0o644);

    // Backend matches by first path component.
    assert_eq!(
        engine.compute_mode(Path::new("readonly-dep/src/lib.rs")),
        0o444
    );
    assert_eq!(engine.compute_mode(Path::new("src/main.rs")), 0o644);
}

#[test]
fn mode_computation_defaults_to_provided_default() {
    let engine = PermissionEngine::new(vec![], 0o600);
    assert_eq!(engine.compute_mode(Path::new("any/path.rs")), 0o600);
}

// ── Deny-by-default ────────────────────────────────────────────────────────

#[test]
fn deny_by_default_no_rules_match() {
    // With no rules, the engine uses its default_mode (0o644).
    let engine = PermissionEngine::new(vec![], 0o644);
    assert!(engine.check("random.txt", PermissionOp::Read).is_ok());
    assert!(engine.check("random.txt", PermissionOp::Write).is_ok());
    assert!(engine.check("random.txt", PermissionOp::Execute).is_err());
}

#[test]
fn deny_explicit_no_access_rule() {
    let rules = vec![PermissionRule {
        paths: vec!["secrets/**".into()],
        mode: 0o000,
        allow_delete: false,
    }];
    let engine = PermissionEngine::from_rules(rules);

    let err = engine
        .check("secrets/key.pem", PermissionOp::Read)
        .unwrap_err();
    assert!(err.to_string().contains("permission denied"));
    assert!(err.to_string().contains("Read"));

    assert!(engine
        .check("secrets/key.pem", PermissionOp::Write)
        .is_err());
    assert!(engine
        .check("secrets/key.pem", PermissionOp::Execute)
        .is_err());
}

// ── Default protections ────────────────────────────────────────────────────

#[test]
fn default_protections_have_expected_count() {
    let rules = default_protections();
    // 10 infrastructure rules + 3 source-directory rules
    assert_eq!(rules.len(), 13);
}

#[test]
fn default_protections_block_write_to_vfs() {
    let rules = default_protections();
    let engine = PermissionEngine::from_rules(rules);

    assert!(engine
        .check(".vfs/manifest.yaml", PermissionOp::Read)
        .is_ok());
    assert!(engine
        .check(".vfs/manifest.yaml", PermissionOp::Write)
        .is_err());
}

#[test]
fn default_protections_allow_write_to_src() {
    let rules = default_protections();
    let engine = PermissionEngine::from_rules(rules);

    assert!(engine.check("src/main.rs", PermissionOp::Read).is_ok());
    assert!(engine.check("src/main.rs", PermissionOp::Write).is_ok());
}

#[test]
fn default_protections_deny_on_unmatched_path_uses_default() {
    // Paths that don't match any default_protections rule fall back to default 0o644.
    let rules = default_protections();
    let engine = PermissionEngine::from_rules(rules);

    assert_eq!(engine.compute_mode(Path::new("unmatched/file.txt")), 0o644);
    assert!(engine
        .check("unmatched/file.txt", PermissionOp::Read)
        .is_ok());
    assert!(engine
        .check("unmatched/file.txt", PermissionOp::Write)
        .is_ok());
}

// ── PermissionError display ────────────────────────────────────────────────

#[test]
fn permission_error_display_includes_path_and_op() {
    let rules = vec![PermissionRule {
        paths: vec!["restricted/**".into()],
        mode: 0o000,
        allow_delete: false,
    }];
    let engine = PermissionEngine::from_rules(rules);
    let err = engine
        .check("restricted/data.bin", PermissionOp::Write)
        .unwrap_err();

    let msg = err.to_string();
    assert!(msg.contains("restricted/data.bin"));
    assert!(msg.contains("Write"));
    assert!(msg.contains("permission denied"));
}

// ── Backend rule priority ──────────────────────────────────────────────────

#[test]
fn backend_rule_overrides_glob_rule_for_same_path() {
    let rules = vec![PermissionRule {
        paths: vec!["vendor/**".into()],
        mode: 0o644,
        allow_delete: true,
    }];
    let backends = vec![BackendPermissionRule {
        name: "vendor".into(),
        mode: 0o444,
    }];
    let engine = PermissionEngine::new_with_backends(rules, backends, 0o644);

    // Backend rule (0o444) overrides the glob rule (0o644)
    assert_eq!(engine.compute_mode(Path::new("vendor/lib.rs")), 0o444);
    assert!(engine.check("vendor/lib.rs", PermissionOp::Read).is_ok());
    assert!(engine.check("vendor/lib.rs", PermissionOp::Write).is_err());
}
