# Wave 11 context-pack fixtures (GAP-101)

Six regression fixtures for `graph understand` context-pack **required-path
recall**, one per repository of the Wave 11 six-repository bake-off.

Each file is one *reduced but real* neighbourhood of a pinned repository:

* `prompt` — the exact Wave 11 context-pack prompt.
* `truth_paths` — the case's required paths, verbatim from the frozen case set.
* `repo_sha` — the clone SHA the case was validated against (the path list and
  the edges come from exactly this revision).
* `files` — repository-relative paths: the required paths, the real directories
  and packages that implement and register them, plus a bounded sample of the
  real competing paths that carry the task's words.
* `edges` — the real `imports` edges the baked Hilo graph holds among the
  selected files. Files the real edges did not reach carry an inert self-edge,
  because a graph node exists only as an edge endpoint.

## Provenance

* Path lists: `git ls-files` from the pinned clone at `repo_sha`.
* Edges: the baked `.vfs/graph/edges.jsonl` produced by `hilo init` +
  `hilo graph warm` on that clone during the Wave 11 run.
* Required paths and prompts: `cases/wave11-full-candidates.jsonl` (category
  `context-pack`), SHA-256
  `35fa4778c3cac2f7b13afe71f30ee31d618850bc6af0742be562730ea5d83e22`.

## Reduction

A full pinned clone is 9k–15k files; committing six of them would be several
megabytes of test data. Each fixture keeps the required paths, their real
package neighbourhood (parent directory, the nearest ancestor holding a module
root, and that package's sub-packages), and up to 30 real competing paths per
task token plus the top 40 literal anchor competitors. Nothing is invented but
the residual self-edges, and every path is a path of the pinned revision.

Recall measured here is therefore a *floor* for the full corpus, not a
substitute: the final cross-corpus re-measurement is GAP-118.

## Scoring

`required_path_recall = |required paths present in the pack output| / |required paths|`,
where "present" means the exact path string appears anywhere in the
`graph understand` output — the rubric the Wave 11 bake-off used.
