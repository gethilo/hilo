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
# cargo is resolved without relying on an interactive PATH: PATH first, then
# ~/.cargo/bin, then $CARGO_HOME/bin, then rustup toolchains; if none exist
# the script prints a diagnostic and exits 127. The build log is written to
# <repo>/.hilo-build-last.XXXXXX.log (deleted on success); set
# HILO_BUILD_LOG_DIR to keep it in an explicit scratch directory.
set -u

# Resolve the cargo toolchain BEFORE using it: runtimes like systemd-run give
# a minimal PATH without ~/.cargo/bin, where a bare `cargo` is "command not
# found" (127). Resolution order (RELEASE-019):
#   1. cargo already on PATH
#   2. the default rustup proxy at $HOME/.cargo/bin/cargo
#   3. a relocated cargo home at $CARGO_HOME/bin/cargo
#   4. rustup-managed toolchains under ~/.rustup (or $CARGO_HOME/toolchains)
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"

resolve_cargo() {
    if command -v cargo >/dev/null 2>&1; then
        command -v cargo
        return 0
    fi
    if [ -x "$HOME/.cargo/bin/cargo" ]; then
        echo "$HOME/.cargo/bin/cargo"
        return 0
    fi
    if [ -n "${CARGO_HOME:-}" ] && [ -x "$CARGO_HOME/bin/cargo" ]; then
        echo "$CARGO_HOME/bin/cargo"
        return 0
    fi
    local candidate
    for candidate in "$HOME/.rustup/toolchains/"*/bin/cargo \
                     "${CARGO_HOME:-"$REPO_ROOT/.no-cargo-home"}/toolchains/"*/bin/cargo; do
        if [ -x "$candidate" ]; then
            echo "$candidate"
            return 0
        fi
    done
    return 1
}

if ! CARGO_BIN="$(resolve_cargo)"; then
    {
        echo "build-hilo: cargo not found on PATH or ~/.cargo/bin/cargo (set PATH to include ~/.cargo/bin or install rustup)"
        echo "build-hilo: probed: command -v cargo; \$HOME/.cargo/bin/cargo; ${CARGO_HOME:-(CARGO_HOME unset)}/bin/cargo; toolchains under \${HOME}/.rustup/toolchains/"
    } >&2
    exit 127
fi
echo "build-hilo: cargo resolved: $CARGO_BIN"

# rustc must be reachable beside cargo (cargo -vV invokes it): when the
# resolved toolchain was found via ~/.cargo/bin or a toolchain dir rather
# than PATH, prepend that dir to PATH so the rustup rustc proxy is visible.
RUSTC_DIR="$(dirname "$CARGO_BIN")"
case ":$PATH:" in
    *":$RUSTC_DIR:"*) ;;
    *) export PATH="$RUSTC_DIR:$PATH" ;;
esac

# Build log: keep it with the workspace instead of generic /tmp so the
# evidence is re-findable next to the tree (RELEASE-019). HILO_BUILD_LOG_DIR
# names an explicit scratch directory; on failure to use it, fall back to
# ${TMPDIR:-/tmp} with a loud note. The log is deleted on a clean build.
LOG_DIR="${HILO_BUILD_LOG_DIR:-$REPO_ROOT}"
if [ ! -d "$LOG_DIR" ] && ! mkdir -p "$LOG_DIR" 2>/dev/null; then
    echo "build-hilo: build-log dir '$LOG_DIR' unusable; falling back to ${TMPDIR:-/tmp}" >&2
    LOG_DIR="${TMPDIR:-/tmp}"
fi

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
LOG="$(mktemp "$LOG_DIR/.hilo-build-last.XXXXXX.log")"
"$CARGO_BIN" build "${PROFILE_ARGS[@]:+${PROFILE_ARGS[@]}}" "$@" > "$LOG" 2>&1
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
