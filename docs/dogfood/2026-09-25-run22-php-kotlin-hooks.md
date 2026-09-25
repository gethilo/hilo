# Run 22 — 2026-09-25 (boardctl-dogfood) — PHP + Kotlin, the 6th/7th languages + the hook lifecycle

**Angle (stale-surface rule).** 21 runs never executed a PHP or Kotlin
corpus — the README's "26 languages" claim had only been exercised on Rust
(×2), Go, Python, TS/JS, Java, C, Ruby (8 languages). No run had driven the
git-hook lifecycle across two machines (run 11 proved post-commit on one
machine; post-merge never fired in any run).

**HEAD tested:** 281b6ff (local release `hilo 0.3.1-dev`, build stamp
`v0.3.0-108-g281b6ff`). Corpora: composer/composer (589 .php parsed, 3983
edges, warm 10.9s cold) and JetBrains/Exposed (866 files, 16657 edges, warm
83.9s cold).

## Working example: what a PHP/Kotlin developer or agent gets today

```bash
hilo init && hilo graph warm          # composer: 10.9s; Exposed: 83.9s
hilo graph impact 'pkg:Composer\DependencyResolver\Solver' --max-depth 1
  # → 2 dependents, == grep (exact). Cross-namespace use-imports are reliable.
hilo graph impact 'pkg:org.jetbrains.exposed.v1.core.Table' --max-depth 1
  # → 173/176 vs grep import-truth (98.3%).
hilo graph related src/Composer/Installer.php   # outgoing deps, [external package] labels
hilo meta <file> --set user.vfs.dogfood --value run22-php   # xattr round-trips via getfattr
hilo classify                          # composer: 279 test / 2 entrypoint / 20 unknown
# MCP: vfs_graph_impact over stdio JSON-RPC == CLI (4/4 with probe files)
```

What to AVOID until rows land:

- **File-form impact on PHP/Kotlin/Ruby** — "No dependents found" is a
  silent-empty answer (DF-WARPFS-84); translate to the pkg: form yourself.
- **Trusting stats Orphans on PHP** — composer's BaseCommand (31 same-
  namespace subclasses) is reported as an orphan (DF-WARPFS-82).
- **Relying on a pull to refresh a second machine's graph** — nothing
  writes `.vfs/.dirty`; post-merge never fires (DF-WARPFS-83). Run
  `hilo graph warm` manually after pulling.
- **Same-namespace reuse is invisible on PHP entirely** (extends,
  implements, new, class-constants without a use statement).

## The full hook lifecycle, measured (the part one machine cannot see)

| step | machine A (composer) | machine B (composer2) |
|---|---|---|
| fresh clone + init + warm | 3983 edges | 3983 edges, impact(Solver)=3 — identical |
| commit DogfoodProbe.php (imports Solver) | hook fires `warm --changed`, edges 3983→3984, impact=3 — WORKS | — |
| commit DogfoodProbe2.php | edges →3985, impact=4 — WORKS | — |
| `git pull` on B | — | file lands on disk, **graph stale: impact=3, files=456, no .dirty, exit 0 — FAILS (DF-83)** |

Post-commit is the best-proven path in the product. Post-merge is dead
code: a consumer for a marker with no writer (GAP-087's fix removed the
writer; the consumer and the README promise stayed).

## Self-heal re-verified at 5x prior scale

Deleted graph.db on the 866-file Exposed graph → next `stats` silently
replayed edges.jsonl into a fresh db in 28.5s (RC=0, 866 files correct).
The DF-61/64 fix holds on real corpora, not just fixtures.

## Errors hit (and their meaning)

- `hilo graph related <path> --max-depth 2` → "unexpected argument":
  related takes NO depth flag (unlike impact) — friction, not filed (help
  text is accurate).
- `hilo meta <path> --set user.vfs.dogfood run22-php` → "unexpected
  argument 'run22-php'": --set and --value are separate flags; one
  plausible spelling fails. Usability polish, not filed (docs show the
  two-flag form).
- `jq 'select(.source|test(...))'` on edges.jsonl → 16k null errors: the
  edge schema is {from,to,rel,provenance,confidence} — agent tooling
  guessing the field name gets a wall of errors instead of a schema
  pointer. Minor; noted here only.

## Verdict inputs (details in the log and rows DF-WARPFS-82..87)

pkg-form blast radius on both new languages is real, fast, and
grep-exact-to-near-exact; the post-commit hook works; self-heal works at
scale. Against that: a third of the graph's value (same-namespace reuse on
PHP), the pull-refresh promise, and file-form queries are silent lies.
