# crucible-vault

A Rust Obsidian-Flavored-Markdown vault app — headless core daemon plus editor GUI — that replaces Obsidian + the Crucible plugin for a personal vault at a fraction of the memory footprint, while keeping the Obsidian Web Clipper as the capture path.

Status: pre-plan (spikes and feature planning in progress). See `docs/assessment-2026-10-13.md` and `docs/crucible-feature-inventory.md`.

Settings: Ctrl+, opens the editor Settings tab, which edits the vault config files in `_custom/crucible-vault/` in place (comments kept, saved through the daemon); `editor.toml` holds the editor prefs and hotkeys.

Renderer: the editor builds with the wgpu GPU renderer by default (`gpu` feature; every launch defaults to Vulkan on the AMD radeon ICD on Linux unless you set `WGPU_BACKEND`, `VK_LOADER_DRIVERS_SELECT`, `VK_DRIVER_FILES`/`VK_ICD_FILENAMES` or `ICED_BACKEND`); for the software renderer build with `cargo install --path crates/editor --no-default-features --features cpu`.
