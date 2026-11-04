# repro-typescript — the observed GAP-121 false positive, minimised

`src/lib.ts` imports `./helper` (so it has an import edge and is a candidate
for `graph untested`), and `src/lib.test.ts` imports `./lib` (so the graph
carries a file-level `tested_by` edge to it).

Result before the fix: `graph untested` (file granularity) omits `src/lib.ts`,
i.e. a confident green over a file whose `neverCalled` function has zero test
references. This is the consensus `internal/harness/planning.go` shape.

After the fix, `graph untested --granularity function` lists
`neverCalled  src/lib.ts` while `--granularity file` output is unchanged.
