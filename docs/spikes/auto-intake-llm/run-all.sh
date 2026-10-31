#!/usr/bin/env bash
# run-all.sh — terra low vs medium × {structure, cleanup} × corpus/*.md (20 calls, serial: parallel calls on one codex persona supersede each other; P=codex-profile-a default).
# Usage: ./run-all.sh  (re-execs under `host-env run router-client`)      (results.tsv + out/<effort>-<task>-<doc>.{json,txt})
case "${1:-}" in -h|--help) sed -n "2,/^set -euo/{/^#/s/^# \{0,1\}//p}" "$0"; exit 0;; esac
. "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")/../../../scripts/lib/build-id.sh"
build_flags "${BASH_SOURCE[0]}" "$@"
set -euo pipefail
[ -n "${FLEET_ENV_RUN:-}" ] || exec host-env run router-client -- "$0" "$@"
P=${P:-codex-profile-a}; cd "$(dirname "$0")"; mkdir -p out
for d in corpus/*.md; do b=$(basename "$d" .md); for t in structure cleanup; do
  ./llm.sh $P/gpt-6.1-terra        prompts/$t.md "$d" out/low-$t-$b
  ./llm.sh $P/gpt-6.1-terra-medium prompts/$t.md "$d" out/med-$t-$b; done; done
column -t -s $'\t' results.tsv
