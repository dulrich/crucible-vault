# S3 — GUI stack RSS / stability shoot-out (2026-10-13)

Host: X11 `DISPLAY=:0`, RX 9070 (RADV gfx1201), 24 cores, 61 GiB. Release builds, `strip = true`. Throwaway spike code; aggregates only.

## Workload (identical across stacks)
- Shared crate `vaultsel/` loads the vault read-only (`.md` notes excluding `.obsidian/`, `_resources/`, `.git`, `.trash`; image files indexed vault-wide by name): **8008 notes, 6602 images indexed**.
- Deterministic 20 picks: image-embed count desc, then size desc, then path. Embed counts of picks: `[71, 69, 69, 67, 61, 61, 55, 54, 52, 52, 48, 42, 42, 41, 39, 37, 35, 35, 34, 33]`; 848 embeds resolve to local files (rest remote/unresolved).
- Each app: list of all 8008 notes (labelled `note N`, never titles) + markdown render of the open note with its local images + editable buffer with the note source (memory only, never saved).
- `SPIKE_AUTO=1`: opens pick #1 at t=6 s, picks #2..#20 at t=15..33 s (1 s apart). `SPIKE_PASTE=1`: 1 MB paste at t=40 s.
- Images: embeds rewritten to absolute local paths (`vaultsel::rewrite_embeds`). iced: custom `markdown::Viewer::image` -> `image` widget. GPUI: `TextView` only loads images over HTTP, so images are laid out as `img(path)` elements under the text (same pixel load, different layout). Tauri: `marked` + `convertFileSrc` (asset protocol scoped to the vault).
- Rendering verified visually on a **synthetic** 4-note vault (`/tmp/spike-synth-vault`, `SPIKE_VAULT=`): all three show headings, lists, all 4 images.

## Commands
```bash
cd runs/spike/gui-rss
./run.sh <label> <window-title> <binary>   # ~6 min: startup, mem@5s/12s/40s, CPU 280-340s, mem@340s, kernel log, stderr
./paste.sh <label> <window-title> <binary> # startup + 1 MB paste at t=40s + mem after
./mem.sh <pid...>      # Rss/Pss summed over /proc/<pid>/smaps_rollup across the process tree (task/*/children walk)
./cpu.sh 60 <pid...>   # utime+stime delta over the tree, % of one core
```
Labels: `iced-1/2`, `gpui-1/2`, `tauri-1/2`, `paste-<stack>-1/2`; raw logs in `results/`.
- Checkpoints: idle = t=5 s; 1 note = t=12 s; 20 notes = t=40 s; 5 min idle = t=340 s. Idle CPU = t=280–340 s.
- Startup = fork -> titled window `Map State: IsViewable` (`xwininfo -name`, polled 5–10 ms) — window mapped, not first content paint. iced-1/2 used `wmctrl -l` before the switch; `paste.sh` re-measured iced with xwininfo (0.31/0.28 s).
- Kernel: `journalctl -k --since <run start>` grepped `amdgpu|ring.*timeout|gpu reset|vulkan|drm`; app stderr grepped for Vulkan/validation.

## Results (MiB, run1 / run2)

| Stack | Procs | RSS idle 5s | RSS 1 note | RSS 20 notes | RSS 5 min | PSS 20 notes | PSS 5 min | Idle CPU 60s | Startup (mapped) | Binary | Driver errors |
|---|---|---|---|---|---|---|---|---|---|---|---|
| iced 0.14.0 (wgpu) | 1 | 159 / 153 | 206 / 201 | 389 / 384 | 389 / 386 | 340 / 334 | 340 / 337 | 0.02% / 0.00% | 0.34/0.18 s (wmctrl); 0.31/0.28 s (xwininfo) | 19.3 MiB | 0 kernel; stderr only RADV "not conformant" banner |
| GPUI (gpui-pre 0.3.9 + gpui-component 0.7.3) | 1 | 130 / 129 | 714 / 712 | **4913 / 4898** | **4913 / 4898** | 4866 / 4851 | 4866 / 4851 | 0.25% / 0.27% | 0.14 / 0.12 s | 25.6 MiB | 0 kernel; stderr only RADV banner |
| Tauri 2.12.3 + CM6 (WebKitGTK 4.1) | 3 (app+WebKit Web+Network) | 454 / 449 | 599 / 612 | 1041 / 1017 | 1003 / 980 | 802 / 778 | 764 / 741 | 0.23% / 0.22% | 0.18 / 0.18 s | 7.3 MiB (+system WebKitGTK) | 0 kernel; no stderr warnings |
| **Obsidian baseline** (user's running instance, untouched) | 9 | — | — | — | 1019→1018 (4 samples) | — | 613→609 | 0.02% | n/a | n/a | n/a |

- Obsidian samples (`./mem.sh $(pgrep -f /opt/Obsidian/obsidian)`), KiB: 14:25:05 RSS 1044440 / PSS 627779 (uptime 1:12:42); 14:31:03 1042656 / 627628 (1:18:41); 14:36:36 1042196 / 627531 (1:24:13); 14:58:12 1042632 / 623447 (1:45:49). **Workload not identical**: whatever the user has open, plugins, ~1.75 h uptime, none of the scripted 20-note image load.
- Kernel log 14:24 -> end of all runs: 0 amdgpu/ring-timeout/reset/DRM lines. No Vulkan validation output. No crashes in 10 runs.
- Run-to-run spread: RSS within 5 %, startup within 0.03 s (except first iced wmctrl sample).

### 1 MB paste (`paste.sh`, 2 runs, appended to note #20's buffer)
| Stack | Paste op | RSS / PSS after (MiB) | Note |
|---|---|---|---|
| iced `text_editor` | 0.60 / 0.57 ms (perform only; layout lazy) | 594 / 545 | +205 MiB over 20-note state |
| GPUI `EditorState::insert` | 219 / 219 ms (synchronous, main thread) | **10965 / 7934** | ~+6 GiB over 20-note state |
| Tauri CM6 `dispatch` | 48 / 49 ms | 1011 / 772 | flat — CM6 virtualizes rendering |

## Notable
- **GPUI: >4.8 GiB after 20 image-heavy notes, never released.** Idle ~130 MiB (lowest), but `img(path)` keeps every full-resolution decoded image in its asset cache with no eviction, and the gpui-component editor adds ~6 GiB on a 1 MB paste. Likely library defaults, not inherent to GPUI — a real app needs own downscaling + bounded image cache and a different/fixed editor. That is engineering you'd own.
- **iced**: leanest under load (~390 MiB at 20 notes, flat at 5 min), ~zero idle CPU (no idle redraws). Image cache grows (+180 MiB over 19 notes) but bounded here.
- **Tauri**: ~450 MiB WebKit floor across 3 processes, ~1.0 GiB at 20 notes, −40 MiB after 5 min (WebKit GC) — about the running Obsidian's RSS (different workload).
- Idle CPU negligible everywhere (≤0.3 % of one core).

## Build friction
| Stack | Clean release build | Cargo.lock entries | System deps | Maturity |
|---|---|---|---|---|
| iced 0.14.0 | 39 s | 467 | none beyond existing Vulkan/xkbcommon runtime | Stable crates.io release; `markdown`+`image`+`text_editor` built in; `markdown::Viewer` makes local images easy. No virtualized list — the 8008-row list rebuilds every `view` (worked, but a cost). |
| GPUI | 51 s | 789 | `libxkbcommon-x11-dev`, `libx11-xcb-dev` (installed mid-spike) | Zed git `gpui` has no editor/markdown widgets, so used **`gpui-pre =0.3.9` (crates.io snapshot of zed rev `93d745668805029476e6d7fbb19652116fed51de`, Zed gpui 0.2.2)** as pinned by **`gpui-component =0.7.3`** — deviation from "git dep pinned to a commit" because gpui-component pins its own gpui. Pitfall: `gpui-pre-platform` builds **headless with no window and no error** unless `features = ["x11"]`. Fast-moving API (Rust 2024 `gen` keyword clash, huge gpui-component surface). `uniform_list` gives a virtual list for free. |
| Tauri 2.12.3 + CM6 | 49 s (+ <1 s `bun build`) | 409 | `libwebkit2gtk-4.1-dev`, `libgtk-3-dev`, `libsoup-3.0-dev`, `libjavascriptcoregtk-4.1-dev` | **`npm` absent**; used `/usr/local/bin/bun` (`bun add codemirror@6.0.2 @codemirror/lang-markdown@6.5.2 marked@14.1.4`; `bun build --minify` -> 0.66 MB bundle). Asset-protocol scope must list every image root. CM6 + WebKit mature. |

## Editing feel — for the user to verify hands-on
| | iced `text_editor` 0.14 | gpui-component editor 0.7.3 | CodeMirror 6 |
|---|---|---|---|
| Selection | yes (`Action::Select`, word/line, drag) | yes | yes |
| Undo/redo | **no** — iced 0.14 has no undo stack (no "undo" in `iced_widget`/`iced_core` text code); app must own it | yes (`undo_manager.rs`) | yes (`history` in basicSetup) |
| IME | `input_method` support in `text_editor` (23 refs); winit composition | XIM handler on X11 (`x11/xim_handler.rs`, `zed-xim`), text-input on Wayland | browser/WebKit IME (mature) |
| 1 MB paste | 0.6 ms, layout lazy | 219 ms sync, +6 GiB | 48 ms, flat memory |
| Shaping/ligatures | cosmic-text (rustybuzz); font ligatures | Zed text system (HarfBuzz-class); font ligatures | full browser shaping |

`xdotool` and `xclip` are **absent**, so pastes were in-app via each widget's API, not the real clipboard/keyboard path.

Manual 3-step check per stack (`SPIKE_VAULT=/tmp/spike-synth-vault <binary>`, or the live vault, read-only):
1. Click into the editor, type a line, select a word (shift+arrows, double-click), press Ctrl+Z then Ctrl+Shift+Z/Ctrl+Y — expect iced not to undo.
2. With an IME active (ibus/fcitx, CJK or compose), type a composed character: preedit should show inline and commit.
3. Copy ~1 MB of text from another app, paste, scroll and type at the end; watch latency and `./mem.sh <pid>`. Type `->` `!=` `fi` in a ligature font to check shaping.

## Trade-offs (user decides)
- **iced** — lowest memory under load, ~zero idle CPU, stable release, everything built in; editor is thin (no undo, basic). Expect to write own undo, virtual list, probably a better editor widget.
- **GPUI** — fastest startup and smallest idle footprint, rich editor (undo, IME, Zed lineage), virtual list; out-of-box image + editor memory behaviour is pathological here (4.9–11 GiB). Pre-1.0 snapshot pinned via a third-party component library; silently headless without the right feature.
- **Tauri + CM6** — best editor (undo, IME, virtualization, mature markdown tooling) and easiest markdown/image rendering; ~450 MiB WebKit floor, ~1 GiB under load ≈ Obsidian's footprint; needs a JS toolchain (no npm here; bun works).

## Incident (vault opacity)
While screenshotting the synthetic vault, a title-based window lookup (`wmctrl -l | grep spike-iced`) matched the concurrently running **live-vault** iced measurement window, so one screenshot of a live note was captured and viewed by the agent. The file was deleted immediately; no content from it is recorded here or anywhere. Later lookups used fresh window IDs only.
