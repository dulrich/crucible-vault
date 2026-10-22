# Newsletter mail intake

cv reads newsletters from a **Maildir drop folder outside the vault** (plan `auto-intake-sprint`, decision D25). It never speaks IMAP or SMTP: an external fetcher, or you saving `.eml` files, fills the folder, and the `track-mail` action (every 15 minutes) turns each message into a note or supersedes an existing RSS stub with the full body. Nothing is fetched from the network by cv for mail.

## 1. Fill the Maildir

Pick a directory outside the vault, for example `~/Mail/newsletters`. cv scans `<maildir>/new/*` and loose `<maildir>/*.eml`. Either of these fills it.

### Proton Bridge + mbsync (automatic)

1. Install and sign in to Proton Mail Bridge (it exposes a local IMAP server on `127.0.0.1`, with a Bridge-generated password).
2. Install `isync` (`mbsync`) and create `~/.mbsyncrc` with an IMAP account pointing at the Bridge (host `127.0.0.1`, the Bridge's IMAP port, the Bridge username and password, `TLSType STARTTLS`/`CertificateFile` as Bridge documents), a Maildir store at `~/Mail/newsletters`, and a channel that syncs only the newsletter folder (next section), e.g. `Patterns "Folders/Newsletters"`, `Create Near`, `SyncState *`.
3. Run `mbsync <channel>` from a systemd user timer (every 10 to 15 minutes). mbsync writes new mail to `<maildir>/new/`.

### Saving `.eml` by hand (no Bridge)

In Proton Mail web open a newsletter, use **More → Export** (or drag the message out), and put the `.eml` file into `<maildir>/` (loose files are scanned). Useful when you only want a few paid issues.

## 2. A Proton filter that routes newsletters to one folder

In Proton Mail: Settings → Filters → Add filter. Match the senders (or the `List-Id` / `List-Unsubscribe` header, when the publisher sets one), and apply **Move to** a folder named `Newsletters` (and optionally mark as read). Sync only that folder with mbsync. Many ESPs (HubSpot and others) omit `List-Id`, so match on `From`.

## 3. Turn it on

In `pipelines.toml` (the migration `0001-auto-intake-config` adds this disabled):

```toml
[mail]
enabled = true
maildir = "/home/you/Mail/newsletters"
processed = "cur"               # a handled new/ message moves to <maildir>/cur/<name>:2,S; loose files to <maildir>/processed/
watch = true                    # default: a dropped message enqueues track-mail once it settles (~5 s quiet, size stable)
```

An empty `maildir` is "unset" and the step does nothing. Handled messages are moved, never deleted; an unparseable message stays where it is.

With `watch = true` (the default) the daemon watches `<maildir>` and `<maildir>/new` (non-recursive) and enqueues `track-mail` once a dropped message has been quiet for about 5 seconds with an unchanged size; the 15-minute schedule stays as the backstop, and a missing directory is logged once and left to the schedule until the config changes. Migration `0002-intake-followups` writes the explicit `watch = true` into an existing `[mail]` block.

## 4. Describe the senders: `[[newsletter]]` rows

In `sources.toml` (editable in the Settings tab):

```toml
[[newsletter]]
name = "Example Letter"
from = ["writer@news.example"]      # exact address, or "@news.example" for a whole domain
# list_id = "letter.news.example"   # when the publisher sets List-Id
blog = "Example Blog"               # optional: the [[blog]] row this letter mirrors (its RSS stubs are superseded)
kind = "blog"                       # kinds.toml key for the captured note
tags = ["example"]
paywall_markers = ["Upgrade to read the rest"]
```

A row needs a `from` or a `list_id`. Mail from a sender no row matches is still captured (source "unknown-sender") and left open in Intake; it is never dropped.

**Substack auto-match (plan `intake-followups-mail-youtube` WP-2).** Mail whose `List-Id` (else sender local part) is `<pub>.substack.com` needs no `[[newsletter]]` row when a `[[blog]]` row's feed `link` host is `<pub>.substack.com`: it behaves as a newsletter of kind `blog` named for that blog row, mirroring it (RSS supersede, the blog row's paywall markers). An explicit `[[newsletter]]` row always wins over the auto-match. To follow a Substack publication you only need a `[[blog]]` row on `https://<pub>.substack.com/feed`.

Migration `0002-intake-followups` adds the rows the 2026-10-21 mail dump needed (each only when absent): `[[newsletter]]` rows for the `news` senders Newsbit and Example News Report, the `blog` senders Example Weekly (example-creator) and Example Letter (beehiiv), and `blog`-linked rows for Example Newsletter and Example Author (custom-domain feeds the host auto-match cannot reach); plus `[[blog]]` rows on `<pub>.substack.com/feed` for pub-a, pub-b and pub-c, which the auto-match then covers. A new `[[blog]]` row makes `track-blogs` fetch that feed.

**Body cleanup (WP-1).** Before the word count and the note are made, the markdown body is cleaned of newsletter chrome (`cv_ingest::mail::clean_body`): tracking redirects are unwrapped, `utm_*` parameters and tracking pixels dropped, share/like/restack rows and the unsubscribe footer removed. The paywall-marker verdict is taken on the body before cleanup, so removed chrome can never turn a partial message complete.

## Round 2: cleanup rules, canonical identity, publication (plan `intake-round-2-clean-dedupe-reading`)

**Cleanup rules (WP-1).** Besides the round-1 chrome, the cleanup drops CSS lines, spacer lines, referral blocks, and icon, pixel and avatar images (a markdown image whose decoded size is 64x64 or smaller in every known dimension, with its wrapping link). Post-body images and prose paragraphs are never touched, and the cleanup is idempotent. A localized `_attachments/…` image has no size in its markdown, so the reconcile re-clean decodes the file header from the vault (dimensions only) and hands that to the cleanup; remote images are judged by their URL and markup only.

**Canonical identity (WP-2).** A mail note's `source` (and `post-id`, when it has one) is one canonical post URL: the first link of the message with publication evidence (its host is the Substack host of the mail, or the `[[blog]]` row's host), rebased onto the blog row's host when there is one, and free of tracking (`token=`, `r=`, a JWT path segment, redirect and ESP hosts). An unproven or opaque link is never the identity; with none, the Message-ID stays the identity and no `source` is written. That is what lets a restack, redirect, `open.substack.com` or custom-domain link converge on the RSS note instead of creating a second one.

**`publication` (WP-3).** Every new blog, mail, YouTube and clip note carries `publication`, the name shown in the Reading List's Author column: the source row's optional `short`, else its name (`channel` for YouTube). An existing `publication` is never overwritten.

**Reconcile repair phases (WP-6).** `cv intake reconcile [--dry-run]` also runs, after the backfill phases above (a dry run prints the same counts and writes nothing to the vault or `state.db`):

- **Re-clean r2** (`mail_recleaned_r2`, the round-1 `mail_recleaned` renamed): the re-clean above, now with the image-dimension hook.
- **Re-key** (`mail_rekeyed`, `mail_token_stripped`, `rekey_skipped`, `rekey_conflict`): each `via: email` note with a `message-id` finds its `.eml` in the processed folders and gets the canonical `source` (and `post-id` if present) when it differs. With no canonical URL, a `source` that is not a clean identity (a token or tracker URL) is removed together with `post-id`. A note whose message is gone counts `rekey_skipped` and is otherwise left alone.
- **Merge** (`dup_merged`, `dup_marked`, `dup_other`, `dup_distinct`, `dup_conflict`): a mail note whose canonical identity now matches a `via: rss` note is resolved against it by the supersede engine, and **only mail and RSS twins are merged**. A match with a clip or any other note is `dup_other` and untouched. The mail note moves to `.trash/` (recoverable) only when it is redundant, by the same rule as clip filing: the RSS note was replaced by the fuller mail body, or it was kept and already contains the mail body (containment at or above `[supersede] threshold_percent`). Otherwise both stay and the mail note gets `duplicate-of: [[<rss note>]]`. Before the trash, every tracker row pointing at the mail note is re-pointed to the surviving note and the survivor's rows are finalized on its resolved body, so an interrupted run never leaves a row pointing at a missing note and a rerun finishes the pair.
- **Publication** (`publication_backfilled`, `publication_no_row`, `publication_conflict`): each note linked from a tracker row and lacking `publication` gets its source row's value, after `author`. A note whose rows match no source row counts `publication_no_row`.

**Round 9 phases (plan `round-9-feedback-todos` WP-2).** The reconcile also runs:

- **Description repair** (`mail_description_repaired`): when the re-clean changes a body, `description` is recomputed from the cleaned body (its first 40 words, as capture does); a `description` that carries CSS (an `@media`/`@import`/`@font-face`/`@keyframes` at-rule, `!important`, or a `{ property: … }` block) is recomputed even when the body is already clean. Any other description is left.
- **RSS origin** (merge): a note with no `via: email` that a `blog` tracker row points at (pre-D25 RSS notes have no `via`) now counts as the RSS twin of a mail note.
- **Forum re-key** (`forum_rekeyed`, `forum_rekey_conflict`): a forum post `source` (`…/threads/…#post-<P>`) whose stored `post-id` is still the page-level key gets `https://<host>/posts/<P>`.
- **Spurious duplicate-of** (`dup_of_removed`, `dup_of_kept`, `dup_of_conflict`): removed only when the note and its target shared the old page key, share no identity after the re-key, and have no content match; every other `duplicate-of` stays.
- **Fanfic publication** (`fanfic_publication_backfilled`, `fanfic_publication_no_work`, `fanfic_publication_conflict`): a `fanfic`-tagged note without `publication` gets the matched Work's `title` (the `auto_file` Works match).

The deploy migration `0004-round-9-identity-repair` runs the reconcile once, then the localize folder backfills; a note excluded from the backfill before escaped-paren image URLs were matched (`\(`/`\)` in a remote image URL) gets one retry, every other excluded note stays excluded.

The historical same-id duplicates that are not a mail/RSS pair stay as they are. The deploy migration `0003-intake-round-2` also adds the `tag = "clippings"` starter to `clip-images` and the Author column, `maxHeight = "none"` and numeric column widths to the Reading List view, then runs the reconcile once.

## What happens to a message

1. The post URL is looked for in the HTML (Substack `/p/<slug>`, `open.substack.com/pub/<pub>/p/<slug>`, "view in browser" links). ESP tracking redirects are unwrapped only when the target is in the URL itself; nothing is fetched.
2. The supersede engine resolves it against the vault by canonical URL, then by content containment within `[supersede] window_days` of the same source. A fuller email body replaces the RSS stub **in place** (path, links, `read` and your properties are kept; the old body is saved under the state dir's `superseded/`). A complete existing note stays.
3. Otherwise a new captured note is written (`via: email`). An email body is complete unless a paywall marker matches (an email never inherits the RSS stub's `complete`); a complete one is recorded done, so the Reading list owns it. Partial ones stay open in Intake.

Re-running is a no-op (the Message-ID is recorded).

## Backfill: `cv intake reconcile` (plan `intake-followups-mail-youtube` WP-5)

`cv intake reconcile [--dry-run]` also repairs mail already recorded. `--dry-run` prints every count and writes nothing; the output is aggregate `key=value` lines only.

- **Re-match** (`mail_*`): each message recorded under `unknown-sender` is found again by Message-ID in `<maildir>/<processed>/` and `<maildir>/processed/` (a file that is gone counts `mail_missing` and the row stays as it is). When the newsletter resolver (explicit row, else the Substack auto-match) now matches, the row is re-keyed to the newsletter's name (`mail_rematched`; a row already at that key absorbs the old row's missing fields, `mail_collisions`). The note gets the newsletter's kind and extra tags and is **not moved** (a note in another kind's folder counts `mail_folder_mismatch`), a paywall marker of the newsletter in the original body marks it `complete: false`, and the row is ticked only when the note is complete (`mail_ticked`). No note is ever created.
- **Re-clean**: every note with `via: email` has its body put through the same cleanup as new mail (frontmatter untouched, written only on a change, skipped when you edited the note meanwhile): `mail_recleaned_r2`, `mail_clean_unchanged`, `mail_clean_conflict`. A second run changes nothing.
- **YouTube** (`yt_*`): an open YouTube row ticks when its filed note is a clipping of that video with transcript text; a note that only embeds the video never does (`yt_no_evidence`).

The `.eml` files must still sit in the processed folders for the re-match; rows whose message is gone stay `unknown-sender`.
