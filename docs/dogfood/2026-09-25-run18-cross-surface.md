# Dogfood Run 18 — 2026-09-25 (warpfs-dogfood): cross-surface integration session

## Angle

Runs 1–17 each drove ONE surface in isolation (CLI/graph, FUSE, S3 backend,
git backend, plugins, triggers, permissions, MCP ×2, docs site, workspace
sync, Go FFI). No run ever used the surfaces TOGETHER, the way a real agent
session does: write metadata via the CLI, read it through the FUSE mount and
the MCP server; edit a file and watch the trigger engine update the graph;
then ask the SAME structural question on every surface and compare the
answers. That cross-surface coherence is the untested promise, and it is
where run 18 found a P0.

Promise under test: *"an agent can use Hilo's surfaces (CLI, FUSE mount, MCP
server) interchangeably against the same project and get coherent answers,
while the trigger engine keeps the graph current."*

HEAD tested: 47f153b (release binary 0.3.1-dev, build 47f153b; origin/master
== local master).

## What worked (real-use verified)

- **Integrated session core loop**: `init` (12ms) → `graph warm` (8.7ms
  cached) → `meta --set user.vfs.role` via CLI → read back through the FUSE
  mount (`getfattr` returns the CLI-set value) and through MCP
  `vfs_get_metadata` (`user.vfs.role: core-tokenizer`). One write, three
  consistent readers. This is the product's core promise and it held.
- **MCP round-trips**: `vfs_graph_related`, `vfs_graph_impact`,
  `vfs_graph_stats`, `vfs_resolve_path` all answered correctly and matched
  the CLI's answers, query for query (serverInfo.version now correctly
  0.3.1-dev — run 15's drift row fixed).
- **Trigger engine (DF-WARPFS-28/29 fixes verified live)**: with the stock
  `triggers: []` manifest, `mount --triggers` now loads defaults; a nested
  file edit (`svc/api/src/client.rs`) appended the new
  `imports → pkg:parser` and `imports → pkg:std` edges to edges.jsonl
  within ~2s, and `graph stats` reflected them WITHOUT a manual warm.
  `hilo mount` on a stock Debian 13 box now mounts successfully
  (MOUNT_RC=0, mountpoint up) — run 7's DF-WARPFS-6 stock-machine failure is
  fixed on the same box class that originally failed it.
- **Concurrency (DF-WARPFS-30 fix holds)**: MCP impact/stats queries and a
  full `graph warm` all succeeded WHILE the trigger-mount daemon was up —
  0/30 anomalies on steady-state churn. The DuckDB file-lock deadlock run 10
  found is genuinely gone in steady state.
- **CLI impact refresh**: after adding a new dependent file, warm+impact
  correctly lists BOTH dependents (`main.rs`, `client.rs`) via the pkg:
  node.
- **FUSE fidelity**: sizes, xattrs, readdir all consistent with disk;
  inner-tree mount correctly excludes `.vfs/` from the served view.

## What broke (findings → rows)

### DF-WARPFS-61 (P0): cold-start trigger reconcile leaves a WRONG graph.db and warm cannot heal it

Sequence, reproduced end-to-end:

1. Trigger mount daemon starts against a freshly rebuilt `graph.db` and
   runs its cold-start reconcile.
2. An edit fires during the startup window.
3. Result: `graph stats` served **"Total edges: 2 distinct / 10 raw"** while
   edges.jsonl (the inventory, the declared source of truth) held 5 distinct
   edges. The 2-edge view PERSISTED across repeated queries — every
   `graph impact` from that point on silently missed dependents (e.g. the
   pkg:parser impact answered 1 dependent where the true graph had 2+).
4. The query window ALSO caught: `t01` — an empty-graph read
   ("Graph cache is empty. No edges parsed yet.") seconds after a successful
   warm, and `t02` — a raw DuckDB lock-conflict error leaking to the CLI
   user (`Conflicting lock is held in .../hilo (PID …)`). The DF-WARPFS-30
   fix covers the steady state but NOT the daemon's cold-start reconcile
   window.
5. **The heal path is broken**: `graph warm` says
   "Discovered 4 edges across 3 files [all cached, graph unchanged]" and
   exits 0 while writing an EMPTY graph.db. The `.parse_cache.json` says
   "unchanged", so warm never repopulates the database it just (re)created.
   Recovery requires deleting BOTH `graph.db` AND `.parse_cache.json` —
   a recovery a real user cannot discover, while the tool claims success.

Warm and cold evidence: warm graph-state churn
`6→8 raw` normal; the failure needed the exact cold-start sequence
(fresh/rebuilt graph.db + first daemon start + edit in window) and
reproduced on the first deliberate attempt. Filing as P0 because the
resulting impact answers are silently wrong and survive.

Fix direction: (a) the daemon's cold-start reconcile must merge against
edges.jsonl, not overwrite from a partial parse — or defer reconcile until
the first watched event settles; (b) warm must treat "graph.db missing/
freshly created" as a cache miss regardless of parse cache; (c) readers
must retry (bounded) on the cold-start lock window instead of surfacing
DuckDB's lock error; (d) regression test: warm → start trigger mount →
edit immediately → poll stats N times → assert stats always agree with a
distinct-count of edges.jsonl.

### DF-WARPFS-62 (P2): trigger re-parse re-appends existing edges — duplicates grow unboundedly in edges.jsonl

Each edit that re-parses a file appended ALL of that file's edges again,
including previously-known ones. Observed: `client.rs → pkg:parser` present
×2 after the second real edit, ×3 after the third; distinct count stayed
correct (dedup happens at query time) but raw lines grow monotonically.
Because edges.jsonl is the declared inventory/ground truth and is
git-committed (inventory policy), every duplicate is churn in a tracked
file and inflates the "raw" number users see in stats. Evidence in
`/tmp/dogfood-warpfs-r17/.vfs/graph/edges.jsonl` at capture time; fix
direction: the incremental append should diff against the file's existing
edge set (the same dedup rule warm uses) before appending.

### DF-WARPFS-63 (P1): `hilo mount <path-inside-project>` on a fresh machine hangs on every read

On the fresh Debian 13 install leg: `hilo mount ~/smoke-r18/mt --daemon`
(the mount point is INSIDE the served tree, which is how a user testing
hilo in its own repo would mount it) reported success
("Hilo mount daemonized … filesystem available"), `mountpoint` returned
true, and then EVERY read hung: `ls <mount>` (timeout 5s), `getfattr` on a
file, `cat` — all blocked, twice, across two separate mount attempts. The
daemon process stayed alive the whole time; `fusermount3 -uz` was needed to
clear the mount. On the control dev host the same inner-tree mount pattern
works flawlessly (readdir, getfattr, cat all fine, `.vfs/` correctly
excluded), so the hang is environment-dependent — same box class that
exposed DF-WARPFS-6, and the same lesson as run 7: install-green ≠
mount-works on a stock box. 6-core Debian 13, rustup 1.98.1, release build
24m19s (BUILD_RC=0). Evidence: install-leg transcript, smoke commands with
exit 124. Fix direction: reproduce on a stock box, capture the daemon's
stderr tracing (a user cannot — daemon logs went to /dev/null; note: the
--daemon flag eats the tracing subscriber run 29-fix added), and check for
a mount-point-inside-root FUSE recursion/dust-off difference between the
dev box and stock kernel/fuse3 versions.

### DF-WARPFS-64 (P2): `graph stats` does not rebuild from edges.jsonl when graph.db is missing

Deleting `graph.db` (the declared cache — manifest says inventory is truth)
leaves every graph command reporting "Graph cache is empty. No edges parsed
yet." with exit 0 instead of rebuilding from `.vfs/graph/edges.jsonl` — the
rebuild the README/design promises. Combined with DF-WARPFS-61 this makes
cache loss require an undiscoverable two-file deletion (db + parse cache).
Fix direction: treat missing graph.db as a trigger for a full rebuild from
the JSONL inventory.

## What was measured (Step 2b)

Release build, 4-file fixture, warm unless stated:

- `graph impact svc/parser/src/lib.rs --max-depth 3`: **33.5ms ± 6.8ms**
  (n=20, hyperfine). Nothing to profile.
- Same command with `--prepare 'rm graph.db'` (cold cache): **1.54s ± 0.74s**
  (n=10) — that cold path IS the DF-WARPFS-61/64 recovery gap in numbers.
- `graph warm` (cached): 8.7ms ± 0.5ms (n=10).
- MCP spawn+initialize+`vfs_graph_impact`: median 20ms (n=10, cold server
  per session).
- Fresh-box install: rustup+build 1493s (~25min) on 6 vCPU — within the
  README's claim once toolchain install is counted separately; the build
  itself was 24m19s, near the README's 15–20min estimate and faster than
  run 8's 33m45s.

Nothing a user would notice as slow in normal operation → no PERF row
filed; the one slow number (1.54s cold-graph impact) is folded into
DF-WARPFS-61/64 where it belongs.

## Install leg (ephemeral bunker, las-bunker-03)

- Agent e4db409b, fresh Debian 13 (trixie), public clone of
  github.com/gethilo/hilo at 47f153b == origin HEAD (8.6s clone; no
  credential minted, no visibility change).
- Documented path from zero: rustup minimal 1.98.1 (curl installer) →
  `cargo build --release -p hilo-cli` → BUILD_RC=0 in **1493s**.
- Smoke (initial attempt): FAILED silently because `/tmp/smoke` was owned
  by uid 1004 — residue from a PREVIOUS agent's user on the shared tmpfs
  (infra gotcha, not a hilo defect; recorded here so future install legs
  use `$HOME` smoke dirs).
- Smoke (clean dir, run by hand): init OK, warm OK (1 edge/1 file),
  meta set+read OK, **mount --daemon MOUNT_RC=0 and MOUNT_UP on stock
  Debian 13** (DF-WARPFS-6 fix verified) — then reads hung (DF-WARPFS-63).
- MCP initialize verified separately on the box earlier in the run (via
  the /tmp/smoke2 project; serverInfo 0.3.1-dev).
- Agent destroyed, key removed. Destroy ran even after failures.

## Verdict

**🟡 PROMISING-BUT-ROUGH** for the integrated-session surface. Individually,
every surface passed again (many earlier fixes verified live: DF-WARPFS-6,
28/29/30, MCP version drift). Together, they disagree exactly when the
trigger daemon, the parse cache, and the DuckDB cache interact at
cold-start: the flagship impact query silently serves a wrong graph and
`warm` — the documented recovery — claims success while writing an empty
database. The single-surface green stacks hid a cross-surface coherence
defect that only this angle could see.

Time-to-first-success: ~10 min (init → warm → meta → mount → all surfaces
answering). Friction count: 4 defects (1×P0, 1×P1, 2×P2).

## Left behind

- docs/dogfood/2026-09-25-run18-cross-surface.md (this file)
- docs/dogfood/diagnostics.md — Run 18 section
- skills/hilo-usage/SKILL.md — run-18 section (cross-surface recipe +
  cold-start recovery rule)
- Board rows DF-WARPFS-61..64
- .coding-hermes/dogfood-log.md — run 18 entry
