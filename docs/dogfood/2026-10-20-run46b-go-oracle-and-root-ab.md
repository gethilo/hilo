# Dogfood run 46b — warpfs (Hilo) — second angle, same tick: a Go-consumer correctness oracle + the ancestor-root A/B — 2026-10-20

- **Verdict:** 🟢 **the headline promise holds on a real Go consumer repo** (blast radius
  **131/131 dependents found, 0 false dependents**, precision 1.00, file-level recall 1.00;
  warm queries 20–80 ms) / 🔴 **but the QUERY root is not the project you are standing in**: with
  the cwd's own cache cold, `graph impact` silently answers from an *ancestor* project's graph and
  exits 0.
- **Tick:** `warpfs-dogfood-2026-10-20-12-00-33` · **Target:** warpfs (`/home/kara/warpfs`) ·
  **HEAD:** `33ce3a24` (= `origin/master` at tick start) ·
  **Binary:** `/var/tmp/hermes-cargo-target/release/hilo`, build stamp
  `v0.3.0-530-g33ce3a24-dirty`, built `2026-10-20T10:32:47Z` (HEAD build; the `-dirty` is
  `.gitreins/logs` rot only). The PATH binary `/home/kara/.cargo/bin/hilo` is **stale**
  (`v0.3.0-293-g3adfb9ed`, 2026-10-08) and was **not** used.
- **Collision disclosure (this is the second angle of a duplicate fire).** A sibling instance of
  the SAME tick ran in this checkout and landed
  **`afd49c4e` "dogfood: run 46 — the agent-first map used as an agent, on a repo that is not
  Hilo (7 rows)"** (report `docs/dogfood/2026-10-20-run46-agent-map-realuse.md`, rows
  `DF-WARPFS-64-R2`, `GAP-104-R2`, `DF-WARPFS-158`…`162`, log entry, push) before this pass wrote
  anything. Its rows and files are **byte-untouched** here; its corpus was **task-router**
  (Python). This angle deliberately takes **surfaces that run did not drive**: a **Go** repo, the
  documented **cold-clone** claims, and the **root-selection** mechanism behind its own
  `DF-WARPFS-161`.

## The promise under test

> *"Give your AI coding agent a pre-built map of every codebase it touches — dependencies,
> entrypoints, test coverage, blast radius — without burning context window on file reads."*
> — `README.md:3`

**Corpus: a repo that genuinely consumes Hilo.** A fresh `git clone --no-hardlinks` of the fleet's
**crier** repo at `6f6a533` (595 `.go` files) — its `AGENTS.md` documents `.vfs/` as *"a LOCAL hilo
code-graph cache"*, `.vfs/manifest.yaml` is tracked, and `edges.jsonl` is deliberately
**gitignored** there (the other, equally documented inventory policy). This is the consumer shape
the product is sold for: an agent that already uses Hilo in anger.

The null hypothesis is not "does it run" but **"are the answers right — and do I know when they
are not?"**

## What worked: blast radius, scored against an independent oracle

Method: for each package, the oracle is the set of `.go` files whose source contains the exact
quoted import path (`grep` ground truth); `hilo`'s answer is the rows of
`hilo graph impact <file> --max-depth 3`. Scripts: `/tmp/df46-0714/oracle.py` and
`file_recall.py` (transcripts in the log entry's Evidence line). 6 packages, warm graph (8895
edges / 635 files discovered, 8692 distinct edges in the DB after reconcile):

| file (`hilo graph impact …`) | oracle importers | hilo `imports` rows | files in the answer | file-level recall | extra |
|---|---|---|---|---|---|
| `internal/guard/types.go` | 20 | 20 | 28 | 1.000 | 0 |
| `internal/relay/relay.go` | 6 | 6 | 9 | 1.000 | 0 |
| `config/config.go` | 36 | 35 | 39 | **1.000** | 0 |
| `internal/ratelimit/ratelimit.go` | 4 | 4 | 6 | 1.000 | 0 |
| `internal/namespace/namespace.go` | 30 | 30 | 40 | 1.000 | 0 |
| `internal/webhook/webhook.go` | 35 | 35 | 43 | 1.000 | 0 |
| **aggregate** | **131** | **130** | — | **131/131** | **0** |

- **Every one of the 131 oracle importers appears in the answer — 0 missing, 0 false
  dependents.** `hilo graph related <file> --direction reverse` returns the same file sets for the
  same packages; the two verbs agree.
- The one relation-level delta is honest to state: `config/config_test.go` is reported as
  **`tested_by`** (the name-based `heuristic conf=0.80` relation) rather than as the ast_exact
  `imports` row, even though `edges.jsonl` carries
  `{"from":"config/config_test.go","to":"pkg:github.com/crier-dev/crier/config","rel":"imports","provenance":"ast_exact","confidence":1.0}`
  (it also carries the heuristic `tested_by` edge to `config/config.go`). The dependent is never
  hidden, so this is a labelling nuance, not a miss — **no row was filed for it** (a
  non-deficiency nobody can feel is not a finding).
- Granularity, stated honestly by the tool itself: file-form impact answers at **package**
  scope (`via pkg:github.com/crier-dev/crier/internal/guard`), and the tool prints the caveat
  *"this total can exceed the direct importers of the file"*. For an agent deciding "what breaks
  if I touch this?", package scope is the right answer; for "who calls this function?", it is not
  — the caveat is the mitigation, and it is present on every such run.
- `--max-depth 1` returns **0 rows for every file** (the file→pkg hop is depth 1), while the
  documented README form `hilo graph impact <file>` (no depth flag) works. Not a defect — but
  `--max-depth 1` reading as "nothing depends on this" is a trap a user can walk into; the
  default is correct.

## What failed: the query's project root is not the project you are in

`docs/graph-engine.md` states the rule for the `surfaces` verb: *"The classification walks up from
the current directory the way `cargo` finds `Cargo.toml` … It stops at the first repository root
(a `.git` entry) that is not Hilo, so a foreign checkout nested under a Hilo tree classifies as
`foreign` rather than inheriting the ancestor's identity."* **The graph query verbs do inherit the
ancestor's identity.**

Minimal deterministic A/B — two Hilo projects, `outer` (warmed, has its own importer) and `inner`
nested inside it (own `.vfs/manifest.yaml`, valid, not yet warmed). The cwd tree's own ground
truth is 1 importer (`cmd/other/main.go`):

```
=== A) inner NOT warmed yet — ask the headline question ===
$ hilo graph impact internal/guard/types.go --max-depth 3
inner/internal/guard/types.go  (subject, scope=self)          <- path relative to OUTER
note: no dependents, dependencies, tests or build targets were found for
'inner/internal/guard/types.go'; the file itself is the only review row.
rc=0                                                          <- confidently empty, success exit

=== B) same cwd, same file, after ONE 'hilo graph warm' in the cwd ===
$ hilo graph impact internal/guard/types.go --max-depth 3
internal/guard/types.go  (subject, scope=self)                <- path relative to INNER now
cmd/other/main.go  ←  imports (depth: 2, scope=crate, via pkg:example.com/inner/internal/guard)
rc=0
```

Same binary, same file, same command; the only variable is whether the **cwd's own** cache
exists. In (A) the answer was computed from `outer`'s graph (proof: the subject is echoed
**relative to `outer`**, and the cwd's real importer is absent), and the user is told "no
dependents" with **rc=0**. Re-run script: `repro_root2.sh` (transcript `evidence/15-root-ab.txt`).

The same shape on a **real consumer repo**: a fresh clone of crier (own manifest, no inventory —
its policy gitignores `edges.jsonl`) sitting three levels under `/tmp`, where an old
`/tmp/.vfs` exists, answered `hilo graph impact internal/guard/types.go --max-depth 3` as
`df46-repro/cleanparent/clean-crier/internal/guard/types.go` — **relative to `/tmp`** — with
"no dependents", rc=0, while the same file's package has **20 importers** in the cwd's own
project (measured after warming that clone). The only hint is a warning that names the foreign
file:

```
warning: failed to parse /tmp/.vfs/manifest.yaml (IO error: stream did not contain valid UTF-8);
using default DuckDB settings
```

…and that hint exists **only because that ancestor manifest is corrupt**; with a valid ancestor
manifest (the A/B above) there is **no warning at all**. Meanwhile `hilo graph stats` in the very
same cwd reads the **cwd's** (empty) cache and prints
`Graph cache is empty. Query a file or run \`hilo graph warm\` to populate.` — so in one project
directory, two verbs of the same CLI disagree about which project's graph they are reading.

Severity: this is the *confidently wrong answer* class the project already forbids for
`graph surfaces` ("a refusal, never a success-shaped empty report"). It also compounds
`DF-WARPFS-64-R2` (no-warm ⇒ answers over a one-file on-demand graph): the fix for either must
decide the root **first** — the cwd's own manifest anchors it, and a query with no usable cache
should refuse rather than inherit an unrelated project's.

**Filed/updated:** `DF-WARPFS-161` already carries the *path-display* symptom at **P3 with no
acceptance criteria** (filed by this tick's first angle minutes earlier). This pass
**extended that row** (evidence + acceptance criteria + P2) rather than minting a duplicate:
the wrong-answer dimension, the A/B repro, and the mechanism (nearest warm ancestor wins over the
cwd's manifest) are what the foreman needs.

## Documented claims driven verbatim

| Documented claim | Drive | Result |
|---|---|---|
| `docs/getting-started.md` — on a clone of an already-initialized project `edges.jsonl` **is tracked** and `graph.db` is absent | clone of hilo's own repo (tracks `edges.jsonl`, 3758 edges) | Holds: `edges.jsonl` present, no `graph.db` |
| "queries self-heal … rebuild the missing `graph.db` from the tracked `edges.jsonl` on first open (read-through reconcile) … after a **short** rebuild pause" | first query in that clone, `time -p hilo graph stats` | Works: **5.51 s** one-shot, correct numbers (`Total edges: 3081 distinct / 3758 raw`), `graph.db` created, and a self-explaining line: `warning: reconcile checkpoint claims 1 cached edges but the graph db holds 0; rebuilding the cache from edges.jsonl (DF-WARPFS-61)` |
| README Quickstart: `hilo init` → `hilo graph warm` → `hilo classify` → `hilo graph stats/impact/related` | run in order on the crier clone | All work; `graph warm` **23.13 s** cold on 638 files (`Discovered 8895 edges across 635 files (4 languages)`); every documented command exits 0 |
| README: init/`graph warm` are the consumer path; `.vfs/graph/graph.db` is rebuildable, never committed | `git status` in the clone | Truthful — the derived cache is gitignored, the clone stays clean |

## Step 2b — measured cost (hyperfine 1.20.0, 15 runs, warmup 3, `-N`)

| Operation (crier clone @ HEAD, warm, 638 files / 8692 edges) | Result |
|---|---|
| `graph stats` | **37.3 ms ± 3.0** |
| `graph impact <file> --max-depth 3` | **45.6 ms ± 3.5** |
| `graph related <file> --relation imports` | **20.4 ms ± 0.9** |
| `graph search 'rate limit'` | **79.3 ms ± 3.0** |
| cold `graph warm` (one-shot, no inventory) | **23.13 s** |
| cold first query with a tracked `edges.jsonl` (one-shot, read-through reconcile) | **5.51 s** |

**No `PERF-<n>` row.** Nothing here is slow enough for a user to feel: every repeatable query is
20–80 ms against a "<1 s" promise, and the two one-shot costs are first-use only and disclosed by
the docs. Profiling them would produce a number nobody acts on — the honest report is that there
is nothing to file.

## Step 5 — install leg (ephemeral bunker): RUNNING, verdict PENDING — never a pass

The sibling angle already launched the leg on this HEAD and I **did not spawn a second agent**
(a second spawn re-measures the host and clobbers the first pass's evidence):

```
$ bunker-qa.sh status
BUNKER QA — 2026-10-20T12:20:46Z — server bunker-mvp
bunkerd=active docker=yes act=no uidmap=yes
$ bunker list --server bunker-mvp    # agents present
  df20-warpfs-install  running  2.7 GB  2026-10-20T12:07:47Z   (not mine — untouched)
  11d784da             running  2.5 GB  2026-10-20T12:12:24Z   ttl 4h   <- this leg
```

Cells collected from the agent at 12:20Z (`~/.qa-evidence.jsonl` on the agent): `repo-head` OK
(tested HEAD `33ce3a24`, pull skipped — dirty clone), `launch` OK (ttl 4h, script_bytes=123362),
`toolchain-bootstrap` OK ×4 (go 1.27.3, rust stable, gcc/g++, GNU Make 4.3) + INFO *"bare agent
missing system deps: openssl(libssl-dev) — rust build cells graded ENV-BLOCKED when they fail on
these (no sudo to apt install)"*. At 12:20Z `inst.log` (23.6 KB, growing) showed cargo mid-build
(`Compiling serde_yaml / hyper / arrow-* / openssl-sys`), i.e. **the from-source install is in
progress, not finished**.

**Verdict: `PARTIAL` — the leg RAN and no cell was graded; the fresh-install cell was still
building at tick close.** Closing evidence: `bc284ff3` "dogfood: run 46 close-out — install leg RAN
and is PARTIAL (fresh-install cell still building), no cell graded" (same tick, landed and pushed
while this angle ran; `bunker-qa.sh collect` was called 4× over 28 min and returned nothing new).
Runs 44/45 both lost this leg to a 45 m TTL before the release link finished; this one has a 4 h
TTL and a live build, so it may still land. The foreman/next tick must read
`bunker-qa.sh collect --evidence /tmp/bunker-qa-evidence-20261020T121154Z-2722236.jsonl` for the
final cells; **nothing in this tick may be read as "install verified"**.

## Left behind, and what this pass deliberately did NOT write

- `docs/dogfood/2026-10-20-run46b-go-oracle-and-root-ab.md` (this file) + the `dogfood-log.md`
  `run 46b` entry citing `afd49c4e`.
- `DF-WARPFS-161` **updated** (P3 → **P2**, note carrying the A/B transcript, the mechanism and
  explicit acceptance criteria). No new row was minted: the finding is the same defect the first
  angle filed, ~40 minutes earlier, and a second row would be a duplicate id-family sibling.
- **No code changed. No visibility/permission touched. No second bunker agent. The foreman was
  not woken** (scheduler state is not this lane's to mutate).
- The first angle's artifacts were left byte-untouched; its install agent `11d784da` was only
  read.
