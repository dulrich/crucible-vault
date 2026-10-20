#!/usr/bin/env bash
# usage: run-daemon.sh <label> <seconds> [ENV=VAL ...] — scratch daemon + sampler at 1 s
label=$1; secs=$2; shift 2
S=~/.local/share/cv-mem-scratch; D=$(dirname "$0")
env -i HOME=$HOME PATH=$PATH CV_VAULT=$S/vault CV_SOCKET=$S/run/$label.sock \
  CV_STATE_DB=$S/state/state.db CV_INDEX_DB=$S/state/index-$label.db CV_SEARCH_DB=$S/state/search.db \
  RUST_LOG=info "$@" <code-root>/crucible-vault/target/release/cv daemon > $S/run/$label.log 2>&1 &
pid=$!
echo "$pid" > $S/run/$label.pid
"$D/sample.sh" "$pid" 1 "$secs" "$D/$label.tsv"
