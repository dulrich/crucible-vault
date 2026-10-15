# crucible-vault

A Rust Obsidian-Flavored-Markdown vault app — headless core daemon plus editor GUI — that replaces Obsidian + the Crucible plugin for a personal vault at a fraction of the memory footprint, while keeping the Obsidian Web Clipper as the capture path.

Status: pre-plan (spikes and feature planning in progress). See `docs/assessment-2026-10-13.md` and `docs/crucible-feature-inventory.md`.

Settings: Ctrl+, opens the editor Settings tab, which edits the vault config files in `_custom/crucible-vault/` in place (comments kept, saved through the daemon); `editor.toml` holds the editor prefs and hotkeys.
