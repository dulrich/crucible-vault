# Data migrations

`./install.sh` applies the data migrations found in `CV_MIGRATIONS_DIR` (default `deploy/migrations/local/`; it may be any directory, including one inside the vault, such as `$VAULT/_custom/migrations/`). A migration is a bash file named `NNNN-<slug>.sh`; the runner applies each one once, in name order, and records it in the ledger `${XDG_STATE_HOME:-~/.local/state}/crucible-vault/install.applied` (one `<id>:<phase> <date> <commit>` line per applied phase, where the id is the file name without `.sh`). A recorded phase never runs again, so re-running `./install.sh` only rebuilds and restarts. Ledger lines for ids with no file are ignored.

The runner sources every migration file, applied ones included, to discover which phases it defines, so a migration file must source cleanly and define only functions and constants at the top level.

A migration defines one or both functions:

- `migrate_stopped` runs after the editor has quit and `crucible-vault.service` is stopped (edit `state.db`, config in `$CONFIG_DIR`, unit files).
- `migrate_running` runs after the daemon is back up (`cv run …`, backfills).

Environment: `REPO`, `VAULT`, `CONFIG_DIR` (`$VAULT/_custom/crucible-vault`), `STATE_DB`, `BACKUP_DIR`, `CV_MIGRATIONS_DIR`, `CV_MIGRATIONS_LIB` (the directory holding `anchor.sh`, so a migration anywhere sources `"$CV_MIGRATIONS_LIB/anchor.sh"`), and the `backup <file>` helper (call it before editing any file). The function runs under `set -euo pipefail`; a non-zero exit stops the install with nothing recorded for that phase.

## Rules

- **Idempotent.** Check the target state first and no-op when it already holds: a phase can be re-run after a partial failure, and the user may have made the change by hand. A second run is byte-identical.
- **Never assume the user's file is pristine.** Edit by anchored insertion or replacement; fail with a clear message when the anchor is gone or a key holds a different value. Never overwrite a file wholesale or a user's edit.
- **Plan before writing.** Compute every edit on temp copies and write only when all of them succeed, so a failure leaves the config untouched.
- **Anchored `[[action]]` edits use `lib/anchor.sh`.** It provides `anchor_id_line`, `anchor_block_end`, `anchor_has_key`, `anchor_insert_after`, `anchor_replace` and `anchor_die` (hermetic test `lib/test-anchor.sh`). The block end is the next `[[action]]` header or EOF, and the id match is fully anchored.
- **TOML array adds use `anchor_array_add`** (in `lib/anchor.sh`): `anchor_array_add <file> <table> <key> <value>` adds a value to a single-line `[<table>]` array such as `[kinds.fanfic]` `hosts`; exit 0 = added, 2 = already listed (file untouched), 1 = anchor gone (no table, no key, not a single-line array; file untouched). Tested against the 0036 and 0039 hosts edits in `lib/test-anchor.sh`.
- **View-definition edits use `lib/view-def.sh`** (source it after `backup` is defined): `viewdef_add_formula <file> <after-key> <line>` inserts a formula after an anchor key in `[formulas]`; `viewdef_order_insert <file> <view> <after> <new>` inserts a column after another in a view's first `order = [` line; `viewdef_add_props <file> <key> <label>` appends a `[properties."<key>"]` display-name block at EOF; each skips when already applied and returns 1 when its anchor is gone. `viewdef_apply <file> <edit-fn> [args...]` is the skeleton: it runs the edit function on a temp copy and, only when something changed, takes the backup and replaces the file; a failed edit leaves the file untouched and returns 1 (a migration that wants the skip-and-log carve-out turns that into a logged exit 0). Tested by replaying the 0035 and 0040 fixtures against the real migrations in `lib/test-view-def.sh`.
- **Append-only numbering.** Never renumber a migration or reuse an id; the ledger key is the full id. Never edit an applied migration; ship a new one. The single exception is below.
- **A migration embeds or pins what it installs.** Never read `config/examples/` at run time for text it writes into live config, because the example moves on and the migration would silently change. Embed the block in the script, or pin a copy under a `fixtures/` directory next to it.
- **Note content stays opaque.** Migrations touch config and daemon state, never personal notes directly (run an action through `cv run`).
- **Bash and coreutils** (awk and perl-base where needed). No extra interpreter runtime.

## Tests

Every migration ships a `test-NNNN.sh` beside it that sources `lib/test-harness.sh`: the `--help` handler, a scratch dir with an exit trap, `say`/`fail`, `here`, `repo`, and the real `backup` helper from `scripts/lib/backup.sh` (a test asserts its real output, `    backup: $BACKUP_DIR/<name>`, never a stub). Fixtures are small synthetic inputs seeded in the test; assert anchored deltas (inserted lines, untouched remainder), not a whole-file compare with a copy of `config/examples/`. A test covers the first run, a byte-identical second run, and a changed anchor failing non-zero.

## One-time relocation exception

The runner and the migrations directory moved when the data migrations were split from the public tree. That move forced mechanical edits to already-applied migrations and their tests, and this is the single exception to "never edit an applied migration". It covers only location changes: `source` paths, test-harness and build-id imports, and caller-relative repo discovery (`lib/test-harness.sh`, a test's own `lib/` and `scripts/` paths, an `anchor.sh` source line that now honours `CV_MIGRATIONS_LIB`). Migration bodies and ledger ids are unchanged. The exception is spent; later relocations need a new decision here.
