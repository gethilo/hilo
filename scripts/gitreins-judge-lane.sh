#!/usr/bin/env bash
# gitreins-judge-lane.sh — run gitreins on this repo's working tier-2 judge lane.
#
# Why this exists (DF-WARPFS-177): the tier-2 judge lane is resolved purely from
# the environment (GITREINS_LLM_BASE_URL / GITREINS_LLM_MODEL / GITREINS_LLM_API_KEY
# — the .gitreins/config.yaml `defaults:` block is documentation, the evaluator
# never reads it). The ambient env names the deepseek lane, whose account returns
# `402 Payment Required` on every call, so every bare `gitreins task complete`
# in this repo wrote verdict passed=false. This wrapper exports a funded,
# plan-covered lane (ollama-cloud / glm-5.3-flash) for the invocation it wraps.
#
# Usage (from anywhere inside the repo):
#   bash scripts/gitreins-judge-lane.sh task complete <TASK-ID>          # tier1+tier2
#   bash scripts/gitreins-judge-lane.sh judge <TASK-ID> --async          # re-judge
#   bash scripts/gitreins-judge-lane.sh judge --ephemeral --title t --criterion c
#
# The API key is resolved internally (ambient env -> ~/.hermes/.env ->
# ~/.hermes/config.yaml `ollama-cloud:` block) and never appears in argv.
# If the lane dies, edit LANE_* below to the next verified endpoint set —
# base URL + model + key are ONE unit (judge-provider-failover doctrine).
set -euo pipefail

LANE_BASE_URL="https://ollama.com/v1"
LANE_MODEL="glm-5.3-flash"
LANE_KEY_ENV="OLLAMA_CLOUD_API_KEY"

key=""
if [ -n "${!LANE_KEY_ENV:-}" ]; then
    key="${!LANE_KEY_ENV}"
elif [ -f "$HOME/.hermes/.env" ]; then
    key="$(awk -F= -v k="$LANE_KEY_ENV" '$1 ~ "^"k"[[:space:]]*$" { sub(/^[^=]*=[[:space:]]*/, ""); gsub(/^"|"$/, ""); print; exit }' "$HOME/.hermes/.env")"
fi
if [ -z "$key" ] && [ -f "$HOME/.hermes/config.yaml" ]; then
    key="$(awk '/^  ollama-cloud:/{f=1;next} f&&/^  [a-zA-Z0-9_-]+:/{f=0} f&&/api_key:/{print $2;exit}' "$HOME/.hermes/config.yaml" | tr -d '\"')"
fi
if [ -z "$key" ]; then
    echo "gitreins-judge-lane.sh: no $LANE_KEY_ENV found (env, ~/.hermes/.env, ~/.hermes/config.yaml) — refusing to run the judge on an unprobed lane" >&2
    exit 2
fi
if [ "${#key}" -lt 30 ]; then
    echo "gitreins-judge-lane.sh: resolved $LANE_KEY_ENV looks truncated (len ${#key}) — refusing (bad-key wedge reads as a hang, not a 401)" >&2
    exit 2
fi

export GITREINS_LLM_BASE_URL="$LANE_BASE_URL"
export GITREINS_LLM_MODEL="$LANE_MODEL"
export GITREINS_LLM_API_KEY="$key"

exec gitreins "$@"
