# CLI Reference

## `hilo init`

Initialize Hilo in the current directory. Creates `.vfs/` with inventory
files and a default manifest.

```bash
hilo init
```

Git hooks (`.git/hooks/post-commit` + `post-merge`) are only installed
when the directory is a git repository. Without `.git/`, the hooks are
skipped and a warning is printed:

```
Initialized Hilo in <dir>
warning: .git/ not found — skipping git hook installation
  Run 'git init' first, then 'hilo init' to enable hooks.
```

Hooks are enabled by running `git init` first, then `hilo init` again.

Refuses to run when the current directory is your HOME directory — an
accidental `hilo init` there scatters `.vfs/` state across your home.
Pass `--allow-home` to override explicitly:

```bash
hilo init --allow-home
```

## `hilo meta`

Read and write extended attributes on files.

```bash
# Read all Hilo xattrs
hilo meta hilo-cli/src/main.rs

# Set a specific attribute
hilo meta --set user.vfs.role --value entrypoint hilo-cli/src/main.rs

# Read (prints all xattrs — no per-key read flag)
hilo meta hilo-cli/src/main.rs
```

## `hilo graph`

### `warm`

Walk the directory tree, parse all source files with tree-sitter, and
build the dependency graph. Writes to `.vfs/graph/edges.jsonl` and
`.vfs/graph/graph.db`.

```bash
hilo graph warm

# With cross-repo workspace edges
hilo graph warm --workspace

# Only parse files of a specific language
hilo graph warm --language rust

# Only parse files changed since the last warm (used by the post-commit hook)
hilo graph warm --changed

# Allow HOME (or its canonical equivalent) as the project root — without
# this flag `graph warm` refuses to walk the home directory
hilo graph warm --allow-home
```

Supported languages (26): Go, Python, TypeScript, Rust, JavaScript,
Java, C, C++, Ruby, C#, Kotlin, PHP, Swift, Elixir, Haskell, Erlang,
Scala, Zig, Lua, Dart, Clojure, OCaml, R, Julia, Elm, Nim.

### Excluded directories

Dependency and cache trees are pruned before descending, so their contents
never reach the parser: `go/pkg/mod/`, `vendor/`, `node_modules/`, `venv/`,
`__pycache__/`, `.venv/`, `site-packages/`, `target/`, `.cache/`, `.rustup/`,
`.npm/`, and every hidden (dot) entry.

To re-include one specific excluded path (e.g. a vendored crate that is part
of your project), list it under `graph.include_paths` in
`.vfs/manifest.yaml`. Only the listed path — and its subtree — becomes
visible; every other excluded tree stays pruned:

```yaml
graph:
  include_paths:
    - vendor/critical
```

### HOME guard

`hilo graph warm` (like `hilo init`) refuses to treat your HOME directory as
the project root. Running it from `$HOME` would otherwise parse — and later
JIT-parse on every query — every source file in your home directory,
including dependency and cache trees. The error names `--allow-home`; pass
it only when you really mean it:

```bash
hilo graph warm --allow-home
```

### Project precondition

`hilo graph warm` requires a Hilo project root: a directory with
`.vfs/manifest.yaml` (what `hilo init` writes) or a root-level
`manifest.yaml`. Outside a project it exits non-zero naming `hilo init`
instead of leaving a partial `.vfs/graph/` (parse cache, `edges.jsonl`,
DuckDB cache) behind. `--allow-home` overrides the HOME refusal only — it
does not stand in for `hilo init`.

`hilo serve --mcp` has the same precondition: outside a project it refuses
to start rather than serving tools that can only answer from an empty graph.
An initialized project with no edges yet is a valid server root.

### `stats`

Aggregate statistics about the dependency graph.

On an empty graph — before `hilo graph warm`, or after
`hilo graph clean` — `hilo graph stats` prints exactly one line:

```
Graph cache is empty. Query a file or run `hilo graph warm` to populate.
```

```bash
hilo graph stats

# Output:
# Total edges: 202 distinct / 292 raw (edges.jsonl)
# Total files: 81
# File census: 81 files with edges (distinct edge sources) — warm last counted 90 discovered files = 82 contribute + 8 zero-edge (7 no imports + 1 package facades); some sources may be stale entries no longer on disk (edges.jsonl is append-only)
# Components (6 total):
#   src: 61 files, 180 edges
#   docs: 12 files, 8 edges
#   render: 4 files, 3 edges
# Conformance: 2 implements, 1 consumes (heuristic extraction — see `hilo graph wiring`)
#   codec: 2 files, 2 edges
#   .: 2 files, 1 edges
# Most connected: pkg:std
# Edge types:
#   imports: 200
#   tested_by: 1
#   tests: 1
```

The `Components` section enumerates the repository's top-level subsystems:
one entry per top-level directory, counted by distinct files and by the
edges whose source file lives inside it. Root-level files (no directory)
aggregate under `.`. Graph pseudo-nodes (`pkg:`, `sys:`, `std:`,
`external:` — resolved dependencies rather than files) never appear in the
roster. The header always shows the full component count; the list is
capped like the orphans list by `--limit` (default 25), with a
`... N more components (use --limit 0 to show all)` trailer — pass
`--limit 0` to print every component.

The `File census` line reconciles `Total files` with the coverage
accounting `hilo graph warm` prints at the end of every run (persisted in
`.vfs/graph/coverage.json`). `Total files` is a census of distinct edge
sources in the graph; warm's Coverage line counts every discovered file —
zero-edge files (no imports, package facades) never carry an edge source,
and `edges.jsonl` is append-only, so sources of files deleted after an
earlier warm can linger in the census. When the two definitions disagree,
the line states both numbers and names the difference instead of leaving
two headline numbers that look contradictory. A missing or unreadable
ledger degrades gracefully: the census line is simply omitted.

### `wiring`

The wiring report: the aggregate per-module view an external scorer reads
(GAP-113) plus the silent-fallback interface detector (GAP-112).

**GAP-113 — the report.** `hilo graph wiring` groups every module (the first
path component of a repo-relative path: `hilo-graph`, `hilo-cli`, …) and
emits, per module: its public surfaces (read from `.vfs/graph/surfaces.jsonl`),
inbound/outbound connection counts (from `.vfs/graph/edges.jsonl`), the
surfaces and edges added or removed against a stored baseline, the surfaces
with no coverage link (from `.vfs/graph/coverage_links.jsonl`), and a
classification:

- `entrypoint` — the module declares an executable entry (`main`).
- `service` — a non-test, non-entrypoint module another module imports.
- `lib` — a non-test, non-entrypoint leaf that still exports/imports.
- `test` — every source file in the module is a test file.
- `dead` — no edges in, none out, no public surfaces.

Baselines record the current per-module snapshot (surfaces + edge identities)
so a later run reports exactly what moved. Non-code top-level directories
(docs, hidden trees, dependency caches) are named in the report's `excluded`
list with the reason they are not modules — nothing is dropped silently. An
absent input artifact is a `notes` entry, never a silent zero.

```bash
hilo graph wiring                                   # scan the current directory
hilo graph wiring --json path/to/project
hilo graph wiring --write-baseline .wiring-base.json   # record the current state
hilo graph wiring --baseline .wiring-base.json --json  # report the delta
```

Baseline comparison keys on the content-derived `surface_id` (COV-1) and on
the edge identity `from|rel|to`, so a delta moves only when a surface's
identity or the edge set moves. A `pkg:<crate>` edge endpoint resolves to the
module that owns that crate when the name matches one (`pkg:hilo_metadata` →
`hilo-metadata`), and stays an external dependency otherwise.

**GAP-112 — the findings.** Detect silent-fallback wiring — the TRBL-084
shape. An interface consumed from at least one NON-test site (Go `x.(I)` type
assertions, Rust
`dyn Trait`, Python `isinstance`, TS/JS `instanceof`) whose ONLY satisfiers
classify test-role is reported as a FINDING: tests stay green while every
production run silently falls to a slow/absent path.

Three-way verdict per consumed interface — never collapsed:

- `pass` — a non-test satisfier exists.
- `FINDING` — consumed from non-test code, satisfiers all test-role.
- `unsupported` — the language has no conformance extraction. NEVER rendered
  as `pass`.

Conformance extraction (heuristic, not a type checker) exists for Go
(method-set matching against named interfaces, directory = package), Rust
(`impl Trait for Type`, `dyn Trait`), Python (ABC/Protocol subclassing,
`isinstance`/`issubclass`), and TypeScript/JavaScript (`implements` clauses,
`instanceof` against corpus-declared interfaces). Every other language seen
in the scan prints `conformance: unsupported (<lang>)`.

```bash
hilo graph wiring            # scan the current directory
hilo graph wiring --json path/to/project
```

Exit code is non-zero when findings exist, so CI can gate on it.

`--json` locks this field set (schema `hilo.graph.wiring/2`, exposed on both
the `version` and the retained `schema` field):

```json
{
  "version": "hilo.graph.wiring/2",
  "schema": "hilo.graph.wiring/2",
  "root": "/abs/path/scanned",
  "scope": "self" | "foreign",
  "scanned_files": 215,
  "languages_unsupported": ["java"],
  "modules": [
    {
      "module": "hilo-graph",
      "classification": "service" | "entrypoint" | "lib" | "test" | "dead",
      "surfaces": [
        { "surface_id": "…", "name": "GraphDB", "owner_file": "hilo-graph/src/lib.rs" }
      ],
      "surface_count": 66,
      "inbound_edges": 3,
      "outbound_edges": 113,
      "untested_surfaces": [
        { "surface_id": "…", "name": "…", "owner_file": "…" }
      ],
      "untested_count": 0,
      "delta": null
    }
  ],
  "excluded": [
    { "path": "docs", "reason": "no supported source files" }
  ],
  "baseline": { "path": "/abs/baseline.json", "compared": true, "written": null },
  "notes": [],
  "interfaces": [
    {
      "interface": "BatchWriter",
      "state": "pass" | "finding" | "unsupported",
      "consumers": ["batch/flush.go"],
      "satisfiers": [
        { "type": "FileSink", "file": "batch/filesink.go", "role": "production" },
        { "type": "FakeSink", "file": "batch/sink_test.go", "role": "test" }
      ]
    }
  ],
  "finding_count": 0,
  "totals": {
    "modules": 16,
    "surfaces": 208,
    "untested_surfaces": 28,
    "inbound_edges": 5,
    "outbound_edges": 422,
    "findings": 0
  }
}
```

With a compared baseline, each module's `delta` becomes an object with
`surfaces_added`, `surfaces_removed` (each `{surface_id, name, owner_file}`),
`edges_added` and `edges_removed` (edge identities).

Warm emits the underlying edge families (provenance `ast_heuristic`,
confidence 0.8): `type:<T> -[implements]-> iface:<I>`,
`<file> -[consumes]-> iface:<I>`, and `<file> -[conformance_of]-> type:<T>`.
`graph stats` shows them on a `Conformance:` line.

### `related`

Find files related to a given path through the dependency graph.

```bash
# Forward: what does this file import?
hilo graph related hilo-cli/src/main.rs

# Filter by relation type
hilo graph related hilo-cli/src/main.rs --relation imports

# Reverse: what imports this file?
hilo graph related hilo-graph/src/lib.rs --direction reverse

# Reverse with relation filter
hilo graph related hilo-graph/tests/fixtures/handler.go --direction reverse --relation tested_by
```

### `impact`

Find the review set for a changed file: the file itself, everything that
depends on it (directly or transitively, up to `--max-depth`), and the bounded
both-direction neighbourhood that carries its helpers, tests, fixtures and
build targets.

```bash
# Depth 1 = only edges that target this file itself (usually zero rows for a
# Rust/Java source — see Depth semantics below)
hilo graph impact hilo-graph/src/lib.rs --max-depth 1

# Full transitive closure (default: 10)
hilo graph impact hilo-graph/src/lib.rs --max-depth 10

# JSON output
hilo graph impact hilo-graph/src/lib.rs --format json

# Include external cross-repo edges in the traversal
hilo graph impact hilo-graph/src/lib.rs --external
```

#### Depth semantics

`--max-depth` counts HOPS between graph nodes, and dependency chains pass
through the `pkg:<crate>` pseudo-node: the parser emits edges that target
`pkg:<crate>` (and, for named imports, `pkg:<crate>::<item>`) instead of
file→file edges, so a file-form query reaches that file's real importers as
`file → pkg:<crate> → importer` — two hops, not one.

- `--max-depth 1` reports only edges whose target is the queried file itself
  (or a `local:` node resolving to it). For a Rust or Java source — whose
  imports resolve to `pkg:` nodes — that is usually **zero dependents**, even
  when the file has many importers. Read it as "no edge targets this file
  directly", never as "nothing depends on this file".
- **A file-form query typically needs `--max-depth 2` or more.** The default
  is 10, which is why the default answer looks right: the crate hop is inside
  the budget. Rows reached through a crate node print `scope=crate` and
  `via pkg:<crate>`; a true file-level dependent prints `scope=file`. That
  per-row scope is what keeps a crate-level match from being read as a
  dependent of the file itself.

#### Family expansion (`GAP-048`)

Resolving a `pkg:<crate>` node also matches that crate's **family**: the
member nodes the parser emits for named imports (`pkg:<crate>::<item>`) and
underscore-sibling companion crates (`pkg:<crate>_<sibling>`, e.g.
`serde_derive` for `serde`). The match is prefix-anchored — `pkg:ab::Thing`
never leaks into a query on `pkg:a`.

The tradeoff, stated plainly: **the reported count can exceed the number of
direct importers of the queried file.** A crate-level query also counts
importers of its sibling members (`pkg:grep_matcher`, `pkg:grep_searcher` for
`pkg:grep`), and a symbol member counts once per importing file. That is
deliberate — the parser emits member/member-crate nodes as the import targets,
so a crate query that matched only the exact `pkg:<crate>` node under-reported
the blast radius (the pre-GAP-048 measurement was 6 of 148 serde importers).
The cost is precision at the crate boundary.

To get just the direct importers of one file: filter the output to
`scope=file` rows (text output labels every row), or query the specific
`pkg:<crate>::<item>` node instead of the file. When a file-form query does
return crate-scoped rows, the text output ends with a one-line `note:` saying
how many rows were crate-scoped and that family expansion is included.
`--format json` adds no prose — every row carries `scope` and `via` instead.

#### Review set (`GAP-117`)

The answer leads with the queried file itself (`scope=self`, `depth: 0`), so an
empty row list is never mistaken for "there is no such subject" and a file with
no in-graph neighbours still answers with something.

After the subject come the dependents (unchanged `scope=file` / `scope=crate`
semantics), then a bounded both-direction expansion:

- `scope=dependency` — files reached by an OUTGOING edge, from the subject or
  from a node the expansion reached: the helper a rule calls, the header an
  implementation defines, the properties class a configuration binds;
- `scope=link` — files reached by an INCOMING edge during the expansion: the
  test/fixture/build target that references a *dependency* rather than the
  changed file (`alarm.cc → include/grpcpp/alarm.h ←
  test/cpp/common/alarm_test.cc ← test/cpp/common/BUILD`).

The expansion follows at most two forward hops and two reverse hops, so it
stays a review set rather than the whole repository. `pkg:` / `sys:`
pseudo-nodes are never emitted as rows — a review set is a list of files.

`--format json` emits `{"subject": "<path>", "total": <row count>,
"files": […]}`. On the degraded streaming path (the DuckDB cache skipped
replay) the expansion is skipped and the answer is the subject row plus the
streaming dependents — smaller, never wrong or empty.

### `understand`

Multi-resolution harmonic context output for a natural-language task.

```bash
hilo graph understand "how does plugin execution get sandboxed"
```

Output is three tiers — `## MAP` (file → symbols), `## SIGNATURES`
(`file:line  signature`) and `## DETAIL` (`file [provenance=…, score=…]` plus
whitespace-minified source). A tier with nothing to show says
`(no files in this tier)` instead of printing an empty body.

Token budget override (default: 6000):

```bash
hilo graph understand "how does plugin execution get sandboxed" --budget 12000
```

The budget sizes the harmonic tiers only — it caps the `DETAIL` tier at 60% of
the budget in characters, so for a fixed graph the output is non-decreasing in
`--budget` (higher budgets admit more detail blocks; `--budget 1` and
`--budget 200` both fit none and report the empty tier). The flat resolution
(MCP `vfs_graph_understand` with `resolution: "flat"`) ignores the budget by
design: it returns every matched file's detail block. The CLI itself only
exposes the harmonic resolution.

The `MAP` tier lists each file's definitions, with an allowance that follows
the task text: a file whose definitions name the task — one of them carries a
task word as a whole word, e.g. `dispatch_request` for "request" or
`ServeHTTP` for "servehttp" — lists its definitions in source order up to 24,
and any task-named definition past that is appended, so the file's own flow
vocabulary is never truncated. A file that only mentions the task in passing
keeps the historical 8. `SIGNATURES` covers the same definitions for the
`Detail`/`Signature` tiers.

`local:` import specifiers (`local:../duckdb/connection`) never appear as rows:
an unopenable import string names nothing, so it is resolved to the file it
names — against the importing file's own directory, with
extension/`index` probing — and the resolved file takes its place. A
specifier whose target is not in the graph is dropped rather than printed as
an empty row.

The pack also seeds the **modules the task's domain object names** (GAP-101).
A task token that matches a handful of distinct module sites is an object
name, and its modules are seeded just below the literal anchors:

- a file whose own name carries the token as a module word —
  `pydantic/type_adapter.py` for "TypeAdapter", `alarm.cc` for "alarm",
  `B905.py` for "B905";
- a directory named after it — `django/db/models/fields/` for "field",
  `cli/` for "cli" — and the files that directory directly holds;
- the object's **registration site**: module roots (`mod.rs`, `lib.rs`,
  `main.rs`, `__init__.py`, `index.*`, `BUILD`, `CMakeLists.txt`) and the
  sub-packages declared beside the object's own module — the `helpers.rs` next
  to a rule's `mod.rs`, the `BUILD` target that registers `alarm_test.cc`.

A token matching more than 40 distinct module sites is ordinary task prose
(`source`, `context`, `tests`) and seeds nothing; so is a token whose only
module files are test fixtures. Seeds are **additive**: a file that already
anchored keeps rank `1.0` and a seeded file sits at `0.95` and below, above a
one-hop traversal neighbour (≤ 0.8), and the whole result is re-capped at
`max_nodes` — so the boost adds reach at the cost of the lowest-ranked
traversal context, never at the cost of an anchor.

Measured against the six pinned Wave 11 context-pack cases (Pydantic
TypeAdapter, Ruff B905, Svelte keyed-each, Deno CLI/worker, Spring Boot
auto-configuration, gRPC Alarm), with the committed reduced-corpus fixtures:
mean required-path recall rises from 2.8% to 61.8%, and the strongest case
(Deno) improves rather than regresses. The fixtures and their provenance live
in `hilo-graph/tests/fixtures/wave11-context/`; the final full-corpus
re-measurement is GAP-118.

### `search`

Deterministic semantic code search (TF-IDF + BM25 + RRF) over three weighted
channels — path, the symbols a file defines, and the file's own
documentation. Natural-language prompts therefore reach the file that *owns*
a behaviour, not just the files that name it in their paths; query question
words and prepositions are dropped before matching.

```bash
# Top 20 matches (default)
hilo graph search "rate limiter"

# Natural-language intent — returns the defining file (indexing symbols and
# the documentation vocabulary comments/docstrings carry), not just files
# whose paths happen to contain the words
hilo graph search "where is the rate limiter applied?"

# Custom result limit
hilo graph search "rate limiter" --limit 50

# Cheap path-only index (skips symbol + documentation extraction)
hilo graph search "rate limiter" --no-symbols
```

### `module`

Per-module statistics and test coverage.

```bash
hilo graph module hilo-graph/src
```

### `untested`

List source files with no test coverage.

```bash
hilo graph untested
```

### `rule-list`

List all rules defined in the manifest.

```bash
hilo graph rule-list
```

### `rule-check`

Execute a named rule query against the dependency graph. Rules are defined
in the project manifest; this repo currently defines none (see
`hilo graph rule-list`).

```bash
# List rules defined in the manifest
hilo graph rule-list

# Check a named rule (fails with "Rule not found" unless defined in .vfs/manifest.yaml)
hilo graph rule-check <RULE_NAME>
```

### `clean`

Delete the cached dependency graph (`edges.jsonl` + DuckDB cache) so the
next `graph warm` re-parses every source file from scratch. The reset path
for a corrupted or stale graph database.

```bash
hilo graph clean
```

### `surfaces`

Enumerate the repository's externally-visible contract surfaces (COV-1) —
CLI verbs and flags, MCP tools, FFI exports, FUSE ops, manifest config keys,
and public API items — and write them to `.vfs/graph/surfaces.jsonl`. Two
different kinds of provider feed it: the `cli_verb` / `cli_flag` / `mcp_tool`
rows come from the running binary's **own** registries, and the remaining
kinds are parsed from **Hilo's own** source files.

```bash
hilo graph surfaces
hilo graph surfaces --json
hilo graph surfaces --kind mcp_tool
```

`--json` prints the inventory (`schema`, `scope`, `surfaces`, `census`);
`--kind` restricts both the rows and the census to one kind.

**Scope.** Because the compiled rows always describe the `hilo` binary
itself, `graph surfaces` classifies the tree before enumerating it and
declares which it answered:

- `scope: "self"` — the tree is a Hilo checkout (it carries the workspace
  markers: `hilo-cli/src/cli.rs`, `hilo-mcp/src/tools/mod.rs`,
  `hilo-graph/src/lib.rs`), so the full self-inventory is returned and
  written to `.vfs/graph/surfaces.jsonl`.
- `scope: "foreign"` — the tree is not Hilo. **No** Hilo-owned row is
  presented as the target's surfaces: the command prints the `scope: foreign`
  marker with empty `surfaces` / `census`, writes nothing, and exits
  non-zero. Hilo does not derive the surface inventory of an arbitrary
  repository.

The classification walks up from the current directory (like `cargo` finding
`Cargo.toml`), so a run inside a Hilo subdirectory still reads as `self`; it
stops at the first repository root that is not Hilo, so a foreign checkout
nested under a Hilo tree reads as `foreign`.
### `test-classes`

Report the test-class taxonomy: how many tests of each **class** exist and,
per surface, the set of classes that actually reach it (joined through the
COV-2 coverage links). A surface with forty unit tests and zero
integration/e2e/conformance coverage is no longer indistinguishable from a
well-covered one — class diversity is the signal.

Requires the COV-1 inventory (`.vfs/graph/surfaces.jsonl`, from
`hilo graph surfaces`) and the COV-2 links (`.vfs/graph/coverage_links.jsonl`,
from `hilo graph coverage-links`). Each missing input is named in the error.

```bash
hilo graph surfaces          # COV-1: enumerate the contract surfaces
hilo graph coverage-links    # COV-2: link tests to surfaces with evidence
hilo graph test-classes      # COV-3: per-class totals + per-surface class mix
hilo graph test-classes --json
```

The eight classes. The rule is precedence-ordered (first match wins): the path
component / file name is consulted first, then a code marker — and the marker
layer applies only to a **test-bearing** file.

| class | path component / file name | code marker |
|-------|----------------------------|-------------|
| `bench` | `benches/`, `benchmark/`, `benchmarks/`, `*bench*` | `#[bench]`, `criterion_*` |
| `property_fuzz` | `fuzz/`, `fuzzers/`, `fuzz_targets/`, `*fuzz*` | `proptest!`, `quickcheck!`, `#[quickcheck]`, `fuzz_target!`, `libfuzzer_sys` |
| `chaos_fault` | `chaos/`, `faults/`, `fault_injection/`, `*chaos*`, `*fault*` | `fault_inject`, `inject_fault` |
| `conformance_golden` | `conformance/`, `golden/`, `goldens/`, `*golden*`, `*conformance*`, `*snapshot*` | `insta::assert*`, `assert_snapshot`, `expect_file!`, `expect_test` |
| `e2e_process` | `e2e/`, `e2e_tests/`, `end_to_end/`, `*e2e*` | `assert_cmd`, `cargo_bin`, `process::Command`, `subprocess`, `pexpect`, `rexpect` |
| `doc_smoke` | `doctests/`, `doc_tests/`, `smoke/`, `*smoke*`, `*doctest*` | `doctest` |
| `integration` | any `tests/` / `test/` / `spec/` directory | — |
| `unit` | any other test-bearing file (in-source / co-located) | — |

A file is **test-bearing** when its path matches a test pattern or a
class-specific directory, it declares a test function (`#[test]`, Go
`func Test*`, Python `def test_*`), or its source carries a test-framework
marker — so a production file that merely mentions `golden` in a comment is
not a conformance test.

A surface reached by exactly one class is flagged `CLASS-GAP` with the missing
classes named; a class with zero tests reports `0` and still names itself
(never an absent key). The rule is pinned by a test over a fixture tree that
holds one test of each class (`hilo-graph/tests/fixtures/test_classes/`).

### `rollup`

Group the COV-1 surfaces into **features/components** and roll coverage and the
class mix up to that level (COV-4), so "is the workspace-mount feature
covered?" is one query rather than a per-ask re-derivation.

```bash
hilo graph surfaces          # COV-1: enumerate the contract surfaces
hilo graph coverage-links    # COV-2: link tests to surfaces with evidence
hilo graph rollup            # COV-4: group + roll up (--by feature, the default)
hilo graph rollup --by crate
hilo graph rollup --by module --json
hilo graph rollup --group hilo-cli   # one group's own gap list
```

A surface's group is chosen by one documented precedence — first match wins:

| # | source | how it is decided |
|---|--------|-------------------|
| 1 | `user.vfs.feature` | explicit xattr on the surface's owner file |
| 2 | `user.vfs.component` | explicit xattr, consulted when no feature is set |
| 3 | crate boundary | nearest ancestor carrying a project manifest (`Cargo.toml`, `go.mod`, `package.json`, `pyproject.toml`, …) |
| 4 | module path prefix | the file's module path with the file name and a trailing `src/` dropped (`hilo-cli/src/commands/graph.rs` → `hilo_cli::commands`) |

The **chosen source is recorded on each surface row** (`source`:
`xattr_feature` | `xattr_component` | `crate` | `module`), so an annotated
group is always distinguishable from a structural stand-in — it is never
re-inferred when the report is read.

`--by crate` / `--by module` bias the structural fallback; explicit
annotations still win in every mode, because precedence (1) is unconditional.
On a repo with no annotations `--by feature` therefore degrades to the crate
boundary — the same grouping `--by crate` produces — because in an
un-annotated tree a crate is the coarsest structural stand-in for a feature.

An un-annotated repo still groups, and the report **states which fallback it
used** (`fallback_note`, plus the per-source `source_census`), so the report is
never empty and never silently fabricated.

Each group reports its member surface count, covered/uncovered counts, its
class mix (per test class, how many members it reaches — all eight classes,
zeroes included), and its own gap lists. The arithmetic is checked by a test:
`sum(group.surface_count) == total_surfaces == surfaces.len()` — every surface
lands in exactly one group, with no double counting and none dropped.

`--group <name>` restricts the report to one group and answers with that
group's own gap list (its uncovered surfaces and its `class_gap` surfaces); an
unknown name is a loud error, never a success-shaped empty report.

Requires the COV-1 inventory (`.vfs/graph/surfaces.jsonl`). The COV-2 links
(`.vfs/graph/coverage_links.jsonl`) supply the coverage counts and the class
mix — without them every surface reports **uncovered with that absence named**
in the report, never as a silent zero. Wiring numbers (GAP-113) are omitted
until that row lands, with the omission stated in `rules`.

### `audit` (COV-5 — symbol-level connection + test audit)

The graph's primitives (`impact`, `related`, `untested`) are FILE-scoped, so a
public function that is defined but never called from any entrypoint — or
called but never linked to a test — is invisible as an individual fact. The
audit answers, **per public function**, two questions and returns the NAMED
list, never a score:

1. is it reachable from a declared entrypoint?
2. does it carry a coverage link?

```bash
hilo graph audit            # scan the current directory
hilo graph audit --json     # locked shape
hilo graph audit path/to/project
```

The result partitions into three buckets, each with a count, the rule that
produced it, and its named members:

| bucket | meaning |
|--------|---------|
| `unreachable` | no caller path from a declared entrypoint |
| `unlinked` | reachable from an entrypoint, but no test link |
| `ok` | reachable AND linked |

```bash
hilo graph audit --json | jq '.buckets | keys'
# ["ok", "unlinked", "unreachable"]
```

The rules, exactly:

- **Public function** — Rust bare `pub fn` (`pub(crate)`/`pub(super)` are NOT
  public), Go `func`/method whose name is exported (leading uppercase), Python
  `def`/`async def` not starting with `_`, TypeScript/JavaScript declarations
  inside an `export`. Test files and generated files define no audited symbol
  (they are the link source, not the surface).
- **Declared entrypoint** — a file whose `classify_file` role is `entrypoint`
  (filename convention `main.rs`/`__main__.py`/`index.js`/`Program.cs`/
  `Main.kt`/`index.php`/`main.swift`, or an AST-detected `main`/canonical entry
  symbol). No hand list.
- **Reachable** — the defining file is itself a declared entrypoint, or a file
  reachable-from-an-entrypoint names the symbol, or the symbol is named inside
  its own file beyond its definition sites while that file is reachable. File
  reachability is the forward closure of `A names a public symbol of B` from
  the entrypoints.
- **Reference** — a lexical identifier occurrence (comments and string
  literals included). It deliberately over-approximates, so `unreachable` is a
  conservative, no-false-accusation claim: "the name is textually absent from
  every reachable file".
- **Linked** — a test file names the symbol, or an on-disk COV-2 link
  (`.vfs/graph/coverage_links.jsonl`) targets its name or file.

Languages without a public-function extractor are named under `unknown` — they
are never collapsed into `ok`. `census` carries a per-language row
(`files`, `public_symbols`, the extractor rule), so a zero is distinguishable
from an unexercised extractor, and every bucket reports its rule whether or not
it is empty. The command is a REPORT, not a gate: it always exits 0. (feat(graph): COV-5 symbol-level connection + test audit)

## `hilo classify`

Auto-tag every source file with `user.vfs.role` and `user.vfs.status`
using tree-sitter AST queries. No LLM required.

```bash
# Dry run — show what would be tagged
hilo classify --dry-run

# Apply tags
hilo classify

# Verbose output (per-file)
hilo classify --verbose

# Enable feature inference (sets user.vfs.feature xattrs from directory structure)
hilo classify --features
```

Roles detected: `entrypoint`, `library`, `test`, `script`, `example`,
`config`, `build`, `generated`, `unknown`.

Statuses detected: `stable`, `beta`, `unstable`, `deprecated`, `unknown`.

## `hilo mount`

Mount the current directory as a FUSE filesystem with xattr passthrough.

```bash
mkdir /mnt/vfs
hilo mount /mnt/vfs

# With triggers (auto-reparse on file changes)
hilo mount /mnt/vfs --triggers

# Allow other users to access
hilo mount /mnt/vfs --allow-other

# Run in the background (detached daemon — returns immediately)
hilo mount /mnt/vfs --daemon
```

**Note:** `hilo mount` runs in the foreground and blocks the terminal
until unmounted. Run it in a separate terminal, background it with `&`,
or pass `--daemon` to detach it into a background process that keeps the
mount alive until `fusermount -u /mnt/vfs` unmounts it.

## `hilo serve`

Start the MCP server for agent integration.

```bash
# Stdio transport (for Claude Desktop, Hermes)
hilo serve --mcp
```

Run it from inside a Hilo project (`hilo init` first): without a manifest the
server refuses to start and names `hilo init`, rather than exposing tools
that can only answer from an empty graph.

## `hilo backend`

Manage virtual backends (S3, gdrive, onedrive, dropbox, external) and
backend-backed workspaces via sync tools
(spec `specs/backend-backed-workspace-spec.md` §9).

### `mount`

Mount a virtual backend.

```bash
hilo backend mount --type s3 --bucket my-bucket --prefix data --at /s3

# Explicit region (default: us-east-1)
hilo backend mount --type s3 --bucket my-bucket --at /s3 --region eu-west-1

# S3-compatible endpoint (MinIO et al.): connect THERE with static
# credentials and path-style addressing
hilo backend mount --type s3 --bucket my-bucket --at /s3 \
  --endpoint http://minio.internal:9000
```

`--endpoint <URL>` points the S3 driver at an explicit S3-compatible
endpoint (MinIO, R2, self-hosted gateways). When set, the driver connects
there with static credentials and path-style addressing, ignoring
`AWS_ENDPOINT_URL`. Without it, the endpoint is resolved from the
environment at sync time: `AWS_ENDPOINT_URL` is the env fallback, and the
resolved endpoint is disclosed on every `backend sync` plan line — never
silently consumed.

Backend-backed workspace mounts (new surface; `--type` s3/gdrive/onedrive/
dropbox/external):

```bash
# S3 with the native engine (default tool for s3)
hilo backend mount --type s3 --bucket my-bucket --prefix workspace/ \
  --at /mnt/vfs/ws --tool native --mode mirror

# External tool remote (gdrive/onedrive/dropbox/external)
hilo backend mount --type gdrive --remote "gdrive:workspace" --at /mnt/vfs/gd \
  --tool auto --mode stream --poll-secs 120

# Optional flags: --tool auto|native|rclone|s3sync|gdrive|onedrive|dropbox
#                 --mode stream|mirror (default mirror)
#                 --endpoint <URL> (S3-compatible endpoint: MinIO et al.)
#                 --url <URL> (remote URL for --type external)
#                 --ignore-file <PATH> (extra ignore file, optional)
#                 --poll-secs <N> (default 60)
#                 --no-default-ignores
```

`--tool auto` resolution: s3 → native engine; gdrive/onedrive/dropbox →
the matching official CLI if installed, else `rclone`, else the mount fails
with a "required tool not found" error. A successful mount writes the entry
to `.vfs/backends/mounts.yaml` (spec §11.3). The legacy `git`/`local` mount
types are NOT supported: `hilo backend mount --type git|local` fails with
`unknown backend type` (exit 2, nothing written); they were removed
2026-09-22 (DF-WARPFS-19) because they printed success while cloning into
`~/.hilo/worktrees`, invisible to `list`/`sync`.

### `list`

List all mounted backends.

```bash
hilo backend list
```

### `sync`

Sync a mounted backend against the current workspace (ignore-aware: matched
files stay local-only, never transferred).

```bash
hilo backend sync              # two-way (default)
hilo backend sync --push       # upload local changes only
hilo backend sync --pull       # download remote changes only
hilo backend sync --push sub/  # limit to a subtree
```

Conflict ledger: LWW resolutions are appended to
`.vfs/sync/conflicts.jsonl` (one JSONL record per resolution, even when
the winner matches the transfer direction — spec §7).

### `setup`

Detect sync tools and credentials for a backend type. Writes nothing.

```bash
hilo backend setup                # all types
hilo backend setup --type s3      # s3|gdrive|onedrive|dropbox|external
```

## `hilo workspace`

Manage multi-repo workspace mounts.

### `mount`

Mount all repos and backends from the manifest.

```bash
hilo workspace mount /mnt/hilo
```

Each repo's `ref` is resolved on a fresh clone by trying, in order: the
ref as given, as a branch (`refs/heads/<ref>`), then as a tag
(`refs/tags/<ref>`). If none resolve, the error names every form tried
and lists the repo's available branches (capped at the first 10), instead
of a bare `revspec ... not found` message.

### `unmount`

Unmount a workspace.

```bash
hilo workspace unmount /mnt/hilo
```

### `sync`

Sync a local directory against a remote backend prefix (S3 today).
`--both` (default) mirrors non-ignored files in both directions (newer side
wins); `--push` uploads only; `--pull` downloads only. The direction is
enforced on the sync plan, not just labeled: a `--push` run never
downloads and a `--pull` run never uploads — files that would move the
other way are skipped and reported (`N skipped by direction`) and stay
available for a later `--both`/`--push`/`--pull` run. Files matched by
the ignore file stay local-only and are never transferred.
See docs/ignore-file.md for the ignore format.

```bash
hilo workspace sync --bucket my-bucket --prefix data --at ./ws --dry-run
hilo workspace sync --bucket my-bucket --prefix data --at ./ws --push   # upload only
hilo workspace sync --bucket my-bucket --prefix data --at ./ws --pull   # download only
```

### `ephemeral`

List ephemeral (rebuildable/redownloadable) files in the workspace — the
built-in catalog covers common build/artifact/cache paths (`target/`,
`node_modules/`, `.venv/`, `*.o`, `.vfs/graph/`, ...); a `.hiloephemeral`
file (same git-ignore-style syntax as `.hiloignore`) adds or (`!`) removes
patterns. Output is TSV: `path<TAB>size<TAB>reason`. PATH arguments limit
the listing to subtrees.

```bash
hilo workspace ephemeral
# target/artifact.bin	64	target/
# node_modules/pkg/index.js	32	node_modules/
```

### `wipe`

Plan or apply a wipe of ephemeral files. Default is a dry-run plan; pass
`--apply` to delete. Only ephemeral files are removed, `user.vfs.ephemeral
= false` is the only wipe protector (set it with `hilo meta --set
ephemeral --value false <file>`), and symlinks are never touched.

```bash
hilo workspace wipe --ephemeral
# would remove	target/artifact.bin
# would free 64 bytes across 1 file(s) (dry-run; pass --apply to delete)
hilo workspace wipe --ephemeral --apply
# removed	target/artifact.bin
# freed 64 bytes across 1 file(s)
```

## `hilo ignore`

Inspect ignore decisions (the git-ignore-style `.hiloignore` file, with
`.vfsignore` accepted as a legacy alias).

### `check`

Report whether a path would be ignored, and which rule decided it.

```bash
hilo ignore check build/out.o
# path: build/out.o
# ignored: true
# rule: build/
# source: /path/to/workspace/.hiloignore
```

## `hilo plugin`

Load and manage wasm plugins.

> **NOT IMPLEMENTED (DF-WARPFS-59):** plugin execution does not exist.
> `load`/`list` only validate metadata and persist files in
> `.vfs/plugins/`; declared manifest hooks never fire (see
> `docs/hilo-plugins.md`).

See [`docs/hilo-plugins.md` §CLI](hilo-plugins.md#cli) for the full
validate-and-persist behaviour, or load the checked-in example at
[`examples/plugins/minimal.wasm`](../examples/plugins/README.md).

### `load`

Load a .wasm plugin and register it in the runtime. *(The "runtime" is a
metadata registry — nothing is executed; DF-WARPFS-59.)* Validates the
`\0asm` magic + version 1 header, then persists the file to
`.vfs/plugins/`.

```bash
hilo plugin load examples/plugins/minimal.wasm
# loaded plugin: minimal
#   hooks: 0
#   edge_types: []
# persisted to: .vfs/plugins/minimal.wasm
```

### `list`

List plugins discovered in `.vfs/plugins/`. Invalid files (bad magic or
version) are skipped, and the version is reported as the unknown marker `?`
— no manifest is parsed yet.

```bash
hilo plugin list
# plugins in .vfs/plugins:
#   minimal v? — 0 hooks, 0 edge types
```
