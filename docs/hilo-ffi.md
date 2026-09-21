# hilo-ffi — UniFFI Language Bindings

The `hilo-ffi` crate (11th workspace member) defines the UniFFI interface
for Hilo and generates language bindings for **Go, Python, Kotlin, and
Swift** — letting non-Rust applications use Hilo's metadata and graph
primitives through a native SDK.

## Interface

The `.udl` file (`src/hilo.udl`) is the **source of truth**. Metadata calls remain namespace functions. Repository-sensitive graph, backend, rule, and directory operations are methods on `HiloHandle`, whose constructor requires an explicit repository root. Embedded hosts therefore never depend on their process working directory.

| Function | Description |
|---|---|
| `vfs_get_metadata(path, key)` | Get a `user.vfs.*` xattr value |
| `vfs_set_metadata(path, key, value)` | Set a `user.vfs.*` xattr value |
| `vfs_graph_related(path)` | Get graph edges related to a file |
| `vfs_graph_impact(path, max_depth)` | Transitive dependents via BFS |
| `vfs_graph_stats()` | Graph summary statistics |
| `vfs_resolve_backend(path)` | Backend info for a virtual path |
| `vfs_rule_check(rule_name)` | Run a DuckDB rule query |
| `vfs_list_directory(path)` | List virtual directory entries |

Supporting dictionaries (`MetadataResult`, `SetMetadataResult`,
`GraphEdge`, `GraphRelatedResult`, `GraphImpactEntry`,
`GraphImpactResult`, `GraphStats`, `BackendInfo`, `RuleCheckResult`,
`DirectoryListing`) and the `HiloError` enum (`InvalidInput`, `NotFound`,
`BackendUnavailable`, `InternalError`) are defined in the same UDL.

## Generating Bindings

The repository provides the UniFFI 0.28 generator as a Cargo binary. Go uses
the matching third-party generator release. From a fresh clone with no
preinstalled generator, this one block produces and checks both artifacts:

```bash
cd hilo-ffi
rm -rf /tmp/hilo-bindings
mkdir -p /tmp/hilo-bindings/python /tmp/hilo-bindings/go
cargo run -p hilo_ffi --bin uniffi-bindgen -- generate src/hilo.udl --language python --no-format --out-dir /tmp/hilo-bindings/python
cargo install uniffi-bindgen-go --git https://github.com/NordSecurity/uniffi-bindgen-go --tag v0.4.0+v0.28.3 --locked
uniffi-bindgen-go src/hilo.udl --out-dir /tmp/hilo-bindings/go --no-format
test -f /tmp/hilo-bindings/python/hilo.py
test -n "$(find /tmp/hilo-bindings/go -name '*.go' -print -quit)"
```

Kotlin and Swift use the same repository-owned binary:

```bash
cargo run -p hilo_ffi --bin uniffi-bindgen -- generate src/hilo.udl --language kotlin --out-dir hilo-kotlin/
cargo run -p hilo_ffi --bin uniffi-bindgen -- generate src/hilo.udl --language swift --out-dir Hilo/
```

`hilo-ffi/tests/bindgen_cli.rs` runs the Python command in CI, preventing the
UDL, generator entry point, and docs workflow from silently drifting.
Generated code is NOT committed — the `.udl` is the source of truth.

## Output Targets

| Language | Output directory | Package/Module |
|---|---|---|
| Go | `hilo-go/vfs/` | `vfs` |
| Python | `hilo/` | `hilo` wheel |
| Kotlin | `hilo-kotlin/` | `hilo-kotlin` |
| Swift | `Hilo/` | `Hilo` |

## Build

```bash
cargo build -p hilo_ffi
```

The build script (`build.rs`) auto-generates Rust scaffolding from
`hilo.udl`.
