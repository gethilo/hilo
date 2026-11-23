# Evidence index — dogfood 2026-11-23

Binary v0.4.1-125-g675e6ed-dirty; repo tip ca3043a. The binary is not proven to match the tip. See docs/dogfood/2026-11-23-run51-workspace-mount.md for interpretation.

use-arms.py, use-arms.log, use-arms.err and use-arms.json contain the timed command arms. walkbook-run.sh and tip-run.log contain an inherited probe with exit-status-through-pipe and PID-attribution defects: do not treat its piped exit statuses or daemon-kill assertions as proof. t2t3-rerun.log captures a single mount and unsuccessful workspace manifest attempts. ws6.yaml and ws7.yaml are workspace manifest examples.

IMPORTANT CORRECTION: the first workspace probe set xattrs on the source clone, not the separate backing clone that Hilo mounted. Its ENODATA does not establish a read failure. The separately run backing-path control set an xattr on the actual clone and successfully read it through the workspace mount, but received EOPNOTSUPP writing through the mount. Its terminal output was not captured in this directory; rerun it using /home/kara/.hermes/cache/scratch/warpfs-ws-control.sh while scratch survives, or reconstruct it from the report. No current-tip conclusion follows from this older binary.

bunker-ledger-collect.jsonl is the 8-line partial ledger for agent 7b01b53f on bunker-las-03. Collection reported 6 synced rows and RUNNING; no completed install verdict is present. Collection handle: /tmp/bunker-qa-evidence-20261123T185424Z-901520.jsonl.

bunker-ledger-collect-d4.jsonl is delivery 4's re-collect at 14:46 -05 against the same handle: 18 rows (launch OK, target-binding OK, toolchain-bootstrap OK x10, fresh-install OK, tests OK, compose_config OK, docker_build N/A, lint FAIL owned by QA-WARPFS-035, ci-pass OK). Delta vs delivery 3's collect: one new cell, ci-pass OK at 19:45:06Z (act 1 job green). Battery still RUNNING, no qa-run.finished; agent left intact for the collector.
