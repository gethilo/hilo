//! Integration tests for the tree-sitter Go and Python import parsers.

use hilo_graph::parser::{Language, Parser};
use std::path::Path;

/// Write `rel` under `dir` (creating parents) and return its absolute path.
///
/// `hilo graph warm` stores importer paths cwd-stripped; the resolver only
/// walks `__init__.py`-bounded parents, so an absolute path inside a
/// tempdir yields the same module name as the repo-relative one would —
/// without any `set_current_dir` (tests in this file run in parallel).
fn write_py(dir: &Path, rel: &str, content: &str) -> String {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, content).unwrap();
    path.to_string_lossy().into_owned()
}

#[test]
fn test_parse_simple_imports() {
    let source = r#"
package main
import "fmt"
import "os"
"#;
    let mut parser = Parser::for_language(Language::Go).unwrap();
    let edges = parser.parse_imports("src/main.go", source).unwrap();
    assert!(!edges.is_empty());
    // Should find at least "fmt" and "os"
    let tos: Vec<&str> = edges.iter().map(|e| e.to.as_str()).collect();
    assert!(tos.contains(&"std:fmt"));
    assert!(tos.contains(&"std:os"));
}

#[test]
fn test_parse_third_party_import() {
    let source = r#"
package foo
import "github.com/gin-gonic/gin"
"#;
    let mut parser = Parser::for_language(Language::Go).unwrap();
    let edges = parser.parse_imports("src/handler.go", source).unwrap();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].from, "src/handler.go");
    assert_eq!(edges[0].to, "pkg:github.com/gin-gonic/gin");
    assert_eq!(edges[0].rel, "imports");
}

#[test]
fn test_parse_empty_file() {
    let source = "package main\n";
    let mut parser = Parser::for_language(Language::Go).unwrap();
    let edges = parser.parse_imports("src/empty.go", source).unwrap();
    assert!(edges.is_empty());
}

#[test]
fn test_parse_import_block() {
    // A single import block with multiple specs, including an aliased import.
    let source = r#"
package main

import (
    "fmt"
    "os"

    "github.com/spf13/cobra"
)
"#;
    let mut parser = Parser::for_language(Language::Go).unwrap();
    let edges = parser.parse_imports("cmd/root.go", source).unwrap();
    let tos: Vec<&str> = edges.iter().map(|e| e.to.as_str()).collect();
    assert!(tos.contains(&"std:fmt"));
    assert!(tos.contains(&"std:os"));
    assert!(tos.contains(&"pkg:github.com/spf13/cobra"));
    // All edges share the same source file and relation.
    assert!(edges.iter().all(|e| e.from == "cmd/root.go"));
    assert!(edges.iter().all(|e| e.rel == "imports"));
}

// ── Python relative imports (GAP-076) ───────────────────────────────
//
// A relative import must emit BOTH nodes: the raw one as written
// (`pkg:.mod`) and the resolved absolute module (`pkg:pkg.mod`) that
// `PkgResolver::pkg_node` produces for the target file. Only then does
// `hilo graph impact <file>` find the files that import it.

#[test]
fn python_relative_import_emits_resolved_absolute_node() {
    let dir = tempfile::tempdir().unwrap();
    // Package layout: pkg/mod.py, pkg/sub/use.py — both dirs are packages.
    let init = write_py(dir.path(), "pkg/__init__.py", "from .mod import Thing\n");
    let use_py = write_py(dir.path(), "pkg/sub/use.py", "from ..mod import Thing\n");
    write_py(dir.path(), "pkg/mod.py", "class Thing: ...\n");
    write_py(dir.path(), "pkg/sub/__init__.py", "\n");

    let mut parser = Parser::for_language(Language::Python).unwrap();

    // `pkg/__init__.py`: its module IS the package, so `.mod` → pkg.mod.
    let edges = parser
        .parse_imports(&init, "from .mod import Thing\n")
        .unwrap();
    let tos: Vec<&str> = edges
        .iter()
        .filter(|e| e.rel == "imports")
        .map(|e| e.to.as_str())
        .collect();
    assert!(
        tos.contains(&"pkg:.mod"),
        "raw node must be kept, got {tos:?}"
    );
    assert!(
        tos.contains(&"pkg:pkg.mod"),
        "resolved absolute node missing, got {tos:?}"
    );

    // `pkg/sub/use.py` (`pkg.sub.use`): one dot climbs to `pkg` → pkg.mod.
    let edges = parser
        .parse_imports(&use_py, "from ..mod import Thing\n")
        .unwrap();
    let tos: Vec<&str> = edges
        .iter()
        .filter(|e| e.rel == "imports")
        .map(|e| e.to.as_str())
        .collect();
    assert!(
        tos.contains(&"pkg:..mod"),
        "raw node must be kept, got {tos:?}"
    );
    assert!(
        tos.contains(&"pkg:pkg.mod"),
        "resolved absolute node missing, got {tos:?}"
    );

    // `from . import sibling` — empty `rest` resolves to the package itself.
    let edges = parser
        .parse_imports(&use_py, "from . import sibling\n")
        .unwrap();
    let tos: Vec<&str> = edges
        .iter()
        .filter(|e| e.rel == "imports")
        .map(|e| e.to.as_str())
        .collect();
    assert!(tos.contains(&"pkg:."), "raw node must be kept, got {tos:?}");
    assert!(
        tos.contains(&"pkg:pkg.sub"),
        "resolved package node missing, got {tos:?}"
    );
}

#[test]
fn python_relative_import_in_standalone_file_emits_only_raw_node() {
    let dir = tempfile::tempdir().unwrap();
    // No `__init__.py` anywhere: the file is not a member of a package, so
    // there is no absolute module to resolve the dots against.
    let script = write_py(dir.path(), "script.py", "from .mod import Thing\n");

    let mut parser = Parser::for_language(Language::Python).unwrap();
    let edges = parser
        .parse_imports(&script, "from .mod import Thing\n")
        .unwrap();
    let tos: Vec<&str> = edges
        .iter()
        .filter(|e| e.rel == "imports")
        .map(|e| e.to.as_str())
        .collect();
    assert_eq!(
        tos,
        vec!["pkg:.mod"],
        "expected only the raw node, got {tos:?}"
    );
}

#[test]
fn python_absolute_import_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let init = write_py(dir.path(), "flask/__init__.py", "\n");
    write_py(dir.path(), "flask/config.py", "class Config: ...\n");

    let mut parser = Parser::for_language(Language::Python).unwrap();
    let edges = parser
        .parse_imports(&init, "import flask.config\n")
        .unwrap();
    let tos: Vec<&str> = edges
        .iter()
        .filter(|e| e.rel == "imports")
        .map(|e| e.to.as_str())
        .collect();
    // Exactly one edge: the absolute form must not be duplicated.
    assert_eq!(tos, vec!["pkg:flask.config"], "got {tos:?}");

    let edges = parser
        .parse_imports(&init, "from flask.config import Config\n")
        .unwrap();
    let tos: Vec<&str> = edges
        .iter()
        .filter(|e| e.rel == "imports")
        .map(|e| e.to.as_str())
        .collect();
    assert_eq!(tos, vec!["pkg:flask.config"], "got {tos:?}");
}

#[test]
fn python_relative_import_in_namespace_package_resolves_against_nearest_package() {
    // PEP 420: `pkg/ns/` has no `__init__.py` but is importable, so its files
    // are `pkg.ns.*` and their relative imports resolve against `pkg`.
    let dir = tempfile::tempdir().unwrap();
    write_py(dir.path(), "pkg/__init__.py", "\n");
    write_py(dir.path(), "pkg/config.py", "\n");
    let ns_mod = write_py(dir.path(), "pkg/ns/mod.py", "from ..config import Config\n");
    write_py(dir.path(), "pkg/ns/sibling.py", "\n");

    let mut parser = Parser::for_language(Language::Python).unwrap();

    let edges = parser
        .parse_imports(&ns_mod, "from ..config import Config\n")
        .unwrap();
    let tos: Vec<&str> = edges
        .iter()
        .filter(|e| e.rel == "imports")
        .map(|e| e.to.as_str())
        .collect();
    assert!(
        tos.contains(&"pkg:..config"),
        "raw node must be kept, got {tos:?}"
    );
    assert!(
        tos.contains(&"pkg:pkg.config"),
        "namespace file must resolve against its nearest package, got {tos:?}"
    );

    // A sibling inside the namespace directory stays namespace-qualified.
    let edges = parser
        .parse_imports(&ns_mod, "from .sibling import Thing\n")
        .unwrap();
    let tos: Vec<&str> = edges
        .iter()
        .filter(|e| e.rel == "imports")
        .map(|e| e.to.as_str())
        .collect();
    assert!(
        tos.contains(&"pkg:pkg.ns.sibling"),
        "namespace component must survive, got {tos:?}"
    );
}

#[test]
fn python_relative_import_without_any_package_ancestor_emits_only_raw_node() {
    // `ns/mod.py` with no `__init__.py` anywhere up the chain: there is no
    // enclosing package to resolve against, so nothing extra is emitted.
    let dir = tempfile::tempdir().unwrap();
    let mod_py = write_py(dir.path(), "ns/mod.py", "from ..config import Config\n");

    let mut parser = Parser::for_language(Language::Python).unwrap();
    let edges = parser
        .parse_imports(&mod_py, "from ..config import Config\n")
        .unwrap();
    let tos: Vec<&str> = edges
        .iter()
        .filter(|e| e.rel == "imports")
        .map(|e| e.to.as_str())
        .collect();
    assert_eq!(
        tos,
        vec!["pkg:..config"],
        "expected only the raw node, got {tos:?}"
    );
}

#[test]
fn python_relative_import_matches_flask_corpus_shape() {
    // The worked examples from the flask corpus; every resolved node here
    // must be one `python_module_for_file` also produces for the target.
    //
    // The fixture mirrors UPSTREAM flask exactly: `src/flask/sansio/` has no
    // `__init__.py` (PEP 420 namespace package, `git ls-files
    // src/flask/sansio/` in flask upstream lists only README.md, app.py,
    // blueprints.py, scaffold.py). Rows 3-5 below therefore exercise the
    // namespace fallback — without it those imports resolve to nothing and
    // `hilo graph impact src/flask/config.py` misses `sansio/app.py`.
    let dir = tempfile::tempdir().unwrap();
    write_py(dir.path(), "src/flask/__init__.py", "\n");
    write_py(dir.path(), "src/flask/config.py", "\n");
    write_py(dir.path(), "src/flask/globals.py", "\n");
    write_py(dir.path(), "src/flask/sansio/app.py", "\n");
    write_py(dir.path(), "src/flask/sansio/scaffold.py", "\n");
    write_py(dir.path(), "src/flask/json/__init__.py", "\n");
    write_py(dir.path(), "src/flask/json/tag.py", "\n");

    let cases: [(&str, &str, &str, &str); 6] = [
        (
            "src/flask/config.py",
            "from .sansio.app import App\n",
            "pkg:.sansio.app",
            "pkg:flask.sansio.app",
        ),
        (
            "src/flask/__init__.py",
            "from .config import Config\n",
            "pkg:.config",
            "pkg:flask.config",
        ),
        (
            "src/flask/sansio/app.py",
            "from ..config import Config\n",
            "pkg:..config",
            "pkg:flask.config",
        ),
        (
            "src/flask/sansio/app.py",
            "from .scaffold import Scaffold\n",
            "pkg:.scaffold",
            "pkg:flask.sansio.scaffold",
        ),
        (
            "src/flask/json/tag.py",
            "from ..json import dumps\n",
            "pkg:..json",
            "pkg:flask.json",
        ),
        (
            "src/flask/json/__init__.py",
            "from ..globals import current_app\n",
            "pkg:..globals",
            "pkg:flask.globals",
        ),
    ];

    let mut parser = Parser::for_language(Language::Python).unwrap();
    for (file, source, raw, resolved) in cases {
        let path = dir.path().join(file).to_string_lossy().into_owned();
        let edges = parser.parse_imports(&path, source).unwrap();
        let tos: Vec<&str> = edges
            .iter()
            .filter(|e| e.rel == "imports")
            .map(|e| e.to.as_str())
            .collect();
        assert!(tos.contains(&raw), "{file}: raw {raw} missing, got {tos:?}");
        assert!(
            tos.contains(&resolved),
            "{file}: resolved {resolved} missing, got {tos:?}"
        );
    }
}
