# Dogfood Run 43 — FFI `HiloHandle` Python consumer at HEAD — 2026-10-13

First real consumer drive of the FFI interface. Verdict: 🟢 SHIPPABLE with caveats.

## Promise tested

"A non-Rust application can use Hilo's metadata and graph primitives through native
bindings: generate Python bindings from `hilo.udl`, construct `HiloHandle(root)` with an
explicit repository root (never depending on the process working directory), and drive
stats / related / impact / backend / rules / directory + the two metadata namespace
functions." — docs/hilo-ffi.md.

## The working integration (copy this)

```bash
# 1. build the cdylib (rename step is REQUIRED — docs/hilo-ffi.md §Native library filename)
cargo build -p hilo_ffi
# 2. generate bindings from hilo-ffi/ (NOT the workspace root — see pitfalls)
cd hilo-ffi && cargo run -p hilo_ffi --bin uniffi-bindgen -- \
  generate src/hilo.udl --language python --no-format --out-dir /tmp/hilo-bindings/python
# 3. the rename: UniFFI dlopens libuniffi_hilo.so, cargo built libhilo_ffi.so
cp target/debug/libhilo_ffi.so /tmp/hilo-bindings/python/libuniffi_hilo.so
```

```python
import sys; sys.path.insert(0, "/tmp/hilo-bindings/python")
import hilo                       # 9ms import

h = hilo.HiloHandle("/path/to/repo")   # explicit root — CWD-independent, 0ms
h.vfs_graph_stats()                    # GraphStats(total_files, total_edges, unique_relations, tested_pct)
h.vfs_graph_related("src/app.py")      # GraphEdge(from,to,rel,provenance,confidence)
h.vfs_graph_impact("pkg:src", 2)       # pkg:/sys: forms work; see caveat #1 for file paths
h.vfs_resolve_backend("src/store.py")  # BackendInfo(backend='local-only', ...)
h.vfs_list_directory("src")            # DirectoryEntry(name, entry_type, size, ...)
h.vfs_rule_check("my_rule")            # needs manifest.rules entry; see caveat #3
hilo.vfs_set_metadata(REPO+"/src/app.py", "user.vfs.note", "v")  # namespace fns
hilo.vfs_get_metadata(REPO+"/src/app.py", "user.vfs.note")
```

Cross-surface parity proven both directions: FFI-set xattrs are read by `getfattr` and
`hilo meta`; CLI-set xattrs read back through FFI. Handle methods resolve relative paths
against the repo root from any CWD.

## Caveats (board rows this run)

1. **DF-WARPFS-146 (P1):** `vfs_graph_impact`/`vfs_graph_related` raise `NotFound` for a real
   file whose edges only touch `pkg:` pseudo-nodes (e.g. a file that only imports in-repo).
   The error text names the disk path — misleading; the file exists. Use `pkg:` forms for
   such files, or catch NotFound. CLI `graph impact` prints a graceful note instead.
2. **DF-WARPFS-147 (P2):** the namespace fns `vfs_get/set_metadata` resolve relative paths
   against the process CWD — pass absolute paths, the handle root does not cover them.
3. **DF-WARPFS-148 (P2):** `vfs_rule_check` looks rules up in `manifest.rules`
   (`QueryRule{name,description,query}`), undocumented. Queries run on the DuckDB graph.db
   (only the `edges` table exists; quote reserved `"from"` inside single-quoted YAML).
   Missing rule = `NotFound .vfs/manifest.yaml#rule:<name>` (synthetic path).
4. **DF-WARPFS-149 (P3):** `graph warm` can append a byte-identical duplicate edge to
   edges.jsonl despite `deduplicate: true` (DuckDB stats dedupe; JSONL does not).

## Errors hit, and the right way

- `missing field 'package' at line 30 column 1` from bindgen = you ran it from the workspace
  root; bindgen walked up to the virtual workspace Cargo.toml. Run from `hilo-ffi/`.
- `OSError: libuniffi_hilo.so: cannot open shared object` = the rename step was skipped.
- Rule SQL errors surface as `InternalError ... SQL parse error` — the query language is
  DuckDB SQL over the graph tables, not a DSL.

## Measured (Step 2b)

Full 13-call consumer session 316ms ±12 warm (hyperfine n=20); `import hilo` 9ms;
`HiloHandle(root)` ~0ms; cold first graph query 53ms. Nothing user-noticeable; no PERF row.
