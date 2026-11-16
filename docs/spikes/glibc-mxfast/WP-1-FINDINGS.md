# glibc thread-arena retention on the real daemon — findings (2026-11-15, spikes 1–2 of the research hand-off)

Aggregates only. Scenario-A blocks on the research scratch vault copy, view replies discarded unread.

Origin: research `glibc-arena-retention/README.md` (Gentle Mink session). It found that a fastbin-sized burst freed on a worker thread stays resident as that arena's top chunk, that `malloc_trim` cannot shrink it, and that `GLIBC_TUNABLES=glibc.malloc.mxfast=0` removed it in a synthetic reproducer. Earlier daemon spike: `docs/spikes/memory-release-strategy/FINDINGS.md`.

Binary: release `cv` at `5cd0f5b6` (1.1.0), frozen copy. Each block is a fresh daemon in a throwaway `lhmc-bench` container (Debian 13, glibc 2.41-12+deb13u4), unpinned on the 24-core host at `--cpu-shares 52`, so parallel interactive sessions win the CPU. Arms are interleaved by `research/scripts/measure.mjs` (seeded), so residual host load falls on every arm alike. Each block runs 3 view passes (26 views + 5 searches each), then idles, with readings at fixed times after the passes end (30 s and 120 s; 60 s and 90 s in run C) and one more 20 s after a "poke" pass (one extra view pass). A reading records:
- settled anon (mean RssAnon over the last 10 s);
- malloc free-resident from `cv status` (anon minus `mallinfo2` in-use);
- per-arena resident size from `/proc/<pid>/smaps`: `[heap]` for the main arena, and 64 MiB-aligned anonymous `rw-p` mappings for thread arenas.

## Summary

- **mxfast=0 changes nothing on the real daemon.** Bad-mode rate, settled anon, free-resident, view p95 and pass CPU are all within noise of the A/A pair (run B). The reproducer's fastbin mechanism is not what holds memory here.
- **The bad mode is real but rare on 1.1.0: about 7 % of blocks at 120 s.** Normal blocks settle at 54–58 MiB with 0–1 MiB free-resident and the largest thread arena at about 6.8 MiB. Bad blocks hold 70–80 MiB, with 13–24 MiB free-resident, all of it in one thread arena that is 16–26 MiB resident. Part of spike 2 is confirmed: the excess is freed memory held in a single non-main arena.
- **Two distinct bad-mode shapes, only one of them explained:**
  - **S1, below-threshold hold (3 of about 180 blocks; cause known).** After the burst, free-resident sits at 11.7–15.4 MiB. That is under the idle release's 16 MiB `QUIET_TRIM_THRESHOLD` (`crates/cv/src/daemon/memory.rs`), so the release never trims and the excess stays for good. The earlier spike measured a trim at 1.3–5 ms with no effect on concurrent latency, so the gate saves almost nothing.
  - **S2, late arena growth (6 of 90 in run B, 0 of 64 in run C; trigger unknown).** At 30 s the block is clean (free-resident 0, arena 6.8 MiB). By 120 s one thread arena is 24–26 MiB resident, with 19–24 MiB free-resident. It is still there at 120 s even though at least one idle release was due in between (the 30 s `cv status` re-arms it, and free-resident is above 16 MiB). That fits "non-main arena top, not trimmable", but no daemon trim log was kept, so it is not shown. The only daemon request in that window is the probe's own `cv status`. Run C could not test that: 0/32 vs 0/32, against an expected 2 or so at 7 %.
- **The probe changes the result.** A `cv status` every 5 s (run T) counts as activity, so the idle release never fires and every block stays at 73–89 MiB. Readings must be few and spaced at least 25 s apart, or must read `/proc` only.
- **The research's 75 MiB boundary no longer separates the modes on 1.1.0.** The gap is 62–70 MiB, so this work uses 66 MiB, fixed from the pilot. At 30 s the 10 s mean still includes the tail from before the trim, which puts a few blocks at 66–68 MiB with no retained memory. Free-resident (>5 MiB) or the largest arena (>15 MiB) classify better than anon.

## Runs

| run | design | blocks | result |
|---|---|---|---|
| P pilot | base only, sequential | 1 + 8 | 1 S1 block (70.3 / 72.3 MiB at 30 / 120 s, free-res 13.5 / 15.4); 8 normal (52.7–57.8 MiB at 120 s, free-res 0–1.2) |
| B main | base-a, base-b (A/A), mx0; 30 interleaved rounds + 1 warm-up, sequential | 90 + 3 | table below |
| T trace | base, 5 s probe series (anon, free-res, top arena) over 150 s idle, 3 lanes | 15 | all 15 held 73–89 MiB and 16–31 MiB free-res throughout: the probe blocks the idle release |
| C probe A/B | status30 (cv status at 30 s) vs nostatus (/proc only before 120 s); 4 lanes × 8 interleaved rounds | 64 | status30: 1 S1 block (free-res 13.3 at 30 s, arena 25.8 MiB throughout); nostatus: 0. No S2 in either arm |

Run B, bad mode at 120 s (anon > 66 MiB, Clopper–Pearson 95 %):

| arm | bad / n at 120 s | 95 % CI | of which S1 / S2 | bad / n at 30 s (true retention, free-res > 5) |
|---|---|---|---|---|
| base-a | 3 / 30 | [0.021, 0.27] | 0 / 3 | 0 / 30 |
| base-b | 3 / 30 | [0.021, 0.27] | 1 / 2 | 1 / 30 |
| mx0 | 2 / 30 | [0.008, 0.22] | 0 / 2 | 0 / 30 |

measure.mjs report for run B (`main/report.md`; ratio vs base-a, 95 % bootstrap CI; every row "no measurable difference"):

| metric | base-a median | base-b | mx0 |
|---|---|---|---|
| view p95 (ms) | 64.75 | 0.971 [0.926, 1.067] | 0.985 [0.915, 1.063] |
| pass CPU (s) | 2.285 | 1.009 [0.987, 1.024] | 0.993 [0.978, 1.013] |
| anon at 120 s (MiB) | 56.75 | 1.001 [0.998, 1.005] | 0.999 [0.996, 1.004] |
| free-resident at 120 s (MiB) | 0 | — | — |

The poke reading (20 s after one extra view pass) is 74–81 MiB with 17–24 MiB free-resident in every arm. That is the state before the trim, with the 20 s idle release not yet due, not a bad mode.

The first single mx0 block (49 MiB, 0 free-res) was a normal-mode block. Read next to the S1 first base block, it looked like an effect. It wasn't.

## Implications (for a plan, not decided here)

1. **Do not ship mxfast=0.** It costs nothing measurable and fixes nothing.
2. **S1 has a direct, cheap fix.** Lower the idle-release threshold (for example to 4–8 MiB, above the 0–1 MiB normal residue), or trim once per idle period without a threshold. Earlier spikes priced trims as cheap. That fix is small enough for a direct one-file change or a single-WP plan.
3. **S2 needs one more spike before any fix (spike 3 territory).** Keep the daemon log for every block and record the `cv::memory` "trim after idle" lines, to show whether a trim ran after the growth and what it returned. Find the trigger by checking what runs on a worker thread between 30 and 120 s idle (the config poll every 2 s, the mail-watch tick every 1 s, the status/gauge snapshot on the blocking pool). With an S2 rate near 5 %, a rate comparison needs a few hundred blocks, so first look for the trigger in the logs and time series of bad blocks.
4. **For unit sizing (suggestion 4), the high mode on this scenario is about 80–90 MiB anon** (the T trace, with no trim at all). The process high-water mark in these blocks was about 99 MiB.

## Reproduce

The harness ran from a throwaway scratch directory holding the scripts inlined below, plus a `bin/cv` copy of the release binary. It needs the research repo's `live-host-measurement-confounds/` (`client.pl`, `var/scratch/` vault copy, `lhmc-bench` image) and `research/scripts/measure.mjs`.

```
cp target/release/cv <scratch>/bin/cv
for i in $(seq 8); do ./run-block.sh base; done                                   # P
bun …/research/scripts/measure.mjs run --config main.json --out main --host local   # B (report: measure.mjs report main)
for l in 1 2 3 4; do bun …/measure.mjs run --config probe-$l.json --out probe-$l --host local & sleep 37; done; wait   # C
```

The classification thresholds (anon > 66 MiB; free-res > 5 MiB; largest arena > 15 MiB) were applied to `raw.jsonl` after the run. In run B the in-block `bad_*` flags used the 75 MiB default, because `envPrefix` did not reach the container.

### Appendix: harness

`cvshim.sh`

```
#!/bin/sh
# cvshim.sh - applies $SPIKE_TUNABLES to the daemon only (exec keeps the pid), passes every other cv call through.
set -eu
if [ "${1:-}" = daemon ] && [ -n "${SPIKE_TUNABLES:-}" ]; then GLIBC_TUNABLES=$SPIKE_TUNABLES exec /cvbin/cv "$@"; fi
exec /cvbin/cv "$@"
```

`run-block.sh`

```
#!/bin/sh
# run-block.sh <base|mx0> - one block in a throwaway low-priority lhmc-bench container (cpu weight 20, unpinned,
# so interactive sessions win CPU; arms are interleaved by measure.mjs so residual host load hits both arms).
set -eu
H=$(cd "$(dirname "$0")" && pwd); L=<code-root>/research/live-host-measurement-confounds
case ${1:?arm} in base) tun= ;; mx0) tun=glibc.malloc.mxfast=0 ;; *) echo "arm?" >&2; exit 2 ;; esac
blk=$(mktemp -d /tmp/cvmx-blk.XXXXXX); trap 'rm -rf "$blk"' EXIT
docker run --rm --cpu-shares 52 --memory 1g -e SPIKE_TUNABLES="$tun" -e PASSES="${PASSES:-3}" -e BADMIB="${BADMIB:-75}" -e NOSTATUS="${NOSTATUS:-0}" \
  -v "$L:/lhmc:ro" -v "$H:/spike:ro" -v "$H/bin:/cvbin:ro" -v "$L/var/scratch:/scratch:ro" -v "$blk:/blk" \
  lhmc-bench sh /spike/block.sh
```

`block.sh`

```
#!/bin/sh
# block.sh - one fresh cv daemon block of scenario A (research cvblock.sh K3, extended) inside lhmc-bench.
# Passes, then idle readings at 30 s and 120 s, then one poke view pass and a 20 s reading.
# Each reading: settled anon (mean RssAnon of the last 10 s), bad (>75 MiB), malloc free-resident (cv status),
# per-arena smaps rss (arenas.pl). Prints MEASURE lines.
set -eu
S=/scratch; BIN=/spike/cvshim.sh; R=/blk/work; D=/lhmc; PASSES=${PASSES:-3}
rm -rf "$R"; mkdir -p "$R/state" "$R/cache" "$R/run" "$R/data" "$R/hm"
cp "$S/ro/search.db" "$R/state/search.db"
export CV_VAULT=$S/pristine CV_CONFIG_DIR=$S/pristine/_custom/crucible-vault CV_STATE_DB=$R/state/state.db \
  CV_SEARCH_DB=$R/state/search.db CV_INDEX_DB=$R/cache/index.db CV_SOCKET=$R/run/cv.sock \
  XDG_DATA_HOME=$R/data XDG_RUNTIME_DIR=$R/run XDG_STATE_HOME=$R/state XDG_CACHE_HOME=$R/cache \
  XDG_CONFIG_HOME=$R/hm/.config HOME=$R/hm CV_LINK_WRITE_ROOTS=
unset CV_ROUTER_GATEWAY_KEY NOTIFY_SOCKET || true
"$BIN" daemon >"$R/daemon.log" 2>&1 &
pid=$!
samp=
cleanup() { [ -z "$samp" ] || kill "$samp" 2>/dev/null || true; kill "$pid" 2>/dev/null || true; rm -rf "$R"; }
trap cleanup EXIT INT TERM
i=0
while [ $i -lt 300 ]; do [ -S "$CV_SOCKET" ] && "$BIN" status >/dev/null 2>&1 && break; i=$((i + 1)); sleep 1; done
[ $i -lt 300 ] || { echo "block.sh: daemon did not come up" >&2; exit 1; }
grep -q . /proc/$pid/environ && tr '\0' '\n' </proc/$pid/environ | grep -c '^GLIBC_TUNABLES=' >"$R/tun" || true
drain() { n=0; while [ $n -lt 900 ]; do
  q=$("$BIN" status 2>/dev/null | grep -Eo '(queued|running)=[0-9]+' | grep -Eo '[0-9]+' | awk '{s+=$1} END{print s+0}') || q=0
  [ "${q:-0}" = 0 ] && break; n=$((n + 1)); sleep 1; done; }
drain; sleep 5
now_ms() { date +%s%3N; }
t0=$(now_ms)
ticks() { awk '{print $14+$15}' "/proc/$pid/stat"; }
c0=$(ticks)
( while kill -0 "$pid" 2>/dev/null; do
    printf '%s %s\n' "$(( $(now_ms) - t0 ))" "$(awk '/^RssAnon/{print $2}' "/proc/$pid/status")"; sleep 0.5
  done >"$R/samples.txt" ) &
samp=$!
: >"$R/requests.txt"
pass() { perl "$D/client.pl" "$CV_SOCKET" views "$1" >>"$R/requests.txt"; }
p=0; while [ $p -lt "$PASSES" ]; do pass 5; p=$((p + 1)); done
c1=$(ticks); tp=$(now_ms)
vp95=$(awk '$1=="view"{print $3}' "$R/requests.txt" | sort -n | awk '{a[NR]=$1} END{i=int(0.95*NR+0.999); if(i<1)i=1; printf "%.1f", a[i]}')
reading() { # reading <tag>
  e=$(( $(now_ms) - t0 ))
  st=$(awk -v e="$e" '$1>=e-10000&&$1<=e{s+=$2;n++} END{printf "%.2f", (n?s/n:0)/1024}' "$R/samples.txt")
  fr=-1; [ "$tag" = i60 ] || [ "$tag" = i90 ] || { [ "${NOSTATUS:-0}" = 1 ] && [ "$tag" = i30 ]; } || fr=$("$BIN" status 2>/dev/null | awk '/^  daemon:/{for(i=1;i<=NF;i++) if($i=="free-resident"){print $(i+1); exit}}')
  set -- $(perl /spike/arenas.pl "$pid")
  echo "MEASURE anon_${tag}_mib $st MiB"
  echo "MEASURE bad_${tag} $(awk -v s="$st" -v t="${BADMIB:-75}" 'BEGIN{print (s > t) ? 1 : 0}')"
  echo "MEASURE freeres_${tag}_mib ${fr:--1} MiB"
  echo "MEASURE heapmain_${tag}_mib $(awk -v k="$1" 'BEGIN{printf "%.2f", k/1024}') MiB"
  echo "MEASURE arenatop1_${tag}_mib $(awk -v k="$2" 'BEGIN{printf "%.2f", k/1024}') MiB"
  echo "MEASURE arenasum_${tag}_mib $(awk -v k="$4" 'BEGIN{printf "%.2f", k/1024}') MiB"
  echo "MEASURE narenas_${tag} $5"
}
sleep_to() { w=$(( $1 * 1000 - ( $(now_ms) - tp ) )); [ $w -le 0 ] || sleep "$(awk -v w="$w" 'BEGIN{printf "%.3f", w/1000}')"; }
sleep_to 30; tag=i30; reading
sleep_to 60; tag=i60; reading
sleep_to 90; tag=i90; reading
sleep_to 120; tag=i120; reading
pass 0; sleep 20; tag=poke; reading
errs=$(grep -cE 'ERROR|panicked' "$R/daemon.log" || true)
bad=$(grep -c ' err$' "$R/requests.txt" || true)
echo "MEASURE view_p95_ms $vp95 ms"
echo "MEASURE pass_cpu_s $(awk -v a="$c0" -v b="$c1" 'BEGIN{printf "%.2f", (b-a)/100}') s"
echo "MEASURE pass_wall_s $(awk -v a="$t0" -v b="$tp" 'BEGIN{printf "%.2f", (b-a)/1000}') s"
echo "MEASURE anon_peak_mib $(awk '{if($2>p)p=$2} END{printf "%.2f", p/1024}' "$R/samples.txt") MiB"
echo "MEASURE tunables_set $(cat "$R/tun" 2>/dev/null || echo 0)"
echo "MEASURE errors $(( errs + bad ))"
```

`arenas.pl`

```
#!/usr/bin/perl
# arenas.pl <pid> - per-arena resident KiB from /proc/<pid>/smaps: main = [heap]; thread arenas = anonymous
# rw-p mappings whose start is 64 MiB aligned (glibc HEAP_MAX_SIZE). Prints: main_kib top1_kib top2_kib sum_kib n
use strict; use warnings; no warnings "portable";
open my $f, '<', "/proc/$ARGV[0]/smaps" or die; my ($cur, %rss, $main) = (undef); $main = 0;
while (<$f>) {
  if (/^([0-9a-f]+)-([0-9a-f]+) (\S+) \S+ \S+ \S+\s*(.*)$/) { my ($s, $p, $n) = (hex $1, $3, $4);
    $cur = ($n eq '[heap]') ? 'main' : ($p eq 'rw-p' && $n eq '' && $s % (64 << 20) == 0) ? $s : undef; next; }
  if (defined $cur && /^Rss:\s+(\d+)/) { if ($cur eq 'main') { $main += $1 } else { $rss{$cur} += $1 } }
}
my @v = sort { $b <=> $a } values %rss; my $sum = 0; $sum += $_ for @v;
printf "%d %d %d %d %d\n", $main, $v[0] // 0, $v[1] // 0, $sum, scalar @v;
```

`main.json`

```
{
 "variants": [
  {"name": "base-a", "group": "base-a", "argv": ["sh", "<code-root>/crucible-vault/runs/spike/glibc-mxfast/run-block.sh", "base"]},
  {"name": "base-b", "group": "base-b", "argv": ["sh", "<code-root>/crucible-vault/runs/spike/glibc-mxfast/run-block.sh", "base"]},
  {"name": "mx0", "group": "mx0", "argv": ["sh", "<code-root>/crucible-vault/runs/spike/glibc-mxfast/run-block.sh", "mx0"]}
 ],
 "invocations": 30, "warmup": 1, "padMax": 0,
 "aa": ["base-a", "base-b"], "baseline": "base-a", "seed": 20261115,
 "envPrefix": ["env", "BADMIB=66"],
 "metrics": {
  "bad_i30": {"kind": "rate", "better": "lower"}, "bad_i120": {"kind": "rate", "better": "lower"}, "bad_poke": {"kind": "rate", "better": "lower"},
  "anon_i30_mib": {"kind": "level", "better": "lower"}, "anon_i120_mib": {"kind": "level", "better": "lower"}, "anon_poke_mib": {"kind": "level", "better": "lower"},
  "anon_peak_mib": {"kind": "level", "better": "lower"},
  "freeres_i30_mib": {"kind": "level", "better": "lower"}, "freeres_i120_mib": {"kind": "level", "better": "lower"}, "freeres_poke_mib": {"kind": "level", "better": "lower"},
  "heapmain_i120_mib": {"kind": "level", "better": "lower"}, "arenatop1_i30_mib": {"kind": "level", "better": "lower"}, "arenatop1_i120_mib": {"kind": "level", "better": "lower"},
  "arenatop1_poke_mib": {"kind": "level", "better": "lower"}, "arenasum_i120_mib": {"kind": "level", "better": "lower"},
  "pass_cpu_s": {"kind": "level", "better": "lower"}, "tunables_set": {"kind": "level", "better": "lower"}, "errors": {"kind": "level", "better": "lower"}
 }
}
```

`probe-1.json`

```
{"variants": [
  {"name": "status30", "group": "status30", "argv": ["env", "BADMIB=66", "sh", "<code-root>/crucible-vault/runs/spike/glibc-mxfast/run-block.sh", "base"]},
  {"name": "nostatus", "group": "nostatus", "argv": ["env", "BADMIB=66", "NOSTATUS=1", "sh", "<code-root>/crucible-vault/runs/spike/glibc-mxfast/run-block.sh", "base"]}],
 "invocations": 8, "warmup": 0, "padMax": 0, "baseline": "status30", "seed": 20261111,
 "metrics": {"bad_i30": {"kind": "rate", "better": "lower"}, "bad_i60": {"kind": "rate", "better": "lower"}, "bad_i90": {"kind": "rate", "better": "lower"}, "bad_i120": {"kind": "rate", "better": "lower"}, "bad_poke": {"kind": "rate", "better": "lower"}}}
```

