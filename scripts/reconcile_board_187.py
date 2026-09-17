#!/usr/bin/env python3
"""One-shot BOARD-HYGIENE-001 (warpfs tick 187) board reconciliation.

Reconciles the warpfs .coding-hermes/board/tasks.jsonl JSONL board:

  A. "repeated updates" families (identical title + identical created_at, one row a
     strict evidence-subset of the other) collapse to ONE authoritative row that
     keeps the strongest evidence. Dropped rows are preserved verbatim in the audit
     artifact (.coding-hermes/board/reconciliation-187.json).
  B. Rows whose task id collided across QA foreman cycles but which describe
     DIFFERENT historical findings keep their finding and get a fresh unique id
     (never colliding with an existing QA id). The earliest row of each id keeps the
     original id and every re-identified row records original id/line/date in a
     `reconciliation` object (the original-row -> canonical-id mapping also lives in
     the artifact).
  C. Malformed fields are normalised conservatively: status "done" -> "complete"
     (only where completion evidence exists), free-form guard_result/ci_result ->
     the board vocabulary (unknown or in-flight CI => SKIP, never a guessed green),
     with the full original text retained in `<field>_note` and in the artifact.
  D. Character-array reasoning (a string that was appended to and then stored as
     list(str) + [note]) is restored to a list of readable segments so the appended
     closure note survives as its own element.

Unrelated rows are written back byte-identically. The script refuses to run unless
the board still matches the recorded pre-change hash.

stdlib only. python3 scripts/reconcile_board_187.py [--board DIR] [--check]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path

BASE_COMMIT = "71b333f630b8c42a0b885b6df923f46e2f718f4a"
BASE_COMMIT_SHORT = "71b333f"
TICK = "warpfs-2026-09-17-07-32-51"
TASK_ID = "BOARD-HYGIENE-001"
ARTIFACT_NAME = "reconciliation-187.json"

# Pre-change hash of tasks.jsonl at BASE_COMMIT (guard against double-apply).
EXPECTED_SHA256_BEFORE = None  # filled in from the artifact if present, else computed

# --- A. repeated-update families: drop the stale rows, keep the canonical one -----
# canonical_line -> [(dropped_line, reason), ...]
REPEAT_FAMILIES: dict[int, list[tuple[int, str]]] = {
    109: [(106,
           "stale pre-closure stub of the same finding (identical title + identical created_at "
           "2026-09-12T16:30:00Z); the canonical row carries the full closure evidence "
           "(commit eb0d143, worker/foreman summaries)")],
    128: [(117,
           "stale pre-closure stub of the same finding (identical title + identical created_at "
           "2026-09-14T16:10:00Z); superseded by the task_completed row (events.jsonl id=340)"),
          (127,
           "partial write of the same closure event, updated_at 2026-09-16T05:35:50Z - ten seconds "
           "before the authoritative row (05:36:00Z == events.jsonl task_completed id=340) and "
           "byte-identical to it except for the duplicated appended closure note")],
    130: [(121,
           "stale pre-closure stub of the same finding (identical title + identical created_at "
           "2026-09-15T15:54:29Z); its reasoning text is retained as the first element of the "
           "canonical row's restored reasoning")],
    131: [(123,
           "stale pre-closure stub of the same finding (identical title + identical created_at "
           "2026-09-15T16:58:45Z); reasoning text retained on the canonical row")],
    129: [(124,
           "stale pre-closure stub of the same finding (identical title + identical created_at "
           "2026-09-15T16:58:45Z); reasoning text retained on the canonical row")],
}

DROP: dict[int, tuple[int, str, str]] = {}  # built in main() once the rows are loaded

# --- B. collided ids that are DIFFERENT historical findings -> fresh unique ids ---
# line -> (new_id, note)
REIDENTIFY: dict[int, tuple[str, str]] = {
    73: ("QA-WARPFS-1-2",
         "same finding as QA-WARPFS-1 (bunker spawn cannot find server config under a sandboxed "
         "HOME); dated re-report 2026-09-04, resolved 2026-09-07. Kept for provenance - NOT a "
         "separate work item."),
    75: ("QA-WARPFS-1-3",
         "same finding as QA-WARPFS-1 (sandboxed-HOME server-config lookup); dated re-report "
         "2026-09-05, resolved 2026-09-07. Kept for provenance - NOT a separate work item."),
    78: ("QA-WARPFS-1-4",
         "distinct finding: bunker-las-03 host DNS SERVFAIL (get.docker.com) aborted agent spawn. "
         "Infra/bunker finding, unresolved at tick 187; not a warpfs repo defect."),
    82: ("QA-WARPFS-1-5",
         "distinct finding: bunker host ENOSPC (disk 95%) broke agent image extraction. "
         "Infra/bunker finding, unresolved at tick 187 (re-reported 2026-09-09 as QA-WARPFS-2-4)."),
    85: ("QA-WARPFS-1-6",
         "distinct finding: the qa-foreman cron prompt hardcodes bunker-las-03 which is absent from "
         "~/.bunker/config.yaml. Infra/tooling finding (not the warpfs repo), unresolved at tick 187."),
    92: ("QA-WARPFS-1-7",
         "distinct finding: bunker one-range port pool occupied, battery could not run. Same class as "
         "QA-WARPFS-6 (2026-09-10); infra finding, unresolved at tick 187."),
    76: ("QA-WARPFS-2-2",
         "distinct finding: our own tasks.jsonl carried double-escaped (unparseable) rows. Row text "
         "records the instance as REPAIRED 2026-09-07; left PENDING because no writer/root-cause fix "
         "is evidenced here (a tick-187 live parse of every line succeeds, which confirms the "
         "instance is gone but is not a writer fix)."),
    79: ("QA-WARPFS-2-3",
         "distinct finding: bunker spawn hard-depends on get.docker.com with no vendored installer or "
         "DNS fallback. Infra/harness finding, unresolved at tick 187 (re-reported 2026-09-09 as "
         "QA-WARPFS-5-2)."),
    86: ("QA-WARPFS-2-4",
         "dated re-report (2026-09-09) of the bunker host disk-exhaustion finding tracked by "
         "QA-WARPFS-1-5; kept for provenance - do not dispatch as separate work."),
    77: ("QA-WARPFS-3-2",
         "same finding as QA-WARPFS-2 (spawn stderr not captured in the evidence file); dated "
         "re-report 2026-09-05, resolved 2026-09-07. Kept for provenance - NOT a separate work item."),
    80: ("QA-WARPFS-3-3",
         "distinct finding: the filed QA-WARPFS-1 root cause was stale vs the current failure "
         "signature. Meta/board observation, unresolved at tick 187."),
    87: ("QA-WARPFS-3-4",
         "distinct finding: board audit note that the P1 blocker rows are environmental while only "
         "QA-WARPFS-4/5 were real project findings. Board observation, unresolved at tick 187."),
    88: ("QA-WARPFS-4-2",
         "distinct finding: the warpfs foreman tick 2026-09-09 18:13 failed at the duckbrain_write "
         "node (tick narration/report nodes skipped). Tooling finding (hermes-dagger lane), not the "
         "warpfs repo; unresolved at tick 187."),
    89: ("QA-WARPFS-5-2",
         "dated re-report (2026-09-09) of the get.docker.com hard-dependency finding tracked by "
         "QA-WARPFS-2-3; kept for provenance - do not dispatch as separate work."),
}

# --- C. malformed field corrections ---------------------------------------------
# line -> list of (field, new_value, why, note_field)
FIELD_FIXES: dict[int, list[tuple[str, str, str]]] = {
    94: [("ci_result", "SKIP",
          "run was in_progress at tick close - in-flight CI is recorded as SKIP, never as a "
          "guessed green")],
    95: [("ci_result", "SKIP",
          "run 34758404307 was in progress at Build and prior runs only 'succeeded' - in-flight "
          "CI is recorded as SKIP")],
    96: [("guard_result", "PASS",
          "explicitly states the gitreins commit guard Tier-1 legs passed ('secrets PASS, scoped "
          "tests leg PASS'); the non-blocking cppcheck timeout is not a guard failure"),
         ("ci_result", "SKIP",
          "the run for 488ee3d was in_progress at closure - SKIP, not a guessed green")],
    105: [("guard_result", "PASS",
           "all recorded gate counters are clean (fmt=0, workspace_check=0, 190+32 tests passed / "
           "0 failed, -D warnings compile=0)"),
          ("ci_result", "SKIP",
           "pre-push runs were green but the post-push runs were still pending at closure - SKIP")],
    112: [("ci_result", "GREEN",
           "explicit 'GREEN (run 34855848946, all steps success)'")],
    114: [("guard_result", "PASS",
           "explicit 'tier1 PASS'; the two tier-2 INCOMPLETE results were evaluator budget caps "
           "and are not guard-leg failures")],
    116: [("guard_result", "PASS",
           "explicit 'tier1 PASS' (tier-2 rerun was pending after the evaluator fix)")],
    122: [("ci_result", "SKIP",
           "no completed run is asserted for the pushed commits - SKIP rather than a guessed state")],
    126: [("ci_result", "SKIP",
           "the fix commit's run completed green but the board/config commits were still "
           "in_progress at tick end and INT-CI-005 warns the conclusion can misreport - SKIP")],
}

STATUS_FIXES: dict[int, tuple[str, str]] = {
    116: ("complete",
          "status 'done' was not in the board vocabulary; completion evidence is present "
          "(commit 2310e902, worker_summary, completed_at 2026-09-14 16:05:00, task_completed "
          "event 435 recording 'tier1 gitreins guard PASS at commit')"),
}

# --- D. char-array reasoning restoration ----------------------------------------
CHAR_ARRAY_LINES = (128, 129, 130, 131)

# --- restored completion evidence (sourced from events.jsonl, not invented) ------
COMPLETION_EVIDENCE: dict[int, dict[str, str]] = {
    128: {"completed_at": "2026-09-16T05:36:00Z", "commit_hash": "35ab89a",
          "note": "completed by the closure row written at 2026-09-16T05:36:00Z; events.jsonl "
                  "task_completed id=340 records fix_commit 35ab89a in repo "
                  "totalwindupflightsystems/gitreins (engine/llm.py LLMResponseError)"},
    129: {"completed_at": "2026-09-16T05:45:36Z", "commit_hash": "f2f1134",
          "note": "completed by the closure row written at 2026-09-16T05:45:36Z; events.jsonl "
                  "task_completed id=341 records commit f2f1134"},
    130: {"completed_at": "2026-09-16T06:01:37Z", "commit_hash": "0e3a004",
          "note": "completed by the closure row written at 2026-09-16T06:01:37Z; events.jsonl "
                  "task_completed id=342 records commit 0e3a004 (bankai wave)"},
    131: {"completed_at": "2026-09-16T06:01:37Z", "commit_hash": "0e3a004",
          "note": "completed by the closure row written at 2026-09-16T06:01:37Z; events.jsonl "
                  "task_completed id=343 records commit 0e3a004 (bankai wave)"},
    109: {"completed_at": "2026-09-13T05:32:00Z", "commit_hash": "eb0d1437d175b51b2d97e536646e0004d0b2caa3",
          "note": "completed by the closure row written 2026-09-13 00:32 (events.jsonl "
                  "task_dispatched 313 / task_completed 314 / audit 315, code_commit eb0d1437d175)"},
}

RULES = [
    "A. repeated updates to one task collapse to a single authoritative row that keeps the "
    "strongest evidence; dropped rows are preserved verbatim in removed_rows.",
    "B. a collided id whose later rows describe DIFFERENT historical findings keeps every finding: "
    "the earliest row of the id keeps the original id, each additional finding gets a fresh unique "
    "id recorded in reidentified_rows + reconciliation{original_id,original_line,original_ts}.",
    "C. malformed fields normalised conservatively: status done->complete only with completion "
    "evidence; free-form guard_result/ci_result mapped to the board vocabulary with unknown or "
    "in-flight CI => SKIP (never a guessed green); the full original text is retained in "
    "<field>_note and in field_corrections.",
    "D. character-array reasoning restored to a list of readable segments so the appended closure "
    "note survives as its own element (duplicated appended segments deduplicated).",
    "E. rows that are not listed in this artifact are written back byte-identically; events.jsonl, "
    "board.jsonl and fixtures.jsonl are not modified at all.",
]


def load_lines(path: Path) -> list[str]:
    return path.read_bytes().decode("utf-8").splitlines()


def style_of(raw: str) -> tuple[str, bool]:
    """Return (separators, ensure_ascii) matching the original line's formatting."""
    spaced = '"id": "' in raw or '": ' in raw
    seps = (", ", ": ") if spaced else (",", ":")
    ascii_only = all(ord(c) < 128 for c in raw)
    return seps, ascii_only


def dump_like(row: dict, raw: str) -> str:
    seps, ascii_only = style_of(raw)
    return json.dumps(row, ensure_ascii=ascii_only, separators=seps)


def is_char_array(v) -> bool:
    """True for a string that was exploded into chars, possibly with appended notes.

    The corrupt writer stored list(reasoning) + [note], so element 0 is a single
    character and later elements may be whole 1-char runs or appended note strings.
    """
    if not isinstance(v, list) or not v:
        return False
    if not all(isinstance(x, str) for x in v):
        return False
    return len(v[0]) == 1


def restore_reasoning(v: list) -> list[str]:
    """list(str) + [note] -> [text, note, ...] with duplicated segments removed."""
    segments: list[str] = []
    buf: list[str] = []
    for element in v:
        if isinstance(element, str) and len(element) == 1:
            buf.append(element)
            continue
        if buf:
            segments.append("".join(buf))
            buf = []
        segments.append(element if isinstance(element, str) else json.dumps(element))
    if buf:
        segments.append("".join(buf))
    out: list[str] = []
    for seg in [s for s in segments if s != ""]:
        if seg in out:  # the closure writer appended its note twice on this row
            continue
        out.append(seg)
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--board", default=None, help="board dir (default <repo>/.coding-hermes/board)")
    ap.add_argument("--check", action="store_true", help="report planned changes without writing")
    args = ap.parse_args()

    repo = Path(__file__).resolve().parent.parent
    board = Path(args.board) if args.board else repo / ".coding-hermes" / "board"
    tasks_path = board / "tasks.jsonl"
    artifact_path = board / ARTIFACT_NAME

    lines = load_lines(tasks_path)
    sha_before = hashlib.sha256(tasks_path.read_bytes()).hexdigest()

    if artifact_path.exists():
        prev = json.loads(artifact_path.read_text())
        expected = prev.get("files", {}).get("tasks.jsonl", {}).get("sha256_before")
        if expected and expected != sha_before:
            print(f"REFUSING: tasks.jsonl sha256 {sha_before} != recorded pre-change {expected} "
                  f"(board already reconciled?). Nothing written.")
            return 2

    rows: dict[int, dict] = {}
    raws: dict[int, str] = {}
    for n, raw in enumerate(lines, 1):
        rows[n] = json.loads(raw)
        raws[n] = raw

    global DROP
    DROP = {
        dropped: (canon, rows[canon]["id"], reason)
        for canon, entries in REPEAT_FAMILIES.items()
        for dropped, reason in entries
    }

    # sanity: the pre-change board must actually be the broken one
    ids = [rows[n]["id"] for n in rows]
    dup = sorted({i for i in ids if ids.count(i) > 1})
    if not dup:
        print("REFUSING: no duplicate ids found - board does not look like the pre-change state.")
        return 2

    artifact: dict = {
        "artifact": ARTIFACT_NAME,
        "task": TASK_ID,
        "tick": TICK,
        "base_commit": BASE_COMMIT,
        "base_commit_short": BASE_COMMIT_SHORT,
        "board": ".coding-hermes/board",
        "files": {
            "tasks.jsonl": {
                "sha256_before": sha_before,
                "lines_before": len(lines),
                "line_count_before": len(lines),
            },
            "events.jsonl": {"modified": False},
            "board.jsonl": {"modified": False},
            "fixtures.jsonl": {"modified": False},
        },
        "duplicate_ids_before": dup,
        "rules": RULES,
        "removed_rows": [],
        "reidentified_rows": [],
        "field_corrections": [],
        "reasoning_restorations": [],
        "mapping": {},
    }

    new_rows: dict[int, dict] = {}
    changed_lines: set[int] = set()

    for n, row in rows.items():
        new_rows[n] = dict(row)

    # ---- A. drop stale repeats --------------------------------------------------
    for n, (canon_line, canonical_id, why) in sorted(DROP.items()):
        dropped = rows[n]
        artifact["removed_rows"].append({
            "original_line": n,
            "original_id": dropped.get("id"),
            "original_status": dropped.get("status"),
            "original_ts": dropped.get("ts"),
            "original_created_at": dropped.get("created_at"),
            "canonical_line": canon_line,
            "canonical_id": canonical_id,
            "reason": why,
            "raw": raws[n],
        })
        artifact["mapping"].setdefault(dropped.get("id"), []).append(
            {"original_line": n, "canonical_id": canonical_id, "action": "collapsed"}
        )

    # ---- B. re-identify collided distinct findings -------------------------------
    for n, (new_id, note) in sorted(REIDENTIFY.items()):
        row = new_rows[n]
        old_id = rows[n]["id"]
        row["id"] = new_id
        row["reconciliation"] = {
            "action": "reidentified",
            "original_id": old_id,
            "original_line": n,
            "original_ts": rows[n].get("ts"),
            "original_status": rows[n].get("status"),
            "base_commit": BASE_COMMIT_SHORT,
            "artifact": f".coding-hermes/board/{ARTIFACT_NAME}",
            "note": note,
        }
        artifact["reidentified_rows"].append({
            "original_line": n,
            "original_id": old_id,
            "new_id": new_id,
            "original_status": rows[n].get("status"),
            "original_ts": rows[n].get("ts"),
            "original_title": rows[n].get("title"),
            "note": note,
            "raw": raws[n],
        })
        artifact["mapping"].setdefault(old_id, []).append(
            {"original_line": n, "canonical_id": new_id}
        )
        changed_lines.add(n)

    # ---- C. field + status fixes -------------------------------------------------
    for n, fixes in sorted(FIELD_FIXES.items()):
        row = new_rows[n]
        for field, new_value, why in fixes:
            old_value = row.get(field)
            row[field] = new_value
            row[f"{field}_note"] = (
                f"{old_value}  [BOARD-HYGIENE-001 tick 187: normalised to {new_value}; "
                f"original free-form text retained here verbatim. Rationale: {why}.]"
            )
            artifact["field_corrections"].append({
                "original_line": n,
                "id": row.get("id"),
                "field": field,
                "from": old_value,
                "to": new_value,
                "why": why,
            })
        changed_lines.add(n)

    for n, (new_status, why) in sorted(STATUS_FIXES.items()):
        row = new_rows[n]
        old_status = row.get("status")
        row["status"] = new_status
        row["reconciliation"] = {
            "action": "status_normalized",
            "original_status": old_status,
            "original_line": n,
            "base_commit": BASE_COMMIT_SHORT,
            "artifact": f".coding-hermes/board/{ARTIFACT_NAME}",
            "note": why,
        }
        artifact["field_corrections"].append({
            "original_line": n,
            "id": row.get("id"),
            "field": "status",
            "from": old_status,
            "to": new_status,
            "why": why,
        })
        changed_lines.add(n)

    # ---- D. reasoning restoration ------------------------------------------------
    for n in CHAR_ARRAY_LINES:
        row = new_rows[n]
        if not is_char_array(row.get("reasoning")):
            print(f"NOTE: line {n} reasoning is not a character array - left as is")
            continue
        before = json.dumps(row["reasoning"], ensure_ascii=False)
        row["reasoning"] = restore_reasoning(row["reasoning"])
        artifact["reasoning_restorations"].append({
            "original_line": n,
            "id": row.get("id"),
            "segments_restored": len(row["reasoning"]),
            "segment_lengths": [len(s) for s in row["reasoning"]],
            "original_char_count": len(before),
            "restored": row["reasoning"],
        })
        changed_lines.add(n)

    # ---- completion evidence (sourced from events.jsonl) -------------------------
    for n, ev in sorted(COMPLETION_EVIDENCE.items()):
        row = new_rows[n]
        if row.get("completed_at") is None:
            row["completed_at"] = ev["completed_at"]
            changed_lines.add(n)
        if ev["commit_hash"] and not row.get("commit_hash"):
            row["commit_hash"] = ev["commit_hash"]
            changed_lines.add(n)
        existing = row.get("foreman_note") or ""
        row["foreman_note"] = (
            (existing + " | " if existing else "")
            + f"BOARD-HYGIENE-001 tick 187 (base {BASE_COMMIT_SHORT}): {ev['note']}."
        )
        changed_lines.add(n)
        artifact.setdefault("completion_evidence", []).append({
            "line": n, "id": row.get("id"), "completed_at": row.get("completed_at"),
            "commit_hash": row.get("commit_hash"), "note": ev["note"],
        })

    # ---- canonical rows absorb the dropped siblings (documented) ------------------
    for n, (canon_line, canonical_id, _why) in sorted(DROP.items()):
        row = new_rows[canon_line]
        dropped_lines = sorted(
            k for k, (cl, _cid, _r) in DROP.items() if cl == canon_line
        )
        row["reconciliation"] = {
            "action": "repeated_updates_collapsed",
            "absorbed_lines": dropped_lines,
            "absorbed_original_statuses": [rows[k].get("status") for k in dropped_lines],
            "base_commit": BASE_COMMIT_SHORT,
            "artifact": f".coding-hermes/board/{ARTIFACT_NAME}",
            "note": "authoritative row for repeated updates to this task; superseded rows are "
                    "preserved verbatim in the audit artifact (removed_rows).",
        }
        changed_lines.add(canon_line)

    # ---- write -------------------------------------------------------------------
    if not args.check:
        artifact["files"]["tasks.jsonl"]["lines_after"] = len(lines) - len(DROP)
        artifact["files"]["tasks.jsonl"]["line_count_after"] = len(lines) - len(DROP)
        artifact["changed_lines"] = sorted(changed_lines)
        artifact["dropped_lines"] = sorted(DROP)
        artifact["summary"] = {
            "duplicate_ids_before": len(dup),
            "rows_removed": len(DROP),
            "rows_reidentified": len(REIDENTIFY),
            "field_corrections": len(artifact["field_corrections"]),
            "reasoning_restorations": len(artifact["reasoning_restorations"]),
        }

        out: list[str] = []
        for n, raw in enumerate(lines, 1):
            if n in DROP:
                continue
            if n in changed_lines:
                out.append(dump_like(new_rows[n], raw))
            else:
                out.append(raw)  # byte-identical for untouched rows
        tasks_path.write_text("\n".join(out) + "\n", encoding="utf-8")
        artifact["files"]["tasks.jsonl"]["sha256_after"] = hashlib.sha256(
            tasks_path.read_bytes()
        ).hexdigest()
        artifact_path.write_text(json.dumps(artifact, ensure_ascii=False, indent=2) + "\n",
                                 encoding="utf-8")

    print(f"rows: {len(lines)} -> {len(lines) - len(DROP)}  changed: {len(changed_lines)}  "
          f"dropped: {len(DROP)}  re-identified: {len(REIDENTIFY)}  "
          f"field_fixes: {len(artifact['field_corrections'])}")
    for n in sorted(changed_lines):
        print(f"  line {n}: {rows[n].get('id')} -> {new_rows[n].get('id')}")
    if args.check:
        print("(check mode - nothing written)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
