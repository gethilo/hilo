# warpfs release readiness evidence — 2026-11-01

Target: gethilo/hilo, canonical origin/master, candidate f34229387b7505b3b8093495649f10665e53a1e6.
Baseline: v0.3.0. Range: v0.3.0..candidate, 758 commits.

## Semver census
feat 26 (minor signal); fix 125; perf 5; docs 41; chore 179; test 10; refactor 0; other 372; breaking footers/typed ! markers 0. Proposed version: v0.4.0 minor. No major proposal.

## Version and tooling
Workspace and all 11 crates report 0.4.0. CHANGELOG.md has [0.4.0] dated 2026-10-04 and [Unreleased]. Repository-owned Makefile release target runs scripts/build-hilo.sh -p hilo-cli, strip target/release/hilo, and lists the binary. No release script, GoReleaser, Dockerfile, package registry workflow, or tag/release workflow exists. CI workflows are push-to-master/main, pull-request, and Pages only; no tag trigger or artifact publication.

## Candidate gates
Clean detached candidate worktree at f34229387b7505b3b8093495649f10665e53a1e6. Candidate source tree is clean. cargo fmt --all -- --check: PASS. cargo check --workspace --locked: PASS (15.18s). Candidate-bound GitReins full guard completed PASS from the detached worktree: gitreins 0.17.1, outcome pass, changedFileCount 0, degraded false, skippedSteps [], tests full PASS; secrets scan was bounded to working-tree scope and reports no files to scan (gitleaks not run for that scope). Receipt: /home/kara/.hermes/cache/scratch/warpfs-gitreins-guard-20261101.json.

## Hosted CI
GitHub repo gethilo/hilo. Exact candidate run 41301940225: completed success, Build & Test (stable) job 124834524288, all steps success, https://github.com/gethilo/hilo/actions/runs/41301940225. GitHub API branch tip f34229387b7505b3b8093495649f10665e53a1e6 matches git ls-remote origin refs/heads/master.

## GitReins and dogfood/QA
Recent candidate-ancestry verdicts include PASS 2898c9c7 bound to 7b0de9fe, PASS 2a0898a0 bound to f795014b, PASS 551a6a97 bound to fa93c12b, and FAIL 13c1fd82 bound to 4a46668c. The candidate-bound full guard receipt is complete: outcome pass, working-tree scope, changedFileCount 0, degraded false, skippedSteps [], with working-tree secrets scanning explicitly bounded. Latest QA event 10009 (2026-11-01) is FAILED-harness-blocked: clean-machine product cells are UNVERIFIED; bunker-mvp lock blocked launch and bunker-las-03 SSH sync failed. Existing open release-relevant QA rows include QA-WARPFS-REVIEW-2 and QA-WARPFS-028. Open P1/P2 findings remain in the board.

## Artifact and deployed freshness
The candidate Makefile release path was not run during this readiness sweep because it is a side-effecting long build and release authorization is absent. Existing /home/kara/.cargo/bin/hilo reports hilo 0.4.0 but build v0.3.0-293-g3adfb9ed, built 2026-10-08; it is stale relative to candidate f3422938.

## Public posture
Repository public, MIT licensed. master branch protection API returns 404 (unprotected). GitHub security_and_analysis reports dependabot updates, secret scanning, non-provider secret scanning, push protection, and validity checks disabled. Tracked-secret scan found no live credential pattern; only redacted/fixture-like board text and test placeholders matched broad patterns.

## Decision
NO CUT. Explicit RELEASE-cut authorization is absent (RELEASE-012 remains pending OWNER-RELEASE-AUTH). The candidate is not release-tooling-complete: no published artifact workflow and prior v0.3.0 Release has zero assets. Fresh clean-machine QA is blocked/unverified, open P1/P2 findings remain, and the existing working checkout carries unrelated staged board/backend changes. Do not tag or create a GitHub Release.
