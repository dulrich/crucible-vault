# WP-1 findings: no-blurb cause, video-post marker, sprint measurements

Plan `intake-removals-sources-rescan` rev 2, WP-1. Measured 2026-11-17 against the live vault read-only, with `cv 1.1.4 (5a61f42d)`. Aggregates only: no note title, path, sentence, sender or publication name appears here. Rows are anonymous numbers (the same row number is the same note across tables). Scripts live beside this file in `docs/spikes/intake-second-sprint/`.

## Method

Inputs. `index-3.db` and `state.db` were opened only with `sqlite3 -readonly`; vault files were only read. `docs/spikes/intake-second-sprint/dump-queries.sh <out>` reproduces the four TSV dumps (`src.tsv`: source, publication, day-folder flag, intake flag = has `post-id` or `message-id`, for every note with a `source`; `rss.tsv`: path, publication, via, enriched for day-folder notes with a `post-id`; `enr.tsv`: path, via, enriched for every note; `dup.tsv`: clip path and `duplicate-of` value). Commands, with `S=_custom/crucible-vault/sources.toml` of the vault and `V` the vault root:

- Prefix tally: `perl prefix-publication-tally.pl $S src.tsv`; classes: `perl prefix-publication-classify.pl $S src.tsv`.
- RSS recurring lines (reads note files by relative path, so run from the vault root): `cd $V && perl <repo>/docs/spikes/intake-second-sprint/rss-recurring-lines.pl < rss.tsv`.
- Enriched census and the no-blurb candidate list: `perl enriched-census.pl $V enr.tsv cands.tsv` (new; reads each enriched note, counts body words, tests for a `> [!abstract]-` and a `> [!removed]-` block).
- Duplicate-of pairs: `cd $V && perl <repo>/docs/spikes/intake-second-sprint/duplicate-of-pairs.pl < dup.tsv` (new).
- Tracker rows: `sqlite3 -readonly ~/.local/state/crucible-vault/state.db` on `tracker_items` (`word_count`, `full_word_count`, `access`, `complete`, `outcome`, `note_path`).

Scratch re-run. A scratch tree under `/dev/shm/wp1/s` (outside every repo) held only the 9 candidate notes (copied with `cp -p`) plus `_custom/prompts/Intake Enrich.md`, and a config dir with copies of `agents.toml`, `providers.toml`, `actions.toml`, `kinds.toml`, `vault.toml` (no `schedules.toml`, `pipelines.toml`, `sources.toml`, `captures.toml`, so nothing scheduled could fire). `docs/spikes/intake-second-sprint/scratch-env.sh <dir>` points `CV_VAULT`, `CV_CONFIG_DIR`, `CV_STATE_DB`, `CV_SEARCH_DB`, `CV_INDEX_DB`, `CV_SOCKET`, `XDG_RUNTIME_DIR`, `XDG_STATE_HOME`, `XDG_CACHE_HOME` at scratch and refuses to continue if any of the six `CV_*` paths is outside the scratch dir; it also loads the `CV_ROUTER_GATEWAY_KEY` from the host-env projection into the process environment only (never printed). The one scratch edit was `default = "true"` on the `force` param of `intake-enrich` in the scratch `actions.toml` (that is the forced form: job param `force`, default in `config/examples/actions.toml` `[[action]] id = "intake-enrich"`). `cv daemon` ran against scratch; each note was run with `cv run intake-enrich --note <path>`; outcome vars were read from the scratch `state.db` `job_step_checkpoints` (step 1, `intake_enrich`). The configured model route (`router:codex-profile-a/gpt-6.1-terra` via the local gateway) was reachable and credentialed; the 9 calls succeeded. The scratch daemon was stopped with SIGTERM (clean "daemon stopped" log), the live service stayed `active` throughout, and the scratch tree was deleted.

Historic evidence. The live `state.db` keeps `jobs` and `job_step_checkpoints` (43k jobs, since 2026-10-18), and `~/.local/state/crucible-vault/superseded/` keeps the pre-replace text of every note the supersede engine replaced. Both were read-only and matched to the 9 notes by path / by `message-id` or `post-id`.

## Measurements

Where a number differs from the plan's Context both are given.

| Item | Plan Context | Re-run | Note |
|---|---|---|---|
| Records with URL prefixes | 22 | 22 (3 multi-prefix) | same |
| Intake notes with a `source` under a prefix | 559 | 559 (174 + 385) | same |
| of which carry the record's publication | 385 | 385 | same |
| of which blank | 0 | 0 | same |
| of which carry a different value | 174 across 8 fanfic records | 174 across 8 records, all `kind = fanfic`, one distinct stale value per record | same |
| RSS publications with 5+ notes | 66 (2,001 notes) | 66 (2,001 notes) | same |
| Tail: publications / notes / lines / notes with 3+ | 18 / 325 / 538 / 42 | 18 / 325 / 538 / 42 | same |
| Head: publications / notes / lines | 23 / 491 / (not stated) | 23 / 491 / 551 | same |
| Enriched RSS notes in those pubs still carrying tail lines | 101 and 21 | 101 and 21 notes (31 lines) | same |
| Enriched notes | 338 | 339 | index now holds one more (new enrich since the plan's measurement) |
| Notes with a fold | 203 | 204 | the same one note |
| Fold quoted lines | 1,157 | 966 non-blank quoted lines; 1,677 counting the bare `>` separator and blank lines | cannot reproduce 1,157; the plan's counting rule is not recorded. Neither of the two natural rules gives it |
| Enriched, 300+ words, no abstract block and no fold | 9 (7 mail, 2 clips) | 9 (7 mail, 2 clips) | same |
| `duplicate-of` notes | 8, two are the pattern | 8, two are the pattern | same; the two pattern notes' sizes differ, see below |

Prefix classes of the 174 (`prefix-publication-classify.pl`): 8 hold the record's long name instead of its short name, 77 are a substring variant of the short name, 89 hold a value that is not a configured publication. 174 notes per record: 8, 16, 31, 2, 7, 59, 31, 20 (sorted record order).

Via x length x abstract x fold, enriched notes (339). `via` blank means a tracker blog note.

| via | words | abstract | fold | notes |
|---|---|---|---|---|
| mail | 300+ | no | no | 7 |
| mail | 300+ | yes | no | 10 |
| mail | 300+ | yes | yes | 58 |
| clip | 300+ | no | no | 2 |
| clip | 300+ | yes | no | 1 |
| blank | 300+ | yes | no | 108 |
| blank | 300+ | yes | yes | 114 |
| blank | 150-299 | yes | no | 3 |
| blank | 150-299 | no | yes | 4 |
| blank | under 150 | no | no | 4 |
| blank | under 150 | no | yes | 28 |

Total fold lines: 966 non-blank quoted lines in 204 notes (1,677 counting bare `>`). The row for 300+ words, no abstract, no fold is the 9 of the no-blurb section.

`duplicate-of` pairs (clip words and target words are body words, frontmatter excluded; "Clippings" is whether the clip lives under `Clippings/`):

| pair | target words | clip words | clip under Clippings | transcript heading (clip / target) |
|---|---|---|---|---|
| 1 | 295 | 5,128 | no | no / no |
| 2 | 1,509 | 1,656 | no | yes / yes |
| 3 | 1,934 | 1,800 | no | no / no |
| 4 | 1,807 | 1,895 | no | no / no |
| 5 | 2,357 | 1,930 | no | no / no |
| 6 | 1,652 | 1,464 | no | yes / yes |
| 7 | 1 | 3,507 | yes | no / no |
| 8 | 1 | 4,072 | no (day folder, carries `post-id`) | no / no |

Differences from the plan for the two pattern pairs (7 and 8). The plan states tracker notes of 61 and 84 words and clips of 3,580 and 4,161 words. Those match whole-file `wc -w` (frontmatter included): the tracker notes' bodies are one word each (the stored `word-count` frontmatter is 1; the tracker row says 0). Body-only clip counts are 3,507 and 4,072. Also only pair 7's clip is under `Clippings/`; pair 8's clip has already been filed to the day folder. Either way WP-8's "tracker post under SHORT_WORDS" test holds (1 word) and the clips pass `MIN_REFINE_WORDS` (300).

## No-blurb cause

Result in one line: none of the 9 is an enrich failure. Each was enriched successfully, then the supersede engine replaced the note body wholesale and kept the `enriched` stamp, so the generated blocks vanished. A forced re-run produces a top block on all 9.

Per-note rows. "Then" is the step outcome recorded in the live `job_step_checkpoints` for the note's original `intake-enrich` job; "Re-run" is the forced scratch run on the same note as it is today. Both parsed (a parse failure fails the job with nothing written; all 18 jobs were `done`). `skipped` was unset in every run (no pre-model skip exists once `enriched` is forced; the `already enriched` skip is the only one, `crates/cv/src/steps/intake_enrich.rs:171-176`). `banners_folded` 0 and `fold_refused_over_half` false in all 18.

| row | kind | words now | words then | then: top_block / resolved / dropped / spans / lines | re-run: top_block / resolved / dropped / spans / lines | superseded copy with generated blocks | cause class |
|---|---|---|---|---|---|---|---|
| 1 | mail | 3,890 | 319 | true / 7 / 0 / 1 / 3 | true / 10 / 0 / 2 / 26 | yes (top + fold) | body replaced after enrich |
| 2 | clip | 1,130 | 28 | false / 2 / 0 / 1 / 1 | true / 8 / 0 / 2 / 8 | yes (fold only) | body replaced after enrich (enriched as a stub) |
| 3 | mail | 1,130 | 454 | true / 8 / 0 / 2 / 18 | true / 10 / 0 / 2 / 10 | yes (top + fold) | body replaced after enrich |
| 4 | mail | 903 | 628 | true / 6 / 0 / 1 / 3 | true / 8 / 0 / 3 / 27 | yes (top + fold) | body replaced after enrich |
| 5 | mail | 976 | 187 | true / 6 / 0 / 1 / 1 | true / 6 / 0 / 0 / 0 | yes (top + fold) | body replaced after enrich |
| 6 | mail | 4,791 | 496 | true / 6 / 0 / 1 / 3 | true / 9 / 0 / 2 / 30 | yes (top + fold) | body replaced after enrich |
| 7 | mail | 1,444 | 281 | true / 6 / 0 / 1 / 1 | true / 7 / 0 / 0 / 0 | yes (top + fold) | body replaced after enrich |
| 8 | clip | 890 | 2 | false / 0 / 0 / 0 / 0 | true / 8 / 0 / 2 / 4 | no (2-word stub, nothing generated) | body replaced after enrich (enriched as a stub) |
| 9 | mail | 1,161 | 358 | true / 9 / 0 / 2 / 18 | true / 10 / 0 / 2 / 13 | yes (top + fold) | body replaced after enrich |

Re-run `words` is the step's own pre-enrich body count; the "words now" column of the census equals it for every row. Summary: in all 9 re-runs the top block's first section is a prose paragraph (the model's `summary`), not an Entities / Key claims / Contents section, so the answer held a non-empty `summary` every time. The raw model answer itself is not logged anywhere (the step records outcome vars only; `api_usage` is token counts), so "answer parsed" is inferred from the job finishing `done` and "summary present" from the rendered top block; nothing was patched to capture the raw text.

Cause classes, counted: answer lacked a summary 0; answer malformed and members dropped 0 (`anchors_dropped` 0 in all 18 runs); body truncated or over a size limit 0 (the largest note, 4,791 words, produced a top block; truncation would fail the job); pre-model skip 0; not reproducible (re-run succeeded) 9, because the original run also succeeded; other 9, structurally described next. The classes overlap because "not reproducible" is a symptom, and "other" is the cause.

The cause. After the original enrich, each of the 9 notes was rewritten by the supersede engine (`Replaced`: a later, more complete capture of the same post replaces the body in place). `replaced_text` copies the whole old frontmatter and swaps the body (`crates/cv/src/supersede.rs:467-513`; module doc lines 12-17: "frontmatter is kept except `access`/`complete`/`word-count`/`via`"). So `enriched` and `enrich-model` survive while the `%% cv-enrich %%` top block and fold, the inserted headings and everything else the enrich had generated are gone. Evidence, all 9 rows:

- The live `intake-enrich` job for each note recorded a word count 19-60% of today's for the 7 mail notes (187-628 words then, 903-4,791 now), and 28 and 2 words for the clips: the bodies are different texts.
- Each note has exactly one file in `<state dir>/superseded/` (the engine's pre-replace copy, `supersede.rs:14-17`); 7 of the 9 copies carry the generated top block and fold, the row 2 copy carries a fold only (short body), the row 8 copy is the 2-word stub with no generated blocks.
- Every note has later link-scan / localize-on-edit event jobs (write events) minutes to days after its enrich, coinciding in most cases with `track-mail` runs for the mail rows and with `clip-auto-file` runs for the two clips (rows 2 and 8 were enriched while a clip stub with 28 and 2 words was in the vault; the clip's real body arrived afterwards).
- The 150-word line is not involved: at enrich time the 7 mail notes were above it and got a top block.

So the premise of the plan's Context item 4 ("the step stamps `enriched` after a parseable answer whether or not a top block was produced") describes a real code path (`intake_enrich.rs:203-204`) but is not what produced these 9. WP-7's "do not stamp when no block was produced on a note of `SHORT_WORDS` words or more" would not have fired for any of them (the enrich produced blocks), so it would not prevent a recurrence.

Smallest fix per class, with anchors:

- Body replaced after enrich (all 9): in `replaced_text`, drop the enrich stamp keys together with the body they described, `doc.remove("enriched")` and `doc.remove("enrich-model")` (and WP-7's `enrich-miss` once it exists), next to the existing `doc.remove("complete")` handling at `crates/cv/src/supersede.rs:481-493`. The note then re-enters `intake-enrich-backfill`'s candidates (`crates/cv/src/steps/intake_enrich.rs:263-276`, `:336-339`) and the track step's own enqueue (`enqueue_for`, `:239-250`) re-enriches it. For the clip stubs the same removal covers the stub-then-fill order.
- Existing notes (the 9 and any later one): WP-7's recovery mode, as planned (clear `enriched` on notes of `SHORT_WORDS` words or more that carry neither generated block, `intake_enrich.rs:168-177` / `:258-276`). It is needed and sufficient for the 9 because their forced re-runs all succeed; a cap of 1-2 attempts is ample.
- Answer lacked a summary / malformed / truncated / pre-model skip: observed 0 times. WP-7's miss counter (`enrich-miss`, no stamp when a 150+ word note gets no block) stays valid as defence in depth, not as the fix for this symptom. No prompt change is indicated; the prompt (`config/examples/prompts/Intake Enrich.md`, identical to the vault copy) is not implicated.

A separate observation for WP-7: the supersede engine can also replace a note whose `enriched` stamp is current and whose blocks are intact; the fold lines the old body had are then discarded from the note (they remain only in the `superseded/` copy). If WP-3's per-group labels exist by then, a replaced body also loses any stored positions. That is the same fix site.

## Video-post marker

The two pairs (7 and 8) have an identical tracker post shape.

Tracker row (`tracker_items`, both): `source_kind = blog`, `outcome = note`, `full_body = 1`, `word_count = 0`, `full_word_count = 0`, `access = free`, `complete = 1`, `status = x`, `duration_s` NULL, no stored kind column. The same `word_count`/`full_word_count` of 0 occurs in exactly these two rows of the whole table.

Note frontmatter keys (both): `title, source, post-id, author, publication, published, created, description, word-count, access, complete, tags, read, updated, enriched, enrich-model`; `word-count: 1`, `access: free`, `complete: true`; no `kind`, no media or duration key. The legacy `kind: podcast` the index shows 71 times belongs to the old `_blog_metadata` notes, not to captured tracker posts.

Body shape (booleans): the body is exactly one non-blank line; that line is a single markdown link with a one-word text; the link target is on the publisher platform's API host under `/feed/podcast/<id>` (a player link), and is not the post's own `source` URL; embed/iframe/video-host/player word present: false; image present: false; body has 2 or more lines: false.

Raw feed HTML. Not obtainable. `crates/cv-ingest/src/feed.rs` parses title, link, guid, `content:encoded`, `description`/`summary`, authors, published and the first `audio/*` enclosure URL only (`:31-32`, `:104-118`); video enclosures, `<iframe>` and `media:*` elements are not parsed. `crates/cv-ingest/src/blogs.rs:127-153` keeps `body_html` (converted to Markdown for the note) and computes `PostKind::Podcast` from an audio enclosure in memory; neither the HTML nor the kind is written to `tracker_items` or the note (`tracker_items` has no body column; the blog note frontmatter has no `kind`). There is no retained copy and the spike did no network fetch. What survives is the Markdown conversion: the feed body was nothing but a link to the platform's podcast player (no text at all, hence `word_count = 0`), so the video-ness is not stored; what is stored is "the feed carried no text, only a player link".

Which predicate separates them. Surveyed all 110 tracker note rows with `word_count` or `full_word_count` under 150, plus the whole `tracker_items` table for the zero rows:

- Ordinary short posts: 3 to 143 words, 3 or more non-blank lines (or 2-3 lines with 27+ words), complete and access blank or free. Not link-only.
- Paywalled excerpts: `access` paid or unknown, `complete = 0`, 7 to 130 words; their stored `full_word_count` is 184 to 8,056.
- Podcast-player link plus text: 3 other short posts carry the same `/feed/podcast/` link; 2 are paid and `complete = 0`, 1 is paid and complete with 122 words of text. So the player link alone is not a discriminator.
- The two video posts are the only rows with `word_count = 0` and `full_word_count = 0`, the only complete and free posts whose body is a single link, and the only ones with a one-word body.

Predicate, computable at clip-filing time from what intake stores today: the target is a tracker post whose row has `complete = 1` and `access = 'free'` and whose body (frontmatter excluded) is one non-blank line that is one markdown link and nothing else; equivalently the note's `word-count` frontmatter is at most 3 (it is 1 in both) and the tracker row's `word_count = 0`. It must be stated as "link-only body", not "video": nothing stored tells a video player link from an audio-only player link (the only stored distinction is the player link's `/feed/podcast/` path, which both a video and an audio episode on that platform may carry). For the transcript use that is acceptable (both want the clip as the body), but WP-8 should say so in its doc and test, and should not name the pattern "video" in code beyond the existing `transcript-of` key. No usable marker is missing, so the plan's stop condition does not trigger.

What would have to be stored at capture to have a true video marker: the feed item's media kind, i.e. the presence of a video enclosure or `<iframe>`/`<video>` element, or the player link's host and path class, written to the tracker row (`kind` or a `media` column) and the note frontmatter; `feed.rs` would need to keep non-audio enclosures. Out of scope for this sprint.

## Recommendations for WP-7 and WP-8

WP-7.
1. Re-scope the fix: add the supersede-side removal of `enriched` / `enrich-model` (`crates/cv/src/supersede.rs:467-513`, `replaced_text`) as WP-7's primary change; this is the only change that stops the recurrence. It touches a file the plan's WP-7 does not list; the file list needs `crates/cv/src/supersede.rs` and a test in `crates/cv/src/supersede_tests.rs` (a replaced enriched note loses `enriched`, `enrich-model`, `enrich-miss`).
2. Keep the miss counter and the cap as defence in depth, and note in the doc that it did not produce the measured 9.
3. The recovery mode (clear `enriched` on `SHORT_WORDS`+ notes with neither generated block) recovers the 9; its fixture should include a mail note and a clip whose body grew after a stub enrich. A re-run of each of the 9 now produces a top block, so with the plan's cap of a small fixed number none should end capped.
4. The prompt is not implicated; no example-prompt change and no WP-9 prompt migration is needed for this cause.
5. Do not try to reconstruct the old fold from `superseded/`; a re-enrich regenerates it.

WP-8.
1. Use the link-only predicate above for the "kept target is a short post that is not an excerpt" test: the target row has `complete = 1`, `access = 'free'`, and the note body is a single link line (equivalently `word-count` at most 3 and tracker `word_count = 0`), plus the planned clip side `MIN_REFINE_WORDS`. The plan's wording "carries the video marker WP-1 recorded" maps to this.
2. Do not key on the player URL path or the publisher host: config-free generality is lost and 3 other posts carry that link with text. Do key on the absence of text.
3. Target word counts: the plan's `SHORT_WORDS` (150) bound is far looser than needed (the two are 1 word); keep it as the guard, the predicate does the discriminating. Pair 1 has a 295-word target and a 5,128-word clip, so a bound above 150 would have risked it; leave `SHORT_WORDS` as written.
4. The two existing pairs are found by `duplicate-of` on a clip pointing at a link-only tracker post; one clip is under `Clippings/` and one is already in the day folder with a `post-id` and an `enriched` stamp, so the reconcile must not require the `Clippings/` folder and must clear the clip's `enriched` stamp when it adds `## Transcript` (the existing `transcript_refine` clears it in the same write).
5. Order caveat unchanged: only clip-arrives-second is in scope.

Notes on the sprint's items 1 and 3 (WP-3 to WP-6 context): the prefix and recurring-line numbers reproduce exactly; the only deltas are the fold-line count (966 non-blank, 1,677 including bare `>`, against the plan's 1,157) and the enriched and fold note counts (339 and 204 against 338 and 203). If WP-3 sizes anything on the 1,157 figure it should re-measure with its own counting rule.
