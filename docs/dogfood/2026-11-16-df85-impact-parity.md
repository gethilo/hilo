# DF-WARPFS-85 impact parity — 2026-11-16 (rework: evidence made reproducible)

Closes the evidence gap that failed the previous Tier-2 judgement: the parity
number is now produced by a test that lives in this repository, not by an
out-of-repo scratch run.

## Corpus

JetBrains/Exposed shallow checkout at commit `51b78c1`
(`51b78c1fa4c8967aa56779fbcd8d6ae8dfa40c24`, 2026-11-12, "build: Run GitHub
workflows on pull requests targeting develop/* and release/* branches
(#3012)"), full tree, 985 `.kt` files.

The checkout itself is **not** in the repo and must not be committed. Point the
test at any copy:

```text
HILO_EXPOSED_CORPUS=<path to an Exposed checkout>
```

The test copies the corpus into its own temp directory and deletes the copied
`.vfs`, so a cache carried in with the checkout can never answer for the binary
under test, and the source checkout is never mutated.

## Reproduction

```text
CARGO_TARGET_DIR=/var/tmp/hermes-target \
HILO_EXPOSED_CORPUS=<path to an Exposed checkout> \
cargo test -p hilo-cli --test dfwarpfs85_impact_parity -- --nocapture
```

Without the env var the test prints `skipped: HILO_EXPOSED_CORPUS not set` and
passes, so CI needs no corpus. With the env var set, a parity break is a
failure, never a skip.

The test drives the real compiled binary (`env!("CARGO_BIN_EXE_hilo")`) through
`hilo init` → `hilo graph warm` → `hilo graph impact pkg:... --max-depth 1`, and
asserts:

1. the `Coverage:` line's arithmetic closes (files = sum of the buckets);
2. the depth-1 impact listing equals the importer-truth set — **set equality of
   project-relative paths**, computed by the test's own recursive `.kt` scan for
   `import org.jetbrains.exposed.v1.core.Table`, so there is no constant to rot
   and no reliance on an external `grep`;
3. `exposed-maven-plugin/src/test/resources/template/sourceFiles/Cities.kt` and
   `Users.kt` each carry an `imports` edge to
   `pkg:org.jetbrains.exposed.v1.core.Table` in `.vfs/graph/edges.jsonl`, **and**
   appear in the depth-1 impact listing.

## Measured result (re-measured 2026-11-16 in this worktree)

Raw commands against a throwaway copy of the corpus:

```text
hilo graph warm
hilo graph impact pkg:org.jetbrains.exposed.v1.core.Table --max-depth 1
grep -rl 'import org\.jetbrains\.exposed\.v1\.core\.Table' --include='*.kt' . | wc -l
grep template/sourceFiles/\(Cities\|Users\)\.kt .vfs/graph/edges.jsonl | grep pkg:org.jetbrains.exposed.v1.core.Table
```

```text
Coverage: 1059 files = 971 contribute edges + 0 package facades (__init__.py) + 88 no imports + 0 parse-miss + 0 unreadable + 0 unsupported extension
181
181
{"from":"exposed-maven-plugin/src/test/resources/template/sourceFiles/Cities.kt","to":"pkg:org.jetbrains.exposed.v1.core.Table","rel":"imports","provenance":"lexical","confidence":0.3}
{"from":"exposed-maven-plugin/src/test/resources/template/sourceFiles/Users.kt","to":"pkg:org.jetbrains.exposed.v1.core.Table","rel":"imports","provenance":"lexical","confidence":0.3}
```

Equality statement: `graph impact pkg:org.jetbrains.exposed.v1.core.Table`
depth-1 dependents = 181 files = `grep -rl` importer truth = 181 files.
**Parity 181/181**, and both template `.kt` files under
`src/test/resources/template/sourceFiles/` contribute `imports` edges — the
DF-WARPFS-85 defect does not reproduce at `367bd6b`'s recovery code.

The number moved from the 2026-09-25 snapshot's 176 to today's 181 because the
upstream corpus moved (two different Exposed revisions; 11 months of upstream
commits). The historical `173/176` measurement is kept verbatim in
`docs/dogfood/2026-09-25-run22-php-kotlin-hooks.md`, `docs/dogfood/diagnostics.md`
and `skills/hilo-usage/SKILL.md`, each annotated as superseded by this artifact.

## Negative control (the test can fail)

Run against a deliberately broken copy of the same corpus — the `Table` import
line deleted from both template files, plus three additional real importers
(`exposed-core/src/main/kotlin/RedProbe{A,B,C}.kt`) so the true count moves off
181 — the same command printed:

```text
Coverage: 1062 files = 972 contribute edges + 0 package facades (__init__.py) + 88 no imports + 2 parse-miss + 0 unreadable + 0 unsupported extension
impact depth-1 dependents: 182 | grep import-truth: 182 | parity: OK

thread 'exposed_kotlin_template_impact_matches_import_truth' panicked at hilo-cli/tests/dfwarpfs85_impact_parity.rs:235:9:
missing `imports` edge exposed-maven-plugin/src/test/resources/template/sourceFiles/Cities.kt -> pkg:org.jetbrains.exposed.v1.core.Table (template .kt files must contribute edges)
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out
```

Two things are proven by that output: the parity assertion tracks the corpus
(182/182, not a hard-coded 181), and a broken template edge FAILS instead of
skipping.

## Cost

A full warm of this corpus from an empty cache dominates the run: roughly five
to six minutes (308s green run, 355s red control on this host). It is env-gated,
so it never charges the default suite.
