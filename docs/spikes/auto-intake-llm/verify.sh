#!/usr/bin/env bash
# verify.sh — anchor fidelity: for each out/<eff>-<task>-<doc>.txt, count anchors (quote/starts_at/
# first_line/last_line/content_ends_after) found verbatim (after whitespace squeeze) in corpus/<doc>.md.
set -euo pipefail
if [ "${1:-}" = "--help" ]; then sed -n 2,3p "$0"; exit 0; fi
cd "$(dirname "$0")"
printf 'eff\ttask\tdoc\tanchors\tfound\tentities\tclaims\tchapters\tspans\tkinds\tscores\n'
for f in out/{low,med}-{structure,cleanup}-*.txt; do
  b=${f#out/}; b=${b%.txt}; eff=${b%%-*}; r=${b#*-}; task=${r%%-*}; doc=${r#*-}
  norm=$(tr -s '[:space:]' ' ' < corpus/$doc.md)
  j=$(sed -e '/^```/d' "$f")
  n=0; ok=0
  while IFS= read -r a; do [ -z "$a" ] && continue; n=$((n+1)); a2=$(printf '%s' "$a" | tr -s '[:space:]' ' '); case "$norm" in *"$a2"*) ok=$((ok+1));; esac; done < <(jq -r '[.key_claims[]?.quote, .chapters[]?.starts_at, .spans[]?.first_line, .spans[]?.last_line, .content_ends_after] | .[] | select(. != null)' <<<"$j")
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$eff" "$task" "$doc" "$n" "$ok" \
    "$(jq '.entities|length? // "-"' <<<"$j")" "$(jq '.key_claims|length? // "-"' <<<"$j")" "$(jq '.chapters|length? // "-"' <<<"$j")" \
    "$(jq '.spans|length? // "-"' <<<"$j")" "$(jq -r '[.spans[]?.kind]|group_by(.)|map("\(.[0])=\(length)")|join(",")' <<<"$j")" "$(jq -c '.scores // "-"' <<<"$j")"
done
