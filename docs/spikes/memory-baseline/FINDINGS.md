# Memory baseline — `cv` daemon + `cvault-editor` (2026-10-19)

Baseline for the memory-reduction plan (`plans/memory-instrumentation-and-reduction.md`). Build: 0.1.1 (`41365e2`, `target/release` byte-identical to the installed binaries). Aggregates only: no note text, titles or paths. Per-path data (the image-note pick list) stayed in the scratch dir `~/.local/share/cv-mem-scratch/`, outside every repo.

## Setup

- Scratch: `rsync -a --exclude .obsidian --exclude .trash <live vault>/ <scratch>/vault/` (2.1 GiB). `state.db` and `search.db` were copied with `sqlite3 .backup` (56 MB and 867 MB). `<scratch>/vault/_custom/crucible-vault/schedules.toml` was cut to `search-refresh` only, and `search-refresh` got a command starter so `cv run` can start it. No tracker, daily-brief or LLM action ran. Embeddings went to the local provider (`127.0.0.1:4811`).
- Daemon: `run-daemon.sh` (1 s sampler) and `scenario.sh <label> [ENV=…]`. The scenario runs a cold start (fresh index), then 4 searches via `rpc.py`, `search-refresh` ×1, then ×2 more, `link-scan-all`, and 20 more searches. It prints one row per phase (`ab.tsv`).
- Editor: `run-editor.sh` (idle) and `run-editor-load.sh <label> <daemon> [ENV=…]` (`edload.tsv`). The editor starts on the user's saved tabs (`editor.json` copied into a scratch `XDG_STATE_HOME`). The load script then opens the 15 scratch notes with the most image embeds via `cv uri obsidian://open?vault=vault&file=…`, 3 s apart. Window on the 7680×2160 X11 display.
- Live vs. retained malloc: `mshim.c` is a spike-only `LD_PRELOAD` shim, never shipped. A thread reads `stats` or `trim` from a FIFO and runs `malloc_stats()` or `malloc_trim(0)`. ptrace is off (`yama/ptrace_scope=1`), so gdb could not attach.
- Live processes: read-only `/proc/<pid>/{status,smaps_rollup,smaps}`.

**Units:** all MiB values are KiB ÷ 1024 from the TSV rows (rev 2; rev 1 divided by 1000). Acceptance thresholds are stated in KiB.

**Metric note:** this machine swaps (swappiness 10, 7.8 GiB of swap in use). The live daemon showed RSS 50.5 MiB but VmSwap 135.8 MiB. Footprint is therefore **RSS + swap** (or `Pss_Anon + Swap`). RSS alone understates it.

## Daemon

| Phase (scratch, default malloc) | RSS MiB | anon MiB | 64 MiB arena RSS | Note |
|---|---|---|---|---|
| start (cold index build, 8.5k notes) | 43–51 | 31–38 | 0 | binary text 13 MiB; flat over 4 min (`cold-default.tsv`) |
| + 4 searches | 67–80 | 51–64 | 0 | first search opens the semsearch index: **+26 MiB live** (sign codes + per-chunk `chunk_source` map); later queries ~0 |
| + first `search-refresh` | 162–184 | 147–169 | 64 | **+104 MiB** |
| + 2 more refreshes | 185.4 (189 880 KiB) | 171.3 | 64 | +3–4 MiB per run |
| + `link-scan-all`, + 20 searches | 186.6 | 171.3 | 64 | flat |

- **Live check:** daemon at 40 min, two scheduled refreshes. VmHWM 188.4 MiB, RSS 50.5 MiB + swap 135.8 MiB = **186.3 MiB footprint**, matching the scratch steady state. The 2026-10-15 reading (330 MiB at 2 h, 646 MiB at 3.6 h on an earlier build) included tracker/localize/daily jobs not reproduced here (network-bound). This plan's instrumentation is what attributes them.
- **Live vs. retained** (`mshim`, after the first refresh): malloc `in use` 47.0 MiB vs `system` 175.2 MiB, so **128.2 MiB is freed but retained by glibc**. One `malloc_trim(0)` takes anon from 166.5 to **49.2 MiB**.
- **Cause of the spike:** `NotePrep::local_hashes` (`crates/cv/src/steps/search.rs:329`) calls `local_notes()`. That fully prepares every indexable note (~8.5k) into one `Vec<(String, LocalNote)>`, then keeps only `(path, content_hash)`. The whole vault's prepared text is live at once, on a `spawn_blocking` thread whose arena keeps the pages.
- **`MALLOC_ARENA_MAX=2` alone** (`GLIBC_TUNABLES=glibc.malloc.arena_max=2:glibc.malloc.trim_threshold=131072`): steady anon 159.0 vs 171.3 MiB, **−12 MiB only**. Arena count is not the lever; retention of freed memory is.
- Side finding: every refresh reports `missing_index=25` (live: 25–26). The same ~25 notes are re-prepared and re-sent to the embedder every 30 min. Not memory-relevant; recorded for the backlog.
- Disk, not memory: a stale `~/.cache/crucible-vault/index-2.db` (68 MB + WAL) from the 2026-10-15 generation remains next to `index-3.db`.

Areas, steady state after a refresh (~186 MiB RSS):

| Area | MiB | Kind |
|---|---|---|
| glibc-retained freed memory (refresh peak) | ~128 | reclaimable by trim; avoidable by streaming |
| semsearch resident index (codes + chunk-source map) | ~26 | live, by design |
| core heap (index writer resolver, state, runtime, SQLite caches) | ~22 | live |
| binary text + libs (file-backed) | ~14 | shared/clean |

## Editor

| Run | idle RSS | idle anon | idle file | load (15 image notes) RSS | load anon | CPU s over load |
|---|---|---|---|---|---|---|
| default (wgpu, all ICDs) | 146–152 | 54 | 53 | **275.4–275.8** | 175 | 2 |
| `WGPU_BACKEND=vulkan VK_LOADER_DRIVERS_SELECT=*radeon*` | 116.7 | 43 | 43 | 243.6 | 165 | 2 |
| `ICED_BACKEND=tiny-skia` | **71.3** | 38 | 19 | **200.5** | 162 | 5 |
| tiny-skia + `malloc_trim(0)` after load (`edload2.tsv`, swap 0) | 70.5 | 38 | 18 | **139.9** (143 260 KiB) | 102 | 5 |
| wgpu + `malloc_trim(0)` after load (`edload2.tsv`, swap 0) | 149.1 | 54 | 54 | 214.7 (219 828 KiB; untrimmed 282 412 KiB) | 114 | 2 |

- **File-backed GPU stack, idle default (~53 MiB):** `libLLVM.so.21.1` 41.9 MiB, `libgallium` 8.4 MiB, seven Vulkan ICDs for absent vendors (nouveau, freedreno, lavapipe, intel ×2, asahi, dzn) ~17 MiB, `libvulkan_radeon` 5 MiB. Binary text is 22 MiB. radv links LLVM itself, so LLVM stays mapped under any wgpu/Vulkan configuration on this AMD box. Only the software renderer drops it.
- **Load, live vs. retained** (`mshim`, wgpu): malloc `in use` 96.6 MiB vs `system` 163.0 MiB, so **66.4 MiB is freed but retained** despite the `M_ARENA_MAX=2` / `M_TRIM_THRESHOLD` cap. `malloc_trim(0)`: RSS 276.3 → 215.3 MiB.
- Live load (96.6 MiB) breaks down per code survey: decoded-image LRU, 64 MiB budget, `MAX_WIDTH` 2048 (`crates/editor/src/image_cache.rs:48,52`), with full-resolution decode transients unbudgeted (`:385-401`); one shaped buffer (~22 MiB, visible tab only); plus the resolver, explorer and view-store maps. `ViewStore.entries` has no cap (`crates/editor/src/views/mod.rs:104`).
- Live editor at 40 min: RSS 98.7 MiB + swap 47.1 MiB = 145.8 MiB, consistent with the idle runs.
- tiny-skia costs CPU: 5 s vs 2 s over the load script. Scroll and typing feel at 7680×2160 could not be measured (no xdotool; synthetic input would move the user's pointer), so it is a user hand-check.

## Instrumentation today

None. No code reads its own memory. `cv status` has no memory fields. Editor stderr goes to `/dev/null` when `install.sh` relaunches it (`install.sh:185`). The only history is spike notes like this one.

## Reduction levers (measured)

| Lever | Process | Effect |
|---|---|---|
| Stream `local_hashes` (hash per note, drop the prepared body) | daemon | removes the ~110 MiB refresh peak (VmHWM) |
| `malloc_trim(0)` after heavy work | daemon | steady anon 166.5 → 49.2 MiB |
| `malloc_trim(0)` after image-decode bursts / when idle | editor | load −61 MiB |
| Renderer: tiny-skia | editor | idle −78 MiB, load −75 MiB; CPU ×2.5 |
| tiny-skia + trim (measured together) | editor | idle −52.7 %, load −49.3 % (282 412 → 143 260 KiB) |
| Renderer: wgpu Vulkan-only + radeon ICD | editor | idle −33 MiB, load −32 MiB |
| arena cap only | daemon | −12 MiB (not worth a change) |

Targets (−50 % footprint = RSS + swap, KiB): daemon `refresh3` ≤ 94 940 (baseline 189 880); editor idle ≤ 76 350 (baseline 152 700, `edload2.tsv`); editor `open15+30s` ≤ 141 206 (baseline 282 412). The measured tiny-skia + trim path reaches 143 260 KiB at load (−49.3 %): 2 MiB short of a strict half.

## After WP-M3 (daemon: streamed `local_hashes` + trim after jobs over 32 MiB retained)

`scenario.sh m3after` (release `cv` at M2 + M3, cold index build at start). Columns: label, phase, Rss, Pss_Anon, 64 MiB-arena Rss, heap Rss, VmHWM, VmSwap (KiB).

| phase | Rss | Pss_Anon | heap | VmHWM | swap |
|---|---|---|---|---|---|
| start | 71 840 | 56 188 | 30 948 | 71 856 | 0 |
| search4 | 86 472 | 70 372 | 30 948 | 86 472 | 0 |
| refresh1 | 73 528 | 57 428 | 23 916 | 95 520 | 0 |
| **refresh3** | **74 252** | 58 152 | 23 916 | **95 520** | 0 |
| linkscan | 76 620 | 60 328 | 23 916 | 110 516 | 0 |
| search24 | 81 744 | 65 452 | 23 916 | 110 516 | 0 |

`refresh3` footprint 74 252 KiB vs target ≤ 94 940 (baseline 189 880): **−60.9 %**. VmHWM 95 520 at `refresh3` (≤ 112 640). The later 110 516 peak comes from `link-scan-all`, which is outside this WP's levers.

## After WP-M4 (editor: quiet-period trim)

`run-editor-load.sh m4gpu m4d` (release editor at M2–M4, default wgpu renderer with all ICDs, i.e. pre-M5; scratch daemon `m4d`). KiB: Rss, Pss_Anon, Pss_File, CPU s, swap.

| phase | Rss | Pss_Anon | Pss_File | CPU s | swap |
|---|---|---|---|---|---|
| idle | 154 744 | 55 480 | 54 822 | 0 | 0 |
| open5 | 211 356 | 108 220 | 57 139 | 1 | 0 |
| open15 | 282 912 | 177 848 | 58 030 | 2 | 0 |
| **open15+30s** | **220 984** | 115 920 | 58 030 | 2 | 0 |

The trim fires after the 5 s quiet period: `open15+30s` is 220 984 KiB vs the 282 412 baseline (−21.8 %), matching the spike's wgpu + `malloc_trim` row (219 828). Idle is unchanged; the renderer (M5) is the idle lever.
