# Release readiness record — warpfs / gethilo/hilo — tick warpfs-releng-2026-11-11-14-28-54 (re-fire pass)

Author: release-engineering satellite. Written 2026-11-11T16:00Z.
Status: NO CUT. This record appends to the same tick's earlier pass (commit `223ea96`) and re-verifies it; it does not replace it.

## 1. Target and identity

| Item | Value |
|---|---|
| Repo root | `/home/kara/warpfs` (lane stand-in workdir is only a carrier) |
| Remote | `git@github.com:gethilo/hilo.git` (origin — matches the released project, folder name is `warpfs`) |
| GitHub repo | `gethilo/hilo`, default branch `master`, visibility PUBLIC |
| Certified tip | `223ea96bf9faeedb541f3487085eef0388bd94a2` (local fetch == `git ls-remote`; local `master` re-anchored to this SHA this pass) |
| Product tree | `bc38f28caf18f586c2bc2803db54db1b8534f225` (tip is a board-only commit; product files identical) |
| Baseline | last real tag `v0.4.0` peels to `57002335c7295500b48ac65f7e2d08f260ab28b5` |
| Current published release | `hilo 0.4.0` with assets `hilo-0.4.0-x86_64-linux.tar.gz` (32,941,916 B) + `.sha256` (97 B) — intact, unmodified |

## 2. Commit census (v0.4.0..223ea96)

- Total merge-inclusive: 39. Non-merge: 37.
- Typed subjects: 2 `fix`, 5 `chore`. Zero `feat`, `perf`, `docs`, `test`, `refactor`.
  - `c89bf2d fix(deps): remediate runtime RustSec advisories` (material)
  - `e3db55b fix(s3): bound proxy-disconnect test failure` (patch)
- Breaking: 0. The one `BREAKING CHANGE` string in the range is prose inside releng commit `8c5547e` ("zero BREAKING CHANGE markers"), not a declaration — checked by opening the commit, not by grep count.
- Remaining subjects are non-conventional `release:` / `releng:` / `board:` / `qa(...)` bookkeeping.
- Product-path diff excluding `.coding-hermes` and `.gitreins`: 24 files, +744/−437 (Cargo.lock + 11 manifests, `hilo-backends/src/s3.rs`, `hilo-graph/src/graph.rs` + tests, `.github/workflows/ci.yml`, `scripts/audit-gate.sh`, `scripts/rust-lint.sh`, `tests/test_gitreins_rust_guard.py`, `docs/releasing.md`, `CHANGELOG.md`).
- Semver: **patch v0.4.1**. Material fix + security remediation; no major marker, so no owner-confirmation path is open.

## 3. Gate evidence

| Gate | Result | Receipt |
|---|---|---|
| Exact-SHA hosted CI (tip `223ea96`) | **RED / unverified** | run `43045903594` — rerun issued by this pass at 15:47Z; failed 15:55Z in `Install system dependencies`; `Check formatting`, `Build`, `Clippy`, `Test`, `Advisory gate` all skipped (never executed) |
| Exact-SHA hosted CI (`bc38f28`) | RED, same step | run `43044173273` (15:01Z) |
| Exact-SHA hosted CI (`9234171`) | RED, same step | run `43040495853` (14:38Z) |
| Pages workflow | green, not a product gate | run `43044173341` @ `bc38f28` |
| Failure text | `Temporary failure resolving 'azure.archive.ubuntu.com'` | apt errors Err:2..Err:5 for noble, noble-updates, noble-backports, noble-security; per-package failures on `attr`, `libsepol-dev`, `libselinux1-dev`; exit 100 |
| Local `cargo fmt --all -- --check` | rc=0 | detached worktree `/home/kara/.hermes/cache/scratch/wt-v041a` @ `223ea96` |
| Local `bash scripts/audit-gate.sh` | rc=0 | 2 lock-only advisories tolerated (`rkyv`, `zip`), runtime-edge intersection empty |
| Local `cargo fetch --locked` | rc=0 | ambient `CARGO_HOME=/var/tmp/hermes-cargo-home` |
| GitReins tier-2 | PASS at `bc38f28` | verdict `a1938617` (task `RELEASE-020-R4`); prior `32b2dc80` FAIL superseded. Caveat: `working_tree_dirty_path_count=3` — the dirty paths are board files, product files clean |
| Dogfood / QA | partial, not bound to `bc38f28` | open QA rows QA-WARPFS-QA-3, QA-WARPFS-DOCS-1, QA-WARPFS-031..039, DF-WARPFS-172/R2-R2 |

Three consecutive exact-SHA failures of the same pre-gate step across two hours ⇒ sustained runner/network outage, not a one-run blip. Local passes cannot clear an exact-SHA hosted gate.

## 4. Decision

**NO CUT.** No tag created, no Release object created, no asset built or uploaded, no existing tag moved.

Rationale: the release rule blocks a cut on red, absent, skipped or unproven exact-SHA hosted CI without an explicit owner override; none was given. Skipped gate steps are unproven, not green. Tagging now would publish a security claim the hosted gate never tested.

Blocked on `INT-CI-017`, restored to the canonical board this pass (it existed only in an unpushed local commit) and bumped P2 → P1 because it is the sole blocker on the v0.4.1 security patch.

## 5. Version hygiene

- Workspace `[workspace.package] version = 0.4.1`; all 11 member crates `0.4.1`; `CHANGELOG.md` has `## [0.4.1] — 2026-11-11` and `## [Unreleased]`; `docs/releasing.md` documents the advisory gate and the asset bundle; `Makefile:30 release` target and `scripts/build-hilo.sh` are repository-owned.
- No stray `0.4.0` literal in any tracked product file (remaining hits are `.gitreins` rescued patches and third-party `Cargo.lock` entries). No README badge to update.
- Gap: no `README.md` / `CONTRIBUTING.md` link to `docs/releasing.md` — `RELEASE-017` stays open on that arm.
- No tag-triggered release workflow exists (`RELEASE-015` open); publication is operator-run `gh release create`, as for v0.4.0.

## 6. Deployed-binary staleness

- `/home/kara/.cargo/bin/hilo` → `hilo 0.4.0`, `build: v0.3.0-293-g3adfb9ed`, built 2026-10-08. Stale.
- `target/release/hilo` → `hilo 0.4.0`, `build: v0.3.0-818-ga046c89f`, built 2026-11-08. Stale.

Neither is a v0.4.1 artifact; nothing in this pass produced a binary.

## 7. Premise corrections for the next pass

1. `RELEASE-018` — its environment half is stale. The lane shell no longer exports `CARGO_HOME=/mnt/usb512/cargo-home`; ambient is `/var/tmp/hermes-cargo-home` + `/var/tmp/hermes-target`, both writable, and `cargo fetch --locked` exits 0 at `223ea96`. Re-probe the cargo-home arm; the guard budget/orphan-lock arms are untouched.
2. A local-only commit `c1d65b4` on this host claimed "close RELEASE-020 (v0.4.1 landed, judge PASS)". No `v0.4.1` tag or Release has ever existed (`git ls-remote refs/tags/v0.4.1` empty; `gh release list` shows only 0.4.0 and v0.3.0). That commit was never pushed, its closure was **not** adopted, and it is preserved on branch `releng/local-divergence-c1d65b4`; its `INT-CI-017` row is restored to the canonical board with attribution.

## 8. Unblock sequence (mechanical, next pass)

1. Land the `INT-CI-017` fix for the apt step (retries + explicit `archive.ubuntu.com`/`security.ubuntu.com`, drop the optional `libssl-dev`); do not skip or condition any gate step.
2. Push to `master`; require a green run on that SHA with `Check formatting`, `Build`, `Clippy`, `Test`, `Advisory gate` all executed and green.
3. Build the asset from the tag tree: annotated `v0.4.1` → detached worktree → `scripts/build-hilo.sh -p hilo-cli` → `strip` → `hilo-0.4.1-x86_64-linux.tar.gz` + `.sha256` (match the v0.4.0 bundle shape).
4. `gh release create v0.4.1` with categorized notes plus a `## Known issues` section (QA partials, FUSE TMPDIR, upgrade-cell ENV-BLOCK).
5. Verify: peeled tag commit == release record, both assets downloaded and re-hashed, tag-SHA CI green, then close `RELEASE-020` against its own acceptance criteria.

## 9. What was not mutated

No tag, no Release, no asset, no `ci.yml` edit, no `Cargo.*` edit, no deployed binary. Board writes this pass: `RELEASE-020` note append, `INT-CI-017` row (restored, P1), three `events.jsonl` appends (10079–10081). Local `master` was re-anchored from the divergent `c1d65b4` to `origin/master` (223ea96); `c1d65b4` is preserved on branch `releng/local-divergence-c1d65b4`.
