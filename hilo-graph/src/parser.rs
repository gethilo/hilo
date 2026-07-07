//! AST parsing with tree-sitter for 13 languages.
//!
//! Supports Go, Python, TypeScript, Rust, JavaScript, Java, C, C++, Ruby,
//! C#, Kotlin, PHP, and Swift. Each language gets a dedicated extraction
//! function that understands its specific import/dependency syntax.

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
            _ => None,
        }
    }

    /// All extensions this parser handles.
    pub fn all_extensions() -> &'static [&'static str] {
        &[
            "go", "py", "ts", "tsx", "rs", "js", "jsx", "java", "c", "cpp", "cc", "cxx", "rb",
            "cs", "kt", "kts", "php", "phtml", "swift",
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
            Language::Rust => extract_rust_imports(tree.root_node(), source.as_bytes(), &mut paths),
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

fn extract_rust_imports(node: Node, source: &[u8], paths: &mut Vec<String>) {
    if node.kind() == "use_declaration" {
        let text = node.utf8_text(source).unwrap_or("");
        let trimmed = text.strip_prefix("use ").unwrap_or(text).trim();
        let crate_name = trimmed.split("::").next().unwrap_or(trimmed);
        if crate_name != "crate" && crate_name != "self" && crate_name != "super" {
            paths.push(format!("pkg:{crate_name}"));
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
        extract_rust_imports(child, source, paths);
    }
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
}
