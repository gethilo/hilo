# Verdict: DF-WARPFS-158

**Task:** Fix graph understand budget enforcement and empty DETAIL tier
**Evaluated:** 2026-11-01T07:18:41.028285
**Result:** ✓ PASS

## Pipeline Stages

- ✓ **tier2**
  - COMPLETE
  ✓ On the delivered DF-WARPFS-158 source tree, harmonic graph understand output including MAP, SIGNATURES, DETAIL, and any footer is bounded by token_budget*4 for normal budgets; budget 1000 is strictly shorter than 6000; DETAIL considers later candidates after an oversized block; when none fit, output names a skipped path and block size; focused formatter tests pass.: Fresh run: `cd hilo-graph && cargo test --lib signal::tests::understand_` -> "test result: ok. 34 passed; 0 failed; 0 ignored; 0 measured; 529 filtered out; finished in 3.16s" (exit 0), including understand_harmonic_output_stays_within_total_budget (asserts text.len() <= budget*4 at 1000/6000/40000), understand_budget_1000_is_strictly_smaller_than_6000, understand_detail_tier_names_a_budget_too_small_for_one_block (asserts "(no files in this tier" + "no detail block fits"), understand_budget_split_is_observable_in_detail_blocks (200->0 blocks, 6000->>0, 40000->strictly more). Code: hilo-graph/src/signal.rs:373 budget_chars = token_budget*4; :390-397 footer length reserved then truncate_to_budget; :2719-2727 tier split 15/25/60 of available; :2806-2835 DETAIL loop uses append_if_fits per block and pushes non-fitting candidates to `skipped` instead of breaking (later candidates still considered), and when rendered==0 emits "no detail block fits the {detail_budget}-char budget; skipped {path} ({size} chars)"; :2841 truncate_to_budget(output, available) bounds the whole response (MAP+SIGNATURES+DETAIL+footer). CLI uses the same core: hilo-cli/src/commands/graph.rs:3330 and :4404 call hilo_graph::signal::understand_with_symbols, which delegates to understand_with_source_and_symbols (signal.rs:256-273).
Budget enforcement, monotonicity, DETAIL skip-not-break with skipped-path/size reporting, and the focused formatter tests all verified by a fresh passing test run and code inspection.

## Summary

Judge Result: DF-WARPFS-158

Stage tier2: PASS
  COMPLETE
  ✓ On the delivered DF-WARPFS-158 source tree, harmonic graph understand output including MAP, SIGNATURES, DETAIL, and any footer is bounded by token_budget*4 for normal budgets; budget 1000 is strictly shorter than 6000; DETAIL considers later candidates after an oversized block; when none fit, output names a skipped path and block size; focused formatter tests pass.: Fresh run: `cd hilo-graph && cargo test --lib signal::tests::understand_` -> "test result: ok. 34 passed; 0 failed; 0 ignored; 0 measured; 529 filtered out; finished in 3.16s" (exit 0), including understand_harmonic_output_stays_within_total_budget (asserts text.len() <= budget*4 at 1000/6000/40000), understand_budget_1000_is_strictly_smaller_than_6000, understand_detail_tier_names_a_budget_too_small_for_one_block (asserts "(no files in this tier" + "no detail block fits"), understand_budget_split_is_observable_in_detail_blocks (200->0 blocks, 6000->>0, 40000->strictly more). Code: hilo-graph/src/signal.rs:373 budget_chars = token_budget*4; :390-397 footer length reserved then truncate_to_budget; :2719-2727 tier split 15/25/60 of available; :2806-2835 DETAIL loop uses append_if_fits per block and pushes non-fitting candidates to `skipped` instead of breaking (later candidates still considered), and when rendered==0 emits "no detail block fits the {detail_budget}-char budget; skipped {path} ({size} chars)"; :2841 truncate_to_budget(output, available) bounds the whole response (MAP+SIGNATURES+DETAIL+footer). CLI uses the same core: hilo-cli/src/commands/graph.rs:3330 and :4404 call hilo_graph::signal::understand_with_symbols, which delegates to understand_with_source_and_symbols (signal.rs:256-273).
Budget enforcement, monotonicity, DETAIL skip-not-break with skipped-path/size reporting, and the focused formatter tests all verified by a fresh passing test run and code inspection.

Overall: PASS ✓
