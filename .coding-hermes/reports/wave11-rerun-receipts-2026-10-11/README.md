# Wave 11 Re-evaluation (GAP-118) — Receipts

Run: `rerun-gap118-20261011-01` on the same controlled Linux benchmark host as the
baseline run (`full-run-20261005-03`). This directory carries verification
receipts; the full per-command stdout/stderr archive stays on the benchmark host
alongside the baseline archive (~6GB; noted in the run manifest).

## What changed vs baseline
- Hilo binary: rebuilt from master `5825161f5444882292a0bc8a3b0ff3854ca44719`
  (post GAP-098 / GAP-101 / GAP-116 / GAP-117). Baseline Hilo was
  `9da34ed584a654a0303a3caf5585a5420461cb70`.
- Every other tool pin is IDENTICAL to baseline:
  - scc 0.2.9, binary SHA-256 `4a0285a00f56974aa3ccaa67cd4973b46ee5bd907e6b72851f03b58ca2545e77` (verified identical)
  - CodeGraph 1.6.2, binary SHA-256 `3184bbf582b5db7cffd07373017631fdc4392842860c2e3991be3f23b6fb3559` (verified identical)
  - Code Review Graph 2.3.9, Aider 0.86.2, Repomix 1.18.1 (same installs)
- Case files identical (SHA-256 verified before the run):
  - core `35fa4778c3cac2f7b13afe71f30ee31d618850bc6af0742be562730ea5d83e22`
  - surface `6198064d7b9a7a925c080a204eb12a92a55468291a3b8e4464b89bac8087c4d2`
  - fairness mapping `69719ebad1aa3ee8aa4314bcefdac76df4d67bced584b9656381b88bb8b52631`
- Scoring code identical: `score_wave11_full.py` (unchanged) run against both run dirs.

## Repository pins (identical in both runs)
- pydantic/pydantic `0b2205799fb3735ed0639dd560d088d7858ebfe5`
- astral-sh/ruff `df78bcfc148681112a823b390f15e610d20b1bf6`
- sveltejs/svelte `7c2fcdae6a29fb3b36855dc8e86eb7ce4c56b93a`
- denoland/deno `3d44d1d82fdaca0b4e776bfe89e46e026e29f72d`
- spring-projects/spring-boot `2ad19e44980bc1f31e119b81a838e9829295aa4f`
- grpc/grpc `daa899ed7e3a0c2f12451c90fdc3eb4d7e03aa54`

## Baseline reference scores (Hilo)
- Architecture: 18.8% path recall (6/6 scorable)
- Context-pack: 19.3% path recall (6/6 scorable)
- Exact search: MRR 0.457, 79.2% path recall (12/12)
- Flow: 18.1% path recall (6/6)
- Impact tests: 26.7% path recall, 66.7% test/build recall (6/6)
- Natural search: MRR 0.025, 15.0% path recall (6/6)
- Total scorable: 172/288 rows; 48 tasks (42 core + 6 surface)

## Files in this directory
- `hilo-binary-sha256.txt` — post-fix Hilo binary hash (build provenance)
- `input-hash-verification.txt` — case/mapping file hashes re-verified at run start
- `run-manifest.json` — rerun manifest copy
- `score-summary.json`, `baseline-comparison.json`, `hilo-per-case-delta.json`,
  `gap118-summary.json` — scoring outputs
- `index-failures.jsonl` — scc index failures recorded at baseline severity
