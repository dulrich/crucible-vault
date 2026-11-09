# Spike: auto-intake premises (2026-10-20)

Promoted from `runs/spike/paywall-signal/` by plan auto-intake-sprint WP-6. Aggregates only; no note text, titles or personal paths. Commands run read-only against the live vault and `state.db` (`?mode=ro`).

## 1. "Intake duplicates the Reading list" = double listing, not double writing

- `select source_kind, outcome, full_body, coalesce(status,'NULL'), count(*) from tracker_items where first_seen_ms > <2026-10-14> group by 1,2,3,4` → blog `note` outcomes: 102 (92 status NULL, 10 `x`). Each also appears in `daily-reading-list` (it is a note in `daily/day/<date>/`) — so the post is listed twice (Intake view Done box + Reading list read box), and only 10 were ticked.
- 79 of the 102 `note` rows have `note_path` NULL (days 2026-10-16..17) → their Intake row links the URL, not the note. Anomaly to diagnose in the plan.
- Vault scan of tracker notes (`post-id` present, 2873): 63 groups share a `source`; **all 63 pre-cutover** (Crucible era), 62 in the same day folder. cv itself is not re-writing posts. Clip-vs-tracker overlap by exact `source`: 1 group. `Clippings/` holds 9 files.

## 2. Paywall signal: Substack `/api/v1/posts/<slug>` `audience`

`curl -s https://<host>/api/v1/posts/<slug>` (no auth) over the 40 most recent tracked Substack-shaped post URLs (`results.txt`, the host column dropped: aggregates only): 34 `everyone`, 5 `only_paid`, 1 `founding`. For the 6 paid posts, feed word count / API `wordcount`: 84/1109, 418/5296, 497/5358, 374/1188, 72/531, 30/3240 → the feed carries 3–31 % of the post. Free posts' feed body ≈ full. Signal: `audience != everyone` ⇒ paid; `feed_wc / wordcount < ~0.8` ⇒ partial.

## 3. Works → kind

21 works notes: 20 with `source-prefix-filter` (21 prefixes over 3 host groups: 11, 5 and 5), 15 with `author-filter`; tags carrying a `kinds.toml` key: 7 of one kind, 2 of another, 1 of a third.

## 4. Mail infrastructure

No IMAP/MTA/bridge tooling on the machine (no mail fetchers or MTAs installed; no Maildir). No mail-provider config present beyond non-mail clients.

## 5. Real newsletter `.eml` shape (maintainer sample, 2026-10-20)

Structure only (no body text recorded): 88 KB, CRLF, provider export with full headers. `multipart/mixed` → `multipart/related` → one `text/html; charset=utf-8`, `Content-Transfer-Encoding: base64` (60 KB decoded). **No `List-Id`**; `List-Unsubscribe` and `Message-Id` sit on the ESP's domain (HubSpot); `From` is on the publisher's domain. Link hosts in the HTML: 26 × `*.hubspotlinks.com` tracking redirects, 2 × `*.hubspotemail.net`, **zero direct publisher post URLs** and no "view in browser" text. The publisher is not in `sources.toml`. So for ESP-sent newsletters: URL identity is unavailable without fetching (WP-4 fetches nothing), the `[[newsletter]]` matcher must work on `From` (address or domain) because `List-Id` may be absent, and this is an email-only source (outcome `new`).
