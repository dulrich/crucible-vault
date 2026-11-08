# Daemon memory growth — findings (2026-11-07)

Aggregates only. Raw journal and job list were deleted after extraction (they carry note/mail titles).

## Measurements

- Source: `journalctl --user -u crucible-vault --since 2026-11-04` `cv::memory` lines (5-min cadence; journal retention starts 2026-11-03 23:50). Per-restart table: perl one-liner in the session over `rss`/`malloc_in_use`/`malloc_retained`/`hwm`/`uptime_secs`.
- Every daemon start (9 restarts, 2026-11-04..07) has the same shape: RSS 50–100 MiB at 0.1 h, 110–160 MiB at 2 h, and on the longest run (pid 10058, 16 h on 2026-11-05/06) RSS swinging 260↔440 MiB with high-water 569 MiB.
- `malloc_in_use` stays flat across all runs: 55–72 MiB regardless of uptime. **Not a leak.**
- `malloc_retained` (= mallinfo2 `arena + hblkhd − in_use`) climbs monotonically (14 → 538 MiB over 16 h). It never falls, even when RSS drops 180 MiB between samples. `malloc_trim` returns pages with `madvise` but does not shrink `arena`, so this gauge measures arena address space, not resident memory.
- Live daemon at 2 h (`/proc/<pid>/smaps`): 34 threads on 24 CPUs; resident anon 118 MiB outside `[heap]` spread over **42 distinct 64 MiB arena slots** (9 hold more than 4 MiB); AnonHugePages 32 MiB.
- The 2026-11-03 "77.7 MiB rss+swap, hwm 84.2 MiB" figure was taken at 02:26, minutes after the round-22 redeploy restarted the daemon (`d6942c6` 02:19, `377f713` 02:23). It is a fresh-start number, so the comparison mixes uptimes, not vaults. Index size is unchanged (16,187 notes reconciled).
- Job-type correlation: the high-water rose more than 20 MiB in 29 of the 5-minute intervals. Comparing the job types created in those windows with all 620 windows shows no dominant trigger. The clip pipeline (`clip-images`, `clip-auto-file`, `intake-enrich`, `clip-enrich`) and `track-blogs` are 3–4× over-represented, but each appears in only 2–4 of the 29 windows. Script below (input `jobs.tsv` = job-id ms epoch + type from paged `cv jobs list`).

## Diagnosis

glibc per-thread arenas in the daemon. `cv_malloc::cap_arenas()` runs only in the editor's `main()`; `cmd_daemon` (`crates/cv/src/cli.rs`) builds a default multi-thread tokio runtime (one worker per CPU plus the blocking pool), so glibc creates up to 8 × 24 arenas. Transient allocations from steps that run on different threads (feed and mail parsing, clip enrich, search refresh) leave free memory scattered over dozens of arenas. `trim_after_job` releases some of it, which is why RSS swings, but not the partly used pages. It is the same mechanism the editor fix (plan editor-cutover-blockers WP-2) measured at 100–130 MiB.

Contributing churn: `track-mail` re-parses the same ~15 loose `.eml` files on every run, because moving them to `processed/` fails with `Read-only file system`. The unit's `ProtectHome=read-only` has no `ReadWritePaths` for the configured maildir (510 failures on 2026-11-07 alone; first journal occurrence 2026-11-04 00:00, after migration 0032 hardened the unit).

## Proposed fix (unmeasured — needs a restart, which is the user's)

1. Call `cv_malloc::cap_arenas()` first in `cmd_daemon`, before the runtime exists (one line, same as the editor).
2. Optionally cap the runtime: `worker_threads(4)` / `max_blocking_threads(16)`. The daemon is I/O-bound, and 24 workers are not needed.
3. Make the `malloc_retained` gauge honest, or rename it to `malloc_arena_free`. Today it never drops after a trim, and the 32 MiB trim threshold is therefore always exceeded, so the daemon trims after every job.
4. Separately, add the maildir to the unit's `ReadWritePaths` (new migration).

Verification: after deploy, the same per-restart table should show RSS staying within ~1.5× `malloc_in_use` at 16 h uptime.

## lift script

```perl
use POSIX qw(mktime);
open J,"jobs.tsv"; my @jobs=sort{$a->[0]<=>$b->[0]} map{[split]}<J>;
sub ep{ my($Y,$M,$D,$h,$m,$s)=$_[0]=~/(\d+)-(\d+)-(\d+)T(\d+):(\d+):(\d+)/; return (mktime($s,$m,$h,$D,$M-1,$Y-1900))*1000 }
open L,"j.log"; my @samp; while(<L>){ push @samp,ep(substr($_,0,19)) if /process="daemon"/ }
open K,"jumps.txt"; my @jt=map{ep((split)[0])}<K>;
sub types{ my $t=shift; my %s; for(@jobs){ next if $_->[0]<=$t-300000; last if $_->[0]>$t; $s{$_->[1]}=1 } keys %s }
my(%inj,%inall); $inall{$_}++ for map{types($_)} @samp; $inj{$_}++ for map{types($_)} @jt;
my $n=@samp; my $k=@jt;
printf "%-34s jumps %2d/%d  all %4d/%d  lift %.1f\n",$_,$inj{$_}//0,$k,$inall{$_},$n,(($inj{$_}//0)/$k)/($inall{$_}/$n) for sort {(($inj{$b}//0)/$inall{$b}) <=> (($inj{$a}//0)/$inall{$a})} grep{$inall{$_}>=3} keys %inall;
```
