# Hilo release procedure

This procedure is the release gate for the `v0.4.1` line. Run it from the
workspace root on the exact commit that will be tagged. Do not tag or publish
until every gate below passes.

## Dependency advisory gate

The lockfile scan and the runtime-reachable scan answer different questions.
`cargo audit --file Cargo.lock --json` reports every advisory package in the
lockfile, including packages used only by a target, optional feature, build
script, or another workspace member. A release must separately intersect the
advisory package names with the normal, workspace dependency tree for the
release target. The gate below keeps lock-only findings visible but blocks when
any advisory package is runtime-reachable.

The gate is implemented as `scripts/audit-gate.sh` and runs as a blocking step
in `.github/workflows/ci.yml`, so a runtime-reachable advisory fails CI before a
tag can be cut (the gate is enforced, not just documented). Run it locally from
the repository root:

```bash
bash scripts/audit-gate.sh
```

The script exits `0` only when the intersection is empty. Exit `1` means an
advisory package is reachable through normal workspace edges (blocked). Exit
`2` means the gate could not be evaluated (cargo-audit missing, audit failed to
run, `cargo tree` failed) and is also blocking. It prints the lockfile advisory
packages it tolerated as build-only or feature-disabled so every one stays
visible in the release evidence.

A non-zero `cargo audit` status of `1` is expected when the lockfile still has
advisories that are not reachable through normal runtime dependencies. It is
not, by itself, a release pass: the intersection must be empty, and every
lock-only finding must be reviewed. Any other audit failure, a failed
`cargo tree`, or a non-empty intersection blocks the release.

For the full release candidate, also run:

```bash
cargo fmt --check
cargo build --workspace
cargo clippy --workspace -- -D warnings
cargo test --workspace
cargo tree --target x86_64-unknown-linux-gnu --edges normal -i wasmtime
cargo audit --file Cargo.lock --json
```

The `wasmtime` reverse tree must show the version selected through `extism`
and the version must satisfy every published RustSec patched range applicable
to that release. Record the exact command output and target triple with the
release evidence. The tag and publication step is intentionally separate from
this dependency gate and must use the exact CI-verified commit.

## Tag and release assets

After all checks pass, create an annotated tag named `vX.Y.Z`. The release
bundle is produced with `scripts/build-hilo.sh -p hilo-cli` and consists of
the versioned tarball and its matching `.sha256` checksum. Tagging, uploading,
and publishing are separate releng actions; this procedure does not perform
them.
