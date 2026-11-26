---
name: dogfood-hilo
description: Use when verifying Hilo's documented graph workflow.
---

# Hilo graph workflow

Use a disposable Git repository, not an existing project, when exploring commands that write `.vfs`. Run `hilo init --no-hooks`, then `hilo graph warm`, `hilo graph stats`, and the documented impact query for a source file. Check the binary build identity against the target commit before treating the output as proof for that revision. `--no-hooks` avoids installing Git hooks in the consumer repository. A fresh `.vfs` is fresh data, not proof of a cold OS cache.

The 2026-11-26 19:30 UTC dogfood used an older dirty binary, so it does not establish that the target commit works. Its fresh-machine battery remained in flight; see `docs/dogfood-warpfs-2026-11-26-193027.md` for the collect handle and limits.
