# Hilo (warpfs) dogfood — 2026-09-20 run 7 (FUSE mount deep dive)

**Verdict: 🟡 PROMISING-BUT-ROUGH** — the query engine is solid, but the
flagship FUSE mount is unusable on a stock machine and hostile to the
traversal tools agents actually use.

This run deliberately took the angle front-to-back runs (`docs/dogfood/`
2026-08-13 … 2026-09-20 run 6) never covered: the **mount surface**, not the
CLI/CAG surface. Everything below is measured, not asserted.

## Verdict basis

| Question | Answer | Evidence |
|---|---|---|
| Does it work? | Partly. CLI/graph surface clean; mount works only on hosts that already have `user_allow_other` | mount fails EXIT=1 on fresh Debian 13 |
| Is it useful? | The graph queries are. The mount, as shipped, is not — an agent that touches it hangs | `find` over the mount returns 0 rows in 15s |
| Is it usable? | Docs never mention the two things that decide whether the mount works | `hilo-fuse.md` documents only `hilo mount /mnt/hilo --triggers` |
| Is it trustworthy? | Data integrity is good (content/size/xattr fidelity verified); metadata honesty is not | all mtimes fabricated to mount time |

## What was actually verified working (so the rough verdict is fair)

Measured on this box, HEAD `b83a68f`, binary built 19:10 (brand-new build,
`ops.rs` last touched 2026-08-26, so the binary post-dates the source).

- Mount established: `hilo mount <dir> --daemon` → `mount` shows
  `hilo on <dir> type fuse (ro,...)`. Unmount clean via `fusermount3 -u`,
  no leftover `hilo mount` process.
- **Content fidelity is exact.** `stat -c %s` and `sha256sum` match disk for
  every file sampled (README.md 11439, Cargo.lock 155433, Cargo.toml,
  .gitignore, docs/index.md, hilo-cli/src/main.rs).
- **The flagship promise holds through the mount.** A file classified by
  `hilo classify` shows `user.vfs.role` / `user.vfs.status` both on disk and
  through the mount, and `hilo meta <mount-path>` resolves it. This is the
  claim "query via getfattr instead of reading files" — it is real.
- Traversal is **fast** where it works: `find <mount>/hilo-cli -type f` → 16
  files in 110 ms; `ls -R <mount>/hilo-cli` → 1 s.
- Write attempt correctly refused: `Read-only file system`.
- Same 105 ms/call syscall latency on disk and through the mount — the mount
  adds no measurable per-call overhead (that cost is this host's load, ~14).

## Findings (filed as DF-WARPFS-5..8)

### DF-WARPFS-5 [P0-ish] — readdir on an EMPTY directory never returns

Deterministic, reproduced in isolation on a purpose-built repo:

```
MOUNT ls -a emptydir        rc=124 ms=8016   out=[ ]     <- hung, killed by timeout
MOUNT ls -a withfile        rc=0   ms=109    out=[. .. a.txt]
MOUNT ls -a withsub/sub     rc=0   ms=111    out=[. .. b.txt]
DISK  ls -a emptydir        rc=0   ms=111    out=[. .. ]
```

- Every non-empty directory answers in ~110 ms. Every **empty** directory
  hangs until killed (8 s cap, 12 s cap, 15 s cap, 20 s cap — all rc=124/0
  with zero output).
- `find <mount> -type f` therefore returns **nothing** on any repo that
  contains one empty directory: 3/3 runs, 0 rows, 15 s each, against 24 rows
  in 111 ms on disk.
- Cause (read from source, `hilo-fuse/src/ops.rs:277-365`): `readdir` always
  `reply.ok()`s, but when `files.get(&ino)` is missing it returns **without
  replying** (line 315-321). An empty directory is exactly the case where
  `populate_directory` inserts no children, so the inode never lands in the
  map for that lookup path and the kernel waits forever on a reply that never
  comes.
- Impact: any agent that walks a mounted tree with `find`/`git status`/
  recursive globbing gets **no results and no error**. Silence, not failure —
  the worst possible shape for an agent tool.

### DF-WARPFS-6 [P1] — `hilo mount` fails on a stock Debian box (forced allow_other)

On the fresh ephemeral bunker agent (Debian 13, nothing preinstalled but the
base image; `/etc/fuse.conf` has `user_allow_other` commented out — the
default everywhere):

```
$ hilo mount $HOME/mnt
Hilo mounted at /home/bunker-bd4b20a4/mnt
fusermount3: option allow_other only allowed if 'user_allow_other' is set in /etc/fuse.conf
error: FUSE mount failed: Operation not permitted (os error 1)
EXIT=1
```

- Nobody asked for `allow_other`: `hilo mount --help` shows it as an opt-in
  flag and the generated manifest says `allow_other: false`. The mount still
  carries `allow_other` in its option vector.
- Mechanism, traced into the dependency: `fuser` 0.15.1 documents it at
  `src/mnt/mount_options.rs:26` — *"AutoUnmount requires AllowOther or
  AllowRoot. If AutoUnmount is set and neither is set, the FUSE configuration
  must permit allow_other, otherwise mounting will fail."* Hilo sets
  `auto_unmount: true` (`hilo-cli/src/commands/mount.rs:67`) while passing
  `allow_other: false`, so the mount is refused.
- Why this stayed invisible for months: this dev host has `user_allow_other`
  set, so the mount succeeds locally. `hilo mount --help` exposes no way to
  turn `auto_unmount` off, so a user on a stock box has **no CLI path to a
  working mount**.
- Impact: the flagship feature of an "agent-first virtual filesystem" fails
  for every user who has not first edited a root-owned system config file —
  and the docs never say so. `hilo-fuse.md`'s only example is
  `hilo mount /mnt/hilo --triggers`.

### DF-WARPFS-7 [P1] — the ignore stack does not reach the mount

- `hilo ignore check target/` → `ignored: true`. `hilo ignore check .git/` →
  `ignored: true`. The project's own policy classifies both as non-content.
- The mount serves both anyway: `ls <mount>` lists `target`, `.git`, `.vfs`,
  `.pytest_cache`; `ls <mount>/target` shows `CACHEDIR.TAG debug release tmp`
  on a workspace whose `target/` is **130 GB**.
- Cause: `grep -rn ignore hilo-fuse/src/*.rs` returns **zero hits**. The
  mount builds its tree with a raw `std::fs::read_dir` walk
  (`hilo-fuse/src/ops.rs:125`), so ignore policy applies to graph/ephemeral
  surfaces but not to the mount.
- Contradiction with the project's own architecture doc: `hilo-core/src/
  manifest.rs:458` describes an "included subtree — becomes visible to
  discovery" concept, and `docs/hilo-fuse.md` describes a virtual filesystem
  that "mounts repositories and backends as a unified directory tree". What
  ships is a raw mirror of the working directory.
- Impact: the mount is meant to be the zero-copy surface an agent queries. A
  tree containing `target/` and `.git/` makes every traversal (when it works
  at all) both wrong and enormous. The only thing that saved this run was the
  ignore *stack* still being correct for the CLI — the two surfaces disagree.

### DF-WARPFS-8 [P2] — the mount fabricates timestamps

```
README.md   disk mtime=2026-09-19 20:13:55   mount mtime=2026-09-19 20:47:12
Makefile    disk mtime=2026-07-19 12:15:26   mount mtime=2026-09-19 20:47:12
LICENSE     disk mtime=2026-06-24 20:09:01   mount mtime=2026-09-19 20:47:12
```

Every file reports an mtime equal to *the moment of the stat call* — the two
independent mount sessions in this run wrote different fabricated values
(20:42:55 in one, 20:47:12 in the other) for the same unchanged files. Sizes
are real (verified byte-exact); only the timestamp is invented. `getattr`
never consults the backing file's metadata.
- Impact: incremental tooling that uses mtime for change detection (`make`,
  `git status` heuristics, agent caches keyed on freshness) sees a repository
  where **everything changed on every stat**. An agent asking "what changed
  recently?" through the mount gets an answer that is not merely wrong but
  changes every time it asks.

## Time-to-first-success / friction

- CLI path (init → warm → classify → query): seconds to low minutes — this
  remains the strong surface and the earlier runs' measurements hold.
- Mount path: **never reached** on the fresh box (EXIT=1, blocked by
  DF-WARPFS-6), and on the dev box no tree traversal ever completed
  (DF-WARPFS-5). Friction count for this run: **3 blocking** (empty-dir hang,
  allow_other, ignore-not-applied) + 1 honesty defect + 2 doc gaps.

## Install leg (ephemeral bunker, las-bunker-03)

PASSED — and this is the third independent confirmation of installability.

- Agent `bd4b20a4` @ 100.69.3.13, TTL 2h, destroyed at end of run.
- Fresh Debian 13, nothing preinstalled: rustup minimal → clone
  `github.com/gethilo/hilo` at `b83a68f` (same HEAD as this checkout) →
  `cargo build --release` **RC=0 in 1217 s (20m17s)** → `--version`,
  `--help`, `init`, `graph warm`, `graph stats` all pass.
- Smoke numbers from the fresh build: `975 edges across 97 files
  (4 languages)`, `776 distinct / 964 raw`, coverage `103 files = 97
  contribute + 1 facade + 5 no imports`.
- README's "15-20 min" build claim holds again (1146 s / 1142 s / 1217 s
  across three boxes).
- **`hilo mount` was then attempted on that same fresh box and failed**
  (DF-WARPFS-6). The install leg passing does not imply the mount works —
  exactly why both legs exist.
- MCP surface re-verified from the fresh binary: `tools/list` returns
  **17 tools**, matching README/AGENTS.md. (The `serve` help text in
  `hilo-cli/src/main.rs:27` and `SKILL.md` still say 15 — stale, filed under
  the same doc-drift family.)

## What this run could not test (stated explicitly)

- Multi-user access semantics of `allow_other` (needs a second real user).
- Stream/mirror backend mounts (`.vfs/backends/mounts.yaml` unpopulated).
- `--triggers` inotify behaviour under sustained writes.
- Rootless-Docker/FUSE interaction: the bunker agent mounts would need
  `user_allow_other` there too, which is the finding, not a workaround.

## Files left behind

- `docs/dogfood/2026-09-20-run7-fuse-integration.md` (this report)
- `docs/dogfood/diagnostics.md` — Run 7 section (mechanisms + how to reproduce)
- `skills/hilo-usage/SKILL.md` — FUSE pitfalls added (empty-dir hang, mount
  prereq, ignore gap)
- Board rows `DF-WARPFS-5..8` in `.coding-hermes/board/tasks.jsonl`
- `.coding-hermes/dogfood-log.md` — run entry
