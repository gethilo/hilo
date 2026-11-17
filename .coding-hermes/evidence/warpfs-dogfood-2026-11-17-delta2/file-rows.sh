#!/usr/bin/env bash
# Stage the two delta2 findings as board rows (boardctl, one field per argv).
# Run from the isolated worktree rooted at origin/master.
set -u
cd /home/kara/.hermes/cache/scratch/warpfs-delta2-wt || exit 9
B=.coding-hermes/board

boardctl -C "$B" create \
  --id QA-WARPFS-043 \
  --type task \
  --status pending \
  --priority P2 \
  --complexity 3 \
  --capability-tags 'qa-harness,install-leg,bunker,build-time' \
  --evidence-run-id 'warpfs-dogfood-2026-11-17-12-14-37' \
  --evidence-fingerprint 2c09b889f27edb557420675d60a7d10e5582cb352f0dd210696e2cb2e2c8ebb9 \
  --evidence-artifact '.coding-hermes/evidence/warpfs-dogfood-2026-11-17-delta2/bunker-c3a0807b-qa-evidence.jsonl' \
  --evidence-artifact '.coding-hermes/evidence/warpfs-dogfood-2026-11-17-delta2/inst-log-tail.txt' \
  --title '[QA][P2] fresh-install cell SKIPs at its 3600 s budget: a cold cargo build --workspace on the 6-CPU agent was still compiling hilo_core / hilo_backends / hilo_fuse at 56 m 36 s, so the install leg never grades' \
  --reasoning 'Second delivery of tick warpfs-dogfood-2026-11-17-12-14-37 settled the install leg that the first delivery left RUNNING. Agent c3a0807b on bunker-mvp (6 CPU, launched 2026-11-17T12:46:21Z, TTL 4h) ran the battery into the fresh-install cell, which ended SKIP with the cell own text: install exceeded 3600s timeout (slice/cpu too small for this workspace or genuine hang) - grade UNVERIFIED upstream. Its inst.log (qa-logs-99209/inst.log, 1056 lines) shows bundled DuckDB compiled and the run killed while hilo-cli, hilo_plugins, git2, hilo_core, hilo_backends and hilo_fuse were still compiling; ps showed cargo at 56:36 with three hilo crates in flight one minute before the kill. The same workspace on a 16-core host (local HEAD release build in this delivery) had not finished at 50 m 15 s. The default budget is therefore sized under the real cold-build cost and the leg can only SKIP, which the ledger records as UNVERIFIED rather than as a measured build time. The next cell then failed as go vet ./... on a repo with no go.mod - that is the missing rust branch of the lint ladder, already owned by QA-WARPFS-035.' \
  --acceptance-criteria 'The fresh-install leg must produce a NUMBER and a verdict instead of a SKIP.
(1) BUNKER_QA_INSTALL_TIMEOUT_S default is derived from a measured cold build of THIS workspace, or the agent image ships a warm cargo registry plus ccache and a prebuilt libduckdb object cache, so a 6-CPU agent either finishes or fails inside the budget.
(2) The fresh-install evidence row carries elapsed_seconds and the last crate still compiling whenever the cell ends SKIP or on timeout, so the ledger states the cost it refused to grade.
(3) A warpfs battery on a 6-CPU agent reaches a real PASS or FAIL in the fresh-install cell, citing the binary path it built and its md5.
Measurements to beat: 56 m 36 s unfinished on 6 CPUs (agent c3a0807b); 50 m 15 s unfinished on 16 cores (local HEAD release build, same tick). Related owners: QA-WARPFS-040 (tests cell has no timeout wrapper), QA-WARPFS-035 (lint cell has no rust branch), QA-WARPFS-021 (2-core slice starvation).'
rc1=$?

boardctl -C "$B" create \
  --id DF-WARPFS-175 \
  --type task \
  --status pending \
  --priority P3 \
  --complexity 2 \
  --capability-tags 'docs,installability,build-time,onboarding' \
  --evidence-run-id 'warpfs-dogfood-2026-11-17-12-14-37' \
  --evidence-fingerprint 7cbf5aa6bb16b758bf49d27ee0154ffe7a8457342ab3b7b43dffd0e67fb03894 \
  --evidence-artifact '.coding-hermes/evidence/warpfs-dogfood-2026-11-17-delta2/local-head-build-park-sample.txt' \
  --title '[P3] README quickstart sets no cold-build expectation: a from-scratch cargo build --release did not finish in 50 m on 16 cores / 60 m on a 6-CPU agent (bundled DuckDB C++), so a new user cannot tell building from hung and has no published binary to fall back on' \
  --reasoning 'Measured twice in tick warpfs-dogfood-2026-11-17-12-14-37. On this 16-core, 59 GB host the isolated release build of origin/master (838b487 / product tip ee76982) was still compiling the bundled DuckDB C++ translation units at 50 m 15 s of cargo wall time (34 live compile processes, 1322 CPU ticks in a 20 s sample, so it was progressing - slow, not parked). On the 6-CPU bunker agent c3a0807b the same build was killed by the battery 3600 s budget while three hilo crates were still in flight (QA-WARPFS-043). README quickstart (line 13-19 region) lists the apt dependencies and says cargo build --release, with no expected duration, so a first-time user sees a command that appears to do nothing for the best part of an hour, and the only certified artifact is the 2026-11-12 v0.4.1 binary at 4216c7f, three product commits behind origin/master. RELEASE-015 (pending) owns publishing a built binary on a v* tag; this row is the user-facing half: say how long a source build takes and point at the released binary.' \
  --acceptance-criteria 'A first-time user can tell whether the build is working, and can avoid it if they only want to run hilo.
(1) README quickstart states the measured cold-build cost of cargo build --release on this workspace (a range with the hardware named) and says what a healthy build prints during the DuckDB phase.
(2) README points at the installed binary / released artifact path (coordinating with RELEASE-015) so a user who does not need to build is not forced through a 50-60 minute compile.
(3) The stated numbers are reproducible: the doc names the command and the sample source, and the evidence file .coding-hermes/evidence/warpfs-dogfood-2026-11-17-delta2/local-head-build-park-sample.txt shows the 20 s CPU-tick sample that distinguishes slow from parked.'
rc2=$?

echo "rc_create_qa=$rc1 rc_create_df=$rc2"
