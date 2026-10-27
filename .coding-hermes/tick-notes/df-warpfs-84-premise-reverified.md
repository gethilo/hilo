## TASK: DF-WARPFS-84 — file→pkg id resolution missing on PHP and Kotlin (premise RE-VERIFIED live at HEAD)

Tick: `warpfs-foreman-2026-10-27-21-44-08`. This note records a pick-time premise re-probe only.
The row stays **pending** — no implementation was attempted this tick.

### Why this note exists
The row carried `pm_note: "Premise unverified since the filing run — re-verify at HEAD before fixing"`.
Two independent dispatches of adjacent rows in this family produced zero-write workers, so the next
dispatcher needs a transcript proving the defect is still live at HEAD, not a re-run.

### Probe (HEAD `c77d5daf`, binary `target/debug/hilo` = `v0.3.0-647-g1bdb99e1`, 2026-10-27T08:21:01Z)
Script `/tmp/dfw84-probe.sh`, raw output `/tmp/dfw84-probe.out`,
scratch `/var/tmp/dfw84-1793138738` (two fresh `hilo init` + `graph warm` trees, one PHP one Kotlin).

**PHP** — `src/legacy/Base.php` is `namespace App\Legacy; class Base {}`;
`src/legacy/Child.php` has `use App\Legacy\Base; class Child extends Base {}`;
`src/Service.php` has `use App\Legacy\Base;` and a `Base $b` parameter.

```
file-form:  src/legacy/Base.php  (subject, scope=self)
            note: no dependents, dependencies, tests or build targets were found ... only review row
pkg-form:   src/legacy/Child.php  ← imports (depth 1, scope=crate) [ast_exact conf=1.00]
            src/Service.php       ← imports (depth 1, scope=crate) [ast_exact conf=1.00]
```

**Kotlin** — `src/Table.kt` is `package org.example.core; class Table {}`;
`src/User.kt` has `import org.example.core.Table`.

```
file-form:  src/Table.kt  (subject, scope=self)
            note: no dependents, dependencies, tests or build targets were found ... only review row
pkg-form:   src/User.kt  ← imports (depth 1, scope=crate) [ast_exact conf=1.00]
```

Both languages reproduce the defect exactly as filed: the FILE form is **silently empty** (rc=0,
"no dependents") while the PKG form resolves the real dependents. `conf=1.00` on the pkg form while
the file form answers nothing is the overstatement-of-precision shape the finding describes.

### Where the code stands (starting points for the fix)
- `hilo-graph/src/resolution.rs:239` already routes `.php`/`.phtml` to `php_pkg_node()` →
  `php_class_for_file()` → `parser::php_declared_class()` (so a PHP file→class mapper EXISTS,
  added by the DF-WARPFS-82 work at `87507216`); the file-form impact path therefore fails
  somewhere after that resolver, not because the mapper is missing. Root-cause that path before
  writing new resolution code — do NOT assume "PHP has no mapper".
- Kotlin has NO resolver branch at all: `resolution.rs` handles go / python / java / php / ruby.
  `Language::Kotlin` appears in `parser.rs`, `classify.rs`, `audit.rs`, `wiring.rs`,
  `conformance.rs`, `signal.rs` — but not in `resolution.rs`.
- The row's fix direction (one general file→pkg step keyed by language namespace/package rules,
  not per-language whack-a-mole) is the right frame; the pkg-form output above is the oracle.

### Acceptance as filed
`hilo graph impact <file>` on a warmed PHP fixture must print the same dependent-id set as the pkg
form, and the same equivalence must hold for Kotlin; Ruby (DF-WARPFS-78) green in the same run.
Cheapest verifiable form for a worker: a committed integration test that warms a scratch PHP +
Kotlin corpus and asserts `file-form set == pkg-form set` (non-empty), plus a red arm proving the
file form was empty before the fix.

### Dispatch hazards measured this tick (do not repeat blind)
- The cargo-installed `~/.cargo/bin/hilo` is STALE (v0.3.0-293, 2026-10-08) — never verify with it.
- `cargo build/test` in this repo currently recompiles `libduckdb-sys` from source into the shared
  `/var/tmp/hermes-cargo-target` (cold, contended); a scoped `cargo build -p hilo-cli` did not finish
  in 420s and a scoped `cargo test -p hilo-cli --test cli` did not finish in 180s. Prefer the
  AGENTS.md rule: heavy build/test on bunker-las-03, not this box.
- Sibling agents run cargo/gitreins on this shared host simultaneously (observed live), so gate
  timings are not attributable to the change.

Addresses DF-WARPFS-84
