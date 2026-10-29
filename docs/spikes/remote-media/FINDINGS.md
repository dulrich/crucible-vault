# Remote-media spike (2026-10-29)

Measured for `plans/remote-media-sprint.md`. The live vault was read read-only, `.trash/` and `.git/` were excluded, and only aggregates are recorded. The probes were throwaway Rust binaries linking `cv-text` (`runs/spike/remote-media/`, prunable). Plan WP-4's `cv media audit` is the reproducible, tracked version of these counts.

## 1. Remote image embeds left (markdown image syntax, outside fenced code)

A robust image regex (one level of nested `[]` in the alt, balanced parens in the URL, a title with escaped quotes) finds **50** remote `![…](http…)` refs that are not YouTube/X embed pages. Of these:

- **26 (13 notes) are invisible to `cv_text::attachments::parse_attachment_refs` / `has_remote_image_refs`.** 17 have a CommonMark title containing escaped `\"` (e.g. Substack clips, where the title repeats the alt). `remote_md_image_re`'s title alternative `"[^"\n]*"` rejects that. The other 9 have nested `[…]` inside the alt. So neither localize-on-edit nor the sweep ever selects these notes. The example note the user named (5 substackcdn images) is in this class. All 26 URLs answer `206 image/*` today (`curl -r 0-0`).
- **24 are detected but remain.** By host response today: 10 answer `image/jpeg`; 5 answer `text/html` (a page, not an image); 6 answer `401` (expired GitHub private-image tokens); 1 answers `403`; 2 answer `404`. The 10 fetchable ones are in `_x_metadata` (not in any localize folder) or in notes recorded as settled/failed in `ignored_ids` `localize_backfill` (33 rows), which are never re-selected.

By URL extension: 39 image extension, 11 with no or another extension. No remote markdown image ref has a video or audio file extension.

## 2. Video and post embeds

- `![](…)` embed pages: **579 `youtube.com/watch`**, **22 `x.com/i/…`** and 1 other `x.com` (mostly `daily/`). The editor renders these as `[remote image: …]` (`crates/editor/src/reading/view/embeds.rs:154-173`).
- HTML outside fences: 33 `<iframe>` (8 bsky, 7 vimeo player, 4 datawrapper, 3 bloomberg, …), 36 `<video>` (14 github private-user-images, 8 sanity, 7 ghost, …) and 10 `<source>`. The editor shows HTML blocks as plain text (`crates/editor-model/src/render/lower.rs:213,324`).

## 3. Audio

There are 49 `<audio>` tags with a remote `src`: 26 in `daily/`, 15 in `_raw_transcript/`, 8 in `_crucible/`. Most are hosted on `api.substack.com` (podcast audio in transcript notes). No markdown audio refs.

## 4. Image descriptions (`state.db` `image_descriptions` vs `_MD5` image files)

- 6,405 distinct `_MD5` images; **1,849 have no record**: 879 webp, 571 jpg, 450 png, 11 gif, 11 svg, 4 avif.
- Records: 5,361 `vision`, 80 `svg-text`, 62 `failed`. Of the failures, 42 are Crucible-era "API returned 500 … exited prematurely" records that were classified permanent. 18 are the legacy `finish reason "length"` text. 2 are ENOENT.
- 5 non-`_MD5` image files: 3 `Pasted image *.png` and 2 under `_resources/`.
- Describe throughput (local gemma, 2026-10-28 run): about **4.5 s/image** (40 consecutive records over about 3 min). That makes 1,849 images about 2.3 h, and 579 new YouTube thumbnails about +45 min.
- Cause of the gap: describe runs only from `clip-enrich` (clip notes) and the manual `image-describe-backfill` (limit 100). Nothing is scheduled, and `localize-on-edit` and the sweep do not describe.

## 5. Crucible reference surface (`https://github.com/dulrich/obsidian-crucible/blob/master/`)

Settings: `src/types.ts:522-547` (defaults `:794-818`), UI `src/settings/sections/localize.ts:32-117`. Crucible has per-type flags (images on; audio, video and pdf off), per-type extension whitelists, image conversion (format and quality), folder and name templates, and on create/edit/paste triggers. There is **no** link/embed/localize mode per type, no video embed, no thumbnail, no size cap or timeout, and no failure marker. `cv` already ports the flags, whitelists, conversion and templates as `localize` step params (`crates/cv/src/steps/localize/params.rs`), plus `timeout_secs` and `max_bytes`. The new modes in the plan are new features, not port gaps.
