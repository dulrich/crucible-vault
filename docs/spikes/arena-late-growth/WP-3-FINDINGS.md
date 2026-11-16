# Measuring the 6 MiB quiet-trim threshold build against 1.1.0 (2026-11-16, plan idle-release-threshold-and-arena-growth WP-3)

Aggregates only. Scenario-A blocks on the research scratch vault copy; no note text, titles or paths. Daemon logs stayed in `/tmp/cvwp3/`, outside every repo.

Basis: `docs/spikes/arena-late-growth/WP-1-FINDINGS.md` (the S1/S2 split, the harness reused here and inlined in the appendix). Plan: WP-3 of plan idle-release-threshold-and-arena-growth.

Binaries (frozen, never rebuilt here): `bin-110/cv` = release 1.1.0, `cv --build` `5cd0f5b6d606ebc2fd58fc44fa788ae0b5089a15`; `bin-wp2/cv` = release build of WP-2 commit `03d541d5` (quiet-trim threshold 6 MiB), `cv --build` `03d541d5405e833566690a45efcd159b6f615c89` (clean). Each block records `cv --build` in its `meta.txt`; all 45 `wp2`/24 `wp2p*` blocks read `03d541d5…`, all `a110`/`b110` blocks read `5cd0f5b6…`.

Host: 24 cores, kernel 7.2.12+deb13-amd64, transparent huge pages `enabled=[always]`, `defrag=[madvise]`, khugepaged `scan_sleep_millisecs=10000`, `pages_to_scan=4096`, `max_ptes_none=511`. Each block is a fresh daemon in a throwaway `lhmc-bench` container (Debian 13), `--cpu-shares 52`, unpinned, `--memory 1g`, binary mounted read-only at `/cvbin`, scratch vault read-only. Daemon at `RUST_LOG=info,cv::memory=debug`. Arms interleaved in each lane by `research/scripts/measure.mjs` (seeded). Block: 3 view passes (26 views + 5 searches each), then a 120 s idle window read from `/proc` only every 5 s (RssAnon; per-arena smaps resident and AnonHugePages via `arenas2.pl`), with `cv status` only at 30 s and at 120 s. View latency is the per-request ms from `client.pl`; the per-block view p95 is the nearest-rank 95th percentile of its 78 view requests.

## Summary

- **Criterion 1, S1 mechanism: PASS, exercised.** Three trim lines in the (6 MiB, 16 MiB] band, all `trimmed=true`, each followed by a lower reading (table below): 1 in the 45 plain `wp2` blocks (6.43 MiB at 54.3 s), 2 in the 24 provoking `wp2p1`/`wp2p2` blocks (12.42 MiB at 23.0 s; 14.28 MiB at 51.8 s). Each is a hold that 1.1.0 (16 MiB gate) would have kept. **No in-band line has `trimmed=false`** (all 158 trim lines in the 158 daemon logs are `trimmed=true`). **Zero non-S2 `wp2` blocks hold free-resident above 6 MiB at 120 s** (0/33 non-S2 `wp2` blocks; maximum 4.4 MiB; the same holds in `wp2p1`/`wp2p2`, maximum 4.3 MiB). Caveat in the next bullet.
- **What the in-band evidence is, and is not.** The plain `wp2` arm had no naturally occurring S1 block (0/45; 1.1.0 had 3/89 = 3.4 %, 95 % CI [0.007, 0.095], so about 1.5 expected; not a significant difference). Its one in-band case is a small THP regrowth (top arena 6.8 → 8.5 MiB at 35 s) that the re-armed second idle attempt trimmed. The `PASSES=1` provoking arm (added because the plain arm gave only that one case in 134 blocks, below the "first ~60 blocks give zero" trigger but thin) lands the post-burst residue in the band directly: `wp2p1` block with top arena 22.4 MiB at pass end logs `free_resident=12.42 MiB trimmed=true` at 23.0 s and the arena goes to 6.4 MiB. That one is the S1 mechanism proper. So S1 on the fixed build is confirmed by mechanism (3 cases, 0 failures) and not by a natural-S1 rate, which 45 blocks cannot resolve.
- **Criterion 2, anon and view p95 vs the A/A floor: PASS.** Median anon at 120 s: a110 60.60, b110 60.70, wp2 60.50 MiB. The A/A floor is |a110 − b110| = 0.10 MiB; wp2 − a110 = −0.10 and wp2 − b110 = −0.20 MiB, so wp2 is lower or equal (Mann-Whitney p 0.92 / 0.93; bootstrap CIs include 0). Median per-block view p95: a110 79.3, b110 74.8, wp2 74.0 ms; A/A floor 4.5 ms; wp2 − a110 = −5.3, wp2 − b110 = −0.8 ms (lower; p 0.54 / 0.96).
- **Criterion 3, errors: PASS.** 0 `ERROR`/`panicked` lines in any daemon log and 0 `err` requests, in all 158 blocks (a110 44, b110 45, wp2 45, wp2p1 12, wp2p2 12).
- **S2 is present equally in every arm and is not a threshold effect.** Per block, S2 = top thread arena has AnonHugePages > 0 at 120 s and was ≤15 MiB at 30 s: a110 13/44, b110 11/45, wp2 12/45, wp2p1 2/12, wp2p2 0/12. The rate is far above WP-1's (about 6 %): 36/134 on the three main arms (27 %, CI [0.196, 0.352]); counting only the WP-1 definition (>15 MiB at 120 s) 23/134 (17 %, CI [0.112, 0.246]). The host's khugepaged state differed from WP-1's runs; this spike did not look into why. 0 of the non-S2 blocks has any AHP in its top arena. S2 blocks are excluded from the S1 criteria (36 blocks of the 134 main-arm blocks; 38 of 158 counting the provoking arms); the in-band trim table keeps them because the log lines are facts regardless of class.
- **Control, 1.1.0 S1 blocks: 3 of 89** (a110 2/44, b110 1/45; non-S2, free-resident > 5 MiB at 30 s or 120 s with top arena 24.8–25.3 MiB at 30 s; free-resident 6.1–12.1 MiB at 30 s and 6.2–12.1 at 120 s; no trim line was logged). That is the population the 6 MiB threshold is meant to release. `wp2` had 0/45.

## Runs

| run | design | blocks | wall |
|---|---|---|---|
| pilot | 1 `wp2` block, to check the harness and per-request latency parsing (view p95 was first read from the wrong column; fixed before the main run; pilot discarded) | 1 | 2 m 17 s |
| main | arms `a110`, `b110` (both 1.1.0, the A/A pair), `wp2`; 4 lanes × 9 rounds, seeds 20261131–20261134, `PASSES=3`; see deviations | 134 complete: a110 44, b110 45, wp2 45 | 02:14–03:17, 63 min wall, about 137 s per block |
| provoking | arms `wp2p1` (`PASSES=1`), `wp2p2` (`PASSES=2`), both on the `wp2` binary; 2 lanes × 6 rounds, seeds 20261145, 20261146 | 24: wp2p1 12, wp2p2 12 | 03:19–03:46, 27 min wall |

Total 158 complete blocks, about 5 h of lane-time. Block counts exceed the 36/arm planned because two `measure.mjs` lanes were accidentally run twice for part of the time (see "Deviations").

Rates (Clopper-Pearson 95 %):

| quantity | k / n | 95 % CI |
|---|---|---|
| S2 (AHP > 0, ≤15 MiB at 30 s): a110 / b110 / wp2 | 13/44, 11/45, 12/45 | [0.168, 0.452], [0.129, 0.395], [0.146, 0.419] |
| S2 (>15 MiB at 120 s only): a110 / b110 / wp2 | 8/44, 7/45, 8/45 | [0.082, 0.327], [0.065, 0.295], [0.080, 0.321] |
| S2, wp2p1 / wp2p2 | 2/12, 0/12 | [0.021, 0.484], [0, 0.265] |
| S1 non-S2, 1.1.0 (A/A pooled) | 3 / 89 | [0.007, 0.095] |
| S1 non-S2, wp2 | 0 / 45 | [0, 0.079] |
| top-arena AHP > 0 in a non-S2 block | 0 / 120 | [0, 0.030] |

Medians (all blocks / non-S2 blocks): anon at 120 s a110 60.60 / 60.50, b110 60.70 / 60.50, wp2 60.50 / 60.40 MiB; view p95 a110 79.3 / 83.8, b110 74.8 / 74.9, wp2 74.0 / 74.0 ms. Non-S2 wp2 − a110 anon −0.10 MiB (p 0.85, bootstrap [−0.20, +0.10]), p95 −9.8 ms (p 0.31, [−19.7, +8.8]); wp2 − b110 anon −0.10, p95 −0.95. Free-resident at 120 s (median, every arm): 3.5–3.7 MiB. That residue is above what WP-1 called "0–1.2 MiB trimmed", so it is a point for the 6 MiB choice: the 6 MiB threshold sits above it (maximum non-S2 wp2 value 4.4 MiB, and 4.3 MiB in `wp2p*`), so the sampler would not churn a settled daemon. The provoking arms have median anon 59.9 (`wp2p1`) and 60.5 (`wp2p2`) MiB.

## In-band trim cases (wp2 binary)

Band (6 MiB, 16 MiB], idle or sampler trim lines. `t` is seconds after the last request (end of the passes). Observed lines only; the inferred idle attempts without a line are not in this table. Free-resident is the logged figure, read before the trim. Top-arena MiB values are from the 5 s `/proc` series before and after the line.

| block | t (s) | free_resident (MiB) | trimmed | top arena before → after (MiB) | anon before → after (MiB) | next `cv status` free-resident |
|---|---|---|---|---|---|---|
| `wp2` (plain, THP partial regrowth) | 54.3 (second attempt, after the 30 s `cv status` re-armed the gate; first line at 24.3 s: 22.72 MiB, `trimmed=true`) | 6.43 | true | 8.5 → 6.8 | 63.5 → 56.9 | 3.5 at 120 s (3.6 at 30 s, before the line) |
| `wp2p1` (1 pass) | 23.0 (first attempt) | 12.42 | true | 22.4 → 6.4 | 69.2 → 48.5 | 0.0 at 30 s and 120 s |
| `wp2p1` (1 pass, S2 block) | 51.8 (second attempt; first line at 22.0 s, 16.49 MiB, above the band) | 14.28 | true | 22.2 → 6.5 | 71.0 → 52.8 | 0.2 at 120 s |

Lines outside the band, for context: every other `wp2` first-attempt line is 18.34–27.04 MiB, `trimmed=true`. Of the 1.1.0 first-attempt lines the lowest is 16.21 MiB. No sampler tick (300 s) falls inside the 120 s window, so no `trim after sampler tick` line appears; the sampler half of criterion 1 is the unit-tested threshold, not measured here.

Inferred idle-attempt windows (never treated as lines): first attempt at 20–25 s after the last activity; after the 30 s `cv status` a second at about 50–55 s. In 155 of 158 logs the second attempt left no line (3 blocks have a second line), which fits free-resident being at or below the gate when it fired (`IdleGate::poll` spends the period's release before the threshold check, so a missing line is not a missing attempt).

## S2 counts per arm

| arm | blocks | S2 (AHP>0, ≤15 MiB at 30 s) | S2 and >15 MiB at 120 s | S1 non-S2 | excluded from S1 criteria |
|---|---|---|---|---|---|
| a110 | 44 | 13 | 8 | 2 | 13 |
| b110 | 45 | 11 | 7 | 1 | 11 |
| wp2 | 45 | 12 | 8 | 0 | 12 |
| wp2p1 | 12 | 2 | 0 | 0 | 2 |
| wp2p2 | 12 | 0 | 0 | 0 | 0 |

In S2 blocks free-resident at 120 s is 0.0–23.5 MiB (partial regrowths low, full ones 15–23.5); those are THP-collapsed free chunks (WP-1) and are not a threshold failure. The second idle attempt of S2 blocks logs no line when it fires before the regrowth completes. No S2 block in `wp2` has a trim line after its first one except the cases above (the sampler is outside the window).

## Deviations from the brief

1. **Duplicate lanes.** I started some lanes twice by mistake (`start-lanes` plus an explicit start), so lanes 3 and 4 existed twice for a while: up to 5–6 `measure.mjs` processes and an equal number of containers for the first minutes, and one duplicate lane 4 ran until 03:15, when I killed it by PID. The brief allowed at most 4 concurrent lanes; the higher concurrency lasted about 1 h of the 63 min and was visible in the lane-4 `raw.jsonl` (duplicated entries, not used). The extra blocks come from the same seeded variant order, so arms stay balanced (44/45/45). The analysis reads the per-block output directories, not `raw.jsonl`. Orphan containers of killed clients (2) were stopped or let finish; one orphan's partial dir was deleted, the others finished and are counted.
2. **Statistics.** `measure.mjs` was used to interleave and run, not to report; I computed medians, Mann-Whitney U (normal approximation, ties ranked by midrank) and a seeded 10 000-resample bootstrap of the median difference with `stats.pl` (appendix), because the per-block metrics come from the kept directories (134 main blocks including the duplicate-lane ones).
3. **Optional provoking arm: done.** Added `wp2p1`/`wp2p2` (24 blocks) because the plain arm yielded one in-band case; reported above as a separate mechanism check, not pooled into any rate.
4. **`cv status` timing.** The 30 s read happens right after the 30 s `/proc` sample; its free-resident can be read a few hundred ms after the sample. The pre-passes `drain` status calls (before the passes) are not in the idle window.
5. The in-block `MEASURE s2` metric requires only AHP>0 and ≤15 MiB at 30 s (the brief's definition); WP-1's `>15 MiB at the end` is reported separately above.

## Reproduce

The harness ran from a throwaway scratch directory (`<scratch>`) holding the scripts below plus `bin-110/cv` and `bin-wp2/cv`. It needs the research repo's `live-host-measurement-confounds/` (`client.pl`, the `var/scratch/` vault copy, the `lhmc-bench` image) and `research/scripts/measure.mjs`. Per-block output lands in `/tmp/cvwp3/<arm>.<id>/` (`daemon.log`, `series.txt`, `threads.txt`, `meta.txt`, `requests.txt`).

```
cd <scratch>
sh start-lanes.sh 1 4                                    # main: lane-1..4.json (a110, b110, wp2); one process only, not twice
for l in 5 6; do bun …/research/scripts/measure.mjs run --config prov-lane-$l.json --out prov-lane-$l --host local & sleep 40; done; wait   # provoking arms
sh classify4.sh /tmp/cvwp3/*.* > all.cls                 # per block: arm class top1@30 top1@120 ahp fr30 fr120 anon120 p95 errors passes trims dir
grep -E '^(a110|b110|wp2) ' all.cls > main.cls; perl stats.pl main.cls; perl stats.pl main.cls nos2
perl cp.pl <k> <n>                                       # Clopper-Pearson
```

`classify4.sh` columns: 1 arm, 2 class (S2 / S1 / N), 3 top arena at 30 s (MiB), 4 top arena at 120 s, 5 AnonHugePages of the top arena at 120 s (KiB), 6 free-resident at 30 s, 7 at 120 s, 8 anon at 120 s, 9 view p95 (ms), 10 errors, 11 passes, 12 trim lines (`kind:t:free_resident MiB:trimmed`), 13 directory. S1 = non-S2 and (top arena > 15 MiB at 30 s, or free-resident > 5 MiB at 30 s or 120 s).

### Appendix: harness

Unchanged from WP-1 (`block3.sh` lineage), inlined here complete. `cvshim2.sh` is not used (`BIN=/cvbin/cv`; no THP arm).

`run-block.sh`

```
#!/bin/sh
# run-block.sh <a110|b110|wp2|wp2p1|wp2p2> - one block in a throwaway low-priority lhmc-bench container; keeps the
# block's daemon log + idle series in a fresh /tmp/cvwp3/<id>/ dir (outside every repo).
set -eu
H=$(cd "$(dirname "$0")" && pwd); L=<code-root>/research/live-host-measurement-confounds
lg=info,cv::memory=debug; P=${PASSES:-3}
case ${1:?arm} in a110|b110) bin=bin-110 ;; wp2) bin=bin-wp2 ;; wp2p1) bin=bin-wp2; P=1 ;; wp2p2) bin=bin-wp2; P=2 ;; *) echo "arm?" >&2; exit 2 ;; esac
mkdir -p /tmp/cvwp3; blk=$(mktemp -d "/tmp/cvwp3/$1.XXXXXX"); chmod 777 "$blk"
echo "MEASURE_DIR $blk" >&2
docker run --rm --cpu-shares 52 --memory 1g -e ARM="$1" -e LOG="$lg" -e NOTHP=0 -e PASSES="$P" \
  -v "$L:/lhmc:ro" -v "$H:/spike:ro" -v "$H/$bin:/cvbin:ro" -v "$L/var/scratch:/scratch:ro" -v "$blk:/blk" \
  lhmc-bench sh /spike/block4.sh
```

`block4.sh`

```
#!/bin/sh
# block4.sh - WP-3: 3 view passes, then a 120 s idle window read from /proc only every 5 s (RssAnon + arenas2.pl);
# `cv status` only at 30 s and 120 s. Keeps /blk/daemon.log, series.txt, meta.txt, requests.txt.
set -eu
S=/scratch; BIN=/cvbin/cv; R=/blk/work; D=/lhmc; PASSES=${PASSES:-3}; ARM=${ARM:?}
rm -rf "$R"; mkdir -p "$R/state" "$R/cache" "$R/run" "$R/data" "$R/hm"
cp "$S/ro/search.db" "$R/state/search.db"
export CV_VAULT=$S/pristine CV_CONFIG_DIR=$S/pristine/_custom/crucible-vault CV_STATE_DB=$R/state/state.db \
  CV_SEARCH_DB=$R/state/search.db CV_INDEX_DB=$R/cache/index.db CV_SOCKET=$R/run/cv.sock \
  XDG_DATA_HOME=$R/data XDG_RUNTIME_DIR=$R/run XDG_STATE_HOME=$R/state XDG_CACHE_HOME=$R/cache \
  XDG_CONFIG_HOME=$R/hm/.config HOME=$R/hm CV_LINK_WRITE_ROOTS=
unset CV_ROUTER_GATEWAY_KEY NOTIFY_SOCKET || true
RUST_LOG=${LOG:-info,cv::memory=debug} "$BIN" daemon >/blk/daemon.log 2>&1 &
pid=$!
cleanup() { kill "$pid" 2>/dev/null || true; sleep 1; rm -rf "$R"; }
trap cleanup EXIT INT TERM
i=0
while [ $i -lt 300 ]; do [ -S "$CV_SOCKET" ] && "$BIN" status >/dev/null 2>&1 && break; i=$((i + 1)); sleep 1; done
[ $i -lt 300 ] || { echo "block.sh: daemon did not come up" >&2; exit 1; }
drain() { n=0; while [ $n -lt 900 ]; do
  q=$("$BIN" status 2>/dev/null | grep -Eo '(queued|running)=[0-9]+' | grep -Eo '[0-9]+' | awk '{s+=$1} END{print s+0}') || q=0
  [ "${q:-0}" = 0 ] && break; n=$((n + 1)); sleep 1; done; }
drain; sleep 5
echo "start_epoch_ms $(date +%s%3N)" >/blk/meta0.txt
grep -q "^THP_enabled:.*1" /proc/$pid/status && echo "thp_enabled 1" >>/blk/meta0.txt || echo "thp_enabled 0" >>/blk/meta0.txt
now_ms() { date +%s%3N; }
: >"$R/requests.txt"
pass() { perl "$D/client.pl" "$CV_SOCKET" views "$1" >>"$R/requests.txt"; }
p=0; while [ $p -lt "$PASSES" ]; do pass 5; p=$((p + 1)); done
tp=$(now_ms)
cat /blk/meta0.txt >/blk/meta.txt; echo "tp_epoch_ms $tp" >>/blk/meta.txt
: >/blk/threads.txt
fr() { "$BIN" status 2>/dev/null | awk '/^  daemon:/{for(i=1;i<=NF;i++) if($i=="free-resident"){print $(i+1); exit}}'; }
: >/blk/series.txt
k=0; fr30=-1; fr120=-1
while [ $k -le 24 ]; do
  w=$(( k * 5000 - ( $(now_ms) - tp ) )); [ $w -le 0 ] || sleep "$(awk -v w="$w" 'BEGIN{printf "%.3f", w/1000}')"
  printf '%s %s %s\n' "$(( $(now_ms) - tp ))" "$(awk '/^RssAnon/{print $2}' "/proc/$pid/status")" "$(perl /spike/arenas2.pl "$pid")" >>/blk/series.txt
  e=$(( $(now_ms) - tp )); for f in /proc/$pid/task/*/stat; do sed -E "s/^([0-9]+) \((.*)\) \S+ (\S+ ){10}(\S+) (\S+).*/$e \1 \2 \4 \5/" "$f" 2>/dev/null; done | tr " " "\t" >>/blk/threads.txt
  if [ $k = 6 ]; then fr30=$(fr); fi; if [ $k = 24 ]; then fr120=$(fr); fi
  k=$((k + 1))
done
fr120=${fr120:--1}
echo "fr30 ${fr30:--1}" >>/blk/meta.txt; echo "fr120 ${fr120:--1}" >>/blk/meta.txt; echo "cv_build $(/cvbin/cv --build)" >>/blk/meta.txt; echo "arm $ARM passes $PASSES" >>/blk/meta.txt
cp "$R/requests.txt" /blk/requests.txt
mib() { awk -v k="$1" 'BEGIN{printf "%.2f", k/1024}'; }
at() { awk -v t="$1" '$1>=t*1000-2500{print $'"$2"'; exit}' /blk/series.txt; }
a30=$(at 30 4); a150=$(at 120 4); ah=$(at 120 9); m=$(awk '{if($4>m)m=$4} END{print m+0}' /blk/series.txt)
on=$(awk '$1>=30000 && $4>15360 {print $1; exit}' /blk/series.txt)
echo "MEASURE anon_i30_mib $(mib "$(at 30 2)") MiB"
echo "MEASURE anon_i120_mib $(mib "$(at 120 2)") MiB"
echo "MEASURE arenatop1_i30_mib $(mib "$a30") MiB"
echo "MEASURE arenatop1_i120_mib $(mib "$a150") MiB"
echo "MEASURE arenatop1_max_mib $(mib "$m") MiB"
echo "MEASURE freeres_i30_mib ${fr30:--1} MiB"
echo "MEASURE freeres_i120_mib ${fr120:--1} MiB"
echo "MEASURE s2 $(awk -v a="$a30" -v b="$a150" -v h="$ah" 'BEGIN{print (h>0 && a<=15360)?1:0}')"
echo "MEASURE ahp_top1_i120_kib $ah KiB"
echo "MEASURE view_p95_ms $(awk '$1=="view"{print $3}' /blk/requests.txt | sort -n | awk '{a[NR]=$1} END{i=int(NR*0.95+0.999999); if(i<1)i=1; print (NR?a[i]:-1)}') ms"
echo "MEASURE onset_ms ${on:--1} ms"
echo "MEASURE trims $(grep -c 'trim after' /blk/daemon.log || true)"
echo "MEASURE errors $(( $(grep -cE 'ERROR|panicked' /blk/daemon.log || true) + $(grep -c ' err$' /blk/requests.txt || true) ))"
```

`arenas2.pl`

```
#!/usr/bin/perl
# arenas2.pl <pid> - arenas.pl plus AnonHugePages: main_kib top1_kib top2_kib sum_kib n main_ahp_kib top1_ahp_kib sum_ahp_kib
use strict; use warnings; no warnings "portable";
open my $f, '<', "/proc/$ARGV[0]/smaps" or die; my ($cur, %rss, %ahp, $main, $mainh) = (undef); $main = 0; $mainh = 0;
while (<$f>) {
  if (/^([0-9a-f]+)-([0-9a-f]+) (\S+) \S+ \S+ \S+\s*(.*)$/) { my ($s, $p, $n) = (hex $1, $3, $4);
    $cur = ($n eq '[heap]') ? 'main' : ($p eq 'rw-p' && $n eq '' && $s % (64 << 20) == 0) ? $s : undef; next; }
  next unless defined $cur;
  if (/^Rss:\s+(\d+)/) { if ($cur eq 'main') { $main += $1 } else { $rss{$cur} += $1 } }
  if (/^AnonHugePages:\s+(\d+)/) { if ($cur eq 'main') { $mainh += $1 } else { $ahp{$cur} += $1 } }
}
my @k = sort { $rss{$b} <=> $rss{$a} } keys %rss; my ($sum, $sumh) = (0, 0); $sum += $rss{$_} for @k; $sumh += $ahp{$_} // 0 for @k;
printf "%d %d %d %d %d %d %d %d\n", $main, @k ? $rss{$k[0]} : 0, @k > 1 ? $rss{$k[1]} : 0, $sum, scalar @k, $mainh, @k ? $ahp{$k[0]} // 0 : 0, $sumh;
```

`classify4.sh`

```
#!/bin/sh
# classify4.sh <dir>... - one line per complete block:
# arm class top1@30 top1@120 ahp_top1@120_kib fr30 fr120 anon120 view_p95 errors passes trims(kind:t_s:free_res_mib:trimmed,...) dir
# class: S2 = top arena AHP>0 at 120 s and top1@30<=15 MiB; S1 = (not S2) free-res@30 or @120 >5 MiB or top1@30 >15 MiB; N otherwise
set -eu
for d in "$@"; do
  [ -s "$d/series.txt" ] && grep -q '^fr120 ' "$d/meta.txt" 2>/dev/null || continue
  tp=$(awk '$1=="tp_epoch_ms"{print $2}' "$d/meta.txt"); fr30=$(awk '$1=="fr30"{print $2}' "$d/meta.txt"); fr120=$(awk '$1=="fr120"{print $2}' "$d/meta.txt")
  ps=$(awk '$1=="arm"{print $4}' "$d/meta.txt"); arm=$(basename "$d" | cut -d. -f1)
  set -- $(awk '$1>=27500&&!a{a=$4} $1>=117500&&!b{b=$4; h=$9; n=$2} END{printf "%.1f %.1f %d %.1f", a/1024, b/1024, h, n/1024}' "$d/series.txt")
  p95=$(awk '$1=="view"{print $3}' "$d/requests.txt" | sort -n | awk '{a[NR]=$1} END{i=int(NR*0.95+0.999999); print a[i]}')
  er=$(( $(grep -cE 'ERROR|panicked' "$d/daemon.log" || true) + $(grep -c ' err$' "$d/requests.txt" || true) ))
  trims=$(sed 's/\x1b\[[0-9;]*m//g' "$d/daemon.log" | grep 'trim ' | while read -r ts rest; do
    e=$(date -d "$ts" +%s%3N); f=$(echo "$rest" | grep -Eo 'free_resident=[0-9]+' | cut -d= -f2); t=$(echo "$rest" | grep -Eo 'trimmed=[a-z]+' | cut -d= -f2)
    k=$(echo "$rest" | grep -Eo 'after (idle|job)|sampler' | tr -d ' ')
    printf '%s:%.1fs:%.2f:%s,' "$k" "$(awk -v a="$e" -v b="$tp" 'BEGIN{print (a-b)/1000}')" "$(awk -v f="$f" 'BEGIN{print f/1048576}')" "$t"; done)
  c=N
  if awk -v a="$1" -v h="$3" 'BEGIN{exit !(h>0 && a<=15)}'; then c=S2
  elif awk -v a="$1" -v f="$fr30" -v g="$fr120" 'BEGIN{exit !(a>15 || f>5 || g>5)}'; then c=S1; fi
  echo "$arm $c $1 $2 $3 $fr30 $fr120 $4 $p95 $er $ps ${trims:-none} $(basename "$d")"
done
```

`stats.pl`

```
#!/usr/bin/perl
# stats.pl <cls-file> [nos2] - per-arm n, median anon@120, median per-block view p95, pairwise median differences,
# Mann-Whitney U (normal approx, tie-corrected not applied) and a seeded 10000-resample bootstrap 95% CI of the median difference.
use strict; use warnings; srand(20261116);
my ($f, $mode) = @ARGV; $mode //= ""; my %d;
open my $h, '<', $f or die; while (<$h>) { my @c = split; next if $mode eq "nos2" && $c[1] eq "S2";
  push @{$d{$c[0]}{anon}}, $c[7]; push @{$d{$c[0]}{p95}}, $c[8]; push @{$d{$c[0]}{fr120}}, $c[6]; }
sub med { my @s = sort { $a <=> $b } @_; my $n = @s; $n % 2 ? $s[$n/2] : ($s[$n/2-1] + $s[$n/2]) / 2 }
sub mwu { my ($x, $y) = @_; my @a = ((map { [$_, 0] } @$x), (map { [$_, 1] } @$y)); @a = sort { $a->[0] <=> $b->[0] } @a;
  my ($i, @r) = (0); while ($i < @a) { my $j = $i; $j++ while $j + 1 < @a && $a[$j+1][0] == $a[$i][0]; my $rk = ($i + $j) / 2 + 1; $r[$_] = $rk for $i .. $j; $i = $j + 1; }
  my $r1 = 0; $r1 += $r[$_] for grep { $a[$_][1] == 0 } 0 .. $#a; my ($n1, $n2) = (scalar @$x, scalar @$y);
  my $u = $r1 - $n1 * ($n1 + 1) / 2; my $z = ($u - $n1 * $n2 / 2) / sqrt($n1 * $n2 * ($n1 + $n2 + 1) / 12);
  my $p = 2 * (1 - 0.5 * (1 + erf_(abs($z) / sqrt(2)))); return ($u, $z, $p) }
sub erf_ { my $x = shift; my $t = 1 / (1 + 0.3275911 * $x); my $y = 1 - ((((1.061405429 * $t - 1.453152027) * $t + 1.421413741) * $t - 0.284496736) * $t + 0.254829592) * $t * exp(-$x * $x); return $y }
sub boot { my ($x, $y) = @_; my @dd; for (1 .. 10000) { my @a = map { $x->[rand @$x] } @$x; my @b = map { $y->[rand @$y] } @$y; push @dd, med(@a) - med(@b) } @dd = sort { $a <=> $b } @dd; return ($dd[250], $dd[9749]) }
for my $a (sort keys %d) { printf "%-6s n=%d median anon@120=%.2f MiB  median fr@120=%.2f  median view p95=%.1f ms\n", $a, scalar @{$d{$a}{anon}}, med(@{$d{$a}{anon}}), med(@{$d{$a}{fr120}}), med(@{$d{$a}{p95}}) }
for my $pair (["a110", "b110"], ["wp2", "a110"], ["wp2", "b110"]) { my ($x, $y) = @$pair; next unless $d{$x} && $d{$y};
  for my $m (qw(anon p95)) { my ($u, $z, $p) = mwu($d{$x}{$m}, $d{$y}{$m}); my ($lo, $hi) = boot($d{$x}{$m}, $d{$y}{$m});
    printf "%s - %s  %-4s diff of medians=%+.2f  MWU z=%.2f p=%.3f  bootstrap95=[%+.2f, %+.2f]\n", $x, $y, $m, med(@{$d{$x}{$m}}) - med(@{$d{$y}{$m}}), $z, $p, $lo, $hi } }
```

`cp.pl`

```
#!/usr/bin/perl
# cp.pl k n - Clopper-Pearson 95% CI by bisection on the exact binomial tail
use strict; use warnings; my ($k, $n) = @ARGV;
sub lc_ { my ($n, $k) = @_; my $s = 0; $s += log($_) for 1 .. $n; $s -= log($_) for map { $_ || 1 } 1 .. $k; $s -= log($_) for 1 .. ($n - $k); $s }
sub pmf { my ($i, $p) = @_; return $p == 0 ? ($i == 0 ? 1 : 0) : $p == 1 ? ($i == $n ? 1 : 0) : exp(lc_($n, $i) + $i * log($p) + ($n - $i) * log(1 - $p)) }
sub cdf { my ($k, $p) = @_; my $s = 0; $s += pmf($_, $p) for 0 .. $k; $s }
sub bis { my ($f) = @_; my ($lo, $hi) = (0, 1); for (1 .. 60) { my $m = ($lo + $hi) / 2; $f->($m) ? ($lo = $m) : ($hi = $m) } ($lo + $hi) / 2 }
my $lo = $k == 0 ? 0 : bis(sub { 1 - cdf($k - 1, $_[0]) < 0.025 });
my $hi = $k == $n ? 1 : bis(sub { cdf($k, $_[0]) > 0.025 });
printf "%d/%d  [%.3f, %.3f]\n", $k, $n, $lo, $hi;
```

`start-lanes.sh`

```
#!/bin/sh
# start-lanes.sh <first> <last> - staggered start of measure.mjs lanes (same seeds each time)
for l in $(seq "$1" "$2"); do
  bun <code-root>/research/scripts/measure.mjs run --config lane-$l.json --out lane-$l --host local >lane-$l.log 2>&1 &
  sleep 47
done
wait
```

`lane-1.json` (lanes 2–4 differ only in `seed`, 20261132–20261134)

```
{"variants": [
  {"name": "a110", "group": "a110", "argv": ["sh", "<scratch>/run-block.sh", "a110"]},
  {"name": "b110", "group": "b110", "argv": ["sh", "<scratch>/run-block.sh", "b110"]},
  {"name": "wp2", "group": "wp2", "argv": ["sh", "<scratch>/run-block.sh", "wp2"]}],
 "invocations": 9, "warmup": 0, "padMax": 0, "baseline": "a110", "seed": 20261131,
 "metrics": {"s2": {"kind": "rate", "better": "lower"}, "anon_i120_mib": {"kind": "level", "better": "lower"}, "view_p95_ms": {"kind": "level", "better": "lower"}, "arenatop1_i120_mib": {"kind": "level", "better": "lower"}, "freeres_i120_mib": {"kind": "level", "better": "lower"}, "trims": {"kind": "level", "better": "lower"}, "errors": {"kind": "level", "better": "lower"}}}
```

`prov-lane-5.json` (lane 6: seed 20261146)

```
{"variants": [
  {"name": "wp2p1", "group": "wp2p1", "argv": ["sh", "<scratch>/run-block.sh", "wp2p1"]},
  {"name": "wp2p2", "group": "wp2p2", "argv": ["sh", "<scratch>/run-block.sh", "wp2p2"]}],
 "invocations": 6, "warmup": 0, "padMax": 0, "baseline": "wp2p1", "seed": 20261145,
 "metrics": {"s2": {"kind": "rate", "better": "lower"}, "anon_i120_mib": {"kind": "level", "better": "lower"}, "view_p95_ms": {"kind": "level", "better": "lower"}, "freeres_i120_mib": {"kind": "level", "better": "lower"}, "trims": {"kind": "level", "better": "lower"}, "errors": {"kind": "level", "better": "lower"}}}
```
