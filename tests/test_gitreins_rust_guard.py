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


if __name__ == "__main__":
    unittest.main()
