# tested-by granularity fixtures (GAP-121)

Each language directory holds the SAME shape: one source file with two public
functions, and a test file that references exactly ONE of them.

    <lang>/lib.*        two functions: `used` (referenced by the test)
                        and `never_called` (referenced by nothing)
    <lang>/main.*       an entrypoint that calls `used`
    <lang>/*test*       a test file that references `used`

The point: at FILE granularity this looks like a tested file (a test file
exists beside it). At FUNCTION granularity `never_called` has zero references
and must be reported. That is the GAP-121 false positive — Hilo used to say
`tested_by ... conf=1.00` for the file and omit it from `graph untested`.

The negative control is produced by the test suite, not committed as a second
tree: `hilo-graph/tests/granularity_test.rs` copies each directory to a temp
dir, deletes the reference to `used` from the test file, and asserts BOTH
functions are then reported. A run that still reports coverage fails. Keep the
import when editing by hand — an unused import must NOT count as coverage, and
that is itself asserted.

## `repro-typescript/`

The minimisation of the ORIGINAL false positive
(totalwindupflightsystems/consensus `internal/harness/planning.go`): `src/lib.ts`
imports `./helper`, so it is a candidate for `graph untested`, and
`src/lib.test.ts` imports `./lib`, so the graph carries a file-level
`tested_by` edge to it. `graph untested` therefore omits `src/lib.ts` while
`neverCalled` has zero test references. `hilo-cli/tests/gap121_granularity.rs`
proves both halves of that through the binary (init + warm + query).
