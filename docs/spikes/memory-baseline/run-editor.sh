#!/usr/bin/env bash
# usage: run-editor.sh <label> <daemon-label> <seconds> [ENV=VAL ...] — scratch editor + sampler at 1 s
for _a in "$@"; do case "$_a" in --help|-h)
  printf 'usage: %s %s\n' "${0##*/}" '<label> <daemon-label> <seconds> [ENV=VAL ...]'
  awk 'NR>1 && /^#/ {sub(/^# ?/, ""); print; next} NR>1 {exit}' "$0"
  exit 0;; esac; done
. "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")/../../../scripts/lib/build-id.sh"
build_flags "${BASH_SOURCE[0]}" "$@"

label=$1; dl=$2; secs=$3; shift 3
S=~/.local/share/cv-mem-scratch; D=$(dirname "$0")
cp $S/xdg/crucible-vault/editor.json.orig $S/xdg/crucible-vault/editor.json 2>/dev/null
env -i HOME=$HOME PATH=$PATH DISPLAY=$DISPLAY XAUTHORITY=$XAUTHORITY XDG_RUNTIME_DIR=$XDG_RUNTIME_DIR \
  XDG_STATE_HOME=$S/xdg CV_SOCKET=$S/run/$dl.sock CV_VAULT=$S/vault RUST_LOG=info "$@" \
  <code-root>/crucible-vault/target/release/cvault-editor > $S/run/ed-$label.log 2>&1 &
pid=$!
"$D/sample.sh" "$pid" 1 "$secs" "$D/ed-$label.tsv"
awk '/^[0-9a-f]+-[0-9a-f]+ /{name=$6; if(name=="")name="[anon]"} /^Rss:/{rss[name]+=$2} END{for(n in rss) if(rss[n]>2048) printf "%8d %s\n", rss[n], n}' /proc/$pid/smaps | sort -rn > "$D/ed-$label.maps"
kill "$pid"
