#!/usr/bin/env bash
# usage: summarize.sh results-<cand>.tsv  -> aggregate stats per dpi
if [ "${1:-}" = "--help" ] || [ $# -lt 1 ]; then echo "usage: summarize.sh results-<cand>.tsv  (median/p95 render ms, RSS, status counts per dpi)"; exit 0; fi
f=$1
for dpi in 96 192; do
  echo "== $f dpi=$dpi"
  awk -F'\t' -v d=$dpi 'NR>1&&$2==d{c[$3]++; n++}END{for(k in c)printf "status %s=%d ",k,c[k]; printf "(of %d)\n",n}' "$f"
  awk -F'\t' -v d=$dpi 'NR>1&&$2==d&&$3=="OK"{print $5; print $6}' "$f" | sort -n | awk '{a[NR]=$1}END{if(NR){printf "page_ms n=%d median=%.1f p95=%.1f max=%.1f\n",NR,a[int((NR+1)/2)],a[int(NR*0.95+0.999)],a[NR]}}'
  awk -F'\t' -v d=$dpi 'NR>1&&$2==d&&$3=="OK"{print $8}' "$f" | sort -n | awk '{a[NR]=$1}END{if(NR){printf "rss_kb n=%d median=%d p95=%d max=%d\n",NR,a[int((NR+1)/2)],a[int(NR*0.95+0.999)],a[NR]}}'
  awk -F'\t' -v d=$dpi 'NR>1&&$2==d&&$3=="OK"{print $7}' "$f" | sort -n | awk '{a[NR]=$1}END{if(NR){printf "wall_ms(per pdf, 2 pages) median=%.1f p95=%.1f\n",a[int((NR+1)/2)],a[int(NR*0.95+0.999)]}}'
  awk -F'\t' -v d=$dpi 'NR>1&&$2==d&&$3=="OK"{t+=2; if(($9+0)<=0)b++; if(($10+0)<=0)b++}END{printf "zero-nonwhite pages (blank, rust only): %d of %d\n",b,t}' "$f"
done
