# Live image-tab check — 2026-11-13 (1.0.4, live editor + daemon)

Aggregates only; the note list lives in `/dev/shm/cv-imgtabs/img15.tsv` (outside the repo).

- Scripts (scratch, untracked): `runs/spike/image-tabs-live/`. Set: `sh pick.sh <vault> 15 <list>` — the 15 notes with the most image embeds (37–71 each), opened 3 s apart with `cv uri obsidian://open?vault=vault&file=…`.
- Sampler: `sh sample.sh samples.tsv <stop-file> 2` — `/proc/<pid>/smaps_rollup` Rss and Pss_Anon for `cvault-editor` and `cv daemon` every 2 s. The editor's `cv status` gauges refresh only every 300 s (`crates/editor/src/memory.rs` `REPORT_EVERY`), so they are not used here.

| phase | editor rss | editor anon | daemon rss |
|---|---|---|---|
| before opening | 190 MiB | 71 MiB | 72 MiB |
| 15 opened, +10 s | 321 MiB | 198 MiB | 73 MiB |
| flipping (peak) | 414 MiB (hwm 467) | 292 MiB | 88 MiB |
| after flipping, 15 open | 288 MiB | 166 MiB | 88 MiB |
| all 15 closed, +10 s | 220 MiB | 98 MiB | 88 MiB |

User observation: images reappear without lag; a one-frame flash on quick tab swaps (the hidden-tab release redraws the tab before its re-decode lands).
