# GAP-112 Worker Summary — conformance edges + wiring silent-fallback detector

Branch: wt/GAP-112 · Worktree: /home/kara/worktrees/warpfs-GAP-112

## Files changed

New:
- hilo-graph/src/conformance.rs — per-language conformance extractors (Go method-set matching, Rust impl/dyn, Python ABC/Protocol/isinstance, TS/JS implements/instanceof), `implements`/`consumes` site types, support matrix
- hilo-graph/src/wiring.rs — silent-fallback detector (`detect_wiring`), three-way `WiringState` (pass/finding/unsupported), `satisfier_role` classification (is_test_file + test-double names)
- hilo-cli/src/commands/wiring.rs — `hilo graph wiring [--json] [path]` command
- hilo-cli/src/commands/wiring_fixture.rs — TRBL-084 regression fixture tests (3 legs)

Modified:
- hilo-graph/src/parser.rs — added `Parser::parse_tree` (raw AST access through the shared grammar table)
- hilo-graph/src/lib.rs — module + exports
- hilo-graph/Cargo.toml — `wiring-filter-neutered` feature (AC3 neutering harness)
- hilo-cli/src/cli.rs — `GraphCommand::Wiring` + args + dispatch
- hilo-cli/src/commands/mod.rs — module wiring
- hilo-cli/src/commands/graph.rs — warm emits `implements`/`consumes`/`conformance_of` edges (`discover_conformance_edges`, included in derived-edge fast-path check); stats conformance line
- hilo-cli/Cargo.toml — feature forwarding to hilo_graph
- README.md — wiring in the query quickstart
- docs/cli-reference.md — full `graph wiring` section incl. locked `--json` schema `hilo.graph.wiring/1`

## Fixture (TRBL-084 shape, Go)

/tmp/trbl-fixture: `BatchWriter` interface; `Flush` production call site with runtime type-assert `s.(BatchWriter)`; `FakeSink` test-only implementor (batch/sink_test.go). Leg 2 adds `FileSink` (batch/filesink.go).

## Three fixture runs

1. FINDING (test-only implementor alone):
```
$ hilo graph wiring /tmp/trbl-fixture
conformance: unsupported (Java)
FINDING   BatchWriter — consumed from non-test code but satisfied only by:
          consuming site: batch/flush.go
          satisfier: FakeSink (batch/sink_test.go, role: test)
EXIT=1
```
2. CLEARED (production implementor added):
```
pass      BatchWriter — non-test satisfier exists
EXIT=0
```
   JSON (leg 2): `state: "pass"`, satisfiers list FakeSink(test)+FileSink(production), schema hilo.graph.wiring/1.
3. NEUTERED FAILS (AC3): `cargo test -p hilo-cli --lib --features wiring-filter-neutered trbl084_neutered` → FAILED with `neutered run must fail this assertion — filter is vacuous: pass BatchWriter satisfiers=[("FakeSink", "production")]`. Normal mode: same test suite green (2 passed).

Rust leg (unit-level): `wiring::tests::rust_dyn_wiring_detection` — `&mut dyn BatchWriter` consumer in src/lib.rs, only satisfier in tests/sink.rs → Finding.

## Stats output

Before (fixture warmed on GAP-112 baseline): `Edge types:` lists only imports/tested_by-family lines; no conformance line.
After:
```
Conformance: 2 implements, 1 consumes (heuristic extraction — see `hilo graph wiring`)
  conformance_of: 2
```
edges.jsonl (fixture):
```
{"from":"type:FileSink","to":"iface:BatchWriter","rel":"implements","provenance":"ast_heuristic","confidence":0.8}
{"from":"type:FakeSink","to":"iface:BatchWriter","rel":"implements","provenance":"ast_heuristic","confidence":0.8}
{"from":"batch/flush.go","to":"iface:BatchWriter","rel":"consumes","provenance":"ast_heuristic","confidence":0.8}
```

## AC evidence

- AC1 (implements edges for Go/Rust/Python/TS/JS; explicit unsupported elsewhere): unit tests `conformance::tests::*` (method-set match across files, embedded-interface expansion, Rust impl+dyn, Python ABC/Protocol + cross-file, TS implements/instanceof, JS instanceof vs TS interface). Support matrix test pins all 22 other languages to unsupported. Live: `graph wiring` printed `conformance: unsupported (Java)` for a .java file in the fixture.
- AC2 (fixture flagged; clears with production satisfier): fixture legs 1 & 2 above (`trbl084_test_only_implementor_reports_finding`, `trbl084_production_implementor_clears_finding`), plus live CLI runs.
- AC3 (non-vacuous): `--features wiring-filter-neutered` run → `trbl084_neutered_filter_must_fail` FAILS (filter neutered ⇒ finding vanishes ⇒ assertion fails). Without the feature the suite is green.
- AC4 (`--json` documented, stable field set): docs/cli-reference.md `### wiring` — schema `hilo.graph.wiring/1`: schema, root, scanned_files, languages_unsupported, results[ {interface, state, consumers[], satisfiers[{type,file,role}]} ], finding_count. Verified by live run.
- AC5 (unsupported never pass): `wiring::tests::unsupported_language_is_not_pass` + live Java line; WiringState::Unsupported is a distinct arm never rendered as "pass".
- AC6 (stats conformance line): live `graph stats` output above.

## Gates

- cargo fmt --all — applied
- cargo clippy --workspace --all-targets -- -D warnings — PASS
- cargo test -p hilo_graph --lib — 423 passed
- cargo test -p hilo-cli --lib — 139 passed
- Full workspace build: bunker-las-02 fallback (bunker-las-03 offline 3d): `cargo build --workspace` PASS 37.5s; `cargo clippy --workspace --all-targets -- -D warnings` PASS; `cargo test -p hilo_graph --lib` 423 passed (112s)
