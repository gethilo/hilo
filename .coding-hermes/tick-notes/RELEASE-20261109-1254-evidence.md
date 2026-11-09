# warpfs release readiness evidence — 2026-11-09 re-delivery pass 2

Target: gethilo/hilo
Canonical checkout: /home/kara/warpfs
Certified candidate: d578bf415f00e515cc27ee251a5396bad92e7c1f
Default branch: master
Remote parity: local origin/master and git ls-remote origin refs/heads/master both resolve to the certified candidate.
Baseline: v0.4.0 -> 57002335c7295500b48ac65f7e2d08f260ab28b5

## Decision

NOT DUE under the release-engineering no-material-delta rule. v0.4.0 is already published and the post-tag range contains only board, PM, and release-readiness bookkeeping: one conventional chore and five non-conventional releng/PM commits, with no feat, fix, perf, or breaking marker. The product-path diff is empty. No tag was created or moved, no GitHub Release was created, and no artifact was published in this pass.

This is a re-delivery because master advanced after the first pass. The previous v0.4.0 release remains verified. This pass refreshes canonical board row RELEASE-012 rather than creating a duplicate. Open follow-ups RELEASE-015 through RELEASE-018 remain recorded separately.

## Commits and semver

The exact v0.4.0..candidate range has 6 non-merge commits:

- 1 chore: 89a4440 chore(board): file RELEASE-014..017, close RELEASE-013 — v0.4.0 cut published
- 5 other/non-conventional bookkeeping commits: dc4f34e PM: make five owner-gated slices dispatchable; 773087b PM(dispatchability): warpfs board repair; aeb8cad releng: refresh v0.4.0 readiness evidence — no material post-tag delta; e7171a3 releng: record v0.4.0 no-material-delta readiness pass; d578bf4 releng: correct readiness artifact hash in RELEASE-012
- 0 feat, 0 fix, 0 perf, 0 docs, 0 test, 0 refactor
- 0 BREAKING CHANGE footers and 0 type-bang breaking subjects
- Product-path diff excluding .coding-hermes/board and .coding-hermes/tick-notes: empty

There is no material product delta after v0.4.0, so no patch/minor bump is proposed. Major is not implicated.

## Version hygiene and release tooling

The workspace and all 11 published workspace crates report version 0.4.0. CHANGELOG.md contains [Unreleased] followed by [0.4.0] dated 2026-11-09. The repository-owned Makefile release target remains the authoritative build path: scripts/build-hilo.sh -p hilo-cli, strip target/release/hilo, then list the binary. No scripts/release*, GoReleaser configuration, Dockerfile, docs/releasing.md, or tag-triggered release workflow exists. RELEASE-015 and RELEASE-017 own those repeatability gaps.

## Hosted CI

Exact candidate run 42662211165 on d578bf415f00e515cc27ee251a5396bad92e7c1f completed success. Its single required job, Build & Test (stable), passed setup, formatting, build, clippy, and test. Check-runs API for the certified candidate reports one completed/success check. The earlier candidate run 42657026493 on 773087b is superseded by this newer certified tip and is not used as the current gate.

## GitReins

No stored GitReins verdict was found bound to the certified candidate. Historical verdicts are not candidate evidence: QA-WARPFS-032 is passed=false on 0dd62c472feb2fa88dc7e8250367056d136b0558 because its S3 proxy test still hangs; DF-WARPFS-80 is passed=true on b0753403d8e52b76b2341d7e3b2f9d76d0d25d71; DF-WARPFS-168-R3 has historical verdicts on 10d5898985b585d800d5253160fb1f41aa276f20. None is bound to d578bf4.

## Dogfood and QA

The latest committed dogfood record is Run 48 from 2026-10-26, classified UNKNOWN-VALUE because it used a stale v0.3.0-542-gd1f88c75 artifact and could not complete a current-head build within its budget. Run 47b later recorded a completed fresh-install cell but remained PROMISING-BUT-ROUGH for its broader surface. The latest committed clean-machine QA evidence is not bound to d578bf4 and remains partial/unverified; prior evidence graded only 2 of 14 canonical cells and left 11 UNVERIFIED. These are not release blockers for this pass because no release is warranted, but they remain open evidence gaps for the next material cut. RELEASE-016 owns the clean-machine receipt.

## Previous release artifact and deployed freshness

v0.4.0 is a public, non-draft, non-prerelease GitHub Release published 2026-11-09T16:07:29Z. The annotated tag peels to 57002335c7295500b48ac65f7e2d08f260ab28b5 and has not moved. Assets are hilo-0.4.0-x86_64-linux.tar.gz (32,941,916 bytes, uploaded) and its .sha256 file (97 bytes, uploaded); the recorded asset checksum is aa066b2c823e475bf0fa135abbcf2b74a5f6870b921d59d6f21edd5624f493a7. The prior release's exact-tag CI run 42639045303 was green.

The installed /home/kara/.cargo/bin/hilo reports version 0.4.0 but build v0.3.0-293-g3adfb9ed, built 2026-10-08, with a 1,424,102,520-byte file. It is stale and not evidence about the published asset; do not refresh it in this verification-only pass. The repository target/release/hilo also reports build v0.3.0-818-ga046c89f and predates the certified candidate; it is likewise not release evidence.

## Open blockers and next gates

- RELEASE-015: add a tag-triggered CI artifact publisher.
- RELEASE-016: obtain a clean-machine install/capability receipt for v0.4.0.
- RELEASE-017: document the release procedure and evidence contract.
- RELEASE-018: repair the unmounted CARGO_HOME environment and distinguish toolchain failures from lint findings in the guard.
- RELEASE-012 remains the rolling readiness row; no duplicate RELEASE row was filed.

## Board receipt

This evidence file is intended to be committed with the RELEASE-012 refresh. No tag, Release object, or new artifact was created by this sweep.
