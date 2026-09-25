# hilo-ffi — UniFFI Language Bindings

This crate defines the UniFFI interface for Hilo and generates language bindings for Go, Python, Kotlin, and Swift.

## Interface

The `.udl` file (`src/hilo.udl`) is the **source of truth**. Metadata calls are namespace functions; repository-sensitive graph, backend, rule, and directory calls are methods on `HiloHandle`, constructed with an explicit repository root:

```python
handle = HiloHandle("/absolute/path/to/repository")
stats = handle.vfs_graph_stats()
```

This keeps embedded consumers independent of the host process's working directory.

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

## Generating Bindings

The repository builds its own pinned Python/Kotlin/Swift generator. Go uses the
UniFFI 0.28.3-compatible `uniffi-bindgen-go` release. This block works from a
fresh clone with neither generator preinstalled and leaves inspectable Python
and Go artifacts under `/tmp/hilo-bindings`:

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

For the other repository-owned generators:

```bash
cargo run -p hilo_ffi --bin uniffi-bindgen -- generate src/hilo.udl --language kotlin --out-dir hilo-kotlin/
cargo run -p hilo_ffi --bin uniffi-bindgen -- generate src/hilo.udl --language swift --out-dir Hilo/
```

`tests/bindgen_cli.rs` executes the in-repository Python generator so UDL or CLI
drift fails CI. Generated code is NOT committed — the `.udl` is the source of
truth.

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

> The `vendored-openssl` feature belongs to `hilo-cli`/`hilo_backends`
> (restricted-host builds); it does NOT exist on `hilo_ffi` —
> `cargo build -p hilo_ffi --features vendored-openssl` fails
> (dogfood run 16).

The build script (`build.rs`) auto-generates Rust scaffolding from `hilo.udl`.

### Native library filename and the rename step

The build produces the cdylib artifact `target/debug/libhilo_ffi.so`
(release: `target/release/libhilo_ffi.so`). The generated Python bindings
(`hilo.py`) do **not** load that name: UniFFI names the library after the
UDL namespace (`hilo`), so they dlopen `libuniffi_hilo.so` from the same
directory as `hilo.py`. Copy and rename the built library next to the
generated Python module before importing it:

```bash
cp target/debug/libhilo_ffi.so /tmp/hilo-bindings/python/libuniffi_hilo.so
```

Kotlin and Swift bindings have their own platform embedding steps. The
**Go bindings DO consume this `.so` directly**: the generated cgo code
`#include`s `hilo.h` and links `libhilo_ffi.so` (verified run 16,
2026-09-25: `ldd <consumer>` shows the link). A Go consumer needs the
cgo environment at build time and a way to find the library at runtime:

```bash
# after generating into /tmp/hilo-bindings and building the cdylib:
CGO_CFLAGS="-I/tmp/hilo-bindings/go/hilo" \
CGO_LDFLAGS="-L target/release -lhilo_ffi -Wl,-rpath,$PWD/target/release" \
go build ./...
# (or drop the -Wl,-rpath flag and export LD_LIBRARY_PATH=target/release at runtime)
```

The Python loader looks
for `libuniffi_hilo.dylib` on macOS and `uniffi_hilo.dll` on Windows
(same copy/rename step, next to `hilo.py`).

Smoke-check a Go build as part of the verification block:

```bash
test -f /tmp/hilo-bindings/python/hilo.py
test -n "$(find /tmp/hilo-bindings/go -name '*.go' -print -quit)"
test -f /tmp/hilo-bindings/go/hilo/hilo.h
cd <go-consumer-module> && CGO_CFLAGS="-I/tmp/hilo-bindings/go/hilo" \
CGO_LDFLAGS="-L <repo>/target/release -lhilo_ffi" go build ./...
```
