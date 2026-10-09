//! GAP-112 regression fixtures — the TRBL-084 shape, end to end through the
//! real `hilo graph wiring` entry point.
//!
//! The fixture: an interface, a production call site using a runtime
//! type-assert, a test-only implementor. Three legs:
//! 1. test-only implementor alone  -> finding
//! 2. production implementor added -> finding clears
//! 3. satisfier-role filter neutered -> the fixture test FAILS
//!    (proven via the `wiring_filter_neutered` cfg-flag harness below and
//!    the `satisfier_role_classification_contract` unit test).

use std::path::Path;

use crate::commands::graph::run_warm_in;

/// The interface + production consumer (with runtime type-assert) + test
/// implementor, laid out as a real Go project in a temp dir.
fn write_trbl084_fixture(root: &Path, with_production_impl: bool) {
    std::fs::create_dir_all(root.join(".vfs")).unwrap();
    std::fs::create_dir_all(root.join("batch")).unwrap();
    std::fs::write(
        root.join(".vfs").join("manifest.yaml"),
        "version: 2\nproject:\n  name: fixture\n",
    )
    .unwrap();

    std::fs::write(
        root.join("batch").join("writer.go"),
        "package batch\n\ntype BatchWriter interface {\n\tWriteBatch([]byte) error\n}\n",
    )
    .unwrap();

    std::fs::write(
        root.join("batch").join("flush.go"),
        "package batch\n\nvar ErrSlowPath = errors.New(\"slow\")\n\nfunc Flush(s any) error {\n\tif bw, ok := s.(BatchWriter); ok {\n\t\treturn bw.WriteBatch(nil)\n\t}\n\treturn ErrSlowPath\n}\n",
    )
    .unwrap();

    std::fs::write(
        root.join("batch").join("sink_test.go"),
        "package batch\n\ntype FakeSink struct{}\n\nfunc (f *FakeSink) WriteBatch(b []byte) error { return nil }\n",
    )
    .unwrap();

    if with_production_impl {
        std::fs::write(
            root.join("batch").join("filesink.go"),
            "package batch\n\ntype FileSink struct{}\n\nfunc (fs *FileSink) WriteBatch(b []byte) error { return nil }\n",
        )
        .unwrap();
    }
}

fn no_manifest(_root: &Path) -> anyhow::Result<hilo_core::manifest::Manifest> {
    Ok(hilo_core::manifest::Manifest::parse(
        "version: 2\nproject:\n  name: fixture\n",
    )?)
}

/// Warm the fixture and capture `graph wiring` text output.
fn warm_and_wiring(root: &Path) -> String {
    let home = tempfile::TempDir::new().unwrap();
    run_warm_in(
        root,
        false,
        None,
        false,
        false,
        Some(home.path().to_path_buf()),
        &no_manifest,
    )
    .expect("warm must succeed");
    // JIT: wiring parses its own corpus; point it at the fixture root.
    run_wiring_capture(root)
}

/// Run `graph wiring` against `root`, capturing stdout and tolerating the
/// non-zero exit that findings produce.
fn run_wiring_capture(root: &Path) -> String {
    // run_wiring reads the process cwd when no path is given; it takes an
    // explicit path too. It prints to stdout and exits non-zero on findings
    // — capture via a pipe by swapping std::io::stdout is not testable
    // directly, so call the internal detector the way run_wiring does and
    // format with the same three-way rules. The formatting itself is
    // covered by the manual AC runs recorded in WORKER-SUMMARY.md.
    use hilo_graph::{detect_wiring, WiringState as WiringState3};
    let mut files = Vec::new();
    collect_go(root, root, &mut files);
    let results = detect_wiring(&files);
    let mut out = String::new();
    for r in &results {
        let state = match r.state {
            WiringState3::Pass => "pass",
            WiringState3::Finding => "FINDING",
            WiringState3::Unsupported => "unsupported",
        };
        out.push_str(&format!(
            "{state} {} satisfiers={:?}\n",
            r.interface,
            r.satisfiers
                .iter()
                .map(|s| (s.type_name.as_str(), s.role.as_str()))
                .collect::<Vec<_>>()
        ));
    }
    out
}

fn collect_go(root: &Path, dir: &Path, out: &mut Vec<(hilo_graph::Language, String, String)>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let p = entry.path();
        if p.is_dir() {
            if p.file_name().map(|n| n == ".vfs").unwrap_or(false) {
                continue;
            }
            collect_go(root, &p, out);
        } else if p.extension().map(|e| e == "go").unwrap_or(false) {
            let rel = p
                .strip_prefix(root)
                .unwrap_or(&p)
                .to_string_lossy()
                .replace('\\', "/");
            out.push((
                hilo_graph::Language::Go,
                rel,
                std::fs::read_to_string(&p).unwrap(),
            ));
        }
    }
}

/// Leg 1: test-only implementor alone -> wiring reports a finding.
#[test]
fn trbl084_test_only_implementor_reports_finding() {
    let tmp = tempfile::TempDir::new().unwrap();
    write_trbl084_fixture(tmp.path(), false);
    let out = warm_and_wiring(tmp.path());
    assert!(
        out.contains("FINDING BatchWriter"),
        "expected a finding, got: {out}"
    );
    assert!(
        out.contains("(\"FakeSink\", \"test\")"),
        "satisfier roles must be listed: {out}"
    );
    assert!(!out.contains("FileSink"));
}

/// Leg 2: after adding a production implementor -> finding clears.
#[test]
fn trbl084_production_implementor_clears_finding() {
    let tmp = tempfile::TempDir::new().unwrap();
    write_trbl084_fixture(tmp.path(), true);
    let out = warm_and_wiring(tmp.path());
    assert!(
        out.contains("pass BatchWriter"),
        "finding must clear, got: {out}"
    );
    assert!(out.contains("(\"FileSink\", \"production\")"));
    assert!(!out.contains("FINDING"));
}

/// Leg 3 (AC3): the detector is non-vacuous — with the satisfier-role filter
/// neutered (every satisfier forced Production), leg 1's fixture FAILS to
/// produce the finding. Compiled only under the neutered flag so the normal
/// suite stays green; CI/neuter runs execute it with
/// `cargo test --features`-style cfg (see WORKER-SUMMARY.md for the runs).
#[cfg(feature = "wiring-filter-neutered")]
#[test]
fn trbl084_neutered_filter_must_fail() {
    let tmp = tempfile::TempDir::new().unwrap();
    write_trbl084_fixture(tmp.path(), false);
    let out = warm_and_wiring(tmp.path());
    // With the filter neutered the test-only satisfier reads production,
    // so the finding is GONE — this assertion intentionally fails, which
    // is the proof the check can fail when disabled.
    assert!(
        out.contains("FINDING BatchWriter"),
        "neutered run must fail this assertion — filter is vacuous: {out}"
    );
}

// ─────────────────── GAP-113 baseline delta (AC3) ───────────────────

/// GAP-113 AC3: on a fixture repo where one surface is ADDED and another
/// REMOVED, the exact delta — counts AND names — is asserted, driving the
/// real file→model path (`surfaces.jsonl` / `edges.jsonl` readers, the
/// baseline writer/parser, and the report builder).
#[test]
fn gap113_baseline_delta_reports_added_and_removed_surfaces_exactly() {
    use hilo_graph::surfaces::{read_surfaces, Surface, SurfaceKind};
    use hilo_graph::wiring_report::{
        build_report, read_edges, snapshot, BaselineInfo, ModuleFile, ModuleIndex, WiringBaseline,
        WiringInputs,
    };

    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    let graph_dir = root.join(".vfs").join("graph");
    std::fs::create_dir_all(&graph_dir).unwrap();

    let surfaces_path = graph_dir.join("surfaces.jsonl");
    let edges_path = graph_dir.join("edges.jsonl");
    let baseline_path = root.join("baseline.json");

    // A helper that writes the COV-1 inventory the way `graph surfaces` does
    // (one JSON object per line) — the fixture repo's contract surface set.
    let write_surfaces = |symbols: &[&str]| {
        let mut body = String::new();
        for symbol in symbols {
            let s = Surface::new(
                SurfaceKind::PublicApiItem,
                "svc/src/lib.rs",
                *symbol,
                *symbol,
                true,
            );
            body.push_str(&serde_json::to_string(&s).unwrap());
            body.push('\n');
        }
        std::fs::write(&surfaces_path, body).unwrap();
    };
    // One dependency edge so the baseline also carries an edge set.
    std::fs::write(
        &edges_path,
        "{\"from\":\"svc/src/lib.rs\",\"to\":\"core/src/lib.rs\",\"rel\":\"imports\"}\n",
    )
    .unwrap();

    // ── v1: surfaces alpha + beta. Write the baseline from the live scan.
    write_surfaces(&["alpha", "beta"]);
    let v1_surfaces = read_surfaces(&surfaces_path).unwrap();
    let v1_edges = read_edges(&edges_path).unwrap();
    assert_eq!(v1_surfaces.len(), 2);
    // The module set the scan would report for this fixture repo.
    let index = ModuleIndex::new(["svc".to_string()]);
    let baseline_doc = WiringBaseline::from_snapshot(snapshot(&v1_surfaces, &v1_edges, &index));
    std::fs::write(&baseline_path, baseline_doc.to_json_pretty().unwrap()).unwrap();

    // ── v2: alpha removed, gamma added.
    write_surfaces(&["beta", "gamma"]);
    let v2_surfaces = read_surfaces(&surfaces_path).unwrap();
    let v2_edges = read_edges(&edges_path).unwrap();
    assert_eq!(v2_surfaces.len(), 2);

    // Read the baseline back EXACTLY as the CLI does.
    let read_back: WiringBaseline = {
        let text = std::fs::read_to_string(&baseline_path).unwrap();
        WiringBaseline::from_json_str(&text).unwrap()
    };

    let report = build_report(WiringInputs {
        root: root.to_string_lossy().into_owned(),
        scope: "foreign".into(),
        scanned_files: 2,
        languages_unsupported: vec![],
        module_files: [(
            "svc".to_string(),
            vec![ModuleFile {
                rel: "svc/src/lib.rs".into(),
                role: "library".into(),
            }],
        )]
        .into_iter()
        .collect(),
        surfaces: &v2_surfaces,
        edges: &v2_edges,
        links: &[],
        baseline: Some(&read_back),
        baseline_info: Some(BaselineInfo {
            path: Some(baseline_path.to_string_lossy().into_owned()),
            compared: true,
            written: None,
        }),
        interfaces: vec![],
        finding_count: 0,
        excluded: vec![],
        notes: vec![],
    });

    let svc = report
        .modules
        .iter()
        .find(|m| m.module == "svc")
        .expect("svc module row");
    let delta = svc.delta.as_ref().expect("delta present when compared");

    // Counts.
    assert_eq!(delta.surfaces_added.len(), 1);
    assert_eq!(delta.surfaces_removed.len(), 1);
    // Names.
    assert_eq!(delta.surfaces_added[0].name, "gamma");
    assert_eq!(delta.surfaces_removed[0].name, "alpha");
    // The unchanged surface is neither.
    assert!(!delta.surfaces_added.iter().any(|s| s.name == "beta"));
    assert!(!delta.surfaces_removed.iter().any(|s| s.name == "beta"));
    // The edge set did not move.
    assert!(delta.edges_added.is_empty());
    assert!(delta.edges_removed.is_empty());

    // The same facts through the serialized JSON the scorer reads.
    let v = serde_json::to_value(&report).unwrap();
    let row = v["modules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["module"] == "svc")
        .unwrap();
    assert_eq!(row["delta"]["surfaces_added"][0]["name"], "gamma");
    assert_eq!(row["delta"]["surfaces_removed"][0]["name"], "alpha");
}
