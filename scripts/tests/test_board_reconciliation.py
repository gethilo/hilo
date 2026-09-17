#!/usr/bin/env python3
"""Regression tests for the BOARD-HYGIENE-001 board reconciliation (warpfs tick 187).

These tests exercise the REAL board data and the REAL pre-change snapshot
(git commit 71b333f) rather than fixtures: the negative tests prove the
verification logic detects the pre-change defects, and the positive tests prove
the reconciled board fixes them without losing provenance.

Run:  python3 -m unittest discover -s scripts/tests -t . -v
      (or) python3 scripts/tests/test_board_reconciliation.py
"""

from __future__ import annotations

import json
import subprocess
import sys
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "scripts"))

import verify_board_reconciliation as vbr  # noqa: E402

BOARD = REPO / ".coding-hermes" / "board"
BASE = vbr.BASE_COMMIT_DEFAULT
ARTIFACT = json.loads((BOARD / vbr.ARTIFACT_NAME).read_text())


def rows_of(text: str) -> list[dict]:
    return vbr.parse_rows(text)[1]


class PrechangeSnapshot(unittest.TestCase):
    """The snapshot the acceptance criteria name must really be the broken state."""

    @classmethod
    def setUpClass(cls) -> None:
        cls.base_text = vbr.git_show(REPO, BASE, ".coding-hermes/board/tasks.jsonl")
        cls.base_rows = rows_of(cls.base_text)
        cls.new_text = (BOARD / "tasks.jsonl").read_text()
        cls.new_rows = rows_of(cls.new_text)

    def test_snapshot_has_duplicate_ids(self) -> None:
        dups = vbr.duplicate_ids(self.base_rows)
        self.assertEqual(
            set(dups),
            {"QA-WARPFS-1", "QA-WARPFS-2", "QA-WARPFS-3", "QA-WARPFS-4", "QA-WARPFS-5",
             "GAP-067", "GAP-074", "GAP-077", "GAP-079", "GAP-080"},
            "the pre-change snapshot's duplicate-id families changed; audit artifact is stale")
        self.assertEqual(dups["QA-WARPFS-1"], 7)
        self.assertEqual(dups["GAP-074"], 3)

    def test_snapshot_has_invalid_status_and_char_array_reasoning(self) -> None:
        statuses = {r.get("status") for r in self.base_rows} - vbr.STATUS_VOCAB
        self.assertIn("done", statuses, "pre-change board should carry the 'done' status")
        char_rows = [r["id"] for r in self.base_rows if vbr.is_char_array(r.get("reasoning"))]
        for tid in ("GAP-074", "GAP-077", "GAP-079", "GAP-080"):
            self.assertIn(tid, char_rows, f"{tid} was expected to carry character-array reasoning")

    def test_checker_flags_the_snapshot(self) -> None:
        """The uniqueness/vocabulary checks must be able to FAIL (not vacuously true)."""
        self.assertTrue(vbr.duplicate_ids(self.base_rows), "duplicate check cannot fail")
        self.assertNotEqual(
            {r.get("status") for r in self.base_rows} - vbr.STATUS_VOCAB, set(),
            "status vocabulary check cannot fail on the snapshot")


class ReconciledBoard(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.new_text = (BOARD / "tasks.jsonl").read_text()
        cls.new_rows = rows_of(cls.new_text)
        cls.by_id = {r["id"]: r for r in cls.new_rows}
        cls.base_rows = rows_of(vbr.git_show(REPO, BASE, ".coding-hermes/board/tasks.jsonl"))

    def test_unique_ids_valid_statuses_and_vocabularies(self) -> None:
        self.assertEqual(vbr.duplicate_ids(self.new_rows), {})
        for row in self.new_rows:
            self.assertIn(row.get("status"), vbr.STATUS_VOCAB, row["id"])
            if row.get("guard_result") is not None:
                self.assertIn(row["guard_result"], vbr.GUARD_VOCAB, row["id"])
            if row.get("ci_result") is not None:
                self.assertIn(row["ci_result"], vbr.CI_VOCAB, row["id"])

    def test_no_character_array_reasoning_and_closure_notes_survive(self) -> None:
        for row in self.new_rows:
            self.assertFalse(vbr.is_char_array(row.get("reasoning")), row["id"])
        # the appended closure note must survive as its own element (text, not chars)
        for line_no in (128, 129, 130, 131):
            entry = next(e for e in ARTIFACT["reasoning_restorations"]
                         if e["original_line"] == line_no)
            row = self.by_id[entry["id"]]
            self.assertIsInstance(row["reasoning"], list)
            self.assertEqual(row["reasoning"], entry["restored"])
            self.assertGreaterEqual(len(row["reasoning"]), 2)
            self.assertIn("CLOSED", row["reasoning"][1])
            self.assertGreater(len(row["reasoning"][1]), 100,
                               "closure note must be readable text, not single characters")

    def test_completed_findings_keep_completion_evidence(self) -> None:
        for tid in vbr.REQUIRED_COMPLETE + ("GAP-074", "GAP-073"):
            row = self.by_id[tid]
            self.assertEqual(row["status"], "complete", tid)
            self.assertTrue(row.get("completed_at"), f"{tid} lost completed_at")
            blob = json.dumps(row)
            self.assertTrue(vbr.looks_like_commit_sha(blob), f"{tid} lost its commit evidence")

    def test_gap_067_077_079_080_are_not_pending(self) -> None:
        for tid in vbr.REQUIRED_COMPLETE:
            self.assertNotEqual(self.by_id[tid]["status"], "pending")
            matches = [r for r in self.new_rows if r["id"] == tid]
            self.assertEqual(len(matches), 1, f"{tid} should have exactly one row")

    def test_distinct_qa_findings_are_individually_represented(self) -> None:
        """Every historical QA row/finding survives with its own unique id."""
        base_qa = [r for r in self.base_rows if r["id"].startswith("QA-WARPFS-")]
        new_qa = {r["id"]: r for r in self.new_rows if r["id"].startswith("QA-WARPFS-")}
        self.assertEqual(len(new_qa), len(base_qa),
                         "a QA finding disappeared from the board during reconciliation")
        # each pre-change QA row is either the canonical carrier or a re-identified row
        reidentified = {int(e["original_line"]): e for e in ARTIFACT["reidentified_rows"]}
        for n, row in enumerate(self.base_rows, 1):
            if not row["id"].startswith("QA-WARPFS-"):
                continue
            if n in reidentified:
                new_id = reidentified[n]["new_id"]
                self.assertIn(new_id, new_qa)
                self.assertEqual(new_qa[new_id]["reconciliation"]["original_id"], row["id"])
                self.assertEqual(new_qa[new_id].get("ts"), reidentified[n]["original_ts"])
                self.assertEqual(new_qa[new_id]["title"], row["title"])
            else:
                self.assertIn(row["id"], new_qa, f"canonical carrier for line {n} lost")

    def test_no_pending_finding_was_silently_closed(self) -> None:
        removed = {int(r["original_line"]) for r in ARTIFACT["removed_rows"]}
        documented = {int(c["original_line"]): c["to"]
                      for c in ARTIFACT["field_corrections"] if c["field"] == "status"}
        line_map, idx = {}, 1
        for n in range(1, len(self.base_rows) + 1):
            if n in removed:
                continue
            line_map[n] = idx
            idx += 1
        for n, base_row in enumerate(self.base_rows, 1):
            if n not in line_map or base_row.get("status") == "complete":
                continue
            new_row = self.new_rows[line_map[n] - 1]
            expected = documented.get(n, base_row["status"])
            self.assertEqual(new_row["status"], expected,
                             f"{base_row['id']} (line {n}) changed status without evidence")
        # the only flip is the documented done -> complete normalisation
        flips = [(c["original_line"], c["from"], c["to"])
                 for c in ARTIFACT["field_corrections"] if c["field"] == "status"]
        self.assertEqual(flips, [(116, "done", "complete")])


class Preservation(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.base_text = vbr.git_show(REPO, BASE, ".coding-hermes/board/tasks.jsonl")
        cls.base_raw = cls.base_text.split("\n")
        if cls.base_raw and cls.base_raw[-1] == "":
            cls.base_raw = cls.base_raw[:-1]
        cls.new_text = (BOARD / "tasks.jsonl").read_text()
        cls.new_raw = cls.new_text.split("\n")
        if cls.new_raw and cls.new_raw[-1] == "":
            cls.new_raw = cls.new_raw[:-1]

    def test_unrelated_lines_are_byte_identical(self) -> None:
        removed = {int(r["original_line"]) for r in ARTIFACT["removed_rows"]}
        changed = {int(n) for n in ARTIFACT["changed_lines"]}
        missing = [n for n, raw in enumerate(self.base_raw, 1)
                   if n not in removed and n not in changed and raw not in self.new_raw]
        self.assertEqual(missing, [], "untouched rows were modified or lost")
        # and they keep their relative order (only listed lines were dropped)
        survivors = [raw for n, raw in enumerate(self.base_raw, 1)
                     if n not in removed and n not in changed]
        positions = [self.new_raw.index(raw) for raw in survivors]
        self.assertEqual(positions, sorted(positions), "surviving rows were reordered")

    def test_events_board_and_fixtures_are_untouched(self) -> None:
        for name in ("events.jsonl", "board.jsonl", "fixtures.jsonl"):
            rel = f".coding-hermes/board/{name}"
            self.assertEqual((BOARD / name).read_text(), vbr.git_show(REPO, BASE, rel),
                             f"{name} differs from the {BASE} snapshot")

    def test_removed_rows_are_preserved_verbatim_in_the_artifact(self) -> None:
        removed_lines = {int(r["original_line"]) for r in ARTIFACT["removed_rows"]}
        self.assertEqual(removed_lines, {106, 117, 121, 123, 124, 127})
        for entry in ARTIFACT["removed_rows"]:
            original = self.base_raw[entry["original_line"] - 1]
            self.assertEqual(entry["raw"], original,
                             f"artifact did not keep line {entry['original_line']} verbatim")
            self.assertTrue(entry["reason"])

    def test_mapping_covers_every_prechange_duplicate_id(self) -> None:
        self.assertEqual(set(ARTIFACT["mapping"]),
                         set(vbr.duplicate_ids(rows_of(self.base_text))))
        for original_id, entries in ARTIFACT["mapping"].items():
            for entry in entries:
                self.assertIn(entry["canonical_id"],
                              {r["id"] for r in rows_of(self.new_text)},
                              f"{original_id} -> {entry['canonical_id']} is not on the board")

    def test_normalised_fields_keep_their_original_text(self) -> None:
        rows = rows_of(self.new_text)
        by_id = {}
        for r in rows:
            by_id.setdefault(r["id"], []).append(r)
        for c in ARTIFACT["field_corrections"]:
            if c["field"] not in ("guard_result", "ci_result"):
                continue
            row = by_id[c["id"]][0]
            note = row.get(f"{c['field']}_note") or ""
            self.assertIn(str(c["from"]), note, f"{c['id']}.{c['field']} lost its original text")
            self.assertIn(c["to"], (row[c["field"]],))

    def test_events_parse_behaviour_matches_snapshot(self) -> None:
        base_events = vbr.git_show(REPO, BASE, ".coding-hermes/board/events.jsonl")
        self.assertEqual(vbr._bad_lines((BOARD / "events.jsonl").read_text()),
                         vbr._bad_lines(base_events))


class EndToEndChecker(unittest.TestCase):
    def test_verify_script_passes_on_the_reconciled_board(self) -> None:
        out = subprocess.run(
            [sys.executable, str(REPO / "scripts" / "verify_board_reconciliation.py"), "--quiet"],
            capture_output=True, text=True, cwd=str(REPO),
        )
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
        self.assertIn("RESULT: OK", out.stdout)

    def test_verify_script_fails_on_the_snapshot(self) -> None:
        """Point the checker at the pre-change snapshot: it must FAIL.

        A throwaway git repo is seeded with the exact 71b333f board content and
        committed, so `git show <its HEAD>:...` supplies the base for the checks.
        """
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            fake = Path(tmp)
            bd = fake / ".coding-hermes" / "board"
            bd.mkdir(parents=True)
            for name in ("tasks.jsonl", "events.jsonl", "board.jsonl", "fixtures.jsonl"):
                (bd / name).write_text(vbr.git_show(REPO, BASE, f".coding-hermes/board/{name}"))
            (bd / vbr.ARTIFACT_NAME).write_text((BOARD / vbr.ARTIFACT_NAME).read_text())
            git = ["git", "-c", "user.email=tick187@test", "-c", "user.name=tick187"]
            subprocess.run(["git", "init", "-q"], cwd=tmp, check=True)
            subprocess.run(git + ["add", "-A"], cwd=tmp, check=True)
            subprocess.run(git + ["commit", "-qm", "pre-change snapshot"], cwd=tmp, check=True)
            sha = subprocess.run(["git", "rev-parse", "HEAD"], cwd=tmp,
                                 capture_output=True, text=True, check=True).stdout.strip()
            rep = vbr.check_board(fake, sha, quiet=True)
            self.assertTrue(rep.failures,
                            "the checker reported the pre-change snapshot as OK — it cannot fail")
            joined = "\n".join(rep.failures)
            self.assertIn("task ids unique", joined)
            self.assertIn("no removed row is still present verbatim", joined)


if __name__ == "__main__":
    unittest.main(verbosity=2)
