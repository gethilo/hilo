#!/usr/bin/env python3
"""DF-WARPFS-142 — tests for scripts/build-hilo.sh (OOM diagnosis wrapper).

The wrapper must leave cargo's exit codes untouched while naming the likely
cause when a build dies of SIGABRT/SIGKILL/SIGSEGV or prints cc-rs's
"command did not execute successfully (status code signal: 6 (SIGABRT))"
shape. Real cargo is never invoked: every test puts a stub `cargo` first on
PATH that exits with a scripted code and/or prints scripted output.

Run:  python3 scripts/tests/test_build_hilo_wrapper.py
"""
import os
import stat
import subprocess
import tempfile
import textwrap
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
WRAPPER = REPO / "scripts" / "build-hilo.sh"

OOM_HINT = (
    "Build failed: likely out-of-memory (OOM). Try reducing parallelism "
    "with CARGO_BUILD_JOBS=1 or use a machine with more RAM."
)


class BuildHiloWrapperTest(unittest.TestCase):
    def run_wrapper(self, stub_body: str, args=()):
        """Run the wrapper with a stub `cargo` whose body is stub_body.

        Returns (returncode, combined_output).
        """
        with tempfile.TemporaryDirectory() as td:
            bindir = Path(td) / "bin"
            bindir.mkdir()
            stub = bindir / "cargo"
            stub.write_text("#!/bin/sh\n" + textwrap.dedent(stub_body) + "\n")
            stub.chmod(stub.stat().st_mode | stat.S_IEXEC)
            env = dict(os.environ)
            env["PATH"] = f"{bindir}:{env.get('PATH', '')}"
            proc = subprocess.run(
                [str(WRAPPER), *args],
                capture_output=True,
                text=True,
                env=env,
                timeout=60,
            )
            return proc.returncode, proc.stdout + proc.stderr

    def test_sigabrt_exit_code_prints_oom_hint(self):
        # gcc/cc-rs OOM abort surfaces as exit 134 (128+SIGABRT).
        rc, out = self.run_wrapper('exit 134\n')
        self.assertEqual(134, rc)
        self.assertIn(OOM_HINT, out)

    def test_sigkill_exit_code_prints_oom_hint(self):
        # OOM-killer death surfaces as exit 137 (128+SIGKILL).
        rc, out = self.run_wrapper('exit 137\n')
        self.assertEqual(137, rc)
        self.assertIn(OOM_HINT, out)

    def test_sigsegv_exit_code_prints_oom_hint(self):
        rc, out = self.run_wrapper('exit 139\n')
        self.assertEqual(139, rc)
        self.assertIn(OOM_HINT, out)

    def test_ccrs_sigabrt_text_exit_101_prints_oom_hint(self):
        # Cargo traps the build-script failure and exits 101; the signal is
        # only visible in cc-rs's message (the live DF-WARPFS-142 shape).
        body = """
            echo 'error occurred in cc-rs: command did not execute
            successfully (status code signal: 6 (SIGABRT) (core dumped))'
            exit 101
        """
        rc, out = self.run_wrapper(body)
        self.assertEqual(101, rc)
        self.assertIn(OOM_HINT, out)

    def test_ccrs_sigkill_text_exit_101_prints_oom_hint(self):
        body = """
            echo 'status code signal: 9 (SIGKILL)'
            exit 101
        """
        rc, out = self.run_wrapper(body)
        self.assertEqual(101, rc)
        self.assertIn(OOM_HINT, out)

    def test_oom_stdlib_shapes_also_classified(self):
        for line in (
            "g++: fatal error: Cannot allocate memory",
            "rustc: out of memory",
            "terminate called after throwing an instance of 'std::bad_alloc'",
        ):
            body = f"echo '{line}'\nexit 101\n"
            rc, out = self.run_wrapper(body)
            self.assertEqual(101, rc)
            self.assertIn(OOM_HINT, out, msg=f"line not classified: {line}")

    def test_plain_compile_error_gets_no_oom_hint(self):
        # A genuine Rust compile error must NOT be mislabelled as OOM.
        body = """
            echo 'error[E0308]: mismatched types'
            exit 101
        """
        rc, out = self.run_wrapper(body)
        self.assertEqual(101, rc)
        self.assertNotIn("out-of-memory", out)

    def test_success_is_silent_and_exits_zero(self):
        rc, out = self.run_wrapper('exit 0\n')
        self.assertEqual(0, rc)
        self.assertNotIn("OOM", out)

    def test_debug_flag_builds_without_release(self):
        # --debug must be consumed by the wrapper, not forwarded to cargo.
        marker = Path(self.enterContext(tempfile.TemporaryDirectory())) / "seen"
        body = (
            f'for a in "$@"; do case "$a" in --release) echo FORWARDED_RELEASE ;; esac; done\n'
            f'echo "$@" > {marker}\n'
            "exit 0\n"
        )
        rc, out = self.run_wrapper(body, args=["--debug", "-p", "hilo-cli"])
        self.assertEqual(0, rc)
        self.assertNotIn("FORWARDED_RELEASE", out)
        self.assertIn("-p hilo-cli", marker.read_text())

    def test_extra_args_forwarded(self):
        marker = Path(self.enterContext(tempfile.TemporaryDirectory())) / "seen"
        body = f'echo "$@" > {marker}\nexit 0\n'
        rc, out = self.run_wrapper(body, args=["-p", "hilo-cli"])
        self.assertEqual(0, rc)
        self.assertIn("-p hilo-cli", marker.read_text())

    def test_wrapper_is_executable(self):
        mode = WRAPPER.stat().st_mode
        self.assertTrue(mode & stat.S_IXUSR, "scripts/build-hilo.sh must be executable")


if __name__ == "__main__":
    unittest.main(verbosity=2)
