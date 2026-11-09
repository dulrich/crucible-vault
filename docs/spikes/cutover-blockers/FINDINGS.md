# Spike — editor cut-over blockers: works#Unread parity + editor RSS/startup attribution (2026-10-13)

Worktree `.claude/worktrees/spike-cutover` (base `e974901`), binaries `target/release/{cv,cvault-editor,cvault-migrate-views}` built there. Scratch copy `$SCRATCH_BASE/cv-wp8-scratch/` (`source env.sh`; already migrated by WP-8). Aggregates only; per-path data under `<scratch>/spike/`. The user's default-socket daemon was never contacted.

## Q1 — works#Unread 12 scratch vs 10 live

- Extra rows: `comm -13 <(sort -u live-works#Unread.txt) <(sort -u scratch-works#Unread.txt)` → 2 (written to `<scratch>/spike/extra.txt`).
- Frontmatter shapes (python over the scratch notes, names/types only): both extras have `read: false` (YAML boolean, same as live rows), `source` a plain string, `author` a block list whose single item is a **double-quoted wikilink** `"[[…]]"`.
- Matching (python re-implementation of `matchClause` over the 20 `<works folder>/*.md`, `<scratch>/spike/q1-match.txt`): both extras match the same work (#14 of 20 sorted) via the **author clause only** (work has `author-filter` set, 0 source prefixes). The work's `author-filter` is also a double-quoted wikilink string `"[[…]]"`; the entry's author item equals it exactly as a string (no `#`, case or whitespace difference). The link target resolves to an existing note.
- Work author-filter shapes: 17 none, 3 wikilink, 0 plain, 0 `#`. Attribution of live rows: live 10 rows = 10 matched by prefix clauses (works #0,#2,#3,#6×2,#9,#12×3,#16); 0 live rows come from any wikilink-author work. Scratch 12 = the same 10 + 2 via work #14's wikilink author.
- Conclusion: Obsidian Bases types a `"[[…]]"` frontmatter value as a Link, and `list(note.author).map(value.toString()…)` vs `file(...).properties["author-filter"].toString()` do not compare equal there (no wikilink-author work matches any entry in live); `views` treats both as plain strings `"[[…]]"` (frontmatter props are strings; `Value::Link` arises only from `file.links`/`link()`, `crates/views/src/db.rs:189-210`), so they match.
- Confirmation: added `!value.properties["author-filter"].toString().contains("[[") && ` to the `files("<works folder>").filter(value.folder == "<works folder>" && …` clause in `<scratch>/config/views/works.toml`, started `target/release/cv daemon` on the scratch env, `cvault-migrate-views dump "works#Unread" --out <scratch>/spike/variant-unread.txt` → **10 rows, hash `7ba12c2108fa`** (= live). Control with the unmodified def on a fresh daemon → 12 rows, `efd78ce30665`. Config restored (`diff -r crates/views/defs <scratch>/config/views` → identical).

## Q2 — startup

`startup.sh <label>`: launch → `xwininfo -name crucible-vault` IsViewable, plus throwaway `eprintln!` spans (main.rs: main/connected/app_new_done/boot_called/first_view; app.rs `App::new`: around `refresh_files()` and `load_bookmarks()`). `perf_event_paranoid=4` → perf not used. Log: `results/startup.log`.

| Run | Mapped (s) | refresh_files (ms) | App::new done (ms) | first view (ms) |
|---|---|---|---|---|
| synth 5-note daemon, run 1 (cold shader/pipeline caches) | 1.006 | 0 | 691 | 996 |
| synth run 2 / 3 | 0.151 / 0.145 | 0 / 0 | 39 / 37 | 135 / 135 |
| scratch 8k daemon run 1 / 2 / 3 | 2.230 / 2.124 / 2.169 | 2091 / 1999 / 2033 | 2128 / 2034 / 2071 | 2216 / 2116 / 2160 |

- Dominant phase: `App::refresh_files` → `Explorer::build` (`crates/editor/src/explorer.rs:121-148`), which calls `q.subfolders(folder)` once per folder; `Query::subfolders` (`crates/cv-index/src/query.rs:268-290`) runs `SELECT DISTINCT folder FROM files` over the whole table and filters/sorts in Rust on every call → O(F²): scratch index has 15,507 `files` rows, 2,700 distinct folders (`sqlite3 "file:index-1.db?immutable=1"`); one DISTINCT query costs ~3 ms in the sqlite3 CLI, ×2,700 ≈ 2 s. Everything else (connect, bookmarks, iced boot, first view) ≈ 0.13 s, matching S3's 0.31 s.

## Q2 — RSS attribution

`run.sh <label>` = WP-8 workload (`driver.sh`: same picks, `cv uri` at t=6, 15..33 s, Ctrl+E 0.4 s after each unless `NO_CTRL_E=1`), **tail shortened to t=90 s** (WP-8 established 5-min flatness; here t=60 and t=90 equal t=40 within 0.3 MiB in every run). `smaps.py` sums `/proc/<pid>/smaps` Rss by category: `[heap]`, anonymous, GPU driver (`/dev/dri`, amdgpu/radv/drm/mesa/LLVM libs), other file-backed libs. Run B used throwaway `SPIKE_REPLACE=1` (app.rs `open()` → `open_into(.., replace=true)`, which replaces the active tab and calls `on_tab_closed` on the old one) because the editor binds no close-tab key (tab close is only `TabsMsg::Close`, a mouse action). All runs: `open_requested 20`, ctrl_e 20 (A: 0), tabs persisted 20 (B/A1: 1).

RSS MiB (kB/1024); smaps at the 20-note checkpoint (t=40 s), MiB.

| Run | Tabs / mode | after 1 | RSS@40 s | RSS@90 s | heap | anon | GPU drv | libs |
|---|---|---|---|---|---|---|---|---|
| A source | 20 / Source | 140.9 | 569.0 | 569.3 | 447.9 | 14.1 | 77.0 | 29.8 |
| A1 source 1 tab | 1 / Source | 139.4 | 230.7 | 230.7 | 119.6 | 3.8 | 77.0 | 30.0 |
| A malloc | 20 / Source | 138.9 | 517.5 | 517.5 | 394.3 | 15.7 | 77.0 | 30.2 |
| B reading 1 tab | 1 / Reading | 198.7 | 448.6 | 448.3 | 216.4 | 119.4 | 77.0 | 35.5 |
| B malloc | 1 / Reading | 188.6 | 345.5 | 345.2 | 222.8 | 9.9 | 77.0 | 35.5 |
| C reading 20 tabs #1 | 20 / Reading | 202.5 | 776.5 | 776.3 | 474.5 | 189.3 | 77.0 | 35.4 |
| C reading 20 tabs #2 | 20 / Reading | 198.8 | 769.4 | 769.2 | 475.2 | 181.2 | 77.0 | 35.7 |
| C malloc | 20 / Reading | 188.8 | 641.1 | 641.0 | 470.2 | 57.9 | 77.0 | 35.7 |

malloc = `MALLOC_ARENA_MAX=2 MALLOC_TRIM_THRESHOLD_=131072`. Idle 5 s: 121.6–122.0 MiB in all runs. Startup in these runs 2.15–2.21 s. "other" 0.25 MiB, memfd 0.01 MiB everywhere.

- Per-tab cost: Source (A) (582,648 − 144,316) kB / 19 = **22.5 MiB/tab**; Reading (C) 30.2 / 30.0 MiB/tab → reading adds ~7.6 MiB/tab, of which glibc non-main-arena retention ~6.4 MiB/tab ((C anon − C-malloc anon)/20).
- The 20 picks total 1,316,386 bytes / 14,843 lines (`wc -lc`) → ~64 KiB text per tab, so a Source tab costs ~350 bytes of heap per byte of text.
- GPU driver mappings are a constant 77 MiB; file libs 30–36 MiB; neither grows with tabs.
- Arena variant: C −130 MiB (anon 185 → 58), B −103 MiB, A −52 MiB (heap). It does not touch the per-tab Source cost.
- Single-tab shapes still grow: A1 +91 MiB and B +250 MiB over their first note with only one tab alive — heap/arena retention after tab replacement (freed but not returned) plus process-wide caches (cosmic-text font/shape caches, iced glyph/image atlases); B-malloc brings B to 345 MiB (≤ 400).

## Q2 — Source mode default / layout unused

- `ViewMode` `#[default] Source` (`crates/editor/src/reading/mod.rs:33-40`); a tab with no `ReadingStore` entry reads as default (`reading/mod.rs:111-113`, `set_mode` `or_default()` at :140/:182). `UiState.layout` (`crates/editor/src/ui_state.rs:17-22, 32`) is loaded/saved but never read: no `ui.layout` reference in `crates/editor/src` outside tests (`grep -rn '\.layout\b'`); `App::new` (`app.rs:212-216`) restores only `ui.panes`.

## Files

`run.sh`, `driver.sh` (WP-8 copy + `NO_CTRL_E`), `ctrl_e.py` (copy), `smaps.py`, `startup.sh`, `results/*.log`. Throwaway source instrumentation (reverted): `crates/editor/src/main.rs` (SPIKE timing spans, `spike_view` wrapper), `crates/editor/src/app.rs` (`SPIKE_REPLACE` in `open()`, spans around `refresh_files`/`load_bookmarks`).
