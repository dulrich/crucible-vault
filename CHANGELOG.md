# Changelog

All notable changes to this project are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## Unreleased

### Changed

- **Fanfic `Ch` titles:** a clip titled `Fanfic <Story> Ch <n>, <Fandom>` (or `Ch. <n>`) is renamed to `<Story> - Chapter <n>` at filing, and by the `fanfic-title-backfill` action.
- **Home on list lines:** in Source, Home on a list or task line (`- `, `- [ ] `, `1. `, `1) [x] `) first stops just after the marker, then at the indent end and column 0 as before.
- **Reading uses Inter for body text:** plain Reading text was drawn in the generic sans-serif fallback (DejaVu Sans on Debian) instead of the app font, so Inter's arrows never showed; `<-` and `=>` now render as arrows in Reading as they do in Source. Stored, copied and found text is unchanged (`->` and `--` have no Inter substitution).

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
