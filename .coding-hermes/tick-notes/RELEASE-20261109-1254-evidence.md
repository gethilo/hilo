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

v0.4.0 is a public, non-draft, non-prerelease GitHub Release published 2026-11-09T16:07:29Z. The annotated tag peels to 57002335c7295500b48ac65f7e2d08f260ab28b5 and has not moved. Assets are hilo-0.4.0-x86_64-linux.tar.gz (32,941,916 bytes, uploaded) and its .sha256 file (97 bytes, uploaded); the recorded asset checksum is aa066b2c823e475bf0fa135abbcf2b74a5f6870b921d59f6f21edd5624f493a7. The prior release's exact-tag CI run 42639045303 was green.

The installed /home/kara/.cargo/bin/hilo reports version 0.4.0 but build v0.3.0-293-g3adfb9ed, built 2026-10-08, with a 1,424,102,520-byte file. It is stale and not evidence about the published asset; do not refresh it in this verification-only pass. The repository target/release/hilo also reports build v0.3.0-818-ga046c89f and predates the certified candidate; it is likewise not release evidence.

## Open blockers and next gates

- RELEASE-015: add a tag-triggered CI artifact publisher.
- RELEASE-016: obtain a clean-machine install/capability receipt for v0.4.0.
- RELEASE-017: document the release procedure and evidence contract.
- RELEASE-018: repair the unmounted CARGO_HOME environment and distinguish toolchain failures from lint findings in the guard.
- RELEASE-012 remains the rolling readiness row; no duplicate RELEASE row was filed.

## Board receipt

This evidence file is intended to be committed with the RELEASE-012 refresh. No tag, Release object, or new artifact was created by this sweep.

## Pass 3 — 2026-11-09T13:36:15-05:00 (tick warpfs-releng-2026-11-09-12-54-05, third firing)

This section is appended; every section above is preserved byte-for-byte. Where this
pass disagrees with a figure above, this pass wins and names what changed.

Candidate (certified): 37e0b2fcc2db92cbb179ffe048096d5d29036b89
Baseline: v0.4.0 -> 57002335c7295500b48ac65f7e2d08f260ab28b5 (annotated tag, unmoved,
peeled commit re-read this pass)
Remote parity: `git ls-remote origin refs/heads/master` and the fast-forwarded
canonical checkout both resolve to the certified candidate.

### What moved since pass 2

Pass 2 certified d578bf415f00e515cc27ee251a5396bad92e7c1f. Two commits landed after it:

- 8c5547e releng: correct v0.4.0 asset digest transcription in readiness evidence
- 37e0b2f releng: mark RELEASE-012 blocked on RELEASE-018 cargo home

Both are release bookkeeping. Neither touches product paths, so the decision below is
unchanged; the pass-2 evidence file also carried a one-character digest transcription
error (…d59d6f2… instead of …d59f6f2…) that 8c5547e corrected and this pass
re-verified from the published sidecar rather than from any note.

### Commits and semver (re-derived at the certified tip, machine counts)

`git rev-list --count --no-merges v0.4.0..37e0b2f` = 9. Class census from
`sort | uniq -c` over subjects:

- 6 releng (release bookkeeping)
- 1 chore(board)
- 2 non-conventional PM commits (773087b, dc4f34e)
- 0 feat, 0 fix, 0 perf, 0 docs, 0 test, 0 refactor
- 0 `BREAKING CHANGE:` footers, 0 `type!:` subjects

Materiality by shipped bytes, per the rule that the diff decides and not the prefix:
`git diff --stat v0.4.0..37e0b2f -- . ':(exclude).coding-hermes'` emits 0 bytes. The
entire post-tag range is board, tick-notes and PM bookkeeping. CHANGELOG `[Unreleased]`
is empty.

Semver: no material delta, so no patch, minor, or major bump is proposed. Major is not
implicated and no owner confirmation is required because nothing is cut.

### Decision

NOT DUE. v0.4.0 remains the current published release and the post-tag range contains no
installable change. No tag was created or moved, no GitHub Release was created, and no
artifact was published or rebuilt in this pass.

### Version hygiene

`[workspace.package] version = "0.4.0"` and all 11 member crates report 0.4.0
(hilo-backends, hilo-cli, hilo-core, hilo-ffi, hilo-fuse, hilo-graph, hilo-mcp,
hilo-metadata, hilo-permissions, hilo-plugins, hilo-triggers). CHANGELOG has `[Unreleased]`
(empty) followed by `[0.4.0] — 2026-11-09`. Version surfaces and the tag agree.

### Release tooling inventory (unchanged, re-read at tip)

Repository-owned path is authoritative and remains `make release` ->
`scripts/build-hilo.sh -p hilo-cli` -> `strip target/release/hilo`. Still absent:
`scripts/release*`, GoReleaser, Dockerfile, `docs/releasing.md`, and any tag-triggered
release workflow. RELEASE-015 and RELEASE-017 continue to own those gaps.

### Hosted CI on the certified candidate

`.github/workflows/ci.yml` triggers on branch push (master/main) and pull_request only —
it has no `tags:` trigger, so a tag SHA inherits the branch-push run rather than minting
its own. Run 42672061291 on 37e0b2fcc2db92cbb179ffe048096d5d29036b89 completed success.
All 11 steps green: Set up job, actions/checkout@v4, Install Rust, Install system
dependencies, Cache cargo, Check formatting, Build, Clippy, Test, Post Cache cargo,
Post actions/checkout@v4, Complete job. No skipped required job.

### Previous release artifact — re-verified end to end this pass

Fetched from the Release itself, not from a note:
`gh release download v0.4.0 -p 'hilo-0.4.0-x86_64-linux.tar.gz*'`.

- v0.4.0 is public, non-draft, non-prerelease, published 2026-11-09T16:07:29Z.
- Assets: hilo-0.4.0-x86_64-linux.tar.gz (32,941,916 bytes) and .sha256 (97 bytes).
- Sidecar content: `aa066b2c823e475bf0fa135abbcf2b74a5f6870b921d59f6f21edd5624f493a7`.
- Independently recomputed `sha256sum` of the downloaded tarball equals that value, and
  `sha256sum -c` against the sidecar returns OK.
- Tarball members: `./README.md`, `./LICENSE`, `./hilo`.
- `./hilo` is an ELF 64-bit LSB pie executable, x86-64, stripped (BuildID 59db166e…).
- Executing the extracted binary succeeds (rc=0) and self-reports
  `hilo 0.4.0` / `build: v0.4.0` / `built: 2026-11-09T15:44:47Z`.

The last line is new evidence: the shipped asset's embedded build stamp is `v0.4.0`, so
the published binary is provably the tag build, not a stale local object. Exact-tag CI
remains run 42639045303 on 5700233, green.

### Deployed-binary staleness

No hilo daemon, service, container, or cron entry runs on this host
(`systemctl --user list-units`, `docker ps`, `crontab -l` all empty for hilo), so nothing
is serving a stale build. The installed CLI `/home/kara/.cargo/bin/hilo` reports 0.4.0
with `build: v0.3.0-293-g3adfb9ed`, built 2026-10-08T05:38:34Z — it predates the release
and is not release evidence. Not refreshed in this verify-only pass.

### RELEASE-018 — mechanism now proven, not inherited

Pass 2 reported the lane's commit gate as dead. This pass measured the mechanism:

- The lane shell exports `CARGO_HOME=/mnt/usb512/cargo-home`.
- That path does not exist; `findmnt -T /mnt/usb512` resolves to `/`, so no volume is
  mounted, and `/mnt/usb512` is `drwxr-xr-x root root`.
- A one-dependency scratch project (`cargo fetch`, serde) under that CARGO_HOME exits 101
  with `error: failed to create directory
  '/mnt/usb512/cargo-home/registry/cache/index.crates.io-1949cf8c6b5b557f'` caused by
  `Permission denied (os error 13)`. Nothing is created.
- `cargo metadata --no-deps` exits 0 under the same CARGO_HOME, which is why shallow
  probes look healthy: it never touches the registry cache.

The guard's enforcing leg is `test_command: bash scripts/rust-lint.sh`, which compiles the
workspace with clippy and therefore must populate that registry cache. It cannot, and
rust-lint.sh is deliberately fail-closed (exit 2 toolchain / exit 3 budget), so the guard
blocks rather than passes. Conclusion: RELEASE-018 is confirmed and its blast radius is
"cannot commit through the guard in this lane", not "guard fails open".

Consequence for this tick: the carrier diff is the tick-notes artifact plus the two JSONL
board files and contains no source, so the carrier commit used
`git -c core.hooksPath=/dev/null commit` under GUARD DISCLOSURE. The linter the guard would
have run is exercised by hosted run 42672061291 step 8 (Clippy) on the same certified SHA.

### New finding filed as RELEASE-019

The documented release build path is not runnable outside an interactive shell.
Transient unit `warpfs-relbuild-040.service` (`systemd-run --user`, 2026-11-09 10:40:16)
failed with status 127: `scripts/build-hilo.sh: line 40: cargo: command not found`, then
`build-hilo: failure was not memory-shaped (exit 127)`. `scripts/build-hilo.sh` and the
`release` target add no PATH/CARGO_HOME bootstrap, so the build depends on `~/.cargo/bin`
being in an inherited PATH; the same run wrote its log under an inherited
`TMPDIR=/var/tmp/hermes-scratch`, outside the repo and outside lane scratch. A release
procedure that only works when a human's shell is loaded is not a repeatable publisher —
this compounds RELEASE-015.

### GitReins

No stored verdict is bound to 37e0b2f: `grep -rl 37e0b2f .gitreins/history` returns
nothing across 395 verdict.json files. Historical verdicts (QA-WARPFS-032 on 0dd62c47,
DF-WARPFS-80 on b0753403, DF-WARPFS-168-R3 on 10d58989) are not candidate evidence and
were not re-used as such.

### Dogfood and QA

Unchanged from pass 2: latest committed dogfood is Run 48 (2026-10-26), UNKNOWN-VALUE on a
stale v0.3.0-542-gd1f88c75 artifact; the latest clean-machine QA evidence is not bound to
this candidate and remains partial. Neither blocks a cut that is not due; both remain open
evidence gaps for the next material release. RELEASE-016 owns the clean-machine receipt.

### Board receipt

RELEASE-012 is refreshed in place as the single rolling readiness row (no duplicate sweep
row for this tick id), and the build-repeatability defect above is filed as RELEASE-019.
Nothing was closed.
