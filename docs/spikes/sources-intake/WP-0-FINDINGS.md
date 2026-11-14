# Sources/intake consolidation — diagnosis (2026-11-13)

Read-only measurement of the live config (`_custom/crucible-vault/sources.toml`, reviewable), `state.db` `tracker_items` and the derived `index-3.db` `props`/`files`. Aggregates and config values only; no note text or titles. Databases opened with `sqlite3 -readonly`.

## Config shape

`grep -c` per table header: 35 `[[blog]]`, 23 `[[serial]]`, 14 `[[youtube]]`, 9 `[[newsletter]]` (81 rows). Grouping rows by `publication_of(short, name)` (awk over the file): 75 publications, 6 of them spread over more than one row — 2 newsletter+serial, 2 blog+newsletter, 1 blog+serial, 1 newsletter+newsletter. Serial `kind`: 20 `fanfic`, 2 `blog`, 1 `3-2-1`. `works` is set on 6 rows (3 serial `true`; 1 serial, 1 blog, 1 youtube `false`). `[[newsletter]]` rows use `from` (9), `list_id` (2), `blog` (2), `hosts` (0).

## (a) Newsletter filed under Unknown sender — not a migration regression

`select source_key, day, first_seen_ms … where source_kind='mail' and (source_name like '%jamesclear%' or source_key='3-2-1 Newsletter')`: three rows keyed `unknown-sender` (seen 2026-10-22, 10-29, 11-09) and one keyed by the newsletter name (seen 2026-11-12). Config history (`git log -S jamesclear -- …/sources.toml` in vault-repo): a `[[serial]]` row with the site's URL prefix was added 2026-10-23; the `[[newsletter]]` row with the sender address was added 2026-11-10. Mail matches only `from`/`list_id` (`crates/cv-ingest/src/mail.rs` `row_matches`), never a serial prefix, so mail before 2026-11-10 was unknown; the serial row gave clips of the same site their publication, which is the "previously worked" path. Migration 0039 only adds `kind = "fanfic"` to kind-less serial rows. The three old rows stay unknown because the re-match runs only inside `intake_reconcile` (`reconcile_backfill.rs` `mail()`), which has no schedule (`schedules.toml` has no reconcile entry) and no config-change trigger. Across all mail: 122 rows, 8 `unknown-sender` rows from 5 senders.

## (b) One post, three notes — three hostnames

`select … where title like '<post title>%'`: three `note` rows on one day. Row 1 `blog`, feed on `<pub>.substack.com`, item URL on the publication's custom domain, `access=paid`, 326 of 973 words. Row 2 `mail`, same publication, URL on `<pub>.substack.com`, 357 words, `complete=1`. Row 3 `blog`, a second publication of the same author (cross-post), its own custom domain, 120 of 909 words. The three notes carry three different `post-id` values, no `duplicate-of`. Identity keys are host-exact canonical URLs (`cv-ingest/src/supersede.rs` `identity_keys`); the mail path proves a `/p/<slug>` link only against the Substack host or the blog row's feed host (`cv-ingest/src/mail.rs` `canonical_post_url`), so the custom domain and the Substack host never meet. Containment cannot rescue it: both bodies are partial, and a new RSS post skips the supersede resolve entirely (`cv/src/steps/tracker/blogs.rs` writes it directly). Rows 1 and 2 are one publication under two hosts; row 3 is a different publication.

## (c) Works `#Unread` ignores "In Works"

The named publication has a `[[blog]]` row (feed) and a `[[serial]]` row (two URL prefixes, `kind = "blog"`), both `works = false`. `crates/cv/src/views.rs` `source_data` emits serial rows without a `works` check and sets `file.serial` from any serial row; `crates/views/defs/works.toml` Unread filters on `file.serial` only. So a serial row lists its unread notes whatever its toggle says, and the publication shows as two Sources in the Settings form.

## (d) Video post + transcript (held for the second sprint)

The video-only tracker note (710 bytes, `complete=true`) and the clipped transcript (21 kB, in `Clippings`) share a `source`; the clip is marked `duplicate-of` the tracker note and nothing else happens (`auto_file.rs` Kept-not-contained branch).

## (f) Missing `read` / `publication`

Intake notes = `.md` under `daily/day/` with `post-id` or `message-id`. Per month (`files` × `props`): through 2026-03 every intake note lacks `read` (about 200 notes); 2026-04..09 lack it on 20, 17, 55, 55, 122, 90; 2026-10 and 2026-11 lack it on none. Total about 560–660 depending on the pre-2026-04 tail. `publication` is absent on about 1,700 intake notes (154–276 per month 2026-04..09, 143 in 2026-10, 68 in 2026-11). Vault totals: `read` on 3,957 notes, `publication` on 2,388, `post-id` on 4,467.

## (h) Note with no blurb (held for the second sprint)

The named note is `via: email`, `enriched` set, `description` present, 34 kB: the enrich job ran, so this is detection quality, not a missed job. Recurring-line stripping exists for known-sender mail only (`cv-ingest/src/boilerplate.rs`), not for RSS bodies.
