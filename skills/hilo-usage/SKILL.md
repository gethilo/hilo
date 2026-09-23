---
name: hilo-usage
description: "How to USE hilo (warpfs) — the agent-first virtual filesystem — for real work: init/warm/query workflows, verified per-language workarounds, known gaps as of 2026-09-13 (run 5)."
version: "1.0.0"
category: software-development
---

# Using Hilo (warpfs) — field guide for agents

Hilo pre-computes a dependency graph for a codebase and answers structural
questions (blast radius, importers, coverage, classification) without file
reads — via CLI, MCP (17 tools), or FUSE xattrs. Rust workspace at repo
root; binary is `hilo` (not `hilo-cli`).

## Setup (any repo)

```bash
git clone https://github.com/gethilo/hilo.git && cd hilo
cargo build --release -p hilo-cli     # ~7-19 min FIRST build (duckdb-sys);
cp target/release/hilo ~/.cargo/bin/  #   incremental after: seconds
cd /your/target/repo
hilo init && hilo graph warm && hilo classify
```

Requirements reality (verified on 3 fresh boxes): rustup + gcc + make.
clang/CMake listed in README are NOT needed unless you deviate from the
default build. libfuse3-dev only for mounts.

## The one rule that saves you: match the query form to the language

The graph stores edge targets in language-specific dialects. File-form
`impact`/`related` only resolve for Rust (crate roots). Everything else
needs the dialect form:

| Language | edges target | working blast-radius query |
|---|---|---|
| Rust | `pkg:<crate>` (+ family) | file-form OK on crate roots; `pkg:<crate>` always works |
| Go | `pkg:<full-import-path>/<dir>` | resolve file's dir → `pkg:<module>/<dir>` |
| Python | `pkg:<module.dots>` | drop `.py`, slashes→dots → `pkg:<module>` |
| TS/JS | `local:<relative-specifier>` | query the EXACT specifier: `hilo graph related 'local:./pluginContainer' --direction reverse` (each `./x` vs `../server/x` variant is a separate query — GAP-069) |

If a file-form answer says "No dependents found" on a non-Rust repo,
**do not trust it** — resolve the dialect form and re-query. Empty result
is structural, not informational (verified: file had 24 incoming edges).

## Verified-fast paths (trust these)

- `graph stats` / `related` / `search` / `untested`: ~20-30ms at 1-5k files
- `graph impact 'pkg:x'`: ~20-100ms
- Incremental: touch files → `graph warm --changed` (~1s, reparses only changes)
- Byte-deterministic stats output (safe to diff across runs/machines)
- `hilo mount <dir> --daemon` → instant; xattrs visible to `getfattr` through
  the mount; unmount with `fusermount -u <dir>`

## Traps (each burned a real run)

1. **`graph clean` then `graph warm` leaves the graph EMPTY (GAP-070).**
   clean deletes edges but not the parse cache; warm says "all cached,
   graph unchanged" and stats says "Graph cache is empty." Fix:
   `rm .vfs/graph/.parse_cache.json && hilo graph warm`.
2. **`hilo meta` syntax is attr-first:** `hilo meta <path> --set <attr>
   --value <val>`. The `--set attr=value` typo now errors cleanly (rc 1,
   GAP-067 fixed) — previously it silently wrote a garbage xattr.
3. **`hilo serve --mcp` is NDJSON** (one JSON-RPC message per line).
   LSP Content-Length framing → `-32700 Parse error`. stdout is pure
   JSON (17 tools); tracing goes to stderr.
4. **`hilo init` without `.git/` is fine** (skips hooks with a warning),
   but `hilo serve --mcp` requires `hilo init` in that project first.
5. **HOME graphs are refused** without `--allow-home` (vendor/HOME guard,
   on by default — this is correct behavior, don't override casually).

## Coverage queries: read the edges, not the summary

`graph untested` / `graph module` under-report on non-Rust repos because
they don't resolve all tested_by target dialects (GAP-066 fixed for
pkg-form; GAP-071 open for TS/JS `local:`-form). Ground truth that always
works:

```bash
grep -c '"rel":"tested_by"' .vfs/graph/edges.jsonl
grep '"rel":"tested_by"' .vfs/graph/edges.jsonl | grep -c '"to":"local:\.\./<module>"'
```

## Understand/search expectations

`graph search` is lexical (TF-IDF/BM25), fast, ranks the right module —
but prints raw `local:` node names on TS/JS (GAP-072). `graph understand`
MAP needs symbol-extraction parity for TS/JS (GAP-037 family); treat its
empty bullets as "not extracted", not "no symbols".

## MCP quickstart

```bash
printf '%s\n%s\n' \
 '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"x","version":"1"}}}' \
 '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' | hilo serve --mcp
```

17 tools; `vfs_graph_impact` with a `pkg:`/`local:` dialect path works,
file paths on Go/Python/TS currently return `{"total":0}` without an
error flag — same trap as the CLI.

## FUSE mount: read this BEFORE you mount anything (run 7, 2026-09-20)

The mount is the surface the README leads with, and the one most likely to
waste your time. Three hard rules:

1. **`hilo mount` fails with `allow_other ... user_allow_other` on a stock
   box** (DF-WARPFS-6). Hilo always sets `auto_unmount`, and `fuser` requires
   `user_allow_other` in `/etc/fuse.conf` for that — commented out by default
   on Debian/Ubuntu — regardless of `--allow-other` being off. There is no
   CLI flag to disable `auto_unmount`. If you must mount, check
   `grep -v '^#' /etc/fuse.conf | grep user_allow_other` first (needs root to
   set). Do NOT "fix" this by editing the repo — it is a product finding.
2. **Do not walk the mount with `find` / recursive glob / `git status`**
   (DF-WARPFS-5). `readdir` on an **empty** directory never returns a reply,
   so `find <mount> -type f` returns 0 rows and hangs forever on any repo
   with one empty dir. Non-empty dirs answer in ~110 ms. Always cap with
   `timeout` and treat "0 rows + no error" as a hang, not an empty result.
3. **The mount serves ignored trees** — `.git/`, `.vfs/`, and `target/`
   (130 GB here) despite `hilo ignore check target/` saying `ignored: true`
   (DF-WARPFS-7). The mount does not consult the ignore stack at all.

What DOES work through the mount (verified): exact sizes and sha256 match
disk; `cat` works; `user.vfs.*` xattrs set by `hilo classify` read back
through the mount and `hilo meta <mount-path>` resolves them. So xattr
queries through a mount are safe **if** you address the file directly rather
than walking the tree. Unmount with `fusermount3 -u <dir>` — clean, no
leftover process.

Timestamps through the mount are fabricated: every file reports the mtime of
the stat call, so "what changed recently?" is unanswerable there
(DF-WARPFS-8). Use the real working tree for anything mtime-based.

## Status snapshot (2026-09-20, run 7)

- ✅ Rust/Go/Python pkg-form workflows + CLI/graph surface: SHIPPABLE
- ✅ Installability: proven on 3 independent fresh boxes (build 1142-1217 s)
- 🟡 Mount: 🟡 PROMISING-BUT-ROUGH — unusable on a stock box (DF-WARPFS-6)
  and traversal-hostile (empty-dir hang DF-WARPFS-5, ignore gap DF-WARPFS-7,
  fabricated mtimes DF-WARPFS-8). xattr-through-mount DOES work.
- 🟡 TS/JS: value real (warm/classify/FUSE/stats all clean) but blast
  radius + coverage need the dialect workarounds above (GAP-069/071/072)
- Full run histories: `docs/dogfood/` (7 integration reports +
  diagnostics.md); run 7 = `2026-09-20-run7-fuse-integration.md`

## Backends + FFI: read this BEFORE you mount or embed anything (run 8, 2026-09-20)

Two surfaces nobody had used before run 8. Both currently require workarounds.

**S3 backend overlay — use `workspace sync`, not `backend sync`.** They are
different engines with different flags and different bugs. For pushing a local
directory to a bucket, this is the one that works on a fresh bucket:

```bash
export AWS_ENDPOINT_URL=http://minio:9000   # or whatever your endpoint is
export AWS_ACCESS_KEY_ID=… AWS_SECRET_ACCESS_KEY=… AWS_DEFAULT_REGION=us-east-1
hilo workspace sync --bucket <B> --at <DIR> --dry-run   # plan first
hilo workspace sync --bucket <B> --at <DIR>             # two-way, last-writer-wins
```

It is verified byte-exact (3 MB binary sha256 matched), honours `.hiloignore`
(ignored files never leave the machine), never transfers `.vfs/` or the ignore
file, is idempotent on re-run, and handles nested + unicode paths. There is no
`--pull`/`--push` flag on it — a plain run is two-way; `--pull` exits 2.

**Do not use `hilo backend mount` without a §9 flag.** The plain form exits 0,
prints `mounted s3://…`, and registers nothing (D1). Any one of
`--tool native` / `--mode mirror` / `--poll-secs 30` selects the real path —
but even then `hilo backend list` will still say `No backends configured in
manifest.`, because the list half reads a different file (D3). Read
`.vfs/backends/mounts.yaml` directly if you need to know what is mounted.

**`hilo backend sync` fails on the first push against a real S3 endpoint** (D2):
it plans the file, then dies with `aws sdk error: service error` because it
HEADs the absent remote key and only converts an error to "not found" when the
message contains the literal `NotFound`. Pre-creating the remote object makes
the same command succeed — useful as a workaround, and decisive as proof of the
cause.

> **Endpoint trap (D4):** `S3Client` only uses an explicit endpoint when
> `AWS_ENDPOINT_URL` is set; otherwise it falls back to the ambient AWS chain
> (`~/.aws/config` + credentials). With the var unset, `backend sync` silently
> targeted a real production bucket from `~/.aws/config` and reported a real
> object to transfer. **Always set `AWS_ENDPOINT_URL` explicitly**, and confirm
> from the plan/bucket listing which store you are actually pointed at before
> letting a sync run.

**Ignore the S3 integration test suite's green tick.** `cargo test -p
hilo_backends --test s3_integration_test` reports `7 passed` in ~0.05–0.23 s
whether or not an S3 endpoint is reachable: it gates on MinIO's own
`/minio/health/live` and reports the early-return skip as `ok` (D5). It is not
evidence about any S3 endpoint.

**FFI (`hilo-ffi`): the library is real, the path to it is not.** The crate
builds and exports the full UniFFI ABI for Go/Python/Kotlin/Swift, but:

- `uniffi-bindgen` is not provided by the repo (no `[[bin]]`, no docs on
  installing it) — the documented `uniffi-bindgen generate …` exits 127 (D8).
  Generate bindings outside this repo, or pin the generator yourself.
- Every graph function opens the literal `.vfs/graph/graph.db` **relative to the
  embedding process's CWD**, and `vfs_graph_related`'s `path` argument is not
  used to find the graph (D6). In a host app the "empty result" you get from the
  wrong CWD is indistinguishable from "no dependencies". Run the embedding
  process with its CWD at the repo root, or expect zeros.
- `vfs_resolve_backend` and the MCP `vfs_backend_status`/`vfs_sync_backend`/
  `vfs_resolve_path` return **constants** (`backend:"local"`, `last_synced:"synced"`,
  `synced_files:1`) regardless of any mounted backend (D7). Do not use them to
  answer "where does this file come from" or "is my remote copy current".

**MCP `serverInfo.version` lies.** `initialize` reports `0.2.0` while the CLI and
workspace are `0.3.0` (D9) — key bug reports off `hilo --version`, not the MCP
handshake.

**Testing this surface cheaply:** a throwaway `moto_server` (S3-compatible) plus
the AWS CLI gives you an independent ground truth to diff every "sync complete"
line against — that comparison produced three of run 8's findings.

## Backend/plugin surface honesty (run 9 findings FIXED 2026-09-22; was "git/local don't exist + plugin load lies")

**Backend mount types are exactly s3|gdrive|onedrive|dropbox|external.**
`--type git` / `--type local` are REJECTED honestly since fix 2ba3969
(2026-09-22, DF-WARPFS-19): exit 2, `unknown backend type: git (expected
s3|gdrive|onedrive|dropbox|external)`, nothing on stdout, nothing persisted.
Run-9's silent-success clone-into-`~/.hilo/worktrees` path is gone. If you
need remote code: clone it yourself and run the normal graph workflow on the
clone.

**The one-command honesty check for ANY backend mount:** after the mount
reports success, `cat .vfs/backends/mounts.yaml`. File exists → real
registration, and `backend list` reads it (the run-8-era "list is blind"
defect is fixed — verified 2026-09-22). File missing → you hit a silent
path; nothing was mounted.

**`hilo plugin load` is honest since e113f59 + 2de2af2 (2026-09-22,
DF-WARPFS-22/26):** non-wasm files are rejected with a `\0asm` magic /
version error naming the byte offset; a valid-header module loads with
truthful `hooks: 0 / edge_types: []` (no fabricated metadata anywhere —
`plugin list` derives the same way, skips invalid files, shows version `?`);
the load PERSISTS to `.vfs/plugins/<name>.wasm` where `plugin list` finds
it. Still missing: a checked-in example `.wasm` and a CLI section in
`docs/hilo-plugins.md` (tracked as DF-WARPFS-27).

**`hilo serve --mcp` no longer refuses to run outside a project** (the
cli-reference.md claim is false at 0.3.0): it starts anywhere and serves all
17 tools over an empty graph, so a mis-rooted server answers every structural
question with zeros instead of an error. Pin the server's CWD to an
initialized project yourself, and treat empty graph answers as suspect until
`hilo graph stats` in the same directory shows edges.

## Run 10 (2026-09-23, triggers + permissions): what actually works

**`hilo mount --triggers` is a no-op on a stock `hilo init` project** (run
10, v0.3.0-20-g8832da0): `hilo init` writes `triggers: []`, and that empty
list suppresses the 9 default parse-and-diff watchers — the stderr banner
honestly says `loaded 0 triggers` while stdout prints "(triggers enabled)".
Check the BANNER, not stdout. Even after you delete the `triggers:` key
from `.vfs/manifest.yaml` (loads the defaults), parse-and-diff fires ONLY
for files at the project ROOT — writes to `src/**` silently do nothing
(FileEvent gets the bare inotify name, engine.rs:195; the directory
reconstruction exists but never reaches the event; failures are logged via
`info!` with no subscriber installed anywhere in hilo-cli).

**The 10-second liveness canary for the trigger pipeline** (works at any
version):

```bash
printf 'use crate::config::Config;\nfn main() {}\n' > probe_root.rs   # ROOT, not src/
sleep 2 && grep -c probe_root .vfs/graph/edges.jsonl                   # 0 = engine dead
```

Root file fires in ~0.5 s when the engine works; nested files never fire
(fixed-or-not, re-probe both before trusting the pipeline).

**Never run `hilo graph stats`/`warm`/`understand` while a mount is up:**
the trigger engine holds the DuckDB lock on `.vfs/graph/graph.db`, so CLI
graph commands die with `Conflicting lock is held … PID <mount pid>`.
Unmount (`fusermount3 -u <mnt>`), or treat every lock error as
"something of mine still holds graph.db". Two `--triggers` mounts on one
project fight each other the same way, and the loser runs with impact
computation silently disabled.

**Manifest `permissions.rules` currently have NO effect on any surface**
(run 10): the FUSE engine is built from hardcoded default protections only
(ops.rs:113; no manifest read in the mount path), the mount is read-only
anyway (EROFS from the kernel), and hilo-mcp contains zero permission code
despite docs/hilo-permissions.md claiming MCP enforcement. Do not promise
path-level access control to an agent based on this manifest block; the
only real protections today are `.vfs/**`/`.git/**` hiding via the ignore
stack and the read-only mount itself.
