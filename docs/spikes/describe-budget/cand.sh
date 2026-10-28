#!/usr/bin/env bash
# Candidate settings run: sampler = Unsloth-recommended (t 1.0, top_p 0.95, top_k 64) + repeat_penalty 1.15/256.
# Arms: off (reasoning_effort none; caps narr 512 / extr 2048), on (reasoning default; caps 4096 both).
# Usage: cand.sh <in-dir> <out-dir> <arms...>
case "${1:-}" in -h|--help) sed -n "2,/^set -euo/{/^#/s/^# \{0,1\}//p}" "$0"; exit 0;; esac
set -euo pipefail
IN=$1; OUT=$2; shift 2
URL=http://127.0.0.1:4811/v1/chat/completions
NARR='In one dense paragraph, describe what this image is (chart, diagram, photo, table, screenshot, etc.), its title or subject, and the trend or point it makes. Plain prose only, no JSON, no markdown formatting.'
EXTR='Transcribe this image as structured plain text: its title, subtitle, axis labels and ranges, series names, data values, annotations, table content, and component labels, in that order where present. Plain text only, no JSON, no markdown formatting.'
S='{"temperature":1.0,"top_p":0.95,"top_k":64,"repeat_penalty":1.15,"repeat_last_n":256}'
mkdir -p "$OUT"; tmp=$(mktemp -d)
for img in "$IN"/*.png; do
  id=$(basename "$img" .png); base64 -w0 "$img" > "$tmp/b64"
  for arm in "$@"; do for pass in narr extr; do
    p=$NARR; [ $pass = extr ] && p=$EXTR
    if [ $arm = off ]; then cap=512; [ $pass = extr ] && cap=2048; x='{"reasoning_effort":"none"}'; else cap=4096; x='{}'; fi
    jq -n --rawfile img "$tmp/b64" --arg p "$p" --argjson cap $cap --argjson s "$S" --argjson x "$x" '{model:"gemma-4-12B-it-qat-UD-Q4_K_XL",max_tokens:$cap,messages:[{role:"user",content:[{type:"text",text:$p},{type:"image_url",image_url:{url:("data:image/png;base64,"+$img)}}]}]} + $s + $x' > "$tmp/req.json"
    s=$(date +%s%3N); curl -s -m 600 "$URL" -H 'content-type: application/json' -d @"$tmp/req.json" > "$tmp/r.json"; e=$(date +%s%3N)
    jq --argjson ms $((e-s)) --arg id $id --arg arm $arm --arg pass $pass '{id:$id,arm:$arm,pass:$pass,ms:$ms,finish:.choices[0].finish_reason,ct:.usage.completion_tokens,content:.choices[0].message.content,reasoning_chars:((.choices[0].message.reasoning_content // "")|length)}' "$tmp/r.json" > "$OUT/$id.$arm.$pass.json"
  done; done
done; rm -rf "$tmp"
