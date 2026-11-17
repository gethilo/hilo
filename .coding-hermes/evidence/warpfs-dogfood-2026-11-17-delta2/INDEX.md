# Evidence — tick `warpfs-dogfood-2026-11-17-12-14-37`, DELTA (second delivery)

Scope: this delivery verified provenance of run 49 and settled the one leg the first
delivery left open (the ephemeral install battery) plus the ungraded HEAD build. It did
not re-walk run 49's product measurements.

| file | what it is | figures it supports |
|---|---|---|
| `bunker-c3a0807b-qa-evidence.jsonl` | the install agent's own ledger, pulled verbatim over `bunker --server bunker-mvp exec c3a0807b -- cat <home>/qa-evidence.jsonl` | 8 rows: launch OK, 5 toolchain-bootstrap rows (go 1.27.4, rust stable, gcc wrappers, make, jq; one INFO that `libssl-dev` is absent), `fresh-install` **SKIP** at the 3600 s budget, `lint` **FAIL** on `go vet ./...` |
| `inst-log-tail.txt` | `wc -l` plus the last six lines of the cell's `qa-logs-99209/inst.log` at the moment of the kill | inst.log = **1056** lines, ending while `hilo-cli`, `hilo_plugins`, `git2`, `hilo_core`, `hilo_backends`, `hilo_fuse` were still compiling |
| `local-head-build-park-sample.txt` | 20 s `/proc/<pid>/stat` utime+stime sample over the cargo process tree for the local HEAD release build | 34 live compile pids, **1322 ticks** in 20 s (~3.3 cores) — slow, not parked |
| `sample-build-park.py` | the sampler that produced it (re-runnable: `python3 sample-build-park.py <cargo-pid>`) | distinguishes a compiling build from a stalled one; run it against the `cargo` pid, not a `c++` driver whose children do the work |
| `file-rows.sh` | the exact `boardctl create` argv lists for both filed rows | provenance for `QA-WARPFS-043` and `DF-WARPFS-175` (ids minted by `board_next_id.py` against the canonical `origin/master` board copy) |
| `delta-log-entry.md` | the log entry as appended to `.coding-hermes/dogfood-log.md` | the narration; the log is the greppable record |

Not evidence of product behaviour at HEAD: no HEAD binary exists yet, so the product
claims in runs 48/49 stay scoped to the certified artifact `4216c7f`.

Timings quoted in the rows and the log (all wall clock, UTC on 2026-11-17): agent
launched 12:46:21Z; `fresh-install` timeout at 3600 s → killed ≈13:46Z; cargo observed at
56 m 36 s one minute earlier; local HEAD cargo observed at 50 m 15 s with no binary.

## Third-delivery additions (14:5x UTC)

| file | what it shows |
|------|----------------|
| `bunker-c3a0807b-qa-evidence.jsonl.harvest-3rd` | ledger re-pulled at third-delivery close: still 8 rows, byte-identical to the delivery-2 copy (tests cell had not finished) |
| `lint.log` | the `go vet ./...` refusal verbatim ("directory prefix . does not contain main module") — the ladder's missing Rust branch, owned by QA-WARPFS-035 |
| `tests-log-tail-3rd.txt` | last 2 KB of the tests cell log at close: `cargo test --workspace` ~65 min in, hilo_plugins/metadata/backends/fuse compiling, DuckDB debug units still in flight |

Bunker-host paths are left as-is (`/home/bunker-c3a0807b/...` is the ephemeral agent's home, consistent with the delivery-2 files above); the `~` convention applies to controller-host paths only.
