# Run notes — exactly what was executed (run 49)

All commands ran in the scratch copy
`~/.hermes/cache/scratch/df49/corpus` (a copy of `coding-hermes-tools` @
`6e702d1`), with the installed artifact `~/.cargo/bin/hilo`
(`hilo 0.4.1`, `build: v0.4.0-50-g4216c7f`).

```bash
# 0. consumer setup (outside the hilo repo, as a user would)
cp -a ~/coding-hermes-tools ~/.hermes/cache/scratch/df49/corpus
cd ~/.hermes/cache/scratch/df49/corpus
time hilo init                                    # 0.046 s, installs post-commit + post-merge hooks

# 1. first map build (the promise: "pre-built map, no context burn")
hilo graph clean
/usr/bin/time -f 'wall=%e user=%U sys=%S rss=%MkB' hilo graph warm   # run 1; run again clean -> run 2
HILO_WARM_TIMING=1 /usr/bin/time -f 'wall=%e user=%U sys=%S' hilo graph warm   # phase split

# 2. queries a real agent asks
/usr/bin/time -f 'wall=%e' hilo graph warm        # cached: 0.84 s
hilo graph stats                                  # 0.09 s
/usr/bin/time -f 'wall=%e' hilo graph impact internal/patch/patch.go        # 0.23 s
/usr/bin/time -f 'wall=%e' hilo graph understand "where is the patch apply path and what tests cover it"   # 0.82 s
/usr/bin/time -f 'wall=%e' hilo graph audit       # 0.74 s
hilo graph audit --json > audit.json

# 3. the COV chain on a consumer repo (documented refusal — recorded, not re-filed)
hilo graph surfaces                 # rc=1, scope: foreign
hilo graph coverage-links --unlinked # rc=1
hilo graph test-classes             # rc=1
hilo graph rollup                   # rc=1

# 4. ground truth for the oracle checks
grep -rln 'coding-hermes-tools/internal/patch"' --include='*.go' .     # 5 importers — matched by `graph impact`
jq -r '.from' .vfs/graph/edges.jsonl | sort -u | wc -l                 # 248 distinct sources (3 std:)
jq -r '[.from,.to,.rel,(.provenance//"")]|@tsv' .vfs/graph/edges.jsonl | sort -u | wc -l   # 2321 identities
cat .vfs/graph/coverage.json                                            # unique_edge_sources: 242

# 5. bucket provenance for `graph audit`
bash audit-receiver-classify.sh audit.json corpus > audit-receiver-classify.tsv
sed -n '290,335p' corpus/cmd/toolsd/serve.go   # the 4 net.Addr/net.Conn implementations
sed -n '1,5p' corpus/internal/scratch/s2.go    # Bye
```

## Harness bugs I hit and fixed in my own probe (so the next fire does not re-diagnose them)

1. `jq 'select(... | test(...))'` fails with
   `string and object cannot be added` on this board: `detail` is not a string
   on every row (some carry objects/arrays). Always
   `((.title//"")|tostring)` before concatenating.
2. The first bucket classifier read `func (connModeAddr) Network()` as a
   *method on an exported type* because it took the first receiver token;
   the receiver type is the LAST token. Fixed in the committed script.
3. `hilo graph warm` prints its edge count and `graph stats` prints a
   different one; both are correct for what they count (see the report) —
   do not "fix" either number without reading `coverage.json` first.

## Not done, and why

- A **fresh-HEAD build** from `origin/master` was started
  (`cargo build --release -p hilo-cli` in a worktree at `ee76982`, own
  `CARGO_TARGET_DIR`, 4.4 GB after ~35 min and still in the dependency C++
  build) and did not finish inside the tick. The behaviour and perf arms above
  therefore grade the certified `v0.4.1` artifact (`4216c7f`), which is an
  ancestor of `origin/master`; the tree is 20 commits further and 3 of those
  touch product code (`f9efdd7` mount xattrs, `1d90760` lint reap, `84c0267`
  Go module-local imports). **Nothing in this report should be read as a
  statement about those three fixes.**
- **flamegraph/profile**: `perf_event_paranoid=4`; `perf record` fails. The
  phase split from the product's own instrumentation is what is committed.
- **small-corpus fixed-cost probe** (does `GraphDB::open` cost ~9.5 s even on a
  tiny repo?): the command was refused by the command gate and, per the lane's
  rules, was not re-run by another route. It stays an explicitly labelled
  HYPOTHESIS in the report and produced no board row.
