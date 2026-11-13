# warpfs-ops-2026-11-13-20-33-27 — RELEASE-020 re-verification (second fire)

Lane: OPS (warpfs). Actor `warpfs-ops`. Worktree measured: `/home/kara/warpfs`
(the tick-declared `/home/kara/.hermes/stand-in/pm-lane/warpfs` is a stand-in path, not a git
work tree — the project repo is `/home/kara/warpfs`).

Measured HEAD (clean tree, in sync with `origin/master`):

```
$ git rev-parse HEAD                     -> 235de763d90972995467fe4a4060fa6e8cdc9723
$ git status -sb | head -1               -> ## qa-wtick-20261113-165414...origin/master
$ git rev-list --left-right --count HEAD...origin/master -> 0	0
$ git log -1 --format='%h %ad %s'        -> 235de76 2026-11-13 ops(warpfs): annotate RELEASE-020 with live verification at 6a12726f
```

The lock fix that RELEASE-020 asks for is `c89bf2d` ("fix(deps): remediate runtime RustSec
advisories", 2026-11-10), ancestor of HEAD and contained in `origin/master`
(`git branch -r --contains c89bf2d` -> 2 refs incl. origin/master).

## Row under verification

`RELEASE-020` (P1, pending) — acceptance:

> PASS: at the fix HEAD, `cargo audit` reports no vulnerability affecting a package in the normal
> runtime dependency graph printed by `cargo tree`; `cargo tree` reports a patched wasmtime version
> that clears the published advisory ranges and the plugin-execution tests return 0; `cargo fmt`,
> `cargo build`, `cargo clippy`, and `cargo test` each return 0; then the v0.4.1 release asset
> reports 0.4.1 and the release carries the binary plus a sha256 sidecar.

## Arm 1 — audit: no advisory on a NORMAL (runtime) dependency edge

```
$ cargo audit 2>&1 | tail -3
error: 8 vulnerabilities found!
warning: 15 allowed warnings found

$ jq -r '.vulnerabilities.list[] | .advisory.package+" "+.advisory.id' <cargo audit --json>
zip RUSTSEC-2026-0429 / -0369 / -0368 / -0367
rkyv RUSTSEC-2026-0235
quinn-proto RUSTSEC-2026-0524 / -0523
quinn-udp RUSTSEC-2026-0538

$ cargo tree --workspace --edges normal  <pkg set>  ∩  <advisory pkg set>
quinn-proto not-in-normal-graph
quinn-udp   not-in-normal-graph
rkyv        not-in-normal-graph
zip         not-in-normal-graph
```

`zip` is a build-dependency of `libduckdb-sys`, `rkyv` is an unenabled `rust_decimal` feature, the
`quinn*` crates come in through a non-runtime edge. Gate result:

```
$ timeout 300 bash scripts/audit-gate.sh   -> rc=0   (PASS: no runtime-reachable advisory)
```

Arm 1: **PASS** — advisories exist in the lock, none reachable through normal edges.

## Arm 2 — patched wasmtime + plugin tests

```
$ awk '/^name = "wasmtime"/{getline; print}' Cargo.lock   -> version = "49.0.5"
$ cargo tree -i wasmtime                                  -> wasmtime v49.0.5 via extism v1.41.0 -> hilo_plugins -> hilo-cli
```

wasmtime 43.0.2 (the RUSTSEC-affected version named in the row) is no longer in the lock.

```
     Running tests/plugin_test.rs             -> test result: ok. 17 passed; 0 failed
     Running tests/dfwarpfs59_init_no_plugins -> test result: ok.  1 passed; 0 failed
     Running tests/plugins_not_implemented    -> test result: ok.  2 passed; 0 failed
```

Arm 2: **PASS**.

## Arm 3 — fmt / build / clippy / test

```
$ cargo fmt --all -- --check              -> rc=0
$ cargo build --workspace                 -> rc=0
$ cargo clippy --workspace --all-targets  -> rc=0 (0 warning lines)
$ cargo test --workspace                  -> rc=0, 71 suites, 1669 passed, 0 failed
```

`cargo test` only returns 0 after two host-state faults are removed. Both are recorded because the
first fire and the QA lane attributed the failures to the product; they are environmental:

**(a) FUSE mount denied under the ambient TMPDIR.** With `TMPDIR=/var/tmp/hermes-scratch`,
`dfwarpfs30_trigger_mount_lock::test_trigger_mount_does_not_block_graph_readers_or_second_mount`
fails at its 30 s deadline:

```
    fusermount3: mount failed: Permission denied
    error: FUSE mount failed: Operation not permitted (os error 1)
```

Direct probe (`mntprobe.sh`, mount + wait + list):

```
--- scratch TMPDIR (/var/tmp/hermes-scratch)
  NOT READY after ~10000ms; mnt listing: []
  fusermount3: mount failed: Permission denied
  error: FUSE mount failed: Operation not permitted (os error 1)
--- HOME TMPDIR (/home/kara/.hermes/cache/scratch)
  READY after ~500ms; content: package main
```

Same binary, same tree, different TMPDIR → the mount is a host policy limit on the scratch
filesystem, not a regression. (First fire reached the same conclusion; this fire re-proved it
end-to-end.)

**(b) Stale `$HOME`-level graph cache poisons the cli.rs graph tests.** With `TMPDIR` moved under
`/home/kara`, two different tests failed instead:

```
test graph_accepts_file_prefixed_path_and_teaches_id_forms ... FAILED  (DuckDB Binder Error:
     Referenced column "granularity" not found in FROM clause!  Candidate bindings: "rel")
test graph_stats_no_data_prints_message ... FAILED  (graph reported the /home/kara tree:
     Total edges: 29, Total files: 2, Components (948 total): .hermes … .git.quarantine-20261103 …)
```

Root cause, measured:

```
$ ls -la /home/kara/.vfs/graph/graph.db
-rw-rw-r-- 1 kara kara 1847296 Sep 11 15:51 /home/kara/.vfs/graph/graph.db
$ ls -l /proc/*/cwd | grep -c '\.vfs'      -> 0        (no live consumer)
$ sqlite3 -readonly .../graph.db 'PRAGMA table_info(edges);'
Parse error: file is not a database (26)   (it is DuckDB, as expected)
```

A pre-`granularity` DuckDB graph from 2026-09-11 sitting at the `$HOME` root. `hilo` resolves its
project root by walking up from the working directory, so every invocation whose workspace lives
under `/home/kara` adopts this stale cache — including the test tempdirs.

**Bounded, reversible host fix applied:**

```
$ mv /home/kara/.vfs /home/kara/.vfs.quarantine-20261113   -> rc=0   (rename, no deletion)
$ grep -rIl -e '/home/kara/\.vfs' -e '$HOME/\.vfs' /home/kara/.hermes/scripts  -> no hits
```

Result after quarantine, same tree, same TMPDIR:

```
$ cargo test -p hilo-cli --test cli   -> test result: ok. 85 passed; 0 failed (previously 2 failed, 77s)
$ cargo test --workspace              -> rc=0, 71 suites, 1669 passed, 0 failed
```

`/home/kara/.vfs` was not re-created by the suite. Rollback is `mv /home/kara/.vfs.quarantine-20261113 /home/kara/.vfs`.

Arm 3: **PASS**, conditional on the two host-state conditions above (FUSE-permitted TMPDIR;
no `$HOME`-root graph cache).

## Arm 4 — v0.4.1 release artifact

```
$ gh release view v0.4.1 --json tagName,assets
tagName: v0.4.1
assets: hilo-0.4.1-x86_64-linux.tar.gz (32,735,042 B)
        hilo-0.4.1-x86_64-linux.tar.gz.sha256 (178 B)

$ gh release download v0.4.1 -D <fresh dir>
tarball_sha = a96838c48a2dd772ab11eea1122589aa131d7d8679608d63ba26a1eeb531718c
sidecar_sha = a96838c48a2dd772ab11eea1122589aa131d7d8679608d63ba26a1eeb531718c
$ tar -xzf … && ./x/hilo --version   -> hilo 0.4.1  (build: v0.4.0-50-g4216c7f)
$ tar -tzf …                        -> README.md, LICENSE, hilo
```

Arm 4: **PASS**.

**Artifact defect found while verifying (separate, not blocking arm 4):** the published sidecar
embeds the builder's absolute path, so `sha256sum -c` only works on the build host and silently
validated an unrelated local copy here:

```
$ cat hilo-0.4.1-x86_64-linux.tar.gz.sha256
a96838c4…  /home/kara/.hermes/cache/scratch/warpfs-releng-2026-11-12-16-02-23/assets-v0.4.1/hilo-0.4.1-x86_64-linux.tar.gz
$ sha256sum -c *.sha256     -> ...: OK     (checked the stale local path, NOT the download)
```

The hash itself matches the real tarball (compared above). Release tooling should emit a bare
filename in the `.sha256` line.

## Verdict

All four acceptance arms are now measured PASS at HEAD `235de76` (lock fix `c89bf2d` ancestor,
tree pushed). The row is left `pending` only because `boardctl update --status complete` requires
`--evidence-run-id` bound to a `.gitreins/history/<date>/<run>/` directory (TR-251) and this fire
produced no judge run. A foreman tick that runs one gitreins assessment at this tree can close it
in a single step; nothing else is outstanding.

## Left alone deliberately

- No lane disabled, no scheduler.db write, no service restart (nothing here runs `hilo` as a
  daemon; the two stale installed binaries are packaging scope, RELEASE-021).
- No `/etc/apparmor.d` change for `/var/tmp/hermes-scratch` — root-level host policy, outside a
  project worktree.
- No second board row filed: the `$HOME/.vfs` cache and the TMPDIR/FUSE behaviour are host state,
  recorded here and in the RELEASE-020 event rather than as new rows.
- Nothing deleted: `/home/kara/.vfs` is quarantined by rename only.
