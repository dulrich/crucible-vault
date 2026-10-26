#!/usr/bin/env bash
# usage: fidelity.sh [outfile.tsv]  (run from docs/spikes/pdf-render/; needs target/release/fidelity and vendor/pdfium/lib)
# hayro vs pdfium pixel agreement on page 1 + mid page of every non-.trash live-vault PDF at 96 dpi; aggregates only (sorted index, never names).
set -u
if [ "${1:-}" = "--help" ]; then echo "usage: fidelity.sh [outfile.tsv]"; exit 0; fi
out=${1:-results-fidelity.tsv}
VAULT=${VAULT:-~/vault}
HERE=$(cd "$(dirname "$0")" && pwd)
export PDFIUM_LIB_DIR=$HERE/vendor/pdfium/lib
printf 'idx\tline\n' > "$out"
i=0
while IFS= read -r -d '' f; do
  i=$((i+1))
  line=$(timeout 120 "$HERE/target/release/fidelity" "$f" 1.333333 2>/dev/null | tail -1)
  printf '%s\t%s\n' "$i" "${line:-CRASH}" >> "$out"
done < <(find "$VAULT" -path "$VAULT/.trash" -prune -o -type f -iname '*.pdf' -print0 | sort -z)
