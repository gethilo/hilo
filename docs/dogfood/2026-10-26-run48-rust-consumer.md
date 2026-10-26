# Dogfood Run 48 — 2026-10-26 — Rust consumer + MCP surface

Tick: `warpfs-dogfood-2026-10-26-16-45-28`
Target HEAD: `763f94bb16f29ecb579fa5f8f4188c3bc6717bb7`
Binary used: `/home/kara/warpfs/target/release/hilo`, stamp `v0.3.0-542-gd1f88c75-dirty`, built 2026-10-21. The binary predates target HEAD; the attempted HEAD debug build was stopped after 164 seconds at dependency compilation, so observations are runtime evidence from the available release artifact and are not claimed as HEAD-grading.

## Promise

Hilo claims to give an AI coding agent a pre-built dependency map of a codebase — dependencies, entrypoints, test coverage and blast radius — through CLI, MCP or FUSE, without burning context on file reads.

## Real consumer

Created a fresh four-file Rust crate outside the product repository (`/tmp/df-warpfs-20261026-1645-release/corpus`) with `src/lib.rs`, `src/parser.rs`, `src/main.rs`, and `tests/basic.rs`. Ran the documented consumer sequence with isolated HOME and `GIT_CONFIG_GLOBAL=/dev/null`: `hilo init --no-hooks`, `hilo meta`, `hilo graph warm`, `stats`, `impact`, `related`, `search`, `understand`, and a real NDJSON MCP session (`initialize`, `tools/list`, `vfs_graph_stats`, `vfs_graph_impact`).

Observed: init completed in 0.01s; metadata set/read round-tripped (`role: library`); warm completed in 0.30s but discovered 0 edges across 0 files; stats then said `Graph cache is empty. No edges parsed yet.`; file impact returned a success-shaped self-only review row; search found `src/parser.rs` lexically; understand returned `No files matched task`; MCP returned valid JSON-RPC and advertised 18 tools, with stats reporting zero files/edges and impact returning a self-only review set. The fixture was valid Rust, so this is product/runtime evidence, not a malformed-input failure.

The zero-edge result is consistent with the available artifact's behavior, but because the artifact predates target HEAD, this run does not mint a new product defect for it. Re-prove the same Rust import graph with a binary built from current HEAD before filing.

## Performance

Measured the real user-visible commands using `/usr/bin/time -p` on the four-file release-artifact consumer: init 0.01s real, warm 0.30s, stats 0.04s, impact 0.08s, related 0.02s, search 0.02s. No command was slow enough to be user-noticeable at this corpus size, and no PERF row was filed. A current-HEAD binary was not available for an honest performance comparison.

## Install leg

The ephemeral bunker leg was explicitly attempted with `HOME=/home/kara /home/kara/.hermes/scripts/bunker-qa.sh status`; it reported `bunkerd=active docker=yes act=no uidmap=yes`. A fresh agent was not launched because the only available server, `bunker-mvp`, reported critical disk pressure: 96% used (143.7 GB/149.9 GB), with four existing agents and 20 orphan keys. This is a shared-host capacity block, not proof of installability and not a product install failure. Existing open agents were not mine and were not destroyed. The install verdict is `SKIPPED-install-bunker` for this tick; the owning open rows `QA-WARPFS-027` and `DF-WARPFS-155` already cover this install/host-capacity class, so no duplicate row was minted.

## Value verdict

UNKNOWN-VALUE for this tick's current-HEAD verdict: the available release artifact completed the consumer and MCP protocol path, but the current-HEAD build and fresh-machine install could not be completed within the available run because the build was still compiling and the bunker host was critically full. The last durable project history records a broader SHIPPABLE Rust/Go/Python verdict but roughness on other language and freshness surfaces; this tick adds no new row without current-HEAD attribution.

Evidence retained outside the repo: `/tmp/df-warpfs-20261026-1645-release/` and `/tmp/df-warpfs-20261026-1645/`; current build log `/tmp/warpfs-build-20261026-1645.log`; bunker status captured in the tick transcript.

ch:trace tick=warpfs-dogfood-2026-10-26-16-45-28 project=warpfs head=763f94bb16f29ecb579fa5f8f4188c3bc6717bb7 binary=d1f88c75:stale-release artifact=docs/dogfood/2026-10-26-run48-rust-consumer.md install=SKIPPED-install-bunker:host-capacity-96pct rows=none:existing-rows-QA-WARPFS-027-and-DF-WARPFS-155 own-class verdict=UNKNOWN-VALUE commit=pending
