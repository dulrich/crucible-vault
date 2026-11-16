# S2 late arena growth on the real daemon — diagnosis (2026-11-16, plan idle-release-threshold-and-arena-growth WP-1)

Aggregates only. Scenario-A blocks on the research scratch vault copy; no note text, titles or paths. Daemon logs stayed in `/tmp/`, outside every repo.

Basis: `docs/spikes/glibc-mxfast/WP-1-FINDINGS.md` (the S1/S2 split, the harness this spike extends). S2 there: clean at 30 s idle, one thread arena 24–26 MiB resident with 19–24 MiB free-resident by 120 s, trigger unknown, no trim log kept.

Binary: frozen release `cv` 1.1.0 (build `5cd0f5b6…`). Host: 24 cores, kernel 7.2.12+deb13-amd64, transparent huge pages `enabled=[always]`, `defrag=[madvise]`, khugepaged `scan_sleep_millisecs=10000`, `pages_to_scan=4096`, `max_ptes_none=511`. Each block is a fresh daemon in a throwaway `lhmc-bench` container (Debian 13, glibc 2.41-12+deb13u4), unpinned, `--cpu-shares 52`, `--memory 1g`, 4 lanes, arms interleaved by `research/scripts/measure.mjs` (seeded). The daemon runs at `RUST_LOG=info,cv::memory=debug`, so every `trim after idle|job|sampler tick` line (fields `free_resident`, `trimmed`) is logged. A block runs 3 view passes (26 views + 5 searches each), then a request-free idle window read from `/proc` only, every 5 s: RssAnon, and per-arena smaps resident via `arenas.pl` (run E: `arenas2.pl`, which adds `AnonHugePages` per arena). One `cv status` comes after the window. S2 = largest thread arena ≤15 MiB at 30 s and >15 MiB at the end; S1 = largest thread arena >15 MiB (or free-resident >5 MiB) already at 30 s, right after the idle release.

## Summary

- **S2 trigger identified: khugepaged re-collapsing the trimmed arena into transparent huge pages.** The idle trim (`malloc_trim`, which `MADV_DONTNEED`s free pages inside non-main arena heaps) leaves the burst arena's mapping sparse. With THP `always` and `max_ptes_none=511`, khugepaged later collapses each 2 MiB range that still holds one resident page into a full huge page, faulting the freed holes back in. The daemon does nothing: no request, no log line at any enabled level, about 10 ms of thread CPU across the onset window.
- **The evidence is direct.** In run E the regrown arena is entirely `AnonHugePages`: 24–26 MiB AHP in a 24–26 MiB resident arena, in 4 of 4 S2 blocks. Every other block, 76 of 76, shows 0 AHP in its top arena (95 % CI for AHP in a non-S2 block: [0, 0.047]). The growth comes in two steps about 10 s apart, which is khugepaged's `scan_sleep_millisecs`. The daemon with THP disabled for its process (`prctl(PR_SET_THP_DISABLE)`, arm `nothp`) had 0/40 S2 against 4/40 with THP on.
- **The `cv status` probe is not the trigger.** S2 appears in the arm with no request at all in the idle window (run A `quiet` 2/18, runs D+E `quiet` 6/70). The run-B correlation with the 30 s probe was chance.
- **No trim follows the growth, and none can.** Each S2 block logs exactly one `trim after idle`, at 20.6–21.2 s (free_resident 18.3–22.6 MiB, trimmed=true), *before* the growth. The regrowth starts 50–125 s later. By then the gate has spent its once-per-period release, and the sampler tick (300 s) is outside the window. A later trim would only reopen the cycle, because the collapsed pages are free chunks inside a live arena heap that khugepaged would collapse again.
- **S1 on 1.1.0: 14 blocks of 191** (95 % CI [0.041, 0.120]). In every S1 block the main heap at pass end is small (4.1–14.5 MiB against 21.5 MiB normal): the burst's working set landed in a thread arena. The idle attempt then saw free-resident under 16 MiB, so no trim line appears. That fits the plan's S1 mechanism. THP plays no part in S1: it occurs with THP off too (2/40 in `nothp`), with 0 AHP.

## Runs

| run | design | blocks | result |
|---|---|---|---|
| pilot | 1 `quiet` + 1 `s30d`, sequential | 2 | 1 normal; 1 S1 (top arena 25.3 MiB, free-res 6.9 / 7.2 MiB) |
| A | `s30` (one `cv status` at 30 s), `s30d` (same, daemon at `RUST_LOG=debug`), `quiet` (no request until 150 s); 150 s idle; 4 lanes, stopped after 51 blocks | 51 | S2: quiet 2/18, s30 0/16, s30d 1/17; S1: 1 (s30d) |
| D | `quiet` vs `pre60` (daemon idles 60 s after start-up before the passes, to tell start-relative from pass-relative timing); 200 s idle; per-thread CPU ticks every 5 s; stopped after 58 blocks | 58 | S2: quiet 2/30, pre60 0/28; S1: 7 (4 quiet, 3 pre60) |
| E | `quiet` vs `nothp` (daemon started under `prctl(PR_SET_THP_DISABLE, 1)`, checked via `THP_enabled: 0` in `/proc/<pid>/status`); 200 s idle; per-arena AnonHugePages; 4 lanes × 10 rounds | 80 | S2: quiet 4/40, nothp 0/40; S1: 5 (3 quiet, 2 nothp) |

Rates (Clopper–Pearson 95 %):

| quantity | k / n | 95 % CI |
|---|---|---|
| S2, THP on, all arms (A + D + E) | 9 / 149 | [0.028, 0.112] |
| S2, run E `quiet` | 4 / 40 | [0.028, 0.237] |
| S2, run E `nothp` | 0 / 40 | [0, 0.088] |
| S2, run A `s30` / `s30d` / `quiet` | 0/16, 1/17, 2/18 | [0, 0.206], [0.001, 0.287], [0.014, 0.347] |
| S2, run D `quiet` / `pre60` | 2/30, 0/28 | [0.008, 0.221], [0, 0.123] |
| top-arena AHP > 0 in a non-S2 block (run E) | 0 / 76 | [0, 0.047] |
| S1, all blocks | 14 / 191 | [0.041, 0.120] |

The E rate pair alone is not significant (Fisher one-sided p ≈ 0.06). The mechanism rests on the AHP readings: all regrown memory is huge pages, and no other block has any. `nothp` costs nothing visible in anon: the end-of-window median is 60.4 MiB in both arms, p90 60.8 (nothp) against 71.7 (quiet), max 66.8 against 81.7. View latency and pass CPU were not measured in this spike.

Block wall time (lane-time, 4 lanes in parallel): quiet 88 blocks at 206 s mean, nothp 40 at 216 s, pre60 28 at 276 s, s30/s30d 33 at 166 s. About 11 h of lane-time and about 3.5 h wall in total.

## Per-S2-block evidence (aggregates only)

Times are seconds after the last request (end of the passes). The observed trim is the log line. The inferred idle-attempt window is 20–25 s after the last activity, at 5 s poll granularity. In `s30d` the 30 s `cv status` re-arms the gate, so a second attempt falls at 50–55 s. It logged no line, which fits free-resident being under 16 MiB then (the probe read 0.0 MiB). A missing line is not a missing attempt. "Log at onset" covers every line at the enabled level from 40 s to the window end.

| run / arm | observed trim (t, free_resident MiB, trimmed) | inferred idle attempts | top arena 30 s → end (MiB) | growth steps (window: +MiB) | AHP in top arena at end | free-res at end (MiB) | log at onset |
|---|---|---|---|---|---|---|---|
| A quiet | 21.2 s, 22.5, true | 20–25 s | 6.7 → 26.0 | 125–130: +12.0, 135–140: +7.3 | not read | 23.4 | none (only shutdown at 150 s) |
| A quiet | 20.6 s, 22.6, true | 20–25 s | 6.9 → 24.1 | 120–125: +5.6, 130–135: +11.7 | not read | 21.5 | none |
| A s30d (debug) | 20.7 s, 22.3, true | 20–25 s, 50–55 s | 6.8 → 24.2 | 120–125: +5.7, 130–135: +11.7 | not read | 21.7 | none (`cv::server` hello at 30 s and 150 s only) |
| D quiet | 21.2 s, 22.3, true | 20–25 s | 6.8 → 24.1 | 130–135: +4.4, 140–145: +12.9 | not read | 21.4 | none |
| D quiet | 21.1 s, 18.3, true | 20–25 s | 6.8 → 26.0 | 140–145: +10.3, 150–155: +9.0 | not read | 20.2 | none |
| E quiet | 20.9 s, 22.0, true | 20–25 s | 6.8 → 24.2 | 80–85: +12.8, 90–95: +3.7 | 24 MiB | 22.1 | none |
| E quiet | 20.8 s, 21.7, true | 20–25 s | 6.7 → 26.0 | 75–80: +6.7, 85–90: +12.6 | 26 MiB | 22.9 | none |
| E quiet | 20.8 s, 22.3, true | 20–25 s | 6.9 → 26.0 | 95–100: +10.1, 105–110: +9.1 | 26 MiB | 24.8 | none |
| E quiet | 20.9 s, 22.4, true | 20–25 s | 6.7 → 24.1 | 70–75: +5.8, 80–85: +11.7 | 24 MiB | 22.8 | none |

In every S2 block the regrown arena is the one that held the burst. Its resident size goes back to its pre-trim size (24–26 MiB at pass end, 6.7–6.9 after the trim). No thread arena is created and no thread is spawned at the onset: the thread count follows the same 12/13/14 cycle in S2 and normal blocks. Per-thread CPU over each onset window (run D) is ≤1 tick (10 ms) on one runtime worker. The `notify-rs` inotify thread shows about 110 ms bursts every 30–50 s in *every* block, S2 or not, at times unrelated to the onsets, so it is excluded. Onset timing tracks neither daemon start nor pass end tightly (75–145 s after the trim, earlier in run E), as expected of khugepaged's host-wide scan position. `pre60` was meant to separate the two clocks, but it saw no S2 in 28 blocks, so it gave no timing answer.

Excluded, with evidence: the 30 s `cv status` (S2 in request-free `quiet` blocks); any logged daemon activity (no line at info, or at debug in `s30d`, between the trim and the onset); a daemon timer such as the config poll, mail watch, scheduler tick or watcher (≤10 ms thread CPU in the onset window, no new thread, no new arena, and the regrowth is huge pages, which the daemon cannot produce by touching freed chunks at that CPU cost); start-up of a parallel lane's daemon (in 2 of the 4 run A/D S2 blocks checked, no other lane's daemon started within 20 s before the onset).

## Implications (for a follow-up plan, not decided here)

1. **Fix: disable THP for the daemon process.** Sized: one `cv_malloc` function (for example `disable_thp()`) doing `prctl(PR_SET_THP_DISABLE, 1, 0, 0, 0)` in one `unsafe` block with a `SAFETY:` comment, a no-op off Linux. That is about 15–25 lines with a unit test that reads `THP_enabled: 0` back from `/proc/self/status`, plus a one-line call first thing in the daemon's `main` next to the existing arena setup. The flag is per process and inherited, so it covers every arena. The editor may want the same, because the editor's trim pattern is the same, but that is unmeasured. Alternatives, weaker: `MADV_NOHUGEPAGE` on each arena heap (needs arena address discovery; glibc does not expose it), or a host-wide THP `madvise` setting (out of the repo's scope; systemd 255 has no per-unit THP directive).
2. **What to measure after:** a scenario-A A/B of 1.1.x against the fix build, ≥40 blocks per arm with 200 s request-free idle, recording top-arena resident + AnonHugePages (`arenas2.pl`). Pass criteria: 0 AHP in every block, S2 0/n, and view p95 and pass CPU within noise. THP could have been helping hot paths, and this spike did not measure that.
3. **Trimming later or more often would not fix S2.** The collapsed pages are free chunks in a live arena heap. Another trim would release them again, and khugepaged would refill them on its next pass.
4. **S1 is unchanged by THP** (2/40 with THP off) and stays with the plan's threshold fix (WP-2). The S1 free-resident values seen here sit 1.5–15.7 MiB below the 16 MiB gate, as listed below.

S1 blocks on 1.1.0 (free-resident MiB; a pair is at 30 s / end, a single value is end only, because the request-free arms read it only at the end): pilot 6.9 / 7.2; A 11.9 / 12.2; D 7.4, 14.6, 4.7, 8.7, 15.7, 9.0, 13.1; E 7.0 (nothp), 10.1 (nothp), 7.9, 1.5, 14.8. In all 14 the top thread arena is 24.7–26.1 MiB at 30 s and no trim line is logged.

## Reproduce

The harness ran from a throwaway scratch directory (`<scratch>`) holding the scripts below plus `bin/cv`, a copy of the 1.1.0 release binary. It needs the research repo's `live-host-measurement-confounds/` (`client.pl`, the `var/scratch/` vault copy, the `lhmc-bench` image) and `research/scripts/measure.mjs`. `cvshim.sh` and `arenas.pl` (runs A and D) are inlined in `docs/spikes/glibc-mxfast/WP-1-FINDINGS.md`. Per-block output lands in `/tmp/cvalg/<arm>.<id>/` (`daemon.log`, `series.txt`, `threads.txt`, `meta.txt`).

```
for l in 1 2 3 4; do bun …/research/scripts/measure.mjs run --config lane-$l.json --out lane-$l --host local & sleep 47; done; wait     # A (block.sh)
for l in 1 2 3 4; do bun …/measure.mjs run --config d-lane-$l.json --out d-lane-$l --host local & sleep 41; done; wait                  # D (BLOCK=block2.sh)
for l in 1 2 3 4; do bun …/measure.mjs run --config e-lane-$l.json --out e-lane-$l --host local & sleep 43; done; wait                  # E (BLOCK=block3.sh)
sh classify.sh /tmp/cvalg/<run>/*.*                                                                                                       # classification
```

Runs A and D were stopped early, by killing `measure.mjs` and the running containers. Incomplete blocks (no final `fr150` in `meta.txt`) were discarded, 8 in total. Blocks were classified after the fact with `classify.sh`. The in-block `MEASURE arenatop1_*`, `s2` and `onset_ms` values of `block.sh` (run A) read column 5 (the second-largest arena) by mistake. The raw `series.txt` is unaffected, and `classify.sh` reads the correct column 4. `block2.sh` and `block3.sh` read column 4.

### Appendix: harness

`run-block.sh`

```
#!/bin/sh
# run-block.sh <s30|quiet> - one block in a throwaway low-priority lhmc-bench container; keeps the
# block's daemon log + idle series in a fresh /tmp/cvalg/<id>/ dir (outside every repo).
set -eu
H=$(cd "$(dirname "$0")" && pwd); L=<code-root>/research/live-host-measurement-confounds
a=$1; lg=info,cv::memory=debug
case ${1:?arm} in s30|quiet) ;; pre60) ;; nothp) a=quiet; NOTHP=1 ;; s30d) a=s30; lg=debug,h2=info,hyper=info,hyper_util=info,rustls=info,reqwest=info ;; *) echo "arm?" >&2; exit 2 ;; esac
mkdir -p /tmp/cvalg; blk=$(mktemp -d "/tmp/cvalg/$1.XXXXXX"); chmod 777 "$blk"
echo "MEASURE_DIR $blk" >&2
docker run --rm --cpu-shares 52 --memory 1g -e ARM="$a" -e LOG="$lg" -e NOTHP="${NOTHP:-0}" -e PASSES="${PASSES:-3}" \
  -v "$L:/lhmc:ro" -v "$H:/spike:ro" -v "$H/bin:/cvbin:ro" -v "$L/var/scratch:/scratch:ro" -v "$blk:/blk" \
  lhmc-bench sh /spike/${BLOCK:-block.sh}
```

`block3.sh` (run E). `block2.sh` (run D) is identical except: `BIN=/spike/cvshim.sh`, `arenas.pl` in place of `arenas2.pl`, and the `THP_enabled` line replaced by `[ "$ARM" != pre60 ] || sleep 60`. `block.sh` (run A) is `block2.sh` without `meta0.txt`/`threads.txt`, with the loop to `k -le 30` (150 s), an extra `if [ $k = 6 ] && [ "$ARM" = s30 ]; then fr30=$(fr); echo "status30_ms $(( $(now_ms) - tp ))" >>/blk/meta.txt; fi` as the first line of the loop body, `fr150=$(fr)` without the `fr30` reset, readings `at 150`, and the column-5 slip noted above.

```
#!/bin/sh
# 3 view passes, then a request-free idle window of 200 s read from /proc only every 5 s (RssAnon + arenas2.pl;
# per-thread utime+stime ticks); one `cv status` at the end.
# Keeps /blk/daemon.log, /blk/series.txt (ms-after-pass-end anon_kib main top1 top2 sum n main_ahp top1_ahp sum_ahp), /blk/meta.txt.
set -eu
S=/scratch; BIN=/spike/cvshim2.sh; R=/blk/work; D=/lhmc; PASSES=${PASSES:-3}; ARM=${ARM:?}
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
k=0; fr30=-1
while [ $k -le 40 ]; do
  w=$(( k * 5000 - ( $(now_ms) - tp ) )); [ $w -le 0 ] || sleep "$(awk -v w="$w" 'BEGIN{printf "%.3f", w/1000}')"
  printf '%s %s %s\n' "$(( $(now_ms) - tp ))" "$(awk '/^RssAnon/{print $2}' "/proc/$pid/status")" "$(perl /spike/arenas2.pl "$pid")" >>/blk/series.txt
  e=$(( $(now_ms) - tp )); for f in /proc/$pid/task/*/stat; do sed -E "s/^([0-9]+) \((.*)\) \S+ (\S+ ){10}(\S+) (\S+).*/$e \1 \2 \4 \5/" "$f" 2>/dev/null; done | tr " " "\t" >>/blk/threads.txt
  k=$((k + 1))
done
fr150=$(fr); fr30=-1
echo "fr30 ${fr30:--1}" >>/blk/meta.txt; echo "fr150 ${fr150:--1}" >>/blk/meta.txt
cp "$R/requests.txt" /blk/requests.txt
mib() { awk -v k="$1" 'BEGIN{printf "%.2f", k/1024}'; }
at() { awk -v t="$1" '$1>=t*1000-2500{print $'"$2"'; exit}' /blk/series.txt; }
a30=$(at 30 4); a150=$(at 200 4); m=$(awk '{if($4>m)m=$4} END{print m+0}' /blk/series.txt)
on=$(awk '$1>=30000 && $4>15360 {print $1; exit}' /blk/series.txt)
echo "MEASURE anon_i30_mib $(mib "$(at 30 2)") MiB"
echo "MEASURE anon_i150_mib $(mib "$(at 200 2)") MiB"
echo "MEASURE arenatop1_i30_mib $(mib "$a30") MiB"
echo "MEASURE arenatop1_i150_mib $(mib "$a150") MiB"
echo "MEASURE arenatop1_max_mib $(mib "$m") MiB"
echo "MEASURE freeres_i30_mib ${fr30:--1} MiB"
echo "MEASURE freeres_i150_mib ${fr150:--1} MiB"
echo "MEASURE s2 $(awk -v a="$a30" -v b="$a150" 'BEGIN{print (b>15360 && a<=15360)?1:0}')"
echo "MEASURE onset_ms ${on:--1} ms"
echo "MEASURE trims $(grep -c 'trim after' /blk/daemon.log || true)"
echo "MEASURE errors $(( $(grep -cE 'ERROR|panicked' /blk/daemon.log || true) + $(grep -c ' err$' /blk/requests.txt || true) ))"
```

`cvshim2.sh`

```
#!/bin/sh
# cvshim2.sh - with NOTHP=1, starts the daemon with prctl(PR_SET_THP_DISABLE=41, 1) (x86_64 SYS_prctl=157; inherited
# across execve, so it covers the daemon only); every other cv call passes through.
set -eu
if [ "${1:-}" = daemon ] && [ "${NOTHP:-0}" = 1 ]; then exec perl -e 'syscall(157, 41, 1, 0, 0, 0) == 0 or die "prctl: $!"; exec @ARGV or die' /cvbin/cv "$@"; fi
exec /cvbin/cv "$@"
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

`classify.sh`

```
#!/bin/sh
# classify.sh <dir>... - one line per block: arm class top1@30 top1@end max fr30 frEnd onset_s trims(kind:t_s:free_res_mib:trimmed) dir
set -eu
for d in "$@"; do
  [ -s "$d/series.txt" ] && [ -s "$d/meta.txt" ] || continue
  tp=$(awk '$1=="tp_epoch_ms"{print $2}' "$d/meta.txt"); fr30=$(awk '$1=="fr30"{print $2}' "$d/meta.txt"); fr150=$(awk '$1=="fr150"{print $2}' "$d/meta.txt")
  arm=$(basename "$d" | cut -d. -f1)
  set -- $(awk '$1>=27500&&!a{a=$4} {b=$4} $4>m&&$1>=30000{m=$4} $1>=30000&&$4>15360&&!o{o=$1} END{printf "%.1f %.1f %.1f %s", a/1024, b/1024, m/1024, (o?o/1000:-1)}' "$d/series.txt")
  trims=$(sed 's/\x1b\[[0-9;]*m//g' "$d/daemon.log" | grep 'trim ' | while read -r ts rest; do
    e=$(date -d "$ts" +%s%3N); f=$(echo "$rest" | grep -Eo 'free_resident=[0-9]+' | cut -d= -f2); t=$(echo "$rest" | grep -Eo 'trimmed=[a-z]+' | cut -d= -f2)
    k=$(echo "$rest" | grep -Eo 'after (idle|job)|sampler' | tr -d ' ')
    printf '%s:%.1fs:%.1f:%s,' "$k" "$(awk -v a="$e" -v b="$tp" 'BEGIN{print (a-b)/1000}')" "$(awk -v f="$f" 'BEGIN{print f/1048576}')" "$t"; done)
  c=N; awk -v a="$1" -v b="$2" -v f="$fr30" 'BEGIN{exit !(a>15 || f>5)}' && c=S1
  [ $c = N ] && awk -v a="$1" -v b="$2" 'BEGIN{exit !(b>15 && a<=15)}' && c=S2
  echo "$arm $c $1 $2 $3 $fr30 $fr150 $4 ${trims:-none} $(basename "$d")"
done
```

`e-lane-1.json` (lanes 2–4 differ only in `seed`, 20261132–20261134; `d-lane-*` use arms `quiet`/`pre60` with `BLOCK=block2.sh`, seeds 2026112N; `lane-*` use arms `s30`/`s30d`/`quiet` without `BLOCK`, 12 invocations, seeds 2026111N)

```
{"variants": [
  {"name": "quiet", "group": "quiet", "argv": ["env", "BLOCK=block3.sh", "sh", "<scratch>/run-block.sh", "quiet"]},
  {"name": "nothp", "group": "nothp", "argv": ["env", "BLOCK=block3.sh", "sh", "<scratch>/run-block.sh", "nothp"]}],
 "invocations": 10, "warmup": 0, "padMax": 0, "baseline": "quiet", "seed": 20261131,
 "metrics": {"s2": {"kind": "rate", "better": "lower"}, "anon_i150_mib": {"kind": "level", "better": "lower"}, "arenatop1_i150_mib": {"kind": "level", "better": "lower"}, "trims": {"kind": "level", "better": "lower"}}}
```
