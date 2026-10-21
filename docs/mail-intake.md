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

## What happens to a message

1. The post URL is looked for in the HTML (Substack `/p/<slug>`, `open.substack.com/pub/<pub>/p/<slug>`, "view in browser" links). ESP tracking redirects are unwrapped only when the target is in the URL itself; nothing is fetched.
2. The supersede engine resolves it against the vault by canonical URL, then by content containment within `[supersede] window_days` of the same source. A fuller email body replaces the RSS stub **in place** (path, links, `read` and your properties are kept; the old body is saved under the state dir's `superseded/`). A complete existing note stays.
3. Otherwise a new captured note is written (`via: email`). An email body is complete unless a paywall marker matches (an email never inherits the RSS stub's `complete`); a complete one is recorded done, so the Reading list owns it. Partial ones stay open in Intake.

Re-running is a no-op (the Message-ID is recorded).

## Backfill: `cv intake reconcile` (plan `intake-followups-mail-youtube` WP-5)

`cv intake reconcile [--dry-run]` also repairs mail already recorded. `--dry-run` prints every count and writes nothing; the output is aggregate `key=value` lines only.

- **Re-match** (`mail_*`): each message recorded under `unknown-sender` is found again by Message-ID in `<maildir>/<processed>/` and `<maildir>/processed/` (a file that is gone counts `mail_missing` and the row stays as it is). When the newsletter resolver (explicit row, else the Substack auto-match) now matches, the row is re-keyed to the newsletter's name (`mail_rematched`; a row already at that key absorbs the old row's missing fields, `mail_collisions`). The note gets the newsletter's kind and extra tags and is **not moved** (a note in another kind's folder counts `mail_folder_mismatch`), a paywall marker of the newsletter in the original body marks it `complete: false`, and the row is ticked only when the note is complete (`mail_ticked`). No note is ever created.
- **Re-clean**: every note with `via: email` has its body put through the same cleanup as new mail (frontmatter untouched, written only on a change, skipped when you edited the note meanwhile): `mail_recleaned`, `mail_clean_unchanged`, `mail_clean_conflict`. A second run changes nothing.
- **YouTube** (`yt_*`): an open YouTube row ticks when its filed note is a clipping of that video with transcript text; a note that only embeds the video never does (`yt_no_evidence`).

The `.eml` files must still sit in the processed folders for the re-match; rows whose message is gone stay `unknown-sender`.
