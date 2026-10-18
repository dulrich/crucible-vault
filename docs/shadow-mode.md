# Shadow mode: the soak runbook

The shadow daemon runs against the live vault **read-only** (D11): it indexes, runs actions and logs the writes it *would* make; real writes land only in an overlay under `CV_SCRATCH`. This runbook covers starting the soak, reading `cv shadow report` and `cv shadow parity`, and what stays a manual judgement. The parity verdicts gate cut-over readiness (`docs/cutover.md`).

## 1. Environment

Both directories must be outside the vault and outside every repo (`<code-root>`) — the daemon refuses a `CV_SCRATCH` inside either, and parity P6 checks both:

- `CV_SCRATCH` — the overlay root (default `~/.local/state/crucible-vault/scratch`); the overlay is `$CV_SCRATCH/vault/<vault-relative path>`.
- `CV_CONFIG_DIR` — the TOML config (default `~/.config/crucible-vault` in shadow mode). Copy what you want armed from `config/examples/`.

Set them in a drop-in (`systemctl --user edit crucible-vault-shadow`) rather than editing the unit file.

## 2. Install and start the unit

```bash
systemctl --user link <code-root>/crucible-vault/deploy/crucible-vault-shadow.service && systemctl --user start crucible-vault-shadow
```

Note the start time — it is the `--soak-start` for parity (`journalctl --user -u crucible-vault-shadow | head -1`, or `systemctl --user show crucible-vault-shadow -p ActiveEnterTimestamp`).

## 3. Import Crucible's image descriptions (once)

The search feed folds image descriptions into each note's `contentHash`, so P3 needs them. `state.db` has a single writer, so stop the daemon first:

```bash
systemctl --user stop crucible-vault-shadow
cv shadow import-descriptions <code-root>/obsidian-crucible/image-descriptions
systemctl --user start crucible-vault-shadow
```

It prints `{read, imported, skipped, errors}`; it refuses while a daemon answers on `CV_SOCKET`, and it only reads the source directory. Re-running is idempotent.

## 4. `cv shadow report [--since <rfc3339|ms>]`

Aggregates the `shadow_writes` log (read-only):

- `total` — would-be writes.
- `by_action` — per action id (`direct` = no job: URI intake and client writes; `unknown-job` = the job row was pruned).
- `by_op` — `create`, `modify`, `delete`, `rename_from`/`rename_to`, ….
- `by_path_class` — `<first segment>:<extension>`, where the segment is named only for `_custom`, `daily`, `core`, `attachments` (else `other`).
- `first_at_ms`, `last_at_ms`.
- `outside_overlay` — rows whose path is absolute, contains `..` or is otherwise invalid; must be 0 (P6).

No note path, title or content is ever printed.

## 5. `cv shadow parity [--soak-start <rfc3339|ms>] [--skip p1,p3,…]`

Prints `{generated_at, mode, criteria: {P1..P7: {status, metrics, notes}}}`. Statuses: `pass`, `fail`, `warn`, `unknown` (a probe or service was unavailable), `manual` (the user judges), `skipped`. It exits 0 whatever the verdicts; a non-zero exit is a harness error (e.g. `CV_SCRATCH` refused). It never writes the vault, `state.db` or the companion: the P1 rebuild goes into `$CV_SCRATCH/parity-<ts>/` and is deleted afterwards. Run it while the daemon runs (P1 PSS and P7 read the unit).

- **P1 Index** — `build_ms` (fresh full build), `peak_rss_kb` (this process's `VmHWM` after the build; `peak_rss_kb_before` is the value before it), `daemon_pss_kb` (`/proc/<MainPID>/smaps_rollup`), `md_files`/`links`/`unresolved`/`tags` with `drift_pct` against the S2 baselines (8,008 / 17,913 / 5,293). `fail` when build > 10 s, RSS > 64 MB or PSS > 50 MB; `warn` when any drift exceeds 2 % — **categorizing the cause is manual**.
- **P2 Lint/derived ids** — per field (`word_count`, `yt_video_id`, `post_id`) `{compared, agreed, rate}`, recomputed with `cv-lint` against what Crucible stored. Status `manual`: **you set the bar after the first run** (mismatches are expected: `lintOnSave` is off).
- **P4 Materialize** — overlay files written by materialize jobs whose live counterpart exists at the same path: frontmatter compared as parsed values (key order and YAML re-serialization ignored), body byte for byte. `{candidates, compared, equal}`; no candidates → `unknown`.
- **P5 Intake** — overlay files from URI intake (direct creates) and capture jobs vs the live file, byte for byte. To produce candidates, re-send the clipper URLs to the daemon (`cv uri '<obsidian://new… url>'`) after Obsidian handled them, then re-run parity.
- **P6 Safety** — `outside_overlay_rows` (0), `scratch_outside_vault_and_repos`, `config_dir_outside_vault_and_repos`, `landlock_denials` (journal lines with `permission denied` or a Landlock denial/failure; the startup `landlock` status line is not counted).
- **P7 Soak** — `soak_days` (from `--soak-start`, else the unit's active-since), `n_restarts`, `panics` (`panicked at` journal lines), `stuck_running_jobs` (running with a claim older than 1 h), `cycle_guard_trips`. `pass` needs ≥ 7 days, 0 restarts, 0 panics, 0 stuck; `warn` on cycle-guard trips — **explaining each trip is manual**.

## 6. `cv shadow replay <action-id> --corpus <dir> [--param k=v …] [--active <path>]`

Runs one configured action once against **your own scratch copy** of the vault, to review its effect. The corpus must exist outside `CV_PROTECTED_VAULT`, outside every repo, and must not be `CV_VAULT`. Replay starts an in-process daemon in **live** mode over the corpus with its own `state.db`, index and socket under `$CV_SCRATCH/replay-<ts>/` and an empty config dir, then arms only the named action with only a command starter (no event or schedule starter fires; `run_action` children are not armed). Actions with a `search_upsert`/`search_delete` step are refused (live mode would mutate the real companion). **Agent steps call the real providers** in `providers.toml`. Output: `{job_id, status, message, replay_dir, writes: [{op, path}]}` where `writes` is the corpus diff (create/modify/delete). Make the copy with e.g. `cp -a <vault> /tmp/vault-copy` (outside every repo). Replay always runs with an empty `CV_LINK_WRITE_ROOTS`, so a write through a symlink the copy preserved (e.g. an `initiatives/*/INITIATIVE.md` pointing into a fleet repo) is refused rather than landing in the real repo; links that resolve inside the corpus stay writable.

## 7. What stays manual

- P1 drift-cause categorization; P2's agreement bar; P3 review of `unexplained` (port defects) and `unclassified`; each P7 cycle-guard trip; the decision to cut over (`docs/cutover.md`).
