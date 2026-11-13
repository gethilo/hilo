# WarpFS foreman verification artifact

Tick: warpfs-foreman-2026-11-13-16-29-30
Row: INT-CI-017

The row's premise is disproven at the published tree. Hosted exact-SHA run 43281097211 tested commit 4216c7f456a73b4a64b614d8f1f6addf88f65d6f, which is an ancestor of the current published HEAD 9794ba581bbdc05c4d91a79df0f854a39127b9e3. GitHub reports the run completed successfully. Its Build & Test (stable) job completed every named gate: Install system dependencies, Check formatting, Build, Clippy, Test, Install cargo-audit, and Advisory gate (runtime-reachable RustSec); no gate was skipped.

The published CI workflow contains the resilient apt path: three retries of the normal apt operation followed by an explicit archive.ubuntu.com/security.ubuntu.com source fallback. It does not use continue-on-error. The row's acceptance is therefore satisfied by existing published CI evidence; no worker or implementation change is needed, and the row remains pending because this verification tick does not mint a new commit-bound GitReins verdict.

Direct evidence:
- git merge-base --is-ancestor bc38f28 4216c7f456a73b4a64b614d8f1f6addf88f65d6: PASS
- git show 4216c7f...:.github/workflows/ci.yml: fallback mirror and all named gates present; no continue-on-error
- gh run view 43281097211 --repo gethilo/hilo: completed/success at tested SHA; all named steps completed/success
