#!/usr/bin/env bash
# Verify that the post-fix schema (maxLength restored, maxItems removed) is
# accepted by gemini-3.1-flash-lite via responseJsonSchema on v1beta.
#
# Usage:
#   export GEMINI_API_KEY='your-key'
#   bash scripts/diag-gemini-final-check.sh

set -u

MODEL="${1:-gemini-3.1-flash-lite}"
BASE_URL="${LOOKBACK_EXTERNAL_LLM_BASE_URL:-https://generativelanguage.googleapis.com}"
BASE_URL="${BASE_URL%/}"
URL="${BASE_URL}/v1beta/models/${MODEL}:generateContent?key=${GEMINI_API_KEY:?GEMINI_API_KEY must be set}"

RUN_DIR="$(mktemp -d)"
trap 'rm -rf "$RUN_DIR"' EXIT

# Exact schema from agent-app/workers/workflows/thread-reflection/thread-reflection-single.yaml
# after the fix (maxItems removed, maxLength kept).
SCHEMA='{
  "type":"object",
  "required":["outcome","score_self","summary","task_intent","task_category","reflection_aspect","failure_modes","tools_used","lessons","success_factors","key_decisions","facts"],
  "properties":{
    "outcome":{"type":"string","enum":["SUCCESS","PARTIAL","FAILURE","ABORTED","UNKNOWN"]},
    "score_self":{"type":"number","minimum":0.0,"maximum":1.0},
    "summary":{"type":"string"},
    "task_intent":{"type":"string","maxLength":1000},
    "task_category":{"type":"string","enum":["coding","consultation","research","creative","general"]},
    "reflection_aspect":{"type":"string","enum":["TASK_OUTCOME","INTERACTION_STYLE","BOTH"]},
    "failure_modes":{"type":"array","items":{"type":"string","enum":["tool_misuse","loop","scope_drift","hallucination","context_overflow","data_loss","permission_issue","ambiguous_instruction","conflicting_requirements","missing_context","misleading_premise","goal_drift_by_user","tool_unavailable","external_service_failure","rate_limit","OTHER"]}},
    "failure_modes_other":{"type":"array","items":{"type":"string","maxLength":100}},
    "tools_used":{"type":"array","items":{"type":"string","maxLength":100}},
    "success_factors":{"type":"array","items":{"type":"string","maxLength":200}},
    "lessons":{"type":"array","items":{"type":"string","maxLength":300}},
    "key_decisions":{"type":"array","items":{"type":"string","maxLength":300}},
    "mitigation_hint":{"type":"string","maxLength":500},
    "tool_outcomes":{"type":"array","items":{"type":"object","required":["tool","contribution"],"properties":{"tool":{"type":"string","maxLength":100},"contribution":{"type":"string","enum":["POSITIVE","NEGATIVE","NEUTRAL"]},"error_kind":{"type":"string","maxLength":100}}}},
    "facts":{"type":"array","items":{"type":"object","required":["turn_index","kind"],"properties":{"turn_index":{"type":"integer","minimum":0},"kind":{"type":"string","enum":["OUTCOME_EVIDENCE","SCORE_DRIVER","LESSON_SOURCE","KEY_DECISION_POINT","EXEMPLAR","COUNTER_EXAMPLE","CONTEXT_PIVOT"]},"weight":{"type":"number","minimum":0.0,"maximum":1.0},"note":{"type":"string","maxLength":200},"links":{"type":"array","items":{"type":"object","properties":{"field":{"type":"string","enum":["lesson","failure_mode","key_decision","success_factor"]},"index":{"type":"integer","minimum":0}}}}}}}
  }
}'

payload="${RUN_DIR}/req.json"
jq -n --argjson schema "$SCHEMA" '{
  systemInstruction: {parts:[{text:"You are a reflection generator. Output JSON only."}]},
  contents: [{role:"user", parts:[{text:"Thread: hello world. Reflect briefly."}]}],
  generationConfig: {
    responseMimeType: "application/json",
    responseJsonSchema: $schema,
    maxOutputTokens: 4000,
    temperature: 0.2
  }
}' >"$payload"

out="${RUN_DIR}/out.json"
status=$(curl -sS -o "$out" -w '%{http_code}' -X POST "$URL" \
  -H 'Content-Type: application/json' --data-binary "@${payload}")

echo "HTTP ${status}"
if [[ "$status" == "200" ]]; then
  echo "PASS: post-fix schema accepted."
  jq '{text: (.candidates[0].content.parts[0].text // "")[0:300], usage: .usageMetadata}' "$out"
else
  echo "FAIL: schema still rejected."
  jq '.' "$out"
fi
