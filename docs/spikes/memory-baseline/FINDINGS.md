# Memory baseline — `cv` daemon + `cvault-editor` (2026-10-19)

Baseline for the memory-reduction plan (`plans/memory-instrumentation-and-reduction.md`). Build: 0.1.1 (`41365e2`, `target/release` byte-identical to the installed binaries). Aggregates only: no note text, titles or paths. Per-path data (the image-note pick list) stayed in the scratch dir `~/.local/share/cv-mem-scratch/`, outside every repo.

## Setup

- Scratch: `rsync -a --exclude .obsidian --exclude .trash <live vault>/ <scratch>/vault/` (2.1 GiB). `state.db` and `search.db` were copied with `sqlite3 .backup` (56 MB and 867 MB). `<scratch>/vault/_custom/crucible-vault/schedules.toml` was cut to `search-refresh` only, and `search-refresh` got a command starter so `cv run` can start it. No tracker, daily-brief or LLM action ran. Embeddings went to the local provider (`127.0.0.1:4811`).
- Daemon: `run-daemon.sh` (1 s sampler) and `scenario.sh <label> [ENV=…]`. The scenario runs a cold start (fresh index), then 4 searches via `rpc.py`, `search-refresh` ×1, then ×2 more, `link-scan-all`, and 20 more searches. It prints one row per phase (`ab.tsv`).
- Editor: `run-editor.sh` (idle) and `run-editor-load.sh <label> <daemon> [ENV=…]` (`edload.tsv`). The editor starts on the user's saved tabs (`editor.json` copied into a scratch `XDG_STATE_HOME`). The load script then opens the 15 scratch notes with the most image embeds via `cv uri obsidian://open?vault=vault&file=…`, 3 s apart. Window on the 7680×2160 X11 display.
- Live vs. retained malloc: `mshim.c` is a spike-only `LD_PRELOAD` shim, never shipped. A thread reads `stats` or `trim` from a FIFO and runs `malloc_stats()` or `malloc_trim(0)`. ptrace is off (`yama/ptrace_scope=1`), so gdb could not attach.
- Live processes: read-only `/proc/<pid>/{status,smaps_rollup,smaps}`.

**Metric note:** this machine swaps (swappiness 10, 7.8 GiB of swap in use). The live daemon showed RSS 52 MiB but VmSwap 139 MiB. Footprint is therefore **RSS + swap** (or `Pss_Anon + Swap`). RSS alone understates it.

## Daemon

| Phase (scratch, default malloc) | RSS MiB | anon MiB | 64 MiB arena RSS | Note |
|---|---|---|---|---|
| start (cold index build, 8.5k notes) | 43–53 | 31–39 | 0 | binary text 13 MiB; flat over 4 min (`cold-default.tsv`) |
| + 4 searches | 70–80 | 56–66 | 0 | first search opens the semsearch index: **+26 MiB live** (sign codes + per-chunk `chunk_source` map); later queries ~0 |
| + first `search-refresh` | 174–188 | 160–172 | 64 | **+106 MiB** |
| + 2 more refreshes | 185 | 171 | 64 | +3–4 MiB per run |
| + `link-scan-all`, + 20 searches | 187 | 171 | 64 | flat |

- **Live check:** daemon at 40 min, two scheduled refreshes. VmHWM 193 MiB, RSS 52 MiB + swap 139 MiB = **191 MiB footprint**, matching the scratch steady state. The 2026-10-15 reading (330 MiB at 2 h, 646 MiB at 3.6 h on an earlier build) included tracker/localize/daily jobs not reproduced here (network-bound). This plan's instrumentation is what attributes them.
- **Live vs. retained** (`mshim`, after the first refresh): malloc `in use` 48 MiB vs `system` 179 MiB, so **131 MiB is freed but retained by glibc**. One `malloc_trim(0)` takes anon from 170 to **50 MiB**.
- **Cause of the spike:** `NotePrep::local_hashes` (`crates/cv/src/steps/search.rs:329`) calls `local_notes()`. That fully prepares every indexable note (~8.5k) into one `Vec<(String, LocalNote)>`, then keeps only `(path, content_hash)`. The whole vault's prepared text is live at once, on a `spawn_blocking` thread whose arena keeps the pages.
- **`MALLOC_ARENA_MAX=2` alone** (`GLIBC_TUNABLES=glibc.malloc.arena_max=2:glibc.malloc.trim_threshold=131072`): steady anon 163 vs 171 MiB, **−12 MiB only**. Arena count is not the lever; retention of freed memory is.
- Side finding: every refresh reports `missing_index=25` (live: 25–26). The same ~25 notes are re-prepared and re-sent to the embedder every 30 min. Not memory-relevant; recorded for the backlog.
- Disk, not memory: a stale `~/.cache/crucible-vault/index-2.db` (68 MB + WAL) from the 2026-10-15 generation remains next to `index-3.db`.

Areas, steady state after a refresh (~187 MiB RSS):

| Area | MiB | Kind |
|---|---|---|
| glibc-retained freed memory (refresh peak) | ~131 | reclaimable by trim; avoidable by streaming |
| semsearch resident index (codes + chunk-source map) | ~26 | live, by design |
| core heap (index writer resolver, state, runtime, SQLite caches) | ~22 | live |
| binary text + libs (file-backed) | ~14 | shared/clean |

## Editor

| Run | idle RSS | idle anon | idle file | load (15 image notes) RSS | load anon | CPU s over load |
|---|---|---|---|---|---|---|
| default (wgpu, all ICDs) | 151–156 | 55 | 54 | **282** | 178 | 2 |
| `WGPU_BACKEND=vulkan VK_LOADER_DRIVERS_SELECT=*radeon*` | 120 | 45 | 44 | 250 | 169 | 2 |
| `ICED_BACKEND=tiny-skia` | **73** | 39 | 19 | **205** | 166 | 5 |

- **File-backed GPU stack, idle default (~55 MiB):** `libLLVM.so.21.1` 43 MiB, `libgallium` 8.6 MiB, seven Vulkan ICDs for absent vendors (nouveau, freedreno, lavapipe, intel ×2, asahi, dzn) ~17 MiB, `libvulkan_radeon` 5 MiB. Binary text is 22.5 MiB. radv links LLVM itself, so LLVM stays mapped under any wgpu/Vulkan configuration on this AMD box. Only the software renderer drops it.
- **Load, live vs. retained** (`mshim`, wgpu): malloc `in use` 99 MiB vs `system` 167 MiB, so **68 MiB is freed but retained** despite the `M_ARENA_MAX=2` / `M_TRIM_THRESHOLD` cap. `malloc_trim(0)`: RSS 283 → 220 MiB.
- Live load (99 MiB) breaks down per code survey: decoded-image LRU, 64 MiB budget, `MAX_WIDTH` 2048 (`crates/editor/src/image_cache.rs:48,52`), with full-resolution decode transients unbudgeted (`:385-401`); one shaped buffer (~22 MiB, visible tab only); plus the resolver, explorer and view-store maps. `ViewStore.entries` has no cap (`crates/editor/src/views/mod.rs:104`).
- Live editor at 40 min: RSS 101 MiB + swap 48 MiB = 149 MiB, consistent with the idle runs.
- tiny-skia costs CPU: 5 s vs 2 s over the load script. Scroll and typing feel at 7680×2160 could not be measured (no xdotool; synthetic input would move the user's pointer), so it is a user hand-check.

## Instrumentation today

None. No code reads its own memory. `cv status` has no memory fields. Editor stderr goes to `/dev/null` when `install.sh` relaunches it (`install.sh:185`). The only history is spike notes like this one.

## Reduction levers (measured)

| Lever | Process | Effect |
|---|---|---|
| Stream `local_hashes` (hash per note, drop the prepared body) | daemon | removes the ~110 MiB refresh peak (VmHWM) |
| `malloc_trim(0)` after heavy work | daemon | steady 171 → ~50 MiB anon |
| `malloc_trim(0)` after image-decode bursts / when idle | editor | load −62 MiB |
| Renderer: tiny-skia | editor | idle −80 MiB, load −77 MiB; CPU ×2.5 |
| Renderer: wgpu Vulkan-only + radeon ICD | editor | idle −33 MiB, load −32 MiB |
| arena cap only | daemon | −12 MiB (not worth a change) |

Targets (−50% footprint): daemon steady ≤ 95 MiB (from ~190); editor under the load script ≤ 141 MiB (from 282), idle ≤ 77 MiB (from ~153).
