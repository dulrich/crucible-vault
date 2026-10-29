#!/usr/bin/env bash
# usage: run-editor-load.sh <label> <daemon-label> [ENV=VAL ...] — editor + open the 15 scratch notes
# with the most image embeds (paths stay in the scratch dir, never printed); prints aggregate rows.
for _a in "$@"; do case "$_a" in --help|-h)
  printf 'usage: %s %s\n' "${0##*/}" '<label> <daemon-label> [ENV=VAL ...]'
  awk 'NR>1 && /^#/ {sub(/^# ?/, ""); print; next} NR>1 {exit}' "$0"
  exit 0;; esac; done
. "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")/../../../scripts/lib/build-id.sh"
build_flags "${BASH_SOURCE[0]}" "$@"

label=$1; dl=$2; shift 2
S=~/.local/share/cv-mem-scratch; sock=$S/run/$dl.sock
[ -f $S/run/imgnotes.txt ] || (cd $S/vault && grep -rcE '!\[\[[^]]*\.(png|jpe?g|webp|gif)|!\[[^]]*\]\([^)]*\.(png|jpe?g|webp|gif)' --include='*.md' . | sort -t: -k2 -rn | head -15 | cut -d: -f1 | sed 's|^\./||' > $S/run/imgnotes.txt)
cp $S/xdg/crucible-vault/editor.json.orig $S/xdg/crucible-vault/editor.json
env -i HOME=$HOME PATH=$PATH DISPLAY=$DISPLAY XAUTHORITY=$XAUTHORITY XDG_RUNTIME_DIR=$XDG_RUNTIME_DIR \
  XDG_STATE_HOME=$S/xdg CV_SOCKET=$sock RUST_LOG=info "$@" \
  <code-root>/crucible-vault/target/release/cvault-editor > $S/run/edload-$label.log 2>&1 &
P=$!; sleep 8
row(){ printf '%s\t%s\t' "$label" "$1"; awk '/^Rss:/{r=$2}/^Pss_Anon:/{a=$2}/^Pss_File:/{f=$2}END{printf "%d\t%d\t%d\t",r,a,f}' /proc/$P/smaps_rollup; awk '{printf "%d\t",($14+$15)/100}' /proc/$P/stat; awk '/VmSwap/{printf "%d\n",$2}' /proc/$P/status; }
row idle
n=0; while read -r f; do n=$((n+1)); enc=$(python3 -c 'import sys,urllib.parse;print(urllib.parse.quote(sys.argv[1]))' "$f"); CV_SOCKET=$sock cv uri "obsidian://open?vault=vault&file=$enc" >/dev/null 2>&1; sleep 3; [ $n = 5 ] && row open5; done < $S/run/imgnotes.txt
sleep 3; row open15; sleep 30; row open15+30s
if [ -n "$MSHIM" ]; then echo stats > $MSHIM; sleep 1; echo trim > $MSHIM; sleep 2; row trimmed; echo stats > $MSHIM; sleep 1; fi
kill $P
