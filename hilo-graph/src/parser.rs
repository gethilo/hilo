//! AST parsing with tree-sitter for 26 languages.
//!
//! Supports Go, Python, TypeScript, Rust, JavaScript, Java, C, C++, Ruby,
//! C#, Kotlin, PHP, Swift, Elixir, Haskell, Erlang, Scala, Zig, Lua, Dart,
//! Clojure, OCaml, R, Julia, Elm, and Nim.
//! Each language gets a dedicated extraction function that understands its
//! specific import/dependency syntax.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use hilo_metadata::inventory::Edge;
use tree_sitter::{Node, Parser as TsParser};

use crate::error::{GraphError, GraphResult};

/// Languages supported by the multi-language parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Language {
    Go,
    Python,
    TypeScript,
    Rust,
    JavaScript,
    Java,
    C,
    Cpp,
    Ruby,
    CSharp,
    Kotlin,
    Php,
    Swift,
    Elixir,
    Haskell,
    Erlang,
    Scala,
    Zig,
    Lua,
    Dart,
    Clojure,
    OCaml,
    R,
    Julia,
    Elm,
    Nim,
}

impl Language {
    /// Detect language from a file extension.
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "go" => Some(Language::Go),
            "py" => Some(Language::Python),
            "ts" | "tsx" => Some(Language::TypeScript),
            "rs" => Some(Language::Rust),
            "js" | "jsx" => Some(Language::JavaScript),
            "java" => Some(Language::Java),
            "c" => Some(Language::C),
            "cpp" | "cc" | "cxx" | "hpp" | "hxx" => Some(Language::Cpp),
            "rb" => Some(Language::Ruby),
            "cs" => Some(Language::CSharp),
            "kt" | "kts" => Some(Language::Kotlin),
            "php" | "phtml" => Some(Language::Php),
            "swift" => Some(Language::Swift),
            "ex" | "exs" => Some(Language::Elixir),
            "hs" | "lhs" => Some(Language::Haskell),
            "erl" | "hrl" => Some(Language::Erlang),
            "scala" | "sc" => Some(Language::Scala),
            "zig" => Some(Language::Zig),
            "lua" => Some(Language::Lua),
            "dart" => Some(Language::Dart),
            "clj" | "cljs" | "cljc" | "edn" => Some(Language::Clojure),
            "ml" | "mli" => Some(Language::OCaml),
            "r" | "R" => Some(Language::R),
            "jl" => Some(Language::Julia),
            "elm" => Some(Language::Elm),
            "nim" => Some(Language::Nim),
            _ => None,
        }
    }

    /// All extensions this parser handles.
    pub fn all_extensions() -> &'static [&'static str] {
        &[
            "go", "py", "ts", "tsx", "rs", "js", "jsx", "java", "c", "cpp", "cc", "cxx", "rb",
            "cs", "kt", "kts", "php", "phtml", "swift", "ex", "exs", "hs", "lhs", "erl", "hrl",
            "scala", "sc", "zig", "lua", "dart", "clj", "cljs", "cljc", "edn", "ml", "mli", "r",
            "jl", "elm", "nim",
        ]
    }
}

/// Multi-language parser that extracts dependency edges from source files.
///
/// Construct with [`Parser::for_language`] and reuse across files of the same
/// language by calling [`Parser::parse_imports`] for each source file.
pub struct Parser {
    parser: TsParser,
    language: Language,
}

// ── Language detection ──────────────────────────────────────────────

impl Parser {
    /// Create a new [`Parser`] for the given language.
    pub fn for_language(language: Language) -> GraphResult<Self> {
        let mut parser = TsParser::new();
        let lang = match language {
            Language::Go => tree_sitter_go::LANGUAGE.into(),
            Language::Python => tree_sitter_python::LANGUAGE.into(),
            Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Language::Rust => tree_sitter_rust::LANGUAGE.into(),
            Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Language::Java => tree_sitter_java::LANGUAGE.into(),
            Language::C => tree_sitter_c::LANGUAGE.into(),
            Language::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            Language::Ruby => tree_sitter_ruby::LANGUAGE.into(),
            Language::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
            Language::Kotlin => tree_sitter_kotlin_ng::LANGUAGE.into(),
            Language::Php => tree_sitter_php::LANGUAGE_PHP.into(),
            Language::Swift => tree_sitter_swift::LANGUAGE.into(),
            Language::Elixir => tree_sitter_elixir::LANGUAGE.into(),
            Language::Haskell => tree_sitter_haskell::LANGUAGE.into(),
            Language::Erlang => tree_sitter_erlang::LANGUAGE.into(),
            Language::Scala => tree_sitter_scala::LANGUAGE.into(),
            Language::Zig => tree_sitter_zig::LANGUAGE.into(),
            Language::Lua => tree_sitter_lua::LANGUAGE.into(),
            Language::Dart => tree_sitter_dart::LANGUAGE.into(),
            Language::Clojure => tree_sitter_clojure::LANGUAGE.into(),
            Language::OCaml => tree_sitter_ocaml::LANGUAGE_OCAML.into(),
            Language::R => tree_sitter_r::LANGUAGE.into(),
            Language::Julia => tree_sitter_julia::LANGUAGE.into(),
            Language::Elm => tree_sitter_elm::LANGUAGE.into(),
            Language::Nim => tree_sitter_nim::language(),
        };
        parser.set_language(&lang)?;
        Ok(Parser { parser, language })
    }

    /// Parse a source file and return the dependency edges it declares.
    ///
    /// Each import/include/require declaration produces one [`Edge`] with
    /// `rel = "imports"`. The `from` field is `file_path`, and `to` is the
    /// classified dependency target with a language-appropriate prefix.
    pub fn parse_imports(&mut self, file_path: &str, source: &str) -> GraphResult<Vec<Edge>> {
        let tree = self
            .parser
            .parse(source.as_bytes(), None)
            .ok_or_else(|| GraphError::Other("tree-sitter produced no parse tree".to_string()))?;

        let mut paths: Vec<String> = Vec::new();
        match self.language {
            Language::Go => extract_go_imports(tree.root_node(), source.as_bytes(), &mut paths),
            Language::Python => {
                extract_python_imports(tree.root_node(), source.as_bytes(), &mut paths)
            }
            Language::TypeScript | Language::JavaScript => {
                extract_js_imports(tree.root_node(), source.as_bytes(), &mut paths);
            }
            Language::Rust => {
                let ctx = RustModuleCtx::build(file_path);
                extract_rust_imports(tree.root_node(), source.as_bytes(), &mut paths, &ctx)
            }
            Language::Java => extract_java_imports(tree.root_node(), source.as_bytes(), &mut paths),
            Language::C | Language::Cpp => {
                extract_c_imports(tree.root_node(), source.as_bytes(), &mut paths)
            }
            Language::Ruby => extract_ruby_imports(tree.root_node(), source.as_bytes(), &mut paths),
            Language::CSharp => {
                extract_csharp_imports(tree.root_node(), source.as_bytes(), &mut paths)
            }
            Language::Kotlin => {
                extract_kotlin_imports(tree.root_node(), source.as_bytes(), &mut paths)
            }
            Language::Php => extract_php_imports(tree.root_node(), source.as_bytes(), &mut paths),
            Language::Swift => {
                extract_swift_imports(tree.root_node(), source.as_bytes(), &mut paths)
            }
            Language::Elixir => {
                extract_elixir_imports(tree.root_node(), source.as_bytes(), &mut paths)
            }
            Language::Haskell => {
                extract_haskell_imports(tree.root_node(), source.as_bytes(), &mut paths)
            }
            Language::Erlang => {
                extract_erlang_imports(tree.root_node(), source.as_bytes(), &mut paths)
            }
            Language::Scala => {
                extract_scala_imports(tree.root_node(), source.as_bytes(), &mut paths)
            }
            Language::Zig => extract_zig_imports(tree.root_node(), source.as_bytes(), &mut paths),
            Language::Lua => extract_lua_imports(tree.root_node(), source.as_bytes(), &mut paths),
            Language::Dart => extract_dart_imports(tree.root_node(), source.as_bytes(), &mut paths),
            Language::Clojure => {
                extract_clojure_imports(tree.root_node(), source.as_bytes(), &mut paths)
            }
            Language::OCaml => {
                extract_ocaml_imports(tree.root_node(), source.as_bytes(), &mut paths)
            }
            Language::R => extract_r_imports(tree.root_node(), source.as_bytes(), &mut paths),
            Language::Julia => {
                extract_julia_imports(tree.root_node(), source.as_bytes(), &mut paths)
            }
            Language::Elm => extract_elm_imports(tree.root_node(), source.as_bytes(), &mut paths),
            Language::Nim => extract_nim_imports(tree.root_node(), source.as_bytes(), &mut paths),
        }

        let edges = paths
            .into_iter()
            .map(|path| Edge {
                from: file_path.to_string(),
                to: path,
                rel: "imports".to_string(),
                provenance: "ast_exact".to_string(),
                confidence: 1.0,
            })
            .collect();
        Ok(edges)
    }
}

// ── Go ──────────────────────────────────────────────────────────────

fn extract_go_imports(node: Node, source: &[u8], paths: &mut Vec<String>) {
    if node.kind() == "import_spec" {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "interpreted_string_literal" {
                if let Ok(text) = child.utf8_text(source) {
                    let cleaned = text.trim_matches('"');
                    paths.push(classify_go(cleaned));
                }
            }
        }
        return;
    }
    let children: Vec<Node> = {
        let mut c = node.walk();
        node.children(&mut c).collect()
    };
    for child in children {
        extract_go_imports(child, source, paths);
    }
}

fn classify_go(path: &str) -> String {
    if path.contains('.') {
        format!("pkg:{path}")
    } else {
        format!("std:{path}")
    }
}

// ── Python ──────────────────────────────────────────────────────────

fn extract_python_imports(node: Node, source: &[u8], paths: &mut Vec<String>) {
    match node.kind() {
        "import_statement" | "import_from_statement" => {
            let text = node.utf8_text(source).unwrap_or("");
            let module = text
                .strip_prefix("import ")
                .or_else(|| text.strip_prefix("from "))
                .unwrap_or(text)
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_matches('"');
            if !module.is_empty() {
                paths.push(format!("pkg:{module}"));
            }
            return;
        }
        _ => {}
    }
    let children: Vec<Node> = {
        let mut c = node.walk();
        node.children(&mut c).collect()
    };
    for child in children {
        extract_python_imports(child, source, paths);
    }
}

// ── JavaScript / TypeScript ─────────────────────────────────────────

fn extract_js_imports(node: Node, source: &[u8], paths: &mut Vec<String>) {
    if node.kind() == "import_statement" {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "string" {
                if let Ok(text) = child.utf8_text(source) {
                    let cleaned = text.trim_matches('"').trim_matches('\'');
                    paths.push(classify_js(cleaned));
                }
            }
        }
        return;
    }
    // require() calls
    if node.kind() == "call_expression" {
        let text = node.utf8_text(source).unwrap_or("");
        if text.starts_with("require(") {
            if let Some(arg) = text
                .strip_prefix("require(")
                .and_then(|s| s.strip_suffix(")"))
            {
                let cleaned = arg.trim().trim_matches('"').trim_matches('\'');
                if !cleaned.is_empty() {
                    paths.push(classify_js(cleaned));
                }
            }
        }
        return;
    }
    let children: Vec<Node> = {
        let mut c = node.walk();
        node.children(&mut c).collect()
    };
    for child in children {
        extract_js_imports(child, source, paths);
    }
}

fn classify_js(path: &str) -> String {
    if path.starts_with('.') || path.starts_with('/') {
        format!("local:{path}")
    } else {
        format!("pkg:{path}")
    }
}

// ── Rust ────────────────────────────────────────────────────────────

/// Per-crate module context used to resolve intra-crate Rust imports
/// (`use commands::init;`, `use crate::commands::init;`) to concrete file
/// paths instead of `pkg:` pseudo-nodes (GAP-043).
///
/// The parser emits file→file edges when the imported path names a module
/// declared in this crate; external crates keep their `pkg:` edges.
struct RustModuleCtx {
    /// Crate-root modules (`mod x;` in src/main.rs / src/lib.rs) — used for
    /// `crate::`-prefixed absolute paths.
    root: HashMap<String, String>,
    /// Root modules plus modules declared in the ancestor `mod.rs` files of
    /// the file being parsed (innermost wins) — used for bare relative paths.
    local: HashMap<String, String>,
}

impl RustModuleCtx {
    /// Build the module context for `file_path`, or an empty context when
    /// the file does not live under a Cargo package (resolution then falls
    /// back to `pkg:` edges, preserving pre-GAP-043 behaviour).
    fn build(file_path: &str) -> Self {
        let mut ctx = RustModuleCtx {
            root: HashMap::new(),
            local: HashMap::new(),
        };
        let Some(pkg_dir) = rust_package_dir(file_path) else {
            return ctx;
        };
        let src_dir = pkg_dir.join("src");
        let root_file = if src_dir.join("main.rs").is_file() {
            src_dir.join("main.rs")
        } else {
            src_dir.join("lib.rs")
        };
        if root_file.is_file() {
            collect_rust_modules(&root_file, &src_dir, &mut ctx.root);
            ctx.local = ctx.root.clone();
        }
        // Ancestor mod.rs chain: modules declared in the file's enclosing
        // module files are visible as bare paths from this file. Innermost
        // declarations shadow outer ones (inserted last).
        let parent = Path::new(file_path).parent().unwrap_or(Path::new(""));
        let mut chain: Vec<PathBuf> = Vec::new();
        let mut dir = Some(parent.to_path_buf());
        while let Some(d) = dir {
            if d == src_dir {
                break;
            }
            chain.push(d.clone());
            dir = d.parent().map(Path::to_path_buf);
        }
        for d in chain.iter().rev() {
            let mod_file = d.join("mod.rs");
            if mod_file.is_file() {
                collect_rust_modules(&mod_file, d, &mut ctx.local);
            }
        }
        ctx
    }
}

/// Walk up from `file` to the nearest directory containing a `Cargo.toml`
/// with a `[package]` section (the crate root). Workspace-root manifests
/// are skipped. Returns `None` when no package manifest is found.
fn rust_package_dir(file: &str) -> Option<PathBuf> {
    let mut dir = Path::new(file).parent()?.to_path_buf();
    loop {
        let manifest = dir.join("Cargo.toml");
        if manifest.is_file() {
            if let Ok(text) = std::fs::read_to_string(&manifest) {
                if text.lines().any(|l| l.trim().starts_with("[package]")) {
                    return Some(dir);
                }
            }
        }
        dir = dir.parent()?.to_path_buf();
    }
}

/// Parse `mod_file` for file-backed module declarations (`mod x;`) and map
/// each name to its resolved file path (`<base>/x.rs` or `<base>/x/mod.rs`).
/// Inline modules (`mod x { ... }`) and declarations with no backing file
/// are skipped.
fn collect_rust_modules(mod_file: &Path, base_dir: &Path, map: &mut HashMap<String, String>) {
    let Ok(source) = std::fs::read_to_string(mod_file) else {
        return;
    };
    let mut ts = tree_sitter::Parser::new();
    if ts.set_language(&tree_sitter_rust::LANGUAGE.into()).is_err() {
        return;
    }
    let Some(tree) = ts.parse(&source, None) else {
        return;
    };
    collect_rust_mod_items(tree.root_node(), &source, base_dir, map);
}

fn collect_rust_mod_items(
    node: Node,
    source: &str,
    base_dir: &Path,
    map: &mut HashMap<String, String>,
) {
    if node.kind() == "mod_item" {
        // Inline `mod x { ... }` has a body; file-backed `mod x;` does not.
        if node.child_by_field_name("body").is_none() {
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Ok(name) = name_node.utf8_text(source.as_bytes()) {
                    let as_file = base_dir.join(format!("{name}.rs"));
                    let as_dir = base_dir.join(name).join("mod.rs");
                    let target = if as_file.is_file() {
                        Some(as_file)
                    } else if as_dir.is_file() {
                        Some(as_dir)
                    } else {
                        None
                    };
                    if let Some(t) = target {
                        map.insert(name.to_string(), t.to_string_lossy().into_owned());
                    }
                }
            }
        }
        return;
    }
    let children: Vec<Node> = {
        let mut c = node.walk();
        node.children(&mut c).collect()
    };
    for child in children {
        collect_rust_mod_items(child, source, base_dir, map);
    }
}

/// Resolve a Rust use path to the concrete file it imports, when the first
/// segment names a local module. `from_crate_root` selects the crate-root
/// map (`crate::`-prefixed absolute paths) vs the file's local map (bare
/// paths). Deeper segments descend into module directories; resolution
/// stops at the nearest existing file. Returns `None` for external paths.
fn resolve_rust_path(ctx: &RustModuleCtx, path: &str, from_crate_root: bool) -> Option<String> {
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    let segments: Vec<&str> = path.split("::").filter(|s| !s.is_empty()).collect();
    let first = *segments.first()?;
    let entry = if from_crate_root {
        ctx.root.get(first)
    } else {
        ctx.local.get(first)
    }?
    .clone();
    let mut current = PathBuf::from(entry);
    for seg in &segments[1..] {
        let is_plain_file = current.file_name().and_then(|n| n.to_str()) != Some("mod.rs");
        if is_plain_file {
            // A plain `x.rs` cannot contain file-backed submodules.
            break;
        }
        let dir = current.parent()?;
        let as_file = dir.join(format!("{seg}.rs"));
        let as_dir = dir.join(seg).join("mod.rs");
        if as_file.is_file() {
            current = as_file;
        } else if as_dir.is_file() {
            current = as_dir;
        } else {
            break;
        }
    }
    Some(current.to_string_lossy().into_owned())
}

fn extract_rust_imports(node: Node, source: &[u8], paths: &mut Vec<String>, ctx: &RustModuleCtx) {
    if node.kind() == "use_declaration" {
        let text = node.utf8_text(source).unwrap_or("");
        let mut trimmed = strip_rust_use_visibility(text);
        if let Some(rest) = trimmed.strip_prefix("use ") {
            trimmed = rest;
        }
        let trimmed = trimmed.trim();
        if trimmed.contains('{') {
            // Brace groups: expand to one edge per symbol (`use foo::{a, b}`
            // → pkg:foo::a, pkg:foo::b) instead of leaking the raw group
            // text (`pkg:{...}` garbage) into the graph. Local module
            // symbols resolve to file paths (GAP-043).
            for symbol in expand_rust_use_group(trimmed) {
                push_rust_symbol_edge(&symbol, paths, ctx);
            }
        } else {
            let path = strip_rust_alias(trimmed).trim_end_matches(';').trim();
            let first = path.split("::").next().unwrap_or(path);
            if first == "crate" {
                // `use crate::...` — absolute within this crate: resolve to
                // the module's file, skip when unresolvable.
                if let Some(target) = resolve_rust_path(ctx, &path["crate".len()..], true) {
                    paths.push(target);
                }
            } else if first == "self" || first == "super" {
                // Local pseudo-prefixes: skipped (unchanged behaviour).
            } else if let Some(target) = resolve_rust_path(ctx, path, false) {
                // Bare path naming a local module: file→file edge.
                paths.push(target);
            } else if !first.is_empty() {
                // External crate: keep the pkg: pseudo-node.
                paths.push(format!("pkg:{first}"));
            }
        }
        return;
    }
    // extern crate declarations
    if node.kind() == "extern_crate_declaration" {
        let text = node.utf8_text(source).unwrap_or("");
        if let Some(name) = text.strip_prefix("extern crate ") {
            let cleaned = name.trim().trim_matches(';');
            if !cleaned.is_empty() {
                paths.push(format!("pkg:{cleaned}"));
            }
        }
        return;
    }
    let children: Vec<Node> = {
        let mut c = node.walk();
        node.children(&mut c).collect()
    };
    for child in children {
        extract_rust_imports(child, source, paths, ctx);
    }
}

/// Strip a leading visibility modifier (`pub`, `pub(crate)`, `pub(super)`,
/// `pub(in path)`) from a Rust `use` declaration body.
fn strip_rust_use_visibility(text: &str) -> &str {
    let t = text.trim();
    if let Some(rest) = t.strip_prefix("pub(crate) ") {
        rest
    } else if let Some(rest) = t.strip_prefix("pub(super) ") {
        rest
    } else if let Some(rest) = t.strip_prefix("pub(in ") {
        // `pub(in path) use ...` — skip past the closing paren.
        match rest.find(')') {
            Some(end) => rest[end + 1..].trim(),
            None => rest,
        }
    } else if let Some(rest) = t.strip_prefix("pub ") {
        rest
    } else {
        t
    }
}

/// Strip a `as Alias` suffix (`use foo::Bar as Baz;` → `foo::Bar`).
fn strip_rust_alias(path: &str) -> &str {
    path.split(" as ").next().unwrap_or(path).trim()
}

/// Emit one edge per expanded use-symbol: file→file for local modules,
/// `pkg:` for external crates; skips local pseudo-prefixes
/// (`crate`/`self`/`super`) and empty/alias-only segments.
fn push_rust_symbol_edge(symbol: &str, paths: &mut Vec<String>, ctx: &RustModuleCtx) {
    let symbol = strip_rust_alias(symbol).trim_end_matches(';').trim();
    if symbol.is_empty() {
        return;
    }
    let first = symbol.split("::").next().unwrap_or(symbol);
    if first == "crate" {
        // `use crate::x::y` — absolute path within this crate.
        if let Some(target) = resolve_rust_path(ctx, &symbol["crate".len()..], true) {
            paths.push(target);
        }
        return;
    }
    if first == "self" || first == "super" {
        return;
    }
    if let Some(target) = resolve_rust_path(ctx, symbol, false) {
        paths.push(target);
    } else {
        paths.push(format!("pkg:{symbol}"));
    }
}

/// Expand a Rust `use` tree body into full paths, expanding `{...}` groups
/// into one path per symbol. Nested groups are expanded recursively.
///
/// `use foo::{a, b}`      → ["foo::a", "foo::b"]
/// `use {a, b}`           → ["a", "b"]
/// `use foo::{a::{x, y}}` → ["foo::a::x", "foo::a::y"]
/// `use foo::{self, Bar}` → ["foo", "foo::Bar"]
fn expand_rust_use_group(tree: &str) -> Vec<String> {
    let tree = tree.trim().trim_end_matches(';').trim();
    if !tree.contains('{') {
        return vec![strip_rust_alias(tree).to_string()];
    }
    let open = tree.find('{').unwrap();
    let prefix = tree[..open].trim().trim_end_matches(':').trim();
    // Find the matching close brace, respecting nesting.
    let mut depth = 0usize;
    let mut close = None;
    for (i, ch) in tree[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close.unwrap_or(tree.len() - 1);
    let body = &tree[open + 1..close];
    let mut out = Vec::new();
    for item in split_rust_use_items(body) {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        if item.contains('{') {
            for sub in expand_rust_use_group(item) {
                out.push(join_rust_use_path(prefix, &sub));
            }
        } else if item == "self" || item == "*" {
            // `self`/glob refer to the parent path itself.
            if !prefix.is_empty() {
                out.push(prefix.to_string());
            }
        } else {
            out.push(join_rust_use_path(prefix, strip_rust_alias(item)));
        }
    }
    out
}

/// Join a group prefix and an item path, handling a bare group (`{a, b}`).
fn join_rust_use_path(prefix: &str, item: &str) -> String {
    if prefix.is_empty() {
        item.to_string()
    } else {
        format!("{prefix}::{item}")
    }
}

/// Split a `{...}` group body on top-level commas (nesting-aware).
fn split_rust_use_items(body: &str) -> Vec<&str> {
    let mut items = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (i, ch) in body.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => depth -= 1,
            ',' if depth == 0 => {
                items.push(&body[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    items.push(&body[start..]);
    items
}

// ── Java ────────────────────────────────────────────────────────────

fn extract_java_imports(node: Node, source: &[u8], paths: &mut Vec<String>) {
    if node.kind() == "import_declaration" {
        let text = node.utf8_text(source).unwrap_or("");
        let path = text
            .strip_prefix("import ")
            .unwrap_or(text)
            .trim_end_matches(';')
            .trim();
        if !path.is_empty() {
            paths.push(format!("pkg:{path}"));
        }
        return;
    }
    let children: Vec<Node> = {
        let mut c = node.walk();
        node.children(&mut c).collect()
    };
    for child in children {
        extract_java_imports(child, source, paths);
    }
}

// ── C / C++ ─────────────────────────────────────────────────────────

fn extract_c_imports(node: Node, source: &[u8], paths: &mut Vec<String>) {
    if node.kind() == "preproc_include" {
        let text = node.utf8_text(source).unwrap_or("");
        let cleaned = text
            .strip_prefix("#include")
            .unwrap_or(text)
            .trim()
            .trim_matches('"')
            .trim_matches('<')
            .trim_matches('>');
        let prefix = if text.contains('"') { "local" } else { "sys" };
        if !cleaned.is_empty() {
            paths.push(format!("{prefix}:{cleaned}"));
        }
        return;
    }
    let children: Vec<Node> = {
        let mut c = node.walk();
        node.children(&mut c).collect()
    };
    for child in children {
        extract_c_imports(child, source, paths);
    }
}

// ── Ruby ────────────────────────────────────────────────────────────

fn extract_ruby_imports(node: Node, source: &[u8], paths: &mut Vec<String>) {
    if node.kind() == "call" {
        let text = node.utf8_text(source).unwrap_or("");
        if text.starts_with("require ") {
            let cleaned = text
                .strip_prefix("require ")
                .unwrap_or("")
                .trim()
                .trim_matches('"')
                .trim_matches('\'');
            if !cleaned.is_empty() {
                paths.push(format!("pkg:{cleaned}"));
            }
        }
    }
    let children: Vec<Node> = {
        let mut c = node.walk();
        node.children(&mut c).collect()
    };
    for child in children {
        extract_ruby_imports(child, source, paths);
    }
}

// ── C# ─────────────────────────────────────────────────────────────

fn extract_csharp_imports(node: Node, source: &[u8], paths: &mut Vec<String>) {
    if node.kind() == "using_directive" {
        let text = node.utf8_text(source).unwrap_or("");
        // `using System;` or `using System.IO;` or `using static System.Math;`
        let trimmed = text
            .strip_prefix("using ")
            .unwrap_or(text)
            .trim_start_matches("static ")
            .trim()
            .trim_end_matches(';')
            .trim();
        if !trimmed.is_empty() {
            paths.push(format!("pkg:{trimmed}"));
        }
        return;
    }
    let children: Vec<Node> = {
        let mut c = node.walk();
        node.children(&mut c).collect()
    };
    for child in children {
        extract_csharp_imports(child, source, paths);
    }
}

// ── Kotlin ─────────────────────────────────────────────────────────

fn extract_kotlin_imports(node: Node, source: &[u8], paths: &mut Vec<String>) {
    if node.kind() == "import" || node.kind() == "import_header" {
        let text = node.utf8_text(source).unwrap_or("");
        let trimmed = text.strip_prefix("import ").unwrap_or(text).trim();
        // Take everything up to the first " as " alias
        let path_part = trimmed.split(" as ").next().unwrap_or(trimmed).trim();
        if !path_part.is_empty() {
            paths.push(format!("pkg:{path_part}"));
        }
        return;
    }
    let children: Vec<Node> = {
        let mut c = node.walk();
        node.children(&mut c).collect()
    };
    for child in children {
        extract_kotlin_imports(child, source, paths);
    }
}

// ── PHP ────────────────────────────────────────────────────────────

fn extract_php_imports(node: Node, source: &[u8], paths: &mut Vec<String>) {
    // PHP uses `use` and `use ... as ...` for class imports.
    // The grammar exposes these as `namespace_use_declaration` or `use_declaration`.
    if node.kind() == "namespace_use_declaration" || node.kind() == "use_declaration" {
        let text = node.utf8_text(source).unwrap_or("");
        // Strip `use ` prefix and trailing `;`
        let trimmed = text
            .strip_prefix("use ")
            .unwrap_or(text)
            .trim()
            .trim_end_matches(';')
            .trim();
        // Handle function/const prefixes: `use function foo\bar;` or `use const FOO;`
        let path_part = if trimmed.starts_with("function ") || trimmed.starts_with("const ") {
            trimmed.split_whitespace().nth(1).unwrap_or(trimmed)
        } else {
            // Handle grouped imports: `use Foo\{Bar, Baz};` — take the outer namespace
            trimmed
                .split('{')
                .next()
                .unwrap_or(trimmed)
                .trim()
                .trim_end_matches('\\')
        };
        if !path_part.is_empty() {
            paths.push(format!("pkg:{path_part}"));
        }
        return;
    }
    let children: Vec<Node> = {
        let mut c = node.walk();
        node.children(&mut c).collect()
    };
    for child in children {
        extract_php_imports(child, source, paths);
    }
}

// ── Swift ──────────────────────────────────────────────────────────

fn extract_swift_imports(node: Node, source: &[u8], paths: &mut Vec<String>) {
    if node.kind() == "import_declaration" {
        let text = node.utf8_text(source).unwrap_or("");
        // `import Foundation` or `@testable import MyModule` or `import func Foundation.print`
        let trimmed = text
            .strip_prefix("@testable ")
            .unwrap_or(text)
            .strip_prefix("import ")
            .unwrap_or(text)
            .trim();
        // Take everything up to the first dot (module name only, not submodule.func)
        let module = trimmed.split('.').next().unwrap_or(trimmed).trim();
        if !module.is_empty() {
            paths.push(format!("pkg:{module}"));
        }
        return;
    }
    let children: Vec<Node> = {
        let mut c = node.walk();
        node.children(&mut c).collect()
    };
    for child in children {
        extract_swift_imports(child, source, paths);
    }
}

// ── Elixir ──────────────────────────────────────────────────────────

fn extract_elixir_imports(node: Node, source: &[u8], paths: &mut Vec<String>) {
    // Elixir uses `alias`, `import`, `require`, and `use` keywords.
    // These appear as `call` nodes where the function name is the keyword.
    if node.kind() == "call" {
        let text = node.utf8_text(source).unwrap_or("");
        for keyword in &["alias ", "import ", "require ", "use "] {
            if let Some(rest) = text.strip_prefix(keyword) {
                // Take module path up to first space/comma (handles `alias Foo.Bar, as: B`)
                let path = rest.split([',', ' ']).next().unwrap_or(rest).trim();
                if !path.is_empty() {
                    paths.push(format!("pkg:{path}"));
                }
            }
        }
    }
    let children: Vec<Node> = {
        let mut c = node.walk();
        node.children(&mut c).collect()
    };
    for child in children {
        extract_elixir_imports(child, source, paths);
    }
}

// ── Haskell ─────────────────────────────────────────────────────────

fn extract_haskell_imports(node: Node, source: &[u8], paths: &mut Vec<String>) {
    // `import` node: `import qualified Data.Map as Map`
    if node.kind() == "import" {
        let text = node.utf8_text(source).unwrap_or("");
        // Strip `import` prefix
        let rest = text.strip_prefix("import").unwrap_or(text).trim();
        // Skip `qualified` keyword
        let rest = rest.strip_prefix("qualified").unwrap_or(rest).trim();
        // Take module name up to first space (handles `as`, hiding, etc.)
        let module = rest.split_whitespace().next().unwrap_or("");
        if !module.is_empty() {
            paths.push(format!("pkg:{module}"));
        }
        return;
    }
    let children: Vec<Node> = {
        let mut c = node.walk();
        node.children(&mut c).collect()
    };
    for child in children {
        extract_haskell_imports(child, source, paths);
    }
}

// ── Erlang ──────────────────────────────────────────────────────────

fn extract_erlang_imports(node: Node, source: &[u8], paths: &mut Vec<String>) {
    // `-include_lib("eunit/include/eunit.hrl").` and `-include("records.hrl").`
    let text = node.utf8_text(source).unwrap_or("");
    if text.contains("-include_lib(") || text.contains("-include(") {
        // Extract the string argument
        if let Some(start) = text.find('"') {
            if let Some(end) = text[start + 1..].find('"') {
                let path = &text[start + 1..start + 1 + end];
                if !path.is_empty() {
                    paths.push(format!("local:{path}"));
                }
            }
        }
    }
    let children: Vec<Node> = {
        let mut c = node.walk();
        node.children(&mut c).collect()
    };
    for child in children {
        extract_erlang_imports(child, source, paths);
    }
}

// ── Scala ───────────────────────────────────────────────────────────

fn extract_scala_imports(node: Node, source: &[u8], paths: &mut Vec<String>) {
    if node.kind() == "import_declaration" {
        let text = node.utf8_text(source).unwrap_or("");
        // `import scala.collection.mutable._` or `import akka.actor.{Props, Actor}`
        let rest = text.strip_prefix("import").unwrap_or(text).trim();
        // Take path up to first `{` (grouped import) or end, then trim trailing dots/whitespace
        let path = rest.split('{').next().unwrap_or(rest).trim();
        let path = path.trim_end_matches('.').trim();
        if !path.is_empty() {
            paths.push(format!("pkg:{path}"));
        }
        return;
    }
    let children: Vec<Node> = {
        let mut c = node.walk();
        node.children(&mut c).collect()
    };
    for child in children {
        extract_scala_imports(child, source, paths);
    }
}

// ── Zig ─────────────────────────────────────────────────────────────

fn extract_zig_imports(node: Node, source: &[u8], paths: &mut Vec<String>) {
    // Zig uses `@import("module")` builtin calls
    let text = node.utf8_text(source).unwrap_or("");
    if text.contains("@import(") {
        if let Some(start) = text.find("@import(\"") {
            let rest = &text[start + 9..];
            if let Some(end) = rest.find('"') {
                let path = &rest[..end];
                if !path.is_empty() {
                    paths.push(format!("local:{path}"));
                }
            }
        }
    }
    let children: Vec<Node> = {
        let mut c = node.walk();
        node.children(&mut c).collect()
    };
    for child in children {
        extract_zig_imports(child, source, paths);
    }
}

// ── Lua ─────────────────────────────────────────────────────────────

fn extract_lua_imports(node: Node, source: &[u8], paths: &mut Vec<String>) {
    // `require("module")` or `require 'module'`
    let text = node.utf8_text(source).unwrap_or("");
    if text.starts_with("require") {
        // Find quoted argument
        for quote in &['"', '\''] {
            let prefix = format!("require({quote}");
            if let Some(rest) = text.strip_prefix(&prefix) {
                if let Some(end) = rest.find(*quote) {
                    let path = rest[..end].trim();
                    if !path.is_empty() {
                        paths.push(format!("pkg:{path}"));
                    }
                    return;
                }
            }
            // Handle `require 'module'` (space instead of paren)
            let prefix = format!("require {quote}");
            if let Some(rest) = text.strip_prefix(&prefix) {
                if let Some(end) = rest.find(*quote) {
                    let path = rest[..end].trim();
                    if !path.is_empty() {
                        paths.push(format!("pkg:{path}"));
                    }
                    return;
                }
            }
        }
    }
    let children: Vec<Node> = {
        let mut c = node.walk();
        node.children(&mut c).collect()
    };
    for child in children {
        extract_lua_imports(child, source, paths);
    }
}

// ── Dart ────────────────────────────────────────────────────────────

fn extract_dart_imports(node: Node, source: &[u8], paths: &mut Vec<String>) {
    // `import 'package:foo/bar.dart';` or `import '../utils.dart';`
    // `export 'dart:async';`
    if node.kind() == "import" || node.kind() == "import_or_export" {
        let text = node.utf8_text(source).unwrap_or("");
        let rest = text
            .strip_prefix("import ")
            .or_else(|| text.strip_prefix("export "))
            .unwrap_or(text);
        // Find quoted path
        for quote in &['"', '\''] {
            if let Some(start) = rest.find(*quote) {
                let after = &rest[start + 1..];
                if let Some(end) = after.find(*quote) {
                    let path = &after[..end];
                    if !path.is_empty() {
                        // package: → pkg:, dart: → std:, relative → local:
                        let classified = if path.starts_with("package:") {
                            let stripped = path.strip_prefix("package:").unwrap_or(path);
                            format!("pkg:{stripped}")
                        } else if path.starts_with("dart:") {
                            let stripped = path.strip_prefix("dart:").unwrap_or(path);
                            format!("std:{stripped}")
                        } else {
                            format!("local:{path}")
                        };
                        paths.push(classified);
                        break;
                    }
                }
            }
        }
        return;
    }
    let children: Vec<Node> = {
        let mut c = node.walk();
        node.children(&mut c).collect()
    };
    for child in children {
        extract_dart_imports(child, source, paths);
    }
}

// ── Clojure ────────────────────────────────────────────────────────

fn extract_clojure_imports(node: Node, source: &[u8], paths: &mut Vec<String>) {
    // Clojure: `require` / `use` in `ns` form, or standalone calls.
    // `(ns my.namespace (:require [clojure.string :as str]))`
    // `(require '[clojure.set :as s])`
    // `(use '[clojure.java.io :only [file]])`
    // `(import '[java.util Date Calendar])`
    //
    // The grammar uses s-expression nodes: list_lit, sym_lit, vec_lit, str_lit.
    // We walk looking for list_lit nodes whose first child sym_lit is
    // require/use/import, then extract namespace/package from args.
    if node.kind() == "list_lit" {
        let text = node.utf8_text(source).unwrap_or("");
        // Standalone: `(require ...)`, `(use ...)`, `(import ...)`
        let trimmed = text.trim_start_matches('(').trim();
        if let Some(rest) = trimmed.strip_prefix("require ") {
            extract_clojure_lib(rest, paths);
            return;
        }
        if let Some(rest) = trimmed.strip_prefix("use ") {
            extract_clojure_lib(rest, paths);
            return;
        }
        if let Some(rest) = trimmed.strip_prefix("import ") {
            extract_clojure_import(rest, paths);
            return;
        }
        // Inside ns: `(:require ...)`, `(:use ...)`, `(:import ...)`
        if let Some(rest) = trimmed.strip_prefix(":require ") {
            extract_clojure_lib(rest, paths);
            return;
        }
        if let Some(rest) = trimmed.strip_prefix(":use ") {
            extract_clojure_lib(rest, paths);
            return;
        }
        if let Some(rest) = trimmed.strip_prefix(":import ") {
            extract_clojure_import(rest, paths);
            return;
        }
        // `(ns my.namespace ...)` — walk children to find nested require/use/import
        if trimmed.starts_with("ns ") || trimmed.starts_with("ns\t") {
            let children: Vec<Node> = {
                let mut c = node.walk();
                node.children(&mut c).collect()
            };
            for child in children {
                extract_clojure_imports(child, source, paths);
            }
            return;
        }
    }
    let children: Vec<Node> = {
        let mut c = node.walk();
        node.children(&mut c).collect()
    };
    for child in children {
        extract_clojure_imports(child, source, paths);
    }
}

/// Extract namespace from a `require`/`use` body.
/// Handles: `[clojure.string :as str]`, `clojure.set`, `[clojure.core :refer [join]]`
/// Multiple specs: `[a.b :as ab] [c.d :as cd]`
fn extract_clojure_lib(rest: &str, paths: &mut Vec<String>) {
    // Each `[ns ...]` vector or bare symbol is one require spec.
    // Split on `[` to isolate each spec, take only the first valid symbol.
    // Handle bare symbols (not in vectors): `[clojure.set :as s]` vs `clojure.set`
    // For `[ns1 :as a] [ns2 :as b]`, split on `[` gives:
    //   "", "ns1 :as a] ", "ns2 :as b]"
    for segment in rest.split('[') {
        let segment = segment.trim();
        // Skip segments that follow :refer/:only keywords — these are
        // function lists, not namespace specs: `[clojure.set :refer [union]]`
        if segment.starts_with(':') {
            continue;
        }
        // If the segment before this one contained :refer or :only at the end,
        // it's a function list. Check by looking at whether the preceding
        // segment (before the last `[`) ended with a keyword.
        // Simpler: just check if this segment's first token looks like a
        // function name (short, no dots) AND it's not the first segment.
        // But the cleanest fix: skip segments that don't look like namespaces.
        // Namespaced Clojure libs use dots: `clojure.string`, `my-app.core`.
        // Bare function names like `union`, `str` are not namespaces.
        for token in segment.split([' ', '\t', '\n', '\r', ']']) {
            let token = token.trim_matches('\'');
            if token.is_empty() || token.starts_with(':') {
                continue;
            }
            if token
                .chars()
                .all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == '_')
            {
                classify_clojure_ns(token, paths);
                break; // Only first valid symbol per spec
            }
        }
    }
}

/// Extract package from an `import` body: `[java.util Date Calendar]`
fn extract_clojure_import(rest: &str, paths: &mut Vec<String>) {
    // Form: `[package.Class Name ...]` or `'package.Class`
    for token in rest.split(['[', ']', '{', '}', ' ', '\t', '\n', '\r']) {
        let token = token.trim_matches('\'');
        if token.is_empty() {
            continue;
        }
        if token.starts_with(':') {
            continue;
        }
        // First symbol is the package/class path
        if token
            .chars()
            .all(|c| c.is_alphanumeric() || c == '.' || c == '_' || c == '$')
        {
            paths.push(format!("pkg:{token}"));
            break;
        }
    }
}

fn classify_clojure_ns(ns: &str, paths: &mut Vec<String>) {
    // Clojure require/use namespaces are typically dotted paths
    // (e.g. `clojure.string`, `my-app.core`). Bare single-word names
    // without dots are likely aliases or referred function names
    // (from `:refer [name]`), not real namespaces.
    if ns.contains('.') && !ns.starts_with('-') {
        paths.push(format!("pkg:{ns}"));
    }
    // Skip bare names — they're not namespace specs
}

// ── OCaml ──────────────────────────────────────────────────────────

fn extract_ocaml_imports(node: Node, source: &[u8], paths: &mut Vec<String>) {
    // OCaml: `open Module`, `include Module`, `#load "module"` (for scripts)
    // The grammar exposes `open_module` / `include_module` node kinds.
    // We match on the node kind and extract the module path from children.
    if node.kind() == "open_module" || node.kind() == "include_module" {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "module_path" {
                if let Ok(text) = child.utf8_text(source) {
                    let module_name = text.trim();
                    if !module_name.is_empty() {
                        paths.push(format!("pkg:{module_name}"));
                    }
                }
            }
        }
        return;
    }
    // `#load "module.cma"` and `#use "file.ml"` directives
    let text = node.utf8_text(source).unwrap_or("");
    if text.starts_with("#load ") || text.starts_with("#use ") {
        if let Some(start) = text.find('"') {
            if let Some(end) = text[start + 1..].find('"') {
                let path = &text[start + 1..start + 1 + end];
                if !path.is_empty() {
                    paths.push(format!("local:{path}"));
                }
            }
        }
        return;
    }
    let children: Vec<Node> = {
        let mut c = node.walk();
        node.children(&mut c).collect()
    };
    for child in children {
        extract_ocaml_imports(child, source, paths);
    }
}

// ── R ──────────────────────────────────────────────────────────────

fn extract_r_imports(node: Node, source: &[u8], paths: &mut Vec<String>) {
    // R uses `library(pkg)`, `require(pkg)`, and `source("file.R")`.
    // These appear as `call` nodes.
    if node.kind() == "call" {
        let text = node.utf8_text(source).unwrap_or("");
        for func in &["library(", "require("] {
            if let Some(rest) = text.strip_prefix(func) {
                // Extract package name (may be quoted or bare)
                let cleaned = rest
                    .trim()
                    .trim_end_matches(')')
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .split([',', ' '])
                    .next()
                    .unwrap_or("")
                    .trim();
                if !cleaned.is_empty() {
                    paths.push(format!("pkg:{cleaned}"));
                }
                return;
            }
        }
        if let Some(rest) = text.strip_prefix("source(") {
            let cleaned = rest.trim().trim_end_matches(')').trim();
            for quote in &['"', '\''] {
                if let Some(s) = cleaned.strip_prefix(*quote) {
                    if let Some(end) = s.find(*quote) {
                        let path = &s[..end];
                        if !path.is_empty() {
                            paths.push(format!("local:{path}"));
                        }
                        return;
                    }
                }
            }
            return;
        }
    }
    let children: Vec<Node> = {
        let mut c = node.walk();
        node.children(&mut c).collect()
    };
    for child in children {
        extract_r_imports(child, source, paths);
    }
}

// ── Julia ──────────────────────────────────────────────────────────

fn extract_julia_imports(node: Node, source: &[u8], paths: &mut Vec<String>) {
    // Julia: `using Module`, `import Module`, `using Module: name`,
    // `import Module: name`, `include("file.jl")`
    // The grammar exposes `using_statement`, `import_statement`, `using`,
    // `import`, `import_path` node kinds.
    let text = node.utf8_text(source).unwrap_or("");
    // `include("file.jl")` — a call expression
    if text.starts_with("include(") || text.starts_with("include ") {
        if let Some(start) = text.find('"') {
            if let Some(end) = text[start + 1..].find('"') {
                let path = &text[start + 1..start + 1 + end];
                if !path.is_empty() {
                    paths.push(format!("local:{path}"));
                }
                return;
            }
        }
    }
    // `using Package` / `import Package` / `using Package: name1, name2`
    if node.kind() == "using_statement"
        || node.kind() == "import_statement"
        || node.kind() == "using"
        || node.kind() == "import"
    {
        let rest = text
            .strip_prefix("using")
            .or_else(|| text.strip_prefix("import"))
            .unwrap_or(text)
            .trim();
        // Take module path up to `:` or whitespace
        let module = rest.split([':', ' ', '\t', '\n']).next().unwrap_or("");
        if !module.is_empty()
            && module != "using"
            && module != "import"
            && module.chars().next().is_some_and(|c| c.is_alphabetic())
        {
            paths.push(format!("pkg:{module}"));
        }
        return;
    }
    let children: Vec<Node> = {
        let mut c = node.walk();
        node.children(&mut c).collect()
    };
    for child in children {
        extract_julia_imports(child, source, paths);
    }
}

// ── Elm ────────────────────────────────────────────────────────────

fn extract_elm_imports(node: Node, source: &[u8], paths: &mut Vec<String>) {
    // Elm: `import Module exposing (..)` / `import Module as M`
    // The grammar exposes `import_clause` node kind.
    if node.kind() == "import_clause" || node.kind() == "import" {
        let text = node.utf8_text(source).unwrap_or("");
        let rest = text.strip_prefix("import ").unwrap_or(text).trim();
        // Take module name up to first space (handles `exposing`, `as`)
        let module = rest.split_whitespace().next().unwrap_or("");
        if !module.is_empty() {
            paths.push(format!("pkg:{module}"));
        }
        return;
    }
    let children: Vec<Node> = {
        let mut c = node.walk();
        node.children(&mut c).collect()
    };
    for child in children {
        extract_elm_imports(child, source, paths);
    }
}

// ── Nim ────────────────────────────────────────────────────────────

fn extract_nim_imports(node: Node, source: &[u8], paths: &mut Vec<String>) {
    // Nim: `import module`, `import module1, module2`, `import module as alias`,
    // `from module import ident1, ident2`, `import module except: ident`
    // Classify std/ prefixes as "std:", everything else as "pkg:".

    if node.kind() == "import_statement" {
        let text = node.utf8_text(source).unwrap_or("");
        let body = text.strip_prefix("import ").unwrap_or(text).trim();
        // Split on commas to handle `import a, b, c`
        for part in body.split(',') {
            let part = part.trim();
            // Skip `except` clause content: "module except: ident"
            let module = part.split("except").next().unwrap_or(part).trim();
            // Handle `as` alias: "module as alias"
            let name = module.split_whitespace().next().unwrap_or(module).trim();
            if !name.is_empty() && name.chars().next().is_some_and(|c| c.is_alphabetic()) {
                if name.starts_with("std/") {
                    let stripped = name.strip_prefix("std/").unwrap_or(name);
                    paths.push(format!("std:{stripped}"));
                } else {
                    paths.push(format!("pkg:{name}"));
                }
            }
        }
        return;
    }

    // `from module import ident1, ident2`
    if node.kind() == "import_from_statement" {
        // The module is in a child with field_name "module"
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "identifier" || child.kind() == "dot_expression" {
                if let Ok(text) = child.utf8_text(source) {
                    let name = text.trim();
                    if !name.is_empty() {
                        if name.starts_with("std/") {
                            let stripped = name.strip_prefix("std/").unwrap_or(name);
                            paths.push(format!("std:{stripped}"));
                        } else {
                            paths.push(format!("pkg:{name}"));
                        }
                    }
                }
            }
        }
        return;
    }

    let children: Vec<Node> = {
        let mut c = node.walk();
        node.children(&mut c).collect()
    };
    for child in children {
        extract_nim_imports(child, source, paths);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(lang: Language, source: &str) -> Vec<String> {
        let mut p = Parser::for_language(lang).unwrap();
        p.parse_imports("test.ext", source)
            .unwrap()
            .into_iter()
            .map(|e| e.to)
            .collect()
    }

    #[test]
    fn go_classification() {
        assert_eq!(classify_go("fmt"), "std:fmt");
        assert_eq!(classify_go("github.com/foo/bar"), "pkg:github.com/foo/bar");
    }

    #[test]
    fn go_imports() {
        let imports = parse(Language::Go, "package p\nimport \"fmt\"\nimport \"os\"");
        assert!(imports.contains(&"std:fmt".into()));
        assert!(imports.contains(&"std:os".into()));
    }

    #[test]
    fn python_imports() {
        let imports = parse(
            Language::Python,
            "import os\nfrom collections import defaultdict",
        );
        assert!(imports.contains(&"pkg:os".into()));
        assert!(imports.contains(&"pkg:collections".into()));
    }

    #[test]
    fn ts_imports() {
        let imports = parse(
            Language::TypeScript,
            "import { foo } from './utils';\nimport express from 'express';",
        );
        assert!(imports.contains(&"local:./utils".into()));
        assert!(imports.contains(&"pkg:express".into()));
    }

    #[test]
    fn rust_imports() {
        let imports = parse(
            Language::Rust,
            "use std::collections::HashMap;\nuse serde::Serialize;",
        );
        assert!(imports.contains(&"pkg:std".into()));
        assert!(imports.contains(&"pkg:serde".into()));
    }

    #[test]
    fn rust_use_brace_groups_expand_per_symbol() {
        let imports = parse(
            Language::Rust,
            r#"use {serde::Serialize, anyhow::Context as Ctx};
use std::{collections::HashMap, io::{Read, Write}};
use foo::{self, Bar};
use crate::{grep_matcher, globset};
"#,
        );
        // Bare group: one edge per symbol.
        assert!(imports.contains(&"pkg:serde::Serialize".into()));
        // Alias stripped, symbol path kept.
        assert!(imports.contains(&"pkg:anyhow::Context".into()));
        // Prefixed + nested groups expand recursively.
        assert!(imports.contains(&"pkg:std::collections::HashMap".into()));
        assert!(imports.contains(&"pkg:std::io::Read".into()));
        assert!(imports.contains(&"pkg:std::io::Write".into()));
        // `self` item resolves to the parent path.
        assert!(imports.contains(&"pkg:foo".into()));
        assert!(imports.contains(&"pkg:foo::Bar".into()));
        // `crate::` groups are local — skipped entirely.
        assert!(!imports.contains(&"pkg:crate".into()));
        // The core regression: no raw group text may leak into edge targets.
        assert!(imports.iter().all(|p| !p.starts_with("pkg:{")));
    }

    #[test]
    fn rust_pub_use_visibility_and_alias_stripped() {
        let imports = parse(
            Language::Rust,
            r#"pub use serde_json as json;
pub(crate) use self::imp::*;
pub(super) use crate::flags::{A, B};
use anyhow::{bail, Context as _};
"#,
        );
        // Visibility + alias stripped, first segment kept for single paths.
        assert!(imports.contains(&"pkg:serde_json".into()));
        // Local pseudo-prefixes never become pkg edges.
        assert!(!imports.contains(&"pkg:self".into()));
        assert!(!imports.contains(&"pkg:crate".into()));
        assert!(!imports.contains(&"pkg:pub".into()));
        // Group symbols under a pub(crate)-prefixed path still expand.
        assert!(imports.contains(&"pkg:anyhow::bail".into()));
        assert!(imports.contains(&"pkg:anyhow::Context".into()));
        // No raw group / visibility garbage in any edge target.
        assert!(imports
            .iter()
            .all(|p| !p.contains('{') && !p.contains("pub")));
    }

    /// Parse `source` as Rust with `file_path` as the file's path, so the
    /// intra-crate module context resolves against real fixtures on disk.
    fn parse_rust_at(file_path: &str, source: &str) -> Vec<String> {
        let mut p = Parser::for_language(Language::Rust).unwrap();
        p.parse_imports(file_path, source)
            .unwrap()
            .into_iter()
            .map(|e| e.to)
            .collect()
    }

    /// Write a fixture crate (Cargo.toml + module files) inside `dir` and
    /// return the absolute path of the main source file.
    fn write_rust_crate(dir: &Path, main: &str) -> String {
        std::fs::create_dir_all(dir.join("src/commands")).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"fixture-cli\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("src/main.rs"), main).unwrap();
        std::fs::write(
            dir.join("src/commands/mod.rs"),
            "pub mod init;\npub mod plugin;\n",
        )
        .unwrap();
        std::fs::write(dir.join("src/commands/init.rs"), "pub fn init() {}\n").unwrap();
        std::fs::write(dir.join("src/commands/plugin.rs"), "pub fn plugin() {}\n").unwrap();
        dir.join("src/main.rs").to_string_lossy().into_owned()
    }

    #[test]
    fn rust_intra_crate_bare_path_resolves_to_file() {
        let dir = tempfile::tempdir().unwrap();
        let main = write_rust_crate(
            dir.path(),
            "mod commands;\nuse commands::init;\nuse clap::Parser;\n",
        );
        let imports = parse_rust_at(&main, &std::fs::read_to_string(&main).unwrap());
        let commands_init = dir.path().join("src/commands/init.rs");
        assert!(
            imports.contains(&commands_init.to_string_lossy().into_owned()),
            "bare `use commands::init` must resolve to commands/init.rs, got: {imports:?}"
        );
        // External crates keep their pkg: pseudo-node.
        assert!(imports.contains(&"pkg:clap".into()));
    }

    #[test]
    fn rust_intra_crate_brace_group_expands_to_files() {
        let dir = tempfile::tempdir().unwrap();
        let main = write_rust_crate(dir.path(), "mod commands;\nuse commands::{init, plugin};\n");
        let imports = parse_rust_at(&main, &std::fs::read_to_string(&main).unwrap());
        let init = dir.path().join("src/commands/init.rs");
        let plugin = dir.path().join("src/commands/plugin.rs");
        assert!(
            imports.contains(&init.to_string_lossy().into_owned()),
            "group symbol init must resolve to a file, got: {imports:?}"
        );
        assert!(
            imports.contains(&plugin.to_string_lossy().into_owned()),
            "group symbol plugin must resolve to a file, got: {imports:?}"
        );
        assert!(
            imports.iter().all(|p| !p.starts_with("pkg:commands")),
            "no pkg:commands pseudo-node may survive resolution, got: {imports:?}"
        );
    }

    #[test]
    fn rust_crate_absolute_path_resolves_to_file() {
        let dir = tempfile::tempdir().unwrap();
        let main = write_rust_crate(dir.path(), "mod commands;\nuse crate::commands::init;\n");
        let imports = parse_rust_at(&main, &std::fs::read_to_string(&main).unwrap());
        let init = dir.path().join("src/commands/init.rs");
        assert!(
            imports.contains(&init.to_string_lossy().into_owned()),
            "`use crate::commands::init` must resolve to a file, got: {imports:?}"
        );
        assert!(!imports.contains(&"pkg:crate".into()));
    }

    #[test]
    fn rust_sibling_module_visible_from_nested_file() {
        let dir = tempfile::tempdir().unwrap();
        let _main = write_rust_crate(dir.path(), "mod commands;\n");
        let plugin = dir.path().join("src/commands/plugin.rs");
        std::fs::write(&plugin, "use init::Init;\n").unwrap();
        let imports = parse_rust_at(
            &plugin.to_string_lossy(),
            &std::fs::read_to_string(&plugin).unwrap(),
        );
        let init = dir.path().join("src/commands/init.rs");
        assert!(
            imports.contains(&init.to_string_lossy().into_owned()),
            "sibling `use init::...` inside commands/plugin.rs must resolve to commands/init.rs, got: {imports:?}"
        );
    }

    #[test]
    fn rust_without_cargo_manifest_keeps_pkg_fallback() {
        // No Cargo.toml on the walk up → empty module context → pkg: edges.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src/main.rs");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, "use commands::init;\nuse serde::Serialize;\n").unwrap();
        let imports = parse_rust_at(
            &src.to_string_lossy(),
            &std::fs::read_to_string(&src).unwrap(),
        );
        assert!(imports.contains(&"pkg:commands".into()), "{imports:?}");
        assert!(imports.contains(&"pkg:serde".into()), "{imports:?}");
    }

    #[test]
    fn java_imports() {
        let imports = parse(
            Language::Java,
            "import java.util.List;\nimport com.foo.Bar;",
        );
        assert!(imports.contains(&"pkg:java.util.List".into()));
        assert!(imports.contains(&"pkg:com.foo.Bar".into()));
    }

    #[test]
    fn c_includes() {
        let imports = parse(Language::C, "#include <stdio.h>\n#include \"local.h\"");
        assert!(imports.contains(&"sys:stdio.h".into()));
        assert!(imports.contains(&"local:local.h".into()));
    }

    #[test]
    fn ruby_requires() {
        let imports = parse(Language::Ruby, "require 'json'\nrequire_relative 'helper'");
        assert!(imports.contains(&"pkg:json".into()));
        // require_relative not yet handled — that's fine for Phase 1
    }

    #[test]
    fn empty_file() {
        let imports = parse(Language::Go, "// just a comment\npackage p");
        assert!(imports.is_empty());
    }

    #[test]
    fn language_from_extension() {
        assert_eq!(Language::from_extension("go"), Some(Language::Go));
        assert_eq!(Language::from_extension("py"), Some(Language::Python));
        assert_eq!(Language::from_extension("ts"), Some(Language::TypeScript));
        assert_eq!(Language::from_extension("rs"), Some(Language::Rust));
        assert_eq!(Language::from_extension("js"), Some(Language::JavaScript));
        assert_eq!(Language::from_extension("java"), Some(Language::Java));
        assert_eq!(Language::from_extension("c"), Some(Language::C));
        assert_eq!(Language::from_extension("cpp"), Some(Language::Cpp));
        assert_eq!(Language::from_extension("rb"), Some(Language::Ruby));
        assert_eq!(Language::from_extension("cs"), Some(Language::CSharp));
        assert_eq!(Language::from_extension("kt"), Some(Language::Kotlin));
        assert_eq!(Language::from_extension("kts"), Some(Language::Kotlin));
        assert_eq!(Language::from_extension("php"), Some(Language::Php));
        assert_eq!(Language::from_extension("phtml"), Some(Language::Php));
        assert_eq!(Language::from_extension("swift"), Some(Language::Swift));
        assert_eq!(Language::from_extension("ex"), Some(Language::Elixir));
        assert_eq!(Language::from_extension("exs"), Some(Language::Elixir));
        assert_eq!(Language::from_extension("hs"), Some(Language::Haskell));
        assert_eq!(Language::from_extension("erl"), Some(Language::Erlang));
        assert_eq!(Language::from_extension("scala"), Some(Language::Scala));
        assert_eq!(Language::from_extension("zig"), Some(Language::Zig));
        assert_eq!(Language::from_extension("lua"), Some(Language::Lua));
        assert_eq!(Language::from_extension("dart"), Some(Language::Dart));
        assert_eq!(Language::from_extension("clj"), Some(Language::Clojure));
        assert_eq!(Language::from_extension("cljs"), Some(Language::Clojure));
        assert_eq!(Language::from_extension("ml"), Some(Language::OCaml));
        assert_eq!(Language::from_extension("mli"), Some(Language::OCaml));
        assert_eq!(Language::from_extension("r"), Some(Language::R));
        assert_eq!(Language::from_extension("jl"), Some(Language::Julia));
        assert_eq!(Language::from_extension("elm"), Some(Language::Elm));
        assert_eq!(Language::from_extension("nim"), Some(Language::Nim));
        assert_eq!(Language::from_extension("txt"), None);
    }

    #[test]
    fn csharp_imports() {
        let imports = parse(
            Language::CSharp,
            "using System;\nusing System.IO;\nusing static System.Math;\n",
        );
        assert!(imports.contains(&"pkg:System".into()));
        assert!(imports.contains(&"pkg:System.IO".into()));
        assert!(imports.contains(&"pkg:System.Math".into()));
    }

    #[test]
    fn kotlin_imports() {
        let imports = parse(
            Language::Kotlin,
            "import kotlin.collections.List\nimport kotlinx.coroutines.*\n",
        );
        assert!(imports.contains(&"pkg:kotlin.collections.List".into()));
        assert!(imports.contains(&"pkg:kotlinx.coroutines.*".into()));
    }

    #[test]
    fn php_imports() {
        let imports = parse(
            Language::Php,
            "<?php\nuse App\\Models\\User;\nuse function array_map;\nuse const PHP_INT_MAX;\n",
        );
        assert!(imports.contains(&"pkg:App\\Models\\User".into()));
    }

    #[test]
    fn swift_imports() {
        let imports = parse(
            Language::Swift,
            "import Foundation\nimport UIKit\n@testable import MyModule\n",
        );
        assert!(imports.contains(&"pkg:Foundation".into()));
        assert!(imports.contains(&"pkg:UIKit".into()));
        assert!(imports.contains(&"pkg:MyModule".into()));
    }

    #[test]
    fn elixir_imports() {
        let imports = parse(
            Language::Elixir,
            "alias MyApp.Repo\nimport Ecto.Query\nuse GenServer\n",
        );
        assert!(imports.contains(&"pkg:MyApp.Repo".into()));
        assert!(imports.contains(&"pkg:Ecto.Query".into()));
        assert!(imports.contains(&"pkg:GenServer".into()));
    }

    #[test]
    fn haskell_imports() {
        let imports = parse(
            Language::Haskell,
            "import qualified Data.Map as Map\nimport Control.Monad\n",
        );
        assert!(imports.contains(&"pkg:Data.Map".into()));
        assert!(imports.contains(&"pkg:Control.Monad".into()));
    }

    #[test]
    fn erlang_includes() {
        let imports = parse(
            Language::Erlang,
            "-include_lib(\"eunit/include/eunit.hrl\").\n-module(myapp).\n",
        );
        assert!(imports.iter().any(|p| p.contains("eunit.hrl")));
    }

    #[test]
    fn scala_imports() {
        let imports = parse(
            Language::Scala,
            "import scala.collection.mutable._\nimport akka.actor.{Props, Actor}\n",
        );
        assert!(imports.contains(&"pkg:scala.collection.mutable._".into()));
        assert!(imports.contains(&"pkg:akka.actor".into()));
    }

    #[test]
    fn zig_imports() {
        let imports = parse(
            Language::Zig,
            "const std = @import(\"std\");\nconst foo = @import(\"foo.zig\");\n",
        );
        assert!(imports.contains(&"local:std".into()));
        assert!(imports.contains(&"local:foo.zig".into()));
    }

    #[test]
    fn lua_imports() {
        let imports = parse(
            Language::Lua,
            "local json = require(\"json\")\nlocal utils = require 'utils'\n",
        );
        assert!(imports.contains(&"pkg:json".into()));
        assert!(imports.contains(&"pkg:utils".into()));
    }

    #[test]
    fn dart_imports() {
        let imports = parse(
            Language::Dart,
            "import 'package:flutter/material.dart';\nimport 'dart:async';\nimport '../utils.dart';\n",
        );
        assert!(imports.contains(&"pkg:flutter/material.dart".into()));
        assert!(imports.contains(&"std:async".into()));
        assert!(imports.contains(&"local:../utils.dart".into()));
    }

    #[test]
    fn clojure_imports() {
        let imports = parse(
            Language::Clojure,
            "(ns my-app.core\n  (:require [clojure.string :as str]\n            [clojure.set :refer [union]])\n  (:import [java.util Date Calendar]))\n(require '[clojure.java.io :as io])\n",
        );
        assert!(imports.contains(&"pkg:clojure.string".into()));
        assert!(imports.contains(&"pkg:clojure.set".into()));
        assert!(imports.contains(&"pkg:java.util".into()));
        assert!(imports.contains(&"pkg:clojure.java.io".into()));
    }

    #[test]
    fn ocaml_imports() {
        let imports = parse(
            Language::OCaml,
            "open Base\nopen Stdio\ninclude Comparable\n#load \"unix.cma\"\n",
        );
        assert!(imports.contains(&"pkg:Base".into()));
        assert!(imports.contains(&"pkg:Stdio".into()));
        assert!(imports.contains(&"pkg:Comparable".into()));
    }

    #[test]
    fn r_imports() {
        let imports = parse(
            Language::R,
            "library(ggplot2)\nrequire(dplyr)\nsource(\"utils.R\")\n",
        );
        assert!(imports.contains(&"pkg:ggplot2".into()));
        assert!(imports.contains(&"pkg:dplyr".into()));
        assert!(imports.contains(&"local:utils.R".into()));
    }

    #[test]
    fn julia_imports() {
        let imports = parse(
            Language::Julia,
            "using DataFrames\nusing Statistics: mean, std\nimport Dates\ninclude(\"helper.jl\")\n",
        );
        assert!(imports.contains(&"pkg:DataFrames".into()));
        assert!(imports.contains(&"pkg:Statistics".into()));
        assert!(imports.contains(&"pkg:Dates".into()));
        assert!(imports.contains(&"local:helper.jl".into()));
    }

    #[test]
    fn elm_imports() {
        let imports = parse(
            Language::Elm,
            "module Main exposing (main)\nimport Html\nimport Browser\nimport Json.Decode as D exposing (Decoder)\n",
        );
        assert!(imports.contains(&"pkg:Html".into()));
        assert!(imports.contains(&"pkg:Browser".into()));
        assert!(imports.contains(&"pkg:Json.Decode".into()));
    }

    #[test]
    fn nim_imports() {
        let imports = parse(
            Language::Nim,
            "import std/os\nimport strutils, sequtils\nimport tables as tbl\nfrom std/os import walkDir\nimport std/math except: PI\n",
        );
        assert!(imports.contains(&"std:os".into()));
        assert!(imports.contains(&"pkg:strutils".into()));
        assert!(imports.contains(&"pkg:sequtils".into()));
        assert!(imports.contains(&"pkg:tables".into()));
        assert!(imports.contains(&"std:os".into()));
        assert!(imports.contains(&"std:math".into()));
    }
}
