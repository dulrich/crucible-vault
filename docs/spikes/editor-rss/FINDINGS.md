# WP-8 — embed migration on a scratch copy, view parity, editor RSS (2026-10-13)

Plan `plans/v0-editor.md` WP-8 (+ audit E1). Aggregates only: counts, hashes, MiB, %, seconds, view ids, `_custom/` template names. Per-path data (picks, path lists, stderr) lived only in the scratch dir `~/.local/share/cv-wp8-scratch/`.

## Setup

- Binaries: worktree `target/release/{cv,cvault-editor,cvault-migrate-views}` at base `eef433a` + WP-8 changes.
- Copy (21:25:50–21:25:56 local): `rsync -a --exclude .obsidian --exclude .trash ~/vault/ <scratch>/vault/` — 8,101 `.md` (88 of them symlinks, all under `initiatives/`), 1.8 GiB.
- `<scratch>/config/` = only `views/` (a copy of `crates/views/defs/*.toml`).
- Daemon: `CV_MODE=live CV_VAULT=<scratch>/vault CV_SOCKET=<scratch>/run/daemon.sock CV_STATE_DB=<scratch>/state/state.db CV_INDEX_DB=<scratch>/state/index.db CV_SCRATCH=<scratch>/overlay CV_CONFIG_DIR=<scratch>/config`, `CV_OPEN_CMD` unset; `cv daemon`. `cv status` → `mode: live`, `generation: 1`, `config errors: none`. Every client used the same `CV_SOCKET`.

## Migration (pin 5)

Commands: `cvault-migrate-views --dry-run --report <scratch>/dryrun-1.json`, `cvault-migrate-views --apply --report <scratch>/apply.json`, `cvault-migrate-views --dry-run --report <scratch>/dryrun-2.json`, then `grep` re-scans over `<scratch>/vault`.

| Measure | Plan reference | Scratch |
|---|---|---|
| Notes scanned (regular `.md`; symlinks not followed) | — | 8,013 |
| Distinct `.base` targets | 13 | 13 |
| `.base` embeds | 240 | 240 |
| `query` fences (month notes + `month-template.md`) | 8 | 8 |
| Files touched (dry-run) | 228 | 228 |
| Unmapped / confinement | — | 0 / ok |
| Writes (apply) / conflicts / changed-since-scan | — | 228 / 0 / 0 |
| Fences emitted | — | 256 (= 248 occurrences + 8 restored `monthly-reading#By Day`) |
| Re-scan: `.base` embeds (`grep -rhoE '!\[\[[^]]*\.base[^]]*\]\]' --include='*.md'`) | 0 | 0 |
| Re-scan: `query` fences (`grep -rhcE '^\s*(```|~~~)\s*query' --include='*.md'`) | 0 | 0 |
| Re-scan: second `--dry-run` files touched | 0 | 0 |
| Files with `crucible-view` fences | — | 228 |
| Files with `monthly-reading#By Tag` / of those lacking `#By Day` | — | 8 / 0 |
| Templates' fences (`day`/`month`/`work-template.md`) | rewritten | 1 / 4 / 1 |

Per-target occurrences (dry-run): `blog-story-list#Stories` 21, `daily-reading-list#Reading List` 193, `hubs#Project Alpha` 1, `hubs#Hub View B` 1, `hubs#Hub View C` 1, `initiatives#Active/Blocked/Paused` 1 each, `monthly-reading#By Tag` 8, `monthly-reading#Unread` 9, `works#Active/Complete/Unread` 1 each, `query:monthly-tasks` 8. Dry-run wall: 0.19 s. The 88 symlinked notes carry 0 `.base` embeds and 0 `query` fences (`grep` over them), so not following symlinks loses nothing for the migration.

## Parity (pin 6)

Live: `obsidian base:query path=_custom/bases/<f>.base view="<View>" format=paths` against the running Obsidian, run right after the rsync (before the apply). Scratch: `cvault-migrate-views dump "<f>#<View>" --out <scratch>/parity/scratch-<id>.txt`. Hash = first 12 hex of `sort -u | sha256sum`.

| View id | Live rows | Scratch rows | Live hash | Scratch hash | Only-live / only-scratch | mtime drift | Verdict |
|---|---|---|---|---|---|---|---|
| hubs#Hub View B | 63 | 63 | 22da4ee78e76 | 22da4ee78e76 | 0 / 0 | — | pass |
| hubs#Hub View C | 90 | 90 | 448c3a3543d9 | 448c3a3543d9 | 0 / 0 | — | pass |
| hubs#Hub View A | 6 | 6 | 884e2abb93f0 | 884e2abb93f0 | 0 / 0 | — | pass |
| initiatives#Active | 24 | 0 | a5ae32237943 | e3b0c44298fc | 24 / 0 | 0 | **fail (explained: symlinks)** |
| initiatives#Blocked | 0 | 0 | e3b0c44298fc | e3b0c44298fc | 0 / 0 | — | pass (vacuous, see Active) |
| initiatives#Paused | 5 | 0 | 93adc05c6ab6 | e3b0c44298fc | 5 / 0 | 0 | **fail (explained: symlinks)** |
| works#Active | 18 | 18 | be4ff06f89d2 | be4ff06f89d2 | 0 / 0 | — | pass |
| works#Complete | 2 | 2 | a6ab21b41549 | a6ab21b41549 | 0 / 0 | — | pass |
| works#Unread | 10 | 12 | 7ba12c2108fa | efd78ce30665 | 0 / 2 | 0 | **fail (unexplained)** |

- **initiatives:** every `initiatives/*/INITIATIVE.md` in the vault is a symlink (29 of the 88 symlinked notes, all pointing outside the vault). `cv-index` walks with `follow_links(false)` (`crates/cv-index/src/index.rs:262`), so the index holds 0 `initiatives/%/INITIATIVE.md` rows (`sqlite3 -readonly <scratch>/state/index-1.db`: 8,013 `.md` files indexed of 8,101 on disk) and the views are empty. A daemon/index behaviour, not a view-definition defect; it blocks the initiatives views at cut-over (`docs/cutover.md` row E8).
- **works#Unread:** scratch has 2 extra rows, both entries 3 levels deep under `daily/` with `read: false`; mtimes equal on both sides (no drift since the copy). The `.base` Unread filter is a generated 20-branch literal and `core/works` has 20 top-level works (none born after the `.base`), so it is not a stale literal. Cause not determined without reading note content: a semantic difference between the `.base` literal as Obsidian evaluates it and WP-7's `files("core/works")` cross-file rewrite (or the evaluator). Flagged for review.
- **hubs#Project Alpha** (needs `this`): covered by WP-7's `goldens` test (`crates/views/tests/defs.rs`, host `core/projects/project-alpha/Hub.md`): the golden `hubs#Project Alpha.json` lists the folder's other note and a tagged note, not the host. Implicit in the golden (no dedicated assert).
- **daily-reading-list:** 192 host notes `daily/day/<name>.md` (192 folders, 0 without a host). `drl-dump.sh` (sum of `dump daily-reading-list#Reading List --this daily/day/<host>`): **2,714** rows, 0 failures. Independent `drl-count.sh` (direct children of `daily/day/<name>/`, the `.base` filter `file.folder == "daily/day/" + this.file.name`): **2,714** `.md` (2,716 files incl. 2 non-`.md`, which `cv` does not index; Obsidian Bases would list them). S2 reference: 2,713 over 189 folders — the vault grew by 3 folders.

## Editor RSS (pin 7, audit E1)

`run.sh <label>` (adapted `../gui-rss/run.sh`: same checkpoints, the main checkout's `runs/spike/gui-rss/{mem,cpu}.sh`), env = the scratch daemon env + `XDG_STATE_HOME=<scratch>/xdg-state` (state wiped per run), `SCRATCH`, `CV_BIN`, `EDITOR_BIN`. Workload `driver.sh`: refuses unless `realpath(CV_VAULT)` and `CV_SOCKET` are under the scratch dir; computes the S3 picks with `vaultsel` via the throwaway `picks/` bin on `<scratch>/vault` (`notes=8011 images_indexed=6603`, pick embed counts `[71, 69, 69, 67, 61, 61, 55, 54, 52, 52, 48, 42, 42, 41, 39, 37, 35, 35, 34, 33]`, `resolved_total=848` — identical to S3); `cv uri 'obsidian://open?vault=vault&file=<enc>'` for pick #1 at t=6 s, #2..#20 at t=15..33 s.

Reading mode: the editor's view mode is per tab, defaults to Source and is not persisted (`ui_state.layout` is never applied to tabs), so pre-seeded UI state cannot select it. The driver sends **Ctrl+E** (the editor's Source→Reading cycle) 0.4 s after each open with `ctrl_e.py` — `XSendEvent` to the window whose `_NET_WM_PID` is the editor's pid (no focus change, no XTEST). Check (one trial): one pick in source mode 139 MiB RSS → 191 MiB 6 s after Ctrl+E (images loading).

"20 opens occurred": `open_requested` results from `cv uri` = 20, and `editor_tabs_persisted` (tabs in `<scratch>/xdg-state/crucible-vault/editor.json`, which the editor saves on every open) = 20, in every run; `ctrl_e_sent` = 20.

RSS / PSS in MiB (kB ÷ 1024), 1 process.

| Run | Image budget | Startup (mapped) | RSS idle 5 s | RSS 1 note | RSS 20 notes | RSS 5 min | PSS 20 notes | PSS 5 min | Idle CPU 60 s | Kernel GPU lines | stderr |
|---|---|---|---|---|---|---|---|---|---|---|---|
| editor-1 | 64 MiB (default) | 2.20 s | 121 | 192 | 746 | 746 | 729 | 729 | 0.42 % | 0 | RADV banner only |
| editor-2 | 64 MiB (default) | 2.25 s | 121 | 140 | 568 | 568 | 551 | 551 | 0.00 % | 0 | RADV banner only |
| editor-cache16 | 16 MiB | 2.27 s | 121 | 192 | 780 | 780 | 763 | 763 | 0.70 % | 0 | RADV banner only |
| S3 iced (reference, run 1 / 2) | unbounded | 0.31 / 0.28 s | 159 / 153 | 206 / 201 | 389 / 384 | 389 / 386 | 340 / 334 | 340 / 337 | 0.02 / 0.00 % | 0 | RADV banner |

Verdict vs target (≤ 400 MiB RSS at 20 notes, flat at 5 min, ~0 % idle CPU): **RSS miss** (568–746 MiB at the default budget, 1.4–1.9× the target and S3); **flat at 5 min: pass** (±0.1 MiB); **idle CPU: pass/borderline** (0.00–0.70 %). The image budget does not bound RSS: at 16 MiB the 20-note RSS is no lower, so the growth sits outside `image_cache` (candidates: the renderer's image atlas/uploads and per-tab retained render models for 20 open reading tabs). Run-to-run spread is large (568 vs 746 MiB) — image loads are async, so the t=40 s sample catches varying amounts. Startup to mapped window ~2.2 s vs S3's 0.3 s. Not tuned in code (out of scope).

## Files

`run.sh`, `driver.sh`, `ctrl_e.py`, `picks/` (throwaway; path-depends on `runs/spike/gui-rss/vaultsel` in the main checkout; built into `<scratch>/picks-target`), `drl-count.sh`, `drl-dump.sh`, `results/<label>.log` (aggregates only).

## Re-measure (editor-cutover-blockers WP-2)

Binaries: the worktree's `target/release/{cv,cvault-editor}` with WP-2 applied (lazy source `Content`, Reading is the default, in-process `mallopt` arena cap, single-query explorer). The scratch daemon is the same as in WP-8 (`<scratch>/env.sh`, worktree `cv daemon`). The driver no longer sends Ctrl+E (`NO_CTRL_E=1`, `ctrl_e_sent 0`), and no `MALLOC_*` env is set.

Commands (worktree root):
- `source <scratch>/env.sh; unset MALLOC_ARENA_MAX MALLOC_TRIM_THRESHOLD_; export SCRATCH=<scratch> CV_BIN=$PWD/target/release/cv EDITOR_BIN=$PWD/target/release/cvault-editor NO_CTRL_E=1 DISPLAY=:0; runs/spike/editor-rss/run.sh cutover-<n>`
- `runs/spike/editor-rss/startup.sh wp2-startup-<n>` (same env), run 3 times
- `/proc/pressure/io` `some avg60` is read just before each run

The host was under heavy IO pressure throughout (about 61–77 % `some` avg60). An earlier cutover-1 attempt during a stall was killed and is kept as `results/cutover-1-killed.log`; its 20-note RSS was 300 MiB. Runs were not repeated to wait for lower pressure.

RSS / PSS in MiB (kB ÷ 1024), 1 process. The "20 notes" values are taken at t=40 s.

| Run | IO avg60 before | Startup (run.sh) | RSS idle 5 s | RSS 1 note | RSS 20 notes | RSS 5 min | PSS 20 notes | PSS 5 min | Idle CPU 60 s | open_requested / tabs persisted | Kernel GPU lines | stderr |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| cutover-1 | 74.6 % | 0.133 s | 121.9 | 171.8 | 290.9 | 290.6 | 273.9 | 273.5 | 0.67 % | 20 / 20 | 0 | 1 line (Vulkan banner) |
| cutover-2 | 76.8 % | 0.146 s | 122.0 | 173.2 | 301.0 | 347.0 | 284.0 | 320.7 | **231.97 %** | 20 / **21** | 0 | 1 line (Vulkan banner) |

smaps split at 20 notes, `smaps40`, in MiB:

| Run | heap | gpu_driver | anon | file_libs | memfd_shm | other |
|---|---|---|---|---|---|---|
| cutover-1 | 116.6 | 77.1 | 62.3 | 34.6 | 0.0 | 0.3 |
| cutover-2 | 118.6 | 77.1 | 69.5 | 35.5 | 0.0 | 0.3 |

Startup to the mapped window, using `startup.sh` against the 8k-note scratch daemon:

| Run | IO avg60 before | mapped_s |
|---|---|---|
| wp2-startup-1 | 61.0 % | 0.147 |
| wp2-startup-2 | 62.0 % | 0.148 |
| wp2-startup-3 | 62.2 % | 0.140 |

WP-8 measured 2.20–2.25 s here, mostly the quadratic explorer build.

Compared with WP-8 at the default budget, RSS at 20 notes fell from 568–746 MiB to 291–301 MiB, a 47–61 % reduction. That is below the S3 bare-iced reference of 384–389 MiB.

**Target verdict:**
- **≤ 400 MiB at 20 notes: pass.** Both runs measured 291–301 MiB.
- **Flat at 5 min: pass for cutover-1 (−0.3 MiB), miss for cutover-2 (+46 MiB).**
- **About 0 % idle CPU: pass for cutover-1 (0.67 %), miss for cutover-2 (232 %).**
- **Startup ≤ 0.5 s: pass.** All runs took 0.133–0.148 s.

**Overall: a hold for the orchestrator because of the cutover-2 anomaly.** In cutover-2 the editor persisted 21 tabs after 20 driver opens. `run.sh` wipes the UI state before each run, so one extra note was opened by something other than the driver. In the same run the editor used about 2.3 cores in the t=220–280 s idle window and grew 46 MiB by 5 min. The cause is not attributed. Candidates are stray input to the editor window, which is on the user's live screen during runs, or editor-side work triggered by that extra open, such as a large note or image decode. The growth is mostly heap and anon, while gpu_driver and file_libs are unchanged at 40 s. cutover-1 was clean on every target. A third run was not made, because the brief forbids retrying for pressure or tuning further.

## Re-verify (editor-cutover-blockers WP-3)

Binaries: the WP-3 worktree's `target/release/{cv,cvault-migrate-views}` at base `1bb30ef` (WP-1 + WP-2), built with `cargo build --release -p cv -p editor`. Scratch: `~/.local/share/cv-cutover-scratch/` (fresh; `<scratch>` below).

### Setup

- Copy (00:49:05–00:49:10 local): `rsync -a --exclude .obsidian --exclude .trash ~/vault/ <scratch>/vault/` — 8,101 `.md` (88 symlinks), 30 `initiatives/` entries.
- `<scratch>/config/views/` = copy of `crates/views/defs/*.toml`, nothing else. `<scratch>/env.sh` = the WP-8 isolation env with every path under `<scratch>`, `CV_OPEN_CMD` and `CV_LINK_WRITE_ROOTS` unset, `PATH` led by the worktree's `target/release`.
- Daemon: `source <scratch>/env.sh; exec cv daemon` (harness-owned background task); `cv status` → `mode: live`, `generation: 1`, `config errors: none`. Daemon runs: #1 parity + migration (links unset), #2 works#Unread variant (links unset), #3 refusal (links unset), #4 write-through (`CV_LINK_WRITE_ROOTS=<scratch>/standin`, verified in `/proc/<pid>/environ`).
- Live capture note: `obsidian base:query … format=paths` prints nothing when stdout is a pipe; captured as `script -q -c 'obsidian base:query path=_custom/bases/<f>.base view="<View>" format=paths' /dev/null | tr -d '\r'`, then the RADV `WARNING: radv is not a conformant Vulkan implementation` banner line was filtered out (it appeared once, in the works#Unread capture). Live captured 00:50:51, 1.7 min after the rsync.

### Migration (check 1)

Commands: `cvault-migrate-views --dry-run --report <scratch>/dryrun-1.json`, `--apply --report <scratch>/apply.json`, `--dry-run --report <scratch>/dryrun-2.json`, then the WP-8 `grep` re-scans.

| Measure | WP-8 | WP-3 |
|---|---|---|
| Notes scanned (regular `.md`) | 8,013 | 8,013 |
| Distinct `.base` targets | 13 | 13 (14 incl. `query:monthly-tasks`) |
| `.base` embeds | 240 | 240 |
| `query` fences | 8 | 8 |
| Files touched (dry-run) | 228 | 228 |
| Unmapped / confinement | 0 / ok | 0 / ok |
| Writes / conflicts / changed-since-scan | 228 / 0 / 0 | 228 / 0 / 0 |
| `LinkWriteRefused` failures | — | 0 (the 88 symlinked notes carry 0 `.base` embeds and 0 `query` fences: `find vault -name '*.md' -type l -print0 \| xargs -0 grep -lE …` → 0 files) |
| Second dry-run files touched / occurrences | 0 | 0 / 0 |
| Re-scan `.base` embeds / `query` fences | 0 / 0 | 0 / 0 |
| Files with `crucible-view` fences | 228 | 228 |
| Files with `By Tag` lacking `By Day` | 0 | 0 |

Per-target occurrences identical to WP-8 (`daily-reading-list#Reading List` 193, `blog-story-list#Stories` 21, `monthly-reading#Unread` 9, `monthly-reading#By Tag` 8, `query:monthly-tasks` 8, the 8 single-embed targets 1 each). No drift.

### Parity (check 2)

Hash = first 12 hex of `sort -u | sha256sum`. Scratch: `cvault-migrate-views dump "<f>#<View>" --out <scratch>/parity/scratch-<id>.txt`, run before the apply.

| View id | Live rows | Scratch rows | Live hash | Scratch hash | Only-live / only-scratch | Verdict |
|---|---|---|---|---|---|---|
| hubs#Hub View B | 63 | 63 | 22da4ee78e76 | 22da4ee78e76 | 0 / 0 | pass |
| hubs#Hub View C | 90 | 90 | 448c3a3543d9 | 448c3a3543d9 | 0 / 0 | pass |
| hubs#Hub View A | 6 | 6 | 884e2abb93f0 | 884e2abb93f0 | 0 / 0 | pass |
| initiatives#Active | 24 | 24 | a5ae32237943 | a5ae32237943 | 0 / 0 | **pass** (WP-8: 0 scratch rows) |
| initiatives#Blocked | 0 | 0 | e3b0c44298fc | e3b0c44298fc | 0 / 0 | pass |
| initiatives#Paused | 5 | 5 | 93adc05c6ab6 | 93adc05c6ab6 | 0 / 0 | **pass** (WP-8: 0 scratch rows) |
| works#Active | 18 | 18 | be4ff06f89d2 | be4ff06f89d2 | 0 / 0 | pass |
| works#Complete | 2 | 2 | a6ab21b41549 | a6ab21b41549 | 0 / 0 | pass |
| works#Unread | 10 | 12 | 7ba12c2108fa | efd78ce30665 | 0 / 2 | **pass (intended +2)** |

- works#Unread: scratch hash `efd78ce30665` = the `cutover-blockers` §Q1 control hash; both extras have equal mtimes live vs scratch (no drift). Variant recompute: added `!value.properties["author-filter"].toString().contains("[[") && ` to the `files("core/works").filter(value.folder == "core/works" && …` clause in `<scratch>/config/views/works.toml`, restarted the daemon, `dump "works#Unread"` → **10 rows, `7ba12c2108fa` = live**. Config restored (`diff -r crates/views/defs <scratch>/config/views` empty).

### Symlink write-through (check 3)

Write client: throwaway `runs/spike/cutover-blockers/write_client.py <vault-rel path> <content-file> [--stale]` — `hello` + `write_note` over `CV_SOCKET` (newline-delimited JSON-RPC), `base_hash` = sha256 of the file read through `CV_VAULT` (`--stale` sends 64 zeros); refuses unless `CV_VAULT`/`CV_SOCKET` resolve under the scratch dir; prints only the outcome with the path redacted. Synthetic fixtures (no vault content): `<scratch>/standin/INITIATIVE.md`, `<scratch>/outside/INITIATIVE.md`, linked by two new symlinks `<scratch>/vault/initiatives/zz-standin/INITIATIVE.md` and `…/zz-outside/INITIATIVE.md` (no existing link retargeted).

- **(a) refusal, links unset.** The literal check (a write RPC through an existing real-repo `initiatives/*/INITIATIVE.md` link, identical content, sha256 before/after) was **not run**: the session's permission classifier denied the command. Substitute run instead (daemon #3, links unset): a CAS write through each synthetic link (targets outside the vault and every link-write root) → `{"outcome":"error","code":-32603,"message":"write through the symlinked note <path> refused: the link target is outside the vault root and every link-write root"}` for both; target sha256 unchanged (`00af7ed2f453` → `00af7ed2f453`, `0cd27c6cd920` → `0cd27c6cd920`); both still `symbolic link`. Note: `LinkWriteRefused` reaches the client as JSON-RPC `INTERNAL_ERROR` (-32603) with that message text, not as a distinct error code. The real-repo case follows the same `write_target` branch (`crates/cv-vault/src/fs.rs`, the `!allowed` refusal), but it is unmeasured here.
- **(b) write-through, `CV_LINK_WRITE_ROOTS=<scratch>/standin`** (daemon #4): CAS write → `{"outcome":"written"}`; target `00af7ed2f453` → `032ab0087bb3` (= sha256 of the sent content); `stat -c %F` → `symbolic link`. Stale-hash write → `{"outcome":"conflict"}`, target unchanged (`032ab0087bb3`), still a symbolic link. The `zz-outside` link is still refused under this daemon.
- **(c) external edit** (daemon #4): `sed -i` on `<scratch>/standin/INITIATIVE.md` adding `initiative-status: paused` (target `3165e2287a09` → `5a6510fc385d`): within 5 s `dump initiatives#Active` 26 → 25 and `initiatives#Paused` 5 → 6, with the stand-in moving from Active to Paused (counts include the two synthetic notes); the index row's `initiative-status` prop = `"paused"`. `cv status` generation stayed `1` — incremental updates do not bump the generation, so the view result is the evidence. (A first attempt edited the `status` key, which the view does not read; the index picked it up, the view correctly did not move.)

### Processes

Every `cv daemon` started here was stopped (`kill -TERM`, verified by `/proc/<pid>/exe`); only the user's `~/.cargo/bin/cv` remains. `<scratch>` is left in place, including the synthetic `zz-standin`/`zz-outside` links and fixtures.
