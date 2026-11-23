# Dogfood run 51 — 2026-11-23

Tick: warpfs-dogfood-2026-11-23-18-36-23. Tested binary: Hilo 0.4.1, build v0.4.1-125-g675e6ed-dirty. Repository tip at start: ca3043a; the binary is not a verified build of that tip. Results below describe the tested binary, not a current-tip certification.

On a fresh clone of the heading repository, `hilo init`, `graph warm`, `graph stats`, `graph impact`, `graph search`, `graph understand` and `graph untested --granularity function` ran. MCP stdio initialized, listed 18 tools and returned real graph stats. A scratch HTTP server returned 200 for health and metrics and 404 for an unknown route. A single mount exposed a backing-file xattr.

The cold warm command took 13.624 s wall and the cached repeat 1.427 s, on a 395-file corpus, at load approximately 12.7–15.4 on 16 cores. This was a first warm after cloning, not an empty inventory: the clone contained a tracked .vfs inventory which was reconciled. HILO_WARM_TIMING attributed 4.05 s to GraphDB open including reconciliation and 3.25 s to insert_edges. These are wall-phase timings, not a profiler trace. Warm impact averaged 250 ms under higher load; the first stats command was 341 ms and the second 58 ms. The initial map-building wait is noticeable and already tracked as PERF-016; no new performance row was filed.

Warm reported 2711 discovered edges; the subsequent inventory and stats showed 2670 rows. The interpretation of the difference is not established. Existing pending row DF-WARPFS-43-R2 already owns the reconciliation question; no duplicate was filed.

Workspace mount investigation: a workspace manifest with URL, ref, and mounts mounted two repositories. Initial read-through failures were a probe mistake: the manifest caused Hilo to clone a separate backing tree, whereas the xattr was set on the source clone. A subsequent control set the xattr on the actual backing tree and read it through the workspace mount successfully. On the same writable workspace mount, setting an xattr through the mount returned EOPNOTSUPP on this older binary. That observation is related to DF-WARPFS-70, but was not established on a current-tip build, so no recurrence was filed.

Install: bunker-qa launched agent 7b01b53f on bunker-las-03 against repository snapshot ca3043a. Collection reported RUNNING and six synced evidence rows; the evidence file has eight lines including launch metadata. No install cell had finished at collection. The agent was left running for subsequent collection; installation is not verified.

Probe caveats: the inherited walkbook pipes some commands through head and therefore does not preserve their exit statuses. The corrected backing-path control was run separately. The inherited kill probe did not identify a daemon PID, so it does not prove durable logging after killing that daemon. Foreground workspace mount blocks by design unless daemonized.
