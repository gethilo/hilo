# Dogfood Run 36 — 2026-10-10 — real consumer on a foreign repo at tip + install-leg re-proof (bunker-las-02)

**Angle:** run 35 (earlier today) took the fresh-machine install leg on bunker-las-02
but smoked only a 2-file corpus; runs 1-34 exercised the graph surface on tip but
never re-proved the flagship `graph understand` flow-recall claims (GAP-098 /
flask.A2) on a real foreign repository at HEAD. This run does both: (a) RE-PROOF
of the run-35 install claim on a second ephemeral agent on the same host, and
(b) a real-consumer journey on pallets/flask @ 2ea3e097 with the tip binary,
driving init → warm → stats → understand → search → impact → classify → meta →
MCP (17 tools) → plugin load → concurrency.

## Install leg (sibling re-proof of run 35)

- bunker-las-03 offline (5d+); bunker-las-02 answered; first spawn hit the known
  transient `slice-limits: containment landing did not converge` — retry once per
  the ephemeral-install-leg rule → agent bd0fa5e8 (TTL 2h) spawned clean.
- Bare Debian 13 agent (uid 1021, no sudo, rustc absent): rustup `--profile
  minimal` bootstrap ✔, public https clone of github.com/gethilo/hilo ✔ (44s).
- `cargo build --release -p hilo-cli` (DEFAULT features — a different feature set
  than run 35's vendored-openssl variant): **BUILD_RC=0 in 1477s cold (24m37s)**.
  Confirms the README 15-30min estimate on a second agent and second feature set.
- Smoke on a depth-1 flask clone: `hilo init` → `graph warm` (832 edges / 80
  files, 4s) → `graph stats` → `graph search wsgi_app` (top hit
  src/flask/app.py — correct) → `graph understand "request flow
  WSGI->view->response"` rc=0 with wsgi_app present → `graph impact
  src/flask/app.py` (crate-scoped importers — correct). **All rc=0.**

**Conclusion:** run 35's install-leg claim is re-proven independently (fresh
agent, different feature set, same host). The documented install path works
verbatim on a fresh machine. **No SKIPPED-install-bunker row this run.**

## Real use — consumer journey on flask @ 2ea3e097 (tip binary 0.4.0, v0.3.0-327-g1195639a)

Working (the promise holds):
- `graph warm` 4.0s cold on 83 files; `graph stats` 42ms warm (hyperfine, 10
  runs); `graph understand` 141ms ±12 warm; `--budget 120000` variant 183ms.
- GAP-098 flow recall VERIFIED on real flask (not just the fixture): the exact
  flask.A2 question returns all four criterion symbols (wsgi_app,
  full_dispatch_request, dispatch_request, finalize_request), app.py in MAP
  carrying 152 definitions in its task-word allowance block.
- `graph search wsgi_app` ranks src/flask/app.py #1 [lexical].
- Exit-code contract: `impact` missing arg = 2, unknown path = 1;
  `surfaces` on a foreign tree prints the GAP-112 scope marker and exits **1**
  (run 34's rc=0 note was a shell-pipeline artifact; re-verified rc=1).
- MCP: initialize → tools/list = 17 tools; vfs_graph_understand (harmonic)
  works over stdio; wrong-arg error names the missing field ('missing task
  argument').
- Concurrency: idle `serve --mcp` + CLI `graph stats` + a second MCP client on
  the same repo — no lock error, correct answers (the prior-run -32603 lock
  issue is NOT reproducible on 0.4.0).
- `plugin load examples/plugins/minimal.wasm` → honest `0 hooks` report,
  persisted to .vfs/plugins/. `classify` correct. `ignore check` correct.
- Fresh-clone honesty: `hilo init` appends a managed block to .gitignore
  (graph.db etc.) exactly as the getting-started rebuild table says.

Findings (filed as board rows):
- **DF-WARPFS-117 (P2)** — `graph impact <symbol>` is unreachable: a symbol id
  taken from search/understand errors with "Accepted id forms: bare path,
  sys:, pkg:". The symbol→owner-file hop has no CLI surface; file-level impact
  works. The docs teach impact on paths only.
- **DF-WARPFS-118 (P2)** — `graph understand` DETAIL tier on a foreign repo
  spends its budget on small examples/test-app files (provenance=ast_exact,
  score=1.00) while the task-owning file (src/flask/app.py, owner of the whole
  WSGI chain) is MAP-only at default/12000 budgets; it enters DETAIL only at
  --budget 120000. GAP-098 repaired symbol recall in MAP; detail-tier
  selection still underweights the implementation the question is about.

## Perf (Step 2b)

- Headline (`graph understand`, foreign repo, warm): 141ms ±12 (hyperfine 21
  runs); first call 0.18s. `graph stats` 42ms ±7. Nothing user-noticeable at
  these sizes; cold `graph warm` on the repo itself is a one-time ~35s cost
  already on record (run 34). No PERF row filed.

## Install-leg residue / infra notes (not Hilo defects)

- bunkerd destroy on 19-20GB homes: the archive step's `gzip` gets `signal:
  killed` at ~5min (pattern: 4× f1fe378c, 2× 187c4563, 2× bd0fa5e8) and the
  destroy retries via TTL reaper eventually succeed (~12-25 min per agent).
  During the first failure window bunkerd served no RPC on :10001/:10002
  despite the startup banner — both listeners appear only AFTER the
  registry reconciliation + archive backlog completes (REST/gRPC "listening"
  lines at 20:08:58, 28 min after the restart). Escalation note per
  bunker-agent-isolation; both agents (187c4563, bd0fa5e8) verified destroyed
  by 20:47 PDT, `bunker list` shows 0.

## Left behind

- docs/dogfood/2026-10-10-run36-foreign-repo-consumer.md (this file) + log entry
  + board rows DF-WARPFS-117/118. No code changes (run 35's DF-115 mount fix
  landed separately as e4655347; untouched here).
