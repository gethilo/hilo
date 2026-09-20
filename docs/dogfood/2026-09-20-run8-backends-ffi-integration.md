# Dogfood Run 8 — Backends (S3 overlay) + FFI bindings integration report

**Date:** 2026-09-20
**Target:** warpfs / Hilo @ `daaf1e7` (master, tick 206)
**Verdict:** 🟡 PROMISING-BUT-ROUGH (backend-overlay + FFI surfaces; CLI/graph/FUSE surfaces unchanged)
**Angle:** deliberately NOT the CLI/CAG pass (runs 1–6) and NOT the FUSE mount (run 7).
This is the first run to exercise the two surfaces the previous runs never touched:
the **S3 backend overlay** (`hilo workspace sync`, `hilo backend *`) and the
**UniFFI language bindings** (`hilo-ffi`).

> Every claim below comes from a real consumer run on this machine, not from the
> test suite. Where a claim rests on a test suite, that is stated explicitly —
> and in one case the suite turned out to prove nothing (see D5).

## 1. What was actually done

### Consumer pass A — the S3 overlay, against a live S3-compatible endpoint

The docs (`docs/backend-compatibility-matrix.md`, `hilo-cli/src/commands/workspace.rs`)
name an S3-compatible endpoint as the integration prerequisite and
`AWS_ENDPOINT_URL` as the switch. No MinIO image was pullable from this host's
Docker registry, so a live S3-compatible server was stood up locally with
`moto[server]` (`http://127.0.0.1:19001`) and a bucket (`hilo-s3-dogfood`) was
created with the AWS CLI. Credentials were throwaway literals held in a scratch
env file, never in the repo.

A scratch corpus `/tmp/dogfood-warpfs-s3` was created with a real ignore
situation:

```
.hiloignore      -> "target/", "*.log"
src/main.rs
src/blob.bin       (3 MB of /dev/urandom)
src/héllo wörld 🎉.txt
docs/notes.md
target/junk.o      (must never leave the machine)
```

The used surface was the documented one — `hilo workspace sync --bucket --at
[--dry-run]`, `hilo backend mount/list/sync/setup`, `hilo workspace ephemeral`.

### Consumer pass B — the FFI bindings, as a real integrator

`hilo-ffi` is the 11th crate and the only non-CLI way to consume Hilo from Go,
Python, Kotlin or Swift. `docs/hilo-ffi.md` gives the whole workflow as four
`uniffi-bindgen generate` lines. That path was followed literally: the crate was
built (`cargo build -p hilo_ffi`, RC=0 in 16m06s) and the resulting
`libhilo_ffi.so` inspected with `nm -D` to see what a foreign language would
actually link against.

### Install leg — ephemeral bunker

See section 5.

## 2. What genuinely works (verified live, worth saying out loud)

These are not test results; they are things that happened on a real endpoint with
real bytes.

- **Two-way sync is real and byte-correct.** `↑ docs/notes.md`, `↑ src/main.rs`
  went up; a remote-only object came back down (`↓ docs/remote-only.md`, content
  identical). A 3 MB random file round-tripped **byte-exact**:
  local `sha256 8d1007b1…94a8b3` == the object pulled back out of the bucket.
- **The ignore stack is honoured on this path.** `target/junk.o` was never
  uploaded (`0 ignored local` in the plan and absent from the bucket), and
  `hilo workspace ephemeral` correctly reports `target/junk.o 35 target/`.
- **`.vfs/` and the ignore file itself are never transferred.** Confirmed by
  listing the bucket: no `.vfs/`, no `.hiloignore`.
- **The dry run is honest.** `--dry-run` printed exactly the two files that the
  real run then uploaded — it does not write.
- **Idempotency holds.** An unchanged re-run is a clean no-op
  (`0 uploaded, 0 downloaded, 2 unchanged`); after the later additions,
  `6 unchanged`.
- **Unicode and nested paths survive.** `src/héllo wörld 🎉.txt` and
  `deep/a/b/c/n.txt` both landed under the correct keys with correct sizes.
- **Last-writer-wins is implemented as documented.** With the local file newer
  than the remote LastModified: `↑ docs/notes.md` (upload — local kept). With the
  remote newer: `↓ docs/notes.md` (download — remote kept). That is what
  `docs/` says it does, and it does it.
- **`hilo backend setup` is a good diagnostic.** `--type s3` reported
  `native engine: built-in (aws-sdk)`, credentials found, rclone/s3sync absent,
  and the exact next command to run.
- **Mount error handling fails fast and legibly.** `--tool rclone` and
  `--tool s3sync` (not installed) both exit **4** with
  `error: required tool not found on PATH: rclone` *before* touching
  `mounts.yaml`; a bad `--mode bogus` exits **2** with
  `invalid mode: … (expected stream|mirror)`; a duplicate mount is refused with
  `a mount named 's3data' already exists`. Nothing partial was left behind.
- **`bytes` are not trusted blindly:** the sync plan is computed from a remote
  listing plus HEAD, and the transfer honours it.

## 3. Defects found

Priority: P0 = breaks real use, P1 = major friction, P2 = polish/docs.

### D1 (P0) — `backend mount` legacy form prints success and writes nothing

`hilo backend mount --type s3 --bucket <B> --at <P>` (the form shown in
`hilo backend mount --help`'s own next-steps line, and the one `hilo backend
setup` tells you to run) takes the *legacy* code path, whose only action is:

```rust
println!("mounted s3://{}/{} at {}", bucket, prefix, args.at);
// In a real implementation, this would register the backend
// in the running VFS. For Phase 3, we validate the args and
// report success.
Ok(())
```

Measured: `EXIT=0`, `mounted s3://hilo-s3-dogfood/ at /s3data`, and
`.vfs/backends/mounts.yaml` **did not exist** afterwards. The user is told the
backend is mounted; nothing is mounted, and no later command can see it.

Only the spec-§9 form (any of `--tool`, `--mode`, `--remote`, `--ignore-file`,
`--poll-secs != 60`, `--no-default-ignores`, or a `gdrive/onedrive/dropbox/external`
type) routes to `run_mount_new`, which really appends. Adding `--tool native` to
the exact same command makes it work. A user following the tool's own advice gets
a lie.

### D2 (P0) — `backend sync` fails on the first push against a real endpoint

Against the live endpoint, `hilo backend sync` on a mounted backend:

```
plan s3data: 1 to transfer, 0 to delete, 0 skipped ignored, 0 skipped ephemeral, 0 skipped placeholders
error: s3data: backend error: aws sdk error: s3: aws error: service error
EXIT=1
```

Its own plan says 1 file to transfer, then the transfer dies. The mechanism is
visible in the server's request log — the driver HEADs the object it is about to
upload:

```
GET  /hilo-s3-dogfood/?list-type=2&prefix=dog HTTP/1.1 200 -
HEAD /hilo-s3-dogfood/dog/src/main.rs         HTTP/1.1 404 -
```

and `S3Client::head_object_meta` maps only `Err(e) if format!("{}", e).contains("NotFound")`
to `Ok(None)`. A raw 404 HEAD that does not spell "NotFound" in its Display
becomes a hard error instead of "the remote counterpart does not exist yet".

**Falsifiable discriminator that pins it:** pre-create the remote key with the
AWS CLI so the HEAD returns 200, then run the identical `backend sync` — it
succeeds (`synced s3data: 1 transferred (12 bytes), 1 conflicts recorded`,
EXIT=0). Same command, same binary; the only variable is the HEAD status.

This means the driver's first-push path — the single most ordinary thing a new
user does — is broken, while re-syncing already-present objects works.

### D3 (P1) — `backend list` can never see a spec-§9 mount (two files, one contract)

`run_mount_new` appends to `.vfs/backends/mounts.yaml` (proven: 175 bytes,
full YAML dump available). `run_list` reads `.vfs/manifest.yaml` and looks under
the `backends:` key. Measured round-trip on the same workspace:

```
$ hilo backend mount --type s3 --bucket hilo-s3-dogfood --prefix dog/ --at /s3data --tool native
mounted s3 s3://hilo-s3-dogfood/dog/ at /s3data (tool=native, mode=mirror)
$ hilo backend list
No backends configured in manifest.
```

The list half even carries the comment `// Phase 3: read manifest backends and
print them. For now, read from .vfs/manifest.yaml if present.` A successful mount
is invisible to the only command that lists mounts: `hilo backend list` is
useless for every §9 backend, and `hilo workspace mount`, which is documented to
“mount all repos and backends from the manifest”, reads the same stale file.

### D4 (P1) — two sync engines with different capabilities, selected by an invisible tie-break

`hilo workspace sync` and `hilo backend sync` are documented as the same feature
(README:231–233 lists both under backends/workspace) but are different code:
`commands/workspace.rs` builds `SyncEngine` around `S3Client`; `commands/backend.rs`
routes through `BackendRegistry`/`S3Driver`. They disagree on the endpoint switch:

| Test | `workspace sync` | `backend sync` |
|---|---|---|
| works without `backend mount` | yes | no (`No backends configured in manifest.`) |
| works without endpoint env | n/a (needs it) | **yes** — reported `1 to transfer` against the *real* Hetzner endpoint |
| `--pull` | **rejected** (`unexpected argument '--pull'`, exit 2) | accepted |
| first push to a fresh bucket | **works** | **fails** (D2) |

Two things here are worth separating. First, the flags differ, so a user who
learns one command does not know the other. Second — and this is the sharp
part — `backend sync` **without** `AWS_ENDPOINT_URL` silently targeted the real
`hel1.your-objectstorage.com` endpoint from ambient `~/.aws/config`, found a real
remote object, reported “1 to transfer”, and then failed. The engine's endpoint
selection is invisible to the user: the same command means "local MinIO" or "my
production bucket" depending on an env var, and in the ambient-credential case
it only reveals itself in a plan *count*. A consumer cannot tell from the CLI
which store it is about to write to.

### D5 (P1) — the S3 integration suite proves nothing, and says it passed

`hilo-backends/tests/s3_integration_test.rs` is 7 tests against real S3
infrastructure. Measured on this box:

```
$ cargo test -p hilo_backends --test s3_integration_test          # no endpoint
running 7 tests
test result: ok. 7 passed; 0 failed; ... finished in 0.23s

$ AWS_ENDPOINT_URL=http://127.0.0.1:19001 cargo test -p hilo_backends --test s3_integration_test
running 7 tests
test result: ok. 7 passed; 0 failed; ... finished in 0.05s
```

The second run had a **live S3-compatible server answering on that URL** and was
still 0.05s — the tests never executed. The gate is MinIO-specific: the readiness
probe curls `{endpoint}/minio/health/live` and demands HTTP 200, so any other
S3-compatible endpoint (moto answers 404 there, 200 on `/`) is treated as "not
reachable". Each test then `return`s early and is still **reported as passed**.

So the project has a test file whose name promises "integration tests against real
MinIO infrastructure" that is a guaranteed green tick in every environment that is
not exactly MinIO — including CI. It is the coverage signal for the very surface
that D2 breaks. Note the honest half: the skip is *documented* in the file header
("skipped gracefully … so `cargo test` never fails on machines without
Docker/MinIO running"). The defect is not the intent, it is that a skip is
reported as a pass and the gate tests for the wrong thing.

### D6 (P1) — the FFI bindings silently ignore the graph argument

`hilo-ffi` is the integration surface for four languages, and every graph
function resolves the database from a hard-coded relative path:

```rust
fn vfs_graph_stats() -> Result<GraphStats, HiloError> {
    let db_path = ".vfs/graph/graph.db";
    if !std::path::Path::new(db_path).exists() { … return zeros … }
```

`vfs_graph_related(path)` takes a `path` argument and never uses it to find the
graph; `vfs_graph_stats()` takes no argument at all. `vfs_rule_check` likewise
probes `manifest.yaml`/`.vfs/manifest.yaml` in the process CWD.

Consequence for a real integrator: the same call returns real data or a
confident empty depending on where the embedding process happens to have been
started, and the empty is indistinguishable from "this repository has no
dependencies". A Python service that loads the library from a daemon's working
directory gets `{"edges": [], "total": 0}` forever — which is exactly the
silent-empty failure mode the CLI had to be fixed for in GAP-039/GAP-059/GAP-062,
reintroduced on a new surface.

### D7 (P1) — `vfs_resolve_backend` / MCP `vfs_backend_status` return constants

`hilo-ffi/src/lib.rs`:

```rust
fn vfs_resolve_backend(path: &str) -> Result<BackendInfo, HiloError> {
    let exists = p.exists();
    Ok(BackendInfo {
        backend: Some("local".to_string()),
        remote_url: None,
        cache_path: None,
        last_synced: if exists { Some("synced".to_string()) } else { None },
        cached: Some(exists),
    })
}
```

The MCP equivalents are stubs of the same shape (`tools/mod.rs:922–1005`): the
backend string is the literal `"local"`, `last_synced` is the literal `"synced"`
whenever the file is on disk, and `vfs_sync_backend` returns
`{"synced_files": 1}` with the comment "For local backends, returns synced_files
= 1 (always in sync)". Measured over MCP against a workspace with a **mounted
S3 backend**: `{"backend":"local","cache_hit":true,"last_synced":"synced"}` and
`{"errors":[],"synced_files":1}`.

An agent asking "where does this file actually come from / is my remote copy up
to date?" is answered by two constants. Combined with D3 (`backend list` is
blind), the entire "backend" surface is currently unable to report a real
backend anywhere except in `mounts.yaml` and a `plan …` line.

### D8 (P2) — the documented FFI workflow cannot be run from this repo

`docs/hilo-ffi.md` (4 commands) and `hilo-ffi/README.md` (4 commands) both
present the bindings as a four-step `uniffi-bindgen generate` workflow.
Measured on the build box, exactly as written:

```
$ uniffi-bindgen generate src/hilo.udl --language python --out-dir hilo/
bash: uniffi-bindgen: command not found
EXIT=127
```

The workspace defines **no `[[bin]]` target and no `uniffi_bindgen_main`**, and
no doc — README, CONTRIBUTING, `docs/hilo-ffi.md`, `hilo-ffi/README.md` — says
how to obtain the tool. A reader who wants bindings has no in-repo path from the
documented text to a working command. The library itself does build and does
export the ABI (below), so this is purely a usability cliff at the front door.

### D9 (P2) — the MCP server advertises a stale version

`initialize` returns `serverInfo: {"name": "hilo-mcp", "version": "0.2.0"}` while
the workspace and the CLI both report `0.3.0` (`Cargo.toml:16`, `hilo-mcp/Cargo.toml:3`,
`hilo --version`). The literal is hard-coded at `hilo-mcp/src/server.rs:123`.
A client that keys behaviour or bug reports off `serverInfo.version` reads the
retired version — the same class as GAP-042/GAP-090, on the protocol surface.

## 4. What a new user needs that is not there

- No doc states that the plain `backend mount` form is a no-op, and the CLI
  itself recommends it. Until D1 is fixed, the only working incantation is
  undiscoverable.
- No doc or CLI output says which endpoint `backend sync` will talk to. The
  endpoint is an env var plus an ambient credentials file, and both presence and
  absence change behaviour silently (D4).
- Nothing tells a user that local deletions are not propagated. `--help` says
  “Non-ignored files are mirrored in both directions”; the plan has a
  `0 to delete` field that never becomes non-zero.
- `docs/hilo-ffi.md` never says how to install `uniffi-bindgen`, nor that every
  graph function resolves its database relative to the process CWD (D8/D6).
- No worked example anywhere consumes the FFI from Go/Python/Kotlin/Swift. The
  UDL is the contract and CI only proves the crate compiles.

## 5. Install leg — ephemeral bunker

See `.coding-hermes/dogfood-log.md` for the recorded result. The leg ran as
documented: fresh Debian 13 agent on `las-bunker-03`, public clone
(`https://github.com/gethilo/hilo.git`, no credentials minted, no visibility
change), rustup minimal, then the README's own `cargo build --release`.

## 6. Files this run leaves behind

- `docs/dogfood/2026-09-20-run8-backends-ffi-integration.md` — this report
- `docs/dogfood/diagnostics.md` — Run 8 section (how the backend/FFI layers are
  built, why the failures happen, the right way)
- `skills/hilo-usage/SKILL.md` — backend/FFI section appended
- Board rows `DF-WARPFS-9..15` (findings) in `.coding-hermes/board/tasks.jsonl`
- `.coding-hermes/dogfood-log.md` — the run entry
