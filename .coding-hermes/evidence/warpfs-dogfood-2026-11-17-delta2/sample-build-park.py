#!/usr/bin/env python3
"""Sample the local HEAD release build to distinguish SLOW from PARKED.

Reads /proc/<pid>/stat utime+stime (clock ticks) for the cargo process and each
of its descendant build-script / c++ children, waits SAMPLE_S, re-reads, and
prints one TSV row per pid with the delta. A build that is compiling consumes
ticks; a parked one consumes zero.

Read-only: no repo state is touched.
"""

import subprocess
import sys
import time

SAMPLE_S = 20


def children(pid):
    try:
        out = subprocess.run(
            ["pgrep", "-P", str(pid)], capture_output=True, text=True
        ).stdout
    except Exception:
        return []
    return [int(x) for x in out.split()]


def tree(pid, acc):
    for c in children(pid):
        acc.append(c)
        tree(c, acc)
    return acc


def ticks(pid):
    try:
        with open("/proc/%d/stat" % pid) as f:
            parts = f.read().split()
        return int(parts[13]) + int(parts[14])
    except Exception:
        return None


def comm(pid):
    try:
        with open("/proc/%d/comm" % pid) as f:
            return f.read().strip()
    except Exception:
        return "?"


def main():
    cargo = sys.argv[1] if len(sys.argv) > 1 else None
    if not cargo:
        print("usage: sample-build-park.py <cargo-pid>", file=sys.stderr)
        return 2
    cargo = int(cargo)
    pids = [cargo] + tree(cargo, [])
    before = {p: ticks(p) for p in pids}
    t0 = time.time()
    time.sleep(SAMPLE_S)
    elapsed = time.time() - t0
    print("sample_window_s\t%.1f" % elapsed)
    print("pid\tcomm\tcpu_ticks_before\tcpu_ticks_after\tdelta")
    total = 0
    for p in pids:
        b, a = before.get(p), ticks(p)
        if b is None or a is None:
            print("%d\t%s\tGONE\t-\t-" % (p, comm(p)))
            continue
        total += a - b
        print("%d\t%s\t%d\t%d\t%d" % (p, comm(p), b, a, a - b))
    print(
        "total_delta_ticks\t%d\t(sum across %d live pids in %.1fs)"
        % (total, len(pids), elapsed)
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
