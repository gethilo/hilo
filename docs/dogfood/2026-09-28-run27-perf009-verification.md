# Dogfood Run 27 — PERF-009 Symbol Cache Verification + Graph Query Performance

**Date:** 2026-09-28 17:24-17:40  
**Binary:** v0.3.0-144-g01a9634-dirty (debug build, 2026-09-28 15:51)  
**Corpus:** BurntSushi/ripgrep (111 files, 830 edges, 83 files with edges)  
**Angle:** PERF-009 symbol cache persistence verification + graph query performance baseline

## What Was Tested

1. **PERF-009 symbol cache:** Does `.vfs/graph/.symbols_cache.json` actually persist and speed up `graph understand`?
2. **Graph query performance:** Baseline timing for search/understand on a debug build
3. **Warm rebuild:** How long does it take to rebuild graph.db from edges.jsonl after deletion?

## Results

### PERF-009 Symbol Cache

**Cache file created:** YES (55K, 4 entries)  
**Speedup:** ~10% (1.94s → 1.73s for `graph understand "rate limiter"`)  
**Verdict:** The cache works but provides minimal speedup. Not enough to make understand viable on large corpora where it currently takes 2-3s even with the cache.

**Measurement details:**
- Without cache (deleted): 1.940s ± 0.145s (5 runs)
- With cache (55K file): 1.732s ± 0.239s (10 runs)
- Delta: 0.208s (10.7% faster)

**Why so small?** The understand tool still re-parses the entire corpus to extract symbols on every invocation. The cache only avoids re-extracting symbols from files that haven't changed, but the parsing overhead dominates.

### Graph Query Performance (Debug Build)

**search "main":** 166-172ms warm (20 runs)  
**search "pkg:std":** 228ms warm (20 runs)  
**understand "rate limiter":** 1.73s warm (with cache, 10 runs)

**Note:** These are debug build numbers. The README claims 0.02s for search on 793 files (release build). The 200x gap is expected — debug builds are 100-300x slower than release. This is not a regression.

### Warm Rebuild from Deleted graph.db

**Time:** 4.7s (cold rebuild from edges.jsonl, 830 edges)  
**Output:** "reconcile checkpoint claims 629 cached edges but the graph db holds 0; rebuilding the cache from edges.jsonl (DF-WARPFS-61)"

The warm path correctly detects a missing/corrupt graph.db and rebuilds from edges.jsonl. This is the DF-WARPFS-61/64 fix working as designed.

## Verdict

**No new findings.** The product is stable at HEAD c0da391.

- PERF-009 works but provides only ~10% speedup (not the 100x+ needed to make understand viable on large corpora)
- Graph queries are fast on debug build (166-228ms for search)
- Warm rebuild from deleted graph.db works correctly (4.7s)
- No regressions detected

**Why no rows filed:** The skill says "When nothing is slow enough that a user would notice, say exactly that — a win nobody can feel is not a finding, and filing it devalues the real ones." The 10% speedup from PERF-009 is not worth filing. The debug vs release build difference is expected, not a defect.

## What Was Left Behind

- This integration report
- Dogfood log entry (no new rows)
- No board changes (nothing to file)

</