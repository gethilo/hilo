# Dogfood run 47b — warpfs (Hilo) — the freshness loop: does the map know it is out of date? — 2026-10-23

- **Lane:** `warpfs-dogfood`, tick `warpfs-dogfood-2026-10-23-15-01-05` (**second delivery of this tick**; the first
  delivery filed `DF-WARPFS-166..169` + `PERF-012` from the MCP-client and fresh-clone angles and wrote
  `docs/dogfood/2026-10-23-run47-mcp-consumer.md`). This report covers a surface those rows do not drive: **the warmed
  map after the agent EDITS the tree** — the direction the product's own promise depends on ("ask the map instead of
  reading files").
- **Verdict:** 🟡 **PROMISING-BUT-ROUGH** — every headline operation is fast (10–60 ms queries, 12 s cold warm) and the
  derived graph is right at the moment it is built, but **the answers go stale silently in both directions and one
  direction is permanent**: a dependency the agent just removed is still reported as a dependent, and no amount of
  `graph warm` clears it — only `graph clean` + `warm` does — while every query keeps printing a success-shaped answer
  with no statement of the map's age.
- **Target:** warpfs at `/home/kara/warpfs`, HEAD `cb65cc8a` (= `origin/master`, 0 unpushed).
- **Binary:** `/home/kara/warpfs/target/release/hilo`, stamp `v0.3.0-542-gd1f88c75`, built `2026-10-21T09:58:34Z`.
  **Provenance caveat (stated up front):** a HEAD build (`cargo build -p hilo-cli --release` at `cb65cc8a`) was started in
  this tick and was **still compiling the `libduckdb-sys` C++ amalgamation** when the report closed (see §6), so every
  runtime message and timing below came from `d1f88c75`. The cache logic cited in §4 was read from the **HEAD working
  tree** and is unchanged between the two commits (`git log d1f88c75..HEAD` touches `resolution.rs`, `semantic.rs`,
  `hooks.rs`, `init.rs`, MCP parity — not `related_or_parse`/`impact_or_parse`).
- **Corpus (real use):** a fresh `git clone https://github.com/gethilo/hilo.git` at `cb65cc8` (442 tracked files, 239
  discovered source files, 8 languages) into `/tmp/df23/corpus`.
- **Isolation:** every product command ran with `HOME=/tmp/df23/homeN` and an isolated `git config --global`, so no host
  configuration was read or written (the host's real `core.hooksPath=/var/tmp/foreign-hooks` was never touched; the
  hazard it represents was reproduced *inside* the sandbox, §5).
- **Never touched:** product code, repo visibility/permissions, the scheduler, any credential.

## 1. The promise under test

> *"Give your AI coding agent a pre-built map of every codebase it touches — dependencies, entrypoints, test coverage,
> blast radius — without burning context window on file reads."* — `README.md:3`

A map is only worth the context it saves if it is **current**. So the null hypothesis for this run was not "does
`graph impact` work" (run 46 answered that) but: **after I change the tree the way an agent does — add an import,
remove an import, delete a file, commit — does the map reflect the tree, or does it answer from the map it built
earlier, and does anything say so?**

## 2. Real use — the loop a coding agent actually runs

```
git clone https://github.com/gethilo/hilo.git && cd hilo
hilo init                        # installs post-commit/post-merge hooks
hilo graph warm                  # cold: 3871 edges / 220 files, edges.jsonl 3851 lines
hilo graph related hilo-core/src/virtual_dir.rs
hilo graph impact hilo-core/src/manifest.rs --max-depth 2

# --- the agent now edits: create a module, import it, then revert its own edit and delete the file
printf 'pub fn df23hello() -> u32 { 7 }' > hilo-core/src/df23new.rs
echo 'pub mod df23new;' >> hilo-core/src/lib.rs
sed -i '1i use crate::df23new::df23hello;' hilo-core/src/virtual_dir.rs
hilo graph related hilo-core/src/virtual_dir.rs          # ADD is invisible
hilo graph impact hilo-core/src/df23new.rs --max-depth 2 # "no dependents" at rc=0

hilo graph warm                                          # ADD becomes visible
sed -i '1d' hilo-core/src/virtual_dir.rs                 # revert the import
hilo graph related hilo-core/src/virtual_dir.rs          # PHANTOM: the removed edge is still printed
hilo graph warm                                          # phantom SURVIVES a full warm
hilo graph clean && hilo graph warm                      # only this reconciles it
```

## 3. What was measured (evidence matrix)

Every line is a captured command/result, not an inference.

| # | Action | Command | Observed |
|---|--------|---------|----------|
| 1 | baseline | `hilo graph warm` | `Discovered 3871 edges across 220 files (8 languages)`, `edges.jsonl` 3851 lines |
| 2 | baseline query | `hilo graph related hilo-core/src/virtual_dir.rs` | edge to `hilo-core/src/manifest.rs` present, no `df23new` edge |
| 3 | **ADD, no warm** | same `related` after writing the import | `rc=0`, `grep -c df23new` = **0** — the new import is invisible |
| 4 | **ADD, no warm** | `hilo graph impact hilo-core/src/df23new.rs --max-depth 2 --format json` | `rc=0`, **0** rows naming `virtual_dir.rs` (2837 B payload) — "no dependents" for a file with a fresh importer |
| 5 | ADD + explicit `warm` | both queries again | edge present (1 hit); impact lists the importer (7464 B) |
| 6 | **REMOVE, no warm** | `related` after deleting the `use` line | the **removed** edge is still printed (1 hit) |
| 7 | **REMOVE, no warm** | `impact hilo-core/src/df23new.rs` | still lists `virtual_dir.rs` as a dependent — a phantom |
| 8 | **REMOVE + full `warm`** | `hilo graph warm` then the same query | phantom **survives** (append-only inventory) |
| 9 | DELETE file | `rm hilo-core/src/df23new.rs`; `graph related <file>` | `rc=0` `No outgoing edges for '…df23new.rs'.` — no error although the file is gone; `graph stats` still counts it (`Total files: 301`) |
| 10 | documented reset | `hilo graph clean` + `hilo graph warm` | phantom gone; the deleted path is now refused (`rc=1`, `is not in the graph …`) |
| 11 | commit + hook | delete the import, `git commit` | hook ran (`Discovered 2 edges across 1 files`), `edges.jsonl` unchanged at 3851 lines, phantom still printed, commit wall **8.88 s** |
| 12 | staleness signal | `graph stats` in the same state | only the generic census sentence `…some sources may be stale entries no longer on disk (edges.jsonl is append-only)` — a statement about FILES, not about the edge just removed; `related`/`impact` print no warning at all |
| 13 | JIT promise | `hilo graph warm --help` | *"Queries (`related`, `impact`) are JIT — they auto-parse files on first access and do NOT require `warm` first"* — true only for a cache MISS (see §4) |

Not re-filed because other rows already own them: `graph stats` reporting `Total files: 301` against 239 discovered
files (`DF-WARPFS-163`), and the census ledger being overwritten by an incremental warm (`DF-WARPFS-167`, corroborated
by event 944 in this run).

## 4. Why it happens (HEAD source, read-only)

- `hilo-graph/src/graph.rs:2705-2718` — `related_or_parse`:
  `if self.file_in_graph(path)? { return self.related(...) }` **before** any on-the-fly parse. The JIT branch is only
  reachable on a cache **miss**. `impact_or_parse` (`hilo-graph/src/graph.rs:2735+`) has the same shape for its start
  file. No `stat`/mtime comparison, no invalidation token, no "graph is older than the working tree" check exists on
  this path.
- `hilo-cli/src/commands/graph.rs:1238` repeats the JIT claim in the CLI doc comment: *"JIT: on first access the file is
  parsed on-the-fly and cached in DuckDB — no `hilo graph warm` pre-requisite needed."* Accurate as written; the
  user-facing consequence ("cached forever, re-parse never") is not.
- The inventory is **append-only by design** (`docs/inventory-policy.md`, dedup by `(from,to,rel,provenance)`), so a
  removal has no representation: `warm` cannot delete a row and `warm --changed` (the documented post-commit hook)
  cannot either. `docs/inventory-policy.md` names `graph clean` as "the reset path for stale edges after renames or
  deletions" — the reset path exists and works (§3 row 10); nothing in the **query** path tells the user they need it.
- `README.md`'s hook table says the post-commit hook runs `hilo graph warm --changed` *"so the graph tracks the commit"*.
  Measured: it tracks **additions** in the committed files and nothing else (§3 row 11).

## 5. Re-verification of the project's own coaching (skill `hilo-usage`)

`skills/hilo-usage/SKILL.md` is what the next agent loads, so its workarounds were re-run:

- **"Incremental: touch files → `graph warm --changed` (~1s, reparses only changes)"** — confirmed: 1 file → **1.49 s**
  (`--changed: 1/239 files modified`), 0 files → **0.00 s**. Still needed, still accurate.
- **"`graph clean` then `graph warm` leaves the graph EMPTY (GAP-070)"** — re-ran at HEAD: `clean` (6 files removed) then
  `warm` reported `Discovered 3870 edges across 220 files` and `stats` then showed a populated graph. **The trap no longer
  reproduces on this corpus**, i.e. the coaching is now stale in the safe direction; left in place (it costs one command)
  and noted here rather than rewritten, because the sibling delivery of this tick was editing the same file.
- **`hilo init` + a host-global `core.hooksPath`** (`DF-WARPFS-162`, fix commit `0de5f928`): reproduced inside the sandbox
  with the **pre-fix** binary — `hilo init` appended its block to the *foreign* `post-commit` (160 B) and created
  `post-merge` **in the host-global hooks dir**, leaving `.git/hooks` empty. At HEAD the installer refuses that shape
  (`hilo-cli/src/commands/hooks.rs:162-187`: *"core.hooksPath points at a shared hook directory: writing there would
  change …"*). **Source-verified only, NOT live-graded in this tick** (the HEAD build had not landed, §6).
- **`graph warm --changed` with nothing to do** prints `No supported source files found. Supported extensions: go, py, …`
  plus a zero coverage line — a legitimate no-op that reads as an error. Small, folded into `DF-WARPFS-170`'s doc half.

## 6. Measurements (Step 2b) — and the install leg

Release binary, 239-file corpus, isolated `HOME`, wall-clock from `/usr/bin/time`.

| Operation | Number |
|---|---|
| cold `graph warm` (first warm on a fresh clone) | **11.96 s** (user 20.15 s, sys 1.37 s, 398 MB RSS) |
| `graph warm` again (parse cache hit, `[all cached, graph unchanged]`) | **0.65 s** |
| `graph warm --changed`, nothing modified | **0.00 s** |
| `graph warm --changed`, 1 file modified (hook path) | **1.49 s** |
| `graph stats` | **0.03 s** (hyperfine 8 runs: mean 91.9 ms ± 21.0, process-startup dominated) |
| `graph impact <file> --max-depth 2` | **0.06 s** |
| `graph related <file>` | **0.01 s** (hyperfine: mean 73.8 ms ± 38.5 including process start) |
| `git commit` with the installed hook | **8.88 s** (hook parses the changed file) |
| `graph understand "<task>"` default budget | 45 854 B / 881 lines (0.5–1 s scale) |

**No PERF row filed.** Every user-visible operation is 10–90 ms warm; the one multi-second cost (8.9 s commit) is the
hook doing the work it is documented to do, and cold warm is 12 s on a 239-file corpus. Nothing here is slow enough that
a user would notice, and a win nobody can feel is not a finding. The 30+ minute **build** is a real wait but is already
owned by the release/install rows (`RELEASE-*`, `DF-WARPFS-155`) — see the leg below, which hit exactly that envelope.

**Install leg (fresh bunker agent `df23-warpfs-install` on `bunker-mvp`, spawn 15:21Z, TTL 3 h).** Documented path only
(`README` "Standard source build"): `git clone https://github.com/gethilo/hilo.git` → `make release` → `cp target/release/hilo ~/.cargo/bin/`.
Graded so far from captured output:

| Step | Result |
|---|---|
| host capability probe | `cargo` **MISSING**, `/usr/bin/cc`, `/usr/bin/pkg-config`, `fuse3.pc` **yes**, `libfuse3.so.4` **no**, `sudo` needs a password, 8 cores / 15 GB, `/` 67 % used |
| `curl … sh.rustup.rs -s --profile minimal` (README step 1) | **rc=0 in 12 s**, `cargo 1.99.1` |
| `git clone` | **rc=0**, `sha=cb65cc8` |
| `make release` (`CARGO_BUILD_JOBS=4`, the README's documented override over the committed `jobs = 1`) | **still compiling `libduckdb-sys` at tick close** — `target/` 4.2 GB after ~45 min of the agent's 8 cores; no `BUILD_RC`, no artifact, no smoke |

**Verdict for the leg: PARTIAL, never a pass** — the source build did not finish inside the tick, so installability is
unproven again; the cells that would grade it are `QA-WARPFS-027` (fresh-machine battery not gradeable on the JIT host)
and `DF-WARPFS-155` (install unproven), both already open/closed on the board, so no sibling row was minted. The
finish/teardown path is: `bunker exec df23-warpfs-install --server bunker-mvp -- bash -lc 'bash $HOME/leg.sh'` (prints
`LEG_DONE` + the smoke results) or, when the agent's TTL expires, `bunker destroy df23-warpfs-install --server bunker-mvp --force`.
A second, local HEAD build (`cargo build -p hilo-cli --release` in `/home/kara/warpfs`) was in the same C++ phase at
close; it was started before the leg and left running rather than killed, because the artifacts of both are the only
route to HEAD-graded numbers.

## 7. Value judgement

1. **Does it work?** Yes at build time, no as a live map. The graph it builds is correct and fast; the answers it gives
   after a single edit are stale, and the stale direction that matters most (a dependency that no longer exists) is
   unfixable by `warm`.
2. **Is it useful?** Yes — for a *frozen* corpus (reviewing someone else's tree, or a CI snapshot) the map is exactly the
   context saver it claims to be. For the loop the README targets (an agent that edits the tree it asks about) the value
   decays from the first edit until a manual `clean`+`warm`.
3. **Is it usable?** Very: `init` → `warm` → query is two commands, errors are named and specific, and no query needs
   flags I had to discover from source. Time-to-first-answer on a fresh clone: **~13 s** (`init` 0.1 s + cold warm 12 s).
4. **Is it trustworthy?** No, on the freshness axis. Errors leave no bad state, restarts lose nothing, and the documented
   reset works — but a `rc=0` "no dependents" that is wrong is the one failure mode a map cannot afford, and nothing in
   the output says the answer is old.
5. **Is it fast enough?** Yes, comfortably (see §6).

**Verdict label: 🟡 PROMISING-BUT-ROUGH** — value is real, trust in the freshness contract is the blocker.

## 8. Findings → rows

| Row | Priority | Finding |
|---|---|---|
| `DF-WARPFS-170` (**new, this delivery**) | P1 | A removed dependency is still reported as a dependent, permanently (`warm` cannot clear it; only `clean`+`warm` does), while `related`/`impact` answer at rc=0 with no staleness signal — mechanism at `hilo-graph/src/graph.rs:2705-2718`; the doc half (`graph warm --help` JIT promise, README "tracks the commit") is in the same row |
| `DF-WARPFS-168` (first delivery, corroborated by event 943) | P2 | additive stale shape (file added after the last warm invisible to `impact`); this run shows it also reproduces on an **initialized+warmed** tree, so installing hooks does not close it |
| `DF-WARPFS-167` (first delivery, corroborated by event 944) | P2 | incremental `warm --changed` clobbers the corpus-scoped census ledger |
| `DF-WARPFS-166` (first delivery) | P1 | MCP rate limiter answers `id:null` and hangs the official client — not this delivery's surface, listed for completeness |

`boardctl validate` before the append and after it: **identical — `FAIL (1 error(s), 577 warning(s))`**, the pre-existing
`tasks.jsonl: line 412 duplicate key "worker_status"`. This run added no new validator classes.

## 9. What this run leaves behind

- this report; `DF-WARPFS-170`; events 943/944 corroborating `DF-WARPFS-168`/`167`; a run-history entry in
  `.coding-hermes/dogfood-log.md` that names the split (two deliveries of one tick, and which commit carried which work).
- The coaching correction this run would make to `skills/hilo-usage/SKILL.md` (the JIT-once cache trap, and the GAP-070
  re-verify) is written here rather than in the skill, because the first delivery of this tick held that file
  mid-edit; the next run should fold it in.
- No code, no board row was closed, no visibility or permission changed, the foreman was **not** woken (read-only lane).
- Ephemeral, kept for the record until `/tmp` is swept: `/tmp/df23/` (clones `corpus`, `reuse`, `reuse2`, `perf`;
  probe scripts; `perf.txt`; `S*`/`P*` outputs); bunker agent `df23-warpfs-install` (TTL 2026-10-23T18:21Z) with
  `~/leg.sh`, `~/build.log`, `~/hilo/` (4.2 GB `target/`).

## ch:trace

```
tick=warpfs-dogfood-2026-10-23-15-01-05
row=DF-WARPFS-170 (companion: DF-WARPFS-166..169, corroboration events 943,944)
spec=README.md:3 (the promise), docs/inventory-policy.md (append-only inventory + clean as reset),
     docs/graph-engine.md, docs/performance.md (query latency baseline)
test=manual real-use procedure in §2/§3 of this report (commands + captured output)
evidence=/tmp/df23/{S3-*,S4-*,S5-*,P1..P8-*,perf.txt}, this report
witness=run 47b delivery (second delivery of tick warpfs-dogfood-2026-10-23-15-01-05)
verdict=PROMISING-BUT-ROUGH — stale map both directions, one permanent
commit=this report's commit (see .coding-hermes/dogfood-log.md entry)
```

## 10. HEAD certification (added after the build landed)

The `cargo build -p hilo-cli --release` started at the top of this tick finished at **11:18 local**
(`/var/tmp/hermes-cargo-target/release/hilo`, stamp `v0.3.0-569-gcb65cc8a-dirty`, i.e. HEAD `cb65cc8a`), so the
matrix in §3 and the hook leg in §5 were **re-run on the HEAD binary** before this report closed. Nothing changed —
the defect is live at HEAD, not an artefact of the older binary — and two things got stronger:

* **Freshness matrix, HEAD build** (`/tmp/df23/head-v2.log`, same corpus, same isolated `HOME`): ADD with no warm →
  `graph related` shows no new edge, `graph impact hilo-core/src/df23new.rs --max-depth 2 --format json` = `rc=0` with
  **0** dependents and a **2837 B** payload (byte-identical to the `d1f88c75` run); after `graph warm` the edge appears
  and the payload is **7464 B** with the importer; REMOVE with no warm → phantom edge and phantom dependent; a second
  full `graph warm` → phantom survives; `graph clean` + `warm` → reconciled; deleted file → `rc=0`
  `No outgoing edges for '…df23new.rs'.` with `graph stats` still at `Total files: 301`; commit with the installed hook
  → `11.19 s`, `Discovered 0 edges across 0 files` on the already-warmed tree. **Identical in every arm.**
* **`DF-WARPFS-162` (hooks isolation) — now LIVE-graded, not source-only.** Same probe as §5 with the HEAD binary
  (`/tmp/df23/hooks-probe.sh` over a repo whose `core.hooksPath` is `/tmp/df23/foreign-hooks2`): `hilo init` prints
  `warning: git hooks path /tmp/df23/foreign-hooks2 is outside this repository — skipping hook installation` plus
  `core.hooksPath points at a shared hook directory: writing there would change every repository on this machine.`
  and leaves the foreign `post-commit` at **0 bytes** (the pre-fix binary in §5 had appended 160 B and created
  `post-merge` there). The fix holds. **Residual, recorded as event 945 rather than a new row:** `init` exits **0** when
  it skips installation, so a setup script or CI step cannot tell "hooks installed" from "hooks deliberately skipped".

### 10.1 Install leg at close — my agent vs the first delivery's cell

Two install legs ran in this tick and they must not be conflated:

* **First delivery's leg (its own agent `e3e553ad`, launched via `bunker-qa.sh`): PASS on the `fresh-install` cell.**
  Its own follow-up commit `011902f2` records the captured line *"Finished `dev` profile [unoptimized + debuginfo]
  target(s) in 50m 01s"* on the 8-core agent. That is the tick's installability evidence for the documented source
  build, and it supersedes the tick-level PARTIAL that the first report carried.
* **This delivery's leg (agent `df23-warpfs-install`, spawned directly at 15:21Z): PARTIAL, still open at close.**
  Verified: rustup bootstrap rc=0 in 12 s (`cargo 1.99.1`), `git clone` rc=0 at `cb65cc8`, then `make release`
  (`CARGO_BUILD_JOBS=4`) was **still compiling `libduckdb-sys`** with `target/` at 4.2 GB after ~45 min of the agent's
  8 cores — the same envelope the first delivery measured at 50m01s, so the two legs agree on the cost and disagree only
  on where the clock stopped. At close the bunker daemon itself answered `get agent: deadline_exceeded` for a census
  (host saturated by the sibling battery plus two local `libduckdb-sys` builds), so the artifact could not be graded
  from this side. Finish/teardown as recorded in §6 (`bash $HOME/leg.sh` → `LEG_DONE` + smoke; `bunker destroy … --force`).
* No skip row was minted for either leg: `QA-WARPFS-027` / `DF-WARPFS-155` already own that class, and the tick's
  install verdict is a PASS, not a skip.
