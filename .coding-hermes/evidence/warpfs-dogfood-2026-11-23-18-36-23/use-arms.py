#!/usr/bin/env python3
"""Real-use timing arms for warpfs dogfood tick 2026-11-23-18-36-23.
Binary: /mnt/ssd/caches/hermes-target/release/hilo (v0.4.1-125-g675e6ed).
Corpus: /tmp/dftip-1123/heading (fresh clone of /home/kara/heading).
All timings via time.monotonic(); loadavg stamped per sample; stdout/stderr captured separately."""
import subprocess, time, os, sys, json

HILO = "/mnt/ssd/caches/hermes-target/release/hilo"
REPO = "/tmp/dftip-1123/heading"
OUT = "/tmp/dftip-1123/use-arms.log"
ERR = "/tmp/dftip-1123/use-arms.err"
JS = "/tmp/dftip-1123/use-arms.json"

results = []

def loadavg():
    return open("/proc/loadavg").read().split()[0:3]

def run(label, args, cwd=REPO, env_extra=None, timeout=600):
    env = dict(os.environ)
    if env_extra:
        env.update(env_extra)
    la = loadavg()
    t0 = time.monotonic()
    p = subprocess.run([HILO] + args, cwd=cwd, env=env,
                       stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=timeout)
    dt = time.monotonic() - t0
    entry = {"label": label, "args": " ".join(args), "rc": p.returncode,
             "wall_s": round(dt, 3), "loadavg": la,
             "stdout_lines": len(p.stdout.decode(errors="replace").splitlines()),
             "stderr_lines": len(p.stderr.decode(errors="replace").splitlines())}
    results.append(entry)
    with open(OUT, "a") as f:
        f.write(f"\n=== {label} | rc={p.returncode} | wall={dt:.3f}s | load={la} | cwd={cwd}\n")
        f.write(p.stdout.decode(errors="replace"))
    with open(ERR, "a") as f:
        f.write(f"\n=== {label} | rc={p.returncode} | wall={dt:.3f}s\n")
        f.write(p.stderr.decode(errors="replace"))
    print(json.dumps(entry))
    return p

# Floor
run("floor --version", ["--version"])
# A1 init on fresh clone
run("A1 init", ["init"], timeout=300)
# A2 cold graph warm with phase timing (PERF-016 check)
run("A2 cold graph warm HILO_WARM_TIMING=1", ["graph", "warm"], env_extra={"HILO_WARM_TIMING": "1"}, timeout=900)
# A3 graph stats right after warm (DF-WARPFS-43-R2 arm1: staleness claim; general census)
run("A3 graph stats post-warm", ["graph", "stats"], timeout=300)
# A4 warm arm: second graph warm
run("A4 warm graph warm (2nd run)", ["graph", "warm"], env_extra={"HILO_WARM_TIMING": "1"}, timeout=900)
# A5 documented query surface
run("A5 graph stats again", ["graph", "stats"], timeout=300)
run("A6 graph untested --granularity function", ["graph", "untested", "--granularity", "function"], timeout=300)
run("A7 graph impact <busy file>", ["graph", "impact", "package.json"], timeout=300)
run("A8 graph search 'handler'", ["graph", "search", "handler"], timeout=300)
run("A9 graph understand 'where is config loaded'", ["graph", "understand", "where is config loaded"], timeout=300)

with open(JS, "w") as f:
    json.dump(results, f, indent=1)
print("ARMS-DONE", len(results))
