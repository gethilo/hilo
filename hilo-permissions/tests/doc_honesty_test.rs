//! Documentation-honesty tests for the permissions surface.
//!
//! DF-WARPFS-31: docs/hilo-permissions.md previously claimed the engine is
//! "Used by FUSE for kernel-level enforcement and by the MCP server for agent
//! access control". That is false as shipped: FUSE builds its engine from
//! hardcoded `default_protections()` only (manifest `permissions.rules` are
//! parsed by hilo-core but never consumed), and hilo-mcp contains no
//! permission code. These tests pin the corrected wording so the false claims
//! cannot quietly reappear, following the same docs-as-contract pattern as
//! `hilo-mcp/tests/mcp_test.rs`.

use std::path::PathBuf;

/// Path to the repo-root `docs/` directory.
fn docs_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../docs")
}

/// The raw text of a docs file, with a descriptive panic on failure.
fn doc_text(name: &str) -> String {
    let path = docs_dir().join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

/// The false claim that FUSE and MCP consume this engine must stay gone.
#[test]
fn no_false_fuse_mcp_usage_claim() {
    let doc = doc_text("hilo-permissions.md");
    for banned in [
        "Used by FUSE",
        "MCP server for agent access control",
        "by the MCP server",
    ] {
        assert!(
            !doc.contains(banned),
            "docs/hilo-permissions.md regressed: found banned claim {banned:?} — \
             MCP permission enforcement is not implemented and FUSE uses \
             hardcoded default protections, do not claim otherwise"
        );
    }
}

/// The corrected wording must state the actual wiring: hardcoded defaults,
/// manifest rules not consumed by FUSE, MCP enforcement absent.
#[test]
fn enforcement_status_section_states_real_wiring() {
    let doc = doc_text("hilo-permissions.md").to_lowercase();
    for required in [
        "hardcoded default protections",
        "currently consumed by fuse",
        "mcp permission enforcement is not implemented",
    ] {
        assert!(
            doc.contains(required),
            "docs/hilo-permissions.md must contain {required:?} — the \
             enforcement-status section must state the real wiring"
        );
    }
}

/// docs/hilo-fuse.md must not say the FUSE `permissions` module derives mode
/// bits from manifest rules.
#[test]
fn fuse_doc_does_not_claim_manifest_rules_are_enforced() {
    let doc = doc_text("hilo-fuse.md");
    assert!(
        !doc.contains("mode bits from manifest rules"),
        "docs/hilo-fuse.md regressed: 'mode bits from manifest rules' is false \
         — FUSE enforcement uses hardcoded default protections only"
    );
    assert!(
        doc.contains("NOT consumed by FUSE"),
        "docs/hilo-fuse.md must state that manifest permissions.rules are not \
         consumed by FUSE"
    );
}
