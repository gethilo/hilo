# INT-CI-024 judge-lane verification

Tick: warpfs-foreman-2026-11-27-17-50-12
Base: 4dec8a15757321cbd3416f439ad6fdd1892d1702

STATUS: SKIP — no genuinely served tier-2 judge lane was proven; the board row remains pending.

Requested worker lane: model glm-5.3-flash, provider opencode-go-2.

Observed worker session: the dispatch did not serve the requested pair. Its session records show the fallback chain ending at model cx/gpt-6.1-terra, provider 9router, with fallback entries from glm-5.3-flash through zai-glm-default and synthetic. The requested opencode-go-2 identity was not observed in the API-call record. The worker was stopped after it produced no evidence or product diff; no judge was launched.

Independent probe: a probe requesting glm-5.3-flash through zai-glm returned the text LANE_OK, but the gateway log recorded fallback activation through zai-glm-default, synthetic, and finally cx/gpt-6.1-terra@9router. Therefore the text response is not lane proof.

Current failure evidence: the same dispatch window recorded HTTP 429 subscription-rate-limit errors from synthetic for hf:deepseek-ai/DeepSeek-V4.1-Flash-1017. The configured requested GLM route likewise fell back rather than serving the requested identity. Historical published board evidence for DF-WARPFS-124 records an HTTP 429 from ollama-cloud with no accepted verdict. These are instrument/provider failures, not a product verdict.

Persisted tier-2 verdict: NONE. No non-INCOMPLETE verdict was produced or claimed.

Required follow-up: append this dated recurrence to INT-CI-024 and retry only after a judge lane can be independently proven by its served model/provider and a persisted verdict artifact. Do not close the row from LANE_OK, a fallback response, or a missing verdict.

Evidence boundary: credentials omitted; no product files changed.
