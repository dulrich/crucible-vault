#!/usr/bin/env bash
# Spike: image-describe output budget and reasoning on/off quality (2026-10-28).
# Mirrors cv's describe call: two passes (narrative, extraction), images pre-downscaled to a
# 1568 px long edge, PNG data URL. Adds temperature 0 (Crucible's wire). max_tokens is set high
# (8192) so the natural output length is observable; arms: reasoning off / on (default).
# Usage: run.sh <in-dir> <out-dir>
case "${1:-}" in -h|--help) sed -n "2,/^set -euo/{/^#/s/^# \{0,1\}//p}" "$0"; exit 0;; esac
. "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")/../../../scripts/lib/build-id.sh"
build_flags "${BASH_SOURCE[0]}" "$@"
set -euo pipefail
IN=$1; OUT=$2
URL=http://127.0.0.1:4811/v1/chat/completions
MODEL=gemma-4-12B-it-qat-UD-Q4_K_XL
NARR='In one dense paragraph, describe what this image is (chart, diagram, photo, table, screenshot, etc.), its title or subject, and the trend or point it makes. Plain prose only, no JSON, no markdown formatting.'
EXTR='Transcribe this image as structured plain text: its title, subtitle, axis labels and ranges, series names, data values, annotations, table content, and component labels, in that order where present. Plain text only, no JSON, no markdown formatting.'
mkdir -p "$OUT"
tmp=$(mktemp -d)
for img in "$IN"/*.png; do
  id=$(basename "$img" .png)
  base64 -w0 "$img" > "$tmp/b64"
  for arm in off on; do
    for pass in narr extr; do
      [ -s "$OUT/$id.$arm.$pass.json" ] && continue
      p=$NARR; [ $pass = extr ] && p=$EXTR
      extra='{}'; [ $arm = off ] && extra='{"reasoning_effort":"none"}'
      jq -n --rawfile img "$tmp/b64" --arg p "$p" --arg m "$MODEL" --argjson x "$extra" \
        '{model:$m,max_tokens:8192,temperature:0,messages:[{role:"user",content:[{type:"text",text:$p},{type:"image_url",image_url:{url:("data:image/png;base64,"+$img)}}]}]} + $x' > "$tmp/req.json"
      s=$(date +%s%3N)
      curl -s -m 600 "$URL" -H 'content-type: application/json' -d @"$tmp/req.json" > "$tmp/resp.json" || echo '{}' > "$tmp/resp.json"
      e=$(date +%s%3N)
      jq --argjson ms $((e - s)) --arg id "$id" --arg arm $arm --arg pass $pass \
        '{id:$id,arm:$arm,pass:$pass,ms:$ms,finish:.choices[0].finish_reason,ct:.usage.completion_tokens,pt:.usage.prompt_tokens,content:.choices[0].message.content,reasoning_chars:((.choices[0].message.reasoning_content // "")|length)}' \
        "$tmp/resp.json" > "$OUT/$id.$arm.$pass.json"
      echo "$id $arm $pass $((e - s))ms $(jq -c '{finish,ct}' "$OUT/$id.$arm.$pass.json")"
    done
  done
done
rm -rf "$tmp"
