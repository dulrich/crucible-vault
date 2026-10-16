# editor-usability-round WP-5 — Panels lag, launch focus, idle CPU (synthetic vault only)

All runs use a synthetic vault generated here; nothing reads the live vault or talks to the live daemon.

## Corpus

`python3 -I gen.py vault 8123 2802` → 8,123 notes in 2,709 note-holding folders (2,802 generated, depth 1–4; some stay empty) — live is 8,123 notes / 2,802 directories. Each note: frontmatter (title, tags, kind), 2–10 wikilinks to random notes, a `#tag`, two tasks. Synthetic text only.

`python3 -I gen.py vault-flat 8123 0` → the same 8,123 notes in one folder (cv-testkit-style), for the cardinality comparison (audit E2).

## Panels open latency

Harness: `panels_open_latency` in `crates/editor/src/panels/tests.rs` (`#[ignore]`d, not a gate). It builds a real `cv-index` index of the corpus via `FakeDaemon::open` (same index code as the daemon), opens one note, bumps the index generation (as any save does), and times `PanelsMsg::Toggle` → `sync_panels` through `App::update` (synchronous panel data build). Three rounds.

```
CV_SPIKE_VAULT=$PWD/runs/spike/panels-lag/vault      cargo test -p editor --release --lib panels_open_latency -- --ignored --nocapture
CV_SPIKE_VAULT=$PWD/runs/spike/panels-lag/vault-flat cargo test -p editor --release --lib panels_open_latency -- --ignored --nocapture
```

| corpus | before | after |
|---|---|---|
| nested (8,123 notes / 2,709 folders) | 1194.8 / 1193.2 / 1205.6 ms | 11.9 / 14.0 / 11.4 ms |
| flat (8,123 notes / 1 folder) | 15.8 / 17.2 / 14.6 ms | 11.7 / 14.3 / 10.7 ms |

Root cause: `Panels::refresh_resolver` (`crates/editor/src/panels/mod.rs`) built the link resolver by walking the folder tree with a `folder_files` + `subfolders` query per folder; `subfolders` scans every distinct folder, so the walk is quadratic in folder count — invisible on the single-folder cv-testkit shape. It reruns on every Panels open after any generation change (any save). The explorer and Reading resolver had already dropped the same walk (editor-cutover-blockers WP-2); the fix is the same one-scan `Query::file_paths()`. Target ≤ 100 ms met (~12 ms). Cause was editor-side; no daemon change.

Tracing: `tracing::debug!` with `elapsed_ms` on `sync_panels`, `sync_buffers` (when ≥ 1 ms), `panels sync_buffer`, `panels sync_tags`, `panels refresh_resolver` (view with `RUST_LOG=editor=debug`).

## Launch focus (X11 `:0`, i3)

`env.sh` holds the scratch `CV_*` + `XDG_STATE_HOME`; daemon = `target/release/cv daemon` on `runs/spike/panels-lag/cv.sock` (live mode on the synthetic vault); editor = `target/release/cvault-editor` with a pre-seeded one-tab `editor.json`.

```
xprop -root _NET_ACTIVE_WINDOW          # before launch: 0xa00004 (the user's terminal)
cvault-editor                            # then within ~1 s:
xprop -root _NET_ACTIVE_WINDOW          # 0x1c00002 = the editor (_NET_WM_PID = editor pid)
xprop -id 0x1c00002 WM_HINTS _NET_WM_STATE _NET_WM_USER_TIME WM_CLASS
#   WM_HINTS not found; _NET_WM_STATE_FOCUSED; _NET_WM_USER_TIME not found; WM_CLASS "", ""
xprop -id $(xprop -root _NET_SUPPORTING_WM_CHECK | awk '{print $NF}') _NET_WM_NAME   # "i3"
```

Finding: the window becomes active on map — i3's default for new windows. iced 0.14 `window::Settings` has no activation/focus field, so it is not editor-side; the user can opt out in i3 (`no_focus [title="crucible-vault"]`; `WM_CLASS` is empty). Whether a click is captured during mapping could not be observed (no xdotool; synthesising input would move the user's real pointer) — **needs one manual launch check by the user.**

## Idle CPU

Editor against the synthetic daemon, one note open in Reading (default mode; restored from `editor.json`, not click-opened), no input:

```
./cpu.sh <editor pid> 30          # pid=2781494 window=30s ticks=10 cpu=0.33%
pidstat -p <editor pid> 5 2       # Average: %usr 0.30 %system 0.10 %CPU 0.40
```

No busy loop. The remaining cost is the Reading 250 ms debounce tick (no-op when nothing is stale). The palette toast tick runs only while a toast is up, the panels tick only while panels are visible, the find listener only while a find bar is open.

## Pointer-move storm

Before: the source editor sat in a `mouse_area` whose `on_move` published `SourceMsg::Pointer` per motion event (full update + view each), only to remember a later middle-click position. After: `crates/editor/src/widget/middle_click.rs` reads the cursor position at middle-press time and publishes one `SourceMsg::MiddleClick { path, at }` — zero messages for plain pointer motion. PRIMARY paste unchanged (click at `at` − padding, then read PRIMARY). Not timed live (no pointer synthesis without moving the user's pointer).

## Processes

Daemon (pid 2780590) and editor (pid 2781494) ran via `exec` in harness-owned shells; stopped with `kill <pid>`; `ps -p` confirms both gone.
