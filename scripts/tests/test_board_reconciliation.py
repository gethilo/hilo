#!/usr/bin/env python3
"""Regression tests for the BOARD-HYGIENE-001 board reconciliation (warpfs tick 187).

The tests exercise REAL board data on both sides of every comparison:

  base state = git show 71b333f:...            (pre-change snapshot)
  new state  = git show de9374c:...            (the reconciliation revision,
               materialised into a temp dir — the working tree is NEVER read)

That split is deliberate: ordinary board activity after the reconciliation
(appending events, updating the board header, closing a task) must not turn this
suite red, while a mutated reconciliation revision still must. The live-state
code path is covered separately, in throwaway git repos, so it can be exercised
without touching the real board.

Run:  python3 -m unittest discover -s scripts/tests -t . -v
      (or) python3 scripts/tests/test_board_reconciliation.py
"""

from __future__ import annotations

import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "scripts"))

import verify_board_reconciliation as vbr  # noqa: E402

BASE = vbr.BASE_COMMIT_DEFAULT
REV = vbr.RECONCILED_REV_DEFAULT
REV_DIR = vbr.materialize_board(REPO, REV)
BOARD = REV_DIR / vbr.BOARD_REL
ARTIFACT = json.loads((BOARD / vbr.ARTIFACT_NAME).read_text())


def rows_of(text: str) -> list[dict]:
    return vbr.parse_rows(text)[1]


def seed_repo(root: Path, rev_content: bool = True) -> tuple[str, str]:
    """Throwaway git repo: commit 1 = 71b333f board, commit 2 = de9374c board.

    Returns (base_sha, rev_sha); with rev_content=False both are the snapshot.
    """
    board = root / vbr.BOARD_REL
    board.mkdir(parents=True, exist_ok=True)
    for name in vbr.BOARD_FILES:
        (board / name).write_text(vbr.git_show(REPO, BASE, f"{vbr.BOARD_REL}/{name}"))
    (board / vbr.ARTIFACT_NAME).write_text((BOARD / vbr.ARTIFACT_NAME).read_text())
    git = ["git", "-c", "user.email=tick187@test", "-c", "user.name=tick187"]
    subprocess.run(["git", "init", "-q"], cwd=root, check=True)
    subprocess.run(git + ["add", "-A"], cwd=root, check=True)
    subprocess.run(git + ["commit", "-qm", f"pre-change snapshot {BASE}"], cwd=root, check=True)
    base_sha = subprocess.run(["git", "rev-parse", "HEAD"], cwd=root,
                              capture_output=True, text=True, check=True).stdout.strip()
    if not rev_content:
        return base_sha, base_sha
    for name in vbr.BOARD_FILES:
        (board / name).write_text(vbr.git_show(REPO, REV, f"{vbr.BOARD_REL}/{name}"))
    subprocess.run(git + ["commit", "-qam", f"reconciliation {REV}"], cwd=root, check=True)
    rev_sha = subprocess.run(["git", "rev-parse", "HEAD"], cwd=root,
                             capture_output=True, text=True, check=True).stdout.strip()
    return base_sha, rev_sha


def append_task(root: Path, row: dict) -> None:
    with (root / vbr.BOARD_REL / "tasks.jsonl").open("a") as fh:
        fh.write(json.dumps(row) + "\n")


def edit_task_lines(root: Path, mutate) -> None:
    """Rewrite ONLY the lines `mutate(row, line)` returns a replacement row for.

    Untouched lines keep their exact bytes, so these edits model what a later
    tick actually does to the board.
    """
    path = root / vbr.BOARD_REL / "tasks.jsonl"
    out = []
    for line in path.read_text().split("\n"):
        if not line.strip():
            continue
        row = json.loads(line)
        replacement = mutate(row, line)
        out.append(line if replacement is None else json.dumps(replacement))
    path.write_text("".join(ln + "\n" for ln in out))


def legit_post_reconciliation_activity(root: Path) -> None:
    """What a legitimate later tick does: append events, close a task, new row."""
    board = root / vbr.BOARD_REL
    with (board / "events.jsonl").open("a") as fh:
        fh.write(json.dumps({"id": "evt-tick188", "task": "GAP-060",
                             "action": "task_completed", "ts": "2026-09-17T08:10:00-05:00",
                             "note": "closed by the next tick"}) + "\n")
    with (board / "board.jsonl").open("a") as fh:
        fh.write(json.dumps({"type": "tick", "id": "tick-188",
                             "ts": "2026-09-17T08:10:00-05:00"}) + "\n")

    def close(row: dict, line: str):
        if row["id"] != "BOARD-HYGIENE-001":
            return None
        return {**row, "status": "complete", "completed_at": "2026-09-17T08:10:00-05:00",
                "completed_commit": "a1b2c3d", "foreman_note": "checked into master"}

    edit_task_lines(root, close)
    append_task(root, {"id": "QA-WARPFS-9", "status": "pending", "title": "new tick finding",
                       "ts": "2026-09-17T08:11:00-05:00"})


def close_task_silently(root: Path, task_id: str) -> None:
    """Flip a pending row to complete with no evidence at all."""
    def close(row: dict, line: str):
        if row["id"] != task_id:
            return None
        row = {**row}
        row.pop("completed_at", None)
        row.pop("completed_commit", None)
        row.pop("closure_evidence", None)
        row["status"] = "complete"
        return row

    edit_task_lines(root, close)


class PrechangeSnapshot(unittest.TestCase):
    """The snapshot the acceptance criteria name must really be the broken state."""

    @classmethod
    def setUpClass(cls) -> None:
        cls.base_text = vbr.git_show(REPO, BASE, f"{vbr.BOARD_REL}/tasks.jsonl")
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
        cls.base_rows = rows_of(vbr.git_show(REPO, BASE, f"{vbr.BOARD_REL}/tasks.jsonl"))

    def test_new_state_is_the_immutable_revision_not_the_working_tree(self) -> None:
        """The suite must read de9374c, so later board activity cannot break it."""
        self.assertEqual(self.new_text, vbr.git_show(REPO, REV, f"{vbr.BOARD_REL}/tasks.jsonl"))
        self.assertNotEqual(str(BOARD), str(REPO / vbr.BOARD_REL))

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
        cls.base_text = vbr.git_show(REPO, BASE, f"{vbr.BOARD_REL}/tasks.jsonl")
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
            rel = f"{vbr.BOARD_REL}/{name}"
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
        base_events = vbr.git_show(REPO, BASE, f"{vbr.BOARD_REL}/events.jsonl")
        self.assertEqual(vbr._bad_lines((BOARD / "events.jsonl").read_text()),
                         vbr._bad_lines(base_events))


class ImmutableRevisionMode(unittest.TestCase):
    """Default mode must be immune to legitimate board activity, not blind to mutation."""

    def setUp(self) -> None:
        self.tmp = tempfile.mkdtemp(prefix="warpfs187-test-")
        self.fake = Path(self.tmp)
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.base_sha, self.rev_sha = seed_repo(self.fake)

    def test_legit_activity_after_reconciliation_does_not_break_verification(self) -> None:
        """Appended events, a header update and a task closure: still OK."""
        legit_post_reconciliation_activity(self.fake)
        rep = vbr.check_board(self.fake, base=self.base_sha, rev=self.rev_sha, quiet=True)
        self.assertEqual(rep.failures, [], "legitimate board activity broke the checks")

    def test_legit_activity_is_seen_by_live_mode_without_false_alarms(self) -> None:
        legit_post_reconciliation_activity(self.fake)
        rep = vbr.check_board(self.fake, base=self.base_sha, rev=self.rev_sha,
                              live=True, quiet=True)
        self.assertEqual(rep.failures, [], "live mode rejected a legitimate closure/append")

    def test_silent_closure_is_rejected_in_live_mode(self) -> None:
        legit_post_reconciliation_activity(self.fake)
        close_task_silently(self.fake, "GAP-060")
        rep = vbr.check_board(self.fake, base=self.base_sha, rev=self.rev_sha,
                              live=True, quiet=True)
        joined = "\n".join(rep.failures)
        self.assertIn("closed with completion evidence", joined)
        self.assertIn("no undocumented pending->complete flip", joined)

    def test_deleted_row_and_rewritten_events_are_rejected_in_live_mode(self) -> None:
        board = self.fake / vbr.BOARD_REL
        rows = [r for r in rows_of((board / "tasks.jsonl").read_text()) if r["id"] != "GAP-060"]
        (board / "tasks.jsonl").write_text("".join(json.dumps(r) + "\n" for r in rows))
        events = (board / "events.jsonl").read_text().split("\n")
        (board / "events.jsonl").write_text("\n".join(events[: len(events) // 2]))
        rep = vbr.check_board(self.fake, base=self.base_sha, rev=self.rev_sha,
                              live=True, quiet=True)
        joined = "\n".join(rep.failures)
        self.assertIn("no row deleted", joined)
        self.assertIn("append-only prefix", joined)

    def test_mutated_revision_is_still_rejected_in_default_mode(self) -> None:
        """Strip a re-identified row's provenance, commit it as the 'revision'."""
        new_id = ARTIFACT["reidentified_rows"][0]["new_id"]

        def strip(row: dict, line: str):
            if row["id"] != new_id:
                return None
            return {k: v for k, v in row.items() if k != "reconciliation"}

        edit_task_lines(self.fake, strip)
        subprocess.run(["git", "-c", "user.email=t@t", "-c", "user.name=t",
                        "commit", "-qam", "mutated revision"], cwd=self.fake, check=True)
        mutated = subprocess.run(["git", "rev-parse", "HEAD"], cwd=self.fake,
                                 capture_output=True, text=True, check=True).stdout.strip()
        rep = vbr.check_board(self.fake, base=self.base_sha, rev=mutated, quiet=True)
        self.assertTrue(rep.failures, "a mutated revision was reported as OK")
        self.assertIn("provenance", "\n".join(rep.failures))

    def test_revision_with_reintroduced_duplicate_id_is_rejected(self) -> None:
        def duplicate(row: dict, line: str):
            if row["id"] != "QA-WARPFS-1-2":
                return None
            return dict(row, id="QA-WARPFS-1")

        edit_task_lines(self.fake, duplicate)
        subprocess.run(["git", "-c", "user.email=t@t", "-c", "user.name=t",
                        "commit", "-qam", "duplicate id reintroduced"], cwd=self.fake, check=True)
        mutated = subprocess.run(["git", "rev-parse", "HEAD"], cwd=self.fake,
                                 capture_output=True, text=True, check=True).stdout.strip()
        rep = vbr.check_board(self.fake, base=self.base_sha, rev=mutated, quiet=True)
        self.assertIn("task ids unique", "\n".join(rep.failures))


class EndToEndChecker(unittest.TestCase):
    def test_verify_script_passes_on_the_reconciled_revision(self) -> None:
        out = subprocess.run(
            [sys.executable, str(REPO / "scripts" / "verify_board_reconciliation.py"), "--quiet"],
            capture_output=True, text=True, cwd=str(REPO),
        )
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
        self.assertIn("RESULT: OK", out.stdout)

    def test_verify_script_live_mode_flag_runs(self) -> None:
        out = subprocess.run(
            [sys.executable, str(REPO / "scripts" / "verify_board_reconciliation.py"),
             "--live", "--quiet"],
            capture_output=True, text=True, cwd=str(REPO),
        )
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
        self.assertIn("RESULT: OK", out.stdout)

    def test_verify_script_fails_on_the_snapshot(self) -> None:
        """Point the checker at the pre-change snapshot: it must FAIL.

        A throwaway git repo is seeded with the exact 71b333f board content and
        committed, so `git show <its HEAD>:...` supplies both sides.
        """
        with tempfile.TemporaryDirectory() as tmp:
            fake = Path(tmp)
            sha, _ = seed_repo(fake, rev_content=False)
            rep = vbr.check_board(fake, base=sha, rev=sha, quiet=True)
            self.assertTrue(rep.failures,
                            "the checker reported the pre-change snapshot as OK — it cannot fail")
            joined = "\n".join(rep.failures)
            self.assertIn("task ids unique", joined)
            self.assertIn("no removed row is still present verbatim", joined)


if __name__ == "__main__":
    unittest.main(verbosity=2)
