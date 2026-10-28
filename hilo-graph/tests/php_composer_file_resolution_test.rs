//! DF-WARPFS-84: PHP composer-shaped file→package equivalence for impact.
//!
//! The board row's PASS criterion measures the file form against a warmed
//! composer fixture: `hilo graph impact src/Legacy/PriceMapper.php` must
//! return the same dependent-id set as the pkg form for PriceMapper. This
//! file mirrors that corpus shape (PSR-4 `src/` tree, `namespace` +
//! `use`-qualified references, the Solver/PriceMapper dependency shape from
//! the DF-WARPFS-78-era dogfood) and proves file-form == pkg-form through
//! the PUBLIC impact surface with real parser-emitted edges.
//!
//! The base chain landed with DF-WARPFS-82 (commit 87507216,
//! `php_file_resolution_test.rs` — same-namespace inheritance); this file
//! adds the cross-namespace `use`-import shape the composer measurement
//! used, plus the on-disk anchored-root equivalence assertion.

use hilo_graph::graph::GraphDB;
use hilo_graph::impact::compute_impact_at;
use hilo_graph::parser::{Language, Parser};
use hilo_graph::resolution::{AnchoredRoot, PkgResolver};
use hilo_metadata::inventory::Edge;

const SOLVER_FILE: &str = "src/Composer/DependencyResolver/Solver.php";
const SOLVER_PKG: &str = "pkg:Composer\\DependencyResolver\\Solver";
const PRICEMAPPER_FILE: &str = "src/Legacy/PriceMapper.php";
const PRICEMAPPER_PKG: &str = "pkg:Legacy\\PriceMapper";

/// Parse `source` as PHP and return the edges the parser emits for `file`.
fn php_edges(file: &str, source: &str) -> Vec<Edge> {
    Parser::for_language(Language::Php)
        .expect("PHP parser should build")
        .parse_imports(file, source)
        .expect("PHP parse should succeed")
}

/// A composer-shaped corpus on disk: PSR-4 `src/` tree where a
/// cross-namespace importer (`use`-qualified) references the Solver class
/// and two same-namespace derived classes extend PriceMapper — the two edge
/// shapes the PHP extractor emits. Sources are written to the tempdir so the
/// anchored file-form query reads the real declarations.
fn composer_fixture() -> (tempfile::TempDir, GraphDB) {
    let root = tempfile::tempdir().unwrap();
    let graph_path = root.path().join(".vfs/graph/graph.db");
    std::fs::create_dir_all(graph_path.parent().unwrap()).unwrap();

    let solver_source = "<?php\nnamespace Composer\\DependencyResolver;\n\nclass Solver {}\n";
    let loader_source =
        "<?php\nnamespace Composer\\Package;\nuse Composer\\DependencyResolver\\Solver;\n\nclass Loader {\n    public function run(Solver $s): void {}\n}\n";
    let pricemapper_source = "<?php\nnamespace Legacy;\n\nclass PriceMapper {}\n";
    let mapper_user_source =
        "<?php\nnamespace Legacy;\n\nclass PriceMapperUser extends PriceMapper {}\n";
    let legacy_user_source =
        "<?php\nnamespace App\\Pricing;\nuse Legacy\\PriceMapper;\n\nclass Calculator {\n    public function map(PriceMapper $m): array { return []; }\n}\n";

    let mut edges = Vec::new();
    for (file, source) in [
        (SOLVER_FILE, solver_source),
        ("src/Composer/Package/Loader.php", loader_source),
        (PRICEMAPPER_FILE, pricemapper_source),
        ("src/Legacy/PriceMapperUser.php", mapper_user_source),
        ("src/App/Pricing/Calculator.php", legacy_user_source),
    ] {
        let path = root.path().join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
        edges.extend(php_edges(file, source));
    }

    let graph = GraphDB::open(graph_path.to_str().unwrap()).unwrap();
    graph.insert_edges(&edges).unwrap();
    (root, graph)
}

/// AC: on the warmed composer fixture, the file form and the pkg form return
/// the identical dependent-id set — for the cross-namespace `use` shape
/// (Solver) and the same-namespace inheritance shape (PriceMapper).
#[test]
fn php_composer_file_form_impact_matches_pkg_form_set() {
    let (root_dir, graph) = composer_fixture();
    let anchored = AnchoredRoot::new(root_dir.path());

    for (file_form, pkg_form) in [
        (SOLVER_FILE, SOLVER_PKG),
        (PRICEMAPPER_FILE, PRICEMAPPER_PKG),
    ] {
        let pkg = compute_impact_at(graph.conn(), pkg_form, 5, None)
            .unwrap_or_else(|e| panic!("pkg-form {pkg_form} should query: {e}"));
        let file = compute_impact_at(graph.conn(), file_form, 5, Some(&anchored))
            .unwrap_or_else(|e| panic!("file-form {file_form} should query: {e}"));

        let mut pkg_ids: Vec<String> = pkg.iter().map(|r| r.path.clone()).collect();
        let mut file_ids: Vec<String> = file.iter().map(|r| r.path.clone()).collect();
        pkg_ids.sort();
        file_ids.sort();
        assert_eq!(
            pkg_ids, file_ids,
            "file-form {file_form} must return the same dependent-id set as pkg-form {pkg_form}"
        );
        // The set must be non-empty: the fixture has a real dependent for
        // each subject, so an empty-vs-empty pass would be vacuous.
        assert!(
            !pkg_ids.is_empty(),
            "{pkg_form} has real dependents in the fixture; an empty set means the fixture regressed"
        );
    }
}

/// The anchored resolver names exactly the node the importers' edges target:
/// `php_class_for_file` on the on-disk composer fixture resolves the
/// `namespace X; class Y` declaration to `pkg:X\Y`.
#[test]
fn php_composer_files_resolve_to_their_declared_class_node() {
    let (root_dir, _graph) = composer_fixture();
    let root = AnchoredRoot::new(root_dir.path());
    let mut resolver = PkgResolver::new();
    assert_eq!(
        resolver.pkg_node_at(&root, SOLVER_FILE).as_deref(),
        Some(SOLVER_PKG),
        "the anchored file resolution must name the pkg-form node"
    );
    assert_eq!(
        resolver.pkg_node_at(&root, PRICEMAPPER_FILE).as_deref(),
        Some(PRICEMAPPER_PKG),
        "the anchored file resolution must name the pkg-form node"
    );
}
