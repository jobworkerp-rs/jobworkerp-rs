#!/usr/bin/env bash
# Bisect which part of the reflection schema causes 400 INVALID_ARGUMENT under
# generationConfig.responseJsonSchema on gemini-3.1-flash-lite (v1beta).
#
# Add fields incrementally; the first case that flips to 400 names the culprit.
#
# Usage:
#   export GEMINI_API_KEY='your-key'
#   bash scripts/diag-gemini-schema-bisect.sh

set -u

MODEL="${1:-gemini-3.1-flash-lite}"
BASE_URL="${LOOKBACK_EXTERNAL_LLM_BASE_URL:-https://generativelanguage.googleapis.com}"
BASE_URL="${BASE_URL%/}"
URL="${BASE_URL}/v1beta/models/${MODEL}:generateContent?key=${GEMINI_API_KEY:?GEMINI_API_KEY must be set}"

RUN_DIR="$(mktemp -d)"
trap 'rm -rf "$RUN_DIR"' EXIT

# Wrap a schema fragment in a minimal request and POST it.
# $1 = label, $2 = schema JSON (the value of generationConfig.responseJsonSchema)
try_schema() {
  local label="$1"
  local schema="$2"
  local payload="${RUN_DIR}/req.json"

  jq -n --argjson schema "$schema" '{
    contents: [{role:"user", parts:[{text:"Return JSON matching the schema."}]}],
    generationConfig: {
      responseMimeType: "application/json",
      responseJsonSchema: $schema,
      maxOutputTokens: 800
    }
  }' >"$payload"

  local out="${RUN_DIR}/${label}.out"
  local status
  status=$(curl -sS -o "$out" -w '%{http_code}' -X POST "$URL" \
    -H 'Content-Type: application/json' --data-binary "@${payload}")

  if [[ "$status" == "200" ]]; then
    printf '[%s]  HTTP %s  OK\n' "$label" "$status"
  else
    printf '[%s]  HTTP %s  FAIL\n' "$label" "$status"
    jq -r '.error.message // .' "$out" 2>/dev/null | sed 's/^/    /'
  fi
}

# Schema fragments (incremental). Each adds one suspect feature on top of A.

A='{"type":"object","properties":{"summary":{"type":"string"}},"required":["summary"]}'

B='{"type":"object","properties":{"summary":{"type":"string"},"outcome":{"type":"string","enum":["SUCCESS","PARTIAL","FAILURE","ABORTED","UNKNOWN"]}},"required":["summary","outcome"]}'

C='{"type":"object","properties":{"summary":{"type":"string"},"score_self":{"type":"number","minimum":0.0,"maximum":1.0}},"required":["summary","score_self"]}'

D='{"type":"object","properties":{"summary":{"type":"string"},"tools_used":{"type":"array","items":{"type":"string"},"maxItems":50}},"required":["summary"]}'

# Five enums (matches reflection schema enum count) at top level
E='{"type":"object","properties":{
  "outcome":{"type":"string","enum":["SUCCESS","PARTIAL","FAILURE","ABORTED","UNKNOWN"]},
  "task_category":{"type":"string","enum":["coding","consultation","research","creative","general"]},
  "reflection_aspect":{"type":"string","enum":["TASK_OUTCOME","INTERACTION_STYLE","BOTH"]},
  "contribution":{"type":"string","enum":["POSITIVE","NEGATIVE","NEUTRAL"]},
  "field":{"type":"string","enum":["lesson","failure_mode","key_decision","success_factor"]}
},"required":["outcome"]}'

# Large enum inside array.items (failure_modes)
F='{"type":"object","properties":{
  "failure_modes":{"type":"array","items":{"type":"string","enum":["tool_misuse","loop","scope_drift","hallucination","context_overflow","data_loss","permission_issue","ambiguous_instruction","conflicting_requirements","missing_context","misleading_premise","goal_drift_by_user","tool_unavailable","external_service_failure","rate_limit","OTHER"]},"maxItems":8}
},"required":["failure_modes"]}'

# 4-level nested structure mirroring facts → links → field
G='{"type":"object","properties":{
  "facts":{"type":"array","items":{"type":"object","required":["turn_index","kind"],"properties":{
    "turn_index":{"type":"integer","minimum":0},
    "kind":{"type":"string","enum":["OUTCOME_EVIDENCE","SCORE_DRIVER","LESSON_SOURCE","KEY_DECISION_POINT","EXEMPLAR","COUNTER_EXAMPLE","CONTEXT_PIVOT"]},
    "links":{"type":"array","items":{"type":"object","properties":{
      "field":{"type":"string","enum":["lesson","failure_mode","key_decision","success_factor"]},
      "index":{"type":"integer","minimum":0}
    }}}
  }},"maxItems":30}
},"required":["facts"]}'

# Same as G but drop maxItems
G_NOMAX='{"type":"object","properties":{
  "facts":{"type":"array","items":{"type":"object","required":["turn_index","kind"],"properties":{
    "turn_index":{"type":"integer","minimum":0},
    "kind":{"type":"string","enum":["OUTCOME_EVIDENCE","SCORE_DRIVER","LESSON_SOURCE","KEY_DECISION_POINT","EXEMPLAR","COUNTER_EXAMPLE","CONTEXT_PIVOT"]},
    "links":{"type":"array","items":{"type":"object","properties":{
      "field":{"type":"string","enum":["lesson","failure_mode","key_decision","success_factor"]},
      "index":{"type":"integer","minimum":0}
    }}}
  }}}
},"required":["facts"]}'

# Same as G but flatten links → drop one nesting level
G_FLAT='{"type":"object","properties":{
  "facts":{"type":"array","items":{"type":"object","required":["turn_index","kind"],"properties":{
    "turn_index":{"type":"integer","minimum":0},
    "kind":{"type":"string","enum":["OUTCOME_EVIDENCE","SCORE_DRIVER","LESSON_SOURCE","KEY_DECISION_POINT","EXEMPLAR","COUNTER_EXAMPLE","CONTEXT_PIVOT"]},
    "link_field":{"type":"string","enum":["lesson","failure_mode","key_decision","success_factor"]},
    "link_index":{"type":"integer","minimum":0}
  }},"maxItems":30}
},"required":["facts"]}'

# Full reflection schema (current YAML, no maxLength) — should reproduce 400
FULL='{
  "type":"object",
  "required":["outcome","score_self","summary","task_intent","task_category","reflection_aspect","failure_modes","tools_used","lessons","success_factors","key_decisions","facts"],
  "properties":{
    "outcome":{"type":"string","enum":["SUCCESS","PARTIAL","FAILURE","ABORTED","UNKNOWN"]},
    "score_self":{"type":"number","minimum":0.0,"maximum":1.0},
    "summary":{"type":"string"},
    "task_intent":{"type":"string"},
    "task_category":{"type":"string","enum":["coding","consultation","research","creative","general"]},
    "reflection_aspect":{"type":"string","enum":["TASK_OUTCOME","INTERACTION_STYLE","BOTH"]},
    "failure_modes":{"type":"array","items":{"type":"string","enum":["tool_misuse","loop","scope_drift","hallucination","context_overflow","data_loss","permission_issue","ambiguous_instruction","conflicting_requirements","missing_context","misleading_premise","goal_drift_by_user","tool_unavailable","external_service_failure","rate_limit","OTHER"]},"maxItems":8},
    "failure_modes_other":{"type":"array","items":{"type":"string"},"maxItems":5},
    "tools_used":{"type":"array","items":{"type":"string"},"maxItems":50},
    "success_factors":{"type":"array","items":{"type":"string"},"maxItems":8},
    "lessons":{"type":"array","items":{"type":"string"},"maxItems":10},
    "key_decisions":{"type":"array","items":{"type":"string"},"maxItems":10},
    "mitigation_hint":{"type":"string"},
    "tool_outcomes":{"type":"array","items":{"type":"object","required":["tool","contribution"],"properties":{"tool":{"type":"string"},"contribution":{"type":"string","enum":["POSITIVE","NEGATIVE","NEUTRAL"]},"error_kind":{"type":"string"}}},"maxItems":50},
    "facts":{"type":"array","items":{"type":"object","required":["turn_index","kind"],"properties":{"turn_index":{"type":"integer","minimum":0},"kind":{"type":"string","enum":["OUTCOME_EVIDENCE","SCORE_DRIVER","LESSON_SOURCE","KEY_DECISION_POINT","EXEMPLAR","COUNTER_EXAMPLE","CONTEXT_PIVOT"]},"weight":{"type":"number","minimum":0.0,"maximum":1.0},"note":{"type":"string"},"links":{"type":"array","items":{"type":"object","properties":{"field":{"type":"string","enum":["lesson","failure_mode","key_decision","success_factor"]},"index":{"type":"integer","minimum":0}}}}}},"maxItems":30}
  }
}'

# Full minus facts (kill the deep tree)
FULL_NO_FACTS='{
  "type":"object",
  "required":["outcome","summary"],
  "properties":{
    "outcome":{"type":"string","enum":["SUCCESS","PARTIAL","FAILURE","ABORTED","UNKNOWN"]},
    "score_self":{"type":"number","minimum":0.0,"maximum":1.0},
    "summary":{"type":"string"},
    "task_intent":{"type":"string"},
    "task_category":{"type":"string","enum":["coding","consultation","research","creative","general"]},
    "reflection_aspect":{"type":"string","enum":["TASK_OUTCOME","INTERACTION_STYLE","BOTH"]},
    "failure_modes":{"type":"array","items":{"type":"string","enum":["tool_misuse","loop","scope_drift","hallucination","context_overflow","data_loss","permission_issue","ambiguous_instruction","conflicting_requirements","missing_context","misleading_premise","goal_drift_by_user","tool_unavailable","external_service_failure","rate_limit","OTHER"]},"maxItems":8},
    "failure_modes_other":{"type":"array","items":{"type":"string"},"maxItems":5},
    "tools_used":{"type":"array","items":{"type":"string"},"maxItems":50},
    "success_factors":{"type":"array","items":{"type":"string"},"maxItems":8},
    "lessons":{"type":"array","items":{"type":"string"},"maxItems":10},
    "key_decisions":{"type":"array","items":{"type":"string"},"maxItems":10},
    "mitigation_hint":{"type":"string"},
    "tool_outcomes":{"type":"array","items":{"type":"object","required":["tool","contribution"],"properties":{"tool":{"type":"string"},"contribution":{"type":"string","enum":["POSITIVE","NEGATIVE","NEUTRAL"]},"error_kind":{"type":"string"}}},"maxItems":50}
  }
}'

# Full minus tool_outcomes (kill the nested object array)
FULL_NO_TOOL_OUTCOMES=$(echo "$FULL" | jq 'del(.properties.tool_outcomes)')

# Full minus all maxItems
FULL_NO_MAXITEMS=$(echo "$FULL" | jq 'walk(if type=="object" then del(.maxItems) else . end)')

echo "=== Incremental bisect (looking for first FAIL) ==="
try_schema "A.baseline"                "$A"
try_schema "B.+enum_topLevel"          "$B"
try_schema "C.+number_min_max_float"   "$C"
try_schema "D.+array_maxItems_50"      "$D"
try_schema "E.5enums_topLevel"         "$E"
try_schema "F.large_enum_in_array"     "$F"
try_schema "G.deep_facts_links"        "$G"
try_schema "G_NOMAX.deep_no_maxItems"  "$G_NOMAX"
try_schema "G_FLAT.flatten_links"      "$G_FLAT"

echo
echo "=== Composite tests (toward the full schema) ==="
try_schema "FULL.current_yaml"             "$FULL"
try_schema "FULL_NO_FACTS"                 "$FULL_NO_FACTS"
try_schema "FULL_NO_TOOL_OUTCOMES"         "$FULL_NO_TOOL_OUTCOMES"
try_schema "FULL_NO_MAXITEMS"              "$FULL_NO_MAXITEMS"

echo
echo "=== Reading ==="
echo "First fragment that flips to FAIL identifies the culprit feature."
echo "If FULL_NO_FACTS passes and FULL fails → facts subtree is the killer (flatten or split)."
echo "If FULL_NO_MAXITEMS passes and FULL fails → maxItems on responseJsonSchema is the killer."
