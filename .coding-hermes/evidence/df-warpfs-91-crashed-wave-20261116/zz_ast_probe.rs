// TEMPORARY scratch probe (deleted before commit).
use tree_sitter::Parser;

fn parse(lang: tree_sitter::Language, src: &str) -> String {
    let mut p = Parser::new();
    p.set_language(&lang).unwrap();
    let t = p.parse(src, None).unwrap();
    format!("{}\nERR={}\n", t.root_node().to_sexp(), t.root_node().has_error())
}

#[test]
fn dump_erlang_ast() {
    let lang = tree_sitter_erlang::LANGUAGE.into();
    let cases = [
        "-module(m).\n-behaviour(gen_server).\n",
        "-module(m).\nf() -> cowboy_req:reply(200, #{}, <<\"ok\">>, Req).\n",
        "-module(m).\n-type t() :: cowboy_http:opts().\n",
        "-module(m).\n-import(lists, [reverse/1, map/2]).\n",
        "-include(\"cowboy_req.hrl\").\n-include_lib(\"stdlib/include/assert.hrl\").\n",
        "-module(m).\nf() -> gen_server:call(S, {a, b}).\n",
        "-module(m).\nf() -> ?MODULE:g().\nh() -> (get_mod()):x().\n",
        "-module(m).\nf() -> cowboy_http2:opts(\n  x).\n",
    ];
    for c in cases {
        println!("=== SRC ===\n{c}\n=== AST ===\n{}", parse(lang, c));
    }
    panic!("probe done");
}
