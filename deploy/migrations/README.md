# Install migrations

**Baseline 0.1.0 (D23, 2026-10-19).** Every migration shipped before the 0.1.0 baseline (`0001-vault-sweeps-config` … `0011-search-reembed`) was applied on this machine and folded into it; the files are deleted and `state.db` starts at the v10 baseline. Numbering restarts at `0001` after the baseline. A new migration's id (`<NNNN>-<slug>`) must not reuse a pre-baseline id: the ledger key is the full id, so `0001-<new-slug>` never collides with `0001-vault-sweeps-config`, but reusing an old slug with its old number would be skipped as already applied. Ledger lines for deleted pre-baseline files are ignored by `./install.sh`.

**Baseline 0.1.1 (D24, 2026-10-19).** state.db's v10→v11 step (it dropped the retired shadow-write log, the cut-over marker and an unused tracker column; D24) was applied on this machine and folded into the baseline: `state.db` starts at v11 and versions below it are refused. No deploy scripts shipped with it; numbering is unchanged.

**Baseline 0.2.0 (D27, 2026-10-27).** Deploy migrations `0001`–`0017` (the 0.1.1-era scripts, incl. the `0006`–`0017` carve-outs) and state.db's v11→v15 steps were applied on this machine and folded into the baseline: the scripts, their `test-NNNN.sh` harnesses and fixtures are deleted and `state.db` starts at v15 (versions below it are refused). **Post-baseline numbering continues at `0018`** — it does not restart at `0001`, so the install ledger never shows a third `0001-*` id. Ledger lines for the deleted files are ignored by `./install.sh`.

`./install.sh` applies each `NNNN-<slug>.sh` here once, in name order, and records it in `${XDG_STATE_HOME:-~/.local/state}/crucible-vault/install.applied` (one `<id>:<phase> <date> <commit>` line per applied phase). A recorded phase never runs again, so re-running `./install.sh` only rebuilds and restarts.

A migration is a bash file that defines one or both functions:

- `migrate_stopped` — runs after the editor has quit and `crucible-vault.service` is stopped (edit `state.db`, live config in `$CONFIG_DIR`, unit files).
- `migrate_running` — runs after the daemon is back up (`cv run …`, backfills).

Environment: `REPO`, `VAULT`, `CONFIG_DIR` (`$VAULT/_custom/crucible-vault`), `STATE_DB`, `BACKUP_DIR`, and the `backup <file>` helper (call it before editing any file). The function runs under `set -euo pipefail`; a non-zero exit stops the install with nothing recorded for that phase.

Rules:

- **Idempotent anyway** — check the target state first and no-op when it already holds (a phase can be re-run after a partial failure, and the user may have applied the change by hand).
- **Never assume the user's file is pristine** — the live `actions.toml` carries user edits; edit by anchored insertion/replacement and fail with a clear message when the anchor is gone, never overwrite wholesale.
- **Skip-and-log carve-outs are retired** — the per-migration carve-outs that were tied to 0006–0017 (skip and log, exit 0, when a user-edited view or note no longer matches the anchor; absent-checked append-only edits of generated hub notes) were folded with those migrations. The rules above stay: a new migration that must touch a user-editable file it does not own states its own skip-and-log carve-out here, never edits a changed file, and fails on every other gone anchor.
- **Numbering is append-only** — never renumber or edit an applied migration; ship a new one.
- Note content stays opaque: migrations touch `_custom/` config and daemon state, never personal notes directly (run an action through `cv run` instead).
- **Anchored edits use `lib/anchor.sh`** — a new migration that edits an `[[action]]` block of `actions.toml` sources `deploy/migrations/lib/anchor.sh` (`anchor_id_line`, `anchor_block_end`, `anchor_has_key`, `anchor_insert_after`, `anchor_replace`, `anchor_die`; hermetic test `lib/test-anchor.sh`) instead of writing its own grep/awk. The applied migrations are not retrofitted; their private copies carry two known drifts the lib resolves: (1) the block end is the next table header (`0031`) in some copies and the next `[[action]]` (`0024`, `0026`, `0027`) in others, so a `[[action.steps]]` header ends the block in one and not the other; the lib has one definition, the next `[[action]]` or EOF. (2) `0023` matches the action id without the full-line anchor the others use, so a prefix-sharing id can match; the lib's `anchor_id_line` is fully anchored.
- **A migration embeds or pins what it installs** — never read `config/examples/` at run time for text it writes into live config (the example moves on with later plans and the migration silently changes). Embed the block in the script (as `0026`'s heredocs do) or pin a copy under `fixtures/`; `0026`/`0027` still read their prompt file from `$REPO/config/examples/prompts/` (applied, not retrofitted), and their tests seed that source in scratch.
- **Tests share `lib/test-harness.sh`** — every `test-NNNN.sh` sources it for the `--help` handler, scratch dir + trap, `say`/`fail` and the real `backup` helper (`scripts/lib/backup.sh`, the same file `install.sh` sources); a test asserts the helper's real output (`    backup: $BACKUP_DIR/<name>`), never a stub. Fixtures are small synthetic live-shaped inputs seeded in the test; a test asserts anchored deltas (inserted lines, untouched remainder), not a whole-file compare with a copy of `config/examples/`.
