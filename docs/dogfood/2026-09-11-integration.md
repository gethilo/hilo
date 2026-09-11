# Hilo Dogfood Integration — Run 3 (2026-09-11)

Third deep real-use run. Corpus: **containerd** (`/tmp/dogfood-warpfs3/containerd`,
depth-1 clone: 5489 Go files, 4122 of them under the committed `vendor/` tree —
the exact shape the brand-new PERF-005 guards target), plus the repo itself as a
Rust cross-check. Binary built from master **d734456** (one commit after the
PERF-005 fix fcebefe landed 2026-09-10 22:59). This run field-tested the fresh
PERF-005 safety work end-to-end.

**Verdict: ✅ SHIPPABLE (with one language-specific caveat).** First run in
three where no structural data-quality break was found.

## Installability (ephemeral bunker, las-bunker-03, agent 595d2c78)

- Fresh Debian 13 user, clone from `https://github.com/gethilo/hilo.git`
  (public — clone works with no credentials): OK.
- README's implied toolchain is NOT all present on a bare box: had to install
  Rust via rustup (standard, fine); `pkg-config`/`libfuse3-dev` were present
  (distro default), but `clang` + `cmake` (README requirement line) were NOT
  installed and the build **still succeeded** — the README over-warns here,
  worth a docs pass (benign direction).
- `cargo build --release` from scratch: **RC=0 in 18m52s, 499 crates** —
  inside the README's 15-20 min window.
- Smoke: documented quickstart (init → warm → stats) on Hilo's own source:
  879 edges / 93 files, SMOKE_RC=0, 3s.
- Destroyed agent afterwards (rc=0).

## PERF-005 verification (the freshest fix, exercised as a user would)

- `vendor/` exclusion: warm on containerd parsed **1367 files — exactly the
  non-vendor Go file count; all 4122 vendor files skipped; 0/14070 edges
  reference `vendor/`** (grepped edges.jsonl).
- `$HOME` refusal: `hilo init` in `$HOME` → `error: refusing to operate with
  HOME (/home/kara) as the project root; pass --allow-home to override...`
  Both CLI flag and guard verified live.
- Watch-out recorded (GAP-058): warm prints only "parsing 1367/1367 files" —
  nothing says 4122 files were excluded by policy. A user cannot tell
  deliberate exclusion from data loss without grepping edges.jsonl (this
  reviewer initially misread it as a bug).

## What works (verified this run)

- **Go blast radius via full-path `pkg:` form is EXACT.** Grep ground truth:
  126 files import `github.com/containerd/containerd/v2/core/mount` (non-
  vendor). `hilo graph impact 'pkg:github.com/containerd/containerd/v2/core/mount'
  --max-depth 1` → **126 dependents**. Same precision on other hubs
  (client 110, plugins 98, pkg/namespaces 92, core/content 88 — import counts
  from grep all match the edge set direction).
- **Rust file-form impact works**: `impact hilo-graph/src/lib.rs` → 17
  dependents on the repo itself.
- Speed claims hold at containerd scale: warm 80s one-time, `--changed`
  1s for 1 file, stats 26ms, impact 108ms — "near-instant queries" is fair.
- FUSE: `mount --daemon` instant, `getfattr -n user.vfs.role` THROUGH the
  mount returns xattrs set earlier, clean `fusermount3 -u`.
- MCP: 17 tools; initialize → tools/list → tools/call with a hand-rolled
  stdio client; **0 non-JSON stdout lines** (GAP-050 fix confirmed).
- Meta round-trip: `meta --set` → `hilo meta` → `getfattr` all agree.
- `graph untested` is now meaningful (real tested_by edges: 3409 on
  containerd; GAP-052 fix confirmed).
- Incremental warm `--changed` (PERF-002) works.

## What doesn't (GAP-057..061 filed)

1. **GAP-057 (P1): Go file→package resolution missing.** File-form
   `impact`/`related --direction reverse` on Go files returns empty (CLI) /
   `{dependents:[],total:0}` (MCP); `related` reverse on a REAL Go file with
   known importers → "No incoming edges". The pkg-form is exact, so the only
   missing link is mapping `dir/file.go` → `pkg:<module>/<dir>` — derivable
   from go.mod + file path. Rust resolution works, so this is a per-language
   gap, not structural.
2. **GAP-058 (P2): warm exclusion report** (see watch-out above).
3. **GAP-059 (P2): inconsistent not-found behavior.** `related` on a file
   that isn't in the graph → "No incoming edges", exit 0. `impact` on the
   same path → honest error. Empty must be distinguishable from unknown.
4. **GAP-060 (P2): perf-doc drift, benign direction** — README says impact
   ~0.5s; measured 26-108ms. Also no doc example shows the full-import-path
   `pkg:` form Go/Java users need.
5. **GAP-061 (P3): MCP tool count** — server exposes 17; docs tables say 15
   (covered by open DOC-2; this row is the run-3 verification).

## Time-to-first-success

- Warm machine with built binary: init+warm on containerd 80s, first exact
  blast-radius answer (pkg-form) ~2 min from clone.
- Cold machine (bunker): clone → usable hilo → own-repo graph = ~20 min
  (dominated by the honest, documented 19-min build).
