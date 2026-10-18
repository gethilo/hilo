#!/usr/bin/env bash
# Build wrapper that names OOM failures instead of a bare signal (DF-WARPFS-142).
#
# Why a wrapper and not a build.rs: `cargo build` runs every build script
# in-process, and when libduckdb-sys's cc-rs C++ compile dies under memory
# pressure, cargo aborts the whole build before any downstream build script
# (hilo-graph's own, for example) ever runs. The only seam that sees the
# abort is a process wrapped around cargo itself, so this script runs cargo,
# classifies signal deaths and signal-shaped cc-rs errors, and prints the
# low-memory remediation from the README.
#
# Usage (from the repository root):
#   scripts/build-hilo.sh                 # cargo build --release
#   scripts/build-hilo.sh --debug         # cargo build
#   scripts/build-hilo.sh -p hilo-cli     # extra args pass through to cargo
#
# Exit codes are cargo's, unchanged: a normal compile error is still 101,
# a signal death is still 128+N — the wrapper only adds the diagnosis.
set -u

PROFILE_ARGS=(--release)
if [ "${1:-}" = "--debug" ]; then
    PROFILE_ARGS=()
    shift
fi

if [ "${#PROFILE_ARGS[@]}" -gt 0 ]; then
    echo "build-hilo: cargo build ${PROFILE_ARGS[*]} $*"
else
    echo "build-hilo: cargo build $*"
fi

# Capture to a file FIRST (synchronously — a `>(tee ...)` process
# substitution races the classification below, because bash does not wait
# for it after the command exits), then echo the log through so the console
# still sees the build output. Capturing matters because cargo exits 101 for
# a build-script failure even when the underlying cc-rs compile died of
# SIGABRT — the signal is only visible in the captured output.
LOG="$(mktemp "${TMPDIR:-/tmp}/hilo-build-last.XXXXXX.log")"
cargo build "${PROFILE_ARGS[@]:+${PROFILE_ARGS[@]}}" "$@" > "$LOG" 2>&1
rc=$?
cat "$LOG" >&2

if [ "$rc" -eq 0 ]; then
    rm -f "$LOG"
    exit 0
fi

oom=0
# Direct signal deaths of cargo/its children, as reported by "$?": SIGABRT
# (gcc's out-of-memory abort path), SIGKILL (OOM killer), SIGSEGV (stack or
# heap exhaustion).
case "$rc" in
    134 | 137 | 139) oom=1 ;;
esac

# cc-rs shape: the compiler dies of a signal but cargo traps it and exits
# 101 — classify from the captured output instead of the exit code.
if [ "$oom" -eq 0 ] && grep -qE 'signal: (6|9|11) \((SIGABRT|SIGKILL|SIGSEGV)\)|out of memory|Cannot allocate memory|std::bad_alloc' "$LOG"; then
    oom=1
fi

if [ "$oom" -eq 1 ]; then
    {
        echo "====================================================================="
        echo "Build failed: likely out-of-memory (OOM). Try reducing parallelism with CARGO_BUILD_JOBS=1 or use a machine with more RAM."
        echo "====================================================================="
        echo "hint: the bundled DuckDB C++ build (libduckdb-sys) needs several GB"
        echo "      per translation unit; see README.md \"Low-memory builders\" for"
        echo "      the full recipe. Full build log kept at: $LOG"
    } >&2
else
    echo "build-hilo: failure was not memory-shaped (exit $rc); full log: $LOG" >&2
fi

exit "$rc"
