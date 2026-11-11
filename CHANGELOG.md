# Changelog

All notable changes to this project are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## Unreleased

### Changed

- **Editor stays responsive while the daemon is busy:** opening, reloading and reconnecting a note settle off the UI thread, and an open note is addressed by its note id, so it follows a rename, a move, or a replacement by another note.
- **Reconnect states:** a note deleted while the editor was disconnected reads "file deleted on disk"; two rapid opens of the same unopened path keep the later one.
- **Config vocabulary:** an unknown `folder("<key>")` in a view definition is reported as one load notice, and the default Publications page follows `[structure] app_folder`; creating it from the explorer gives an empty note, the navigation link seeds it.
- **Memory:** both processes release freed heap through one `cv-malloc` entry (triggers and thresholds unchanged), and the protocol client no longer copies a reply's payload or keeps a large reply's line buffer.
- **View test goldens** are one line per column and per row.
- **Memory, replies:** the daemon serialises each reply straight to text (lower peak memory, no leftover allocator arenas), and its idle and 5-minute releases now trim above 16 MiB of free memory.
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
