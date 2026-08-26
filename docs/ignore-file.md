# The Upstream Ignore File (`.hiloignore`)

Hilo's remote backends (S3 today; Google Drive, OneDrive, Dropbox in the
future) share one git-ignore-style pattern format to keep files **local-only**.
Build artifacts, binaries, caches, secrets, and anything else you never want
uploaded or pulled down live in the ignore file and are **never transferred
in either direction** — not up, not down.

This is the "upstream ignore" concept: the same file describes what stays
local for every remote backend, so switching backends does not require
re-encoding your ignore rules.

## Location

- Default: `.hiloignore` in the workspace root (next to `.vfs/`).
- `hilo workspace sync --ignore <PATH>` overrides the default.
- The ignore file itself is always local-only — it is never uploaded or
  downloaded (same for `.hiloephemeral`).

## Format

The format is a practical subset of [gitignore](https://git-scm.com/docs/gitignore):

| Rule | Meaning |
|------|---------|
| `# comment` / blank lines | skipped |
| `*.log` | matches the basename at any depth |
| `target/` | directory-only pattern — the dir and everything below it |
| `/build` | anchored to the workspace root (leading `/`) |
| `docs/private` | a slash in the middle anchors the pattern to the root |
| `**/cache` | `**` crosses directory boundaries |
| `!keep.log` | re-include (last matching pattern wins) |

Notes:

- A pattern that matches a **directory** excludes its whole subtree — and
  per the gitignore rule, a file inside an excluded directory **cannot** be
  re-included by a later `!` pattern.
- `.vfs/` itself is always excluded by the engine and never transferred,
  regardless of the file contents.

### Example

```gitignore
# build output — never leave the machine
target/
build/
*.o
*.so
*.bin

# caches and local state
.cache/
node_modules/

# environment-specific
.env.local
secrets/

# ...but keep this one even though it matches *.bin
!important.bin
```

## Semantics

| Case | Behavior |
|------|----------|
| local-only, not ignored | uploaded |
| remote-only, not ignored | downloaded |
| both sides | the newer side wins (local mtime vs S3 LastModified); equal → unchanged |
| ignored (either side) | never transferred |
| `.vfs/` (either side) | never transferred |
| `.hiloignore` / `.hiloephemeral` | never transferred |
| deletions | not propagated (a file removed locally stays on the remote) |

## Usage

```bash
# Two-way sync a workspace directory against an S3 prefix
hilo workspace sync --bucket my-bucket --prefix my-project --at ./workspace

# Preview what would transfer without touching anything
hilo workspace sync --bucket my-bucket --prefix my-project --at ./workspace --dry-run

# Use a custom ignore file
hilo workspace sync --bucket my-bucket --at ./workspace --ignore ./custom-ignore
```

Credentials come from the standard AWS chain — including
`AWS_ENDPOINT_URL` + `AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY` for
S3-compatible endpoints (MinIO, Hetzner Object Storage, etc.).

> Design note: the full backend-backed workspace architecture
> (adapter/orchestrator over existing sync tools, stream/mirror modes,
> ephemeral classification, feature & compatibility matrix) is specified in
> `specs/backend-backed-workspace-spec.md`. This native S3 two-way sync
> engine is the `hilo-backends` fallback that spec builds on.
