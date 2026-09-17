#!/usr/bin/env bash
# Rust lint gate for the Hilo workspace — the enforcing command wired into
# .gitreins/config.yaml (guards.test_command, INT-GITREINS-002).
#
# Why the Rust gate lives here instead of the native static_analysis path:
#   1. engine/static_analysis.py run_static_check() maps every failure mode
#      (tool missing, timeout, exception) to an empty diagnostics list, and
#      engine/guard_manager.py _check_static_analysis() then prints
#      "<tool> — clean" and passes. That is how the historical cppcheck
#      300s-timeout produced a green guard with zero diagnostics.
#   2. The native clippy backend runs cargo clippy without a deny flag and
#      only fails on severity == "error", so clippy warnings pass. Warnings
#      cannot be denied through static_analysis_tools.
#   3. _check_static_analysis() classifies this repo as C++ because it ships a
#      Makefile (_is_cpp checks for one) and routes to cppcheck regardless of
#      static_analysis_tools, so Rust would never be analyzed natively.
#
# The enforcement seam is the tests guard: guard_manager.py
# _run_test_command() executes test_command with shell=True, fails the guard
# on a nonzero exit AND on timeout (fail-closed), and passes tool output
# through verbatim.
set -euo pipefail

# Portability: never hardcode a user's home. The scheduler sandbox can also
# redirect HOME, which would break rustup, so resolve the real account home.
ACCOUNT_HOME="$(getent passwd "$(id -u)" | cut -d: -f6)"
export RUSTUP_HOME="${RUSTUP_HOME:-${ACCOUNT_HOME:-$HOME}/.rustup}"
export CARGO_HOME="${CARGO_HOME:-${ACCOUNT_HOME:-$HOME}/.cargo}"
export PATH="$CARGO_HOME/bin:$PATH"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"

# Internal budget stays below guards.test_timeout so cargo's own diagnostics,
# not the guard's generic timeout note, reach the reader. A kill here is
# exit 124 -> guard FAIL (fail-closed, never a silent pass).
TIMEOUT="${RUST_LINT_TIMEOUT:-240}"

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# Fail loud when the toolchain is absent: "tool not found -> clean" is the
# false-green class this gate removes.
command -v cargo >/dev/null 2>&1 || { echo "rust-lint: cargo not found on PATH" >&2; exit 2; }
cargo clippy --version >/dev/null 2>&1 || {
    echo "rust-lint: clippy component missing (rustup component add clippy)" >&2
    exit 2
}

run_clippy() {
    # Argument order matters: cargo takes its options AFTER the subcommand.
    timeout --signal=KILL "$TIMEOUT" cargo clippy "$@" --all-targets -- -D warnings
}

# Primary gate: whole workspace, warnings denied.
if run_clippy --workspace; then
    echo "rust-lint: clippy --workspace --all-targets -- -D warnings: clean"
else
    status=$?
    # Distinguish real findings from local infrastructure (cold tree / slow
    # workspace) so a blocked commit is explained honestly.
    if [ "$status" -eq 124 ] || [ "$status" -eq 137 ]; then
        echo "rust-lint: clippy --workspace killed by the ${TIMEOUT}s budget (cold tree?); run heavy lint on the build host" >&2
        exit 3
    fi
    echo "rust-lint: workspace clippy reported findings; classifying with the scoped pair" >&2
    if timeout --signal=KILL "$TIMEOUT" cargo clippy -p hilo_graph -p hilo-cli --all-targets -- -D warnings; then
        echo "rust-lint: scoped clippy green but the workspace run failed — blocking (infra suspected)" >&2
        exit 3
    fi
    echo "rust-lint: clippy reported findings at deny level -D warnings" >&2
    exit 1
fi

# Existing coverage is preserved: the graph unit suite still runs, fail-closed.
echo "rust-lint: running cargo test -p hilo_graph --lib"
exec cargo test -p hilo_graph --lib
