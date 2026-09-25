# Dogfood Run 20 — 2026-09-25 (coding-hermes-tools-dogfood, Python FFI consumer × live backend)

## Angle (stale-surface rule)

Runs 1–19 drove CLI, graph, FUSE, MCP, plugins, workspace, permissions, Go FFI,
Java FFI, cross-surface integration and the backend-sync suite. What NO run ever
did: use the **Python FFI bindings against a project with a LIVE mounted
backend**, and cross-check every FFI answer against the CLI/ground truth on the
same project. The run-8 verdict ("FFI returns constants, backends invisible to
FFI") has also never been re-verified after the DF-WARPFS-12-era rewrites.

Promise under test: *"a Python service embeds Hilo through the UniFFI bindings
and gets the same answers the CLI gives — including where a file comes from
(`vfs_resolve_backend`) — on a project backed by a live S3 store."*

HEAD tested: 075e447 (origin/master == local master). Consumer artifacts:
`/tmp/dogfood-warpfs-run20/` (consumer.py, cwd_test.py, append_rows.py,
qa20c-full.log). Backend: the repo's own docker-compose MinIO, bucket
`hilo-run20`, throwaway creds `hilo_test`.

## What worked (real-use verified)

- **The full documented Python FFI recipe works** (bindgen → `cargo build -p
  hilo_ffi` debug → copy-rename to `libuniffi_hilo.so` → `import hilo`), and on
  a fresh Debian 13 bunker box from zero (rustup minimal 1.98.1, 985s total).
  This is the DF-WARPFS-40 fix (run 11) holding up: the recipe as now documented
  in `hilo-ffi/README.md` needs zero undocumented steps.
- **`vfs_resolve_backend` is REAL now** — the run-8 "constants era" is gone.
  On a project with `.vfs/backends/mounts.yaml` (s3data → s3://hilo-run20),
  the FFI returned `backend=s3, remote_url=s3://hilo-run20/, cached=true` for a
  file under the mount and `backend=local-only` for a workspace file, with the
  mount created purely through the documented CLI. An embedded Python service
  can genuinely answer "where does this file come from".
- **DF-WARPFS-55 (P0, CWD-dependent FFI) verified FIXED from Python**: with the
  embedding process CWD at /tmp, `HiloHandle(repo)` + `vfs_graph_stats` +
  `vfs_graph_impact("pkg:util")` returned the same numbers as the CLI at the
  repo root, including the backend-pulled file (`s3data/src/lib.rs` in the
  dependents of pkg:util after `graph warm --changed`).
- **Cross-surface coherence**: CLI `hilo meta --set user.vfs.role=library` →
  FFI `vfs_get_metadata` reads `library`; remote-only file dropped into MinIO →
  `backend sync --pull` lands it → `graph warm --changed` → FFI impact sees it
  (total 2: `src/lib.rs`, `s3data/src/lib.rs`). Backend files genuinely join
  the graph — the piece run 19 never connected because the §7.1 hook is broken.
- **Push path byte-correct against MinIO**, verified with an independent `mc`
  client: 6 files, 623 bytes, key layout exactly `s3data/...` prefix preserved.
- **Mount round-trip**: `hilo mount mnt --daemon` on the backend-backed
  project, clean unmount; `cat mnt/s3data/src/remote_new.rs` serves pulled
  backend content through FUSE.

## What broke — findings DF-WARPFS-69..74

### DF-WARPFS-69 (P1): pull re-records a "conflict" per file, every sync
Push (0 conflicts, clean) → immediate pull with zero divergence → "6 conflicts
recorded", all `{local_mtime == remote_mtime, resolved: RemoteWins}`. The LWW
planner keeps no sync state, so every run re-resolves the full key set and
logs routine decisions as conflicts. Run 19's DF-WARPFS-67 saw 3 on push; this
run shows the growth rate is per-sync-full-set, so after N syncs a clean
workspace carries ~6N rows and the ledger is noise. Fix direction is in the
row (sync-state manifest + log only true divergence).

### DF-WARPFS-70 (P2): metadata split-brain through a mount
`user.vfs.role=library` set via CLI before mounting is invisible through the
mount (`No such attribute` at `mnt/src/lib.rs` while the same xattr reads fine
on the real path), and writing xattrs through the mount is rejected
(read-only). While a mount is up, the README's headline xattr surface is a
frozen snapshot with no write path. Mount daemon loads its xattr view at
startup and never re-reads host xattrs.

### DF-WARPFS-71 (P2): README backend examples omit `--endpoint`
The working path on MinIO is `backend mount --endpoint http://…`; the README
never mentions the flag. A user following README on the project's own shipped
docker-compose MinIO lands on the env-endpoint resolution path = the
DF-WARPFS-65 empty-credential 400. The working invocation is documented here
and in skills/hilo-usage/SKILL.md.

### DF-WARPFS-72 (P3): "Total files" is really "files with edges"
2-file project, stats says `Total files: 1`; the same run's warm coverage line
says "Coverage: 6 files". `HiloHandle.vfs_graph_stats().total_files` inherits
the understated semantics with a name that doesn't say so.

### DF-WARPFS-73 (P3): every README install path assumes cargo exists
Fresh box: all documented commands die `cargo: command not found` (127) until
rustup is bootstrapped manually. libfuse3-4 present-by-default claim: VERIFIED
on stock Debian 13.

### DF-WARPFS-74 (P2): las-03 /tmp residue bites every fresh leg
Two install attempts failed on `Permission denied` redirecting logs into /tmp
(stale prior-agent uid files); attempt 3 with scratch under $HOME ran green.
Standing doctrine, recorded in the usage skill.

## Performance (Step 2b, release binary, warm)

| command | mean ± σ | runs |
|---|---|---|
| `hilo graph stats` | 19.0 ms ± 1.8 | 20 |
| `hilo graph impact pkg:util` | 16.4 ms ± 1.2 | 20 |
| full Python consumer script (9 FFI calls, import + ctor + queries) | 231 ms ± 12 | 10 |
| cwd_test.py (stats+impact+NotFound probe) | 0.23 s | 1 |

Cold start of the consumer is dominated by Python import + dlopen of the
125 MB cdylib (~200 ms); the FFI call path itself is single-digit ms. Nothing
a user waits on; **no PERF row filed** — the numbers are recorded here as the
baseline for the FFI surface.

## Verdict

🟡 PROMISING-BUT-ROUGH — the Python embedding story is genuinely good
(documented recipe works from zero on a fresh box; resolve_backend is real;
CWD-independence holds), and backend files join the graph. The rough edges are
the conflict-ledger lie (P1), the mount's frozen metadata view, and docs that
route MinIO users into the broken credential path.

time-to-first-success: ~15 min (bindgen+build warm; the pull→graph→FFI chain
worked first try). friction count: 6 findings (1 P1, 4 P2, 1 P3) + 1 doc
friction (meta --set syntax, local to the run).

Left behind: `docs/dogfood/2026-09-25-run20-python-ffi-backend.md` (this
file), diagnostics.md Run 20 section, skills/hilo-usage/SKILL.md run-20
section, board rows DF-WARPFS-69..74, dogfood-log entry.
