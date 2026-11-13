# RELEASE-021 verification evidence

Tick: warpfs-foreman-2026-11-13-02-41-24
Repository: gethilo/hilo
Published ref: origin/master at 2f0d3b18f0ee1db695206d9c9d9fde86617457de7

## Tag and release

The published release state was re-read from the forge.

- Annotated tag `v0.4.1` exists and peels to `4216c7f456a73b4a64b614d8f1f6addf88f65d6f`.
- Release URL: https://github.com/gethilo/hilo/releases/tag/v0.4.1
- Release is published, not draft, and not prerelease.
- Uploaded assets: `hilo-0.4.1-x86_64-linux.tar.gz` (32735042 bytes) and `hilo-0.4.1-x86_64-linux.tar.gz.sha256` (178 bytes).

## Asset verification

The two assets were downloaded into a fresh tick-scoped directory. `sha256sum -c hilo-0.4.1-x86_64-linux.tar.gz.sha256` returned `OK`.

Independent basename-bound comparison: expected and actual SHA-256 both equal `a96838c48a2dd772ab11eea1122589aa131d7d8679608d63ba26a1eeb531718c`.

Archive members: README.md, LICENSE, hilo.

## Tag CI

Run `43281097211` is bound to `4216c7f456a73b4a64b614d8f1f6addf88f65d6f`, completed successfully, and its `Build & Test (stable)` job has no failed steps.

## Trace

ch:trace row=RELEASE-021 spec=none:release-row-acceptance wave=none:verification-only test=none:external-release-readback doc=none:release-row-acceptance evidence=.coding-hermes/tick-notes/warpfs-foreman-2026-11-13-02-41-24-release-021.md witness=tag:v0.4.1@origin verdict=none:verification-only-no-task commit=none:verification-only memory=none:not-written
STATUS: complete
TIER: T3
EVIDENCE: .coding-hermes/tick-notes/warpfs-foreman-2026-11-13-02-41-24-release-021.md
