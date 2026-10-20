# Install migrations

**Baseline 0.1.0 (D23, 2026-10-19).** Every migration shipped before the 0.1.0 baseline (`0001-vault-sweeps-config` … `0011-search-reembed`) was applied on this machine and folded into it; the files are deleted and `state.db` starts at the v10 baseline. Numbering restarts at `0001` after the baseline. A new migration's id (`<NNNN>-<slug>`) must not reuse a pre-baseline id: the ledger key is the full id, so `0001-<new-slug>` never collides with `0001-vault-sweeps-config`, but reusing an old slug with its old number would be skipped as already applied. Ledger lines for deleted pre-baseline files are ignored by `./install.sh`.

**Baseline 0.1.1 (D24, 2026-10-19).** state.db's v10→v11 step (it dropped the retired shadow-write log, the cut-over marker and an unused tracker column; D24) was applied on this machine and folded into the baseline: `state.db` starts at v11 and versions below it are refused. No deploy scripts shipped with it; numbering is unchanged.

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
