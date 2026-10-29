#!/usr/bin/env bash
# Sampler arms on the extraction pass of the 7 temp-0 loopers (reasoning off, max_tokens 2048).
# Usage: loop.sh (run from the spike dir holding in-a/ and in-b/; ARMS="t0 default t0-rep t0-dry rec rec-rep" selects arms)
case "${1:-}" in -h|--help) sed -n "2,/^set -euo/{/^#/s/^# \{0,1\}//p}" "$0"; exit 0;; esac
. "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")/../../../scripts/lib/build-id.sh"
build_flags "${BASH_SOURCE[0]}" "$@"
set -euo pipefail
URL=http://127.0.0.1:4811/v1/chat/completions
EXTR='Transcribe this image as structured plain text: its title, subtitle, axis labels and ranges, series names, data values, annotations, table content, and component labels, in that order where present. Plain text only, no JSON, no markdown formatting.'
mkdir -p out-loop; tmp=$(mktemp -d)
for img in in-a/A01.png in-a/A02.png in-a/A03.png in-a/A04.png in-b/S09.png in-b/S12.png in-b/S12c.png; do
  id=$(basename "$img" .png); base64 -w0 "$img" > "$tmp/b64"
  for arm in ${ARMS:-t0 default t0-rep t0-dry}; do
    case $arm in
      t0) x='{"temperature":0}';;
      default) x='{}';;
      t0-rep) x='{"temperature":0,"repeat_penalty":1.15,"repeat_last_n":256}';;
      rec) x='{"temperature":1.0,"top_p":0.95,"top_k":64}';;
      rec-rep) x='{"temperature":1.0,"top_p":0.95,"top_k":64,"repeat_penalty":1.15,"repeat_last_n":256}';;
      t0-dry) x='{"temperature":0,"dry_multiplier":0.8,"dry_base":1.75,"dry_allowed_length":2}';;
    esac
    jq -n --rawfile img "$tmp/b64" --arg p "$EXTR" --argjson x "$x" '{model:"gemma-4-12B-it-qat-UD-Q4_K_XL",max_tokens:2048,reasoning_effort:"none",messages:[{role:"user",content:[{type:"text",text:$p},{type:"image_url",image_url:{url:("data:image/png;base64,"+$img)}}]}]} + $x' > "$tmp/req.json"
    s=$(date +%s%3N); curl -s -m 300 "$URL" -H 'content-type: application/json' -d @"$tmp/req.json" > "$tmp/r.json"; e=$(date +%s%3N)
    jq --argjson ms $((e-s)) --arg id $id --arg arm $arm '{id:$id,arm:$arm,ms:$ms,finish:.choices[0].finish_reason,ct:.usage.completion_tokens,content:.choices[0].message.content}' "$tmp/r.json" > out-loop/$id.$arm.json
  done
done; rm -rf "$tmp"
