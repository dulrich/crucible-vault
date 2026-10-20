#!/usr/bin/env bash
# usage: scenario.sh <label> [ENV=VAL ...] — warm start on index-cold-default.db, then a fixed workload; prints one row per phase
label=$1; shift; S=~/.local/share/cv-mem-scratch; D=$(dirname "$0"); sock=$S/run/$label.sock
cp $S/state/index-cold-default.db $S/state/index-$label.db 2>/dev/null
env -i HOME=$HOME PATH=$PATH CV_VAULT=$S/vault CV_SOCKET=$sock CV_STATE_DB=$S/state/state.db \
  CV_INDEX_DB=$S/state/index-$label.db CV_SEARCH_DB=$S/state/search.db RUST_LOG=info "$@" \
  <code-root>/crucible-vault/target/release/cv daemon > $S/run/$label.log 2>&1 &
P=$!; echo $P > $S/run/$label.pid
until [ -S $sock ] && grep -q 'daemon serving' $S/run/$label.log; do sleep 0.5; done; sleep 3
row(){ printf '%s\t%s\t' "$label" "$1"; awk '/^Rss:/{r=$2}/^Pss_Anon:/{a=$2}END{printf "%d\t%d\t",r,a}' /proc/$P/smaps_rollup; awk '/^[0-9a-f]+-/{n=$6}/^Size:/{z=$2}/^Rss:/{if(n==""&&z>=65000&&z<=65540)a+=$2; if(n=="[heap]")h+=$2}END{printf "%d\t%d\t",a,h}' /proc/$P/smaps; awk '/VmHWM/{h=$2}/VmSwap/{w=$2}END{printf "%d\t%d\n",h,w}' /proc/$P/status; }
run(){ CV_SOCKET=$sock cv run "$1" >/dev/null; sleep 1; until CV_SOCKET=$sock cv jobs list --status running | grep -qx "no jobs" && CV_SOCKET=$sock cv jobs list --status queued | grep -qx "no jobs"; do sleep 1; done; sleep 2; }
row start
for q in "project planning" "rust memory allocator" "garden notes" "travel"; do "$D/rpc.py" $sock search "{\"query\":\"$q\",\"limit\":50}" >/dev/null; done; sleep 1; row search4
run search-refresh; row refresh1
run search-refresh; run search-refresh; row refresh3
run link-scan-all; row linkscan
for i in $(seq 20); do "$D/rpc.py" $sock search "{\"query\":\"q$i notes\",\"limit\":50}" >/dev/null; done; row search24
kill $P; wait $P 2>/dev/null
