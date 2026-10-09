//! COV-4 — surface grouping + rollup, over a scratch workspace.
//!
//! The unit tests in `hilo_graph::rollup` pin the resolver and the model; this
//! integration test exercises the same public surface the CLI calls, on a
//! fixture repo that looks like a real Cargo workspace, and asserts the two
//! things a rollup must never get wrong:
//!
//! * the arithmetic closes (AC4) — every surface lands in exactly one group,
//!   so `sum(group.surface_count) == total_surfaces == surfaces.len()`;
//! * an un-annotated repo still groups, and SAYS which fallback it used (AC3).
//!
//! It also drives the real xattr → grouping path
//! (`hilo_metadata::xattr::group_annotations`) when the filesystem supports
//! `user.*` xattrs, so an annotation is proven to reach the report rather than
//! being simulated.

use std::collections::BTreeMap;
use std::path::Path;

use hilo_graph::rollup::{rollup, GroupBy, GroupSource, RollupReport};
use hilo_graph::surfaces::{Surface, SurfaceKind};
use hilo_metadata::xattr::get_vfs_xattr;
use hilo_metadata::xattr::{group_annotations, set_vfs_xattr, GroupAnnotations};

/// A two-crate workspace plus a manifest-less directory — the minimum shape
/// that exercises all three precedence levels at once.
fn fixture_workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for crate_dir in ["alpha", "beta"] {
        let root = dir.path().join(crate_dir);
        std::fs::create_dir_all(root.join("src/deep")).expect("mkdir");
        std::fs::write(
            root.join("Cargo.toml"),
            format!("[package]\nname = \"{crate_dir}\"\nversion = \"0.1.0\"\n"),
        )
        .expect("write manifest");
    }
    // No manifest above `loose/` → only the module prefix can group these.
    std::fs::create_dir_all(dir.path().join("loose/src")).expect("mkdir");
    dir
}

fn surface(kind: SurfaceKind, file: &str, name: &str) -> Surface {
    Surface::new(kind, file, name, name, true)
}

/// A rollup over `surfaces` with the real on-disk annotation reader.
fn rollup_with_disk_annotations(root: &Path, surfaces: &[Surface], by: GroupBy) -> RollupReport {
    let reader = |owner_file: &str| group_annotations(&root.join(owner_file));
    rollup(
        root,
        surfaces,
        &[],
        &reader,
        by,
        "absent: no coverage_links.jsonl — fixture exercises grouping only",
    )
}

fn assert_arithmetic_closes(report: &RollupReport) {
    let summed: usize = report.groups.iter().map(|g| g.surface_count).sum();
    assert_eq!(
        summed,
        report.total_surfaces,
        "sum of group sizes must equal the total (no double count, no drop): {:?}",
        report
            .groups
            .iter()
            .map(|g| (&g.group, g.surface_count))
            .collect::<Vec<_>>()
    );
    assert_eq!(summed, report.surfaces.len());

    // Exactly one group per surface, and the group rows are disjoint.
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for row in &report.surfaces {
        *seen.entry(row.group.clone()).or_default() += 1;
    }
    for group in &report.groups {
        assert_eq!(
            seen.get(&group.group).copied().unwrap_or(0),
            group.surface_count,
            "group '{}' count must match its surface rows",
            group.group
        );
        assert_eq!(
            group.surface_count,
            group.covered + group.uncovered,
            "covered + uncovered must account for every member of '{}'",
            group.group
        );
    }
}

#[test]
fn unannotated_workspace_groups_by_crate_and_states_the_fallback() {
    let dir = fixture_workspace();
    let surfaces = vec![
        surface(SurfaceKind::McpTool, "alpha/src/lib.rs", "alpha_lib"),
        surface(
            SurfaceKind::McpTool,
            "alpha/src/deep/thing.rs",
            "alpha_deep",
        ),
        surface(SurfaceKind::CliVerb, "beta/src/cli.rs", "beta_cli"),
        surface(SurfaceKind::FfiExport, "loose/src/ffi.rs", "loose_ffi"),
    ];

    let report = rollup_with_disk_annotations(dir.path(), &surfaces, GroupBy::Feature);

    // AC3: never empty, and the fallback is named.
    assert_eq!(report.group_count, 3, "{:?}", report.groups);
    assert!(
        report
            .fallback_note
            .contains("structural fallback used for 4 of 4"),
        "{}",
        report.fallback_note
    );
    assert!(report.surfaces.iter().all(|s| !s.source.is_explicit()));

    // AC1: the chosen source is on the row — crate where a manifest exists,
    // module where none does.
    let source_of = |name: &str| {
        report
            .surfaces
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("no surface row named {name}: {:?}", report.surfaces))
            .clone()
    };
    let alpha = source_of("alpha_lib");
    assert_eq!(
        (alpha.group.as_str(), alpha.source),
        ("alpha", GroupSource::Crate)
    );
    let loose = source_of("loose_ffi");
    assert_eq!(
        (loose.group.as_str(), loose.source),
        ("loose", GroupSource::Module)
    );

    // AC4: the arithmetic closes.
    assert_arithmetic_closes(&report);
    assert_eq!(report.total_surfaces, 4);

    // The class mix names every class even though nothing is covered.
    let alpha_group = report.groups.iter().find(|g| g.group == "alpha").unwrap();
    assert_eq!(alpha_group.class_mix.len(), 8);
    assert!(alpha_group.class_mix.iter().all(|c| c.surfaces == 0));
    assert_eq!(alpha_group.uncovered, 2);
    assert_eq!(alpha_group.class_gap_count, 0);
}

#[test]
fn module_dimension_regroups_the_same_surfaces_without_losing_any() {
    let dir = fixture_workspace();
    let surfaces = vec![
        surface(SurfaceKind::McpTool, "alpha/src/lib.rs", "alpha_lib"),
        surface(
            SurfaceKind::McpTool,
            "alpha/src/deep/thing.rs",
            "alpha_deep",
        ),
        surface(SurfaceKind::CliVerb, "beta/src/cli.rs", "beta_cli"),
        surface(SurfaceKind::FfiExport, "loose/src/ffi.rs", "loose_ffi"),
    ];

    let by_crate = rollup_with_disk_annotations(dir.path(), &surfaces, GroupBy::Crate);
    let by_module = rollup_with_disk_annotations(dir.path(), &surfaces, GroupBy::Module);

    // Same surfaces, different partition: the module dimension splits
    // `alpha/deep` out of `alpha`.
    assert_eq!(by_crate.group_count, 3);
    assert_eq!(by_module.group_count, 4);
    let module_groups: Vec<&str> = by_module.groups.iter().map(|g| g.group.as_str()).collect();
    assert!(module_groups.contains(&"alpha::deep"), "{module_groups:?}");
    assert!(module_groups.contains(&"beta"), "{module_groups:?}");

    for report in [&by_crate, &by_module] {
        assert_arithmetic_closes(report);
        assert_eq!(report.total_surfaces, 4);
    }
}

#[test]
fn a_named_group_answers_with_its_own_gap_list() {
    let dir = fixture_workspace();
    let surfaces = vec![
        surface(SurfaceKind::McpTool, "alpha/src/lib.rs", "alpha_lib"),
        surface(
            SurfaceKind::McpTool,
            "alpha/src/deep/thing.rs",
            "alpha_deep",
        ),
        surface(SurfaceKind::CliVerb, "beta/src/cli.rs", "beta_cli"),
    ];

    let mut report = rollup_with_disk_annotations(dir.path(), &surfaces, GroupBy::Feature);
    assert!(report.retain_group("alpha"));

    assert_eq!(report.group_filter.as_deref(), Some("alpha"));
    assert_eq!(report.groups.len(), 1);
    assert_eq!(report.surfaces.len(), 2);
    // The whole-rollup totals survive the restriction, and the restricted
    // report's own arithmetic still closes.
    assert_eq!(report.total_surfaces, 3);
    assert_eq!(report.group_count, 2);
    let g = &report.groups[0];
    assert_eq!(g.surface_count, 2);
    assert_eq!(g.uncovered, 2);
    assert_eq!(g.uncovered_names, vec!["alpha_deep", "alpha_lib"]);
    assert_eq!(g.class_gap_count, 0);
    let summed: usize = report.groups.iter().map(|x| x.surface_count).sum();
    assert_eq!(summed, report.surfaces.len());

    // An unknown group is a miss that changes nothing.
    assert!(!report.retain_group("gamma"));
    assert_eq!(report.groups.len(), 1);
}

/// The real annotation path: an xattr written to a fixture file must reach the
/// group name AND be recorded as an explicit source. Skipped (not silently
/// passed) when the filesystem cannot carry `user.*` xattrs.
#[test]
fn explicit_xattr_annotation_outranks_the_crate_boundary() {
    let dir = fixture_workspace();
    let annotated = dir.path().join("alpha/src/lib.rs");
    if set_vfs_xattr(&annotated, "feature", "workspace-mount").is_err() {
        eprintln!("SKIP: filesystem does not support user.* xattrs");
        return;
    }
    // Sanity: the value is really readable back through the same reader the
    // CLI uses (a write that silently no-ops would make the rest vacuous).
    assert_eq!(
        get_vfs_xattr(&annotated, "feature").unwrap().as_deref(),
        Some("workspace-mount")
    );

    let surfaces = vec![
        surface(SurfaceKind::McpTool, "alpha/src/lib.rs", "alpha_lib"),
        surface(
            SurfaceKind::McpTool,
            "alpha/src/deep/thing.rs",
            "alpha_deep",
        ),
        surface(SurfaceKind::CliVerb, "beta/src/cli.rs", "beta_cli"),
    ];
    let report = rollup_with_disk_annotations(dir.path(), &surfaces, GroupBy::Crate);

    let row = report
        .surfaces
        .iter()
        .find(|s| s.name == "alpha_lib")
        .expect("annotated surface row");
    assert_eq!(
        (row.group.as_str(), row.source),
        ("workspace-mount", GroupSource::XattrFeature),
        "an explicit annotation must outrank the crate boundary"
    );
    // The sibling file in the same crate was NOT annotated, so it keeps the
    // structural group — the report carries both provenances at once.
    let sibling = report
        .surfaces
        .iter()
        .find(|s| s.name == "alpha_deep")
        .unwrap();
    assert_eq!(
        (sibling.group.as_str(), sibling.source),
        ("alpha", GroupSource::Crate)
    );

    assert_eq!(report.group_count, 3, "{:?}", report.groups);
    assert!(
        report
            .fallback_note
            .contains("structural fallback used for 2 of 3"),
        "{}",
        report.fallback_note
    );
    let explicit_row = report
        .source_census
        .iter()
        .find(|r| r.source == GroupSource::XattrFeature)
        .expect("the census names the explicit source");
    assert_eq!(explicit_row.count, 1);
    assert_arithmetic_closes(&report);
}

/// The component annotation is the second, finer explicit level: consulted
/// only when no feature is set, and never fabricated from nothing.
#[test]
fn component_annotation_is_used_only_without_a_feature() {
    let dir = fixture_workspace();
    let file = dir.path().join("alpha/src/lib.rs");
    if set_vfs_xattr(&file, "component", "backends").is_err() {
        eprintln!("SKIP: filesystem does not support user.* xattrs");
        return;
    }
    let surfaces = vec![surface(
        SurfaceKind::McpTool,
        "alpha/src/lib.rs",
        "alpha_lib",
    )];
    let report = rollup_with_disk_annotations(dir.path(), &surfaces, GroupBy::Feature);
    assert_eq!(
        (report.surfaces[0].group.as_str(), report.surfaces[0].source),
        ("backends", GroupSource::XattrComponent)
    );

    // And an un-annotated file in the same tree is still un-annotated.
    let other = dir.path().join("beta/src/cli.rs");
    assert_eq!(group_annotations(&other), GroupAnnotations::default());
}
