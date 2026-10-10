# Dogfood Run 39 — 2026-10-10 — FUSE mount re-discovery (run-20 P0/P1 re-proof + graph-query split-brain + traversal perf) — 🟢 FUSE SHIPPABLE / 🟡 graph related silent-empty

**Tick:** warpfs-dogfood-2026-10-10-05-39-14 · **Head:** 3a62d63b · **Binary:** hilo 0.4.0 (v0.3.0-341-g3a62d63b-dirty, local debug build in target-local/, built 04:43Z — sibling run 38's build; code delta since = one test-only commit 4766da26)
**Angle:** runs 35-38 took fresh-install, foreign-repo graph, and sync surfaces on 10-10. Uncovered since 2026-09-20: the FUSE mount itself — run 20's P0/P1 set (DF-5/6/7/8/48/61) was all marked complete with fix commits and never re-dogfooded through real use; PERF-011 (first graph read stalls) still open. Store: scratch copy of the repo at /tmp/df39repo (git snapshot 008857b), mount /tmp/df39mnt — the real repo tree was never touched.

## Re-proof through real use — the run-20 FUSE findings HOLD

| Row (was) | Probe | Result |
|---|---|---|
| DF-5 (P0 empty-readdir deadlock) | `ls` of every dir incl. dogfood/ (36 files), `find` recursive | instant, complete, no hang |
| DF-48 (P0 workspace nested ENOENT) | workspace mount 2 repos, `ls mnt/hilo/hilo-fuse/src`, `find .../docs/dogfood` | all nested paths resolve |
| DF-6 (P1 stock-machine mount) | default `mount --daemon` on this box (fusermount3 3.18.2) | mounts in 12ms setup |
| DF-7 (P1 ignore stack) | `ls /tmp/df39mnt/.git` | ENOENT — excluded correctly |
| DF-8 (P2 fabricated mtimes) | stat mount vs disk for README.md, Cargo.toml | byte-identical mtimes |
| DF-29 (P1 subdir trigger events) | foreground trigger mount, CREATE in hilo-fuse/src/ (nested) | fired ~6-10s later: `parse-and-diff: 5 impacted files` |
| DF-30/32 (P1 db lock / leak) | graph query WHILE trigger mount up; unmount + ps | no lock held; zero leaked processes |
| DF-115 (P1 banner-before-validate) | (fix e4655347 in binary) | not re-probed this run |

Read-only contract: writes through the mount get `Read-only file system` (EROFS) — documented at docs/hilo-permissions.md:19, honest rejection, exit non-zero. Xattr passthrough: `hilo meta` set on repo path, read back through the mount with BOTH `hilo meta` and `getfattr` — cross-surface clean (DF-70's mount-side half works; its pre-mount-write half untouched this run).

## Finding 1 — DF-WARPFS-133 (P1): `graph related` silent-empty on ANY absolute path

`hilo graph related /tmp/df39mnt/hilo-fuse/src/ops.rs` → "No outgoing edges" exit 0. Same file via relative path → full edge list. The absolute path of the UNDERLYING repo (/tmp/df39repo/...) is equally empty, so it is absolute-path resolution, not mount-path resolution. `graph impact` and `graph understand` handle the same absolute paths correctly — only related diverges. An agent browsing the mount (or any script with absolute paths) gets an honest-looking "no edges" instead of an error. The split-brain class DF-70 named for xattrs, now on a query verb.

## Finding 2 — DF-WARPFS-134 (P2): `--daemon` discards stderr = the trigger engine's only channel

`mount --triggers --daemon` produces zero observable trigger activity (5s windows, both edit directions). Foreground control: the same edit fires cleanly (~6-10s: inotify + 500ms debounce + lazy DuckDB open). Daemon mode captures stdout only; mount.rs:50-52 sends trigger logging to stderr. A daemonized trigger mount is observably dead — the "is it working?" question has no answer. Fix: daemon routes stderr to a file and the banner names it; document the latency envelope.

## Finding 3 — DF-WARPFS-135 (P1, PERF): mount traversal 14-20s cold / 1.2-2.1s warm vs 9ms native

Commands and numbers (222-file corpus, graph warmed BEFORE mount; graph.db mtime untouched by the mount — pre-warm buys nothing):
- cold first `find -name '*.rs'`: 14.00s (first mount), 19.87s (second mount)
- warm: hyperfine 1.162s ±0.039 (×10) / 2.082s ±0.455 after remount; native control 0.009s (~129x)
- per-op latency fine: one-dir ls 4.7ms; ~7000 FUSE round-trips × 0.16ms explains warm cost — it is per-entry CPU in the daemon.
- `perf record -F 199 -p <daemon>` during 8 traversals, read numerically: ≈52% of samples in std::path parse/compare inside `readdir::{closure#0}`→`Path::parent()` (14.38% parse_next_component_back + 9.87% next_back + 5.93% Components::eq + 5.49% Rev-try_rfold + 4.41% Component::eq + 2.07% compare + 1.69% PathBuf::eq); ≈12.6% in `populate_directory`'s linear `.any()` over the whole inode map per listing (5.71% RawIterRange + 3.68% Values::try_fold + 3.18% Iter::next). Hot paths: hilo-fuse/src/ops.rs readdir/populate_directory. Updates PERF-011 (2.7-8.0s was a 5-file project; 222 files → 20s cold).

## Value judgment

The FUSE mount is the flagship and it now delivers: browse, read, xattr passthrough, honest read-only, clean unmount hygiene, triggers that genuinely update the graph. The blockers a real agent user hits today are the silent-empty query verb (133) and the traversal latency on any real corpus (135); both have crisp fix directions. Time-to-first-success for "browse + read + query through the mount": ~1 min. Friction count: 3 (findings 1-3; workspace manifest dialect errors are known DF-50 and were not re-filed).

## Honest gaps

- Install leg: SKIPPED-with-citation — runs 35 (26m32s) + 36 (24m37s) proved the documented build on fresh bunker boxes within 24h; code delta since = one test-only commit (4766da26), zero Cargo dep drift (`git diff --stat 1195639a..3a62d63b -- Cargo.toml Cargo.lock` empty). A third leg proves nothing new.
- Workspace surface: taken by sibling run 38 (tick warpfs-dogfood-2026-10-10-05-39-14's sibling my-project-dogfood run at 05:14, rows DF-128..132). This run independently re-proved DF-129 (writable:true inert, EROFS while banner prints rw) on a separate manifest — cite, don't duplicate.
- Trigger-fire latency measured only through one 6-10s envelope; no distribution.

Left behind: this file + board rows DF-WARPFS-133/134/135 (created_by: dogfood-warpfs-run39) + dogfood-log entry + skills/hilo-usage/SKILL.md pitfalls. No code changes.
