"""Live GitReins configuration regression; run with python3 -m unittest discover -s tests.

Requires gitreins, git and Rust/Clippy on PATH. Uses an isolated dependency-free
Rust crate, never mutating product sources or invoking the LLM evaluator.
"""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class RustGuardTest(unittest.TestCase):
    def _fixture(self, root):
        """Isolated, dependency-free Rust fixture (never touches product sources).

        Copies the REAL .gitreins/config.yaml + scripts/rust-lint.sh so the
        checks below exercise the shipped gate, not a paraphrase of it.
        """
        repo = Path(__file__).resolve().parents[1]
        (root / "src").mkdir()
        (root / ".gitreins").mkdir()
        (root / "scripts").mkdir()
        shutil.copyfile(repo / ".gitreins/config.yaml", root / ".gitreins/config.yaml")
        shutil.copyfile(repo / "scripts/rust-lint.sh", root / "scripts/rust-lint.sh")
        (root / "Cargo.toml").write_text(
            '[package]\nname = "hilo_graph"\nversion = "0.1.0"\nedition = "2021"\n'
        )
        # The Makefile triggered the historical C++ misclassification.
        (root / "Makefile").write_text("all:\n\t@true\n")
        (root / "src/lib.rs").write_text("pub fn answer() -> bool { true }\n")

    def _env_with_rust_analyzer_on_path(self, stub_dir):
        """Caller env with *stub_dir* prepended to PATH.

        The gate resolves rust-analyzer from the CALLER PATH first, so a stub
        dir here must win over the real ~/.cargo/bin tool.
        """
        env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
        env.pop("CARGO_TARGET_DIR", None)
        env["PATH"] = f"{stub_dir}{os.pathsep}{env.get('PATH', '')}"
        return env

    def test_clippy_warning_blocks_real_guard(self):
        repo = Path(__file__).resolve().parents[1]
        for tool in ("gitreins", "cargo", "git"):
            self.assertIsNotNone(shutil.which(tool), f"required tool missing: {tool}")
        env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
        env.pop("CARGO_TARGET_DIR", None)
        with tempfile.TemporaryDirectory(prefix="hilo-guard-test-") as tmp:
            root = Path(tmp)
            (root / "src").mkdir()
            (root / ".gitreins").mkdir()
            (root / "scripts").mkdir()
            shutil.copyfile(repo / ".gitreins/config.yaml", root / ".gitreins/config.yaml")
            shutil.copyfile(repo / "scripts/rust-lint.sh", root / "scripts/rust-lint.sh")
            (root / "Cargo.toml").write_text(
                '[package]\nname = "hilo_graph"\nversion = "0.1.0"\nedition = "2021"\n'
            )
            # The Makefile triggered the historical C++ misclassification.
            (root / "Makefile").write_text("all:\n\t@true\n")
            source = root / "src/lib.rs"
            source.write_text("pub fn answer() -> bool { true }\n")

            def run(*args):
                return subprocess.run(args, cwd=root, env=env, text=True,
                                      stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                      timeout=360)

            self.assertEqual(run("git", "init", "-q").returncode, 0)
            self.assertEqual(run("git", "add", ".").returncode, 0)
            clean = run("gitreins", "guard")
            self.assertEqual(clean.returncode, 0, clean.stdout)
            self.assertIn("Tier 1 Guards: PASS", clean.stdout)
            self.assertNotIn("cppcheck", clean.stdout)
            # Valid Rust with a Clippy warning, not a compiler/type error.
            source.write_text("pub fn answer() -> bool { true == true }\n")
            self.assertEqual(run("cargo", "check", "--quiet").returncode, 0)
            self.assertEqual(run("git", "add", "src/lib.rs").returncode, 0)
            bad = run("gitreins", "guard")
            self.assertNotEqual(bad.returncode, 0, bad.stdout)
            self.assertIn("Tier 1 Guards: FAIL", bad.stdout)
            self.assertNotIn("cppcheck", bad.stdout)
            # Print bounded real evidence for reviewers, not fabricated output.
            print("clean guard exit:", clean.returncode,
                  "Clippy-violating guard exit:", bad.returncode)

    def test_broken_rust_analyzer_fails_real_guard(self):
        """NEGATIVE CONTROL (INT-GITREINS-003): an unusable rust-analyzer FAILS.

        Pre-change this scenario PASSED: engine/lsp.py run_lsp_check() returns
        [] when the tool fails to initialize (and when it is not on PATH at
        all), and guard_manager.py _check_lsp() turns that empty list into
        "  rust-analyzer — clean" with passed=True. The gate now proves the
        tool runs, so a present-but-broken binary must block the commit.
        """
        for tool in ("gitreins", "cargo", "git"):
            self.assertIsNotNone(shutil.which(tool), f"required tool missing: {tool}")
        with tempfile.TemporaryDirectory(prefix="hilo-ra-broken-") as tmp:
            root = Path(tmp)
            fixture = root / "fixture"
            fixture.mkdir()
            self._fixture(fixture)
            # A rustup shim with no component installed: present + executable,
            # exits non-zero with the real toolchain's error text.
            stub_dir = root / "stub"
            stub_dir.mkdir()
            stub = stub_dir / "rust-analyzer"
            stub.write_text(
                "#!/usr/bin/env bash\n"
                "echo \"error: Unknown binary 'rust-analyzer' in official toolchain"
                " 'stable-x86_64-unknown-linux-gnu'.\" >&2\n"
                "exit 101\n"
            )
            stub.chmod(0o755)
            env = self._env_with_rust_analyzer_on_path(stub_dir)

            def run(*args):
                return subprocess.run(args, cwd=fixture, env=env, text=True,
                                      stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                      timeout=360)

            self.assertEqual(run("git", "init", "-q").returncode, 0)
            self.assertEqual(run("git", "add", ".").returncode, 0)
            guard = run("gitreins", "guard")
            self.assertNotEqual(guard.returncode, 0, guard.stdout)
            self.assertIn("Tier 1 Guards: FAIL", guard.stdout)
            # The failing leg must be identifiable as the rust-analyzer leg,
            # and the summary must carry the remedy (the guard echoes only the
            # LAST line of a failing leg's output).
            self.assertIn("rust-analyzer", guard.stdout)
            self.assertIn("rustup component add rust-analyzer", guard.stdout)
            # The phantom green tick, and the native lsp leg, are gone.
            self.assertNotIn("rust-analyzer — clean", guard.stdout)
            self.assertNotIn("true == true", guard.stdout)
            print("broken rust-analyzer guard exit:", guard.returncode, "|",
                  [ln for ln in guard.stdout.splitlines() if "rust-lint" in ln])

    def test_working_rust_analyzer_passes_and_reports_version(self):
        """The gate must name the binary it proved AND let a healthy tree through.

        The stub is deliberate: the point of this row is that a toolchain
        component's presence must not be assumed, so the test cannot itself
        depend on one being installed. It pins the script contract — exit 0
        only after printing "rust-analyzer ran: <resolved path> — <version>".
        """
        for tool in ("gitreins", "cargo", "git"):
            self.assertIsNotNone(shutil.which(tool), f"required tool missing: {tool}")
        with tempfile.TemporaryDirectory(prefix="hilo-ra-working-") as tmp:
            root = Path(tmp)
            fixture = root / "fixture"
            fixture.mkdir()
            self._fixture(fixture)
            stub_dir = root / "stub"
            stub_dir.mkdir()
            stub = stub_dir / "rust-analyzer"
            stub.write_text(
                "#!/usr/bin/env bash\n"
                "echo 'rust-analyzer 1.98.0 (stub-for-test)'\n"
                "exit 0\n"
            )
            stub.chmod(0o755)
            env = self._env_with_rust_analyzer_on_path(stub_dir)

            def run(*args):
                return subprocess.run(args, cwd=fixture, env=env, text=True,
                                      stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                      timeout=360)

            self.assertEqual(run("git", "init", "-q").returncode, 0)
            self.assertEqual(run("git", "add", ".").returncode, 0)
            # (1) The gate's own output proves the tool ran: resolved path +
            # reported version. (gitreins guard prints only a per-leg status
            # line for a PASSING leg, so the proof is asserted here.)
            gate = run("bash", "scripts/rust-lint.sh")
            self.assertEqual(gate.returncode, 0, gate.stdout)
            self.assertIn("rust-lint: rust-analyzer ran:", gate.stdout)
            self.assertIn(str(stub), gate.stdout)
            self.assertIn("rust-analyzer 1.98.0 (stub-for-test)", gate.stdout)
            # clippy + the graph lib suite stay enforced behind it.
            self.assertIn("rust-lint: clippy --workspace", gate.stdout)
            self.assertIn("running 0 tests", gate.stdout)
            # (2) The real guard passes with the healthy tool.
            guard = run("gitreins", "guard")
            self.assertEqual(guard.returncode, 0, guard.stdout)
            self.assertIn("Tier 1 Guards: PASS", guard.stdout)
            print("working rust-analyzer gate exit:", gate.returncode,
                  "guard exit:", guard.returncode, "|",
                  [ln for ln in gate.stdout.splitlines() if "rust-lint" in ln])


if __name__ == "__main__":
    unittest.main()
