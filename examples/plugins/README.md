# Example plugins

A minimal, legitimate `.wasm` module to load with `hilo plugin load` — so the
CLI has something real to demonstrate instead of a hand-typed file.

## Files

| File | Purpose |
|------|---------|
| `minimal.wasm` | The example plugin — a real 36-byte wasm module (valid `\0asm` magic + version 1 header, one exported `run` function). Checked in, no toolchain required. |
| `minimal.wat` | The WebAssembly text source `minimal.wasm` is built from. |

## Build note

`minimal.wasm` is committed, so nothing needs to be built to use it. To
regenerate it after editing the source (requires
[wabt](https://github.com/WebAssembly/wabt)):

```bash
wat2wasm minimal.wat -o minimal.wasm
```

## Usage

```bash
# Load it (validates the wasm header, persists the file to .vfs/plugins/)
hilo plugin load examples/plugins/minimal.wasm

# Confirm it is discoverable
hilo plugin list
```

`minimal.wasm` declares no hooks, so the CLI honestly reports `0 hooks` /
`0 edge types` and `hilo plugin list` shows its version as `?` — hilo does
not parse a plugin manifest yet (DF-WARPFS-59: plugin execution is not
implemented). See [`docs/hilo-plugins.md`](../docs/hilo-plugins.md) for the
full CLI reference.
