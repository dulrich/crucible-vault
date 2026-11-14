# Changelog

All notable changes to this project are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## 1.1.0 — 2026-11-14

### Added

- **Publication records:** `sources.toml` now holds one `[[publication]]` per publication (its feeds, mail senders and list ids, hosts, URL prefixes, YouTube channel, kind and In Works) instead of separate blog, newsletter, serial and YouTube rows. The old tables still load, folded by publication name, and are deprecated; `cv sources migrate [--dry-run]` rewrites them as records. A sender, feed, host, prefix or channel shared by two records is a config error.
- **Sources overview:** Settings, Sources lists the publications grouped by kind, with an In Works toggle on each.
- **Create Source from note:** an editor command proposes a publication from the open note (name, kind, host and prefix, and the sender from the retained email) as an unsaved record in Settings, Sources, or opens the record that already matches.
- **`intake-frontmatter-backfill`:** a one-shot action that sets `read: false` on every older intake note that lacks it, and `publication` where blank, without overwriting anything. On the owner's vault the Reading List and Unread grow by roughly 600 notes.
- **Migration 0041:** reconciles intake first (so mail keyed by an old newsletter name is re-keyed), rewrites `sources.toml` as publication records, sets `[kinds.fanfic] works = true` and drops each record's own `works` that equals its kind default, then runs the backfill. It fails, touching nothing, when `sources.toml` does not load (for example a matcher shared by two publications). `cv sources migrate` warns when legacy newsletter names are about to be renamed.

### Changed

- **One note per post across a publication's hosts:** a post is identified by its publication and path, so the feed copy, the custom-domain copy and the email of one post are one note in either arrival order, and a new RSS post is resolved against the vault before it is written.
- **In Works per publication and per kind:** a publication is in Works when its own `works` says so, else when its kind's `works` in `kinds.toml` does, else not; Works rows and `file.serial` follow it, so Works Unread no longer lists a publication that is out of Works.
- **Saving sources re-matches unknown-sender rows:** a change to `sources.toml` enqueues a sources-only reconcile, so mail from a sender you just added is filed against it without a manual `cv intake reconcile`.

### Known limitations

- A `sources.toml` save that lands while a re-match is already running is folded into that run and may not be picked up until the next save or a full `cv intake reconcile`.

## 1.0.4 — 2026-11-13

### Changed

- **Home on list lines cycles:** in Source, a further Home at column 0 of a list or task line returns to the content start after the marker, so repeated Home presses cycle content start → indent end → column 0 → content start.
- **Ctrl+A selects the body first:** in a note with frontmatter, Ctrl+A in Reading or Source selects the body; pressing it again selects the whole note, properties included, and a third press returns to the body.
- **Reading typography:** Reading shows `->` `-->` `--` `---` `...` `!=` `>=` `<=` in prose as `→` `⟶` `–` `—` `…` `≠` `≥` `≤`. Inline code, code and math blocks, tags, URLs and `$math$` keep their ASCII. The file, Source mode, copy and search still use the ASCII: copying pastes ASCII, and find `->` highlights the arrow.

## 1.0.3 — 2026-11-13

### Changed

- **Fanfic `Ch` titles:** a clip titled `Fanfic <Story> Ch <n>, <Fandom>` (or `Ch. <n>`) is renamed to `<Story> - Chapter <n>` at filing, and by the `fanfic-title-backfill` action.
- **Home on list lines:** in Source, Home on a list or task line (`- `, `- [ ] `, `1. `, `1) [x] `) first stops just after the marker, then at the indent end and column 0 as before.
- **Reading uses Inter for body text:** plain Reading text was drawn in the generic sans-serif fallback (DejaVu Sans on Debian) instead of the app font, so Inter's arrows never showed; `<-` and `=>` now render as arrows in Reading as they do in Source. Stored, copied and found text is unchanged (`->` and `--` have no Inter substitution).
- **Image descriptions as alt text:** hovering a described vault image in Reading shows its stored description, and the image menu gains **Copy alt text** (the description, else the markdown alt). The daemon answers a new `image_description` request; an older daemon simply yields no description.
- **Selectable labels:** frontmatter property keys in Reading, and view titles, the filter chip and column headers, can be drag-selected and copied with Ctrl+C; a plain click on a column header still sorts. A Reading selection across a property row copies `key: value` once, and a boolean property now contributes its key line to a whole-note copy.

## 1.0.2 — 2026-11-11

### Changed

- **Hidden tabs release their images:** a tab that no pane shows no longer keeps its images, image-tab bitmap or PDF pages in memory; they are decoded again when the tab is shown. With 15 image-heavy notes open the editor settles about 240 MiB lower (measured against 1.0.1).
- **Image decodes are capped at two at a time:** opening image-heavy notes no longer decodes every image at once, which bounds the memory held by decodes in flight.
- **Search index open:** the first search opens the index on a blocking thread instead of a tokio worker, and `semsearch` 0.3.1 computes its sign codes straight from the stored vectors, so opening allocates about 45 MiB instead of about 428 MiB. Search results are unchanged.
- **View property decode:** view property values are decoded straight from the stored JSON text instead of through an intermediate JSON tree, which saves about 18 MiB of allocation per pass over the views. View results are unchanged.

## 1.0.1 — 2026-11-11

### Changed

- **Editor stays responsive while the daemon is busy:** opening, reloading and reconnecting a note settle off the UI thread, and an open note is addressed by its note id, so it follows a rename, a move, or a replacement by another note.
- **Reconnect states:** a note deleted while the editor was disconnected reads "file deleted on disk"; two rapid opens of the same unopened path keep the later one.
- **Config vocabulary:** an unknown `folder("<key>")` in a view definition is reported as one load notice, and the default Publications page follows `[structure] app_folder`; creating it from the explorer gives an empty note, the navigation link seeds it.
- **Memory:** both processes release freed heap through one `cv-malloc` entry (triggers and thresholds unchanged), and the protocol client no longer copies a reply's payload or keeps a large reply's line buffer.
- **View test goldens** are one line per column and per row.
- **Memory, replies:** the daemon serialises each reply straight to text (lower peak memory, no leftover allocator arenas), and its idle and 5-minute releases now trim above 16 MiB of free memory.
- **Memory, idle release:** the daemon releases freed heap once no request and no job has run for 20 seconds, so memory left by editor view requests no longer waits for the next job; the 5-minute tick stays as a backstop.
- **Memory, views:** a view run loads only the note properties its expressions can reach, and falls back to loading all of them when that set cannot be enumerated.
- **Reply key order:** object keys in a reply follow struct declaration order instead of sorted order (equal as JSON; clients that decode by field name are unaffected).

### Security

- **Dependency updates for three advisories:** `rusqlite` 0.40.2 → 0.41.1 with `libsqlite3-sys` 0.38.2 → 0.39.1 (RUSTSEC-2026-0459, through `semsearch` 0.3.0), `rustls-webpki` 0.103.15 → 0.103.16 and `webpki-root-certs` 1.0.9 → 1.0.12 (RUSTSEC-2026-0382, RUSTSEC-2026-0383).

### Added

- **Migration libs for new migrations:** `deploy/migrations/lib/view-def.sh` (anchored view-definition edits) and `anchor_array_add` (TOML array adds).

## 1.0.0 — 2026-11-08

First public release: a Rust Obsidian-Flavored-Markdown vault app that pairs a headless core daemon with an editor GUI client.

### Added

- **Headless daemon (`cv`):** the vault index, the only writer to the vault, an action runner, a durable job queue, and `obsidian://` URI intake.
- **Editor GUI client:** a lightweight native editor that talks to the daemon over a local protocol and never writes the vault itself.
- **Obsidian-Flavored Markdown:** parsing and editing of OFM notes (wikilinks, embeds, callouts, properties, tags).
- **Views:** Bases-style table and list views defined in TOML, with an expression language, filters, sorting and grouping.
- **Semantic and full-text search:** in-process semantic search plus an FTS5 index, available from the editor.
- **Config-driven vault structure:** the app folder, exclude list, raw-transcript folder, periodic-note folders, editor navigation and note property types are all configurable, with neutral defaults and examples under `config/examples/`.
- **Data-migration mechanism:** ordered, idempotent, append-only deploy migrations applied by the installer, for changes to a vault's live configuration beyond rebuild and restart.

### Changed

- **state.db baseline:** the schema history is folded into a single 1.0.0 baseline at `user_version` 17 (job telemetry and path lineage included). Databases at v17 open unchanged; versions 1 to 16 are refused with a message naming the baseline. From here, structural changes within a major version are appended migration steps, and the next major folds them into a new baseline. `index.db` and `search.db` remain derived and are rebuilt on a schema mismatch.
