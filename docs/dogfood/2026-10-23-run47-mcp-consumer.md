# Dogfood Run 47 — 2026-10-23 — a real MCP consumer on a real repo (RED throttle, hook-clobbered census)

**Tick:** `warpfs-dogfood-2026-10-23-15-01-05` · **Target:** warpfs (/home/kara/warpfs)
**HEAD:** `cb65cc8a` · **Binary:** `/var/tmp/hermes-cargo-target/debug/hilo`, stamp
`v0.3.0-569-gcb65cc8a`, **dev profile** (see "Build identity" — the release build was occupied)
**Corpus:** fresh `git clone --local` of a REAL project (1199 tracked files, 786 files discovered by
warm, Go + TypeScript), cloned to `/var/tmp/df1023-warpfs/canopy2`, commit `d0ecaf86`
**Consumer:** the official MCP Python SDK `mcp==1.28.0` (`ClientSession` over `stdio_client`) — not a
hand-rolled JSON-RPC loop, because the promise is "integrate it into your agent".

**Promise tested (README:3):** *"Give your AI coding agent a pre-built map of every codebase it touches —
dependencies, entrypoints, test coverage, blast radius — without burning context window on file reads."*
The run therefore drives the documented agent path (`hilo init` → `hilo graph warm` → `hilo serve --mcp`)
and then **asks the map the question an agent actually asks**: *"I am going to change the context
compiler — what depends on it and what covers it?"* — and cross-checks every answer against `grep`.

**Verdict: 🟡 PROMISING-BUT-ROUGH.** When the map is warm and the hooks are installed the answers are
*correct and fast* — the pkg-form blast radius was set-identical to grep (16/16 files) over MCP in
199 ms, and REV-WARPFS-10's `total` fix holds in real use (16, not 32). Two things stop a real user:
the documented rate limiter **breaks the official MCP client outright** (the session hangs, forever),
and the documented post-commit hook **overwrites the warm census** so the very next `graph stats`
claims the graph was built from one file.

---

## 1. What worked (and is worth trusting)

| Operation | Result | Evidence |
|---|---|---|
| Cold `hilo graph warm` on the clone | 786 files → 7999 edges, 4 languages, **137.92 s** real (dev profile) | `/var/tmp/df1023-warpfs/warm.time` |
| `graph impact 'pkg:<module>/internal/context'` | 16 dependents at depth 1 — **set-identical to grep** (`grep -rl 'hermes-canopy/internal/context"' --include=*.go` = 16 files) | CLI stdout + grep |
| `vfs_graph_impact` (MCP, same path, `max_depth:1`) | `dependents` 16, `review_set` 16, **`total` 16** (unique union) — REV-WARPFS-10 holds | `mcp1.jsonl` |
| `vfs_get_metadata` / `vfs_list_directory` / `vfs_resolve_path` | 5.9 ms / 2.7 ms / 0.9 ms, correct shapes | `mcp1.jsonl` |
| MCP error taxonomy | unknown tool → `-32603 Unknown tool: …`; missing `path` → `-32603 missing 'path' argument` | `mcp1.jsonl` |
| `hilo init` with a **host-wide** `core.hooksPath` | refuses, names the path, explains the risk, writes nothing (DF-WARPFS-162's fix **holds**) | `/var/tmp/foreign-hooks` unchanged (mtime 2026-10-22 23:55) |
| Post-commit hook refresh | new importer added → commit → `graph warm --changed` discovered it → impact went 16 → **17** | commit `1f2e307c`, 12.00 s commit wall |

Perf (Step 2b), hyperfine `--warmup 1 -r 5` on the 786-file corpus, dev profile, host loadavg ≈ 20):

```
graph stats                      167.4 ms ± 8.8
graph impact pkg (max-depth 1)   126.3 ms ± 8.6
graph search                     407.7 ms ± 37.5
```

MCP round-trips (SDK-measured, loadavg ≈ 7): `stats` 193 ms, `impact` 199 ms, `related` 135 ms,
`untested` 170 ms, `search` 440 ms, `understand` **5 284 ms** (34 KB text), `audit` **5 241 ms**
(1.24 MB payload).

**Nothing here is worth a PERF row except one thing a user feels:** `graph understand` costs 4.7–5.3 s
warm and returns a payload whose own `tokens_estimate` (8 559) is 2.1× the `budget` the caller asked
for (4 000) — the budget half of that is already DF-WARPFS-158 (reproduced below, event filed), so no
new perf row was minted for it. The query verbs are comfortably fast.

## 2. Findings filed as rows

**DF-WARPFS-166 (P1) — the documented rate limiter makes the official MCP client hang.**
`performance.rate_limit_rps: 1` in `.vfs/manifest.yaml` (the documented knob, docs/hilo-mcp.md
"Rate Limiting") and the server answers every id-bearing `tools/call` with a frame whose `id` is
**`null`**:

```json
{"error":{"code":-32000,"data":{"retry_after_seconds":1},
          "message":"Rate limit exceeded. Retry after 1 seconds."},"id":null,"jsonrpc":"2.0"}
```

JSON-RPC 2.0 requires a response to echo the request id; `id: null` is reserved for requests whose id
could not be detected. `hilo-mcp/src/server.rs:158` emits it unconditionally *before* the parsed id is
available. The official client refuses the frame —

```
Failed to parse JSONRPC message from server
pydantic_core ValidationError: 9 validation errors for JSONRPCMessage
  id.int  Input should be a valid integer [input_value=None]
  id.str  Input should be a valid string  [input_value=None]
```

— its stdout reader task dies with the traceback, and the session **hangs**: the `mcp==1.28.0` probe
exits `rc=124` after 120 s having printed nothing, and no retry is possible from the client side
because the error carries no id to attribute. At the documented minimum RPS even the FIRST tool call is
rejected (the `initialize` handshake drains the single-token bucket), so `rate_limit_rps: 1` yields a
server that handshakes and then never answers. Raw frames:
`/var/tmp/df1023-warpfs/rl_raw_frames.txt`; client failure: `mcp_rl2.stderr.txt` (`rc=124`).

**DF-WARPFS-167 (P2) — the documented post-commit hook clobbers the warm census.**
`hilo init` installs `post-commit` → `hilo graph warm --changed`. That incremental warm rewrites
`.vfs/graph/coverage.json` with the *incremental* run's counts
(`hilo-cli/src/commands/graph.rs:632 write_coverage_ledger`), so after one commit on a 786-file repo:

```
Total edges: 7895 distinct / 7895 raw (edges.jsonl)
Total files: 890
File census: 890 files with edges … — warm last counted 1 discovered files = 1 contribute
             + 0 zero-edge … some sources may be stale entries no longer on disk
```

One sentence, two contradictory sizes, in the documented steady state. The trailing hypothesis
("stale entries no longer on disk") is fabricated: the *last* warm counted one file because it was an
incremental warm, not because the ledger is stale. `coverage.json` after the hook:
`{"contributes":1,"discovered_files":1,…}`.

**DF-WARPFS-168 (P2) — a fresh clone has no refresh path and no staleness signal.**
`hilo init` and the hooks it installs are the only thing that keeps the map current, and `.git/hooks/`
is not tracked — so every clone of an initialized project starts with **no hooks**. The documented
clone path (docs/getting-started.md "Clone & rebuild") says `hilo init` is "safe on a clone, but usually
unnecessary". Arm A (docs' path, no init): a new file importing `internal/context` exists on disk and
`graph impact` still answers **16** rows with the new importer absent and **no warning** that the map
predates the tree. Arm B (init + hook): 17 rows. An agent cannot tell the two states apart.

**DF-WARPFS-169 (P2) — the repo's own agent coaching is stale on three verified points.**
`skills/hilo-usage/SKILL.md` (loaded by the next agent) says the server exposes **17** tools (real:
`tools/list` = 18), says *"`hilo init` writes no `.gitignore` entries (DF-WARPFS-38)"* (HEAD's init added
13 lines of managed ignore to the clone's `.gitignore`), and says file-form `impact`/`related` is
empty on Go (HEAD answers for a real Go FILE: `graph impact internal/context/compiler.go` lists the
package's importers). Corrected in the same run; the row asks for a guard so prose cannot drift again.

**PERF-012 (P2) — the documented install compiles single-threaded by default.**
`.cargo/config.toml` ships `[build] jobs = 1` for every user; the file's own comment says agents with
≥8 GB "can override", which means the default cost is paid by everyone else. Measured this tick on a
16-core / 59 GB host with a WARM target dir: a HEAD `cargo build -p hilo-cli --release` was still
compiling after **45+ minutes** (a sibling lane's build, left running). On the crate level the pin is
mild (touch hilo-core+graph+mcp+cli → `CARGO_BUILD_JOBS=1` 10.22 s vs `=8` 9.09 s, because the crate
chain is serial), so the row's ACs ask for the **cold dependency** A/B (duckdb-sys/arrow/aws-sdk), not
a crate-level one — and for the default to stop punishing normal machines.

## 3. Recurrences and verifications appended to existing rows (no duplicate ids)

- **DF-WARPFS-161** (stray ancestor `.vfs` becomes the root) — reproduced on a *real* clone: with
  `/tmp/.vfs` present (a 200-byte binary manifest left on 2026-10-05), `hilo graph impact internal/context`
  run under `/tmp` printed `warning: failed to parse /tmp/.vfs/manifest.yaml …` and answered for the
  subject **`df1023-warpfs/canopy/internal/context`** — the `/tmp` prefix stripped, the wrong root adopted.
- **DF-WARPFS-64-R2** (confident answers before warm) — reproduced: pre-warm `graph impact internal/context`
  = `note: no dependents, dependencies, tests or build targets were found … the file itself is the only
  review row`, rc=0, with 16 importers on disk.
- **DF-WARPFS-163** (stats counts symbol nodes as FILES) — reproduced: `Total files: 890` against 786
  discovered / 743 edge-contributing files in the same report.
- **DF-WARPFS-158** (`understand` budget inert) — reproduced at HEAD, CLI and MCP: `--budget 1000` =
  `--budget 4000` = `--budget 6000` = **34 238 B** (only `--budget 20000` grows, to 53 047 B); MCP with
  `budget: 4000` returns `tokens_estimate: 8559`.
- **DF-WARPFS-162** (`init` honours a host-wide `core.hooksPath`) — **verified fixed in real use**:
  with `core.hooksPath=/var/tmp/foreign-hooks`, `hilo init` printed the refusal + the three remedies,
  exited 0, and wrote nothing (that directory still holds only its pre-commit, mtime unchanged). With
  `core.hooksPath=.git/hooks` the same command created both hooks. This row is closeable on this evidence.

## 4. Not a finding (recorded to save the next run)

`vfs_graph_search` for "context compiler token budget" returns five `provenance: "lexical"` rows with
scores 0.031…0.024 and does **not** surface `internal/context/compiler.go` at `limit: 5`. That is the
already-known lexical-search limitation (usage skill + REV-WARPFS-12), not a new defect; the honest fix
is a ranking row, not another search row.

## 5. Build identity (why the numbers are dev-profile)

`cargo build -p hilo-cli --release` at HEAD was occupied by two sibling lanes for the whole tick
(`/var/tmp/hermes-cargo-target` is shared), so the correctness legs ran on the HEAD **dev** binary
(`v0.3.0-569-gcb65cc8a`, built this tick from a clean tree) and the wall-clock numbers above are an
upper bound on the release user. Every timing is labelled accordingly; no row rests on a dev-profile
number as if it were a release one.

## Left behind

`docs/dogfood/2026-10-23-run47-mcp-consumer.md` (this report), `docs/dogfood/diagnostics.md` (the
throttle/census/refresh lessons appended), `skills/hilo-usage/SKILL.md` (three corrections),
`.coding-hermes/dogfood-log.md` (run entry), rows `DF-WARPFS-166…169` + `PERF-012`, events on
`DF-WARPFS-158/161/162/163/64-R2`. **No code changed; no visibility or permissions touched; the foreman
was not woken (read-only lane).**

ch:trace row=DF-WARPFS-166 spec=docs/hilo-mcp.md#rate-limiting wave=none:serial
test=none:manual-mcp-client-battery doc=docs/dogfood/2026-10-23-run47-mcp-consumer.md
evidence=/var/tmp/df1023-warpfs/rl_raw_frames.txt,mcp_rl2.stderr.txt,mcp1.jsonl
witness=tick-warpfs-dogfood-2026-10-23-15-01-05 verdict=PROMISING-BUT-ROUGH
commit=none:pending-foreman-trace memory=/project/warpfs/status
