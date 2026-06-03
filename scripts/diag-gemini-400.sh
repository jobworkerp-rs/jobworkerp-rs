#!/usr/bin/env bash
# Diagnose Gemini 3.1-flash-lite 400 INVALID_ARGUMENT in the genai path.
# Reproduces the responseJsonSchema payload that genai 0.6.3 sends, plus three
# control variants, so we can isolate whether the failure is caused by
# (a) the responseJsonSchema field itself,
# (b) the schema contents,
# (c) the v1beta endpoint, or
# (d) the API key / model availability.
#
# Usage:
#   export GEMINI_API_KEY='your-key'
#   bash scripts/diag-gemini-400.sh [model] [base_url]
# Defaults: model=gemini-3.1-flash-lite, base_url=https://generativelanguage.googleapis.com

set -u

MODEL="${1:-gemini-3.1-flash-lite}"
BASE_URL_DEFAULT="https://generativelanguage.googleapis.com"
BASE_URL="${2:-${LOOKBACK_EXTERNAL_LLM_BASE_URL:-$BASE_URL_DEFAULT}}"
BASE_URL="${BASE_URL%/}"

if [[ -z "${GEMINI_API_KEY:-}" ]]; then
  echo "ERROR: GEMINI_API_KEY is not set. export GEMINI_API_KEY='...' first." >&2
  exit 1
fi

if ! command -v jq >/dev/null 2>&1; then
  echo "ERROR: jq is required." >&2
  exit 1
fi

RUN_DIR="$(mktemp -d)"
trap 'rm -rf "$RUN_DIR"' EXIT

run_case() {
  local label="$1"
  local endpoint_version="$2"   # v1beta | v1
  local payload_file="$3"

  local url="${BASE_URL}/${endpoint_version}/models/${MODEL}:generateContent?key=${GEMINI_API_KEY}"
  local out="${RUN_DIR}/${label}.out"
  local status

  echo
  echo "================================================================"
  echo "[${label}]  ${endpoint_version}  ${MODEL}"
  echo "================================================================"

  status=$(curl -sS -o "$out" -w '%{http_code}' -X POST "$url" \
    -H 'Content-Type: application/json' \
    --data-binary "@${payload_file}")

  echo "HTTP ${status}"
  if jq -e . "$out" >/dev/null 2>&1; then
    # Show error or short success summary
    if jq -e '.error' "$out" >/dev/null 2>&1; then
      jq '.error' "$out"
    else
      jq '{candidatesText: [.candidates[]?.content.parts[]?.text] | join("") | .[0:200], usageMetadata}' "$out"
    fi
  else
    head -c 1000 "$out"
    echo
  fi
}

# ----- payload 1: genai 0.6.3 style — full reflection schema via responseJsonSchema
cat >"${RUN_DIR}/p1.json" <<'JSON'
{
  "systemInstruction": {
    "parts": [{"text": "You are a reflection generator. Output JSON only."}]
  },
  "contents": [
    {"role": "user", "parts": [{"text": "Thread: hello. Reflect briefly."}]}
  ],
  "generationConfig": {
    "responseMimeType": "application/json",
    "responseJsonSchema": {
      "type": "object",
      "required": ["outcome","score_self","summary","task_intent","task_category","reflection_aspect","failure_modes","tools_used","lessons","success_factors","key_decisions","facts"],
      "properties": {
        "outcome": {"type":"string","enum":["SUCCESS","PARTIAL","FAILURE","ABORTED","UNKNOWN"]},
        "score_self": {"type":"number","minimum":0.0,"maximum":1.0},
        "summary": {"type":"string"},
        "task_intent": {"type":"string"},
        "task_category": {"type":"string","enum":["coding","consultation","research","creative","general"]},
        "reflection_aspect": {"type":"string","enum":["TASK_OUTCOME","INTERACTION_STYLE","BOTH"]},
        "failure_modes": {"type":"array","items":{"type":"string","enum":["tool_misuse","loop","scope_drift","hallucination","context_overflow","data_loss","permission_issue","ambiguous_instruction","conflicting_requirements","missing_context","misleading_premise","goal_drift_by_user","tool_unavailable","external_service_failure","rate_limit","OTHER"]},"maxItems":8},
        "failure_modes_other": {"type":"array","items":{"type":"string"},"maxItems":5},
        "tools_used": {"type":"array","items":{"type":"string"},"maxItems":50},
        "success_factors": {"type":"array","items":{"type":"string"},"maxItems":8},
        "lessons": {"type":"array","items":{"type":"string"},"maxItems":10},
        "key_decisions": {"type":"array","items":{"type":"string"},"maxItems":10},
        "mitigation_hint": {"type":"string"},
        "tool_outcomes": {"type":"array","items":{"type":"object","required":["tool","contribution"],"properties":{"tool":{"type":"string"},"contribution":{"type":"string","enum":["POSITIVE","NEGATIVE","NEUTRAL"]},"error_kind":{"type":"string"}}},"maxItems":50},
        "facts": {"type":"array","items":{"type":"object","required":["turn_index","kind"],"properties":{"turn_index":{"type":"integer","minimum":0},"kind":{"type":"string","enum":["OUTCOME_EVIDENCE","SCORE_DRIVER","LESSON_SOURCE","KEY_DECISION_POINT","EXEMPLAR","COUNTER_EXAMPLE","CONTEXT_PIVOT"]},"weight":{"type":"number","minimum":0.0,"maximum":1.0},"note":{"type":"string"},"links":{"type":"array","items":{"type":"object","properties":{"field":{"type":"string","enum":["lesson","failure_mode","key_decision","success_factor"]},"index":{"type":"integer","minimum":0}}}}}},"maxItems":30}
      }
    },
    "maxOutputTokens": 20000,
    "temperature": 0.2
  }
}
JSON

# ----- payload 2: same schema but via the OpenAPI-subset responseSchema field
cat >"${RUN_DIR}/p2.json" <<'JSON'
{
  "systemInstruction": {"parts":[{"text":"You are a reflection generator. Output JSON only."}]},
  "contents": [{"role":"user","parts":[{"text":"Thread: hello. Reflect briefly."}]}],
  "generationConfig": {
    "responseMimeType": "application/json",
    "responseSchema": {
      "type": "object",
      "required": ["outcome","summary"],
      "properties": {
        "outcome": {"type":"string","enum":["SUCCESS","PARTIAL","FAILURE","ABORTED","UNKNOWN"]},
        "summary": {"type":"string"}
      }
    },
    "maxOutputTokens": 2000,
    "temperature": 0.2
  }
}
JSON

# ----- payload 3: plain call, no schema (sanity check for the endpoint/key)
cat >"${RUN_DIR}/p3.json" <<'JSON'
{
  "contents": [{"role":"user","parts":[{"text":"Say hello."}]}],
  "generationConfig": {"maxOutputTokens": 200}
}
JSON

# ----- payload 4: minimal responseJsonSchema (isolate field-name vs schema-content)
cat >"${RUN_DIR}/p4.json" <<'JSON'
{
  "contents": [{"role":"user","parts":[{"text":"Return JSON with key ok=true."}]}],
  "generationConfig": {
    "responseMimeType": "application/json",
    "responseJsonSchema": {
      "type": "object",
      "properties": {"ok": {"type": "boolean"}},
      "required": ["ok"]
    },
    "maxOutputTokens": 200
  }
}
JSON

echo "Base URL : ${BASE_URL}"
echo "Model    : ${MODEL}"

# v1beta cases
run_case "1.v1beta.responseJsonSchema.full"    v1beta "${RUN_DIR}/p1.json"
run_case "2.v1beta.responseSchema.minimal"     v1beta "${RUN_DIR}/p2.json"
run_case "3.v1beta.no-schema.plain"            v1beta "${RUN_DIR}/p3.json"
run_case "4.v1beta.responseJsonSchema.minimal" v1beta "${RUN_DIR}/p4.json"

# v1 sanity (some models only ship on v1)
run_case "5.v1.no-schema.plain"                v1     "${RUN_DIR}/p3.json"
run_case "6.v1.responseJsonSchema.minimal"     v1     "${RUN_DIR}/p4.json"

echo
echo "================================================================"
echo "Interpretation guide:"
echo "  1 fail + 4 fail + 3 ok  → responseJsonSchema field unsupported on this model"
echo "  1 fail + 4 ok           → schema content (depth/enum count) is the culprit"
echo "  1 fail + 2 ok           → switch to responseSchema (OpenAPI subset) works"
echo "  3 fail                  → model id / API key / endpoint version problem"
echo "  5 ok + 3 fail           → model only on v1, change BASE/version"
echo "================================================================"
