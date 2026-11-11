#!/usr/bin/env bash
# Blocking release gate for runtime-reachable RustSec advisories.
#
# `cargo audit` reports every advisory whose package is pinned in Cargo.lock,
# including packages that are never linked into the shipped binaries:
#   * build-dependencies (they run while compiling, e.g. libduckdb-sys' `zip`)
#   * optional dependencies whose feature is never enabled (e.g. rkyv via
#     rust_decimal's `rkyv` feature)
# A release is blocked only when an advisory package is reachable through
# NORMAL (runtime) workspace dependency edges, which is the set that actually
# ships inside `hilo`.
#
# Exit codes: 0 = gate passed, 1 = runtime-reachable advisory (blocked),
#             2 = the gate could not be evaluated (treat as blocked).
set -uo pipefail

cd "$(dirname "$0")/.." || exit 2
TARGET="${AUDIT_GATE_TARGET:-x86_64-unknown-linux-gnu}"
WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT

command -v cargo-audit >/dev/null 2>&1 || {
  echo "audit-gate: cargo-audit is not installed (cargo install cargo-audit --locked)" >&2
  exit 2
}
command -v jq >/dev/null 2>&1 || { echo "audit-gate: jq is required" >&2; exit 2; }

# cargo audit exits 1 when it finds advisories; that is the point of the scan.
cargo audit --file Cargo.lock --json >"$WORKDIR/audit.json" 2>"$WORKDIR/audit.err"
audit_rc=$?
if [ ! -s "$WORKDIR/audit.json" ]; then
  echo "audit-gate: cargo audit produced no JSON (rc=$audit_rc)" >&2
  cat "$WORKDIR/audit.err" >&2
  exit 2
fi

jq -r '.vulnerabilities.list[]?.package.name' "$WORKDIR/audit.json" \
  | LC_ALL=C sort -u >"$WORKDIR/advisory.txt"

cargo tree --target "$TARGET" --workspace --edges normal --prefix none --format '{p}' \
  | sed -E 's/ v[^ ]+.*$//' | LC_ALL=C sort -u >"$WORKDIR/runtime.txt"

comm -12 "$WORKDIR/advisory.txt" "$WORKDIR/runtime.txt" >"$WORKDIR/intersection.txt"

lockfile_count=$(wc -l <"$WORKDIR/advisory.txt" | tr -d ' ')
if [ -s "$WORKDIR/intersection.txt" ]; then
  echo "audit-gate: FAIL - runtime-reachable advisory package(s):" >&2
  sed 's/^/  - /' "$WORKDIR/intersection.txt" >&2
  echo "audit-gate: see docs/releasing.md for the remediation policy" >&2
  exit 1
fi

echo "audit-gate: PASS - no advisory package is reachable through normal workspace edges"
echo "           lockfile advisory package(s) ($lockfile_count) are build-only or feature-disabled:"
sed 's/^/             /' "$WORKDIR/advisory.txt"
