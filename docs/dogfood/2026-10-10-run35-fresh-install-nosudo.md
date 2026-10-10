# Dogfood Run 35 — 2026-10-10 — Fresh-machine no-sudo install leg (bunker-las-02)

**Angle:** previous runs (1-34) exercised the CLI/graph surface from prebuilt or
locally-built binaries; the install leg had been SKIPPED 16 consecutive times
because bunker-las-03 was unreachable. This run is the first **fresh-machine
install leg** on the README's own documented path, on a sibling host
(bunker-las-02 — las-03 offline 5d, per the ephemeral-install-leg sibling rule).

## What was done (exactly the README's documented steps)

1. `curl … https://sh.rustup.rs | sh -s -- --profile minimal` — worked on a bare
   Debian 13 agent user, no root. rustc 1.99.0, cc 14.2.0, perl 5.40.1 all
   present-by-default as the README's dependency table claims. ✔
2. `git clone https://github.com/gethilo/hilo.git` (public https) — 121s. ✔
3. `cargo build --release -p hilo-cli --no-default-features --features
   vendored-openssl` — **26m32s cold** (README estimates 15-30 min: accurate).
   Binary `hilo 0.4.0 / v0.3.0-324-gbf1baca`. ✔
4. `ldd` shows **zero libfuse3 links** — README's pure-Rust-FUSE claim holds. ✔
5. Smoke (real use on a 2-file Python corpus): `init` (with git-hook warning),
   `meta --set/--get` xattrs, `graph warm` (parse cache hit on 2nd run),
   `graph stats`, `graph search`, `classify` (correct entrypoint/library roles),
   `mount` + file read + listing through the mount. ✔

## Findings (filed as board rows)

- **DF-WARPFS-115 (P1)** — `hilo mount` on a no-libfuse3 binary prints
  `Hilo mounted at <dir>` then fails `No such file or directory (os error 2)`
  when the mount point does not exist. The directory is NOT created, yet the
  success banner is printed first. With a pre-created dir the same binary
  mounts fine. The "mounted" banner must not precede a validated mount, and
  the error should say "mount point does not exist — create it first".
- **DF-WARPFS-116 (P2)** — `graph search` in the README-quality docs is
  described as "Deterministic semantic code search (TF-IDF + BM25)" but on a
  2-file corpus it matched only file/package names lexically: `add` (a real
  symbol in the corpus) returns "No results found", while `math`/`main` return
  file-level hits. Either symbol indexing needs a warm/graph prerequisite that
  the docs don't state, or the semantic claim over-promises on small corpora.

## Install-leg numbers

- rustup bootstrap: ~2 min
- clone: 121s
- cold release build (no-default-features + vendored-openssl): 26m32s
- smoke verbs: all <1s except `graph warm` first-parse (~2s on 2 files)
- destroy: bunkerd DestroyAgent timed out repeatedly (19.5 GB archive step);
  2h TTL is the backstop — see dogfood-log.

## Fresh-box gaps noted (not filed, low severity)

- `getfattr` not present on a fresh Debian 13 agent — README says `attr` is
  optional and Hilo doesn't shell out to it, which is accurate, but the mount
  xattr-inspection examples in the docs all use `getfattr`. A one-line
  "install attr to follow these examples" note would help.
- /tmp is shared between tenants on the bunker: a scratch file at /tmp/build.log
  collided with a foreign process. (Our procedure bug, not Hilo's.)
