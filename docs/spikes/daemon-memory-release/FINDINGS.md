# Daemon memory release triggers — findings (2026-11-09)

Aggregates only. Probe replies (view rows) were deleted unread after the size and error counts were taken. The view ids below are vault `_custom/` configuration, which is reviewable.

Follows `docs/spikes/daemon-memory/FINDINGS.md` (2026-11-07, arena diagnosis) and `docs/spikes/daemon-threads/FINDINGS.md` (round 23: 4 tokio workers, 16 blocking threads, `M_TRIM_THRESHOLD = 128 KiB` via `cv_malloc::tune_daemon()`, **arena count left at glibc's default**. The editor's `M_ARENA_MAX=2` was rejected for the daemon on dupes-scan / link-scan-all p95).

## Summary

The daemon's growth with uptime is **freed memory that is not released**, and the main cause is **when the release runs**. The only release trigger is `trim_after_job` (`crates/cv/src/runner/claim.rs`), which calls `malloc_trim(0)` after a job settles when free-resident exceeds 32 MiB (`cv_malloc::TRIM_RETAINED_THRESHOLD`). The largest transient allocations come from editor requests (`view_run`), which never trigger a release. Between jobs, the freed memory stays resident.

## Measurements

### 1. Live heap stays flat; the growth is freed but resident memory

Daemon run started 2026-11-09 11:52 (build `6880411`, round-23 settings live, 13 threads). Values are MiB from the `cv::memory` journal line at or past each hour. `other` = `anon − in_use − free_res`.

| uptime | anon | malloc_in_use | malloc_free_resident | other |
|---|---|---|---|---|
| 0 h | 70 | 57 | 13 | 0 |
| 1 h | 123 | 62 | 61 | 0 |
| 2 h | 181 | 61 | 120 | 0 |
| 3 h | 197 | 57 | 139 | 0 |
| 4 h | 213 | 61 | 151 | 0 |
| 6 h | 218 | 61 | 157 | 0 |
| 7 h | 115 | 65 | 49 | 0 |
| 8 h | 155 | 61 | 93 | 0 |

All of the growth is `malloc_free_resident`, and none of it is non-malloc anon. `/proc/<pid>/smaps` at 8.5 h: anon 87 MiB (69 MiB in unnamed arena mappings, 17 MiB in `[heap]`). The drop at 7 h is a release that ran.

```sh
journalctl --user -u crucible-vault.service --since "2026-11-09 11:50" --no-pager -o short-unix \
 | grep 'memory process="daemon"' \
 | perl -ne 'if(/^(\d+)\..*anon=Some\((\d+)\).*malloc_in_use=Some\((\d+)\) malloc_free_resident=Some\((\d+)\)/){$s//=$1; $u=($1-$s)/3600; if($u>=$n){printf "%4.1fh anon %4d in_use %3d free_res %4d other %4d\n",$u,$2/2**20,$3/2**20,$4/2**20,($2-$3-$4)/2**20; $n+=1}}'
```

### 2. Growth does not follow jobs

There are 286 consecutive 5-minute sample pairs (all daemon runs whose jobs are still in `state.db`). Each one is classed by whether any job was claimed or settled within it.

| interval | count | anon +15 MiB or more | MiB grown in those |
|---|---|---|---|
| a job was active | 163 | 34 (21 %) | 1422 |
| no job was active | 123 | 33 (27 %) | 2200 |

Growth is as frequent, and larger, with no job running. Per job type, every type with n ≥ 5 shows a 12–33 % share of +15 MiB intervals, and none dominates (localize-on-edit 20 %, search-refresh 23 %, track-mail 18 %, clip-enrich 27 %).

```sh
journalctl --user -u crucible-vault.service --since 2026-11-03 --no-pager -o short-unix \
 | grep 'cv::memory: memory process="daemon"' \
 | perl -ne '/^(\d+)\.\d+ \S+ cv\[(\d+)\].*anon=Some\((\d+)\).*malloc_free_resident=Some\((\d+)\)/ && print "$2 $1 $3 $4\n"' > mem.txt
sqlite3 -readonly "file:$HOME/.local/state/crucible-vault/state.db?mode=ro" \
  "select job_type, coalesce(first_claimed_ms,created_ms)/1000, coalesce(settled_at_ms,created_ms)/1000 from jobs where created_ms > 1793750000000" > jobs.txt
# pair consecutive samples per pid; an interval is "job active" when any job's [claimed, settled] overlaps it
```

### 3. Editor view queries reproduce it on demand

The probe sent read-only requests to the live socket (`hello`, `views_list`, then `view_run` for each of the 26 configured views; no `this_path`, sort or select). Memory was read with `cv status`. There were no writes and no restart.

| step | anon | live | free-resident |
|---|---|---|---|
| before | 120.1 | 58.2 | 61.9 |
| all views, pass 1 | 234.1 | 68.3 | 165.9 |
| pass 2 | 268.0 | 68.3 | 199.7 |
| pass 3 | 285.2 | 68.3 | 216.9 |
| after 30 s idle | 285.2 | 68.3 | 216.9 |

One pass adds about 104 MiB of free-resident memory for about 10 MiB of live growth. Nothing is released while idle.

The per-view runs were sequential, with each view run 3 times. Deltas are anon MiB and are only indicative, because freed memory from earlier views is reused:

| view | reply | anon Δ |
|---|---|---|
| `ingestion#Trash` | 546 KB | +47 |
| `ingestion#Possible duplicates` | 168 KB | +41 |
| `hubs#Project Alpha` | 0.7 KB | +31 |
| `hubs#Hub View A` | 1.5 KB | +4 |
| `ingestion#X posts` | 193 KB | +5 |
| `backlinks#Here`, `daily-intake#Intake`, `blog-story-list#Stories` | < 1 KB | ~0 |

A view's cost tracks how much of the index it **scans**, not how big its reply is (`Project Alpha` returns under 1 KB for +31 MiB). Partway through the sequence anon fell from 276.9 to 97.1 MiB. That was `trim_after_job` firing for a job that settled meanwhile: one release recovers about 180 MiB. The probe was stopped after 14 of the 26 views, once the pattern was clear.

```sh
S=$XDG_RUNTIME_DIR/crucible-vault/daemon.sock
H='{"jsonrpc":"2.0","id":0,"method":"hello","params":{"protocol_version":1,"client":"mem-probe"}}'
mem(){ cv status | grep "daemon: rss" | sed -E 's/.*anon ([0-9.]+) MiB, malloc live ([0-9.]+) MiB \/ free-resident ([0-9.]+).*/anon \1 live \2 free \3/'; }
(printf '%s\n' "$H"; sleep 0.5; printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"views_list","params":null}'; sleep 2) \
 | timeout 6 nc -U "$S" | grep '"id":1' | perl -MJSON::PP -ne '$r=decode_json($_); print "$_\n" for @{$r->{result}{view_ids}}' > views.txt
mem
while IFS= read -r v; do
  req=$(perl -MJSON::PP -e 'print encode_json({jsonrpc=>"2.0",id=>2,method=>"view_run",params=>{view_id=>$ARGV[0]}})' "$v")
  (printf '%s\n' "$H"; sleep 0.2; for i in 1 2 3; do printf '%s\n' "$req"; done; sleep 4) | timeout 30 nc -U "$S" > /dev/null
  printf '%s: ' "$v"; mem
done < views.txt
```

(The all-views passes in the table above sent all 26 `view_run` requests on one connection per pass.)

## Diagnosis

1. **Missing release triggers (primary).** `malloc_trim` runs only after a job settles. Request handlers (`view_run`, and probably `search` and other index reads; only `view_run` was measured) allocate a large transient working set, free it, and leave it resident until the next job settles. With editor panels open and a quiet queue, that is 100–200 MiB held for minutes to hours (the 2–6 h plateau in table 1).
2. **Arena spread (amplifier).** The daemon keeps glibc's default arena count (round 23 rejected `M_ARENA_MAX=2` on latency). Transient memory lands in whichever arena the handling thread uses, so successive bursts on different threads do not reuse each other's freed memory, and free-resident climbs pass over pass (table 3).
3. **View working set (size of each burst).** A view run's transient allocation scales with its scan, up to about 47 MiB for one view.

## Release-strategy options, for the follow-up plan's spike (unmeasured)

The follow-up plan should open with a comprehensive spike that measures each option under a replayed mixed load (jobs plus editor view and search requests) on a scratch copy. For each it records anon over time, release latency, request and job p95, and trim CPU cost.

- **Periodic release on the 5-minute sampler** (`crates/cv/src/daemon/memory.rs`, `SAMPLE_EVERY`), using the same 32 MiB threshold on the blocking pool. Covers every source; worst case holds memory for one interval.
- **Release after heavy request handlers** (`view_run`, `search`), threshold-gated and debounced so a burst of panel refreshes trims once.
- **Idle release**: trim once the daemon has had no request and no job for N seconds.
- **Lower `M_TRIM_THRESHOLD` / `M_MMAP_THRESHOLD`**, so that large transient buffers are mmapped and returned on free instead of landing in arenas. Costs syscalls on each large allocation.
- **Arena count between 2 and the default** (for example 4 or 8, or the default plus periodic release), re-checked against the round-23 dupes-scan / link-scan-all p95 rule.
- **Smaller view working sets** (stream rows or project only the needed columns instead of materializing the full scan). Reduces burst size at the source; the largest change of the six.

Trim cost and `malloc_trim(0)` lock-holding across arenas under concurrent requests are the main unknowns for the trigger-based options.
