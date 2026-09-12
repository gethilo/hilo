//! Query-time resolution layer — map file paths to the `pkg:` graph nodes
//! they belong to (GAP-034).
//!
//! The parser emits edges that target `pkg:<name>` pseudo-nodes (crate
//! names) rather than file paths, so a file-level query like
//! `hilo graph impact crates/globset/src/lib.rs` finds nothing unless the
//! file is resolved to its crate's `pkg:` node first. This module walks up
//! from a file to the nearest `Cargo.toml` with a `[package] name` and
//! derives the `pkg:<name>` node the file belongs to.
//!
//! Go sources resolve the same way but to an import path, not a crate name
//! (GAP-057): the Go parser emits `pkg:<full import path>` edges, so a `.go`
//! file resolves to `pkg:<module>/<dir relative to go.mod>` via a walk up to
//! the nearest `go.mod` (see [`go_package_for_file`]).
//!
//! Python sources resolve to their dotted module path (GAP-064): the Python
//! parser emits `pkg:<dotted module>` edges for `import`/`from ... import`
//! statements (including stdlib modules, which are also `pkg:` nodes), so a
//! `.py` file must resolve to the module it *is* — `fastapi/routing.py`
//! under a regular package (`fastapi/__init__.py`) is `pkg:fastapi.routing`,
//! and an `__init__.py` is the package itself (`pkg:fastapi`). See
//! [`python_module_for_file`].
//!
//! Resolution is query-time only: the canonical `edges.jsonl` and the
//! DuckDB cache are untouched. Files that belong to no package (or are not
//! files at all — `pkg:`/`sys:`/`std:`/`external:` symbol nodes) resolve to
//! `None`.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

/// Resolves file paths to their `pkg:` nodes, caching results per query.
///
/// A single resolver instance should be created per top-level query (impact
/// BFS, related lookup) so repeated visits to the same path cost one
/// filesystem walk at most.
#[derive(Default)]
pub struct PkgResolver {
    cache: HashMap<PathBuf, Option<String>>,
    /// Go package nodes (`pkg:<import path>`), cached separately from the
    /// crate-name cache because a `.go` path and a `.rs` path resolve
    /// through different walks.
    go_cache: HashMap<PathBuf, Option<String>>,
    /// Python module nodes (`pkg:<dotted module>`), cached separately for
    /// the same reason: a `.py` path resolves through the `__init__.py`
    /// package walk, never through Cargo or Go.
    py_cache: HashMap<PathBuf, Option<String>>,
}

impl PkgResolver {
    /// Create an empty resolver.
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve `path` to its `pkg:` node, if the path is a file that belongs
    /// to a Cargo package, a Go module, or a Python package.
    ///
    /// `.go` sources resolve through the `go.mod` walk only (GAP-057) — that
    /// is the node the Go parser emits edges to. The Rust crate walk is
    /// tried for every other path and its behavior is unchanged, so only Go
    /// files opt into the module walk. A `.go` file with no `go.mod` above
    /// it resolves to `None` rather than falling back to the enclosing Rust
    /// crate: a Go source is never a Cargo package member.
    ///
    /// `.py` sources likewise resolve through the `__init__.py` package walk
    /// only (GAP-064): the Python parser emits `pkg:<dotted module>` edges,
    /// so a Python file must never fall back to the enclosing Cargo crate
    /// (that would match crate-level edges the parser never emitted for it).
    /// A standalone `.py` file outside any package resolves to `None`.
    ///
    /// Symbol-node paths (`pkg:...`, `sys:...`, `std:...`, `external:...`)
    /// are not files and always resolve to `None` — they must never trigger
    /// a filesystem walk (a path like `pkg:globset` would otherwise be
    /// interpreted relative to the current directory).
    pub fn pkg_node(&mut self, path: &str) -> Option<String> {
        if is_symbol_node(path) {
            return None;
        }
        if path.ends_with(".go") {
            return self.go_pkg_node(path);
        }
        if path.ends_with(".py") {
            return self.py_pkg_node(path);
        }
        self.crate_name(path).map(|name| format!("pkg:{name}"))
    }

    /// Resolve a `.go` path to its `pkg:<import path>` node, if the file
    /// lives under a Go module. Non-Go paths and symbol nodes return `None`
    /// without touching the filesystem; a `.go` file with no `go.mod`
    /// ancestor also returns `None`.
    fn go_pkg_node(&mut self, path: &str) -> Option<String> {
        if is_symbol_node(path) || !path.ends_with(".go") {
            return None;
        }
        let p = PathBuf::from(path);
        if let Some(hit) = self.go_cache.get(&p) {
            return hit.clone();
        }
        let node = go_package_for_file(&p).map(|import_path| format!("pkg:{import_path}"));
        self.go_cache.insert(p, node.clone());
        node
    }

    /// Resolve a `.py` path to its `pkg:<dotted module>` node, if the file
    /// lives inside a regular Python package.
    ///
    /// Non-Python paths and symbol nodes return `None` without touching the
    /// filesystem; a standalone `.py` file (no `__init__.py` beside it) also
    /// returns `None`, because the Python parser only emits `pkg:` edges for
    /// real module imports.
    fn py_pkg_node(&mut self, path: &str) -> Option<String> {
        if is_symbol_node(path) || !path.ends_with(".py") {
            return None;
        }
        let p = PathBuf::from(path);
        if let Some(hit) = self.py_cache.get(&p) {
            return hit.clone();
        }
        let node = python_module_for_file(&p).map(|module| format!("pkg:{module}"));
        self.py_cache.insert(p, node.clone());
        node
    }

    /// Resolve `path` to its Cargo package name, if any.
    pub fn crate_name(&mut self, path: &str) -> Option<String> {
        if is_symbol_node(path) {
            return None;
        }
        let p = PathBuf::from(path);
        if let Some(hit) = self.cache.get(&p) {
            return hit.clone();
        }
        let name = crate_name_for_file(&p);
        self.cache.insert(p, name.clone());
        name
    }
}

/// Whether `path` is a graph pseudo-node (`pkg:...`, `sys:...`, `std:...`,
/// `external:...`) rather than a filesystem path. Pseudo-nodes must never be
/// walked on disk.
fn is_symbol_node(path: &str) -> bool {
    path.starts_with("pkg:")
        || path.starts_with("sys:")
        || path.starts_with("std:")
        || path.starts_with("external:")
}

/// Walk up from `file` to the nearest `Cargo.toml` with a `[package] name`,
/// and return that name.
///
/// Workspace-root manifests (no `[package]` section) are skipped so a crate
/// nested under a workspace still resolves to its own package name. Returns
/// `None` when no manifest with a package name is found before the
/// filesystem root.
pub fn crate_name_for_file(file: &Path) -> Option<String> {
    let mut dir = file.parent()?;
    loop {
        let manifest = dir.join("Cargo.toml");
        if manifest.is_file() {
            if let Some(name) = package_name(&manifest) {
                return Some(name);
            }
            // Workspace root or manifest without [package]: keep walking up.
        }
        dir = dir.parent()?;
    }
}

/// Extract the `name` value from the `[package]` section of a Cargo.toml.
fn package_name(manifest: &Path) -> Option<String> {
    let text = std::fs::read_to_string(manifest).ok()?;
    let mut in_package = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line.starts_with("[package]");
            continue;
        }
        if !in_package {
            continue;
        }
        let rest = line.strip_prefix("name")?.trim_start();
        let rest = rest.strip_prefix('=')?.trim_start();
        let start = rest.find('"')?;
        let after = &rest[start + 1..];
        let end = after.find('"')?;
        let value = after[..end].trim();
        if value.is_empty() {
            return None;
        }
        return Some(value.to_string());
    }
    None
}

/// Walk up from `file` to the nearest `go.mod` and return the import path of
/// the Go package containing `file` (GAP-057).
///
/// The import path is `<module path>/<file's directory relative to the
/// go.mod directory>`, with forward slashes; a file in the module root is
/// just the module path. Relative and absolute file paths both work, as long
/// as the walk stays on the same form. Returns `None` when no `go.mod` with
/// a module path is found before the filesystem root (the file is outside
/// any module).
pub fn go_package_for_file(file: &Path) -> Option<String> {
    let dir = file.parent()?;
    let mut current = dir;
    loop {
        let manifest = current.join("go.mod");
        if manifest.is_file() {
            let module = module_path(&manifest)?;
            let rel = dir.strip_prefix(current).ok()?;
            return Some(import_path(&module, rel));
        }
        current = current.parent()?;
    }
}

/// Extract the module path from a `go.mod` (`module <path>` line).
///
/// Blank lines and `//` comments are skipped; the path may be quoted.
/// Returns `None` for a `go.mod` without a module line.
fn module_path(manifest: &Path) -> Option<String> {
    let text = std::fs::read_to_string(manifest).ok()?;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        let Some(rest) = line.strip_prefix("module") else {
            continue;
        };
        // `modulefoo` is not a module line: the directive must be followed
        // by whitespace before the path.
        if !rest.starts_with(|c: char| c.is_whitespace()) {
            continue;
        }
        let value = rest.trim().trim_matches('"').trim();
        if value.is_empty() {
            return None;
        }
        return Some(value.to_string());
    }
    None
}

/// Join a module path with a directory relative to the module root, using
/// forward slashes. An empty (or `.`) relative dir yields the module path.
fn import_path(module: &str, rel: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    for component in rel.components() {
        if let Component::Normal(part) = component {
            parts.push(part.to_string_lossy().into_owned());
        }
    }
    if parts.is_empty() {
        module.to_string()
    } else {
        format!("{module}/{}", parts.join("/"))
    }
}

/// Derive the dotted Python module for `file` from the filesystem package
/// boundaries around it (GAP-064).
///
/// A directory is a package component iff it holds an `__init__.py`; the walk
/// climbs from the file's directory while every directory is a package and
/// stops at the **first non-package directory**, so a path outside a package
/// tree never contributes host path components (a tempdir file resolves to
/// `fastapi.routing`, never to `tmp.<random>.fastapi.routing`).
///
/// - `fastapi/routing.py` with `fastapi/__init__.py` → `fastapi.routing`.
/// - Nested packages include every component:
///   `fastapi/dependencies/utils.py` with both `__init__.py` files →
///   `fastapi.dependencies.utils`.
/// - `__init__.py` **is** its package: `fastapi/__init__.py` → `fastapi`
///   (never `fastapi.__init__`), `fastapi/dependencies/__init__.py` →
///   `fastapi.dependencies`.
/// - A `.py` file whose own directory is not a package (standalone script,
///   or a plain directory nested under a package) → `None`.
///
/// Returns `None` for a path with no module name at all, and for an
/// `__init__.py` that somehow sits in no package directory.
pub fn python_module_for_file(file: &Path) -> Option<String> {
    let stem = file.file_stem()?.to_str()?;
    let is_init = stem == "__init__";

    let mut parts: Vec<String> = Vec::new();
    let mut current = file.parent()?;
    loop {
        if !current.join("__init__.py").is_file() {
            // First non-package directory: stop. Anything above it is not
            // part of the module path (tempdir roots, `python3/`, ...).
            break;
        }
        let Some(name) = current.file_name().and_then(|n| n.to_str()) else {
            // Relative path exhausted (`a/b.py` → parent `a` → parent ``):
            // no further named component exists, so the walk ends here.
            break;
        };
        parts.push(name.to_string());
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }
    parts.reverse();

    if is_init {
        // The package node itself; a lone `__init__.py` in no package is a
        // bare file, not a module.
        return (!parts.is_empty()).then(|| parts.join("."));
    }
    if parts.is_empty() || stem.is_empty() {
        // Standalone file: not a member of any package.
        return None;
    }
    parts.push(stem.to_string());
    Some(parts.join("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, content: &str) -> String {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn resolves_nested_crate_src_file() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "crates/globset/Cargo.toml",
            "[package]\nname = \"globset\"\nversion = \"0.4.0\"\n",
        );
        let lib = write(
            dir.path(),
            "crates/globset/src/lib.rs",
            "pub struct Glob;\n",
        );
        assert_eq!(
            crate_name_for_file(Path::new(&lib)).as_deref(),
            Some("globset")
        );
        let mut resolver = PkgResolver::new();
        assert_eq!(resolver.pkg_node(&lib).as_deref(), Some("pkg:globset"));
    }

    #[test]
    fn skips_workspace_root_manifest() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/a\"]\n",
        );
        write(
            dir.path(),
            "crates/a/Cargo.toml",
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n",
        );
        let main = write(dir.path(), "crates/a/src/main.rs", "fn main() {}\n");
        assert_eq!(crate_name_for_file(Path::new(&main)).as_deref(), Some("a"));
    }

    #[test]
    fn symbol_nodes_never_resolve() {
        let mut resolver = PkgResolver::new();
        assert_eq!(resolver.pkg_node("pkg:globset"), None);
        assert_eq!(resolver.pkg_node("sys:std"), None);
        assert_eq!(resolver.pkg_node("external:repo:path"), None);
    }

    #[test]
    fn file_without_manifest_resolves_none() {
        let dir = tempfile::tempdir().unwrap();
        let orphan = write(dir.path(), "src/orphan.rs", "fn x() {}\n");
        let mut resolver = PkgResolver::new();
        assert_eq!(resolver.pkg_node(&orphan), None);
    }

    #[test]
    fn package_name_ignores_comments_and_other_sections() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "Cargo.toml",
            "[workspace]\nname = \"not-the-package\"\n\n[package]\nname = \"real\" # the crate\nversion = \"1.0.0\"\n",
        );
        let src = write(dir.path(), "src/lib.rs", "pub fn f() {}\n");
        assert_eq!(
            crate_name_for_file(Path::new(&src)).as_deref(),
            Some("real")
        );
    }

    // ── Go file → package resolution (GAP-057) ──────────────────────

    #[test]
    fn resolves_nested_go_file_to_module_package() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "go.mod", "module example.com/demo\n\ngo 1.22\n");
        let src = write(dir.path(), "core/mount/lookup_unix.go", "package mount\n");
        assert_eq!(
            go_package_for_file(Path::new(&src)).as_deref(),
            Some("example.com/demo/core/mount")
        );
        let mut resolver = PkgResolver::new();
        assert_eq!(
            resolver.pkg_node(&src).as_deref(),
            Some("pkg:example.com/demo/core/mount")
        );
    }

    #[test]
    fn resolves_go_file_in_module_root() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "go.mod", "module example.com/demo\n");
        let main = write(dir.path(), "main.go", "package main\n");
        assert_eq!(
            go_package_for_file(Path::new(&main)).as_deref(),
            Some("example.com/demo")
        );
        let mut resolver = PkgResolver::new();
        assert_eq!(
            resolver.pkg_node(&main).as_deref(),
            Some("pkg:example.com/demo")
        );
    }

    /// Nearest `go.mod` above `dir`, with the module path it declares.
    fn ancestor_module(dir: &Path) -> Option<(PathBuf, String)> {
        let mut current = dir;
        loop {
            let manifest = current.join("go.mod");
            if manifest.is_file() {
                return module_path(&manifest).map(|module| (current.to_path_buf(), module));
            }
            current = current.parent()?;
        }
    }

    #[test]
    fn go_file_without_module_resolves_none() {
        let dir = tempfile::tempdir().unwrap();
        let orphan = write(dir.path(), "src/orphan.go", "package orphan\n");
        let mut resolver = PkgResolver::new();
        // `tempfile` allocates under TMPDIR, and the walk mirrors the Go
        // toolchain (search up to the filesystem root), so a host that
        // carries a stray go.mod above TMPDIR (this one has /tmp/go.mod from
        // unrelated tooling) legitimately makes the fixture a member of that
        // module. Both outcomes are checked so the test never passes by
        // silently skipping.
        match ancestor_module(dir.path()) {
            None => {
                assert_eq!(go_package_for_file(Path::new(&orphan)), None);
                assert_eq!(resolver.pkg_node(&orphan), None);
            }
            Some((root, module)) => {
                let rel = Path::new(&orphan)
                    .parent()
                    .unwrap()
                    .strip_prefix(&root)
                    .unwrap();
                let expected = format!("pkg:{}", import_path(&module, rel));
                assert_eq!(
                    go_package_for_file(Path::new(&orphan)),
                    Some(import_path(&module, rel))
                );
                assert_eq!(
                    resolver.pkg_node(&orphan).as_deref(),
                    Some(expected.as_str())
                );
            }
        }
        // A bare filename has no module above it: the walk starts and ends at
        // the current directory (the package root, which holds no go.mod).
        assert_eq!(go_package_for_file(Path::new("orphan.go")), None);
        assert_eq!(PkgResolver::new().pkg_node("orphan.go"), None);
    }

    #[test]
    fn std_prefix_nodes_never_resolve() {
        let mut resolver = PkgResolver::new();
        assert_eq!(resolver.pkg_node("std:fmt"), None);
        assert_eq!(resolver.pkg_node("std:net/http"), None);
        assert_eq!(resolver.crate_name("std:fmt"), None);
    }

    #[test]
    fn go_and_rust_resolution_coexist_in_one_tree() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "Cargo.toml",
            "[package]\nname = \"rustdemo\"\nversion = \"0.1.0\"\n",
        );
        write(dir.path(), "go.mod", "module example.com/demo\n");
        let rs = write(dir.path(), "src/lib.rs", "pub fn f() {}\n");
        let go = write(dir.path(), "cmd/tool/main.go", "package main\n");
        let mut resolver = PkgResolver::new();
        assert_eq!(resolver.pkg_node(&rs).as_deref(), Some("pkg:rustdemo"));
        assert_eq!(
            resolver.pkg_node(&go).as_deref(),
            Some("pkg:example.com/demo/cmd/tool")
        );
    }

    #[test]
    fn module_path_skips_comments_and_ignores_non_module_lines() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "go.mod",
            "// a comment\n\ngo 1.22\n\nmodule example.com/demo\n",
        );
        let main = write(dir.path(), "main.go", "package main\n");
        assert_eq!(
            go_package_for_file(Path::new(&main)).as_deref(),
            Some("example.com/demo")
        );
    }

    // ── Python file → module resolution (GAP-064) ───────────────────

    /// Build a FastAPI-shaped fixture: `fastapi/routing.py` and
    /// `fastapi/dependencies/utils.py` inside regular packages. Returns the
    /// tempdir so it outlives the assertions.
    fn fastapi_fixture() -> (tempfile::TempDir, String, String) {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "fastapi/__init__.py", "__version__ = \"0.1\"\n");
        write(
            dir.path(),
            "fastapi/dependencies/__init__.py",
            "from .utils import Depends\n",
        );
        let routing = write(
            dir.path(),
            "fastapi/routing.py",
            "from fastapi.dependencies.utils import Depends\n",
        );
        let utils = write(
            dir.path(),
            "fastapi/dependencies/utils.py",
            "class Depends: ...\n",
        );
        (dir, routing, utils)
    }

    #[test]
    fn resolves_python_file_in_regular_package() {
        let (dir, routing, _) = fastapi_fixture();
        assert_eq!(
            python_module_for_file(Path::new(&routing)).as_deref(),
            Some("fastapi.routing")
        );
        let mut resolver = PkgResolver::new();
        assert_eq!(
            resolver.pkg_node(&routing).as_deref(),
            Some("pkg:fastapi.routing")
        );

        // The stop-at-first-non-package rule: the tempdir root (and every
        // host directory above it) must not contribute module components.
        let leaked = dir
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            !python_module_for_file(Path::new(&routing))
                .unwrap()
                .contains(&leaked),
            "host path component leaked into the module name"
        );
    }

    #[test]
    fn resolves_nested_python_packages_with_every_component() {
        let (_dir, _, utils) = fastapi_fixture();
        assert_eq!(
            python_module_for_file(Path::new(&utils)).as_deref(),
            Some("fastapi.dependencies.utils")
        );
        let mut resolver = PkgResolver::new();
        assert_eq!(
            resolver.pkg_node(&utils).as_deref(),
            Some("pkg:fastapi.dependencies.utils")
        );
        // Second lookup goes through the per-query cache and must agree.
        assert_eq!(
            resolver.pkg_node(&utils).as_deref(),
            Some("pkg:fastapi.dependencies.utils")
        );
    }

    #[test]
    fn python_init_maps_to_package_node_not_dunder_init_module() {
        let (dir, _, _) = fastapi_fixture();
        let top = dir.path().join("fastapi/__init__.py");
        let nested = dir.path().join("fastapi/dependencies/__init__.py");
        assert_eq!(
            python_module_for_file(&top).as_deref(),
            Some("fastapi"),
            "__init__.py is its package, never `fastapi.__init__`"
        );
        assert_eq!(
            python_module_for_file(&nested).as_deref(),
            Some("fastapi.dependencies")
        );
        let mut resolver = PkgResolver::new();
        assert_eq!(
            resolver.pkg_node(&top.to_string_lossy()).as_deref(),
            Some("pkg:fastapi")
        );
        assert_eq!(
            resolver.pkg_node(&nested.to_string_lossy()).as_deref(),
            Some("pkg:fastapi.dependencies")
        );
    }

    #[test]
    fn standalone_python_files_resolve_none() {
        let dir = tempfile::tempdir().unwrap();
        // A script in a directory that is not a package.
        let script = write(dir.path(), "scripts/run.py", "print('hi')\n");
        assert_eq!(python_module_for_file(Path::new(&script)), None);
        assert_eq!(PkgResolver::new().pkg_node(&script), None);

        // A plain directory nested under a package is still not a package:
        // the walk stops at the first directory without `__init__.py`.
        write(dir.path(), "outer/__init__.py", "");
        let nested_plain = write(dir.path(), "outer/plain/mod.py", "");
        assert_eq!(python_module_for_file(Path::new(&nested_plain)), None);
        assert_eq!(PkgResolver::new().pkg_node(&nested_plain), None);

        // A bare relative filename has no package above it in this crate
        // (and no name component at all once the walk reaches the empty
        // parent path).
        assert_eq!(python_module_for_file(Path::new("orphan.py")), None);
        assert_eq!(PkgResolver::new().pkg_node("orphan.py"), None);
    }

    #[test]
    fn python_symbol_nodes_never_resolve() {
        let mut resolver = PkgResolver::new();
        assert_eq!(resolver.pkg_node("pkg:fastapi.routing"), None);
        assert_eq!(resolver.pkg_node("pkg:os.path"), None);
        assert_eq!(resolver.pkg_node("sys:python"), None);
        assert_eq!(resolver.pkg_node("std:python"), None);
        assert_eq!(resolver.pkg_node("external:repo:app/main.py"), None);
    }

    #[test]
    fn python_file_outside_a_package_does_not_fall_back_to_cargo() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "Cargo.toml",
            "[package]\nname = \"rustdemo\"\nversion = \"0.1.0\"\n",
        );
        let script = write(dir.path(), "tools/script.py", "print('hi')\n");
        let lib = write(dir.path(), "src/lib.rs", "pub fn f() {}\n");
        let mut resolver = PkgResolver::new();
        // Rust resolution is untouched ...
        assert_eq!(resolver.pkg_node(&lib).as_deref(), Some("pkg:rustdemo"));
        // ... but a Python file is never a Cargo package member, because the
        // Python parser emits `pkg:<module>` edges, not crate edges.
        assert_eq!(resolver.pkg_node(&script), None);
    }

    #[test]
    fn python_rust_and_go_resolution_coexist_in_one_tree() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "Cargo.toml",
            "[package]\nname = \"rustdemo\"\nversion = \"0.1.0\"\n",
        );
        write(dir.path(), "go.mod", "module example.com/demo\n");
        write(dir.path(), "fastapi/__init__.py", "");
        let rs = write(dir.path(), "src/lib.rs", "pub fn f() {}\n");
        let go = write(dir.path(), "cmd/tool/main.go", "package main\n");
        let py = write(dir.path(), "fastapi/routing.py", "class APIRouter: ...\n");
        let mut resolver = PkgResolver::new();
        assert_eq!(resolver.pkg_node(&rs).as_deref(), Some("pkg:rustdemo"));
        assert_eq!(
            resolver.pkg_node(&go).as_deref(),
            Some("pkg:example.com/demo/cmd/tool")
        );
        assert_eq!(
            resolver.pkg_node(&py).as_deref(),
            Some("pkg:fastapi.routing")
        );
    }
}
