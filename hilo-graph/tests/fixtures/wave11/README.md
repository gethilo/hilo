# Wave 11 search regression corpus (GAP-116)

Wave 11 ran Hilo's `graph search` / `graph understand` workflow on six
pinned repositories and measured natural-language (intent) MRR **0.025** —
the behaviour-owning file appeared in only 2/6 outputs — against a strong
exact-symbol baseline of **0.457** on twelve exact cases. GAP-116 closes that
intent-to-owner retrieval gap.

This directory pins the case set and a corpus for it.

## `cases.jsonl`

The eighteen scorable retrieval cases, transcribed verbatim from the Wave 11
case file (`wave11-full-candidates.jsonl`, core-case SHA-256
`35fa4778c3cac2f7b13afe71f30ee31d618850bc6af0742be562730ea5d83e22`), with an
added `owner_path` = the first (primary defining-file) truth path:

* 6 `natural-search` cases — the intent prompt is the query.
* 12 `exact-search` cases — the first truth symbol is the query (how Wave 11
  invoked `graph search` for exact cases).

Each row carries `repo`, `repo_sha` (the pinned 40-hex source revision),
`prompt`, `owner_path`, `truth_paths` and `truth_symbols`.

## Corpus (`<repo>/...`)

The corpus holds the real upstream files at the pinned SHAs, laid out at their
repository-relative paths, so `graph search` ranks genuine source (paths,
defined symbols, comments/docstrings) rather than synthetic stubs:

* the **owner** file and the other truth paths (tests / fixtures / build
  files), and
* **competing files drawn from the owner's own directory** — the "generic or
  incidental matches" the Wave 11 report names, whose paths and prose carry
  the prompt's vocabulary.

`PROVENANCE.json` records, per file, the repository, the pinned SHA, the byte
count and the SHA-256 of the content. The corpus is a *reduced subset* of each
repository (six to fifteen files), not a full checkout; absolute ranks are
therefore not the Wave 11 full-corpus numbers. It is a deterministic
regression floor: the harness fails if a ranking change drops a case out of
the top 5, pushes intent MRR below 0.20, or regresses exact-symbol MRR below
the 0.457 baseline.

## Running

```bash
# The pinned harness (deterministic; CI-safe)
cargo test -p hilo_graph --test wave11_search_test -- --nocapture

# Same, with the top-8 ranked paths printed per case (evidence for the
# incidental matches that outranked an owner)
HILO_WAVE11_DUMP=1 cargo test -p hilo_graph --test wave11_search_test -- --nocapture
```

The harness prints one line per case — `case=… repo_sha=… query=… owner=…
rank=N` (or `rank=MISS` when the owner is absent) — then `summary n=… top5=…
misses=… mrr=…`. A miss is never rendered as a low-ranked match.

## Re-measuring on the full repositories

To reproduce the full-corpus numbers, clone each repository at its
`repo_sha`, run the shipped binary's documented workflow
(`hilo graph warm`, then `hilo graph search --limit 20 <query>`), and rank the
owner path in the output exactly as Wave 11 did. That re-run is filed as
GAP-118 (re-evaluate Wave 11 after GAP-098/101/116/117); this checked-in
corpus exists so the ranking contract is guarded on every commit without
requiring multi-gigabyte clones.
