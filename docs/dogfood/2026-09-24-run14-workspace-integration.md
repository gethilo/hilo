# Dogfood Run 14 — Workspace / Multi-Repo Surface

**Date:** 2026-09-24 · **Build:** local release `hilo 0.3.1-dev` (build stamp v0.3.0-14-g2b26d3f)
· **Board HEAD at test time:** f9223b9 · **Verdict: 🔴 DOES-NOT-DELIVER for the
workspace surface** (CLI/graph/MCP/docs surfaces remain shippable — runs 1–13).

## The promise tested this run

Spec §6 + `docs/cli-reference.md`: *"Manage multi-repo workspace mounts"* — declare
repos in a manifest, `hilo workspace mount <MOUNT_POINT>` clones them as managed
git worktrees (`~/.hilo/worktrees/<name>/`) and serves them as peers in one FUSE
tree, writable where `writable: true`, with cross-repo `external:` edges via
`hilo graph warm --workspace`. Runs 1–13 covered CLI/graph, FUSE single-repo,
S3, FFI, git backend, plugins, triggers, permissions, Java, MCP, docs site —
the workspace subcommands were never exercised by a real user.

## What a user actually got (real fixture: two Go repos, auth-service imports shared-lib)

```console
$ hilo init                                   # OK, 23ms
$ hilo workspace mount ./mnt --manifest .vfs/manifest.yaml
error: unknown field `version`                # the help's OWN example file rejected
$ # (after reading source to learn the real schema)
$ hilo workspace mount ./mnt --manifest workspace.yaml
error: unknown field `at`                     # spec §4's example field rejected
$ hilo workspace mount ./mnt --manifest workspace.yaml
error: repos[0].auto_pull: invalid type: boolean `false`, expected u64
$ hilo workspace mount ./mnt --manifest workspace.yaml   # finally parses
error: manifest has no mounts defined         # repos[] alone mounts nothing — undocumented
$ hilo workspace mount ./mnt --manifest workspace.yaml
Mounting 2 source(s)...
  shared-lib -> mnt/shared-lib (rw)
  auth-service -> mnt/auth-service (ro)
Hilo workspace mounted at ./mnt               # ~111ms to listable (backgrounded)
$ ls mnt/auth-service/  mnt/auth-service/src/ # top level: fine
go.mod  src/                                  # src/ → EMPTY
$ cat mnt/auth-service/src/handler.go
cat: ...: No such file or directory           # EVERY nested path ENOENT (P0, DF-WARPFS-48)
$ echo hi > mnt/shared-lib/wtest.txt
bash: ...: Read-only file system              # despite "(rw)" in the log (DF-WARPFS-49)
$ hilo graph warm --workspace
No supported source files found ... 0 files   # cross-repo edges unreachable (DF-WARPFS-51)
$ hilo workspace unmount ./mnt                # clean, 10ms, daemon self-exits
```

The backend clone side works perfectly: worktrees appear in
`~/.hilo/worktrees/<name>/` with complete file trees (verified with
`git ls-files` and plain `ls`), clones/pulls provenance-clean. The FUSE layer
on top is what's broken.

## Root cause (read from source after the black-box failure)

`hilo-fuse/src/workspace_mount.rs` keeps a hardcoded inode map.
`populate_mount_children()` (line 170) indexes only the backing repo's TOP
level; a nested directory gets an inode, but `lookup()` (line 213) on a parent
inside a mount re-calls the same top-level populate — children of nested
directories are never inserted, so every nested path misses forever.
`mount_options()` (line 516) hardcodes `MountOption::RO` and
`hilo-cli/src/commands/workspace.rs:50` passes `read_only: true` — the per-repo
`writable` flag never reaches the kernel mount, while the log prints "(rw)".

## Integration guide (works-with-holes as of this run)

The workspace manifest dialect (NOT the `.vfs/manifest.yaml` that `hilo init`
writes — it will be rejected):

```yaml
# workspace.yaml — schema lives only in hilo-core/src/workspace.rs WorkspaceManifest
repos:
  - name: shared-lib
    url: /abs/path/to/remote.git   # local path or remote URL
    ref: master
    writable: false                # currently decorative — whole tree mounts ro
    # auto_pull: <seconds> if you want periodic pulls (u64, NOT boolean)
mounts:
  - source: shared-lib             # required: repos do not auto-mount
    at: mnt/shared-lib             # relative to cwd of `hilo workspace mount`
```

```bash
hilo workspace mount ./mnt --manifest workspace.yaml   # FOREGROUND — run detached
hilo workspace unmount ./mnt                            # ~10ms
```

What works today through the mount: listing the top level of each repo,
reading top-level files (go.mod read fine), clean unmount, managed worktree
clones under `~/.hilo/worktrees/`. What does not: anything nested (P0),
writes anywhere, cross-repo warming.

## Time-to-first-success

Never reached for the flagship workflow. The first FIVE commands errored;
reaching a mount took ~4 parse-error iterations, and after mounting, the
workflow still could not read a single nested source file. Honest
time-to-first-success for "read a nested repo file through a workspace
mount": ∞ (impossible today).

## Perf numbers (Step 2b)

- mount-to-listable (cold, fresh worktree clones): **111ms** — measured with a
  poll loop (bash date-stamps, `TIME_TO_LISTABLE_MS`), 2-repo fixture.
- `hilo workspace unmount` (hyperfine, warm, 5 runs): **10.0ms ± 0.6ms**.
- No PERF row filed: nothing a user would wait on; the surface's problems are
  correctness, not speed.

## Verdict rationale

🔴 for THIS surface: the documented flagship workflow (spec §6) does not
deliver a single one of its three promises (unified readable tree, per-repo
writability, cross-repo edges). The rest of hilo remains shippable per runs
7–13. Findings filed as DF-WARPFS-48 (P0), -49 (P1), -50 (P1), -51 (P2).
