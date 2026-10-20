#!/usr/bin/env bash
# usage: sample.sh <pid> <interval_s> <count> <out.tsv>  — aggregates only
pid=$1; iv=$2; n=$3; out=$4
printf 't_s\trss_kb\tpss_anon_kb\tpss_file_kb\thwm_kb\tthreads\theap_kb\tarena_kb\tother_anon_kb\n' > "$out"
t0=$(date +%s)
for ((i=0;i<n;i++)); do
  [ -r /proc/$pid/smaps_rollup ] || break
  r=$(awk '/^Rss:/{r=$2}/^Pss_Anon:/{a=$2}/^Pss_File:/{f=$2}END{print r"\t"a"\t"f}' /proc/$pid/smaps_rollup)
  s=$(awk '/^VmHWM:/{h=$2}/^Threads:/{t=$2}END{print h"\t"t}' /proc/$pid/status)
  m=$(awk '/^[0-9a-f]+-[0-9a-f]+ /{name=$6; if(name=="")name="[anon]"} /^Size:/{sz=$2} /^Rss:/{ if(name=="[heap]")hp+=$2; else if(name=="[anon]"){ if(sz>=65000&&sz<=65540)ar+=$2; else an+=$2}} END{print hp+0"\t"ar+0"\t"an+0}' /proc/$pid/smaps)
  printf '%s\t%s\t%s\t%s\n' "$(( $(date +%s)-t0 ))" "$r" "$s" "$m" >> "$out"
  sleep "$iv"
done
