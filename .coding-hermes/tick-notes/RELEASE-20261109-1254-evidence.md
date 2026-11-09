# warpfs release readiness evidence — 2026-11-09 12:54 tick

Target: gethilo/hilo
Canonical checkout: /home/kara/warpfs
Default branch: master
Certified release candidate: 7f039cd9abe00ed20f69446ab24a2862b5104cf9
Remote parity: origin/master, git ls-remote, and GitHub API all report 7f039cd9abe00ed20f69446ab24a2862b5104cf9.
Baseline: v0.3.0 at 702c4a8cefcda240e7d555926a77bb69a66c285f.
Range: v0.3.0..7f039cd9abe00ed20f69446ab24a2862b5104cf9, 740 non-merge commits.

## Decision

NO CUT. Proposed version remains v0.4.0 minor, but the candidate is not release-ready. The exact-head hosted CI is green and the candidate-bound GitReins guard passes, but the release notes are stale after version preparation, the repository has no tag-triggered publisher or release script/docs, the previous Release has zero assets, current-head dogfood/clean-machine evidence is incomplete, and open P1 release-relevant QA findings remain. No tag, GitHub Release, or artifact publication was created.

The current sweep refreshes canonical row RELEASE-013 rather than creating a duplicate. A foreman should first land release-notes coverage and a documented, verified artifact/publish path, then rerun exact-head CI and the QA gates before a cut.

## Commits and semver

Census v0.3.0..candidate: feat 27, fix 139, perf 5, chore 212, docs 39, test 10, refactor 1, other 307; total 740. No BREAKING CHANGE footer or typed ! marker found. The material-path census (excluding board, GitReins, docs/README/CHANGELOG, and generated bookkeeping paths) is feat 27, fix 131, perf 5, other 33, total 196. Proposed bump: minor v0.4.0.

The current Cargo workspace and all 11 published workspace crates carry version 0.4.0. CHANGELOG.md has [0.4.0] dated 2026-11-08 and an empty [Unreleased], but the section predates the candidate: 12 commits landed after the prep commit a046c89f55f78245d4c71d3ecd40752d7ec4dff8, including fix(graph) 16539a5 and the gitignore change 8b1147b. The release section does not yet cover those post-prep commits. The candidate also contains later board/QA commits that must be explicitly excluded from release notes as bookkeeping.

## Release tooling and artifacts

Repository-owned tooling found first: Makefile target release, which runs scripts/build-hilo.sh -p hilo-cli, strips target/release/hilo, and lists the binary. README documents make release and states that no prebuilt binaries are published. No scripts/release*, GoReleaser configuration, Dockerfile, package registry workflow, or tag-triggered release workflow exists. The only hosted workflows are CI, Pages, and pages-build-deployment. A manual gh release create fallback is possible only after release notes are corrected and the Makefile artifact is built and verified from the immutable tag; it does not close the missing automation/documentation gap.

Previous release probe: v0.3.0 GitHub Release exists, is public/non-draft/non-prerelease, and has assets: []. Its zero-asset gap is still present. No v0.4.0 tag exists locally or remotely.

Artifact build was not run in this readiness-only pass: the repository's release build is a long, side-effecting Rust/DuckDB build and the candidate is blocked on release-prep coverage and the missing publish contract. The installed /home/kara/.cargo/bin/hilo is not a candidate artifact: it reports hilo 0.4.0 but build v0.3.0-293-g3adfb9ed, built 2026-10-08, while its file mtime is 2026-10-08 and it is an unstripped 1.4 GB binary.

## Hosted CI

Exact candidate run: 42623087215, https://github.com/gethilo/hilo/actions/runs/42623087215.
Status completed, conclusion success, head SHA 7f039cd9abe00ed20f69446ab24a2862b5104cf9. Job Build & Test (stable) completed success. Steps Set up job, checkout, Rust installation, system dependencies, cargo cache, formatting, build, clippy, test, and post steps all completed success.

Recent relevant run history: 42609109723 success at 16539a5; 42593865876 success at 8b1147b; 42578191698 success at 5f34bd9; 42477564871 success at 0dd62c4; 42466946781 success at b8a616c. Runs 42620552979 and 42619750999 were cancelled by concurrency supersession, not red. There is no tag-triggered workflow to verify after a future tag.

## GitReins

Fresh candidate detached worktree: /home/kara/.hermes/cache/scratch/warpfs-release-candidate.xD0d0W, HEAD exactly 7f039cd9abe00ed20f69446ab24a2862b5104cf9, clean, remote git@github.com:gethilo/hilo.git.

Command: gitreins guard --full --scope working-tree --json
Receipt: /home/kara/.hermes/cache/scratch/warpfs-gitreins-20261109-1254.json
Result: PASS, gitreins 0.20.0, changedFileCount 0, degraded false, skippedSteps [], checkCount 2. Tests full PASS; secrets check PASS with no files in working-tree scope, and gitleaks not run for that bounded scope. The receipt is exact-tree evidence but not a substitute for hosted release CI or artifact verification.

Repository history contains prior task-bound verdicts, but no stored GitReins verdict was found bound to the current candidate SHA. The fresh guard above is the candidate-bound verdict.

## Dogfood and QA

Latest committed clean-machine QA audit is event 10046 at tested SHA 0dd62c472feb2fa88dc7e8250367056d136b0558. It is not current-head evidence: the canonical tip is now 7f039cd9. The battery graded only 2 of 14 canonical cells; 11 were UNVERIFIED, install exceeded the 3600-second ceiling, and the run filed QA-WARPFS-035/036 plus product QA-WARPFS-037. Hosted CI was green at the older SHA, but this does not clear the current candidate.

Latest committed dogfood record is Run 48, 2026-10-26, UNKNOWN-VALUE. It used a stale v0.3.0-542-gd1f88c75 artifact, could not complete a current-head build within the run, and reported the fresh-machine host critically full. Earlier run 47b was PROMISING-BUT-ROUGH despite later HEAD re-verification. These records are historical and do not waive current-head artifact/QA evidence.

Open P1 findings relevant to release readiness include QA-WARPFS-025 (shared bunker disk exhaustion truncates the battery), QA-WARPFS-DOCS-1 (battery refused at 98% disk), and NEVER-DONE (blocked perpetual audit). Open P2 release-related rows include RELEASE-012, RELEASE-013, QA-WARPFS-QA-3 (v0.3.0 upgrade build failure on a fresh agent), and DF-WARPFS-99 (stale installed binary prevents PERF-009 verification). These are recorded findings, not silently waived.

## Public posture and deployed freshness

GitHub repo gethilo/hilo is public and MIT licensed. Branch protection API returned 404, meaning master is unprotected. GitHub security_and_analysis reports Dependabot security updates, secret scanning, non-provider secret scanning, push protection, and validity checks disabled. No deployed service health/version endpoint was identified for this CLI repository; the measurable installed binary is stale as recorded above.

## Required next gates

1. Update CHANGELOG [0.4.0] coverage for all material commits after a046c89f, reopen/retain [Unreleased] correctly, and rerun the notes census at the final cut SHA.
2. Define and document the artifact surface: make release output, version/provenance check, checksum/packaging format, install/capability smoke, and manual or automated publication path. The v0.3.0 zero-asset gap must not repeat.
3. Resolve or explicitly disposition the current-head QA/clean-machine evidence gaps, especially QA-WARPFS-025, QA-WARPFS-DOCS-1, QA-WARPFS-QA-3, and QA-WARPFS-037.
4. Re-run exact-head CI, fresh GitReins, artifact build/provenance, and dogfood/QA against the immutable cut SHA before creating any tag.

## Board receipt

Canonical row to refresh: RELEASE-013. This report is the evidence artifact for this pass. No tag, Release object, or artifact was created.
