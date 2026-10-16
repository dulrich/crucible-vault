# Live memory — first live session (2026-10-15)

Read-only `/proc` measurements of the live `cv` daemon (`crucible-vault.service`, started 2026-10-14 23:04, 0 restarts) and `cvault-editor`. Aggregates only.

## Commands

- Snapshot: `grep -E '^(Rss|Pss|Pss_Anon|Pss_File|Swap):' /proc/<pid>/smaps_rollup`; `grep -E 'VmHWM|VmRSS|Threads' /proc/<pid>/status`.
- Arena breakdown: awk over `/proc/<pid>/smaps` selecting anonymous mappings 65000–65540 KiB (glibc 64 MiB arena reservations) and summing their `Rss`.
- Growth: `./sample.sh 60 20` (00:58–01:17, 1-min interval) → `samples.tsv`.
- Activity: `journalctl --user -u crucible-vault --since -3h` line counts; `search feed step done` at :00/:30 (the `search-refresh` schedule, `every = "30m"`).

## Numbers

| proc | uptime | Pss_Anon | RSS | threads | notes |
|---|---|---|---|---|---|
| cv | 1 h 53 m → 2 h 13 m | 330 044 → 330 760 kB | 342 844 → 343 668 kB | 29–30 | VmHWM = VmRSS; 4 fully-resident 64 MiB arenas = 261 760 kB of 330 044 kB anon; 27 arena-shaped mappings total; `[heap]` 16.8 MiB |
| cvault-editor | 11 m → 31 m | 62 640 kB flat | 164 588 kB flat | 46 | Pss_File 84.6 MiB (libs, fonts, GPU); arena cap from editor-cutover-blockers holding |

- systemd: `MemoryCurrent` 485 MiB / `MemoryPeak` 489 MiB for the unit (includes page cache of index/state DBs).
- `state.db` 30 MB; index at `~/.cache/crucible-vault/index-2.db`.

## Reading

- **No steady growth.** Over 20 min the daemon gained 0.7 MiB, all at the 01:00 `search-refresh` run (8 108 matched notes fed). The earlier "≈3 MiB/min" from two points (646 MiB @ 3 h 36 m on 2026-10-14, 330 MiB @ 1 h 53 m today) was a coincidence of two different process lifetimes, not a rate.
- **Plateau set early.** Since VmHWM = VmRSS and the curve is flat, the 330 MiB was reached in the first ~2 h (startup index/scan or the first search feeds) and retained. 79 % of anon sits in 4 fully-touched glibc arenas — consistent with arena retention across tokio/`spawn_blocking` threads (same mechanism the editor's `mallopt` cap fixed: 100–130 MiB there).
- The previous 646 MiB instance (pre-restart) likely included cut-over work (reindex, migrations); unmeasured.

## Next measurement (needs a plan — not on the live daemon)

1. Startup curve: sample a fresh daemon from t=0 (scratch `CV_INDEX_DB`/`CV_STATE_DB`, shadow mode or a scratch vault copy outside every repo) to find which phase fills the arenas.
2. A/B `MALLOC_ARENA_MAX=2` (env, no code) on that scratch instance; if it cuts the plateau, port the editor's `cap_malloc_arenas()` to `cv`'s `main` (or set it in the unit's `Environment=`).
3. Optionally `malloc_trim(0)` after startup scan as a second lever.
