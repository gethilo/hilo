# Workspace Mounts and Manifest Schemas

`hilo workspace mount` mounts several repos (and registered backends) as peers in
one FUSE tree. It accepts **two manifest schemas**, and this document is the
reference for both — including which key in one schema means what in the other.

```bash
hilo workspace mount <MOUNT_POINT> [--manifest <PATH>] [--daemon]
```

| Schema | Written by | Top-level keys | How a repo is mounted |
|---|---|---|---|
| **project** | `hilo init` (`.vfs/manifest.yaml`) | `version`, `project`, `interfaces`, `repos[]`, `backends:`, `metadata`, `graph`, `permissions`, `triggers`, `rules`, `discovery`, `sandbox`, `performance` | `repos[].at` (default `/<name>`) |
| **workspace** | you, for a mount-only tree | `repos[]`, `backends[]`, `mounts[]`, `auto_dependency_order` | `repos[].at`, or an explicit `mounts:` entry |

`--manifest` defaults to `.vfs/manifest.yaml`, so the file `hilo init` writes is
the normal input: a fresh project mounts as a no-op, and adding a repo to
`repos:` is enough to make it mount.

## The project schema (`hilo init`)

This is spec §4. A repo is mounted at its own `at` path; `mounts:` is not needed
(and is not part of this schema):

```yaml
# .vfs/manifest.yaml
version: 2
project:
  name: my-project
interfaces:
  fuse:
    enabled: true
    mount_point: /mnt/vfs/project   # default mount point when none is passed
repos:
  - name: auth-service
    url: git@github.com:org/auth-service.git
    ref: main
    at: /mnt/vfs/workspace/auth-service/   # mount path (default: /auth-service)
    writable: true
    auto_pull: true                        # boolean in this schema
  - name: shared-lib
    url: git@github.com:org/shared-lib.git
    ref: v2.1.0
    at: /mnt/vfs/workspace/shared-lib/
    writable: false
```

- `repos[].at` — the path the repo appears at inside the workspace mount. The
  last component is the directory name (`…/auth-service/` → `auth-service`).
  Omitted ⇒ `/<repo name>`.
- `repos[].auto_pull` — boolean. `true` becomes an auto-pull interval of
  `3600` seconds in the workspace view; `false` means no auto-pull.
- `repos[].manifest` — per-repo overrides file. Parsed; the workspace mount does
  not read it.
- `backends:` (s3 / remote / local) — **inventory, not mount sources** (spec §4,
  §13). They are listed in the run header and mounted by
  `hilo backend mount`, not by `hilo workspace mount`.

## The workspace schema

Use this for a mount-only tree with no project identity, or when you want an
explicit `mounts:` list:

```yaml
# workspace.yaml
repos:
  - name: shared-lib
    url: git@github.com:org/shared-lib.git
    ref: main
    writable: false
    at: shared-lib            # optional: mounts the repo without a mounts: entry
    # auto_pull: 1800         # optional: interval in SECONDS (integers); `true` also works
mounts:
  - source: shared-lib        # a repos[] or backends[] name
    at: shared-lib            # path inside the mount
backends:
  - name: models
    type: s3
    config:
      bucket: my-models
    mount_point: /models/
```

Rules:

- `mounts[].source` must name a declared `repos[]` or `backends[]` entry; an
  undeclared source is a validation error naming the field.
- A repo with `at:` is mounted at that path even without a `mounts:` entry. An
  explicit `mounts:` entry for the same repo wins.
- `auto_pull` accepts an interval in seconds or a boolean (`true` = 3600s), so
  spec §4 snippets work here unchanged.
- `auto_dependency_order: true` topologically sorts the repos when a dependency
  map is supplied (spec §6.2).

## Mount point resolution

1. The positional `<MOUNT_POINT>` argument, if given.
2. Otherwise the project schema's `interfaces.fuse.mount_point` (the run prints
   which one it used).
3. Otherwise the command fails, naming both ways to supply one.

## "Nothing to mount" is not an error

A manifest that declares no `repos[]` and no `backends[]` — exactly what
`hilo init` writes — mounts nothing. `hilo workspace mount` reports that and
exits 0, with the smallest addition that would make it mount something.

Declaring sources but never mounting them **is** an error: in the workspace
schema, `repos[]` alone mounts nothing, so the error names the schema and the
missing `mounts:` entry (or the missing `repos[].at`).

## Reading a rejection

A manifest that fits neither schema is rejected with a message that names both
schemas, the field serde rejected, what it expected, and a hint for the common
confusions:

```console
$ hilo workspace mount --manifest .vfs/manifest.yaml /mnt/ws
error: failed to load manifest .vfs/manifest.yaml: manifest matches neither accepted schema
  - workspace schema (top-level keys: repos[], backends[], mounts[], auto_dependency_order): unknown field `version`, expected one of `repos`, `backends`, `mounts`, `auto_dependency_order` at line 1 column 9
  - project schema (the `.vfs/manifest.yaml` written by `hilo init`: version, project, interfaces, repos[] with at/auto_pull, ...): unknown field `mounts`, expected one of `version`, `project`, `interfaces`, ...
  hint: `mounts:` belongs to the workspace schema; in the project schema a repo mounts at its own `repos[].at` path (default `/<name>`).
```

Validation failures (as opposed to parse failures) are printed with the schema
name too:

```console
manifest validation errors (workspace schema):
  mounts.bad.source: mount source 'bad' does not reference a declared repo or backend
```

## What the mount is

The workspace mount is a plain union of backing files: it does not pass through
the backing files' `user.vfs.*` extended attributes and provides no
workspace-level `.vfs/` graph. Run `hilo meta` and graph commands against the
backing repository paths (`~/.hilo/worktrees/<name>/`) when you need Hilo
metadata. See [hilo-fuse](hilo-fuse.md) and the CLI reference
[`hilo workspace`](cli-reference.md#hilo-workspace).
