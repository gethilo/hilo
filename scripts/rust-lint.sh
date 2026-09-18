#!/usr/bin/env bash
# Rust lint gate for the Hilo workspace — the enforcing command wired into
# .gitreins/config.yaml (guards.test_command, INT-GITREINS-002).
#
# Why the Rust gate lives here instead of the native static_analysis/lsp paths:
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
#   4. INT-GITREINS-003 — the native LSP leg has the same fail-open shape.
#      engine/lsp.py run_lsp_check() returns [] both when the tool is not on
#      PATH ("LSP tool '<t>' not found on PATH - skipping") and when it fails
#      to start ("LSP tool '<t>' failed to initialize"); guard_manager.py
#      _check_lsp() also swallows every exception (logger.warning + continue,
#      ~line 1270). An empty diagnostic list then becomes the output line
#      "  <tool> — clean" and passed=True — so a rustup shim whose component
#      was never installed ("error: Unknown binary 'rust-analyzer' in official
#      toolchain ...") reported a clean PASS for a leg that did no work at all.
#      guards.lsp is therefore disabled in .gitreins/config.yaml, and the tool
#      is proven below, where the seam is fail-closed.
#
# The enforcement seam is the tests guard: guard_manager.py
# _run_test_command() executes test_command with shell=True, fails the guard
# on a nonzero exit AND on timeout (fail-closed), and passes tool output
# through verbatim.
#
# Exit codes, so a blocked commit says WHICH class of failure it is:
#   0  every leg green
#   1  clippy findings at deny level (-D warnings)
#   2  toolchain components missing (cargo / clippy)
#   3  infrastructure: killed by the internal budget (cold tree / slow host)
#   4  rust-analyzer unusable: missing, not executable, or failing to run
#   5  rust-analyzer killed by its own budget (hung binary)
#
# Scope limit of the rust-analyzer leg: it proves the RESOLVED binary runs and
# reports a version — the class the native leg hid (missing component, rustup
# shim with no component installed, broken/non-executable tool). It does not
# drive a full LSP session: clippy --workspace --all-targets below type-checks
# every target, so a second analysis pass would add minutes to each commit
# without adding enforcement.
set -euo pipefail

# Portability: never hardcode a user's home. The scheduler sandbox can also
# redirect HOME, which would break rustup, so resolve the real account home.
ACCOUNT_HOME="$(getent passwd "$(id -u)" | cut -d: -f6)"

# Resolve rust-analyzer from the CALLER's PATH before $CARGO_HOME/bin is
# prepended below: a stub directory prepended to PATH must win, which is how
# tests/test_gitreins_rust_guard.py drives the fail-closed path with a fake
# binary. (Captured, not acted on yet — the toolchain env comes first.)
RA_FROM_CALLER_PATH="$(command -v rust-analyzer 2>/dev/null || true)"

export RUSTUP_HOME="${RUSTUP_HOME:-${ACCOUNT_HOME:-$HOME}/.rustup}"
export CARGO_HOME="${CARGO_HOME:-${ACCOUNT_HOME:-$HOME}/.cargo}"
export PATH="$CARGO_HOME/bin:$PATH"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"

# Internal budget stays below guards.test_timeout so cargo's own diagnostics,
# not the guard's generic timeout note, reach the reader. A kill here is
# exit 124 -> guard FAIL (fail-closed, never a silent pass).
TIMEOUT="${RUST_LINT_TIMEOUT:-240}"
# The rust-analyzer proof is a single --version spawn: bounded tightly so a
# hung binary blocks a commit loudly and briefly instead of burning the guard's
# whole budget. Kill => exit 5 (fail-closed).
RA_TIMEOUT="${RUST_ANALYZER_TIMEOUT:-60}"

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# Fail loud when the toolchain is absent: "tool not found -> clean" is the
# false-green class this gate removes.
command -v cargo >/dev/null 2>&1 || { echo "rust-lint: cargo not found on PATH" >&2; exit 2; }

fail_rust_analyzer() {
    # $1 = reason detail (one line). Fail closed, name the tool, name the
    # remedy. The LAST line printed is the remedy, because the guard's summary
    # only echoes the final output line of a failing leg — so the reason and
    # the remedy both have to be readable from `gitreins guard` alone.
    # Never print a "clean" line for a tool that did not run.
    echo "rust-lint: rust-analyzer unusable — $1" >&2
    if [ -n "${2:-}" ]; then
        echo "rust-lint: rust-analyzer reported: $2" >&2
    fi
    echo "rust-lint: remedy: rustup component add rust-analyzer" >&2
    exit "$3"
}

# ── Fail-closed rust-analyzer leg (INT-GITREINS-003) ────────────────────────
RA_BIN=""
RA_NOT_EXECUTABLE=""
for candidate in "$RA_FROM_CALLER_PATH" "${CARGO_HOME}/bin/rust-analyzer"; do
    [ -n "$candidate" ] || continue
    if [ -f "$candidate" ] && [ -x "$candidate" ]; then
        RA_BIN="$candidate"
        break
    fi
    if [ -e "$candidate" ]; then
        RA_NOT_EXECUTABLE="${RA_NOT_EXECUTABLE:+$RA_NOT_EXECUTABLE; }$candidate"
    fi
done

if [ -z "$RA_BIN" ]; then
    if [ -n "$RA_NOT_EXECUTABLE" ]; then
        fail_rust_analyzer "present but not an executable file: $RA_NOT_EXECUTABLE" "" 4
    fi
    fail_rust_analyzer "not found on PATH or at ${CARGO_HOME}/bin/rust-analyzer" "" 4
fi

# A present binary is not a working one: the rustup shim exists on PATH even
# when the component is absent and then exits non-zero ("Unknown binary"). The
# version probe is the proof that the tool actually ran.
RA_RC=0
RA_VERSION="$(timeout --signal=KILL "$RA_TIMEOUT" "$RA_BIN" --version 2>&1)" || RA_RC=$?
# First line only, without a pipe: `printf | head` under `set -o pipefail` can
# trip SIGPIPE on a large version banner and abort the script before it can
# report the real failure class.
RA_FIRST_LINE="${RA_VERSION%%$'\n'*}"
RA_FIRST_LINE="${RA_FIRST_LINE%$'\r'}"

if [ "$RA_RC" -eq 124 ] || [ "$RA_RC" -eq 137 ]; then
    fail_rust_analyzer "$RA_BIN hung and was killed by the ${RA_TIMEOUT}s budget" "$RA_FIRST_LINE" 5
fi
if [ "$RA_RC" -ne 0 ]; then
    fail_rust_analyzer "$RA_BIN --version exited $RA_RC" "$RA_FIRST_LINE" 4
fi
case "$RA_FIRST_LINE" in
    rust-analyzer\ *) ;;
    *)
        fail_rust_analyzer "$RA_BIN --version printed no rust-analyzer version" "$RA_FIRST_LINE" 4
        ;;
esac

# Success is printed with the resolved path AND the reported version: the
# guard's output then proves the tool ran instead of asserting it.
echo "rust-lint: rust-analyzer ran: $RA_BIN — $RA_FIRST_LINE"

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
