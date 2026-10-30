#!/usr/bin/env bash
# run-all.sh — terra low vs medium × {structure, cleanup} × corpus/*.md (20 calls, serial: parallel calls on one codex persona supersede each other; P=codex-profile-a default).
# usage: ./run-all.sh  (re-execs under `host-env run router-client`)      (results.tsv + out/<effort>-<task>-<doc>.{json,txt})
set -euo pipefail
[ -n "${FLEET_ENV_RUN:-}" ] || exec host-env run router-client -- "$0" "$@"
if [ "${1:-}" = "--help" ]; then sed -n 2,3p "$0"; exit 0; fi
P=${P:-codex-profile-a}; cd "$(dirname "$0")"; mkdir -p out
for d in corpus/*.md; do b=$(basename "$d" .md); for t in structure cleanup; do
  ./llm.sh $P/gpt-6.1-terra        prompts/$t.md "$d" out/low-$t-$b
  ./llm.sh $P/gpt-6.1-terra-medium prompts/$t.md "$d" out/med-$t-$b; done; done
column -t -s $'\t' results.tsv
