# Flow symbol recall regression corpus (GAP-098)

`graph understand` ranked a file's definitions by source order and printed the
first 8. A question that named a *flow* ("request flow WSGI->view->response")
therefore lost the flow's own definitions whenever they sat past the 8th in a
large file, and the cookbook numbers were unambiguous:

| case | question | definitions the flow needs | measured before | measured now |
|---|---|---|---|---|
| `flask.A2` | `request flow WSGI->view->response` | `wsgi_app` (app.py 41/42), `full_dispatch_request`, `dispatch_request`, `finalize_request` (25–27/42) | **1/4** | **4/4** |
| `gin.A2` | `request dispatch flow ServeHTTP->tree->handler` | `ServeHTTP` (gin.go 43/50), `handleHTTPRequest` (45/50), `getValue` (tree.go 20/23), `Next` (context.go 9/151) | **1/4** | **4/4** |

This directory pins the case set and a corpus for it, so the recall contract is
guarded on every commit without a network fetch or a multi-hundred-file
checkout. The harness is `hilo-graph/tests/flow_symbol_recall_test.rs`.

## Corpus (`<repo>/...`)

The corpus holds the real upstream files at the pinned SHAs, laid out at their
repository-relative paths, so the test ranks and parses genuine source:

* the file each flow symbol is defined in (`src/flask/app.py`, `gin.go`,
  `context.go`, `tree.go`), and
* the neighbouring files that anchor and traverse the flow into the pack
  (`src/flask/views.py` for the `view` token of the flask question,
  `src/flask/__init__.py` for the `src/flask/` module façade).

`PROVENANCE.json` records, per file, the repository, the pinned SHA, the byte
count and the SHA-256 of the content; `fixture_provenance_matches_pinned_revisions`
re-verifies those hashes on every run, so a drifted fixture fails loudly instead
of quietly changing the recall floor.

The corpus is a *reduced subset* of each repository (three files each), not a
full checkout. It is a deterministic regression floor for the symbol-allowance
rule, not a reproduction of the full-corpus pack contents.

## Running

```bash
cargo test -p hilo_graph --test flow_symbol_recall_test -- --nocapture
```

The harness prints one line per graded symbol (`<repo> <symbol> in <file>: true`)
and asserts the acceptance floor of **>= 3 of 4** symbols per repository — the
shipped behaviour scores 4/4 on both. A flow file that never enters the pack is
reported as a miss naming the files that did, never as a low-ranked match.

## Re-measuring on the full repositories

To reproduce the full-corpus numbers, clone each repository at its `repo_sha`
and run the shipped binary's documented workflow:

```bash
git clone https://github.com/pallets/flask && cd flask
git checkout d73fa1cdcbd8b1465c151db8924ba58b1dd14e35
hilo init && hilo graph warm
hilo graph understand "request flow WSGI->view->response" \
  | grep -E "wsgi_app|full_dispatch_request|dispatch_request|finalize_request"

git clone https://github.com/gin-gonic/gin && cd gin
git checkout dcaa4296d111981ffb31ac3eba90bb63e1eb5ab9
hilo init && hilo graph warm
hilo graph understand "request dispatch flow ServeHTTP->tree->handler" \
  | grep -E "ServeHTTP|handleHTTPRequest|getValue|Next"
```

Both questions are the bake-off wave-1/wave-10 architecture cases (`flask.A2`,
`gin.A2`); the wave-10 scorecard records `hilo 4/4` on both.
