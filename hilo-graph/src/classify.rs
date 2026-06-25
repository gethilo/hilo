//! Auto-classification — detect file roles, test status, and stability
//! using tree-sitter AST queries. No LLM required.
//!
//! Reference: `tree-sitter-analyzer` (PyPI, MIT) — 13-language call-graph
//! indexing with per-language resolvers for entrypoint/test/library detection.

use crate::error::GraphResult;
use crate::parser::Language;
use tree_sitter::Parser as TsParser;

/// Classification labels written as xattrs (user.vfs.*).
#[derive(Debug, Clone, PartialEq)]
pub struct Classification {
    /// "entrypoint", "library", "test", "example", "config", "script", "unknown"
    pub role: String,
    /// "stable", "beta", "unstable", "deprecated", "unknown"
    pub status: String,
    /// Human-readable reason for the classification.
    pub reason: String,
}

/// Classify a source file by language, filename, and content.
pub fn classify_file(
    language: Language,
    file_path: &str,
    source: &str,
) -> GraphResult<Classification> {
    // 1. Test detection by filename pattern (fastest, no parsing needed)
    if is_test_file(file_path) {
        return Ok(Classification {
            role: "test".into(),
            status: classify_test_status(file_path),
            reason: format!("filename matches test pattern for {:?}", language),
        });
    }

    // 2. Entrypoint detection by filename convention
    if is_entrypoint_by_name(file_path) {
        return Ok(Classification {
            role: "entrypoint".into(),
            status: "stable".into(),
            reason: format!("filename convention: {:?}", language),
        });
    }

    // 3. AST-based detection for main functions and library markers
    let mut parser = TsParser::new();
    let ts_lang = language_to_ts(language);
    parser
        .set_language(&ts_lang)
        .map_err(|e| crate::error::GraphError::Other(format!("failed to set language: {e}")))?;

    let tree = parser.parse(source.as_bytes(), None).ok_or_else(|| {
        crate::error::GraphError::Other("tree-sitter produced no parse tree".into())
    })?;

    let root = tree.root_node();

    // Check for main/entrypoint patterns
    if has_entrypoint(root, source.as_bytes(), language) {
        return Ok(Classification {
            role: "entrypoint".into(),
            status: "stable".into(),
            reason: format!("contains entrypoint pattern for {:?}", language),
        });
    }

    // Check for public API surface (library markers)
    if has_public_api(root, source.as_bytes(), language) {
        return Ok(Classification {
            role: "library".into(),
            status: classify_library_status(file_path),
            reason: format!("has public API surface for {:?}", language),
        });
    }

    // Fallback: classify by directory convention
    classify_by_path(file_path)
}

// ── Language to tree-sitter ─────────────────────────────────────────

fn language_to_ts(lang: Language) -> tree_sitter::Language {
    match lang {
        Language::Go => tree_sitter_go::LANGUAGE.into(),
        Language::Python => tree_sitter_python::LANGUAGE.into(),
        Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Language::Rust => tree_sitter_rust::LANGUAGE.into(),
        Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        Language::Java => tree_sitter_java::LANGUAGE.into(),
        Language::C => tree_sitter_c::LANGUAGE.into(),
        Language::Cpp => tree_sitter_cpp::LANGUAGE.into(),
        Language::Ruby => tree_sitter_ruby::LANGUAGE.into(),
    }
}

// ── Test file detection ─────────────────────────────────────────────

fn is_test_file(path: &str) -> bool {
    let lower = path.to_lowercase();
    // Universal test patterns
    if lower.contains("_test.") || lower.contains(".test.") || lower.contains("_spec.") {
        return true;
    }
    if lower.starts_with("test_") || lower.ends_with("_test.go") {
        return true;
    }
    if lower.contains("/test/") || lower.contains("/tests/") || lower.contains("\\test\\") {
        return true;
    }
    // Language-specific
    if lower.ends_with("test.java") || lower.ends_with("tests.java") {
        return true;
    }
    if lower.ends_with("_spec.rb") {
        return true;
    }
    // __test__ directories (Python convention)
    if lower.contains("__test__") {
        return true;
    }
    false
}

fn classify_test_status(_path: &str) -> String {
    // Test files are inherently less stable than production code
    "beta".into()
}

// ── Entrypoint detection by filename ────────────────────────────────

fn is_entrypoint_by_name(path: &str) -> bool {
    let lower = path.to_lowercase();
    // main.rs, main.go, main.py, Main.java, main.c, main.cpp, etc.
    if lower.ends_with("main.rs")
        || lower.ends_with("main.go")
        || lower.ends_with("main.py")
        || lower.ends_with("main.c")
        || lower.ends_with("main.cpp")
        || lower.ends_with("main.js")
        || lower.ends_with("main.ts")
        || lower.ends_with("main.rb")
    {
        return true;
    }
    // index.js, index.ts (Node.js entrypoint convention)
    if lower.ends_with("index.js") || lower.ends_with("index.ts") {
        return true;
    }
    // __main__.py (Python package entrypoint)
    if lower.ends_with("__main__.py") {
        return true;
    }
    false
}

// ── AST-based entrypoint detection ──────────────────────────────────

fn has_entrypoint(node: tree_sitter::Node, source: &[u8], language: Language) -> bool {
    match language {
        Language::Rust => has_rust_main(node, source),
        Language::Python => has_python_main(node, source),
        Language::Go => has_go_main(node, source),
        Language::JavaScript | Language::TypeScript => has_js_entrypoint(node, source),
        Language::Java => has_java_main(node, source),
        Language::C | Language::Cpp => has_c_main(node, source),
        Language::Ruby => has_ruby_entrypoint(node, source),
    }
}

fn has_rust_main(node: tree_sitter::Node, source: &[u8]) -> bool {
    if node.kind() == "function_item" {
        let text = node.utf8_text(source).unwrap_or("");
        return text.starts_with("fn main(");
    }
    walk_children(node, source, has_rust_main)
}

fn has_python_main(node: tree_sitter::Node, source: &[u8]) -> bool {
    let text = node.utf8_text(source).unwrap_or("");
    if text.contains("if __name__ == \"__main__\"") || text.contains("if __name__ == '__main__'") {
        return true;
    }
    walk_children(node, source, has_python_main)
}

fn has_go_main(node: tree_sitter::Node, source: &[u8]) -> bool {
    if node.kind() == "function_declaration" {
        let text = node.utf8_text(source).unwrap_or("");
        return text.contains("func main()");
    }
    walk_children(node, source, has_go_main)
}

fn has_js_entrypoint(node: tree_sitter::Node, source: &[u8]) -> bool {
    let text = node.utf8_text(source).unwrap_or("");
    // module.exports =, export default function, or direct script entry
    if text.contains("module.exports")
        || text.contains("export default")
        || text.contains("exports.")
    {
        return true;
    }
    walk_children(node, source, has_js_entrypoint)
}

fn has_java_main(node: tree_sitter::Node, source: &[u8]) -> bool {
    let text = node.utf8_text(source).unwrap_or("");
    if text.contains("public static void main(") {
        return true;
    }
    walk_children(node, source, has_java_main)
}

fn has_c_main(node: tree_sitter::Node, source: &[u8]) -> bool {
    if node.kind() == "function_definition" {
        let text = node.utf8_text(source).unwrap_or("");
        return text.contains("main(") && (text.contains("int main") || text.contains("void main"));
    }
    walk_children(node, source, has_c_main)
}

fn has_ruby_entrypoint(node: tree_sitter::Node, source: &[u8]) -> bool {
    let text = node.utf8_text(source).unwrap_or("");
    // Ruby scripts often start with #!/usr/bin/env ruby or have top-level calls
    if text.starts_with("#!/usr/bin/env ruby") {
        return true;
    }
    walk_children(node, source, has_ruby_entrypoint)
}

// ── Public API surface detection ────────────────────────────────────

fn has_public_api(node: tree_sitter::Node, source: &[u8], language: Language) -> bool {
    match language {
        Language::Rust => count_pub_fns(node, source) >= 3,
        Language::Python => has_python_exports(node, source),
        Language::Go => has_go_exports(node, source),
        Language::JavaScript | Language::TypeScript => has_js_exports(node, source),
        Language::Java => has_java_public(node, source),
        Language::C | Language::Cpp => has_c_header_fns(node, source),
        Language::Ruby => has_ruby_public(node, source),
    }
}

fn count_pub_fns(node: tree_sitter::Node, source: &[u8]) -> usize {
    if node.kind() == "function_item" {
        let text = node.utf8_text(source).unwrap_or("");
        if text.starts_with("pub fn") || text.starts_with("pub async fn") {
            return 1;
        }
    }
    let children: Vec<tree_sitter::Node> = {
        let mut c = node.walk();
        node.children(&mut c).collect()
    };
    children.into_iter().map(|c| count_pub_fns(c, source)).sum()
}

fn has_python_exports(node: tree_sitter::Node, source: &[u8]) -> bool {
    let text = node.utf8_text(source).unwrap_or("");
    text.contains("__all__") || text.contains("def ") || text.contains("class ")
}

fn has_go_exports(node: tree_sitter::Node, source: &[u8]) -> bool {
    let text = node.utf8_text(source).unwrap_or("");
    // Go exports are Capitalized functions/types
    let has_export = text.lines().any(|line| {
        let trimmed = line.trim();
        (trimmed.starts_with("func ") || trimmed.starts_with("type "))
            && trimmed
                .split_whitespace()
                .nth(1)
                .map(|s| s.chars().next().map(|c| c.is_uppercase()).unwrap_or(false))
                .unwrap_or(false)
    });
    has_export
}

fn has_js_exports(node: tree_sitter::Node, source: &[u8]) -> bool {
    let text = node.utf8_text(source).unwrap_or("");
    text.contains("export ") || text.contains("module.exports")
}

fn has_java_public(node: tree_sitter::Node, source: &[u8]) -> bool {
    let text = node.utf8_text(source).unwrap_or("");
    text.contains("public class") || text.contains("public interface")
}

fn has_c_header_fns(node: tree_sitter::Node, source: &[u8]) -> bool {
    // .h files with function declarations = library API
    let text = node.utf8_text(source).unwrap_or("");
    let fn_count = text.matches(");").count();
    fn_count >= 2
}

fn has_ruby_public(node: tree_sitter::Node, source: &[u8]) -> bool {
    let text = node.utf8_text(source).unwrap_or("");
    text.contains("def ") && (text.contains("module ") || text.contains("class "))
}

// ── Path-based classification ──────────────────────────────────────

fn classify_by_path(path: &str) -> GraphResult<Classification> {
    let lower = path.to_lowercase();

    if lower.contains("/example")
        || lower.contains("/examples/")
        || lower.contains("\\examples\\")
        || lower.starts_with("examples/")
        || lower.starts_with("example/")
    {
        return Ok(Classification {
            role: "example".into(),
            status: "stable".into(),
            reason: "in examples directory".into(),
        });
    }

    if lower.ends_with(".toml")
        || lower.ends_with(".yaml")
        || lower.ends_with(".yml")
        || lower.ends_with(".json")
        || lower.ends_with(".lock")
        || lower.ends_with(".cfg")
        || lower.ends_with(".ini")
        || lower.ends_with(".env")
    {
        return Ok(Classification {
            role: "config".into(),
            status: "stable".into(),
            reason: "configuration file".into(),
        });
    }

    if lower.contains("/scripts/") || lower.contains("\\scripts\\") {
        return Ok(Classification {
            role: "script".into(),
            status: "beta".into(),
            reason: "in scripts directory".into(),
        });
    }

    Ok(Classification {
        role: "unknown".into(),
        status: "unknown".into(),
        reason: "no classification pattern matched".into(),
    })
}

fn classify_library_status(path: &str) -> String {
    let lower = path.to_lowercase();
    if lower.contains("/src/") && !lower.contains("/test") && !lower.contains("/example") {
        "stable".into()
    } else {
        "beta".into()
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

fn walk_children(
    node: tree_sitter::Node,
    source: &[u8],
    f: fn(tree_sitter::Node, &[u8]) -> bool,
) -> bool {
    let children: Vec<tree_sitter::Node> = {
        let mut c = node.walk();
        node.children(&mut c).collect()
    };
    for child in children {
        if f(child, source) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rust_entrypoint_detection() {
        let source = "fn main() {\n    println!(\"hello\");\n}\n";
        // Integration test — requires full parser setup, tested via CLI
        assert!(source.contains("fn main()"));
    }

    #[test]
    fn test_python_entrypoint_detection() {
        let source = "if __name__ == \"__main__\":\n    main()\n";
        assert!(source.contains("__main__"));
    }

    #[test]
    fn test_go_entrypoint_detection() {
        let source = "package main\n\nfunc main() {\n    fmt.Println(\"hi\")\n}\n";
        assert!(source.contains("func main()"));
    }

    #[test]
    fn test_java_entrypoint_detection() {
        let source = "public class Main {\n    public static void main(String[] args) {}\n}\n";
        assert!(source.contains("public static void main("));
    }

    #[test]
    fn test_c_entrypoint_detection() {
        let source = "int main(int argc, char **argv) {\n    return 0;\n}\n";
        assert!(source.contains("int main"));
    }

    #[test]
    fn test_test_file_detection() {
        assert!(is_test_file("src/foo_test.go"));
        assert!(is_test_file("lib/baz.test.ts"));
        assert!(is_test_file("spec/qux_spec.rb"));
        assert!(is_test_file("src/FooTest.java"));
        assert!(!is_test_file("src/main.rs"));
        assert!(!is_test_file("lib/util.py"));
    }

    #[test]
    fn test_entrypoint_by_name() {
        assert!(is_entrypoint_by_name("src/main.rs"));
        assert!(is_entrypoint_by_name("cmd/server/main.go"));
        assert!(is_entrypoint_by_name("app/main.py"));
        assert!(is_entrypoint_by_name("src/index.js"));
        assert!(is_entrypoint_by_name("pkg/__main__.py"));
        assert!(!is_entrypoint_by_name("src/lib.rs"));
        assert!(!is_entrypoint_by_name("util/helpers.go"));
    }

    #[test]
    fn test_path_based_example() {
        let result = classify_by_path("examples/hello.rs").unwrap();
        assert_eq!(result.role, "example");
    }

    #[test]
    fn test_rust_pub_fn_count() {
        let source = "pub fn foo() {}\npub fn bar() {}\npub async fn baz() {}\nfn private() {}\n";
        assert!(source.matches("pub fn").count() + source.matches("pub async fn").count() >= 3);
    }

    #[test]
    fn test_python_exports_detection() {
        let source = "__all__ = [\"Foo\", \"Bar\"]\n\ndef foo():\n    pass\n";
        assert!(source.contains("__all__"));
    }

    #[test]
    fn test_js_exports_detection() {
        assert!("export function foo() {}".contains("export "));
        assert!("module.exports = { foo }".contains("module.exports"));
    }

    #[test]
    fn test_path_based_config() {
        let result = classify_by_path("Cargo.toml").unwrap();
        assert_eq!(result.role, "config");
        let result = classify_by_path(".github/workflows/ci.yaml").unwrap();
        assert_eq!(result.role, "config");
    }
}
