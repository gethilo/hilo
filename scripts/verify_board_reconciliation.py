#!/usr/bin/env python3
"""Verify the BOARD-HYGIENE-001 (warpfs tick 187) board reconciliation.

Stdlib only.

TWO MODES
---------
default (immutable revision)
    Both sides of every comparison come from git, so ordinary board activity
    (appending events, updating the board header, closing a task) can never turn
    this suite red:

      new state = git show <rev>:...       rev defaults to the reconciliation
                                           revision de9374c
      base state = git show <base>:...     base defaults to the pre-change
                                           snapshot 71b333f

    The revision content is materialised into a temp directory via `git show`;
    the working tree is NOT read. `--repo` only has to point at a real git repo.

--live (explicit opt-in)
    New state = the working tree. Immutable-content checks become
    append/presence checks, and a row may change only as a *documented closure*
    (status -> complete with completed_at / completed_commit / closure_evidence);
    for a row the artifact re-identified, the documented rename is accepted too.
    Tolerated in live mode: appended rows and events, a rewritten/appended board
    header, id-preserving closures with evidence. Reported as failures:
    deleted rows, reordered/rewritten history, mutation of a materialised
    revision, silent closures, regressions complete -> pending, prefix loss.

CHECKS (against the pre-change snapshot 71b333f)
  1. every board JSONL line parses;
  2. task ids are unique (and the snapshot really did have duplicates);
  3. every status is in the board vocabulary;
  4. guard_result / ci_result are in their vocabularies;
  5. no reasoning field is left as a character array;
  6. events.jsonl / board.jsonl / fixtures.jsonl are byte-identical to the
     snapshot, and every tasks.jsonl line the artifact does not list is
     byte-identical too (live mode: append-only prefix / line-presence);
  7. the audit artifact records every removed row (verbatim) and every
     re-identified row, and the original-row -> canonical-id mapping;
  8. the completed rows keep completion evidence and are not pending;
  9. no row that was pending before the reconciliation became complete without
     evidence (no finding was silently closed); a re-identified row may close
     under its documented new id, but only with the same title and evidence;
 10. each normalised guard/ci field retains its original free-form text in
     <field>_note.
 11. LIVE-CONTENT HYGIENE — the board a reader loads TODAY carries no
     over-escaped text: no string value of the working-tree tasks.jsonl
     contains a literal backslash immediately followed by a double quote
     (the `\"` / `\\"` signature of a writer that escaped an already-escaped
     payload, where the text means `"`). See the section below.

LIVE-CONTENT HYGIENE IS NOT REVISION-PINNED (why)
-------------------------------------------------
Checks 1-10 compare the reconciled board against immutable revisions and are
deliberately pinned, so ordinary board activity can never turn them red.
Check 11 is the opposite kind of check: it is a property of the text a reader
actually loads, and pinning it would make it blind BY CONSTRUCTION — the pinned
revision de9374c is itself one of the revisions carrying the corruption (it
flags exactly the four rows QA-WARPFS-1, QA-WARPFS-2, QA-WARPFS-1-2 and
QA-WARPFS-3-4). So check 11 ALWAYS reads the WORKING TREE board of the
repository under test (`<repo>/.coding-hermes/board/tasks.jsonl`) and its
findings are recorded as live-content findings (`Report.live_failures`) rather
than reconciliation failures (`Report.failures`): the two describe different
subjects — the pinned reconciliation contract vs. the checked-out text — and
both make the exit code non-zero.

events.jsonl is append-only history: it is scanned and its over-escaped count
is REPORTED (informational line, printed even under --quiet), never repaired
and never a failure. `--repo` selects the repository being checked, so the
hygiene scan follows it.

Exit code 0 = all checks pass. Any failure prints FAIL lines and exits 1.

Usage:
  python3 scripts/verify_board_reconciliation.py [--repo DIR] [--base REF]
                                                 [--rev REF] [--live] [--quiet]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

# the reconciliation commit: immutable, so the checks never depend on live state
RECONCILED_REV_DEFAULT = "de9374c"
BASE_COMMIT_DEFAULT = "71b333f"
ARTIFACT_NAME = "reconciliation-187.json"
BOARD_REL = ".coding-hermes/board"
BOARD_FILES = ("tasks.jsonl", "events.jsonl", "board.jsonl", "fixtures.jsonl")
# files a legitimate tick only ever appends to
APPEND_ONLY_FILES = ("events.jsonl", "fixtures.jsonl")
# files a legitimate tick may rewrite, but never lose a line of
PRESENCE_FILES = ("board.jsonl",)
# a row may flip to complete only with one of these non-empty (live mode)
COMPLETION_MARKERS = ("completed_at", "completed_commit", "completed_commit_hash",
                      "closure_evidence")
STATUS_VOCAB = {"pending", "in_progress", "blocked", "review", "failed", "complete"}
GUARD_VOCAB = {"PASS", "FAIL", "SKIP"}
CI_VOCAB = {"GREEN", "RED", "SKIP", "PASS", "FAIL"}
# rows whose completion must survive the reconciliation (acceptance criteria)
REQUIRED_COMPLETE = ("GAP-067", "GAP-077", "GAP-079", "GAP-080")


class Report:
    def __init__(self, quiet: bool = False) -> None:
        self.failures: list[str] = []
        # live-content findings (check 11): same weight for the exit code, kept
        # in their own list because they describe the CHECKED-OUT text while
        # `failures` describes the pinned reconciliation contract (see docstring)
        self.live_failures: list[str] = []
        self.checks: list[tuple[str, str]] = []
        self.quiet = quiet

    def check(self, name: str, ok: bool, detail: str = "",
              live_content: bool = False) -> bool:
        self.checks.append((name, "PASS" if ok else "FAIL"))
        if not ok:
            bucket = self.live_failures if live_content else self.failures
            bucket.append(f"{name}: {detail}")
        if not self.quiet:
            print(f"[{'PASS' if ok else 'FAIL'}] {name}" + (f" — {detail}" if detail and not ok else ""))
        return ok

    def note(self, text: str) -> None:
        if not self.quiet:
            print(f"       {text}")

    def note_loud(self, text: str) -> None:
        """Informational line printed even under --quiet.

        Used for the events.jsonl scan: that is a REPORT (count only, history is
        never repaired), so --quiet must not hide it.
        """
        print(f"       {text}")


def git_show(repo: Path, ref: str, relpath: str) -> str:
    out = subprocess.run(
        ["git", "-C", str(repo), "show", f"{ref}:{relpath}"],
        capture_output=True, check=True,
    )
    return out.stdout.decode("utf-8")


def materialize_board(repo: Path, rev: str, dest: Path | str | None = None) -> Path:
    """Write the board files at `rev` into `dest`; return the repo-root-shaped dir.

    This is the immutable target the checks compare against by default, so
    later legitimate board activity cannot affect the result.
    """
    root = Path(tempfile.mkdtemp(prefix=f"warpfs187-{rev[:8]}-")) if dest is None else Path(dest)
    board = root / BOARD_REL
    board.mkdir(parents=True, exist_ok=True)
    for name in BOARD_FILES:
        (board / name).write_text(git_show(repo, rev, f"{BOARD_REL}/{name}"), encoding="utf-8")
    (board / ARTIFACT_NAME).write_text(git_show(repo, rev, f"{BOARD_REL}/{ARTIFACT_NAME}"),
                                       encoding="utf-8")
    return root


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
    return bool(re.search(r"\b[0-9a-f]{7,40}\b", text or ""))


# --------------------------------------------------------------- check 11 seam
# Inside a DECODED string value, a literal backslash immediately followed by a
# double quote is the signature of a writer that escaped an already-escaped
# payload: the reader sees `\"` (or `\\"`) where the text means `"`.
OVERESCAPED_SEQ = "\\\""


def iter_strings(obj, path: str = ""):
    """Yield (dotted path, value) for every string value inside a decoded row."""
    if isinstance(obj, str):
        yield path, obj
    elif isinstance(obj, dict):
        for key, val in obj.items():
            yield from iter_strings(val, f"{path}.{key}" if path else str(key))
    elif isinstance(obj, list):
        for i, val in enumerate(obj):
            yield from iter_strings(val, f"{path}[{i}]")


def over_escaped_fields(row: dict) -> list[str]:
    """Dotted paths of this row's string values that carry over-escaped text."""
    return [p for p, val in iter_strings(row) if OVERESCAPED_SEQ in val]


def over_escaped_rows(rows: list[dict]) -> list[tuple[str, list[str]]]:
    """[(row id, [field paths])] for every row whose text is over-escaped.

    `id` is used when present (tasks.jsonl) and a positional `line-N` fallback
    otherwise, so the same scan works on any JSONL board file.
    """
    found: list[tuple[str, list[str]]] = []
    for n, row in enumerate(rows, 1):
        fields = over_escaped_fields(row)
        if fields:
            found.append((str(row.get("id") or f"line-{n}"), fields))
    return found


def scan_over_escaped(text: str) -> tuple[list[tuple[str, list[str]]], str]:
    """Scan JSONL text; return (findings, parse error).

    A parse error is returned, never raised: the hygiene scan must not mask a
    json.loads failure that belongs to a different check.
    """
    try:
        _raw, rows = parse_rows(text)
    except ValueError as exc:
        return [], str(exc)
    return over_escaped_rows(rows), ""


def events_hygiene(text: str) -> tuple[int, int]:
    """(rows, string values) carrying over-escaped text in events.jsonl."""
    findings, _err = scan_over_escaped(text)
    return len(findings), sum(len(fields) for _rid, fields in findings)


def reduce_over_escaping(value):
    """`value` with every spurious escaping level removed (fixpoint)."""
    if isinstance(value, str):
        out = value
        while True:
            nxt = out.replace("\\\\", "\\").replace('\\"', '"')
            if nxt == out:
                return out
            out = nxt
    if isinstance(value, dict):
        return {k: reduce_over_escaping(v) for k, v in value.items()}
    if isinstance(value, list):
        return [reduce_over_escaping(v) for v in value]
    return value


def is_over_escaping_repair(rev_row: dict, live_row: dict) -> bool:
    """True when live_row is rev_row with check 11's class repaired in place.

    Repairing the over-escaping class necessarily CHANGES the rendered text
    (that is the point: `\\"` becomes `"`), so the live-mode row comparison would
    otherwise read the repair itself as a content mutation.  The allowance is
    deliberately narrow: the pinned row must have been over-escaped, the live
    row must keep the same keys in the same order, and every value must equal
    the pinned value with the spurious escaping removed — nothing else may
    differ.
    """
    if not over_escaped_fields(rev_row):
        return False
    if list(rev_row) != list(live_row):
        return False
    return reduce_over_escaping(rev_row) == live_row


def is_documented_closure(rev_row: dict, live_row: dict) -> bool:
    """True when a live row changed only by closing with real evidence."""
    if rev_row.get("status") == "complete" or live_row.get("status") != "complete":
        return False
    if live_row.get("id") != rev_row.get("id") or live_row.get("title") != rev_row.get("title"):
        return False
    return any(str(live_row.get(k) or "").strip() for k in COMPLETION_MARKERS)


def documented_reidentification(artifact: dict, base_line: int, base_row: dict,
                                live_row: dict) -> bool:
    """True when the artifact records THIS base row being renamed to the live id.

    The reconciliation renumbered duplicate families (artifact key
    `reidentified_rows`), so a pending stub at a given base line is published
    under a NEW id. The rename is documented provenance — the live row is the
    same finding, not a substitution — but it leaves the live row's id different
    from the snapshot's, which is exactly what is_documented_closure refuses.
    The lookup is keyed on (original_line, original_id) -> new_id, so no other
    row of the family can satisfy it.
    """
    for entry in artifact.get("reidentified_rows", []):
        if int(entry.get("original_line", -1)) != int(base_line):
            continue
        if entry.get("original_id") != base_row.get("id"):
            continue
        return entry.get("new_id") == live_row.get("id")
    return False


def is_documented_reidentified_closure(artifact: dict, base_line: int, base_row: dict,
                                       live_row: dict) -> bool:
    """A documented re-identification PLUS a real closure — nothing looser.

    Used only by check 9 (the pending->complete flip check). is_documented_closure
    stays unchanged for the in-place row comparison, which matches rows by id and
    therefore can never see a rename. Every part of "documented closure" still has
    to hold: pending before, complete now, the SAME title, and at least one
    non-empty completion marker. Id equality — and only id equality — is replaced
    by the artifact's own rename record.
    """
    if not live_row:
        return False
    if base_row.get("status") == "complete" or live_row.get("status") != "complete":
        return False
    if live_row.get("title") != base_row.get("title"):
        return False
    if not any(str(live_row.get(k) or "").strip() for k in COMPLETION_MARKERS):
        return False
    return documented_reidentification(artifact, base_line, base_row, live_row)


def same_content_ignoring_escaping(rev_row: dict, live_row: dict) -> bool:
    """True when two rows decode to identical CONTENT.

    The raw line may still differ, which is exactly what an escaping-only edit
    does: the same values re-encoded with a different number of escaping levels
    (or with non-ASCII written literally instead of \\uXXXX).  Repairs of the
    over-escaping class are those edits, so live mode must not read them as
    content mutations — while any change to a value still differs here and is
    still reported.
    """
    return (json.dumps(rev_row, sort_keys=True, ensure_ascii=False)
            == json.dumps(live_row, sort_keys=True, ensure_ascii=False))


def check_board(repo: Path, base: str = BASE_COMMIT_DEFAULT,
                rev: str = RECONCILED_REV_DEFAULT, live: bool = False,
                quiet: bool = False) -> Report:
    """Run every check. Default: both sides read from immutable git revisions."""
    rep = Report(quiet=quiet)
    if live:
        rep.note("mode: live working tree")
        return _check(repo, base, rev, repo, live=True, rep=rep)
    with tempfile.TemporaryDirectory(prefix="warpfs187-check-") as tmp:
        target = materialize_board(repo, rev, tmp)
        rep.note(f"mode: immutable revision {rev} (base {base})")
        return _check(repo, base, rev, target, live=False, rep=rep)


def _check(repo: Path, base: str, rev: str, target: Path, live: bool,
           rep: Report) -> Report:
    board = target / BOARD_REL
    rep.check("board dir materialised", board.is_dir(), f"{board} missing")
    tasks_path = board / "tasks.jsonl"
    artifact_path = board / ARTIFACT_NAME

    new_text = tasks_path.read_text(encoding="utf-8")
    base_text = git_show(repo, base, f"{BOARD_REL}/tasks.jsonl")
    artifact = json.loads(artifact_path.read_text(encoding="utf-8"))

    rep.check("tasks.jsonl parses", _parses(new_text), _parse_error(new_text))
    events_new = (board / "events.jsonl").read_text()
    events_base = git_show(repo, base, f"{BOARD_REL}/events.jsonl")
    rep.check("events.jsonl parse behaviour unchanged vs snapshot",
              _bad_lines(events_new) == _bad_lines(events_base),
              f"{_bad_lines(events_new)[:3]} vs {_bad_lines(events_base)[:3]}")
    new_raw, new_rows = parse_rows(new_text)
    base_raw, base_rows = parse_rows(base_text)
    by_id = {r["id"]: r for r in new_rows}

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

    # 6. preservation vs 71b333f, and vs the reconciliation revision
    if live:
        # the immutable revision content must survive: append-only files keep it
        # as a byte prefix, presence files keep every line, no row disappears.
        for name in APPEND_ONLY_FILES:
            rev_text = git_show(repo, rev, f"{BOARD_REL}/{name}")
            rep.check(f"{name} keeps the {rev} content as an append-only prefix",
                      (board / name).read_text().startswith(rev_text),
                      "revision content was rewritten or truncated, not appended to")
        for name in PRESENCE_FILES:
            rev_lines = [ln for ln in git_show(repo, rev, f"{BOARD_REL}/{name}").split("\n") if ln.strip()]
            live_lines = [ln for ln in (board / name).read_text().split("\n") if ln.strip()]
            missing = [ln[:80] for ln in rev_lines if ln not in live_lines]
            rep.check(f"{name} keeps every {rev} line", not missing,
                      f"header lines lost: {missing[:3]}")
        rev_raw, rev_rows = parse_rows(git_show(repo, rev, f"{BOARD_REL}/tasks.jsonl"))
        gone = sorted(r["id"] for r in rev_rows if r["id"] not in by_id)
        rep.check(f"every {rev} task row still present (no row deleted)", not gone,
                  f"missing ids: {gone}")
        rev_by_id = {r["id"]: r for r in rev_rows}
        rev_raw_by_id = {r["id"]: ln for r, ln in zip(rev_rows, _nonblank(rev_raw))}
        live_raw_by_id = {r["id"]: ln for r, ln in zip(new_rows, _nonblank(new_raw))}
        unexplained = []
        for tid, rev_row in rev_by_id.items():
            live_row = by_id.get(tid)
            if live_row is None:
                continue
            if live_raw_by_id.get(tid) == rev_raw_by_id.get(tid):
                continue
            if is_documented_closure(rev_row, live_row):
                continue
            # escaping-only repair of a pinned row: same decoded content, so the
            # raw line legitimately differs (see same_content_ignoring_escaping)
            if same_content_ignoring_escaping(rev_row, live_row):
                continue
            # the check-11 repair itself: the pinned text rendered without the
            # spurious escaping is the only difference (see is_over_escaping_repair)
            if is_over_escaping_repair(rev_row, live_row):
                continue
            unexplained.append(tid)
        rep.check("task rows unchanged, re-escaped, repaired or closed with completion evidence",
                  not unexplained, f"unexplained in-place edits: {unexplained[:10]}")
    else:
        for name in APPEND_ONLY_FILES + PRESENCE_FILES:
            rel = f"{BOARD_REL}/{name}"
            same = hashlib.sha256((board / name).read_bytes()).hexdigest() == \
                hashlib.sha256(git_show(repo, base, rel).encode()).hexdigest()
            rep.check(f"{name} byte-identical to {base}", same, "file was modified")
        rep.check("events.jsonl unchanged (prefix property holds trivially)",
                  (board / "events.jsonl").read_text() ==
                  git_show(repo, base, f"{BOARD_REL}/events.jsonl"),
                  "events differ from the snapshot")

    removed = {int(r["original_line"]) for r in artifact.get("removed_rows", [])}
    changed = {int(n) for n in artifact.get("changed_lines", [])}
    rep.check("artifact lists every removed line", len(removed) == artifact["summary"]["rows_removed"],
              f"{len(removed)} vs {artifact['summary']['rows_removed']}")

    if not live:
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
        if line_map[n] - 1 >= len(new_rows):
            # the survivor the snapshot maps to is gone from the board
            flipped.append((n, base_row["id"], base_row.get("status"), base_row, {}))
            continue
        new_row = new_rows[line_map[n] - 1]
        if base_row.get("status") != "complete" and new_row.get("status") == "complete":
            flipped.append((n, base_row["id"], base_row.get("status"), base_row, new_row))
    documented = {int(c["original_line"]) for c in artifact.get("field_corrections", [])
                  if c["field"] == "status"}
    undeclared = [f[:3] for f in flipped
                  if f[0] not in documented
                  and not (live and is_documented_closure(f[3], f[4]))
                  # a renamed row may flip only when the artifact documents the
                  # rename (reidentified_rows) AND the closure carries evidence
                  and not (live and is_documented_reidentified_closure(
                      artifact, f[0], f[3], f[4]))]
    rep.check("no undocumented pending->complete flip", not undeclared, f"{undeclared}")

    # 10. normalised fields keep their original text
    notes_ok, notes_bad = True, []
    for row in artifact.get("field_corrections", []):
        if row["field"] not in ("guard_result", "ci_result"):
            continue
        n = line_map.get(int(row["original_line"]))
        target_row = new_rows[n - 1] if n and n - 1 < len(new_rows) else None
        keep = (target_row or {}).get(f"{row['field']}_note") or ""
        if str(row["from"]) not in keep:
            notes_ok = False
            notes_bad.append(f"{row['id']}.{row['field']}")
    rep.check("original free-form guard/ci text retained in <field>_note", notes_ok,
              f"{notes_bad}")

    # 11. LIVE-content hygiene — deliberately NOT revision-pinned (docstring):
    #     the pinned revision de9374c carries the corruption itself, so a pinned
    #     hygiene check would be blind to exactly the class it exists to catch.
    #     Subject = the checked-out board of the repository under test.
    live_tasks = repo / BOARD_REL / "tasks.jsonl"
    if live_tasks.is_file():
        findings, scan_err = scan_over_escaped(live_tasks.read_text(encoding="utf-8"))
        named = "; ".join(f"{rid}: {','.join(fields)}" for rid, fields in findings)
        rep.check("live tasks.jsonl carries no over-escaped string values",
                  not findings and not scan_err,
                  f"live board does not parse: {scan_err}" if scan_err
                  else f"{len(findings)} row(s) with backslash+quote in the decoded text — {named}",
                  live_content=True)
    live_events = repo / BOARD_REL / "events.jsonl"
    if live_events.is_file():
        ev_rows, ev_fields = events_hygiene(live_events.read_text(encoding="utf-8"))
        rep.note_loud(f"events.jsonl (append-only history, reported not repaired): "
                      f"{ev_fields} over-escaped string value(s) in {ev_rows} row(s)")

    # informational
    pending = [r["id"] for r in new_rows if r.get("status") == "pending"]
    rep.note(f"rows: {len(base_rows)} -> {len(new_rows)}  "
             f"complete: {sum(1 for r in new_rows if r.get('status') == 'complete')}  "
             f"pending: {len(pending)}  dropped: {len(removed)}  "
             f"re-identified: {len(artifact.get('reidentified_rows', []))}")
    return rep


def _nonblank(raw: list[str]) -> list[str]:
    return [ln for ln in raw if ln.strip()]


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
    ap = argparse.ArgumentParser(
        epilog="Check 11 (live-content hygiene) is the one check that is NOT "
               "revision-pinned: it always reads the working-tree board of --repo "
               "(default: this checkout), because the pinned revision de9374c "
               "carries the over-escaping itself.")
    ap.add_argument("--repo", default=None)
    ap.add_argument("--base", default=BASE_COMMIT_DEFAULT)
    ap.add_argument("--rev", default=RECONCILED_REV_DEFAULT)
    ap.add_argument("--live", action="store_true",
                    help="check the working tree instead of the immutable revision")
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args()
    repo = Path(args.repo).resolve() if args.repo else Path(__file__).resolve().parent.parent
    try:
        rep = check_board(repo, base=args.base, rev=args.rev, live=args.live, quiet=args.quiet)
    except subprocess.CalledProcessError as exc:
        print(f"RESULT: FAIL (cannot read {exc.cmd[-1]!r} from git)")
        return 1
    except FileNotFoundError as exc:
        print(f"RESULT: FAIL ({exc})")
        return 1
    # reconciliation failures and live-content findings both fail the run; they
    # are counted separately because they describe different subjects (docstring)
    if rep.failures or rep.live_failures:
        print(f"\nRESULT: FAIL ({len(rep.failures) + len(rep.live_failures)} finding(s): "
              f"{len(rep.failures)} reconciliation, {len(rep.live_failures)} live-content)")
        for f in rep.failures + rep.live_failures:
            print(f"  - {f}")
        return 1
    print(f"\nRESULT: OK ({len(rep.checks)} checks passed)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
