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
- **Skip-and-log carve-out (0006, 0007, 0008 and 0013)** — a migration whose target is a user-editable view the user has changed (not the pristine text it knows) may skip and log a clear message instead of failing the phase, provided it never edits the changed file (exit 0, phase recorded); every other config edit still fails on a gone anchor. 0007, 0008 and 0013 use it for `views/works.toml` (0008 skips each edit whose 0007 anchor is gone, 0013 each whose 0008 anchor is gone, independently); 0007's `sources.toml` edit is an append of absent `[[serial]]` rows (no anchor), and it only reads the `core/works` notes.
- **Generated hub note carve-out (0012, 3a)** — 0012 creates the generated hub note `core/Ingestion.md` (embedding the shipped `ingestion` view sections) only when it is absent; an existing note is never edited.
- **Anchored Desktop row carve-out (0012, 3b)** — 0012 inserts exactly one row into `Desktop.md`, immediately after the first line beginning `| [[Works]]`; it is a no-op when `Desktop.md` already contains `[[Ingestion]]`, and skips and logs (exit 0, nothing edited) when the anchor line or the file is gone. It backs up first and preserves every other byte.
- **Appended Ingestion section carve-out (0014 and 0017, 3c)** — 0014 appends (absent-checked, append-only, backup first, every other byte preserved, CRLF and a missing final newline handled) one `## Possible duplicates` section to the generated hub note `core/Ingestion.md` (user-directed 2026-10-25) and one `[[views]]` block to the user-editable `views/ingestion.toml`; it skips and logs (exit 0, nothing edited) when either file is absent, and no-ops when the view or section is already present. 0017 (plan tidiness-trash-orphans WP-3) does the same for one `## Trash` section embedding `ingestion#Trash` in `core/Ingestion.md` and the shipped `Trash` `[[views]]` block (bounded at its first blank line) in `views/ingestion.toml`, with the same skip and no-op behavior.
- **Numbering is append-only** — never renumber or edit an applied migration; ship a new one.
- Note content stays opaque: migrations touch `_custom/` config and daemon state, never personal notes directly (run an action through `cv run` instead).
