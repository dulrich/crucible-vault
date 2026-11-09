# WP-1 spike — daemon worker threads and glibc malloc knobs

Plan: `plans/round-23-daemon-memory-mail-enrich-fanfic.md` WP-1. Date: 2026-11-07. Binary: installed `cv 0.2.0 (70e36d7…-dirty)`, the live build. Host: 24 cores.

## Verdict (pins WP-3's defaults)

- **`TOKIO_WORKER_THREADS` → 4** (`worker_threads(4)` on the daemon runtime). Workers beyond 4 bought no latency or throughput; workers alone bought no memory.
- **glibc: trim threshold only — `M_TRIM_THRESHOLD = 131072`, arena count left at glibc's default.** Mean RSS −44%, HWM −40% against baseline, with run p95 within the 10% rule. The editor's `cap_arenas()` (`M_ARENA_MAX=2` + trim) is **rejected for the daemon**: it costs +11–19% dupes-scan p95 and +13–33% link-scan-all p95 at every worker count (arena lock contention), so WP-3 needs a trim-only entry point in `cv-malloc`.
- **`max_blocking_threads` → 16.** Observed blocking-pool peak is 4 in every config (= the 3 general + 1 fast job slots); 16 is 4× headroom for un-replayed network/SQLite paths, against tokio's default 512.
- Plan stop condition ("no RSS difference across settings") **not met** — WP-3 proceeds.

## Phase-2 medians across 3 cycles (per-block rows below)

| config (phase 2, 3 cycles) | threads | RSS mean MiB | HWM MiB | malloc retained MiB | dupes-scan run p95 | link-scan-all run p95 | link-scan run p95 | rounds/300 s |
|---|---|---|---|---|---|---|---|---|
| baseline 24/unset | 36 | 176 | 238 | 259 | 4.8 s | 984 ms | 2 ms | 59 |
| 4 workers only | 16 | 171 | 241 | 257 | 4.8 s | 987 ms | 2 ms | 58 |
| 4 w + arena 2 + trim | 16 | 115 | 175 | 109 | 5.4 s (+13%) | 1.2 s (+22%) | 3 ms | 54 |
| 4 w + arena 4 + trim | 16 | 102 | 153 | 144 | 5.0 s (+4%) | 1.0 s (+2–12%) | 2 ms | 58 |
| **4 w + trim only** | 16 | **99** | **143** | 150 | **5.1 s (+6%)** | **1.0 s (+2%)** | 2 ms | 58 |

Phase 1 (arena 2 + trim at 2/4/8/24 workers, 4 cycles) shows the arena-2 latency cost is independent of the worker count: w24 dupes-scan p95 5.4–5.6 s vs baseline 4.7–4.8 s in the same cycles. `malloc retained` (system − in-use) is glibc bookkeeping; RSS/HWM is the decision metric — trim-only retains more on paper and resides least.

Thread names (end of run, `/proc/<pid>/task/*/comm`): 6 named `cv*` threads + 2 `notify-rs inotify` + `tokio-runtime-worker` (workers + blocking pool). Blocking peak = max threads − 8 named − workers = 4 (base 36−8−24, w4 16−8−4, w2 14−8−2).

## Method

- Scratch copy of the vault outside every repo (symlinks and `.git` removed); its `_custom/crucible-vault/`: schedules emptied, every pipeline disabled, `maildir = ""`, every provider `base_url` → `http://127.0.0.1:9/v1` (no outbound calls). 50 fixed notes without remote embeds. Deleted after the spike.
- Each block: fresh state/index DBs, vault restored from pristine, a scratch `cv daemon` with the block's env, replay loop for 300 s: `cv run link-scan-all`, `cv run dupes-scan`, 50× `cv run link-scan --note`, drain the queue, repeat. RSS/HWM/threads + `/proc/pressure/cpu` some-avg10 + running `rustc` count sampled every 15 s; at the end `cv status` (malloc gauges), `cv jobs stats` (per-action run p95), thread names.
- **Deviation from the plan (contended host):** the plan's 6 sequential uncontended 20-min runs were replaced by interleaved 300 s blocks in rotated order — phase 1: 4 cycles × {base, w2, w4, w8, w24 at arena 2 + trim}; phase 2: 3 cycles × {base, w4 only, w4 a2, w4 a4, w4 trim}. Other sessions' builds hit every config roughly equally, so configs are compared against base from the same cycle; blocks with builds running are visible in the `psi10`/`rc` columns (c1-base, c1-w2, c3-w2 and c3-p2base/c3-p2w4trim had builds running). Phase-2 medians are over all 3 cycles — in c3 base and trim-only both ran under builds, so the pairing holds; phase-1 figures quoted above use cycles 2–4 only. The plan's "arena 4 at the chosen count" run became phase 2, which also isolates the trim and worker knobs.
- **Coverage gaps:** `localize-on-edit` has no command starter (`cv run` → `-32602 … has no command starter`), so it never ran in any block (smoke included); `search-refresh` was excluded (the embedder is the shared inference server and a fresh index would re-embed ~8.5k notes). Network-bound actions were not exercised. The 16 h RSS bound and live per-action latency are checked post-deploy (plan Execution (e)) against `live-pre-deploy-{7d,16h}.md` here.
- 20-min vs 300 s: the arena effect appears within 90 s (smoke) and RSS plateaus within the block; long-uptime growth is the post-deploy check.

## Per-block rows

`bash summarize.sh | sort -k1.4` — rnds = replay rounds, thE/thM = threads end/max, rssM = mean RSS MiB, hwm/live/retnd MiB, *95 = per-action run p95, psi10 = mean cpu some-avg10, rc = max concurrent rustc. (`loe95` is empty: see coverage gaps.)

```
c1-base     49  37  37    207    276  32.1  327.1   11.3s    2.6s    5ms         2.63  24
c4-base     58  36  36    169    232  31.6  223.8    4.7s   953ms    2ms         0.00   0
c2-base     58  36  36    174    240  31.6  262.4    4.8s   972ms    2ms         0.00   0
c3-base     58  36  36    179    245  31.6  263.5    4.8s   977ms    2ms         0.01   0
block     rnds thE thM   rssM    hwm  live  retnd dupes95   lsa95   ls95  loe95 psi10  rc
c3-p2base   57  36  36    176    238  31.7  259.1    5.6s    1.0s    2ms         0.10   3
c1-p2base   59  36  36    172    231  31.7  236.8    4.7s   961ms    2ms         0.00   0
c2-p2base   59  36  36    179    242  31.7  262.9    4.8s   984ms    2ms         0.00   0
c1-p2w4a2   53  16  16    113    172  31.6  108.9    5.4s    1.2s    3ms         0.00   0
c3-p2w4a2   54  16  16    115    175  31.7  111.0    5.5s    1.3s    3ms         0.00   0
c2-p2w4a2   55  16  16    118    177  31.6   88.4    5.3s    1.2s    3ms         0.00   0
c3-p2w4a4   58  16  16    100    153  31.6  144.2    5.0s    1.1s    2ms         0.04   3
c2-p2w4a4   58  16  16    102    152  31.6  143.3    5.0s    1.0s    2ms         0.00   0
c1-p2w4a4   58  16  16    105    154  31.6  144.1    4.9s    1.0s    2ms         0.00   0
c2-p2w4only   58  16  16    171    227  31.3  224.0    4.8s   987ms    2ms         0.01   0
c1-p2w4only   58  16  16    171    241  31.4  259.8    4.9s    1.0s    2ms         0.00   0
c3-p2w4only   58  16  16    177    244  31.4  256.9    4.7s   964ms    2ms         0.00   0
c3-p2w4trim   57  16  16    103    159  31.4  150.2    5.4s    1.0s    2ms         0.11   9
c2-p2w4trim   58  16  16     94    143  31.4  152.5    5.1s    1.0s    2ms         0.02   0
c1-p2w4trim   58  16  16     99    143  31.3  129.6    4.8s   985ms    2ms         0.00   0
c1-w24      53  36  36    107    175  31.9  108.6    5.6s    1.2s    3ms         0.00   0
c2-w24      53  36  36    119    178  32.2  107.4    5.5s    1.2s    3ms         0.00   0
c4-w24      54  36  36    117    182  32.1  108.8    5.5s    1.3s    3ms         0.00   0
c3-w24      54  36  36    124    175  32.0  108.8    5.4s    1.3s    3ms         0.00   0
c1-w2       47  14  14    114    171  31.2  109.7    9.0s    1.6s    4ms         2.58  22
c3-w2       51  14  14    118    164  31.2   92.3    6.2s    1.5s    4ms         0.14  10
c2-w2       52  14  14    133    174  31.1  109.5    5.5s    1.2s    3ms         0.00   0
c4-w2       56  14  14    115    172  31.2  108.0    5.3s    1.1s    3ms         0.00   0
c2-w4       52  16  16    112    177  31.7  111.1    5.4s    1.2s    3ms         0.00   0
c1-w4       52  16  16    119    172  31.6  109.7    6.3s    1.3s    3ms         0.21   3
c3-w4       53  16  16    124    174  31.7  108.4    5.4s    1.1s    3ms         0.00   0
c4-w4       56  16  16    122    172  31.6  111.3    5.4s    1.2s    3ms         0.00   0
c1-w8       53  20  20    122    177  32.1  108.9    5.5s    1.2s    3ms         0.12   0
c3-w8       53  20  20    125    175  32.0  109.7    5.6s    1.2s    3ms         0.00   0
c2-w8       54  20  20    114    162  31.9   91.5    5.6s    1.3s    4ms         0.01   0
c4-w8       54  20  20    118    165  31.9   91.2    5.7s    1.2s    3ms         0.03   0
```

## Commands

Smoke: `run.sh smoke 90`. Phase 1: `bash matrix.sh > matrix.log`. Phase 2: `bash matrix2.sh > matrix2.log`. Aggregate: `bash summarize.sh`. Live baseline: `cv jobs stats --since 7d` / `--since 16h` on the live daemon (read-only). Scripts are inlined here verbatim; the `runs/` originals are prunable.

### `run.sh`

```bash
#!/usr/bin/env bash
# WP-1 spike (plan round-23): one scratch-daemon run under a fixed local-only replay.
# usage: run.sh <label> <duration-s> [ENV=VAL ...]   (env applied to the daemon only)
set -euo pipefail
label=$1; dur=$2; shift 2
S=$SCRATCH_BASE/cv-spike-threads
OUT=$REPO/docs/spikes/daemon-threads/out/$label; rm -rf "$OUT"; mkdir -p "$OUT"
# fresh state + index each run; vault restored from pristine
rm -rf "$S/state" "$S/cache" "$S/run" "$S/data" "$S/vault"; mkdir -p "$S/state" "$S/cache" "$S/run" "$S/data"
cp -a "$S/pristine" "$S/vault"
export CV_VAULT=$S/vault CV_CONFIG_DIR=$S/vault/_custom/crucible-vault CV_STATE_DB=$S/state/state.db \
  CV_SEARCH_DB=$S/state/search.db CV_INDEX_DB=$S/cache/index CV_SOCKET=$S/run/cv.sock XDG_DATA_HOME=$S/data \
  XDG_RUNTIME_DIR=$S/run CV_LINK_WRITE_ROOTS=
unset CV_ROUTER_GATEWAY_KEY || true
env "$@" RUST_LOG=warn ~/.cargo/bin/cv daemon >"$OUT/daemon.log" 2>&1 &
pid=$!; echo "$pid" >"$OUT/pid"
for _ in $(seq 120); do [[ -S $S/run/cv.sock ]] && ~/.cargo/bin/cv status >/dev/null 2>&1 && break; sleep 1; done
t0=$(date +%s)
sample() { local t; t=$(( $(date +%s) - t0 ))
  printf '%s %s %s %s %s %s\n' "$t" "$(awk '/^VmRSS/{print $2}' /proc/$pid/status)" "$(awk '/^VmHWM/{print $2}' /proc/$pid/status)" "$(ls /proc/$pid/task | wc -l)" "$(awk '/^some/{sub("avg10=","",$2);print $2}' /proc/pressure/cpu)" "$(pgrep -c rustc || true)" >>"$OUT/samples.txt"; }
( while kill -0 $pid 2>/dev/null; do sample; sleep 15; done ) & samp=$!
# fixed note list (aggregates only: paths never leave this dir; deleted after)
mapfile -t notes <"$S/notes.txt"
end=$(( t0 + dur )); round=0
while (( $(date +%s) < end )); do
  round=$((round+1))
  ~/.cargo/bin/cv run link-scan-all >/dev/null 2>&1 || true
  ~/.cargo/bin/cv run dupes-scan >/dev/null 2>&1 || true
  for n in "${notes[@]}"; do ~/.cargo/bin/cv run link-scan --note "$n" >/dev/null 2>&1 || true; done
  for n in "${notes[@]:0:20}"; do ~/.cargo/bin/cv run localize-on-edit --note "$n" >/dev/null 2>&1 || true; done
  # let the queue drain before next round
  for _ in $(seq 600); do q=$( { ~/.cargo/bin/cv status 2>/dev/null || true; } | grep -Eo "(queued|running)=[0-9]+" | grep -Eo "[0-9]+" | paste -sd+ | bc 2>/dev/null || true); [[ ${q:-0} == 0 ]] && break; sleep 1; done
done
echo "$round" >"$OUT/rounds"
~/.cargo/bin/cv status >"$OUT/status.txt" 2>&1 || true
~/.cargo/bin/cv jobs stats >"$OUT/stats.txt" 2>&1 || true
ls /proc/$pid/task | wc -l >"$OUT/threads_end"
for t in /proc/$pid/task/*; do cat "$t/comm"; done | sed 's/[0-9]*$//' | sort | uniq -c >"$OUT/thread_names"
grep -E '^(VmRSS|VmHWM|RssAnon)' /proc/$pid/status >"$OUT/proc_end"
kill -TERM $pid; wait $pid 2>/dev/null || true; kill $samp 2>/dev/null || true
echo "done $label rounds=$round"
```

### `matrix.sh`

```bash
#!/usr/bin/env bash
# WP-1 matrix driver: contention-robust interleave. 4 cycles x 5 configs x 300 s, order rotated per cycle,
# so host contention (other sessions' builds) lands on every config ~equally; compare paired within a cycle.
# Per-sample cpu pressure + rustc count (samples.txt cols 5-6) mark contaminated blocks.
set -euo pipefail
D=$(cd "$(dirname "$0")" && pwd)
M="MALLOC_ARENA_MAX=2 MALLOC_TRIM_THRESHOLD_=131072"
specs=("base" "w2 TOKIO_WORKER_THREADS=2 $M" "w4 TOKIO_WORKER_THREADS=4 $M" "w8 TOKIO_WORKER_THREADS=8 $M" "w24 TOKIO_WORKER_THREADS=24 $M")
n=${#specs[@]}
for c in 1 2 3 4; do
  for i in $(seq 0 $((n-1))); do
    set -- ${specs[$(( (i + c - 1) % n ))]}; label=$1; shift
    echo "$(date +%T) start c$c-$label"; bash "$D/run.sh" "c$c-$label" 300 "$@"
  done
done
echo "$(date +%T) matrix done"
```

### `matrix2.sh`

```bash
#!/usr/bin/env bash
# WP-1 phase 2: isolate which knob costs latency, at 4 workers. 3 cycles x 5 configs x 300 s, rotated order.
set -euo pipefail
D=$(cd "$(dirname "$0")" && pwd)
T=MALLOC_TRIM_THRESHOLD_=131072
specs=("p2base" "p2w4only TOKIO_WORKER_THREADS=4" "p2w4a2 TOKIO_WORKER_THREADS=4 MALLOC_ARENA_MAX=2 $T" "p2w4a4 TOKIO_WORKER_THREADS=4 MALLOC_ARENA_MAX=4 $T" "p2w4trim TOKIO_WORKER_THREADS=4 $T")
n=${#specs[@]}
for c in 1 2 3; do
  for i in $(seq 0 $((n-1))); do
    set -- ${specs[$(( (i + c - 1) % n ))]}; label=$1; shift
    echo "$(date +%T) start c$c-$label"; bash "$D/run.sh" "c$c-$label" 300 "$@"
  done
done
echo "$(date +%T) matrix2 done"
```

### `summarize.sh`

```bash
#!/usr/bin/env bash
# Aggregate WP-1 blocks: one row per block (out/c<N>-<cfg>), aggregates only.
# cols: block rounds threads_end threads_max rss_mean_MiB hwm_MiB live_MiB retained_MiB dupes_p95 lsa_p95 ls_p95 loe_p95 psi10_mean rustc_max
set -euo pipefail
D=$(cd "$(dirname "$0")" && pwd)
p95() { awk -v a="action:$2" '$1==a{f=1;next} f&&$1=="run"{print $5; exit}' "$1"; }
printf '%-9s %4s %3s %3s %6s %6s %5s %6s %7s %7s %6s %6s %5s %3s\n' block rnds thE thM rssM hwm live retnd dupes95 lsa95 ls95 loe95 psi10 rc
for o in "$D"/out/c*-*; do b=${o##*/}
  [[ -f $o/stats.txt ]] || continue
  read -r rssm thm psi rc < <(awk '{r+=$2;n++; if($4>t)t=$4; p+=$5; if($6>c)c=$6} END{printf "%.0f %d %.2f %d\n", r/n/1024, t, p/n, c}' "$o/samples.txt")
  hwm=$(awk '/VmHWM/{printf "%.0f", $2/1024}' "$o/proc_end")
  live=$(grep -Eo 'malloc live [0-9.]+' "$o/status.txt" | grep -Eo '[0-9.]+$')
  ret=$(grep -Eo 'retained [0-9.]+' "$o/status.txt" | grep -Eo '[0-9.]+$')
  printf '%-9s %4s %3s %3s %6s %6s %5s %6s %7s %7s %6s %6s %5s %3s\n' "$b" "$(cat "$o/rounds")" "$(cat "$o/threads_end")" "$thm" "$rssm" "$hwm" "$live" "$ret" \
    "$(p95 "$o/stats.txt" dupes-scan)" "$(p95 "$o/stats.txt" link-scan-all)" "$(p95 "$o/stats.txt" link-scan)" "$(p95 "$o/stats.txt" localize-on-edit)" "$psi" "$rc"
done
```
