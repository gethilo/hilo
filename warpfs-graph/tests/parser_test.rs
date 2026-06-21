//! Integration tests for the tree-sitter Go import parser.

use warpfs_graph::parser::{Language, Parser};

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
