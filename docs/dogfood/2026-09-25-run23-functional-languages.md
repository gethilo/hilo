# Dogfood Run 23 — 2026-09-25 (warpfs-dogfood: Erlang + Elixir + Haskell + Elm languages; functional-family sweep)

## Angle (stale-surface rule)

22 runs had exercised 9 of the 26 claimed languages (Rust, Go, Python, TS/JS,
Java, C/C++, Ruby, PHP, Kotlin) — and never one of the functional family
(Elixir, Haskell, Erlang, Elm, OCaml, Clojure, Scala, ...). Run 23 warms
real-world corpora in four of them: **Elixir** (plug), **Haskell**
(PostgREST), **Erlang** (cowboy), **Elm** (elm-spa-example). Elm is the
sharpest probe available: its extractor is a hand-rolled tree-walk written
against assumed node kinds, and the real `tree-sitter-elm` grammar was never
checked against it (it turns out to match; see below).

HEAD tested: d410fa8 (local master == origin/master). Local release build
`hilo 0.3.1-dev, build: v0.3.0-108-g281b6ff-dirty` (built 2026-09-25).
Corpora under `/tmp/dogfood-warpfs-r23/`.

## Promise under test

*"An agent can answer structural questions about any codebase — in any of
26 languages — by querying a pre-computed metadata graph, in under a
second, without reading files."*

## Verdict: PROMISING-BUT-ROUGH

The cross-language story is consistent and that is itself the finding: the
graph layer (warm, DuckDB, impact/related plumbing, millisecond query
latency) works identically on every language thrown at it, while **each
language's extractor has a defect of its own that the plumbing cannot see**.
Elm and Elixir import extraction are genuinely good; Elixir misses the
qualified-call form; Erlang is near-useless (include attributes only) and
additionally poisons the graph with license-header text; Haskell has a
family-expansion inconsistency that makes its blast radius wrong in both
directions at once.

## What worked (real-use verified)

- **Elm blast radius is exact**: `pkg:Api` → 17/17 == grep of
  `^import Api` (the 18th grep hit is Api.elm itself, the defining file).
  335 edges across 33/34 files, warm 0.86s. The hand-rolled
  `extract_elm_imports` works against the real tree-sitter-elm grammar
  (`import_clause` node kinds match) — a genuine positive result.
- **Elixir import-form extraction is good**: 274 edges across 55 files,
  warm 0.6s; `use Plug.Router`, `import Plug.Conn`, `alias` all become
  `pkg:` edges (58× Plug.Conn, 45× Plug.Test...). `related` on a real
  router module resolves its full import set, 23ms.
- **Haskell extraction is thorough**: 2727 edges across 204/204 files,
  warm 10.3s on a real-world corpus; qualified imports
  (`import PostgREST.Config.PgVersion (PgVersion (..), pgVersion150)`)
  resolve to precise child-module targets.
- **Cross-language consistency of the plumbing**: warm output format,
  coverage accounting, edges.jsonl shape, stats, impact/related latency
  (impact 34ms on 204-file Haskell corpus, n=10) are all identical to what
  runs 1–22 measured on imperative languages. The <1s promise holds
  everywhere except search (below).
- **Impact error messages stay honest**: querying a fabricated node form
  errors cleanly listing accepted id forms (`local:`/`sys:`/`pkg:`/bare).

## What broke

1. **Erlang license-header poisoning (DF-WARPFS-88, P1)** — the substring
   `"AS IS"` in standard BSD comment blocks becomes a `local:AS IS` graph
   node with 16 phantom edges (8 bogus `tested_by`). The extractor
   `extract_erlang_imports` (hilo-graph/src/parser.rs ~1082) does
   `text.contains("-include(")` then string-scans for the first quote pair,
   so comment text inside a matched node leaks into the graph as a
   first-class, queryable node.
2. **Elixir misses the qualified-call form (DF-WARPFS-89, P1)** —
   `pkg:Plug.Conn` impact finds 34 of 44 real dependents on plug (77%).
   14 files reference `Plug.Conn` via direct calls / struct patterns
   (`Plug.Conn.put_status(...)`, `%Plug.Conn{}`) with no import clause.
   In Elixir this is the *most common* dependency form. Also emits a
   literal `pkg:unquote(target)` edge from `use Plug.Router` with a
   variable target.
3. **Haskell family expansion inconsistent (DF-WARPFS-90, P1)** —
   `pkg:PostgREST.Config` impact returns 57 files vs 26 grep-direct
   importers: 31 transitives included AND 4 real child-importing library
   files missing. Over-counts and under-counts simultaneously.
4. **Erlang coverage 6% (DF-WARPFS-91, P2)** — 178/189 cowboy files "no
   imports"; no `-behaviour`/callback/local-call extraction. Green warm,
   near-empty graph.
5. **search --limit is a post-filter (DF-WARPFS-92, P2)** — 448ms vs
   445ms with `--limit 5` on postgrest; 5th/6th language confirmation of
   the DF-WARPFS-76 family.

## Real-use walkthrough (reproducible)

```bash
cd /tmp/dogfood-warpfs-r23/plug && hilo init && hilo graph warm
hilo graph impact pkg:Plug.Conn           # 34 files, 26ms
hilo graph related lib/plug/router.ex     # import set, 23ms
cd ../postgrest && hilo init && hilo graph warm   # 2727 edges, 10.3s
hilo graph impact pkg:PostgREST.Config    # 57 files — see DF-90 caveats
hilo graph search "database connection pool"      # 448ms, --limit is a no-op
cd ../cowboy && hilo init && hilo graph warm      # 34 edges across 11 files — see DF-91
hilo graph impact "local:AS IS"           # works! that's the bug (DF-88)
cd ../elm-spa-example && hilo init && hilo graph warm
hilo graph impact pkg:Api                 # 17/17 exact
```

## How a user should treat each language today (field notes)

| Language | warm | import forms captured | blast-radius trust |
|---|---|---|---|
| Elm | 0.86s / 34 files | `import X`, `import X as Y`, `exposing` | **exact** (17/17) |
| Elixir | 0.6s / 78 files | use/import/alias | 77% — misses qualified calls; treat as import-graph only |
| Haskell | 10.3s / 204 files | qualified + plain imports | verify against grep: family expansion asymmetric |
| Erlang | 0.2s / 189 files | `-include`/`-include_lib` only | near-zero; includes poisoned by comment text |

## Install leg

SKIPPED (DF-WARPFS-93): las-01/03/04 down, las-02 bunkerd crash-looping
unchanged from run 22, fair retries after 95s/60s waits. No spawn on any
host; battery not launched.

## Verdict label reasoning

Not SHIPPABLE: three of four languages tested have a P1 extractor defect
that produces confidently-wrong (`conf=1.00`) structural answers. Not
DOES-NOT-DELIVER: Elm is exact, Elixir/Haskell import graphs are real, the
query plumbing is fast and honest, and every defect is localized to one
extractor function. The 26-language claim currently holds only for a
subset; per-language trust levels need to be visible to users.
