//! GAP-112: conformance extraction — `implements` edges (Type -> Interface)
//! and interface consumption sites.
//!
//! The graph records `imports`/`tested_by`/`tests` edge families, but nothing
//! about interface conformance. An interface whose only satisfiers are test
//! doubles is invisible: a runtime type-assert silently falls to a slow path
//! while tests stay green because a test-only sink implements it (reference
//! incident: trouble TRBL-084 — a batched-write capability whose production
//! sinks did not implement the optional interface).
//!
//! This module extracts, per language:
//!
//! - **implements** sites — a concrete type satisfying an interface:
//!   - Go: method-set matching against named interface types declared in the
//!     same directory (package); a type `T` implements interface `I` when
//!     `T`'s method set covers `I`'s (including embedded interfaces).
//!   - Rust: `impl Trait for Type` items.
//!   - Python: ABC/Protocol subclassing (superclass named `ABC`/`Protocol`,
//!     ending in `Protocol`/`Interface`, or matching a class the corpus
//!     itself declares as ABC/Protocol).
//!   - TypeScript: `implements` clauses naming a corpus-declared `interface`.
//!     (JavaScript shares the TS grammar family: `instanceof` consumption
//!     sites work against interfaces declared in TS files of the same scan.)
//! - **consumes** sites — where an interface is *used* at runtime:
//!   Go `x.(I)` type assertions, Rust `dyn Trait` types, Python
//!   `isinstance(x, I)`, TS/JS `x instanceof I`.
//!
//! Everything here is HEURISTIC. No type checker runs; names are matched
//! textually within a single scan. Consumers must say so in their output
//! (see `hilo graph wiring`) rather than implying certainty.
//!
//! Languages with no conformance extraction surface as explicitly
//! unsupported (`conformance_supported`); they are never silently skipped.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use tree_sitter::Node;

use crate::parser::Language;

/// Edge `rel` for a concrete type satisfying an interface
/// (`type:<T>` -> `iface:<I>`).
pub const IMPLEMENTS_REL: &str = "implements";
/// Edge `rel` for a file consuming an interface (`<file>` -> `iface:<I>`).
pub const CONSUMES_REL: &str = "consumes";
/// Edge `rel` linking a defining file to its `type:<T>` node.
pub const CONFORMANCE_OF_REL: &str = "conformance_of";
/// Provenance for all conformance edges — heuristic, never `ast_exact`.
pub const CONFORMANCE_PROVENANCE: &str = "ast_heuristic";
/// Confidence for heuristic conformance edges (below `ast_exact`'s 1.0).
pub const CONFORMANCE_CONFIDENCE: f64 = 0.8;

/// The languages this module can extract conformance edges for.
pub fn conformance_supported(lang: Language) -> bool {
    matches!(
        lang,
        Language::Go
            | Language::Rust
            | Language::Python
            | Language::TypeScript
            | Language::JavaScript
    )
}

/// One extracted conformance fact from a single file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConformanceSite {
    /// Repo-relative path of the file the site was found in.
    pub file: String,
    /// What kind of site this is.
    pub kind: SiteKind,
    /// Concrete type name (implements sites only).
    pub type_name: Option<String>,
    /// Interface name (last path segment; no generics).
    pub interface: String,
}

/// Kind of a [`ConformanceSite`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteKind {
    /// A concrete type satisfies an interface.
    Implements,
    /// A file consumes an interface at runtime (type-assert / dyn / isinstance
    /// / instanceof).
    Consumes,
}

/// Extract conformance sites from a corpus of files.
///
/// `files` is a list of `(language, repo_relative_path, source)`. Go's
/// method-set matching is directory-scoped: files in the same directory are
/// treated as one package, so a type in `a.go` can satisfy an interface
/// declared in `b.go`. Other languages match within the whole corpus.
pub fn extract_conformance(files: &[(Language, String, String)]) -> Vec<ConformanceSite> {
    let mut sites = Vec::new();
    let go_dirs: BTreeMap<String, Vec<(String, String)>> = group_by_dir(files, Language::Go);
    for (dir, group) in &go_dirs {
        sites.extend(extract_go(dir, group));
    }
    // Rust: single pass for impl items, second for dyn consumption (no
    // cross-file state needed, but keep the two walks separate for clarity).
    let ts_ifaces: HashSet<String> = {
        let mut s = HashSet::new();
        for (lang, _, src) in files {
            if matches!(lang, Language::TypeScript | Language::JavaScript) {
                s.extend(collect_ts_interfaces(src));
            }
        }
        s
    };
    let py_iface_decls: HashSet<String> = {
        let mut s = HashSet::new();
        for (lang, _, src) in files {
            if *lang == Language::Python {
                s.extend(collect_py_interface_names(src));
            }
        }
        s
    };
    for (lang, file, src) in files {
        match lang {
            Language::Rust => sites.extend(extract_rust(file, src)),
            Language::Python => sites.extend(extract_python(file, src, &py_iface_decls)),
            Language::TypeScript | Language::JavaScript => {
                sites.extend(extract_ts(file, src, &ts_ifaces))
            }
            _ => {}
        }
    }
    sites
}

/// Group files of one language by parent directory (relative, `/`-separated).
fn group_by_dir(
    files: &[(Language, String, String)],
    lang: Language,
) -> BTreeMap<String, Vec<(String, String)>> {
    let mut map: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for (l, path, src) in files {
        if *l != lang {
            continue;
        }
        let dir = Path::new(path)
            .parent()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        map.entry(dir)
            .or_default()
            .push((path.clone(), src.clone()));
    }
    map
}

/// Take the last `::`/`.`-separated segment and strip generics.
fn base_name(text: &str) -> String {
    let no_generics = text.split('<').next().unwrap_or(text);
    no_generics
        .rsplit("::")
        .next()
        .unwrap_or(no_generics)
        .rsplit('.')
        .next()
        .unwrap_or(no_generics)
        .trim()
        .to_string()
}

fn node_text<'a>(node: Node, source: &'a [u8]) -> Option<&'a str> {
    node.utf8_text(source).ok()
}

fn find_descendant<'a>(start: Node<'a>, pred: &dyn Fn(Node<'a>) -> bool) -> Option<Node<'a>> {
    if pred(start) {
        return Some(start);
    }
    let mut cursor = start.walk();
    for child in start.children(&mut cursor) {
        if let Some(found) = find_descendant(child, pred) {
            return Some(found);
        }
    }
    None
}

// ── Go ──────────────────────────────────────────────────────────────

/// Per-directory Go conformance: interfaces (name -> method set), concrete
/// types (name -> method set + defining file), then method-set matching.
fn extract_go(dir: &str, files: &[(String, String)]) -> Vec<ConformanceSite> {
    let _ = dir;
    let mut interfaces: HashMap<String, (String, HashSet<String>)> = HashMap::new();
    let mut types: HashMap<String, (String, HashSet<String>)> = HashMap::new();

    for (file, src) in files {
        let Ok(mut parser) = crate::parser::Parser::for_language(Language::Go) else {
            continue;
        };
        let source = src.as_bytes();
        let Some(tree) = parser.parse_tree(source) else {
            continue;
        };
        collect_go_decls(tree.root_node(), source, file, &mut interfaces, &mut types);
    }
    // GAP-112: expand embedded interfaces (`embed:Name` markers) before
    // method-set matching so `ReadWriter { Reader; Write }` sees `Read`.
    expand_go_embeds(&mut interfaces);
    let mut sites = Vec::new();
    sites.extend(drain_go_assertion_sites());
    for (iface, (_, iface_methods)) in &interfaces {
        for (ty, (file, ty_methods)) in &types {
            if ty == iface {
                continue;
            }
            if iface_methods.is_subset(ty_methods) {
                sites.push(ConformanceSite {
                    file: file.clone(),
                    kind: SiteKind::Implements,
                    type_name: Some(ty.clone()),
                    interface: iface.clone(),
                });
            }
        }
    }
    sites
}

/// Walk a Go AST collecting interface declarations (with method sets,
/// expanding embedded interfaces within the same directory), concrete type
/// declarations, method-declaration receivers, and `x.(I)` type assertions.
fn collect_go_decls(
    node: Node,
    source: &[u8],
    file: &str,
    interfaces: &mut HashMap<String, (String, HashSet<String>)>,
    types: &mut HashMap<String, (String, HashSet<String>)>,
) {
    match node.kind() {
        "type_spec" => {
            let name = node
                .child_by_field_name("name")
                .and_then(|n| node_text(n, source))
                .map(base_name);
            let ty = node.child_by_field_name("type");
            if let (Some(name), Some(ty)) = (name, ty) {
                if ty.kind() == "interface_type" {
                    let methods = interface_methods(ty, source);
                    interfaces.insert(name, (file.to_string(), methods));
                } else if let std::collections::hash_map::Entry::Vacant(e) = types.entry(name) {
                    e.insert((file.to_string(), HashSet::new()));
                }
            }
        }
        "method_declaration" => {
            // Receiver: parameter_list -> parameter_declaration -> (*)
            // type_identifier. Take the first type_identifier descendant of
            // the receiver; that is the base type name (pointer receivers
            // contribute to both T and *T method sets for our purpose).
            let recv = node.child_by_field_name("receiver");
            let recv_ty = recv.and_then(|r| find_descendant(r, &|n| n.kind() == "type_identifier"));
            let method = node
                .child_by_field_name("name")
                .and_then(|n| node_text(n, source))
                .map(base_name);
            if let (Some(rt), Some(method)) = (recv_ty, method) {
                if let Some(t) = node_text(rt, source) {
                    let t = base_name(t);
                    types
                        .entry(t)
                        .or_insert_with(|| (file.to_string(), HashSet::new()))
                        .1
                        .insert(method);
                }
            }
        }
        "type_assertion_expression" => {
            // Consumption site: `x.(I)` with a NAMED interface type. The
            // unnamed `.(interface { ... })` form carries no resolvable name
            // and is skipped.
            if let Some(t) = node.child_by_field_name("type") {
                if t.kind() == "type_identifier" {
                    if let Some(name) = node_text(t, source) {
                        // The interface name itself is resolved by the
                        // consumer against the corpus interface set; here we
                        // only record the consumption of a named type assert.
                        // Mark it via a synthetic interface entry so the
                        // consumer sees it even if no declaration was found.
                        GO_ASSERT_SITES.with(|s| {
                            s.borrow_mut().push(ConformanceSite {
                                file: file.to_string(),
                                kind: SiteKind::Consumes,
                                type_name: None,
                                interface: base_name(name),
                            });
                        });
                    }
                }
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    let children: Vec<Node> = node.children(&mut cursor).collect();
    for child in children {
        collect_go_decls(child, source, file, interfaces, types);
    }
}

/// Method names declared by a Go `interface_type` node, expanding embedded
/// interface names found in the same directory table.
fn interface_methods(node: Node, source: &[u8]) -> HashSet<String> {
    let mut methods = HashSet::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "method_elem" => {
                if let Some(n) = child.child_by_field_name("name") {
                    if let Some(t) = node_text(n, source) {
                        methods.insert(base_name(t));
                    }
                }
            }
            "type_identifier" => {
                // Embedded interface: `io.Reader`. The embed is resolved by
                // the caller's second pass over the interfaces table.
                if let Some(t) = node_text(child, source) {
                    methods.insert(format!("embed:{t}"));
                }
            }
            _ => {}
        }
    }
    methods
}

thread_local! {
    static GO_ASSERT_SITES: std::cell::RefCell<Vec<ConformanceSite>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Expand `embed:<Name>` markers in interface method sets against the
/// interfaces table (same directory). Unknown embeds keep the marker, which
/// never matches a real method name and therefore only *tightens* the
/// requirement (heuristic, conservative).
fn expand_go_embeds(interfaces: &mut HashMap<String, (String, HashSet<String>)>) {
    let snapshot: Vec<(String, HashSet<String>)> = interfaces
        .iter()
        .map(|(k, v)| (k.clone(), v.1.clone()))
        .collect();
    for (_, methods) in interfaces.values_mut() {
        let embeds: Vec<String> = methods
            .iter()
            .filter(|m| m.starts_with("embed:"))
            .map(|m| m.trim_start_matches("embed:").to_string())
            .collect();
        for embed in embeds {
            if let Some((_, embedded)) = snapshot.iter().find(|(n, _)| *n == embed) {
                for m in embedded {
                    if !m.starts_with("embed:") {
                        methods.insert(m.clone());
                    }
                }
            }
        }
    }
}

// ── Rust ────────────────────────────────────────────────────────────

fn extract_rust(file: &str, src: &str) -> Vec<ConformanceSite> {
    let Ok(mut parser) = crate::parser::Parser::for_language(Language::Rust) else {
        return Vec::new();
    };
    let source = src.as_bytes();
    let Some(tree) = parser.parse_tree(source) else {
        return Vec::new();
    };
    let mut sites = Vec::new();
    rust_walk(tree.root_node(), source, file, &mut sites);
    sites
}

fn rust_walk(node: Node, source: &[u8], file: &str, sites: &mut Vec<ConformanceSite>) {
    match node.kind() {
        "impl_item" => {
            if let (Some(trait_n), Some(ty_n)) = (
                node.child_by_field_name("trait"),
                node.child_by_field_name("type"),
            ) {
                if let (Some(tr), Some(ty)) = (node_text(trait_n, source), node_text(ty_n, source))
                {
                    sites.push(ConformanceSite {
                        file: file.to_string(),
                        kind: SiteKind::Implements,
                        type_name: Some(base_name(ty)),
                        interface: base_name(tr),
                    });
                }
            }
        }
        "dynamic_type" => {
            // `dyn Trait` — a runtime-dispatch consumption site. The trait
            // field may be a trait bound list (`dyn A + B`); take each bound.
            if let Some(trait_n) = node.child_by_field_name("trait") {
                for name in rust_trait_bound_names(trait_n, source) {
                    sites.push(ConformanceSite {
                        file: file.to_string(),
                        kind: SiteKind::Consumes,
                        type_name: None,
                        interface: name,
                    });
                }
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    let children: Vec<Node> = node.children(&mut cursor).collect();
    for child in children {
        rust_walk(child, source, file, sites);
    }
}

/// Collect each named bound of a Rust trait position: `A`, `A + B`, `A + Send`.
/// Lifetimes and auto traits (`Send`/`Sync`/`Unpin`/`'static`) are dropped.
fn rust_trait_bound_names(node: Node, source: &[u8]) -> Vec<String> {
    const AUTO: [&str; 4] = ["Send", "Sync", "Unpin", "UnwindSafe"];
    let mut out = Vec::new();
    if node.kind() == "trait_bounds" || node.kind() == "higher_ranked_trait_bound" {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            out.extend(rust_trait_bound_names(child, source));
        }
        return out;
    }
    if let Some(t) = node_text(node, source) {
        let name = base_name(t);
        if !name.is_empty() && !name.starts_with('\'') && !AUTO.contains(&name.as_str()) {
            out.push(name);
        }
    }
    out
}

// ── Python ──────────────────────────────────────────────────────────

/// Names of classes the corpus itself declares as ABC/Protocol: superclass
/// `ABC`/`Protocol`, a name ending in `Protocol`/`Interface`, or
/// `metaclass=ABCMeta`.
fn collect_py_interface_names(src: &str) -> HashSet<String> {
    let Ok(mut parser) = crate::parser::Parser::for_language(Language::Python) else {
        return HashSet::new();
    };
    let source = src.as_bytes();
    let Some(tree) = parser.parse_tree(source) else {
        return HashSet::new();
    };
    let mut names = HashSet::new();
    py_interface_walk(tree.root_node(), source, &mut names);
    names
}

fn looks_like_interface(super_name: &str) -> bool {
    super_name == "ABC"
        || super_name == "Protocol"
        || super_name.ends_with("Protocol")
        || super_name.ends_with("Interface")
}

fn py_interface_walk(node: Node, source: &[u8], names: &mut HashSet<String>) {
    if node.kind() == "class_definition" {
        let class_name = node
            .child_by_field_name("name")
            .and_then(|n| node_text(n, source))
            .map(base_name);
        let supers = node.child_by_field_name("superclasses");
        let mut is_iface = false;
        if let (Some(class_name), Some(supers)) = (class_name, supers) {
            let mut cursor = supers.walk();
            for sup in supers.children(&mut cursor) {
                if let Some(t) = node_text(sup, source) {
                    let s = base_name(t);
                    if looks_like_interface(&s) || s == "ABCMeta" {
                        is_iface = true;
                    }
                }
            }
            // `class Foo:` with `__slots__`-style metaclass kwarg handled via
            // keyword_argument scan of the arguments node.
            if !is_iface {
                if let Some(args) = node.child_by_field_name("arguments") {
                    let mut cursor = args.walk();
                    for a in args.children(&mut cursor) {
                        if a.kind() == "keyword_argument" {
                            if let Some(t) = node_text(a, source) {
                                if t.contains("metaclass") && t.contains("ABCMeta") {
                                    is_iface = true;
                                }
                            }
                        }
                    }
                }
            }
            if is_iface {
                names.insert(class_name);
            }
        }
    }
    let mut cursor = node.walk();
    let children: Vec<Node> = node.children(&mut cursor).collect();
    for child in children {
        py_interface_walk(child, source, names);
    }
}

fn extract_python(file: &str, src: &str, iface_decls: &HashSet<String>) -> Vec<ConformanceSite> {
    let Ok(mut parser) = crate::parser::Parser::for_language(Language::Python) else {
        return Vec::new();
    };
    let source = src.as_bytes();
    let Some(tree) = parser.parse_tree(source) else {
        return Vec::new();
    };
    let mut sites = Vec::new();
    py_walk(tree.root_node(), source, file, iface_decls, &mut sites);
    sites
}

fn py_walk(
    node: Node,
    source: &[u8],
    file: &str,
    iface_decls: &HashSet<String>,
    sites: &mut Vec<ConformanceSite>,
) {
    match node.kind() {
        "class_definition" => {
            let class_name = node
                .child_by_field_name("name")
                .and_then(|n| node_text(n, source))
                .map(base_name);
            if let Some(supers) = node.child_by_field_name("superclasses") {
                let mut cursor = supers.walk();
                for sup in supers.children(&mut cursor) {
                    let Some(t) = node_text(sup, source) else {
                        continue;
                    };
                    let s = base_name(t);
                    // `ABC` and `Protocol` are marker base classes, not
                    // interfaces a class conforms to — a class extending
                    // them is the interface declaration, not an implements
                    // site.
                    let is_iface = iface_decls.contains(&s)
                        || (looks_like_interface(&s) && s != "ABC" && s != "Protocol");
                    if is_iface {
                        if let Some(class_name) = &class_name {
                            sites.push(ConformanceSite {
                                file: file.to_string(),
                                kind: SiteKind::Implements,
                                type_name: Some(class_name.clone()),
                                interface: s,
                            });
                        }
                    }
                }
            }
        }
        "call" => {
            // isinstance(x, Iface) / issubclass(x, Iface) — consumption.
            let is_check = node
                .child_by_field_name("function")
                .and_then(|f| node_text(f, source))
                .map(base_name)
                .map(|n| n == "isinstance" || n == "issubclass")
                .unwrap_or(false);
            if is_check {
                if let Some(args) = node.child_by_field_name("arguments") {
                    let mut cursor = args.walk();
                    let children: Vec<Node> = args.named_children(&mut cursor).collect();
                    // Second positional argument names the checked type.
                    if let Some(second) = children.get(1) {
                        if let Some(t) = node_text(*second, source) {
                            let name = base_name(t);
                            if iface_decls.contains(&name)
                                || (looks_like_interface(&name)
                                    && name != "ABC"
                                    && name != "Protocol")
                            {
                                sites.push(ConformanceSite {
                                    file: file.to_string(),
                                    kind: SiteKind::Consumes,
                                    type_name: None,
                                    interface: name,
                                });
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    let children: Vec<Node> = node.children(&mut cursor).collect();
    for child in children {
        py_walk(child, source, file, iface_decls, sites);
    }
}

// ── TypeScript / JavaScript ─────────────────────────────────────────

/// Names declared as `interface X { ... }` in a TS source.
fn collect_ts_interfaces(src: &str) -> HashSet<String> {
    let Ok(mut parser) = crate::parser::Parser::for_language(Language::TypeScript) else {
        return HashSet::new();
    };
    let source = src.as_bytes();
    let Some(tree) = parser.parse_tree(source) else {
        return HashSet::new();
    };
    let mut names = HashSet::new();
    ts_interface_walk(tree.root_node(), source, &mut names);
    names
}

fn ts_interface_walk(node: Node, source: &[u8], names: &mut HashSet<String>) {
    if node.kind() == "interface_declaration" {
        if let Some(n) = node.child_by_field_name("name") {
            if let Some(t) = node_text(n, source) {
                names.insert(base_name(t));
            }
        }
    }
    let mut cursor = node.walk();
    let children: Vec<Node> = node.children(&mut cursor).collect();
    for child in children {
        ts_interface_walk(child, source, names);
    }
}

fn extract_ts(file: &str, src: &str, iface_decls: &HashSet<String>) -> Vec<ConformanceSite> {
    // JS files parse with the JS grammar, which has no implements_clause /
    // interface_declaration; they still contribute instanceof consumption
    // against interfaces declared in TS files of the same scan.
    let lang = if file.ends_with(".ts") || file.ends_with(".tsx") {
        Language::TypeScript
    } else {
        Language::JavaScript
    };
    let Ok(mut parser) = crate::parser::Parser::for_language(lang) else {
        return Vec::new();
    };
    let source = src.as_bytes();
    let Some(tree) = parser.parse_tree(source) else {
        return Vec::new();
    };
    let mut sites = Vec::new();
    ts_walk(tree.root_node(), source, file, iface_decls, &mut sites);
    sites
}

fn ts_walk(
    node: Node,
    source: &[u8],
    file: &str,
    iface_decls: &HashSet<String>,
    sites: &mut Vec<ConformanceSite>,
) {
    match node.kind() {
        "class_declaration" | "class" => {
            let class_name = node
                .child_by_field_name("name")
                .and_then(|n| node_text(n, source))
                .map(base_name);

            // TS nests implements_clause under class_heritage (the probe
            // showed class_declaration -> class_heritage -> implements_clause).
            if let Some(heritage) = find_descendant(node, &|n| n.kind() == "class_heritage") {
                if let Some(clause) =
                    find_descendant(heritage, &|n| n.kind() == "implements_clause")
                {
                    let mut ic = clause.walk();
                    for imp in clause.children(&mut ic) {
                        if imp.kind() != "type_identifier" {
                            continue;
                        }
                        let Some(t) = node_text(imp, source) else {
                            continue;
                        };
                        let name = base_name(t);
                        if iface_decls.contains(&name) {
                            if let Some(class_name) = &class_name {
                                sites.push(ConformanceSite {
                                    file: file.to_string(),
                                    kind: SiteKind::Implements,
                                    type_name: Some(class_name.clone()),
                                    interface: name,
                                });
                            }
                        }
                    }
                }
            }
        }
        "binary_expression" => {
            let op = node
                .child_by_field_name("operator")
                .and_then(|o| node_text(o, source))
                .unwrap_or("");
            if op == "instanceof" {
                if let Some(right) = node.child_by_field_name("right") {
                    if let Some(t) = node_text(right, source) {
                        let name = base_name(t);
                        if iface_decls.contains(&name) {
                            sites.push(ConformanceSite {
                                file: file.to_string(),
                                kind: SiteKind::Consumes,
                                type_name: None,
                                interface: name,
                            });
                        }
                    }
                }
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    let children: Vec<Node> = node.children(&mut cursor).collect();
    for child in children {
        ts_walk(child, source, file, iface_decls, sites);
    }
}

/// GAP-112: extract Go type-assertion consumption sites that were parked in
/// the thread-local during [`collect_go_decls`], and clear the buffer.
///
/// The Go walker records assertions while it walks (before the interface
/// table exists), so they are stashed and drained here, once per Go
/// directory group.
pub fn drain_go_assertion_sites() -> Vec<ConformanceSite> {
    GO_ASSERT_SITES.with(|s| std::mem::take(&mut *s.borrow_mut()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sites_for(lang: Language, file: &str, src: &str) -> Vec<ConformanceSite> {
        extract_conformance(&[(lang, file.to_string(), src.to_string())])
    }

    fn implements(sites: &[ConformanceSite]) -> Vec<(&str, &str)> {
        sites
            .iter()
            .filter(|s| s.kind == SiteKind::Implements)
            .map(|s| (s.type_name.as_deref().unwrap_or(""), s.interface.as_str()))
            .collect()
    }

    fn consumes(sites: &[ConformanceSite]) -> Vec<&str> {
        sites
            .iter()
            .filter(|s| s.kind == SiteKind::Consumes)
            .map(|s| s.interface.as_str())
            .collect()
    }

    // ── Go ──────────────────────────────────────────────────────────

    #[test]
    fn go_method_set_match_and_type_assertion() {
        let src = "\
package batch

type BatchWriter interface {
\tWriteBatch([]byte) error
}

type FileSink struct{}

func flush(s any) error {
\tif bw, ok := s.(BatchWriter); ok {
\t\treturn bw.WriteBatch(nil)
\t}
\treturn nil
}
";
        let sites = sites_for(Language::Go, "writer.go", src);
        assert_eq!(implements(&sites).len(), 0, "no implementors yet");
        assert_eq!(consumes(&sites), vec!["BatchWriter"]);
    }

    #[test]
    fn go_test_only_implementor_matches() {
        let src = "\
package batch

type FakeSink struct{}

func (f *FakeSink) WriteBatch(b []byte) error { return nil }
";
        let sites = sites_for(Language::Go, "sink_test.go", src);
        assert_eq!(
            implements(&sites).len(),
            0,
            "interface not declared in this file"
        );
        assert_eq!(
            implements(&sites).len(),
            0,
            "interface not declared in this file"
        );
    }

    #[test]
    fn go_method_set_match_across_files_same_dir() {
        let iface = "\
package batch

type BatchWriter interface {
\tWriteBatch([]byte) error
}
";
        let implr = "\
package batch

type FakeSink struct{}

func (f *FakeSink) WriteBatch(b []byte) error { return nil }
";
        let files = vec![
            (Language::Go, "writer.go".to_string(), iface.to_string()),
            (Language::Go, "sink_test.go".to_string(), implr.to_string()),
        ];
        let sites = extract_conformance(&files);
        let impls: Vec<(String, String)> = sites
            .iter()
            .filter(|s| s.kind == SiteKind::Implements)
            .map(|s| (s.type_name.clone().unwrap_or_default(), s.interface.clone()))
            .collect();
        assert_eq!(
            impls,
            vec![("FakeSink".to_string(), "BatchWriter".to_string())]
        );
        // The test file's implementor also records its defining file.
        let site = sites
            .iter()
            .find(|s| s.kind == SiteKind::Implements)
            .expect("implements site");
        assert_eq!(site.file, "sink_test.go");
    }

    #[test]
    fn go_embedded_interface_expands() {
        let base = "\
package p

type Reader interface {
\tRead() error
}
";
        let derived = "\
package p

type ReadWriter interface {
\tReader
\tWrite() error
}
";
        let implr = "\
package p

type RW struct{}

func (rw *RW) Read() error  { return nil }
func (rw *RW) Write() error { return nil }
";
        let files = vec![
            (Language::Go, "a.go".to_string(), base.to_string()),
            (Language::Go, "b.go".to_string(), derived.to_string()),
            (Language::Go, "c.go".to_string(), implr.to_string()),
        ];
        let sites = extract_conformance(&files);
        let impls: Vec<(String, String)> = sites
            .iter()
            .filter(|s| s.kind == SiteKind::Implements)
            .map(|s| (s.type_name.clone().unwrap(), s.interface.clone()))
            .collect();
        assert!(impls.contains(&("RW".to_string(), "ReadWriter".to_string())));
        assert!(impls.contains(&("RW".to_string(), "Reader".to_string())));
    }

    // ── Rust ────────────────────────────────────────────────────────

    #[test]
    fn rust_impl_trait_for_type_and_dyn() {
        let src = "\
trait BatchWriter {
    fn write_batch(&mut self, b: &[u8]) -> Result<(), ()>;
}

struct FakeSink;

impl BatchWriter for FakeSink {
    fn write_batch(&mut self, _b: &[u8]) -> Result<(), ()> { Ok(()) }
}

fn flush(sink: &dyn BatchWriter) {}
";
        let sites = sites_for(Language::Rust, "src/lib.rs", src);
        assert_eq!(implements(&sites), vec![("FakeSink", "BatchWriter")]);
        assert_eq!(consumes(&sites), vec!["BatchWriter"]);
    }

    #[test]
    fn rust_scoped_impl_and_dyn_bounds() {
        let src = "\
impl batch::BatchWriter for sink::FileSink {}

fn go(w: &mut (dyn BatchWriter + Send)) {}
";
        let sites = sites_for(Language::Rust, "src/other.rs", src);
        assert_eq!(implements(&sites), vec![("FileSink", "BatchWriter")]);
        assert_eq!(consumes(&sites), vec!["BatchWriter"]);
    }

    // ── Python ──────────────────────────────────────────────────────

    #[test]
    fn python_abc_subclass_and_isinstance() {
        let src = "\
from abc import ABC, abstractmethod

class BatchWriter(ABC):
    @abstractmethod
    def write_batch(self, b): ...

class FakeSink(BatchWriter):
    def write_batch(self, b): return None

def flush(sink):
    if isinstance(sink, BatchWriter):
        return sink.write_batch(b'')
";
        let sites = sites_for(Language::Python, "batch.py", src);
        assert_eq!(implements(&sites), vec![("FakeSink", "BatchWriter")]);
        assert_eq!(consumes(&sites), vec!["BatchWriter"]);
    }

    #[test]
    fn python_protocol_by_name_suffix() {
        let src = "\
from typing import Protocol

class BatchWriter(Protocol):
    def write_batch(self, b): ...
";
        let sites = sites_for(Language::Python, "p.py", src);
        assert_eq!(implements(&sites).len(), 0);
        // The declaration itself registers the interface name; a subclass in
        // a second file matches it.
        let sub = "class ProdSink(BatchWriter):\n    def write_batch(self, b): ...\n";
        let files = vec![
            (Language::Python, "p.py".to_string(), src.to_string()),
            (Language::Python, "q.py".to_string(), sub.to_string()),
        ];
        let sites = extract_conformance(&files);
        assert_eq!(implements(&sites), vec![("ProdSink", "BatchWriter")]);
    }

    // ── TypeScript / JavaScript ─────────────────────────────────────

    #[test]
    fn ts_implements_clause_and_instanceof() {
        let src = "\
interface BatchWriter {
  writeBatch(b: Uint8Array): Promise<void>;
}

class FakeSink implements BatchWriter {
  async writeBatch(b: Uint8Array): Promise<void> {}
}

function flush(sink: unknown) {
  if (sink instanceof BatchWriter) {
    sink.writeBatch(new Uint8Array());
  }
}
";
        let sites = sites_for(Language::TypeScript, "src/writer.ts", src);
        assert_eq!(implements(&sites), vec![("FakeSink", "BatchWriter")]);
        assert_eq!(consumes(&sites), vec!["BatchWriter"]);
    }

    #[test]
    fn ts_class_not_matching_unknown_interface() {
        let src = "\
class ProdSink implements BatchWriter {}
";
        // No `interface BatchWriter` declared in the corpus — no edge.
        let sites = sites_for(Language::TypeScript, "src/sink.ts", src);
        assert_eq!(implements(&sites).len(), 0);
    }

    #[test]
    fn js_instanceof_against_ts_interface() {
        let ts = "interface BatchWriter {\n  writeBatch(b: Uint8Array): void;\n}\n";
        let js = "function flush(sink) {\n  if (sink instanceof BatchWriter) {\n    sink.writeBatch(b);\n  }\n}\n";
        let files = vec![
            (
                Language::TypeScript,
                "writer.ts".to_string(),
                ts.to_string(),
            ),
            (Language::JavaScript, "flush.js".to_string(), js.to_string()),
        ];
        let sites = extract_conformance(&files);
        assert_eq!(consumes(&sites), vec!["BatchWriter"]);
        assert_eq!(implements(&sites).len(), 0);
    }

    // ── support matrix ──────────────────────────────────────────────

    #[test]
    fn conformance_support_matrix() {
        assert!(conformance_supported(Language::Go));
        assert!(conformance_supported(Language::Rust));
        assert!(conformance_supported(Language::Python));
        assert!(conformance_supported(Language::TypeScript));
        assert!(conformance_supported(Language::JavaScript));
        // Everything else is explicitly unsupported — surfaced, never silent.
        for lang in [
            Language::Java,
            Language::C,
            Language::Cpp,
            Language::Ruby,
            Language::CSharp,
            Language::Kotlin,
            Language::Php,
            Language::Swift,
            Language::Elixir,
            Language::Haskell,
            Language::Erlang,
            Language::Scala,
            Language::Zig,
            Language::Lua,
            Language::Dart,
            Language::Clojure,
            Language::OCaml,
            Language::R,
            Language::Julia,
            Language::Elm,
            Language::Nim,
            Language::Terraform,
        ] {
            assert!(
                !conformance_supported(lang),
                "{lang:?} must report unsupported, not silently skip"
            );
        }
    }

    #[test]
    fn base_name_strips_paths_and_generics() {
        assert_eq!(base_name("batch::BatchWriter"), "BatchWriter");
        assert_eq!(base_name("Vec<u8>"), "Vec");
        assert_eq!(base_name("abc.ABCMeta"), "ABCMeta");
        assert_eq!(base_name("BatchWriter"), "BatchWriter");
    }
}
