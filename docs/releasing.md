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

Copy-paste this command from the repository root. The target triple is
intentionally explicit so the result is reproducible:

```bash
set -u
TARGET_TRIPLE=x86_64-unknown-linux-gnu
AUDIT_JSON=$(mktemp)
TREE_NAMES=$(mktemp)
AUDIT_NAMES=$(mktemp)
INTERSECTION=$(mktemp)
trap 'rm -f "$AUDIT_JSON" "$TREE_NAMES" "$AUDIT_NAMES" "$INTERSECTION"' EXIT

set +e
cargo audit --file Cargo.lock --json >"$AUDIT_JSON"
AUDIT_RC=$?
set -e
if [ "$AUDIT_RC" -gt 1 ]; then
  echo "cargo audit failed to run (exit $AUDIT_RC)" >&2
  exit "$AUDIT_RC"
fi

cargo tree \
  --target "$TARGET_TRIPLE" \
  --workspace \
  --edges normal \
  --prefix none \
  --format '{p}' \
  | sed -E 's/ v[^ ]+.*$//' \
  | LC_ALL=C sort -u >"$TREE_NAMES"

jq -r '.vulnerabilities.list[]?.package.name' "$AUDIT_JSON" \
  | LC_ALL=C sort -u >"$AUDIT_NAMES"
LC_ALL=C comm -12 "$AUDIT_NAMES" "$TREE_NAMES" >"$INTERSECTION"

printf 'target: %s\n' "$TARGET_TRIPLE"
printf 'cargo audit exit: %s (non-zero means lockfile findings were reported)\n' "$AUDIT_RC"
printf 'lockfile advisory packages: %s\n' "$(wc -l <"$AUDIT_NAMES")"
printf 'runtime-reachable advisory intersection:\n'
cat "$INTERSECTION"

if [ -s "$INTERSECTION" ]; then
  echo 'FAIL: advisory packages are reachable through normal workspace dependencies' >&2
  exit 1
fi
printf 'PASS: runtime-reachable advisory intersection is empty\n'
```

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
