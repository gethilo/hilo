#!/usr/bin/env python3
"""GAP-117 — re-score the retained Wave 11 impact outputs with the report rubric.

This is the Wave 11 rubric (`score_wave11_full.py`) applied to the raw outputs
the `wave11_impact_recall` integration test retains:

    <raw-dir>/<case-id>-review.json    # graph impact review set (this pipeline)
    <raw-dir>/<case-id>-legacy.json    # reverse-only compute_impact_at (pre-change)
    <raw-dir>/<case-id>-context.txt    # graph understand task-context output

Rubric (identical to the report):
    path_recall       = |set(hits) & set(truth_paths)|      / |set(truth_paths)|
    test_recall       = |set(hits) & set(test_paths)|       / |set(test_paths)|
    build_recall      = |set(hits) & set(test_build_paths)| / |set(test_build_paths)|
    a hit is a truth path appearing as a literal substring of the output text.

Run:
    python3 score_wave11_impact.py \
        --raw-dir "${CARGO_TARGET_DIR:-target}/wave11-impact"

Exits non-zero when the GAP-117 acceptance floors are not met.
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
BASELINE_MEAN_PATH_RECALL = 0.26666666666666666  # the report's measured hilo impact-tests mean
MIN_MEAN_PATH_RECALL = 0.50  # AC2
MIN_BUILD_PATH_RECALL = 2.0 / 3.0  # AC3 (2/3 eligible)


def hits(text: str, targets: list[str]) -> list[str]:
    return [t for t in targets if t and t in text]


def recall(found: list[str], targets: list[str]) -> float | None:
    if not targets:
        return None
    return len(set(found) & set(targets)) / len(set(targets))


def mean(values: list[float]) -> float:
    return sum(values) / len(values)


def mean_present(values: list[float | None]) -> tuple[float, int]:
    present = [v for v in values if v is not None]
    return (sum(present) / len(present), len(present)) if present else (0.0, 0)


def fmt(value: float | None) -> str:
    """`—` for an empty target set (the report's N/A cell), else 3 decimals."""
    return "—" if value is None else f"{value:.3f}"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--cases", default=str(HERE / "cases.jsonl"))
    ap.add_argument("--raw-dir", required=True)
    args = ap.parse_args()

    raw = Path(args.raw_dir)
    cases = [json.loads(line) for line in Path(args.cases).read_text().splitlines() if line.strip()]

    baseline, legacy, review, combined = [], [], [], []
    legacy_test, review_test = [], []
    legacy_build, review_build = [], []
    print(f"{'case':38} {'review':>7} {'legacy':>7} {'wave11':>7} {'test':>7} {'build':>7}")
    for case in cases:
        cid = case["id"]
        review_text = (raw / f"{cid}-review.json").read_text()
        legacy_text = (raw / f"{cid}-legacy.json").read_text()
        context_text = (raw / f"{cid}-context.txt").read_text()
        truth = case["truth_paths"]
        test_paths = case.get("test_paths", [])
        build_paths = case.get("test_build_paths", [])
        baseline_recall = recall(case.get("wave11_baseline_hits", []), truth)
        r_path = recall(hits(review_text, truth), truth)
        l_path = recall(hits(legacy_text, truth), truth)
        c_path = recall(hits(f"{review_text}\n{context_text}", truth), truth)
        r_test = recall(hits(review_text, test_paths), test_paths)
        l_test = recall(hits(legacy_text, test_paths), test_paths)
        r_build = recall(hits(review_text, build_paths), build_paths)
        l_build = recall(hits(legacy_text, build_paths), build_paths)
        print(
            f"{cid:38} {fmt(r_path):>7} {fmt(l_path):>7} "
            f"{fmt(baseline_recall):>7} {fmt(r_test):>7} {fmt(r_build):>7}"
        )
        baseline.append(baseline_recall if baseline_recall is not None else 0.0)
        legacy.append(l_path if l_path is not None else 0.0)
        review.append(r_path if r_path is not None else 0.0)
        combined.append(c_path if c_path is not None else 0.0)
        legacy_test.append(l_test)
        review_test.append(r_test)
        legacy_build.append(l_build)
        review_build.append(r_build)

    base_mean = mean(baseline)
    legacy_mean = mean(legacy)
    review_mean = mean(review)
    combined_mean = mean(combined)
    legacy_build_mean, eligible = mean_present(legacy_build)
    review_build_mean, review_eligible = mean_present(review_build)
    print()
    print(f"cases                          : {len(cases)}")
    print(f"wave11 baseline path recall    : {base_mean:.4f}  (report: 0.2667)")
    print(f"legacy (reverse-only) recall   : {legacy_mean:.4f}")
    print(f"review-set path recall         : {review_mean:.4f}  (AC2 floor {MIN_MEAN_PATH_RECALL})")
    print(f"impact + context path recall   : {combined_mean:.4f}  (AC2 floor {MIN_MEAN_PATH_RECALL})")
    print(f"legacy test-path recall        : {mean_present(legacy_test)[0]:.4f}")
    print(f"review test-path recall        : {mean_present(review_test)[0]:.4f}")
    print(f"eligible build-target cases    : {eligible} (legacy {legacy_build_mean:.4f})")
    print(f"review build-target recall     : {review_build_mean:.4f} over {review_eligible} eligible  (AC3 floor {MIN_BUILD_PATH_RECALL:.4f})")

    ok = True
    if abs(base_mean - BASELINE_MEAN_PATH_RECALL) > 1e-9:
        print("FAIL: locked baseline does not reproduce the report's 26.7%")
        ok = False
    if review_mean < MIN_MEAN_PATH_RECALL or combined_mean < MIN_MEAN_PATH_RECALL:
        print("FAIL: AC2 path-recall floor not met")
        ok = False
    if review_mean <= legacy_mean:
        print("FAIL: review set must beat the reverse-only BFS")
        ok = False
    if eligible != 3 or review_build_mean < MIN_BUILD_PATH_RECALL - 1e-9:
        print("FAIL: AC3 build-target floor not met")
        ok = False
    print("PASS" if ok else "FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
