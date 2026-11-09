# Changelog

All notable changes to this project are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## 1.0.0 - 2026-11-08

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
