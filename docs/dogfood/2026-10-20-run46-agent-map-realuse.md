# Dogfood run 46 — warpfs (Hilo) — the agent-first map, used as an agent — 2026-10-20

- **Verdict:** 🟡 **PROMISING-BUT-ROUGH** — the map is real, fast and *exactly right* on the flat/normal
  shapes; it fails **silently and confidently** on the shapes a script-collection repo is made of, and
  `hilo init` reaches outside the repo into the host's shared git hooks.
- **Tick:** `warpfs-dogfood-2026-10-20-12-00-33` · **Target:** warpfs (`/home/kara/warpfs`) ·
  **HEAD at the time:** `33ce3a24` (= `origin/master`) ·
  **Binary:** `/var/tmp/hermes-cargo-target/release/hilo`, build stamp `v0.3.0-530-g33ce3a24-dirty`,
  built `2026-10-20T10:32:47Z` (HEAD build; the `-dirty` is the `.gitreins/logs` rot, the only dirty
  path — verified `git status --porcelain` minus that path is empty).
- **Not used:** the PATH binary `/home/kara/.cargo/bin/hilo` — **stale** (`v0.3.0-293-g3adfb9ed`,
  built `2026-10-08`). Read `--version`'s build stamp before believing any timing here (run-45 lesson).
- **Ground-truth corpus:** a fresh `git clone --local` of the **task-router** repo at `d643ef4`
  (730 tracked files, **314 `.py`**) into scratch scratch dirs `/tmp/df46-1206/tr`, `/tmp/df46-1206/cold/trc`
  and `/var/tmp/df46b/trb`. A real consumer repo, not Hilo itself, and no `.vfs` existed in it before this run.

## The promise under test (Step 1)

> *"Give your AI coding agent a pre-built map of every codebase it touches — dependencies, entrypoints,
> test coverage, blast radius — without burning context window on file reads."* — README.md:3

So: **an agent (me) points hilo at a repo it does not know, asks the four promised questions, and acts
on the answers.** The null hypothesis is not "does it run"; it is **"are the answers right, and do I know
when they are not?"** Everything below is a real ask on the task-router corpus, not a test script.

## What worked (and worked well)

| Ask (real user form) | Command | Result |
|---|---|---|
| Build the map | `hilo init` + `hilo graph warm` | **8.28 s** for 314 files → 3337 edges, 304/314 files contribute edges, 10 no-imports, 0 unreadable (`coverage.json`). No config, no prompts. |
| "What imports this?" | `hilo graph related scripts/router_server.py` | correct import list incl. external packages, 21 ms |
| "What breaks if I change this?" | `hilo graph impact scripts/router_server.py` | **41 dependents, and an exact set match**: grep for a real `import router_server` statement yields the same 41 files — 0 extra, 0 missed (41 in hilo \ grep-imports = ∅; 41 in grep-imports \ hilo = ∅). The 19 files that merely *mention* the name are correctly not claimed. |
| "Where is X?" | `hilo graph search "circuit breaker"` / `"provider health probe"` | first hit is the right file in both cases (`scripts/router_circuit.py`, `scripts/provider_health_probe.py`); 58 ms mean |
| "Classify the tree" | `hilo classify --dry-run -v` | 175 test / 98 entrypoint / 32 library / 9 unknown — spot-checked entries are correct |
| "Audit the symbols" | `hilo graph audit` | 948 public fns over 314 files; unreachable 8 / unlinked 354 / ok 586, rules printed |
| Error UX when there is truly no graph | `hilo graph search` (first command, no warm) | `error: No graph data. Run \`hilo graph warm\` first.` — exactly right (see F1: it is the only command that does this) |

The headline workflow is fast enough that a user never waits: **cold one-shot warm 8.28 s, then every
query 21–196 ms** (hyperfine, 10 runs, warmup 3 — search 57.8 ± 1.9 ms, impact 54.9 ± 5.5 ms,
related 21.3 ± 1.3 ms, stats 41.4 ± 3.8 ms, understand 195.3 ± 27.4 ms).

## Findings (all filed on the board this tick)

### F1 — `DF-WARPFS-64-R2` (P1) — with no `graph warm`, four commands answer confidently over a one-file graph, rc=0

PRISTINE tree (clone + `hilo init`, **no warm**), first command is the impact query:

```
$ hilo graph impact scripts/router_server.py      # rc=0
scripts/router_server.py  (subject, scope=self)
note: no dependents, dependencies, tests or build targets were found for
'scripts/router_server.py'; the file itself is the only review row.
$ hilo graph untested                             # rc=0  -> 1 untested file(s)
$ hilo graph stats                                # rc=0  -> Total edges: 38 / Total files: 1
$ hilo graph module scripts                       # rc=0  -> Files: 1  Edges: 38  Tests: 0.0%
```

Warmed, the same four commands report 41 dependents / 61 untested files / the 314-file corpus. The
graph the answers were computed over contains exactly the one file the query itself parsed on demand.
`graph search` — the one command with a guard — refuses with the warm hint. A CI guard or an agent that
automates on "no dependents" or "untested" is being told a lie with a success exit code, which is the
same class the project's own docs forbid for the foreign-scope case (`docs/graph-engine.md`: *"a refusal,
never a success-shaped empty report"*).

### F2 — `GAP-104-R2` (P1) — blast radius and coverage go wrong together for a subdirectory module imported by basename

```
$ hilo graph impact scripts/drivers/base.py       # rc=0
scripts/drivers/base.py  (subject, scope=self)
note: no dependents, dependencies, tests or build targets were found ...
$ grep -rn 'from base import\|import base' scripts/drivers/*.py | wc -l    # 6 real importers
$ hilo graph impact pkg:base                      # the same 6, depth 1, scope=crate, ast_exact
$ grep -c '"to":"pkg:base"' .vfs/graph/edges.jsonl                         # 6 edges exist
```

The edges exist and the pkg-form query returns them; the **file-form** query (the form a user has — they
hold a path) cannot join its own file node to the pkg node its importers target. Contrast the flat module
on the same corpus, `scripts/router_server.py`, whose file-form impact is exactly right (F-table above):
the failure is specific to a module whose import site is the **subdirectory** (siblings and tests put
`scripts/drivers` on `sys.path` and `import base` by basename). Same root cause on the coverage surface:
`graph untested` lists `scripts/drivers/hermes.py`, `pi.py`, `deepseek_harness.py` as having no test
coverage although `tests/test_proxy_driver_hermes.py` does `import hermes as h`, etc. —
and `graph stats` shows `base.py` in its Orphans list.
This is GAP-104's shape (file-form impact empty though the edges exist) in the subdirectory/path-entry
case, and the same file→pkg identity family as GAP-064 / GAP-066 / DF-WARPFS-78 / DF-WARPFS-84.

### F3 — `DF-WARPFS-158` (P2) — `graph understand` spends the budget on the tier that is not context, and starves the one that is

```
$ hilo graph understand 'add a provider health probe that reads provider health state'
## DETAIL
(no files in this tier) — no detail block fits the 14400-char budget
```
27,340 bytes / 635 lines emitted, **no file context at all**; the MAP tier is a list of truncated symbol
names. And `--budget` does not bound the response: `--budget 1000` → 27,339 B, `--budget 6000` (the
documented default) → 27,340 B, `--budget 20000` → 74,752 B (DETAIL appears), `--budget 60000` →
170,411 B. Hot path read from source: `hilo-graph/src/signal.rs:2635` (`detail_budget_chars =
token_budget * 4 * 60 / 100`) and `:2647` — **the first block that does not fit `break`s the loop**, so one
oversized candidate empties the whole tier; the MAP (`:~2595`) and SIGNATURES tiers emit every matched
file with no char accounting, so the response overshoots the stated budget before DETAIL is considered.

### F4 — `DF-WARPFS-159` (P2) — the COV coverage chain is unreachable on any consumer repo

`hilo graph surfaces` refuses by design on a non-Hilo tree (rc=1, `scope: foreign`), and
`coverage-links --unlinked`, `test-classes` and `rollup` then **hard-fail rc=1** with
*"no surface inventory … run `hilo graph surfaces` first (COV-1)"* — instructing the user to run a command
that refuses. On a foreign repo the reachable coverage surfaces are only `untested`, `module`, `audit`,
`wiring`; `wiring` degrades gracefully in the same tree (prints the absence note *and* a report), which
proves the chain can be honest. README advertises test coverage for every codebase; four of its eight
coverage commands return nothing there.

### F5 — `DF-WARPFS-160` (P3) — `untested` is a definition the docs never state

61 files listed; cross-checking every `tests/**/*.py`: **3 imported directly by a test** (a genuine
defect — F2), **37 never imported but named/invoked by a test** (e.g. `scripts/router_circuit.py` is
exercised by `tests/test_circuit_v2.py`, which imports `subprocess` and runs the script by path), and
only **21** with no mention anywhere under `tests/`. `tested_by` is emitted only from import statements
(1165 rows, all import-derived), so "no test coverage" really means "not imported by a test".

### F6 — `DF-WARPFS-162` (P1) — `hilo init` writes into the host's SHARED git hooks, and keeps no backup of what it replaced

The most consequential thing found this run, and it is not about the graph:

```
$ cd /tmp/df46-1206/tr && hilo init
Initialized Hilo in /tmp/df46-1206/tr
  Updated Hilo block in post-commit
  Updated Hilo block in post-merge
Installed git hooks: post-commit, post-merge
$ ls .git/hooks/post-commit
ls: cannot access '.git/hooks/post-commit': No such file or directory
$ git rev-parse --git-path hooks/post-commit
/home/kara/.hermes/hooks/post-commit          # core.hooksPath is set GLOBALLY on this host
$ stat -c '%s %y' /home/kara/.hermes/hooks/post-commit
450 2026-10-20 07:11:24 -0500                # 410 bytes at tick start — moved by this init
```

`init` resolves `core.hooksPath` and installs its block there, so initialising a **throwaway clone**
mutated host-wide hook state that **every repo on the box** (including the fleet's gitreins chain) reads,
and every commit anywhere now runs `hilo graph warm --changed`. Worse, the update is not an append: the
current file minus hilo's 149-byte block is 301 bytes against a pre-tick 410, i.e. ~109 bytes of previous
content were **replaced**, and no backup was written (gitreins writes `.bak-<ts>` next to the hook it
touches; hilo writes none) — so the pre-image is unrecoverable. Sibling hooks on this host
(`crier/.git/hooks/post-commit` 295 B, `duckbrain/...` 305 B) still carry an **older block variant** with an
`else` branch that writes `.vfs/.dirty`, so this is a recurring rewrite, not a one-off.

**Remediation and disclosure:** the block was already present before this tick (a HILO block was in the
410-byte pre-tick file), so the current block was left in place — removing it would change host behaviour
more than the update did. A pre-tick byte snapshot was NOT taken before running init, so the +40-byte delta
cannot be reconstructed; the current and candidate-stripped copies are preserved in
`/tmp/df46-1206/hooks-before/`. This is a footprint I created by using the product the documented way.

### F7 — `DF-WARPFS-161` (P3) — a stray ancestor `.vfs` silently becomes the workspace root

With `/tmp/.vfs` present (left by an earlier run; its `manifest.yaml` is 200 bytes of non-UTF-8 binary),
the first command in a fresh tree under `/tmp` printed
`warning: failed to parse /tmp/.vfs/manifest.yaml (IO error: stream did not contain valid UTF-8)`
— a file the user never created — and echoed the subject path as
`df46-1206/cold/trc/scripts/router_server.py`, i.e. **relative to the mis-resolved root `/tmp`**, so the
path in the answer does not exist on disk. The same clone+init under `/var/tmp` (no ancestor `.vfs`)
prints clean relative paths. Related to DF-WARPFS-55 (fixed for pkg-node expansion from CWD), not the
same: this is root **discovery**.

## Step 2b — performance (measured, and what it means)

| Operation (task-router@d643ef4, 314 `.py`, warm) | Command | Result |
|---|---|---|
| Build the map (cold, one-shot) | `/usr/bin/time -p hilo graph warm` | **real 8.28 s**, user 12.39 s, sys 0.83 s |
| search | `hyperfine --warmup 3 --runs 10` | **57.8 ms ± 1.9 ms** |
| impact | ″ | 54.9 ms ± 5.5 ms |
| related | ″ | 21.3 ms ± 1.3 ms |
| stats | ″ | 41.4 ms ± 3.8 ms |
| understand | ″ | 195.3 ms ± 27.4 ms |
| Cold, before any warm: `search` | one-shot | refuses in 0.00 s with the warm hint |
| Cold, before any warm: `impact` | one-shot | 0.02 s, confident-empty (F1) |

**No `PERF-<n>` row is filed: nothing here is slow enough for a user to feel.** One measurement per
operation, and the one number that could hide a regression was checked directly instead of profiled —
see the DF-WARPFS-76 discrimination below.

### DF-WARPFS-76's premise, discriminated (evidence added; the row is NOT re-filed)

`DF-WARPFS-76` (P0, pending) says *"`hilo graph search` re-reads and tree-sitter re-parses EVERY source
file on EVERY invocation"*. At HEAD:

```
$ strace -f -e trace=openat -o s.txt hilo graph search 'provider health'
openat calls=356 | *.py opens=314 | edges.jsonl opens=0 | cache opens=11
$ strace -f -e trace=openat -o i.txt hilo graph impact scripts/router_server.py
*.py opens=0 | openat calls=39
```

So **the re-READ half is live (exactly 314 `O_RDONLY` opens of the corpus, every invocation) and the
re-PARSE half is not** — the symbols/parse caches are consulted (11 cache opens) and the command completes
in 57.8 ms, where a real re-parse of this corpus costs 8.28 s. The user-visible cost of the surviving
re-read is ~0.06 s, i.e. below the threshold of notice. A board event records this discrimination so the
P0 row can be re-scoped (or closed) on evidence rather than re-filed.

## Step 5 — install leg (fresh machine, ephemeral bunker)

**ATTEMPTED and RUNNING at report time — recorded as `PARTIAL`/`SKIPPED-install` unless a later entry in
`.coding-hermes/dogfood-log.md` completes it.** `bunker-qa.sh` (`bunkerd=active docker=yes`, server
`bunker-mvp`) was launched against this repo:

```
$ bunker-qa.sh status
BUNKER QA — 2026-10-20T12:11:46Z — server bunker-mvp
bunkerd=active docker=yes act=no uidmap=yes
$ bunker-qa.sh launch /home/kara/warpfs --keep
→ QA HEAD: 33ce3a245cd9fcd83be11f3fdba68de7a0d9a20c (clone dirty — pull skipped, testing recorded sha)
→ agent=11d784da @ bunker-mvp (tailscale) ttl 4h
→ CELL fresh-install: sync repo → ~/qa-warpfs
→ CELL upgrade-prep: previous release tree v0.3.0 → ~/qa-up-uprev
LAUNCHED  evidence=/tmp/bunker-qa-evidence-20261020T121154Z-2722236.jsonl
```

Evidence rows present: `repo-head` OK (tested HEAD recorded), `launch` OK (script_bytes=123362). The
battery is a 4 h TTL agent this time (the 45 m TTL that reaped runs 44/45 is not the limit here).

**FINAL STATE AT TICK CLOSE (12:40Z): PARTIAL — the leg RAN, no cell was graded.** Four `collect` calls
over 28 min each returned `RUNNING agent=11d784da … agent left intact`; read directly over
`bunker exec 11d784da --server bunker-mvp -- …`, `~/qa-warpfs` was synced (mtime 12:14),
`~/qa-run.log` still carried only the `QA-HEAD` line, **two `cargo` processes were live** (etimes 11:15
and 06:41), `~/qa-warpfs/target` grew 712 MB → 718 MB, an `act` process appeared at 12:35 — and the shared
host's root FS read **133/150 GB (12 GB free, 93 %)**, the QA-WARPFS-025 disk-pressure shape, while the
build target was still growing. So: fresh-install cell IN PROGRESS, `ci-pass` / native suite / 4 chaos
cells not reached; **no install PASS is claimed, and no `SKIPPED-install-bunker` row was filed — the leg
did not fail to run.** Finish it with
`bunker-qa.sh collect --evidence /tmp/bunker-qa-evidence-20261020T121154Z-2722236.jsonl` (idempotent;
destroys the agent on `CELLS-DONE`); manual cleanup `bunker destroy 11d784da --server bunker-mvp`.
Full ladder + event 918 in `.coding-hermes/dogfood-log.md`.

## Left behind

- Board rows: `DF-WARPFS-64-R2`, `GAP-104-R2`, `DF-WARPFS-158`, `DF-WARPFS-159`, `DF-WARPFS-160`,
  `DF-WARPFS-161`, `DF-WARPFS-162` (all `pending`, verified 1 occurrence each; `boardctl validate` OK).
- `docs/dogfood/2026-10-20-run46-agent-map-realuse.md` (this file), the `dogfood-log.md` entry,
  `diagnostics.md` and `skills/hilo-usage/SKILL.md` updates.
- Raw transcripts for every number above: `/tmp/df46-1206/{times.txt,out/}`, `/tmp/df46-1206/*.py`
  probes, `/tmp/df46-1206/hooks-before/` (hook snapshots). Scratch is ephemeral; the report is the record.
- **No code changed. No repo visibility or permission touched. The foreman was not woken (read-only lane
  contract for scheduler state).**
