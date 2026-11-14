# Releng sweep — warpfs (pass 3, re-fire) — warpfs-releng-2026-11-14-21-06-01

Recorded 2026-11-14T23:47Z. Verify-only lane: no product code changed, no version
prepared, no tag created, no Release object mutated.

## Target resolution

| Fact | Value | Source |
|---|---|---|
| Repo under release management | `/home/kara/warpfs` | fleet.toml lane `warpfs-releng` (`repo_url = local:/home/kara/warpfs`) |
| Forge repo | `gethilo/hilo` (PUBLIC, default branch `master`) | `gh repo view gethilo/hilo --json defaultBranchRef,visibility` |
| Origin | `git@github.com:gethilo/hilo.git` | `git -C /home/kara/warpfs remote -v` |
| Baseline tag | `v0.4.1` → peel `4216c7f456a73b4a64b614d8f1f6addf88f65d6f` | `git ls-remote origin 'refs/tags/v0.4*'` (unchanged this pass) |
| Candidate | `cb4ada12a9be512431158de0131b63b6a5147e35` | `git rev-parse origin/master` == `git ls-remote origin refs/heads/master` (exact remote parity) |
| Candidate worktree | clean detached `.../warpfs-releng-20261114-2345a` | `git worktree add --detach`, `status --porcelain` empty |

## Pass relationship (this tick has fired three times)

| Pass | Carrier | Tree | Result |
|---|---|---|---|
| 1 | `de2ac4c` | `f9efdd7` | full sweep, NO CUT |
| 2 | `16f625f` | `de2ac4c` | cleared the stale RELEASE-018 blocker on RELEASE-012 |
| 3 (this) | — | `cb4ada1` | delta pass: tip moved 4 commits, all bookkeeping |

`git diff --name-only f9efdd7..cb4ada1` touches **only** `.coding-hermes/` paths
(0 product paths). Material tree is unchanged from pass 1, so the census is
carried, not re-derived; the receipts that the new tip invalidates (candidate
SHA, CI run, deployed binary) were re-measured below.

## Census `v0.4.1..cb4ada1` (25 non-merge commits)

| Conventional class | Count | Release effect |
|---|---|---|
| `fix` | 3 | patch (`f9efdd7` workspace-mount xattrs, `1d90760` rust-lint process-group reap, `84c0267` Go module-local import classification) |
| `feat` | 0 | — |
| `perf` | 0 | — |
| breaking (`!` subject or `BREAKING CHANGE` footer) | 0 | no major |
| non-release (`ops` 5, `chore` 5, `releng` 4+1, `board` 3, `audit` 3) | 21 | bookkeeping |

Product-path diff excluding `.coding-hermes/` and `.gitreins/` is **11 files**
(`hilo-fuse/src/workspace_mount.rs`, `hilo-graph/src/{impact,parser,resolution}.rs`,
two test files, `scripts/rust-lint.sh`, `tests/test_gitreins_rust_guard.py`,
`README.md`, `docs/hilo-fuse.md`, `docs/hilo-metadata.md`).

**Semver direction: patch → `v0.4.2` is due.** This pass does not cut it (gates below).

## Gates

| Gate | State | Evidence |
|---|---|---|
| Exact-candidate CI | GREEN | run `43650138102` on `cb4ada1` — completed success |
| Baseline tag CI | GREEN | run `43281097211` on `v0.4.1` — success |
| Existing release object | intact | `gh release view v0.4.1` → `hilo-0.4.1-x86_64-linux.tar.gz` 32735042 B `uploaded`, `.sha256` 178 B `uploaded` |
| Version hygiene vs `v0.4.2` | **FAIL (unprepared)** | `Cargo.toml:31` = `0.4.1`, `hilo-cli/Cargo.toml:3` = `0.4.1`, `CHANGELOG.md` `grep -c '0\.4\.2'` = 0 |
| Release tooling present | partial | `Makefile` `release` → `scripts/build-hilo.sh` → strip; `scripts/audit-gate.sh`; `docs/releasing.md` (2813 B) |
| Tag-triggered release workflow | **ABSENT** | `.github/workflows/ci.yml` triggers `branches: [main, master]` only; `pages.yml` likewise. Tagging `cb4ada1` today would produce **zero** CI evidence for the tag commit → RELEASE-015 stands |
| Clean-environment release path | **FAIL** | `scripts/build-hilo.sh` invokes bare `cargo` (no self-resolution, no toolchain diagnostic) → RELEASE-019 stands |
| Candidate-bound GitReins verdict | **MISSING** | latest in-tree `.gitreins/history/2026-11-14/10345fc0/verdict.json` is tier2 PASS for `84c0267` (DF-WARPFS-173), not for `cb4ada1` |
| Candidate-bound dogfood/QA | **MISSING** | in-tree dogfood records are historical/environment-scoped |

## Deployed binary (was the pass-1 finding — now discharged)

`/home/kara/.cargo/bin/hilo` → `hilo 0.4.1`, `build: v0.4.0-50-g4216c7f`,
built `2026-11-12T18:40:55Z`, mtime `2026-11-14 17:34`. Source commit `4216c7f`
**is** the `v0.4.1` tag commit, so the installed binary is current; landed by
`de91ecc` ("refresh installed hilo to published v0.4.1"). No staleness finding
remains. (Cosmetic: the stamp names the describe form `v0.4.0-50-g4216c7f`
rather than `v0.4.1`.)

## Open follow-ups (verified premises)

| Row | Premise now | Note |
|---|---|---|
| RELEASE-012 | rolling readiness row, pending | updated in place this pass |
| RELEASE-015 | TRUE | no tag trigger anywhere in `.github/workflows/` |
| RELEASE-016 | **STALE VERSION** | row targets the `v0.4.0` asset; `v0.4.1` is the current release. Retarget to `hilo-0.4.1-x86_64-linux.tar.gz` + its `.sha256` |
| RELEASE-017 | **PARTIAL** | `docs/releasing.md` already exists (2813 B, present at `cb4ada1`); residual is the public README/CONTRIBUTING link (both files have zero `releasing` mentions) plus captured command output. Do not re-author the doc |
| RELEASE-019 | TRUE | `scripts/build-hilo.sh` calls bare `cargo` |

## Decision

**NO CUT.** `v0.4.1` remains the current release and is not superseded. A patch
`v0.4.2` is due on the next release-capable tick once (a) version + changelog
are prepared, (b) a candidate-bound GitReins verdict and fresh candidate-bound
dogfood/QA exist, and (c) either RELEASE-015 lands (tag-triggered build/attach)
or the maintainer accepts that the tag commit will carry no CI run — the
repository-owned `make release` path is the only asset producer today. No tag,
Release object, asset, or existing release was mutated; nothing was force-moved.
