# Post-id / canonical-URL identity vs duplicate notes (round-12 spike)

Aggregates only; live vault read-only (V=$VAULT), DB copies in a temp dir `T` (`cp ~/.local/state/crucible-vault/search.db* $T/`, `cp ~/.local/state/crucible-vault/state.db* $T/st/`), opened read-only. Driver: `src/main.rs` (copy of the round-11 dupes driver) + `src/identity.rs` (new); own `[workspace]`, deps `cv-text` path + `rusqlite =0.40.2`, `cp ../../../Cargo.lock .`. Raw outputs: `out/`.
Build: `CARGO_TARGET_DIR=$PWD/target nice -n 10 cargo build --release --offline -j 4`.
Identity = stored `post-id`, else `cv_text::url_canon::post_id_from_url(source, &PostIdOptions::default())`, else `canonicalize_url(source).canonical`. Defaults match the daemon's blog tracker (`crates/cv/src/steps/tracker/blogs.rs:100` `canon_opts()` = `PostIdOptions::default()`; `signals.rs:306` passes configured per-host rules, none assumed here). A second identity, "recomputed-first" (recomputed id before stored), is used where stale ids matter. Vault drifted slightly since round 11 (9,580 notes now), so pair counts differ by a few from `docs/spikes/dupes/FINDINGS.md`.

## Commands
```
T=$(mktemp -d /tmp/postid-XXXX)
nice -n 10 target/release/postid-spike shingles $V $T/a.tsv        > out/shingles.txt   # WP-8 shingle pass
nice -n 10 target/release/postid-spike semantic $T/search.db $T/b.tsv > out/semantic.txt # mean-chunk cosine
nice -n 10 target/release/postid-spike identity $V $T/a.tsv $T/b.tsv  > out/identity.txt # sections 1-4
target/release/postid-spike state $V $T/st/state.db                   > out/state.txt    # section 5
```
Shingle pass: containment >=0.5 / 0.7 / 0.9 = 11,365 / 8,727 / 2,785 pairs; cosine >=0.85 / 0.90 / 0.95 / 0.98 = 199,873 / 38,039 / 12,903 / 2,807; 512 notes under 20 shingles excluded from the content pass (so identity sees them, the content pass does not).

## Frontmatter keys (out/identity.txt, 9,580 notes excl. `.trash`; `.trash` holds 14 md, excluded from identity, 31 `>=0.9` pairs touch it)
- `source` 4,824 (50.4%), `post-id` 4,787 (50.0%), `yt-video-id` 1,159 (12.1%), `via` 38, `duplicate-of` 0, `created` 4,647 (48.5%), `published` 4,841 (50.5%), `modified` 4,205, `updated` 253, `date` 173.
- By folder (n / source / post-id / ident): `_blog_metadata` 1,255 / 1,255 / 1,255 / 1,255; `daily` 3,177 / 2,945 / 2,925 / 2,945; `_raw_transcript` 460 / 459 / 443 / 459; `Clippings` 9 / 9 / 8 / 9; other 413 / 156 / 156 / 156. NO source/post-id at all: `_yt_metadata` (2,663), `_x_metadata` (637), `initiatives` (939), `core` (27). Only 4,824 notes (50%) have any identity.

## (1) Identity groups (stored post-id first)
- Groups with >=2 notes: 824 (of 3,919 identity groups), 1,729 notes, max group 7.
- By folder-kind set (groups): `_raw_transcript + daily` 431; `_blog_metadata + daily` 347; `_blog_metadata + _raw_transcript + daily` 27; `daily` only 19. Within-group note pairs: `_raw_transcript x daily` 495, `_blog_metadata x daily` 405, `daily x daily` 89, `_blog_metadata x _raw_transcript` 28. Zero groups involve `Clippings`, `_yt_metadata`, `initiatives`, `other`. Note `_raw_transcript x daily` is the transcript file plus the daily note that embeds the same video, an expected pairing, not necessarily a mistake.
- Stale ids: of 4,787 notes carrying both `post-id` and `source`, 787 (16.4%) have stored != `post_id_from_url(source)` (556 also differ from the canonical source URL). Recomputed-first identity gives 1,135 groups >=2 (2,355 notes) vs 824 (1,729): 315 groups exist only after recomputing, 4 stored-only. So stale/legacy ids hide ~38% more groups: any identity dedupe must recompute from `source`, not trust `post-id`.

## (2) `_blog_metadata` coverage
- 1,255 notes; 100% carry `source` and `post-id` (and so an identity).
- With a twin (same identity, non-metadata note): 374 (29.8%) - 347 with a `daily` note, 27 with `_raw_transcript`+`daily`. No twin: 881 (70.2%); none of those share an identity with another metadata note (the metadata notes are unique per post).
- `cv_text::similar::containment` (smaller body in larger, best twin): >=0.9 = 354, 0.5-0.9 = 13, <0.5 = 7 (n=374).
- Date fields: `published` filled 1,255 (100%; shapes: `YYYY-MM-DD` 883, ISO with offset 305, ISO `Z` 45); `fetched_at` 1,255 (100%, ISO ms `Z`). No `created`/`date`. Other keys: `blog`, `kind`, `has-body`, `word-count`, `source_command` (all 1,255), `authors` 790, `categories` 324, `audio_url` 160, `x-metadata` 113.

## (3) Coverage of the content-similarity candidates
Exclusions applied in this precedence: `daily x daily`, then any pair touching `initiatives`, then any pair touching `_yt_metadata`; pairs touching `.trash` are unresolved (not in identity universe). "Explained" = both notes share an identity (stored-first or recomputed-first).

| set | total | daily x daily | initiatives | _yt_metadata | .trash | remaining | explained | unexplained |
|---|---|---|---|---|---|---|---|---|
| containment >=0.9 | 2,785 | 189 | 669 | 884 | 31 | 1,012 | 670 (66.2%) | 342 |
| BOTH (cont >=0.7 & cos >=0.95) | 902 | 31 | 3 | 33 | 0 | 835 | 647 (77.5%) | 188 |

- Headline: identity explains **66.2%** of the remaining containment>=0.9 pairs and **77.5%** of the remaining BOTH pairs.
- Explained by folder pair (>=0.9): `_blog_metadata x daily` 643, `_raw_transcript x daily` 24, `_blog_metadata x _raw_transcript` 2, `_raw_transcript x _raw_transcript` 1. (BOTH: `_blog_metadata x daily` 502, `_raw_transcript x daily` 142.)
- Unexplained (>=0.9, 342): `_blog_metadata x daily` 196, `other x other` 66, `_blog_metadata x _blog_metadata` 27, `_raw_transcript x other` 22, `daily x other` 20, `_raw_transcript x daily` 9. One side lacks `source` in 107 (31.3%; both lack 65, exactly one 42).
- Unexplained (BOTH, 188): `other x other` 78, `_blog_metadata x daily` 51, `_blog_metadata x _blog_metadata` 23, `_x_metadata x daily` 10, `Clippings x Clippings` 9, `daily x other` 6, `_blog_metadata x _x_metadata` 5, `_raw_transcript x daily` 4. One side lacks `source` in 98 (52.1%; both lack 83, exactly one 15).
- So the residual is not mainly "no source": 196 `_blog_metadata x daily` pairs with content containment >=0.9 where both have `source` but different identity (same post under different URL, e.g. mirror/redirect/AMP/host variants) or where the daily note embeds the article without a matching `source`; plus 27 `_blog_metadata x _blog_metadata` (same text under two URLs; 23 in BOTH), plus `other x other` (66/78: e.g. reference/paper folders) with no source.

## (4) Re-clips (identity groups with >=2 notes outside `_blog_metadata`)
- 488 groups: `_raw_transcript + daily` 458, `daily` only 30. Zero have a `Clippings` member. Groups with members in >=2 different `daily/day/<date>` folders: 2.
- Spread of `created`/`date`/`published` (known for all members of all 488): same day 486, <=7 days 2, more 0.
- Conclusion: identity-visible re-clips on different days are essentially absent (2 of 488). Hypothesis (2), the same post re-clipped on different days, is not supported by identity; the 30 `daily`-only groups are same-day (or the 2 <=7d).

## (5) 2026-10-16 intake (state.db copy)
Schema facts used: table `tracker_items` (PK `source_kind, source_key, item_id`; columns `url`, `day` TEXT, `first_seen_ms`, `outcome` in line/note/ignored/skipped, `note_path`); index `tracker_items_day`. Cmds: `sqlite3 -readonly $T/st/state.db "select day,count(*) from tracker_items group by day order by day desc limit 8"`, `... "select source_kind,outcome,count(*),sum(note_path is not null),sum(url<>'') from tracker_items where day='2026-10-16' group by 1,2"`; matching in `postid-spike state`.
- Rows dated 2026-10-16: 6,507 (the next busiest day has 116): blog 4,075, youtube 2,432. Other days hold 127 blog rows total.
- Blog rows on 2026-10-16: `line` 80, `note` 76 (74 with `note_path`), `skipped` 3,919 (only 545 carry a `url`; the rest are keyed by `item_id` alone). Every other day's blog rows (127) match 0 `_blog_metadata` identities.
- Matched to a `_blog_metadata` identity (item_id == metadata post-id, or `post_id_from_url(url)`, or identity of the note at `note_path`): `skipped` 1,764 of 3,919 (all by `item_id`; 354 also by url) and 0 of the 156 `line`/`note` rows. 1,764 exceeds the 1,255 metadata notes, so the same item appears under more than one `source_key`. So the 2026-10-16 bulk is a backfill of already-known posts recorded as `skipped`, and 45% of those rows correspond to existing `_blog_metadata` notes; the other 2,155 skipped rows have no metadata note (or an unmatched id format).

## Verdict
(a) Identity is enough to auto-handle one clean class: `_blog_metadata` note with a same-identity capture and metadata-in-capture containment >=0.9: 354 notes (374 with any twin), all captures in `daily` (27 also with `_raw_transcript`). That is ~28% of all 1,255 metadata notes. It is ~100% certain by construction (same identity + >=0.9 containment). Identity needs recomputation from `source` (16.4% stored ids are stale; stored-only misses 315 groups). Caveats: `_raw_transcript x daily` (~430 groups) is an intentional pairing, not a duplicate to remove; the metadata note also feeds intake memory (the tracker matches item_id to metadata ids), so removal needs the tracker's "seen" signal preserved.
(b) Residual needing content similarity + labelling: 342 of 1,012 (>=0.9) and 188 of 835 (BOTH) pairs remain after the user-declared exclusions: ~196 `_blog_metadata x daily` with different identities (different URL for the same post), 27 `_blog_metadata x _blog_metadata`, ~66-78 `other x other`, and `Clippings x Clippings`. Roughly a third to half of those have a side lacking `source`. Another 2,500 `_yt_metadata` / `initiatives` / `daily x daily` pairs are excluded as non-duplicates. Hypothesis (1) is largely supported (`_blog_metadata x daily` is the dominant class: 643 explained + 196 unexplained of 1,012 remaining >=0.9 pairs); hypothesis (2) is not (2 of 488 non-metadata groups differ in day).
(c) The `_blog_metadata` -> intake backfill is feasible on identity: 100% have `source`+`post-id`, 100% have a `published` date (3 shapes, plain date in 70%, ISO elsewhere; parse as date prefix) and `fetched_at`. Twin rate is only 29.8%: 881 of 1,255 (70.2%) have no capture and would be genuinely new intake candidates (or are the 1,764 `skipped` tracker rows already recorded on 2026-10-16). Caveat: those rows are the better backfill signal than note twins; 2,155 skipped rows have no metadata note.

## Addendum (round-12 plan audit B1, 2026-10-23): directional containment
The section-2 score compared the smaller body against the larger (`identity.rs`, small-in-large). Auto-removal must instead require the doomed metadata body to be contained in the surviving capture (`cv_text::similar::containment(meta, capture)`, best over the twins). Cmd: `nice -n 10 target/release/postid-spike identity $V $T/a.tsv $T/b.tsv > out/identity-directional.txt` (driver patched to print both).
- Directional meta-in-capture, best over twins (n=374): >=0.9: **347**; 0.5–0.9: 20; <0.5: 7.
- 8 metadata notes scored >=0.9 small-in-large but <0.9 directionally (the capture is the shorter body). These are exactly the cases where trashing would lose the full text, so they move to the labelled bucket.
