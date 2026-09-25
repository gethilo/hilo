# Dogfood Run 16 — Go FFI consumer (first end-to-end) — 2026-09-25

**Angle (stale-surface rule):** runs 8/11 generated the Go bindings but never
drove a real Go consumer; run 11 stopped at Python. This run wrote a real Go
program that answers a structural question about two real repos through the
UniFFI Go bindings — the surface a Go-based agent harness would use.

## What was built (the consumer)

A `dogfood` Go module with the generated bindings vendored as an internal
package, plus a main program that:

1. `NewHiloHandle(repo)` → `VfsGraphStats()` — snapshot (files/edges/tested%)
2. `VfsGraphRelated(path)` — outgoing edges of a hot file
3. `VfsGraphImpact(path, 3)` — transitive dependents (blast radius)
4. `VfsSetMetadata` / `VfsGetMetadata` — write an audit marker xattr through
   the FFI and read it back (also cross-checked with `hilo meta` CLI)
5. `VfsListDirectory(".")` — orientation
6. error path: impact on a nonexistent file (must be `HiloError: NotFound`)

Corpora: a hand-built 4-file Go corpus (8 edges) and a 1775-file copy of a
real polyglot repo (57 graphed files, 373 edges, 1.8% tested).

## What worked (cross-surface proof, Go)

- The full documented generation block (`cargo run -p hilo_ffi --bin
  uniffi-bindgen …` + `uniffi-bindgen-go v0.4.0+v0.28.3 --locked`) produces
  compiling cgo bindings: `go build` clean with only
  `CGO_LDFLAGS="-L… -lhilo_ffi"` and `CGO_CFLAGS="-I…"`.
- stats / related / listdir / metadata get+set / NotFound error path all
  return exactly what the CLI returns on the same graph (stats byte-equal,
  related edge lists equal, impact-on-direct-edges equal).
- xattr written through the **Go** FFI reads back through the **CLI**
  (`hilo meta`) — cross-surface proof on the Go path.
- Checksums: generated checksum functions match the .so (no checksum
  mismatch panics on 0.3.1-dev).

## What broke (P0: silent wrong answers, not crashes)

**`vfs_graph_impact` is CWD-dependent — and silently returns the WRONG
answer, no error.** The pkg-node resolver inside the impact BFS resolves the
(relative) subject path against the **process CWD**, not the handle root
(`python_module_for_file` walks up looking for `__init__.py` —
hilo-graph/src/resolution.rs:359 — and `go_package_for_file` looks for
`go.mod` the same way).

Consequences, all reproduced:

| process CWD | Go FFI `impact(parser.py, 3)` | CLI `graph impact parser.py` from `plugin/` |
|---|---|---|
| repo root (tj copy) | 3 dependents (correct, == Python FFI + CLI from root) | 1 dependent, via WRONG node `pkg:terminal_jail.interruptor.parser` (missing the `plugin.` prefix) |
| anywhere else | **0 dependents, rc=0, silent** | (query by absolute path impossible; bare relative path is repo-relative, so the CLI fails to find the file instead) |

The same file, same graph, same lib: the answer flips with an environment
variable the caller doesn't know it controls. A consumer that calls
`impact()` from any directory other than the repo root silently gets an
empty blast radius — exactly the "0 results and no error" class from the
dogfood skill's pitfall list. The `FfiConverterString.Lower` call site is
innocent (proven by a raw-byte cgo probe AND by the explicit `pkg:` node
query, which returns 3 dependents at depth 1 through the Go bindings).

Minimal reproduction (any consumer, any language):

```
cd <repo-root>            # hilo init && hilo graph warm already done
# cwd=repo-root:  impact("plugin/terminal_jail/interruptor/parser.py", 3) → 3
cd /tmp
# same process now, same handle:  impact(same, 3) → 0, no error
```

CLI re-confirmation of the same root cause: `cd <repo>/plugin && hilo graph
impact plugin/terminal_jail/interruptor/parser.py` fails with "not in the
graph" while `cd <repo> && hilo graph impact …` succeeds — the CLI builds
subjects from CWD-relative filesystem inspection too.

**Fix direction:** `PkgResolver::pkg_node` (and the `is_file`/xattr helpers
it calls) must resolve the subject against the handle root — the FFI layer
knows the root (it canonicalizes it in the constructor, lib.rs:176) but
hands the BFS a bare relative path that downstream filesystem walks
interpret against CWD.

## Friction notes (P2, docs)

1. `hilo-ffi/README.md` says the Go bindings "do not consume this `.so`
   directly" — wrong: the generated cgo code `#include`s `hilo.h` and links
   `libhilo_ffi.so` (`ldd go-consumer` proves it). The Python rename step is
   documented; the Go link/rpath step (the one that actually makes or breaks
   a Go consumer) is not documented at all. The README's own verification
   block only checks `find … -name '*.go'` — a `go build` smoke is missing.
2. `cargo build --release -p hilo_ffi --features vendored-openssl` fails:
   the feature does not exist on `hilo_ffi` (it belongs to `hilo-cli` /
   `hilo_backends`). The README's restricted-host paragraph implies it
   applies to FFI builds too.
3. Namespace-level `vfs_get_metadata(path, key)` is a **free function** over
   a raw path: it does NOT join the handle root, and resolves against CWD,
   while handle methods join the root. Two path dialects in one FFI
   (hilo-ffi/src/lib.rs:128 vs the `HiloHandle` impl). At minimum the UDL
   doc-comment should say "absolute path required".

## Perf (hyperfine, release build, warm unless noted)

- `graph impact <hot file> --max-depth 3`: **17.5 ms ± 1.1** (n=20)
- `graph warm` (incremental, cache hit): 15.6 ms ± 0.5 (n=10)
- `graph warm` (cold graph.db/edges/parse-cache removed): 467 ms ± 15 (n=10)
- `graph search "command isolation"`: 219 ms ± 10 (n=20)
- Go consumer end-to-end (spawn+6 calls): dominated by process start,
  single-digit ms per call.

NO PERF ROW — nothing a user waits on; numbers match runs 12–15.

## Install leg

See `.coding-hermes/dogfood-log.md` entry for the bunker result; this file
holds only the consumer-side detail.
