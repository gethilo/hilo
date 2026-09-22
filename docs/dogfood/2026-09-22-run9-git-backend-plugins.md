# Dogfood Run 9 — Git Backend End-to-End + Plugin Surface (2026-09-22)

**Tick:** warpfs-dogfood-2026-09-22-19-02-14 · **HEAD tested:** 917a991 (v0.3.0, local release build) · **Verdict: 🔴 DOES-NOT-DELIVER for the backend surface** (CLI/graph/MCP surfaces remain shippable — see runs 1–8)

## The promise tested this run

`hilo backend --help` advertises: *"Manage virtual backends (S3, git, local)"*
and `backend mount --help` says `--url <URL>` is *"required for --type git"*.
A real user with code in a git remote should be able to mount that remote as
a backend overlay on a workspace and work against it. That workflow had never
been exercised in runs 1–8 (run 8 did S3 only). This run used it the way a
user would: seed repo → bare remote → `hilo init` → `backend mount --type git`
→ read the mounted code → sync.

## What a user actually gets (verbatim session)

```console
$ hilo init                      # works: 6ms, hooks installed
$ hilo backend mount --type git --url file:///tmp/.../remote.git --at code
mounted git file:///tmp/.../remote.git at code (worktree code)    # RC=0
$ ls code/
ls: cannot access 'code/': No such file or directory               # nothing exists
$ cat .vfs/backends/mounts.yaml
cat: ...: No such file or directory                                # no state was written
$ hilo backend list
No backends configured in manifest.                                # blind (DF-WARPFS-11)
$ # user retries with the sync tool made explicit:
$ hilo backend mount --type git --url ... --at code --tool native
error: invalid config: unknown backend type: git (expected s3|gdrive|onedrive|dropbox|external)
$ hilo backend setup                    # the documented diagnostic
== s3 == ... == external ==             # git is not even listed
```

The plain mount (the documented form, no `--tool`) **exits 0, prints
"mounted … at code", and persists nothing** — no directory, no mounts.yaml,
no error. The explicit-tool form reveals git is rejected outright by the
validator. Same for legacy `--type local`. `backend setup` covers 5 types;
git/local are absent from setup, the compatibility matrix
(`docs/backend-compatibility-matrix.md`) and all of `docs/hilo-backends.md`.

### S3 control probe (same session, fresh workspace)

`backend mount --type s3 --bucket … --prefix … --at s3code` (no `--tool`):
same silent success, zero state written. With `--tool native`: persists a
correct mounts.yaml entry — but `backend list` still reports none (run 8's
DF-WARPFS-11, unchanged), and the first `backend sync --pull/--push` fails
with `aws sdk error: s3: aws error: service error` (run 8's DF-WARPFS-10
opaque-error form, unchanged). The run-8 workaround (`--tool native`) is
still the only working persist path.

## Also probed this run (both broken)

- `hilo plugin load /tmp/fake.wasm` where the file contains `not wasm`
  (plain text): **RC=0**, prints `loaded plugin: fake / hooks: 1 /
  edge_types: ["tested_by"]` — fabricated metadata parsed out of garbage.
  `.vfs/plugins/` is never created; `plugin list` still finds nothing.
  There is no example plugin in the repo; `docs/hilo-plugins.md` documents
  only the Rust API, never the CLI.
- `hilo serve --mcp` outside any project: **starts and serves all 17 tools
  from an empty graph**, while `docs/cli-reference.md` §`hilo serve` claims
  it "refuses to start and names `hilo init`". `serve --help` and
  `SKILL.md` still say "15 tools" (actual: 17).

## What works (re-verified this run, do not regress these)

- `hilo init` (6ms) + post-commit hook wiring (hook fires on real commits,
  correct `|| true` behavior, graceful "not installed" fallback).
- MCP stdio server: clean JSON-RPC, `initialize` + `tools/list` = 17 tools,
  NDJSON framing, pure stdout.
- Graph core on fresh corpora: init → warm (1,089 edges over 100 files in
  3.5s) → stats → impact; loud errors on unknown paths.

## Performance (Step 2b — measured, no finding warranted)

| Operation (release build) | Result |
|---|---|
| `hilo graph stats` (4-file corpus, warm, n=20) | 30.0 ms ± 6.6 |
| `hilo graph impact src/main.rs` (warm, n=20) | 17.0 ms ± 1.9 |
| `hilo graph warm`, hilo's own source (107 files → 1,089 edges) | 3.48 s, peak RSS 211 MB |
| `hilo graph stats` on same | 47 MB peak |
| binary startup `hilo --version` (1.4 GB release binary, n=10) | 5.2 ms ± 0.5 |

Nothing here is slow enough for a user to notice; the backend headline
operation is broken, not slow, so there is no PERF row this run. (The
1.4 GB binary size with 5 ms startup is recorded in diagnostics as a
curiosity, not a defect.)

## Verdict rationale

The CLI/graph/MCP product that runs 1–8 validated is real and fast, and the
v0.3.0 release itself is correctly cut. But this run's surface — the one the
CLI help text itself advertises ("S3, git, local") — does not exist behind
the CLI: the documented git mount is a silent no-op, the rejected types are
still advertised, and the closed defect rows from run 8 reproduce at HEAD.
A user who arrives for the multi-backend story hits a wall in the first
minute and gets a success message while hitting it. That is a
does-not-deliver for the surface, not for the project.

## Rows filed (board: `.coding-hermes/board/tasks.jsonl`)

- DF-WARPFS-19 (P0) — git/local backend doors dead end-to-end; silent
  success on the documented form, validator rejection with `--tool`.
- DF-WARPFS-20 (P1) — silent-success mount path (s3 default/auto), no state
  written, first sync fails opaque.
- DF-WARPFS-21 (P1) — closed-not-done audit: DF-WARPFS-9/10/11/17 marked
  complete, reproduce live at 917a991/v0.3.0; need re-open + read-back gate.
- DF-WARPFS-22 (P1) — plugin load accepts/celebrates garbage, registers
  nothing; no example plugin exists.
- DF-WARPFS-23 (P2) — docs drift family: serve-refusal claim false, 15-vs-17
  tools, hilo-backends.md git-blind, compat matrix omits git/local, hook
  noise on docs-only commits.
- DF-WARPFS-24 (P2) — `aws sdk error: s3: aws error: service error` carries
  no error code/endpoint; string-matched Display mapping (run 8 mechanism).

## Left behind

- this report: `docs/dogfood/2026-09-22-run9-git-backend-plugins.md`
- `docs/dogfood/diagnostics.md` Run 9 section
- `skills/hilo-usage/SKILL.md` git-backend/plugins field notes
- board rows DF-WARPFS-19..24 + dogfood-log.md entry

## Addendum (2026-09-22 late, item 46/47): re-runnable closure probes at 2b26d3f

Every "fixed" claim below is a command a second person can re-run. Both
binaries needed: the old artifact (`/home/kara/.cargo/bin/hilo.bak-20260922`,
built 2026-09-11) and a HEAD build. Run each inside a scratch dir that had
`hilo init`.

1. DF-WARPFS-19 (dead git mount) — old vs new:

       $ hilo-old backend mount --type git --url https://github.com/octocat/Hello-World.git --at ./mnt
       mounted git https://github.com/octocat/Hello-World.git at ./mnt (worktree ./mnt)   # exit 0, .vfs/backends/mounts.yaml NEVER written
       $ hilo-new backend mount --type git --url https://github.com/octocat/Hello-World.git --at ./mnt
       error: invalid config: unknown backend type: git (expected s3|gdrive|onedrive|dropbox|external)   # exit 2, nothing written

2. DF-WARPFS-22 (fake.wasm) — `printf 'not wasm at all' > fake.wasm; hilo plugin load fake.wasm`:
   old exits 0 printing fabricated `hooks: 1` / `edge_types: ["tested_by"]`
   and registers nothing (`plugin list` empty); new exits 1 with
   `invalid wasm module ... missing \0asm magic at byte 0 (file is 15 bytes)`.
   A valid-header file (`printf '\0asm\x01\0\0\0' > x.wasm`) loads with
   honest `hooks: 0`, persists to `.vfs/plugins/x.wasm`, and lists as
   `x v? — 0 hooks, 0 edge types`.

3. DF-WARPFS-20 mount half (silent s3 default form) — NOT live at HEAD:

       $ hilo-new backend mount --type s3 --bucket dogfood-probe-bucket --prefix probe --at s3code
       mounted s3 s3://dogfood-probe-bucket/probe at s3code (tool=native, mode=mirror)   # exit 0
       $ cat .vfs/backends/mounts.yaml   # correct entry present
       $ hilo-new backend list           # shows the configured mount

   (row stays pending only for the sync-error-quality clause)

4. DF-WARPFS-23 serve-refusal — `cd /tmp && hilo-new serve --mcp`:
   `error: no Hilo project found in /tmp/...: expected manifest.yaml or
   .vfs/manifest.yaml; run `hilo init` first` (exit 1). The old claim was
   false at 0.3.0; fixed behavior needs a HEAD build.

Provenance note: fixes 1–2 are observable only on a rebuilt artifact — the
11-day-old binary reports the same `hilo 0.3.0` string as the 0.3.0 release.
From 7026bd9 the artifact identifies itself:

    $ hilo --version
    hilo 0.3.1-dev
    build: v0.3.0-14-g2b26d3f
    built: 2026-09-22T23:19:30Z

