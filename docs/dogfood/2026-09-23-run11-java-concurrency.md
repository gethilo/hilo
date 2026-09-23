# Dogfood Run 11 — Java Corpus + Concurrency + Triggers-unmount (2026-09-23)

**Tick:** warpfs-dogfood (run 11) ·
**HEAD tested:** v0.3.0-28-g740f0d9 (local release binary `hilo 0.3.1-dev`, build stamp
`v0.3.0-14-g2b26d3f`, 2026-09-22T23:19Z) · **Corpus:** google/gson @ depth-1 clone
(264 `.java` files, 243 with edges) — the first **Java** corpus in 11 runs.
**Verdict: 🟡 PROMISING-BUT-ROUGH for the Java surface; ✅ SHIPPABLE for the Rust/Go
core paths re-verified this run; 🔴 for concurrent access** (every parallel
`hilo` invocation after the first fails).

## The promise tested this run

"An agent can answer structural questions about any codebase (dependencies,
entrypoints, test coverage, blast radius) by querying a pre-computed metadata
graph — in <1s, without reading files." 11 runs have covered the CLI/graph
surface (1–6), the FUSE mount (7), S3 backends + FFI ABI (8), git backend +
plugins (9), triggers + permissions (10). This run took the surfaces those
runs did **not** exercise:

1. **A language never tested before** — Java (runs 1–6 did Rust ×2, Go, Python,
   TS/JS; the per-language series stopped there). `README`/`graph-engine.md`
   advertise 26 languages.
2. **Concurrency** — the same graph queried by more than one process at a time,
   which is the actual working mode of an "agent-first" tool (an MCP server plus
   a CLI, or two agents).
3. **The FFI consumer path** — run 8 proved the UniFFI ABI is *exported*
   (`nm -D`) but never generated the documented bindings nor called the library
   from a real non-Rust process.

## What held up (Java, real use)

| Step | Result |
|---|---|
| `hilo init` | hooks installed (`post-commit`, `post-merge`), `.vfs/` created, 12 ms |
| `hilo graph warm` (cold) | **12.0 s** for 264 files → 4418 edges, coverage line complete |
| `hilo graph warm` (incremental) | **29 ms**, `[all cached, graph unchanged]` |
| `hilo graph warm` after an import change, via the installed `post-commit` hook | fired on commit, re-parsed 1 file, appended the new edge (4418 → 4419) — the "living inventory" hook works on Java |
| `hilo graph impact pkg:com.google.gson.Gson` | **112 dependents — exactly the grep ground truth** (112 files importing `com.google.gson.Gson`), all `ast_exact conf=1.00` |
| MCP over stdio | `initialize` clean (`serverInfo 0.3.1-dev`), `vfs_graph_impact` = 112, `vfs_graph_stats` = live numbers |
| `hilo graph search TypeAdapter` | lexical hits with symbol tags |
| `hilo meta --set/--value` + `getfattr` | xattr round-trip byte-exact (`user.vfs.note`), visible in `hilo meta <path>` |
| `hilo classify` | 264 scanned, roles written as xattrs; `gson/src/test/**` correctly `role=test` |
| FUSE mount (plain `--daemon`) + `fusermount3 -u` | up in ~1 s, EROFS by design, clean unmount, process exits — run 7's behaviour still holds |

## Findings

### DF-WARPFS-32 (P1) — a `--triggers` mount leaks its daemon and holds the graph.db lock after unmount

Reproducible 2/2, and the release blocker for the trigger workflow:

```console
$ hilo mount /tmp/wf11/mnt-lock --triggers --daemon
$ fusermount3 -u /tmp/wf11/mnt-lock          # succeeds, rc=0
t+3s  process_alive=yes stats LOCKED
t+26s process_alive=yes stats LOCKED         # still alive; had to kill it
```

A **plain** `--daemon` mount on the same corpus exits within 4 s of
`fusermount3 -u` (probe: `plain: process_alive_4s_after_unmount=no`;
`trig: process_alive_4s_after_unmount=yes`). So the trigger engine's threads
outlive the unmount, and because every graph command opens `graph.db`
read-write, the user is left with a permanent
`Could not set lock on file ".vfs/graph/graph.db"` until they hunt the PID.
`hilo graph stats` is the documented next step after a mount session; it is
dead until the orphan is killed by hand.

### DF-WARPFS-33 (P1) — every concurrent read fails: 7 of 8 parallel `hilo` commands exit 1

The "agent-first" promise is many queries in flight. Measured on the gson corpus
(`/tmp/wf11/concurrent-lock-probe.sh`, 8 processes: 4 × `graph understand`,
4 × `graph stats`):

```
run 1..4 (understand): rc=1  failed to open DuckDB graph database … Conflicting lock is held
run 5   (stats):      rc=0  Total edges: 4419 …
run 6..8 (stats):     rc=1  failed to open DuckDB graph database … Conflicting lock is held
```

One winner, seven failures — and the same failure is what the agent sees when an
MCP server is up and the CLI is asked anything. Root cause is visible in the
code: `GraphDB::open_with_budget_ms` calls `Connection::open(path)`
(hilo-graph/src/graph.rs:1080) for **every** command including pure reads
(`stats`, `impact`, `understand`), and DuckDB takes an exclusive file lock for a
read-write handle. Fix direction: open read-only for query-only paths (DuckDB
`AccessMode::ReadOnly`), or retry-with-backoff on `Conflicting lock`, and say
which process holds it (the error already names the PID — keep that).

### DF-WARPFS-34 (P1) — Java file-form id resolution is missing: a central class answers "No dependents found", and `stats` calls it an orphan

```console
$ hilo graph impact gson/src/main/java/com/google/gson/Gson.java
No dependents found for 'gson/src/main/java/com/google/gson/Gson.java'.   # truth: 112 importers
$ hilo graph impact pkg:com.google.gson.Gson
… 112 rows, ast_exact conf=1.00                                            # exact
$ hilo graph stats | sed -n '/^Orphans/,$p' | grep -c '\.java'
25      # Gson.java and GsonBuilder.java are both listed as "no incoming edges"
$ grep -c '"to":"pkg:com.google.gson.Gson"' .vfs/graph/edges.jsonl
194     # …while the tool's own edge data holds 194 of them
```

Same family as GAP-057 (Go) / GAP-064 (Python) / GAP-069 (TS-JS): the pkg-form
resolves exactly, the **file**-form that the docs use for every other language
(`docs/cli-reference.md:148`: `hilo graph impact hilo-graph/src/lib.rs`) silently
answers empty on Java, and the wrong answer is a *confident* empty rather than an
error. The orphan verdict in `stats` is the same defect wearing a different hat —
the graph knows the relationship, the file-level view does not.

### DF-WARPFS-35 (P2) — `graph understand` on Java emits symbol noise and answers from the wrong files

`hilo graph understand 'how does Gson serialize null fields'` (78 ms, 14.8 kB
pack) produced a MAP/SIGNATURES/DETAIL pack whose symbols are decorators and
expression fragments rather than declarations:

```
gson/src/main/java/com/google/gson/GsonBuilder.java →
  - @CanIgnoreReturnValue      (×7, nothing else)
gson/src/main/java/com/google/gson/JsonDeserializer.java →
  - context)
extras/src/main/java/com/google/gson/interceptors/Intercept.java → (no symbols extracted)
pkg:com.google.gson.Gson → (no symbols extracted)          # every pkg node
```

Only 2 files reached `## DETAIL` (`SerializedName.java`, `DefaultMapJsonSerializerTest.java`);
`Gson.java` — where `serializeNulls` is actually defined (`Gson.java:186`) — is
not in the pack at all, and the SIGNATURES block labels it `:0 (no symbols)`.
For the agent this is the difference between "an answer" and "plausible text".

### DF-WARPFS-36 (P2) — Java classify: public API classes land in `unknown`, benchmarks land in `entrypoint`

```
$ hilo classify --dry-run --limit 0 | tail -5
  By role: test 142 | unknown 68 | library 47 | entrypoint 5 | example 2
```

`gson/src/main/java/com/google/gson/Gson.java`, `GsonBuilder.java`,
`JsonNull.java`, `ToNumberPolicy.java` → `role=unknown` ("no classification
pattern matched"), while `metrics/src/main/java/**/…Benchmark.java` → `role=entrypoint`.
The test detection is right; the main-source role detection is not — so
`graph untested` (102 files) mixes core library files with genuinely uncovered
ones, and `user.vfs.role` is not trustworthy on Java. (GAP-049 family, now
confirmed outside Rust.)

### DF-WARPFS-37 (P2) — `tested_by` edges exist on Java but per-file coverage reads 0.0%

```console
$ hilo graph module gson/src/test/java
Module: gson/src/test/java
Files:  123   Edges: 3038   Tests: 0.0%
$ grep -c '"tested_by"' .vfs/graph/edges.jsonl
1744
```

`docs/graph-engine.md` documents `tests`/`tested_by` as **file→file** ("Test file
A tests source file B"), but on Java the edges target `pkg:` nodes
(`{"from":"…/ModuleTest.java","to":"pkg:com.google.gson.Gson","rel":"tested_by"}`)
and the `module` view does not consume them. GAP-066/071 recurring on a third
language; the docs sentence is the part that makes it a defect rather than a gap.
*(See DF-WARPFS-39 below — the corrected framing: `untested` and the FFI do
produce a number; `module` is the surface that prints 0.0%.)*

### DF-WARPFS-38 (P2) — `hilo init` installs no `.gitignore` entries, so the first commit in a real repo adds a 1.3 MB binary cache

`docs/inventory-policy.md:9-11` states the tracked inventory is "tracked in git
(see `.gitignore`, which explicitly keeps `.vfs/manifest.yaml` and `edges.jsonl`
tracked while ignoring the rebuildable state around them)". That `.gitignore` is
hand-maintained **in hilo's own repo**; `hilo init` does not write one (verified
in a fresh `git init` + `hilo init` — no `.gitignore` is created). On gson, the
first normal `git add -A && git commit` therefore took:

```
.vfs/graph/graph.db          | Bin 0 -> 1323008 bytes
.vfs/graph/.parse_cache.json | 1 +
.vfs/graph/.last_reconcile   | 1 +
.vfs/graph/.last_warm        | 0
```

A 1.3 MB binary (2.4 MB after the next warm) plus parse caches in every user
repo, exactly the state the policy says is ignored.

### DF-WARPFS-39 (P2) — two coverage surfaces report different numbers for the same data, and per-file coverage is not computable at all

Found by cross-checking the FFI consumer's output against the CLI:

| Surface | Number |
|---|---|
| `graph module gson/src/main/java` | `Tests: 0.0%` |
| `graph module gson/src/test/java` | `Tests: 0.0%` |
| `graph untested` | 102 untested of 243 → **58.02%** "tested" |
| FFI `handle.vfs_graph_stats()` | `tested_pct=58.0246913580247` |

Reconciliation (`/tmp/wf11/coverage-reconcile.py`) shows **0 of the 1744
`tested_by` edges target a file** — every one targets a `pkg:` node — and the
58.02% that two surfaces agree on is exactly `141/243`, the share of files that
*emit* `tested_by` (i.e. look like test files). So the surviving number answers
"how many files look like tests", not "is this source file covered", the module
view answers nothing, and true per-file coverage cannot be computed from this
data. Two numbers describing the same board disagreeing is worse than one number
missing, because both look plausible.

### DF-WARPFS-40 (P2) — the documented FFI workflow stops one step short of a working consumer

The FFI consumer path was exercised for the first time this run (runs 1–9 used
the CLI/MCP/FUSE; run 8 verified only that the ABI is exported with `nm -D`):

```console
$ uniffi-bindgen generate src/hilo.udl --language python --no-format --out-dir <dir>
$ test -f <dir>/hilo.py && echo OK          # OK (74496 bytes)
$ cargo build -p hilo_ffi                    # documented Build section → target/debug/libhilo_ffi.so, 8s
$ python3 -c 'import hilo'
OSError: /tmp/wf11/bindings/python/libuniffi_hilo.so: cannot open shared object file
$ cp libhilo_ffi.so libuniffi_hilo.so && python3 -c 'import hilo'   # works
```

After that single undocumented rename, the real consumer ran **5/5 calls**
against the gson graph:

```
vfs_get_metadata(Gson.java, user.vfs.note)  → 'dogfood-run11'   (xattr written by the CLI)
HiloHandle('/tmp/wf11/gson-corpus')         → OK
vfs_graph_stats()                           → total_files=243 total_edges=4419 tested_pct=58.02
vfs_graph_impact('pkg:com.google.gson.Gson', 3) → total=112      (== grep truth)
vfs_graph_related(Gson.java)                → 29 edges
vfs_list_directory(repo)                    → 21 entries
```

`grep -rn libuniffi docs/ hilo-ffi/README.md` → 0 hits. The value is real; the
documentation is one line short of delivering it.

## Ephemeral install leg (bunker-las-02, agent 8b0362e6)

`bunker-qa.sh launch` → `collect` (17 evidence rows, agent destroyed, TTL 4h).
**Not a silent pass, and not a full pass:**

| Cell | Result |
|---|---|
| fresh-install | **INFO ENV-BLOCKED** — bare las-02 agent lacks `pkg-config`, is non-root (no `sudo`), and the build died in `openssl-sys v0.9.117`'s build script. **Third consecutive occurrence** (runs 9, 10, 11) of the same outcome, already filed as **DF-WARPFS-25** (pending) — recorded as recurrence, not re-filed. Installability on a bare machine is therefore still unproven, and the README's own `sudo apt install …` line cannot run on that image |
| ci-pass | **OK** — `act`: 1 job green, rc=0 |
| chaos-disconnect | **OK** — fails fast (rc=101) when the network drops |
| chaos-shutdown | **OK** — SIGTERM + SIGKILL recovery clean |
| upgrade | **FAIL / UNVERIFIED** — the synced tree carries no git history to check out a previous release; the battery grades it untested |
| docker-deploy | **INFO** — `compose up` OK, probe `000` (same as runs 9/10) |
| chaos-resource | **INFO ENV-BLOCKED** — same openssl-sys cause; the 3G cap was never exercised |
| chaos-corruption | **N/A** — no db/state files in the repo |
| chaos-errorpath | **INFO** — no start command detected; missing-config behaviour UNVERIFIED |

The headline feature was not exercised on the ephemeral box because the install
never completed; the evidence is
`/tmp/bunker-qa-evidence-20260923T125748Z-1819211.jsonl` (17 rows).

## Performance (Step 2b, `coding-hermes-perf` law)


Release build, warm cache, gson corpus (243 files / 4418 edges), hyperfine:

| Operation | warm | notes |
|---|---|---|
| `hilo graph stats` | **19.8 ms ± 1.8** (n=20) | ≈ 0.25 s cold: the first call in a fresh corpus pays the reconcile |
| `hilo graph impact pkg:com.google.gson.Gson` | **34.5 ms ± 2.1** (n=20) | 112 rows |
| `hilo graph understand '<question>'` | **78.4 ms ± 3.1** (n=10) | 14.8 kB pack |
| `hilo graph warm` incremental | **29 ms** | all cached |
| `hilo graph warm` cold | **12.0 s** | 264 files, first parse |
| MCP `vfs_graph_impact` round trip (server start included) | **104 ms** | stdio |

**No PERF row.** Nothing here is slow enough that a user would notice; the waits
this run were failures (locked DB, empty answers) plus one self-inflicted
45-minute release build of the FFI crate that no user would run (the documented
build command is the debug one, which took 8 s with a warm cache). The numbers
above are recorded so a later run can detect a regression, not because they need
work.

## Time-to-first-success and friction

- Time-to-first-success on Java: **~14 s** cold (`init` 12 ms → `warm` 12.0 s →
  first exact answer 34 ms) — but the first *file-form* answer is a confident
  wrong empty at the same cost, which is the DF-WARPFS-34 hazard for an agent
  that does not know the `pkg:` form exists.
- Friction: 9 findings (DF-WARPFS-32..40), 3 of them blocking-or-honesty
  (32, 33, 34), plus the two only real integration finds (39: two surfaces
  disagreeing about the same data; 40: a documented workflow that ends one step
  short of a working consumer).

## Verdict reasoning

The Rust/Go core this project is built on is still the strongest part: the Java
pkg-form blast radius came back **exactly** right (112/112 against grep), the
on-commit hook really keeps the inventory current, and the metadata/xattr path
round-trips across CLI, MCP and a Python FFI consumer. What this run adds is the
shape of the remaining gaps: **the file-level id dialect is implemented per
language and Java never got it** (so the answer is a confident empty and the
orphan list contradicts the tool's own edges); **the graph is single-writer where
the product wants many readers** (7 of 8 parallel commands fail; a second agent's
MCP call errors with -32603); and **a mount that was correctly unmounted keeps
the lock** (the documented teardown is not the end of the process). The FFI path,
untested since run 8, is the good news: it works from Python, and its one barrier
is a filename the docs never name.
