#!/usr/bin/env bash
# llm.sh — one spike call to the inference router (Anthropic /v1/messages, 127.0.0.1:4813).
# Usage: GATEWAY_KEY=... llm.sh <model-id> <prompt-file> <doc-file> <out-prefix>
# Writes <out-prefix>.json/.txt and appends one TSV line to results.tsv:
#   model task doc wall_s in_tok out_tok stop_reason json_ok
case "${1:-}" in -h|--help) sed -n "2,/^set -euo/{/^#/s/^# \{0,1\}//p}" "$0"; exit 0;; esac
. "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")/../../../scripts/lib/build-id.sh"
build_flags "${BASH_SOURCE[0]}" "$@"
set -euo pipefail
: "${GATEWAY_KEY:?export GATEWAY_KEY (the router x-gateway-key) first}"
model=$1 prompt=$2 doc=$3 out=$4; here=$(cd "$(dirname "$0")" && pwd)
body=$(jq -n --arg m "$model" --rawfile p "$prompt" --rawfile d "$doc" \
  '{model:$m, max_tokens:16000, system:$p, messages:[{role:"user",content:("<document>\n"+$d+"\n</document>")}]}')
t0=$(date +%s.%N)
curl -s -m 900 http://127.0.0.1:4813/v1/messages -H 'content-type: application/json' \
  -H 'anthropic-version: 2023-06-01' -H "x-gateway-key: $GATEWAY_KEY" -d "$body" > "$out.json" || true
t1=$(date +%s.%N)
jq -r '[.content[]? | select(.type=="text") | .text] | join("")' "$out.json" > "$out.txt" 2>/dev/null || : > "$out.txt"
json_ok=0; sed -e '/^```/d' "$out.txt" | jq -e . >/dev/null 2>&1 && json_ok=1
printf '%s\t%s\t%s\t%.1f\t%s\t%s\t%s\t%s\n' "$model" "$(basename "$prompt" .md)" "$(basename "$doc" .md)" \
  "$(echo "$t1 - $t0" | bc)" "$(jq -r '.usage.input_tokens // "-"' "$out.json" 2>/dev/null)" \
  "$(jq -r '.usage.output_tokens // "-"' "$out.json" 2>/dev/null)" \
  "$(jq -r '.stop_reason // .error.message // "-"' "$out.json" 2>/dev/null | head -c 80)" "$json_ok" >> "$here/results.tsv"
