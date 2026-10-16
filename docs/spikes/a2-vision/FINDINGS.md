# A2 spike — local vision input + Crucible state (2026-10-15)

## Q1: does the local inference server accept OpenAI `image_url` content?
- Endpoint: llama-swap at `http://127.0.0.1:4811/v1` (Crucible provider `vz3dns1`). Crucible's configured image model id `gemma-4-12B-it-Q4_K_M` is no longer served; the served equivalent is `gemma-4-12B-it-qat-UD-Q4_K_XL`.
- Request: `req.json` / `req2.json` (64×64 solid-red PNG as a `data:image/png;base64` URL, prompt "What single colour fills this image? One word.").
- `curl -s http://127.0.0.1:4811/v1/chat/completions -d @req2.json` → content `"Red"`, 162 completion tokens (reasoning in `reasoning_content`), 77 prompt tokens. Cold call 1.44 s wall.
- With `max_tokens: 30` the answer was empty (`finish_reason: length`; reasoning consumed the budget) → a describe call needs a generous `max_tokens` and must read `content`, not assume it is non-empty.
- **Verdict:** vision via `image_url` data URLs works on the local server; A2 needs only a multimodal user-content variant in `cv-providers` wire, no new provider kind.

## Q2: Crucible's frozen state (read-only, `jobs.sqlite?mode=ro&immutable=1`, mtime 2026-10-14 16:36)
- `link_records`: 35,339 rows (outline said 25,051), all `state=pending`, `discovery_method=scan`; 344 `tracked_source="candidate"` (`youtube-channel`), 2,237 with `yt_video_id`, 921 with `x_status_id`; `max(last_seen)` 2026-10-12. `meta.schema_version` 2.
- `jobs` by type/status: blogs_tracker 2, youtube_tracker 2, daily_brief_lite 31, link_scan 5, x_post_discover 979 (+27 failed), x_metadata_fetch 90, youtube_metadata_fetch 139.
- Image descriptions on disk: 5,343 JSON files (already imported by A1 into `image_descriptions`).
- Vault machine-data folders (counts only): `_yt_metadata` 2,272 md, `_x_metadata` 634, `_blog_metadata` 1,255, `_crucible` 215.
- data.json: YT/Blogs tracker enabled but interval 0 (manual runs), diff mode on; channel enrich off; X auto-discover on; YT auto-enqueue on (2 s pacing); daily brief enabled, 1 FX pair, several weather locations, target section `# Daily Brief`; one user trigger ("Ignore Shorts from Channels": `_yt_metadata` create/modify, channelId ∈ set, duration_seconds < 120 → youtube-ignore-video).
- Secrets: `~/.config/crucible-vault/` holds no YouTube key; the unit environment carries only `CV_LINK_WRITE_ROOTS`.

## Q3: blog full-body consistency and filed-blog shape (added 2026-10-15 for audit M4)
Commands: python3 -I scripts run from `~/vault` (read-only; frontmatter parsed with `re.match(r'---\n(.*?)\n---\n')`; aggregates printed only).
- `_blog_metadata/**.md` grouped by frontmatter `blog:` → 46 blogs; 44 have `has-body: true` on every post, 2 on none, 0 mixed. Body chars (after frontmatter) p10/p50/p90 = 784 / 5,645 / 22,943 over 1,255 notes. Keys on all 1,255: `source, post-id, blog, published, word-count, kind, has-body, fetched_at, source_command`.
- Notes under `daily/day/<date>/` (depth ≥ 3) whose frontmatter tags include `blog` → 1,130. Key counts: title/source/post-id/author/published/created/description/word-count/tags 1,130; read 1,129; modified 1,128; x-metadata 193.
- Reading List predicate: `_custom/crucible-vault/views/daily-reading-list.toml:5` `file.folder == "daily/day/" + this.file.name`. Kind `blog`: `kinds.toml` `[kinds.blog] tags=["blog"] folder="{period.day}"`.
- Registry tables: `grep -c '^|'` = 19 lines in `core/Tracked Channels.md`, 33 in `core/Tracked Feeds.md` (incl. header/separator rows).
