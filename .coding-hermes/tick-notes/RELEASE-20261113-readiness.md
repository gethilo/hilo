# WarpFS release readiness sweep — 2026-11-13

Decision: NO CUT / verification-only. The current published release is v0.4.1; the current branch tip contains no material product changes since that tag.

## Identity

- Product checkout: `/home/kara/warpfs`
- Canonical remote: `origin` → `git@github.com:gethilo/hilo.git`
- Forge repository: `gethilo/hilo`
- Default branch: `master`
- Certified current candidate: `01055257ed6e774cbb9de4bfe8b5c5387d970815`
- Remote parity: `origin/master` equals the candidate; ahead/behind `0 0`
- Product checkout: clean at the time of the sweep

## Baseline and commit inventory

Newest release tag is `v0.4.1`, peeled to `4216c7f456a73b4a64b614d8f1f6addf88f65d6f`.

The range `v0.4.1..01055257ed6e774cbb9de4bfe8b5c5387d970815` contains 9 non-merge commits: 0 feat, 0 fix, 0 perf, 0 docs, 0 test, 0 refactor, 1 chore, and 8 other/non-conventional bookkeeping subjects. There are 0 `!` subject markers and 0 `BREAKING CHANGE:` footers. The product-path diff excluding `.coding-hermes`, `.gitreins`, and `.vfs` is empty. This does not propose a new version; v0.4.2 is not due.

## Version and release tooling

- All 12 workspace Cargo manifests report version `0.4.1`.
- `CHANGELOG.md` contains `[0.4.1]` and an empty `[Unreleased]` section.
- Repository-owned `Makefile` has `release`, invoking `scripts/build-hilo.sh -p hilo-cli` followed by `strip`.
- `docs/releasing.md` is tracked and documents the release path.
- No GoReleaser, Dockerfile, or `scripts/release*` publisher was found.
- Existing release tooling is manual and the release-triggered CI workflow gap remains tracked by `RELEASE-015`.

## CI and GitReins

- Exact current-tip GitHub Actions run `43465790927` for SHA `01055257ed6e774cbb9de4bfe8b5c5387d970815`: completed/success.
- Its single `Build & Test (stable)` job completed all named steps successfully, including system dependencies, formatting, build, Clippy, tests, cargo-audit installation, and the runtime-reachable advisory gate; no step was skipped.
- Exact current-tip GitReins Tier 1 verdict: `.gitreins/history/2026-11-13/546e5182/verdict.json`, `passed: true`, evaluated commit `01055257ed6e774cbb9de4bfe8b5c5387d970815`. It is Tier 1 only and records one dirty working-tree path in the evaluated worktree; it is not a fresh Tier 2 release verdict.

## Existing release and artifact verification

- GitHub Release `v0.4.1` exists and is published.
- Remote annotated tag `v0.4.1` peels to `4216c7f456a73b4a64b614d8f1f6addf88f65d6f`; it was not moved.
- Release assets are `hilo-0.4.1-x86_64-linux.tar.gz` and its `.sha256` sidecar.
- Downloaded tarball hash: `a96838c48a2dd772ab11eea1122589aa131d7d8679608d63ba26a1eeb531718c`; sidecar verification passed.
- Extracted binary reports `hilo 0.4.1` and build `v0.4.0-50-g4216c7f`.
- Release tag CI run `43281097211` completed success with all 11 release-gate steps executed.

## Known issues and open release follow-ups

- The deployed `/home/kara/.cargo/bin/hilo` is stale: `hilo 0.4.0`, build `v0.3.0-293-g3adfb9ed`, built 2026-10-08. It is not the v0.4.1 release artifact. This is a deployment freshness issue, not evidence that the published release is invalid.
- `RELEASE-015` remains open for a tag-triggered release workflow.
- `RELEASE-016` remains open for a clean-machine install receipt and libfuse3 prerequisite documentation.
- `RELEASE-017` remains open for documenting the release process and linking it from tracked public project documentation.
- `RELEASE-018` remains open for its deliberate cold-target and timeout-orphan acceptance arms; its original unmounted-volume premise is stale and must not be repeated.
- `RELEASE-019` remains open for bare-environment PATH/toolchain handling in the release build path.
- Existing QA rows remain partial or environment/harness-scoped, including `QA-WARPFS-025`, `QA-WARPFS-DOCS-1`, `QA-WARPFS-031` through `QA-WARPFS-040`, `QA-WARPFS-037`, and `DF-WARPFS-161-R2-R2`. They do not create a new tag candidate because no product-path changes occurred after v0.4.1.

## Release decision

Do not create a tag or GitHub Release. No tag, release object, artifact, or existing release was mutated by this sweep. The next release candidate requires a material product change after `v0.4.1`, fresh exact-SHA gates, and re-verification of the named open follow-ups.
