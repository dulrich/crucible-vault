# crucible-vault

A Rust Obsidian-Flavored-Markdown vault app that replaces Obsidian and the [Crucible](https://github.com/dulrich/obsidian-crucible) plugin for one vault at a fraction of the memory footprint. It is a headless core daemon (`cv`: the vault index, the only writer, the action runner, a durable queue and `obsidian://` intake) plus an editor GUI client (`cvault-editor`). The Obsidian Web Clipper stays the capture path: clips arrive through `obsidian://` and the daemon files them, with or without the editor open.

Status: 1.0.0. Design decisions are recorded in `docs/decisions.md`; the newsletter Maildir intake is described in `docs/mail-intake.md`.

## Build

The OFM parser and the semantic-search crate are path dependencies, so clone them as siblings of this checkout:

```bash
git clone https://github.com/dulrich/crucible-tui
git clone https://github.com/dulrich/semsearch
git clone https://github.com/dulrich/crucible-vault
cd crucible-vault
cargo build --workspace --release
```

The expected layout is `../crucible-tui/crates/ofm` and `../semsearch` next to this directory. The editor builds with the wgpu GPU renderer by default (the `gpu` feature); set `WGPU_BACKEND` or `ICED_BACKEND` to pick a backend, or build the software renderer with `cargo install --path crates/editor --no-default-features --features cpu`.

## Install and configure

`./install.sh` builds, installs the binaries, applies pending data migrations and restarts the daemon (and closes and relaunches the editor). The vault defaults to `~/vault`; override it with `CV_VAULT=/path/to/vault`. Configuration is a set of TOML files in `<vault>/_custom/crucible-vault/`, hot-reloaded by the daemon; Ctrl+, opens the editor Settings tab, which edits them in place (comments kept). `editor.toml` holds editor prefs and hotkeys. Annotated examples are in `config/examples/` (actions, agents, captures, kinds, pipelines, providers, schedules, sources, vault); example view definitions and notes are in `config/examples/views/` and `config/examples/notes/`.

### Suggested vault structure

Every folder below is a documented default and can be changed in config; nothing requires you to use these names.

- `daily/day/` and `daily/month/`: period folders for daily and monthly notes (`[periods.*]` in `captures.toml`).
- `Clippings/`: the inbox where the Web Clipper drops new notes (the configured inbox).
- `_x_metadata/`, `_yt_metadata/`, `_blog_metadata/`: tracker and metadata folders (`metadata_root` in `pipelines.toml`).
- `_raw_transcript/`: the original of a transcript note, kept before it is refined (`[structure] raw_transcript_folder` in `vault.toml`).
- `_custom/`: your config, templates and prompts; `_custom/crucible-vault/` holds the daemon config.
- `.trash/`: the soft-delete folder, matching Obsidian (`[trash]` in `vault.toml`).
- `_crucible/`: the app's own folder, for example review notes in `_crucible/reviews` (`[structure] app_folder`).
- A shared exclude list (`[structure] exclude`) that duplicate detection, link scans and similar steps all read.

### Data migrations

A change that needs a live-config or state edit ships as a migration script. `install.sh` applies your own data migrations from `${CV_MIGRATIONS_DIR:-deploy/migrations/local}`; the contract is in `deploy/migrations/README.md` (idempotent, anchored edits, append-only numbering, a ledger keyed by migration id). The migration layout and `CV_MIGRATIONS_DIR` land with the publication release.

### systemd

The daemon runs as a systemd user service. An example user unit will live under `deploy/examples/`; adapt the paths and environment to your machine.

### Optional CDE integration

If the `crucible` CLI of the [Crucible Dev Env (CDE)](https://github.com/dulrich/crucible-dev-env) compositor is on `PATH`, `install.sh` uses it to close and relaunch the editor around a deploy. When the CLI is absent those hooks are a no-op.

## Licence

MIT, see `LICENSE`. The bundled Lucide icon font (`crates/editor/assets/lucide/`) is under the ISC licence, see the `LICENSE` beside it.
