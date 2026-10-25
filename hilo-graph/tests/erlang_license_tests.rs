//! DF-WARPFS-88 regression: license-header comment text must never become
//! graph edges.
//!
//! The old Erlang extractor substring-probed every AST span for
//! `-include(` and then grabbed the first quoted run inside the matched
//! span. An include directive is visible in the text of every ANCESTOR span
//! (up to the whole-file root), so on cowboy the first quote in the matched
//! span was `"AS IS"` inside the standard ISC/BSD license comment — minting
//! a `local:AS IS` node with 16 phantom edges (8 of them bogus `tested_by`).
//!
//! AC-2: warming an Erlang project must not produce nodes named after
//! license text ("AS IS", "MIT", "BSD", ...).
//! AC-3: the fixture below carries a real ISC license header plus real
//! include directives; the extractor must see the includes and not the
//! license text.

use hilo_graph::parser::{Language, Parser};

/// cowboy's exact ISC license header (as shipped in `cowboy.erl`,
/// `cowboy_app.erl`, `cowboy_router.erl`, ...) — 14 lines before the module
/// begins. The old bug needed the header to appear BEFORE the first
/// `-include` directive, which is how cowboy lays every file out.
const ISC_LICENSE_HEADER: &str = r#"%% Copyright (c) 2011-2026, Loïc Hoguin <essen@ninenines.eu>
%%
%% Permission to use, copy, modify, and/or distribute this software for any
%% purpose with or without fee is hereby granted, provided that the above
%% copyright notice and this permission notice appear in all copies.
%%
%% THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
%% WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
%% MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
%% ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
%% WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
%% ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
%% OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
"#;

/// The license phrases AC-2 forbids as node names, quoted exactly as they
/// appear inside comment text.
const FORBIDDEN_LICENSE_PHRASES: &[&str] = &[
    "AS IS",
    "AS-IS",
    "MIT License",
    "Permission is hereby granted",
    "DISCLAIMS ALL WARRANTIES",
    "BSD",
    "Apache License",
    "GNU General Public",
];

fn erlang_edges(file: &str, source: &str) -> Vec<hilo_metadata::inventory::Edge> {
    Parser::for_language(Language::Erlang)
        .expect("Erlang parser should build")
        .parse_imports(file, source)
        .expect("parse should succeed")
}

fn assert_no_license_text(edges: &[hilo_metadata::inventory::Edge]) {
    for edge in edges {
        for phrase in FORBIDDEN_LICENSE_PHRASES {
            assert!(
                !edge.to.contains(phrase),
                "license-comment text leaked into an edge target: {:?} (phrase {:?})",
                edge.to,
                phrase
            );
        }
    }
}

/// AC-3 core red→green regression: cowboy-shaped file (license header first,
/// then the include directives), like every real cowboy source file.
#[test]
fn erlang_license_header_does_not_create_nodes() {
    let source = format!(
        "{ISC_LICENSE_HEADER}\n-module(cowboy).\n\
         -include(\"cowboy.hrl\").\n\
         -include_lib(\"stdlib/include/assert.hrl\").\n\
         -export([start/0]).\n"
    );
    let edges = erlang_edges("src/cowboy.erl", &source);
    assert_no_license_text(&edges);
    // The real include directives must still be extracted — the fix may not
    // overcorrect into stripping them.
    let tos: Vec<&str> = edges.iter().map(|e| e.to.as_str()).collect();
    assert!(
        tos.contains(&"local:cowboy.hrl"),
        "real -include path missing from edges: {tos:?}"
    );
    assert!(
        tos.contains(&"local:stdlib/include/assert.hrl"),
        "real -include_lib path missing from edges: {tos:?}"
    );
    // Exactly the two include edges; no phantom extras.
    assert_eq!(edges.len(), 2, "unexpected extra edges: {edges:?}");
}

/// DF-WARPFS-88's measured signature: 8 include-carrying files → 16 edges,
/// 8 of them bogus `tested_by` copies. Reproduce the fan-out shape: a test
/// file with the header plus one include must yield exactly 2 edges
/// (imports + tested_by), never 4 (`"AS IS"` duplicating both relations).
#[test]
fn erlang_license_header_no_bogus_tested_by_fanout() {
    let source = format!(
        "{ISC_LICENSE_HEADER}\n-module(cowboy_test).\n\
         -include(\"cowboy.hrl\").\n\
         start_test(_) -> ok.\n"
    );
    let edges = erlang_edges("test/cowboy_SUITE.erl", &source);
    // Non-test-named suffix in classify terms would be 1 edge; the
    // *file* is what classify sees, so compute the expected total from the
    // distinct targets instead of assuming a classification.
    let mut imports = 0;
    let mut tested_by = 0;
    for e in &edges {
        match e.rel.as_str() {
            "imports" => imports += 1,
            "tested_by" => tested_by += 1,
            other => panic!("unexpected relation {other:?} in {edges:?}"),
        }
    }
    assert_eq!(
        imports, tested_by,
        "tested_by must mirror imports 1:1: {edges:?}"
    );
    assert_eq!(imports, 1, "expected exactly one include target: {edges:?}");
    assert_no_license_text(&edges);
}

/// The trap needs no license header at all: ANY quoted run before the
/// directive (a doc comment example, a string attribute) used to win the
/// race against the real path. A stringy first line with no include before
/// it must not manufacture an edge either.
#[test]
fn erlang_quoted_text_before_include_does_not_shadow_it() {
    let source = "%% Example: curl \"http://example.com\" for docs.\n\
                  -module(docs).\n\
                  -include(\"real.hrl\").\n";
    let edges = erlang_edges("src/docs.erl", source);
    let tos: Vec<&str> = edges.iter().map(|e| e.to.as_str()).collect();
    assert_eq!(
        tos,
        vec!["local:real.hrl"],
        "phantom or missing edges: {tos:?}"
    );
}

/// A license header ALONE (no includes) must produce zero edges — this is
/// the `grep -c License → 0` acceptance probe at the extractor level.
#[test]
fn erlang_license_only_file_produces_zero_edges() {
    let source = format!("{ISC_LICENSE_HEADER}\n-module(header_only).\n");
    let edges = erlang_edges("src/header_only.erl", &source);
    assert!(
        edges.is_empty(),
        "edges from a comment-only header: {edges:?}"
    );
}

/// MIT-style header variant — AC-2 names more than BSD/ISC.
#[test]
fn erlang_mit_header_produces_no_nodes() {
    let source = "%% The MIT License (MIT)\n\
                  %% Permission is hereby granted, free of charge, to any person\n\
                  -module(mit_mod).\n\
                  -include_lib(\"kernel/include/logger.hrl\").\n";
    let edges = erlang_edges("src/mit_mod.erl", source);
    assert_no_license_text(&edges);
    assert_eq!(edges.len(), 1, "unexpected edges: {edges:?}");
    assert_eq!(edges[0].to, "local:kernel/include/logger.hrl");
}

/// Macro-valued includes (`-include(?INC("x.hrl"))`-shaped or
/// `-define(INC, "x").` indirection) must not mint edges to the macro
/// source — the grammar models these as non-`string` field children.
#[test]
fn erlang_macro_include_produces_no_fake_path() {
    // `-include(?FILE_MACRO).` — the field child is a macro expansion, not
    // a `string` node; the old extractor would have grabbed whatever was
    // quoted in the span.
    let source = "-module(mac).\n-include(?CONFIG_H).\n";
    let edges = erlang_edges("src/mac.erl", source);
    assert!(
        edges.iter().all(|e| e.to != "local:?CONFIG_H"),
        "macro name leaked as a path: {edges:?}"
    );
}

/// The comments themselves are skipped structurally (AC-1: strip/skip
/// comment blocks before parsing) — this checks the extractor never even
/// descends into a `comment` node's text for include-looking content.
#[test]
fn erlang_comment_with_include_lookalike_is_inert() {
    let source = "%% -include(\"fake_from_comment.hrl\").\n\
                  -module(commenty).\n\
                  -include(\"truth.hrl\").\n";
    let edges = erlang_edges("src/commenty.erl", source);
    let tos: Vec<&str> = edges.iter().map(|e| e.to.as_str()).collect();
    assert!(
        !tos.contains(&"local:fake_from_comment.hrl"),
        "commented-out include extracted as an edge: {tos:?}"
    );
    assert_eq!(tos, vec!["local:truth.hrl"], "unexpected edges: {tos:?}");
}
