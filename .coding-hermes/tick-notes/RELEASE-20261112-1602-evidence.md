# Release readiness record — warpfs / gethilo/hilo — tick warpfs-releng-2026-11-12-16-02-23

Author: release-engineering satellite. Written 2026-11-12T19:00Z.
Status: **CERTIFIED, NOT CUT.** Every gate that a cut depends on is green and was re-verified this pass. The publishing half (asset packaging, annotated tag, `gh release create`) did **not** run: two commands in this tick were stopped by the harness consent gate (an `--amend` + `--force-with-lease` retry to fix a missing co-author trailer, and the asset-packaging command that contained `rm -rf` on the staging dir). Per the gate's instruction neither was retried or rephrased, and no further irreversible action was taken. Nothing was tagged, published, or uploaded.

## 1. Target and identity

| Item | Value |
|---|---|
| Repo root | `/home/kara/warpfs` (lane stand-in workdir is only a carrier) |
| Remote | `git@github.com:gethilo/hilo.git` — matches the released project (folder name is `warpfs`) |
| GitHub repo | `gethilo/hilo`, default branch `master`, PUBLIC |
| Baseline tag | `v0.4.0` peels to `57002335c7295500b48ac65f7e2d08f260ab28b5` |
| Candidate / certified tip | `4216c7f456a73b4a64b614d8f1f6addf88f65d6f` (= `origin/master` after this tick's push) |
| Previous tip | `35b2a774` (also green; superseded by the docs-prep commit below) |
| Build carrier | detached worktree `/home/kara/.hermes/cache/scratch/warpfs-releng-2026-11-12-16-02-23` @ `4216c7f` |

## 2. What this pass changed in the tree (one commit, docs only)

`4216c7f` — `release: v0.4.1 cut prep — correct the changelog against the lock`.
Pushed to `origin/master` (fast-forward from `35b2a77`, guard PASS, secrets clean).

Reason: the `[0.4.1]` changelog entry claimed `cap-std 3.4.5 → 4.0.3`. The crate is **not in the 0.4.1 lock at all** — 0.4.0 resolved the cap-std family (`cap-std`, `cap-fs-ext`, `cap-rand`, `cap-time-ext`); 0.4.1 resolves only the transitive `cap-primitives`. The entry also restated an advisory count from memory ("22"). Both were replaced with measured values, the heading date moved to the cut date, and the CI apt fallback + bounded s3 test were named as part of this release.

Known deviation, disclosed: the commit carries **no `Co-authored-by: Alexis Okuwa` trailer**, unlike its neighbours. The amend+force-with-lease retry that would have added it was the first command stopped by the consent gate; it was not retried. Trailer convention is otherwise intact in the repo.

## 3. Commit census (`v0.4.0..4216c7f`)

- Total 50, non-merge 48 (was 49/47 at `35b2a77`; this pass adds the docs-prep commit).
- Typed non-bookkeeping product fixes: `fix(deps)` `c89bf2d` (material), `fix(s3)` `e3db55b` (test bound), `fix(ci)` `4c0c730` (apt mirror fallback), plus `release:` prep `9234171` / `bc38f28` (advisory remediation + gate enforcement).
- Zero `feat`. Zero `!` subjects; the only `BREAKING CHANGE` string in the range is prose inside a releng commit, not a declaration.
- Semver: **patch `v0.4.1`** — matches the workspace stamp (`[workspace.package] version = 0.4.1`, all 11 member crates `0.4.1`).
- Product diff excluding `.coding-hermes`/`.gitreins`: 24 files, +769/−439.

## 4. Gate evidence (all re-run this pass, live)

| Gate | Result | Receipt |
|---|---|---|
| Exact-SHA hosted CI @ `4216c7f` | **SUCCESS — every step executed, none skipped** | run [43281097211](https://github.com/gethilo/hilo/actions/runs/43281097211): `Install system dependencies` ✓, `Check formatting` ✓, `Build` ✓, `Clippy` ✓, `Test` ✓, `Install cargo-audit` ✓, `Advisory gate (runtime-reachable RustSec)` ✓ |
| Exact-SHA hosted CI @ `35b2a77` (prior tip) | SUCCESS, same 6 gates | run `43216307022` |
| `scripts/audit-gate.sh` @ candidate | **PASS** — runtime-edge intersection empty; 2 lockfile-only advisory packages printed (`rkyv` ×1, `zip` ×4) | run in the carrier worktree, rc=0 |
| `scripts/audit-gate.sh` @ `v0.4.0` (contrast, same day) | **FAIL** — 11 runtime-reachable packages | `cap-primitives, ctutils, h2, libz-sys, rustls, rustls-webpki, wasmtime, webpki-roots, zstd, zstd-safe, zstd-sys` |
| `cargo audit --file Cargo.lock --json` @ `v0.4.0` | 33 advisory records, 14 packages | of which 23 records are on the 11 runtime-reachable packages; `quinn-proto` ×5, `zip` ×4, `rkyv` ×1 are not runtime-reachable |
| `cargo audit` @ candidate | 5 records, 2 packages (`rkyv` ×1, `zip` ×4) | all lock-only/build-only per the gate |
| Informational classes @ candidate (not blocked by policy) | 8 unsound (`tokio`, `rustix`, `git2` ×3, `fuser`, `lru`, `arrow-data`), 5 unmaintained, 2 yanked | disclosed in the release notes draft; worth its own row |
| `scripts/rust-lint.sh` (the guard's test command) @ carrier tree | **PASS** rc=0 in 5m51s: clippy `--workspace --all-targets -D warnings` clean, `cargo test -p hilo_graph --lib` **583 passed / 0 failed**, rust-analyzer version proof, `skills/hilo-usage/SKILL.md` surface check PASS | /var/tmp/hermes-target warm |
| GitReins Tier 1 guard on the commit | **PASS** (test mode: full, secrets clean) | `.gitreins/logs/guard-20261112T183057.261509Z.log` |
| Release build from the certified tree | **SUCCESS** rc=0 | `scripts/build-hilo.sh -p hilo-cli`; first attempt was SIGTERM-killed at 124m14s (`real 124m14s`, exit 143) with libduckdb-sys mid-batch (host load 28–44); the re-run under a `systemd-run --user` unit resumed the cached objects and finished in 15m16s (20.6 GB peak), then a forced build-script rerun took 26s |
| Binary provenance | `hilo 0.4.1` / `build: v0.4.0-50-g4216c7f` / `built: 2026-11-12T18:40:55Z` | unstripped 1,521,882,120 B at `<carrier>/target/release/hilo`; stripped 129,192,560 B; `--version`, `--help`, `graph --help` all rc=0 |

## 5. Decision

**NO TAG, NO RELEASE.** Not on gate grounds — the gates are green and the exact-SHA receipt is the strongest in this project's history (11/11 steps executed, `Test` and `Advisory gate` actually ran). The publishing half was blocked by the consent gate on the packaging command; the tag and the Release object are irreversible under fleet doctrine, so they were not attempted after that stop.

Resume point is mechanical and costs minutes, not hours: the certified binary already exists at
`/home/kara/.hermes/cache/scratch/warpfs-releng-2026-11-12-16-02-23/target/release/hilo`
(strip → stage `hilo` + `README.md` + `LICENSE` → tar.gz + `.sha256` → annotated tag `v0.4.1` on `4216c7f` → `gh release create` with the drafted notes → re-download and re-hash both assets).

## 6. Version hygiene

- `[workspace.package] version = 0.4.1`; all 11 member crates `0.4.1`; `CHANGELOG.md` has `## [0.4.1] — 2026-11-12` (this pass) and `## [Unreleased]`; `docs/releasing.md` documents the advisory gate and the bundle shape.
- No stray `0.4.0` literal in product files (remaining hits are historical `docs/dogfood/*` run records, correct to keep).
- No tag-triggered release workflow (`RELEASE-015` open); publication stays operator-run.
- `RELEASE-017` is **partially** satisfied: `docs/releasing.md` exists and covers tag shape/assets/sha256/evidence, but neither `README.md` nor `CONTRIBUTING.md` links to it. Row left open on that arm.

## 7. Deployed-binary staleness

- `/home/kara/.cargo/bin/hilo` → `hilo 0.4.0`, `build: v0.3.0-293-g3adfb9ed`, 1.42 GB, built 2026-10-08. Stale (unstripped too).
- Nothing in this pass installed a binary; the v0.4.1 artifact is staged in scratch only.

## 8. Hygiene findings (filed/annotated, not caused by this pass)

1. **Abandoned product work in the shared checkout.** `/home/kara/warpfs` carries uncommitted edits to `hilo-graph/src/{impact,parser,resolution}.rs` (+77/−20, last touched 2026-11-11 23:53–23:59 local) plus `.gitreins/tasks.yaml`. They are a DF-WARPFS-173 candidate and they **do not compile**: `rust-lint.sh` fails on `hilo-graph/src/parser.rs:505` (`clippy::if_same_then_else`, deny-level). Consequence: no commit can pass the guard from that checkout until the work lands or is reverted. Not touched by this pass.
2. **Why the docs commit was made in a carrier worktree:** the guard lints the *working tree*, so the dirty checkout could not produce a green commit. The carrier worktree at `4216c7f` gave a clean, guard-passing, CI-certified commit.
3. **Advisory gate scope:** the gate blocks runtime-reachable *vulnerabilities* only; 8 `unsound` advisories on runtime crates remain (see §4). Policy choice, but it should be stated in the release notes rather than discovered.

## 9. What was not mutated

No tag, no Release object, no asset uploaded, no `Cargo.*`/workflow/product file edited, no deployed binary installed, no existing tag moved. The only tree change is the docs-prep commit `4216c7f` (CHANGELOG.md, +18/−5) plus this tick's board/note writes.
