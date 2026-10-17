# Install migrations

`./install.sh` applies each `NNNN-<slug>.sh` here once, in name order, and records it in `${XDG_STATE_HOME:-~/.local/state}/crucible-vault/install.applied` (one `<id>:<phase> <date> <commit>` line per applied phase). A recorded phase never runs again, so re-running `./install.sh` only rebuilds and restarts.

A migration is a bash file that defines one or both functions:

- `migrate_stopped` — runs after the editor has quit and `crucible-vault.service` is stopped (edit `state.db`, live config in `$CONFIG_DIR`, unit files).
- `migrate_running` — runs after the daemon is back up (`cv run …`, backfills).

Environment: `REPO`, `VAULT`, `CONFIG_DIR` (`$VAULT/_custom/crucible-vault`), `STATE_DB`, `BACKUP_DIR`, and the `backup <file>` helper (call it before editing any file). The function runs under `set -euo pipefail`; a non-zero exit stops the install with nothing recorded for that phase.

Rules:

- **Idempotent anyway** — check the target state first and no-op when it already holds (a phase can be re-run after a partial failure, and the user may have applied the change by hand).
- **Never assume the user's file is pristine** — the live `actions.toml` carries user edits; edit by anchored insertion/replacement and fail with a clear message when the anchor is gone, never overwrite wholesale.
- **Numbering is append-only** — never renumber or edit an applied migration; ship a new one.
- Note content stays opaque: migrations touch `_custom/` config and daemon state, never personal notes directly (run an action through `cv run` instead).
