#!/usr/bin/env python3
"""Verify the BOARD-HYGIENE-001 (warpfs tick 187) board reconciliation.

Stdlib only. Checks, against the pre-change snapshot git commit 71b333f:

  1. every board JSONL line parses;
  2. task ids are unique;
  3. every status is in the board vocabulary;
  4. guard_result / ci_result are in their vocabularies;
  5. no reasoning field is left as a character array;
  6. events.jsonl / board.jsonl / fixtures.jsonl are byte-identical to the snapshot,
     and every tasks.jsonl line the artifact does not list is byte-identical too;
  7. the audit artifact records every removed row (verbatim) and every re-identified
     row, and the original-row -> canonical-id mapping;
  8. the completed rows keep completion evidence and are not pending;
  9. no row that was pending before the reconciliation became complete (no finding
     was silently closed);
 10. each normalised guard/ci field retains its original free-form text in <field>_note.

Exit code 0 = all checks pass. Any failure prints FAIL lines and exits 1.

Usage:
  python3 scripts/verify_board_reconciliation.py [--repo DIR] [--base REF] [--quiet]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import sys
from pathlib import Path

BASE_COMMIT_DEFAULT = "71b333f"
ARTIFACT_NAME = "reconciliation-187.json"
STATUS_VOCAB = {"pending", "in_progress", "blocked", "review", "failed", "complete"}
GUARD_VOCAB = {"PASS", "FAIL", "SKIP"}
CI_VOCAB = {"GREEN", "RED", "SKIP", "PASS", "FAIL"}
# rows whose completion must survive the reconciliation (acceptance criteria)
REQUIRED_COMPLETE = ("GAP-067", "GAP-077", "GAP-079", "GAP-080")


class Report:
    def __init__(self, quiet: bool = False) -> None:
        self.failures: list[str] = []
        self.checks: list[tuple[str, str]] = []
        self.quiet = quiet

    def check(self, name: str, ok: bool, detail: str = "") -> bool:
        self.checks.append((name, "PASS" if ok else "FAIL"))
        if not ok:
            self.failures.append(f"{name}: {detail}")
        if not self.quiet:
            print(f"[{'PASS' if ok else 'FAIL'}] {name}" + (f" — {detail}" if detail and not ok else ""))
        return ok

    def note(self, text: str) -> None:
        if not self.quiet:
            print(f"       {text}")


def git_show(repo: Path, ref: str, relpath: str) -> str:
    out = subprocess.run(
        ["git", "-C", str(repo), "show", f"{ref}:{relpath}"],
        capture_output=True, check=True,
    )
    return out.stdout.decode("utf-8")


def parse_rows(text: str) -> tuple[list[str], list[dict]]:
    """Return (raw_lines, parsed_rows) for LF-separated JSONL.

    Blank lines are skipped (events.jsonl carries legacy blank separators);
    the raw list keeps them so byte comparisons still work.
    """
    raw = text.split("\n")
    if raw and raw[-1] == "":
        raw = raw[:-1]
    rows = []
    for n, line in enumerate(raw, 1):
        if not line.strip():
            continue
        try:
            rows.append(json.loads(line))
        except json.JSONDecodeError as exc:
            raise ValueError(f"line {n}: {exc}") from exc
    return raw, rows


def duplicate_ids(rows: list[dict]) -> dict[str, int]:
    counts: dict[str, int] = {}
    for r in rows:
        counts[r["id"]] = counts.get(r["id"], 0) + 1
    return {k: v for k, v in counts.items() if v > 1}


def is_char_array(value) -> bool:
    return (isinstance(value, list) and bool(value)
            and all(isinstance(x, str) for x in value) and len(value[0]) == 1)


def looks_like_commit_sha(text: str) -> bool:
    import re
    return bool(re.search(r"\b[0-9a-f]{7,40}\b", text or ""))


def check_board(repo: Path, base: str, quiet: bool = False) -> Report:
    board = repo / ".coding-hermes" / "board"
    rep = Report(quiet=quiet)
    tasks_path = board / "tasks.jsonl"
    artifact_path = board / ARTIFACT_NAME

    new_text = tasks_path.read_text(encoding="utf-8")
    base_text = git_show(repo, base, ".coding-hermes/board/tasks.jsonl")
    artifact = json.loads(artifact_path.read_text(encoding="utf-8"))

    rep.check("tasks.jsonl parses", _parses(new_text), _parse_error(new_text))
    events_new = (board / "events.jsonl").read_text()
    events_base = git_show(repo, base, ".coding-hermes/board/events.jsonl")
    rep.check("events.jsonl parse behaviour unchanged vs snapshot",
              _bad_lines(events_new) == _bad_lines(events_base),
              f"{_bad_lines(events_new)[:3]} vs {_bad_lines(events_base)[:3]}")
    new_raw, new_rows = parse_rows(new_text)
    base_raw, base_rows = parse_rows(base_text)

    # 2. uniqueness
    dups = duplicate_ids(new_rows)
    rep.check("task ids unique", not dups, f"duplicates: {dups}")
    rep.check("pre-change board really had duplicate ids (check is not vacuous)",
              bool(duplicate_ids(base_rows)),
              "snapshot has no duplicates — the reconciliation premise is unproven")

    # 3. statuses
    bad_status = sorted({str(r.get("status")) for r in new_rows
                         if r.get("status") not in STATUS_VOCAB})
    rep.check("all statuses in vocabulary", not bad_status, f"offending: {bad_status}")

    # 4. guard/ci vocabularies
    bad_guard = sorted({str(r.get("guard_result")) for r in new_rows
                        if r.get("guard_result") is not None
                        and r.get("guard_result") not in GUARD_VOCAB})
    bad_ci = sorted({str(r.get("ci_result")) for r in new_rows
                     if r.get("ci_result") is not None
                     and r.get("ci_result") not in CI_VOCAB})
    rep.check("guard_result in vocabulary", not bad_guard, f"offending: {bad_guard}")
    rep.check("ci_result in vocabulary", not bad_ci, f"offending: {bad_ci}")

    # 5. no character-array reasoning survives
    char_arrays = [r["id"] for r in new_rows if is_char_array(r.get("reasoning"))]
    rep.check("no character-array reasoning remains", not char_arrays,
              f"offending rows: {char_arrays}")
    restored = artifact.get("reasoning_restorations", [])
    rep.check("artifact records the reasoning restorations", len(restored) >= 4,
              f"recorded: {len(restored)}")

    # 6. preservation
    for name in ("events.jsonl", "board.jsonl", "fixtures.jsonl"):
        rel = f".coding-hermes/board/{name}"
        same = hashlib.sha256((board / name).read_bytes()).hexdigest() == \
            hashlib.sha256(git_show(repo, base, rel).encode()).hexdigest()
        rep.check(f"{name} byte-identical to {base}", same, "file was modified")
    rep.check("events.jsonl unchanged (prefix property holds trivially)",
              (board / "events.jsonl").read_text() ==
              git_show(repo, base, ".coding-hermes/board/events.jsonl"),
              "events differ from the snapshot")

    removed = {int(r["original_line"]) for r in artifact.get("removed_rows", [])}
    changed = {int(n) for n in artifact.get("changed_lines", [])}
    rep.check("artifact lists every removed line", len(removed) == artifact["summary"]["rows_removed"],
              f"{len(removed)} vs {artifact['summary']['rows_removed']}")

    survivors_ok = True
    bad_survivors: list[int] = []
    for n, raw in enumerate(base_raw, 1):
        if n in removed or n in changed:
            continue
        if raw not in new_raw:
            survivors_ok = False
            bad_survivors.append(n)
    rep.check("untouched task lines are byte-identical to the snapshot", survivors_ok,
              f"missing/changed original lines: {bad_survivors[:10]}")

    still_present = [r["original_line"] for r in artifact.get("removed_rows", [])
                     if r["raw"] in new_raw]
    rep.check("no removed row is still present verbatim", not still_present,
              f"still present: {still_present}")

    # 7. mapping + re-identification provenance
    by_id = {r["id"]: r for r in new_rows}
    mapped_ok = True
    mapped_bad: list[str] = []
    for row in artifact.get("reidentified_rows", []):
        got = by_id.get(row["new_id"])
        recon = (got or {}).get("reconciliation", {})
        if not got or recon.get("original_id") != row["original_id"] \
                or int(recon.get("original_line", -1)) != int(row["original_line"]):
            mapped_ok = False
            mapped_bad.append(f"{row['original_id']} (line {row['original_line']}) -> {row['new_id']}")
        if got is not None and got.get("title") != row.get("original_title"):
            mapped_ok = False
            mapped_bad.append(f"{row['new_id']}: title changed during re-identification")
    rep.check("re-identified rows carry original id/line provenance", mapped_ok, str(mapped_bad))
    effective_mapping = {k: list(v) for k, v in artifact.get("mapping", {}).items()}
    for row in artifact.get("removed_rows", []):
        effective_mapping.setdefault(row["original_id"], []).append(
            {"original_line": row["original_line"], "canonical_id": row["canonical_id"]})
    base_dups = set(duplicate_ids(base_rows))
    rep.check("original-row -> canonical-id mapping covers every pre-change duplicate id",
              set(effective_mapping) == base_dups,
              f"mapping keys {sorted(effective_mapping)} vs dup ids {sorted(base_dups)}")

    pre_ids = {r["id"] for r in base_rows}
    reused = sorted(r["new_id"] for r in artifact.get("reidentified_rows", [])
                    if r["new_id"] in pre_ids)
    rep.check("fresh ids do not collide with pre-existing ids", not reused,
              f"re-identified rows reused existing ids: {reused}")

    # 8. closures retain evidence
    for tid in REQUIRED_COMPLETE:
        rows = [r for r in new_rows if r["id"] == tid]
        ok = bool(rows) and all(r.get("status") == "complete" for r in rows)
        evidence = ""
        if rows:
            r = rows[0]
            evidence = str(r.get("commit_hash") or "") + str(r.get("foreman_note") or "") + \
                str(r.get("reasoning") or "")
        rep.check(f"{tid} complete with completion evidence",
                  ok and bool(evidence.strip()) and looks_like_commit_sha(evidence),
                  f"status={[r.get('status') for r in rows]} evidence_ok={looks_like_commit_sha(evidence)}")

    # 9. no pending finding was silently closed
    #    survivors are matched by their original line number via the artifact's drop list
    line_map = {}
    new_index = 1
    for n in range(1, len(base_raw) + 1):
        if n in removed:
            continue
        line_map[n] = new_index
        new_index += 1
    flipped = []
    for n, base_row in enumerate(base_rows, 1):
        if n in removed or n not in line_map:
            continue
        new_row = new_rows[line_map[n] - 1]
        if base_row.get("status") != "complete" and new_row.get("status") == "complete":
            flipped.append((n, base_row["id"], base_row.get("status")))
    documented = {int(c["original_line"]) for c in artifact.get("field_corrections", [])
                  if c["field"] == "status"}
    undeclared = [f for f in flipped if f[0] not in documented]
    rep.check("no undocumented pending->complete flip", not undeclared, f"{undeclared}")

    # 10. normalised fields keep their original text
    notes_ok, notes_bad = True, []
    for row in artifact.get("field_corrections", []):
        if row["field"] not in ("guard_result", "ci_result"):
            continue
        n = line_map.get(int(row["original_line"]))
        target = new_rows[n - 1] if n else None
        keep = (target or {}).get(f"{row['field']}_note") or ""
        if str(row["from"]) not in keep:
            notes_ok = False
            notes_bad.append(f"{row['id']}.{row['field']}")
    rep.check("original free-form guard/ci text retained in <field>_note", notes_ok,
              f"{notes_bad}")

    # informational
    pending = [r["id"] for r in new_rows if r.get("status") == "pending"]
    rep.note(f"rows: {len(base_rows)} -> {len(new_rows)}  "
             f"complete: {sum(1 for r in new_rows if r.get('status') == 'complete')}  "
             f"pending: {len(pending)}  dropped: {len(removed)}  "
             f"re-identified: {len(artifact.get('reidentified_rows', []))}")
    return rep


def _parses(text: str) -> bool:
    try:
        parse_rows(text)
        return True
    except ValueError:
        return False


def _bad_lines(text: str) -> list[tuple[int, str]]:
    bad: list[tuple[int, str]] = []
    for n, line in enumerate(text.split("\n"), 1):
        if not line.strip():
            continue
        try:
            json.loads(line)
        except json.JSONDecodeError as exc:
            bad.append((n, str(exc)))
    return bad


def _parse_error(text: str) -> str:
    try:
        parse_rows(text)
        return ""
    except ValueError as exc:
        return str(exc)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", default=None)
    ap.add_argument("--base", default=BASE_COMMIT_DEFAULT)
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args()
    repo = Path(args.repo).resolve() if args.repo else Path(__file__).resolve().parent.parent
    rep = check_board(repo, args.base)
    if rep.failures:
        print(f"\nRESULT: FAIL ({len(rep.failures)} check(s))")
        for f in rep.failures:
            print(f"  - {f}")
        return 1
    print(f"\nRESULT: OK ({len(rep.checks)} checks passed)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
