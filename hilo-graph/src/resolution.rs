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
//! TypeScript/JavaScript sources resolve BACKWARDS through `local:` nodes
//! (GAP-069): the JS/TS parser keeps relative specifiers verbatim
//! (`classify_js`), so one target file is named by MANY `local:` nodes —
//! one per (importer directory, specifier) pair. A per-file walk like the
//! `pkg:` resolvers cannot invert that; [`LocalSpecResolver`] instead
//! builds a reverse index from the edge rows themselves, resolving every
//! `local:` specifier against its own importer's directory.
//!
//! Resolution is query-time only: the canonical `edges.jsonl` and the
//! DuckDB cache are untouched. Files that belong to no package (or are not
//! files at all — `pkg:`/`sys:`/`std:`/`external:` symbol nodes) resolve to
//! `None`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Component, Path, PathBuf};

use duckdb::{params, Connection};

use crate::error::GraphResult;

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

// ── TypeScript/JavaScript `local:` reverse resolution (GAP-069) ─────

/// Whether `path` is a `local:` pseudo-node — a raw import specifier the
/// JS/TS (and C/Zig/Dart/Lua/...) parsers kept verbatim. These are node
/// names, not filesystem paths, and must never be probed on disk.
fn is_local_node(path: &str) -> bool {
    path.starts_with("local:")
}

/// Strip one `?query` / `#hash` suffix from an import specifier. Only the
/// FIRST `?`/`#` counts (a later one is part of the query/hash itself);
/// bundler conventions like `./style.css?raw` and `./mod.js#block` must
/// resolve to the underlying file.
fn strip_query_hash(spec: &str) -> &str {
    match spec.find(['?', '#']) {
        Some(idx) => &spec[..idx],
        None => spec,
    }
}

/// The extension-probing candidates for a resolved base path, in order.
/// Extension-less: the exact path, then the TS/JS/JSON source extensions,
/// then directory `index` modules. A spec that already carries an extension
/// keeps it (probed as `.ts`/`.tsx` first when it ends `.js`/`.mjs` — the
/// TS ESM convention where emitted-JS specifiers name the TS source).
fn candidate_paths(base: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let has_ext = base.extension().is_some();
    if has_ext {
        let ext = base
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if ext == "js" || ext == "mjs" || ext == "cjs" {
            // TS ESM convention: `./x.js` may name `./x.ts` (or `.tsx`).
            // The `.js` file itself is probed too — the plain-JS case must
            // still resolve.
            out.push(base.to_path_buf());
            out.push(rename_ext(base, "ts"));
            out.push(rename_ext(base, "tsx"));
        } else {
            out.push(base.to_path_buf());
        }
        return out;
    }
    out.push(base.to_path_buf());
    for ext in ["ts", "tsx", "js", "jsx", "mjs", "cjs", "json"] {
        let mut with = base.as_os_str().to_owned();
        with.push(".");
        with.push(ext);
        out.push(PathBuf::from(with));
    }
    for ext in ["ts", "tsx", "js", "jsx"] {
        let mut idx = base.as_os_str().to_owned();
        idx.push("/index.");
        idx.push(ext);
        out.push(PathBuf::from(idx));
    }
    out
}

/// `base.rs` with its final extension replaced by `ext` (path may not exist
/// yet — this is lexical only).
fn rename_ext(base: &Path, ext: &str) -> PathBuf {
    let mut out = base.with_extension("").into_os_string();
    out.push(".");
    out.push(ext);
    PathBuf::from(out)
}

/// The first candidate that exists on disk, falling back to the exact
/// (lexically normalized) path. Existence is only a TIE-BREAKER: an
/// unresolvable-but-lexically-normalized base still yields a target so the
/// index keys stay stable even when the fixture target was never written.
fn probe_existing(base: &Path) -> PathBuf {
    for candidate in candidate_paths(base) {
        if candidate.is_file() {
            return candidate;
        }
    }
    base.to_path_buf()
}

/// Lexically normalize `.` / `..` path components (no filesystem access, no
/// requirement that the path exists). `a/b/../c` → `a/c`; a `..` above the
/// root is dropped.
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // Keep a leading `..` on relative paths (`../x` must stay
                // meaningfully relative), drop it above an absolute root.
                if !out.pop() && !path.is_absolute() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Canonicalize a path for index keying: the real filesystem form when it
/// exists, lexical normalization otherwise. Used ONLY for paths already in
/// absolute form — relative (repo-space) keys are never put through this:
/// canonicalizing a relative path against the process CWD would silently
/// move it into the host filesystem's space.
fn canonical_key(path: &Path) -> PathBuf {
    if path.is_absolute() {
        match std::fs::canonicalize(path) {
            Ok(real) => real,
            // Missing file: still normalize into the absolute key space so
            // the query cannot differ from the index by `..` components.
            Err(_) => lexical_normalize(path),
        }
    } else {
        lexical_normalize(path)
    }
}

/// Map `path` into repo space: relative paths pass through untouched;
/// absolute paths are stripped of `root` — raw prefix first (fast path),
/// then with the root canonicalized (symlinked tmpdir roots: macOS
/// `/tmp` → `/private/tmp`), then with the path itself canonicalized.
/// Returns the unchanged (absolute) path when no bridge applies.
fn repo_space(path: &Path, root: Option<&Path>) -> PathBuf {
    let p = lexical_normalize(path);
    if !p.is_absolute() {
        return p;
    }
    let Some(root) = root else {
        return p;
    };
    if let Ok(rel) = p.strip_prefix(root) {
        return rel.to_path_buf();
    }
    if let Ok(real) = std::fs::canonicalize(root) {
        if let Ok(rel) = p.strip_prefix(&real) {
            return rel.to_path_buf();
        }
    }
    if let Ok(real) = std::fs::canonicalize(&p) {
        if let Ok(rel) = real.strip_prefix(root) {
            return rel.to_path_buf();
        }
    }
    p
}

/// Query-time reverse resolver for TS/JS `local:` import nodes (GAP-069).
///
/// The JS/TS parser stores relative specifiers verbatim as `local:<spec>`
/// edge targets (`classify_js`), so ONE target file is named by many nodes —
/// `./pluginContainer` imported from `server/`, `../server/pluginContainer`
/// from `server/inner/`, and `./server/pluginContainer` from the repo root
/// are three different node names for the same real file. A forward
/// per-file walk (the `pkg:` approach) cannot invert that; this resolver
/// goes the other way: it reads every `local:` edge once, resolves each
/// specifier against its own **importer's** directory, and indexes
/// `resolved target path -> {local node -> importer set}`. A file query
/// then looks its path up in the index and gets every node naming it.
///
/// Path spaces (the part that makes this correct rather than lucky):
/// - `hilo graph warm` stores importers repo-relative, so the index is
///   keyed in that same *relative repo space*; JIT-parsed (absolute)
///   importers are bridged into it via `graph_root`.
/// - a query in absolute form is bridged back to repo space the same way
///   before the lookup; a query already in repo form is used verbatim.
///   Both forms of the same file therefore hit the same key, and the
///   root never leaks into returned data (importers keep their stored
///   form — exactly what edge rows contain).
///
/// Resolution rules implemented here:
/// - specifiers starting with `.` resolve against `dirname(importer)`;
/// - specifiers starting with `/` resolve against the repo root (below);
/// - `.`/`..` components are normalized lexically (the target need not
///   exist);
/// - extension probing: the exact path, then `.ts/.tsx/.js/.jsx/.mjs/
///   .cjs/.json`, then `<base>/index.{ts,tsx,js,jsx}`; a `.js`/`.mjs`/`.cjs`
///   spec also probes `.ts`/`.tsx` (TS ESM convention);
/// - a trailing `?query` / `#hash` is not part of the path (bundler
///   conventions: `?raw`, `?inline`, `#block`);
/// - existence is a tie-breaker among candidates, never a requirement: an
///   unresolvable spec normalizes lexically so the index key space stays
///   stable for targets that were never materialized on disk.
///
/// Symbol/pseudo nodes — `pkg:`, `sys:`, `std:`, `external:`, and
/// `local:` itself — never reach the filesystem: `local:` names are looked
/// up in the index, everything else is rejected by [`LocalSpecResolver::nodes_for`].
///
/// One resolver instance per top-level query: the index is built from a
/// single SQL scan and reused for every BFS node, keeping the traversal
/// linear (one query + one scan per `compute_impact` call, not per edge).
pub struct LocalSpecResolver {
    /// Repo-space target path → per-node importer sets. The pairing is the
    /// point (GAP-069): the SAME node string (`local:./x`) can be emitted
    /// from different directories naming DIFFERENT targets, so an importer
    /// counts for a target only when the specifier resolved from that
    /// importer's own directory lands on it.
    by_target: HashMap<PathBuf, BTreeMap<String, BTreeSet<String>>>,
    /// Repository root in ABSOLUTE form (the common ancestor of the
    /// absolute importers' directories, canonicalized) — the bridge used to
    /// map absolute queries and absolute importers into repo space. `None`
    /// when the graph carries no absolute importers (pure warm-built
    /// graphs, most unit tests); a root is only needed for bridging.
    graph_root: Option<PathBuf>,
    /// The absolute-root part of `graph_root` — `Some` only when the graph
    /// itself contained absolute importers. Gates the absolute-query
    /// suffix fallback in [`Self::nodes_for`]: with a derivable root the
    /// exact bridge is authoritative and the fallback must not widen it.
    abs_root: Option<PathBuf>,
}

impl LocalSpecResolver {
    /// Build the reverse index from every `local:` edge in the graph, in
    /// ONE SQL scan (GAP-069).
    ///
    /// Each edge's specifier is resolved against its own importer's
    /// directory; `/`-rooted specifiers resolve against the repo root
    /// (the relative root when the importers are repo-relative, otherwise
    /// the derived absolute root).
    pub fn from_edges(conn: &Connection) -> GraphResult<Self> {
        let mut stmt =
            conn.prepare(r#"SELECT "from", "to" FROM edges WHERE "to" LIKE 'local:%'"#)?;
        let rows = stmt.query_map(params![], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut pairs: Vec<(String, String)> = Vec::new();
        for row in rows {
            pairs.push(row?);
        }
        drop(stmt);

        // Repo root in each space. Relative importers (the `graph warm`
        // storage form) imply the repo root is `""`; absolute importers
        // (JIT parses) contribute their common ancestor directory.
        let rel_root = pairs
            .iter()
            .any(|(from, _)| !Path::new(from).is_absolute())
            .then_some(PathBuf::new());
        let abs_dirs: Vec<PathBuf> = pairs
            .iter()
            .filter(|(from, _)| Path::new(from).is_absolute())
            .filter_map(|(from, _)| Path::new(from.as_str()).parent().map(Path::to_path_buf))
            .collect();
        let abs_root = common_ancestor(&abs_dirs);
        let graph_root = abs_root.clone().or(rel_root);

        let mut by_target: HashMap<PathBuf, BTreeMap<String, BTreeSet<String>>> = HashMap::new();
        for (importer, node) in &pairs {
            let raw = node.strip_prefix("local:").unwrap_or(node);
            let raw = strip_query_hash(raw);
            if raw.is_empty() {
                continue;
            }
            let importer_path = Path::new(importer);
            // The importer's own directory, staying in its stored path
            // space (relative importers → relative dirs; `entry.ts` → `""`).
            let dir = match importer_path.parent() {
                Some(d) => d.to_path_buf(),
                None => continue,
            };
            let base = if raw.starts_with('/') {
                // Repo-root-relative specifier: resolve against the repo
                // root of the importer's own space.
                let root: &Path = match (graph_root.as_deref(), importer_path.is_absolute()) {
                    // Warm-graph importer (repo-relative): the root is "".
                    (_, false) => Path::new(""),
                    // Absolute importer with a derived root.
                    (Some(r), true) => r,
                    // Absolute importer but no absolute root anywhere in
                    // the graph: the best available anchor is the importer
                    // drive/root — unusual, and better than dropping the
                    // edge.
                    (None, true) => Path::new("/"),
                };
                lexical_normalize(&root.join(raw.trim_start_matches('/')))
            } else {
                lexical_normalize(&dir.join(raw))
            };
            // Key the target in repo space. When the probe FOUND a file,
            // register the winner plus the raw base (extension-less query
            // form). When the probe found NOTHING — the target's filesystem
            // is not visible from the querying process (unit fixtures in
            // tempdirs, containerized queries), or the import is dangling —
            // register every probing candidate, so a query naming the real
            // target file (`./thing` → `thing.ts`) still resolves. The
            // over-keying is bounded by this one spec's candidates and only
            // applies in the invisible-filesystem case; with files visible
            // (the production warm path) the probed winner is exact.
            let winner = probe_existing(&base);
            let keys: Vec<PathBuf> = if winner == base && !base.is_file() {
                candidate_paths(&base)
            } else {
                vec![winner.clone(), base]
            };
            for key in keys {
                let key = repo_space(&key, graph_root.as_deref());
                by_target
                    .entry(key)
                    .or_default()
                    .entry(node.clone())
                    .or_default()
                    .insert(importer.clone());
            }
        }
        Ok(Self {
            by_target,
            graph_root,
            abs_root,
        })
    }

    /// Edges to keep for `path` — the `(node, allowed importer set)` pairs
    /// that resolve to it — or an empty vec when none do.
    ///
    /// `path` may be absolute or repo-relative; absolute queries are
    /// bridged into repo space before the lookup, so both forms of the
    /// same file resolve identically.
    ///
    /// Symbol/pseudo nodes of every family (`pkg:`, `sys:`, `std:`,
    /// `external:`, `local:`) never match: they are not filesystem paths
    /// and must never be looked up as targets.
    ///
    /// The importer sets matter for correctness, not just hygiene: the SAME
    /// node string (`local:./pluginContainer`) can appear in edges from
    /// different directories naming DIFFERENT targets. A consumer filtering
    /// on the node alone would drag in edges that name some other file; it
    /// must additionally require `edge.from ∈ allowed importers`.
    ///
    /// Known limitation (shared with the `pkg:` resolvers, which also never
    /// verify a path belongs to the querying repo): when the graph carries
    /// no absolute importers there is no derivable root, so an absolute
    /// query is matched by component-aligned suffix. A file in a DIFFERENT
    /// repo root with an identical internal path therefore resolves — the
    /// resolver cannot know the difference. Ambiguity is still guarded
    /// WITHIN the index: several distinct index keys matching one query
    /// yield nothing rather than a guess.
    pub fn nodes_for(&self, path: &str) -> Vec<(String, BTreeSet<String>)> {
        if is_symbol_node(path) || is_local_node(path) {
            return Vec::new();
        }
        let key = self.repo_key(path);
        if let Some(entries) = self.by_target.get(&key) {
            return inner_to_vec(entries);
        }
        // Fallback bridge for pure-warm graphs (repo-relative importers, no
        // derivable absolute root): an ABSOLUTE query cannot be stripped to
        // repo space, so match it against the index keys as a path-component
        // suffix — `/tmp/xyz/server/pluginContainer.ts` ends with the repo
        // key `server/pluginContainer.ts`. Guards: components must align on
        // directory boundaries (`server/pluginContainer.ts` never matches
        // `my-server/pluginContainer.ts`), and an ambiguous result —
        // several distinct index keys matching the same query — returns
        // nothing rather than a guess. (A file in a DIFFERENT repo with an
        // identical internal path is NOT distinguishable here — documented
        // limitation, shared with the pkg: resolvers.)
        if !Path::new(path).is_absolute() || self.abs_root.is_some() {
            return Vec::new();
        }
        let matches: Vec<&BTreeMap<String, BTreeSet<String>>> = self
            .by_target
            .iter()
            .filter(|(k, _)| !k.as_os_str().is_empty() && Path::new(path).ends_with(k.as_path()))
            .map(|(_, v)| v)
            .collect();
        if matches.len() == 1 {
            return inner_to_vec(matches[0]);
        }
        Vec::new()
    }

    /// Map a query path into the repo-relative key space of the index:
    /// absolute paths are stripped of the graph root, repo-relative paths
    /// are used verbatim (after lexical normalization).
    fn repo_key(&self, path: &str) -> PathBuf {
        repo_space(Path::new(path), self.graph_root.as_deref())
    }

    /// The `local:` node names that resolve to `path`. Convenience form —
    /// prefer [`nodes_for`](Self::nodes_for) when edges must be filtered by
    /// importer.
    pub fn node_names(&self, path: &str) -> Vec<String> {
        self.nodes_for(path)
            .into_iter()
            .map(|(node, _)| node)
            .collect()
    }

    /// Whether `path` resolves to at least one `local:` node (convenience
    /// for wiring checks).
    pub fn resolves(&self, path: &str) -> bool {
        !self.nodes_for(path).is_empty()
    }
}

/// Copy an index bucket into the public `(node, importer set)` shape.
fn inner_to_vec(entries: &BTreeMap<String, BTreeSet<String>>) -> Vec<(String, BTreeSet<String>)> {
    entries
        .iter()
        .map(|(node, importers)| (node.clone(), importers.clone()))
        .collect()
}

/// Common ancestor directory of the given absolute paths, or `None` for an
/// empty set. Uses the canonical form when a path exists so symlinked
/// tempdir roots (`/tmp` vs `/private/tmp`) agree; falls back to the raw
/// parent chain when it does not.
fn common_ancestor(paths: &[PathBuf]) -> Option<PathBuf> {
    if paths.is_empty() {
        return None;
    }
    let canon: Vec<PathBuf> = paths.iter().map(|p| canonical_key(p)).collect();
    let mut ancestor = canon[0].clone();
    for path in &canon[1..] {
        while !path.starts_with(&ancestor) {
            ancestor = ancestor.parent()?.to_path_buf();
        }
    }
    Some(ancestor)
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

    // ── TS/JS `local:` reverse resolution (GAP-069) ─────────────────

    /// The vite-shaped three-importer fixture: one target file named by a
    /// DIFFERENT `local:` node per importer directory. `./pluginContainer`
    /// only works from INSIDE `server/`; `../server/pluginContainer` from a
    /// SIBLING root-level dir (`client/`); `./server/pluginContainer` from
    /// the repo root. Returns the tempdir (kept alive by the caller) and
    /// the target's absolute path.
    fn vite_fixture() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "server/pluginContainer.ts", "// target\n");
        write(dir.path(), "server/host.ts", "import './pluginContainer'\n");
        write(
            dir.path(),
            "client/main.ts",
            "import '../server/pluginContainer'\n",
        );
        write(
            dir.path(),
            "entry.ts",
            "import './server/pluginContainer'\n",
        );
        let target = dir
            .path()
            .join("server/pluginContainer.ts")
            .to_string_lossy()
            .into_owned();
        (dir, target)
    }

    fn graph_with(rows: &[(&str, &str, &str)]) -> duckdb::Connection {
        let conn = duckdb::Connection::open_in_memory().unwrap();
        crate::graph::ensure_schema(&conn).unwrap();
        for (from, to, rel) in rows {
            conn.execute(
                "INSERT INTO edges (\"from\", \"to\", rel, provenance, confidence) VALUES (?, ?, ?, 'ast_exact', 1.0)",
                params![from, to, rel],
            )
            .unwrap();
        }
        conn
    }

    #[test]
    fn local_resolver_maps_three_specifier_forms_to_one_target() {
        // Importer paths are stored REPO-RELATIVE by `hilo graph warm`
        // (parse_imports receives cwd-stripped paths) — model that exactly.
        let (dir, target) = vite_fixture();
        let conn = graph_with(&[
            ("server/host.ts", "local:./pluginContainer", "imports"),
            (
                "client/main.ts",
                "local:../server/pluginContainer",
                "imports",
            ),
            ("entry.ts", "local:./server/pluginContainer", "imports"),
        ]);
        let resolver = LocalSpecResolver::from_edges(&conn).unwrap();
        let names = resolver.node_names(&target);
        assert_eq!(
            names.len(),
            3,
            "all three specifier forms must resolve to the target, got {names:?}"
        );
        assert!(names.contains(&"local:./pluginContainer".to_string()));
        assert!(names.contains(&"local:../server/pluginContainer".to_string()));
        assert!(names.contains(&"local:./server/pluginContainer".to_string()));

        // The same query in repo-relative form must hit the same index key.
        let via_rel = resolver.node_names("server/pluginContainer.ts");
        assert_eq!(via_rel.len(), 3, "relative query form must agree");
        drop(dir);
    }

    #[test]
    fn local_resolver_absolute_importers_bridge_through_derived_root() {
        // JIT parses can store ABSOLUTE importer paths; the index must
        // bridge them into repo space via the derived common ancestor so
        // both storage forms answer the same queries.
        let dir = tempfile::tempdir().unwrap();
        let abs = |rel: &str| dir.path().join(rel).to_string_lossy().into_owned();
        write(dir.path(), "server/pluginContainer.ts", "");
        write(dir.path(), "server/host.ts", "");
        write(dir.path(), "client/main.ts", "");
        write(dir.path(), "entry.ts", "");
        let conn = graph_with(&[
            (&abs("server/host.ts"), "local:./pluginContainer", "imports"),
            (
                &abs("client/main.ts"),
                "local:../server/pluginContainer",
                "imports",
            ),
            (
                &abs("entry.ts"),
                "local:./server/pluginContainer",
                "imports",
            ),
        ]);
        let resolver = LocalSpecResolver::from_edges(&conn).unwrap();
        let target = abs("server/pluginContainer.ts");
        assert_eq!(
            resolver.node_names(&target).len(),
            3,
            "absolute importers must resolve to the absolute target"
        );
        // Same query in repo-relative form hits the bridged key.
        assert_eq!(
            resolver.node_names("server/pluginContainer.ts").len(),
            3,
            "relative query on an absolute-importer graph must agree"
        );
        drop(dir);
    }

    #[test]
    fn local_resolver_does_not_global_basename_match() {
        // `./pluginContainer` from OTHER directories names DIFFERENT files.
        // Neither those files nor this target may cross-match.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "server/pluginContainer.ts", "");
        write(dir.path(), "elsewhere/pluginContainer.ts", "");
        write(
            dir.path(),
            "elsewhere/consumer.ts",
            "import './pluginContainer'\n",
        );
        let conn = graph_with(&[(
            "elsewhere/consumer.ts",
            "local:./pluginContainer",
            "imports",
        )]);
        let resolver = LocalSpecResolver::from_edges(&conn).unwrap();

        let here = dir
            .path()
            .join("server/pluginContainer.ts")
            .to_string_lossy()
            .into_owned();
        assert!(
            resolver.node_names(&here).is_empty(),
            "a same-named node from another directory must not match"
        );

        let other = dir
            .path()
            .join("elsewhere/pluginContainer.ts")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            resolver.node_names(&other),
            vec!["local:./pluginContainer".to_string()],
            "the actual target still resolves"
        );
        drop(dir);
    }

    #[test]
    fn local_resolver_extension_probing_and_index_modules() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/thing.ts", "");
        write(dir.path(), "src/widgets/index.ts", "");
        write(dir.path(), "src/legacy.js", "");
        write(dir.path(), "src/data.json", "");
        // Extension-less file, directory index, TS-ESM .js→.ts mapping
        // (real .js file must still resolve), and JSON.
        let conn = graph_with(&[
            ("src/a.ts", "local:./thing", "imports"),
            ("src/b.ts", "local:./widgets", "imports"),
            ("src/c.ts", "local:./legacy.js", "imports"),
            ("src/d.ts", "local:./data.json", "imports"),
        ]);
        let resolver = LocalSpecResolver::from_edges(&conn).unwrap();
        for rel in [
            "src/thing.ts",
            "src/widgets/index.ts",
            "src/legacy.js",
            "src/data.json",
        ] {
            let abs = dir.path().join(rel).to_string_lossy().into_owned();
            assert_eq!(
                resolver.node_names(&abs).len(),
                1,
                "{rel} must resolve through exactly one specifier"
            );
        }
        drop(dir);
    }

    #[test]
    fn local_resolver_slash_rooted_spec_resolves_against_project_root() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/thing.ts", "");
        write(dir.path(), "deep/nested/importer.ts", "");
        let conn = graph_with(&[("deep/nested/importer.ts", "local:/src/thing", "imports")]);
        let resolver = LocalSpecResolver::from_edges(&conn).unwrap();
        let target = dir
            .path()
            .join("src/thing.ts")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            resolver.node_names(&target),
            vec!["local:/src/thing".to_string()],
            "/-rooted spec must resolve against the importers' common ancestor"
        );
        drop(dir);
    }

    #[test]
    fn local_resolver_relative_and_absolute_queries_agree() {
        let (dir, target) = vite_fixture();
        let conn = graph_with(&[
            ("server/host.ts", "local:./pluginContainer", "imports"),
            (
                "client/main.ts",
                "local:../server/pluginContainer",
                "imports",
            ),
            ("entry.ts", "local:./server/pluginContainer", "imports"),
        ]);
        let resolver = LocalSpecResolver::from_edges(&conn).unwrap();
        // Absolute-form query (what compute_impact tests pass), resolved
        // through the absolute→repo fallback bridge (pure-warm graph).
        let abs = resolver.node_names(&target);
        // A canonicalized query (macOS /tmp symlink safety) must agree.
        let canonical = std::fs::canonicalize(&target).unwrap();
        let via_canonical = resolver.node_names(&canonical.to_string_lossy());
        // A repo-relative query must agree too.
        let via_rel = resolver.node_names("server/pluginContainer.ts");
        assert_eq!(abs.len(), 3, "absolute query form, got {abs:?}");
        assert_eq!(via_canonical.len(), 3, "canonical query form");
        assert_eq!(via_rel.len(), 3, "relative query form");
        drop(dir);
    }

    #[test]
    fn local_resolver_symbol_nodes_never_probe() {
        let conn = graph_with(&[("a.ts", "local:./b", "imports")]);
        let resolver = LocalSpecResolver::from_edges(&conn).unwrap();
        // Pseudo/symbol nodes are node names, not paths — they must return
        // empty without touching the index (and without panicking).
        assert!(resolver.node_names("pkg:vite").is_empty());
        assert!(resolver.node_names("sys:fs").is_empty());
        assert!(resolver.node_names("std:fmt").is_empty());
        assert!(resolver.node_names("external:repo:lib.ts").is_empty());
        assert!(resolver.node_names("local:./b").is_empty());
        assert!(!resolver.resolves("local:./b"));
    }

    #[test]
    fn local_resolver_empty_graph_is_empty() {
        let conn = duckdb::Connection::open_in_memory().unwrap();
        crate::graph::ensure_schema(&conn).unwrap();
        let resolver = LocalSpecResolver::from_edges(&conn).unwrap();
        assert!(resolver.node_names("anything.ts").is_empty());
        assert!(!resolver.resolves("anything.ts"));
    }

    #[test]
    fn local_resolver_treats_query_hash_as_not_part_of_path() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/style.css", "");
        write(dir.path(), "src/raw.ts", "import './style.css?raw'\n");
        write(dir.path(), "src/worker.ts", "import './style.css#inline'\n");
        let conn = graph_with(&[
            ("src/raw.ts", "local:./style.css?raw", "imports"),
            ("src/worker.ts", "local:./style.css#inline", "imports"),
        ]);
        let resolver = LocalSpecResolver::from_edges(&conn).unwrap();
        let target = dir
            .path()
            .join("src/style.css")
            .to_string_lossy()
            .into_owned();
        let mut names = resolver.node_names(&target);
        names.sort();
        assert_eq!(
            names,
            vec![
                "local:./style.css#inline".to_string(),
                "local:./style.css?raw".to_string()
            ],
            "?query / #hash suffixes must not be part of the path"
        );
        drop(dir);
    }
}
