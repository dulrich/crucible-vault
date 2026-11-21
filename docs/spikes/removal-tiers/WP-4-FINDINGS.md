# WP-4 findings — date-masked boilerplate keys, scratch-vault acceptance

Plan `abstract-removal-confidence-deletion` WP-4. Aggregates only; no note text, titles or paths.

## Setup (2026-11-21)

- Starting state, copied once into `$TMPDIR/wp4acc/base/` (`$TMPDIR` = `/dev/shm`, outside every repo):
  - vault: `cp -rL --preserve=timestamps,mode ~/vault base/vault` (symlinks dereferenced, so no write can reach a live file through a link; 0 symlinks in the copy, 2.5 GiB);
  - `state.db`: `sqlite3 -readonly ~/.local/state/crucible-vault/state.db ".backup base/state/state.db"` (v20; 174 `removal_labels`, 51196 `mail_boilerplate`, 1014 `mail_boilerplate_seen` rows); `search.db` and the index were not copied (the scratch daemon rebuilds them);
  - maildir: `cp -a ~/Downloads/processed base/maildir/` (copied rather than proven read-only: the reconcile's mail re-match phase is not read-only by inspection);
  - scratch config in `base/vault/_custom/crucible-vault/`: `actions.toml` reduced to the one `intake-reconcile` action (no event or schedule starters), `schedules.toml` emptied, `pipelines.toml` `[mail] maildir` pointed at the scratch maildir and `watch = false`.
- Each run: a fresh `cp -a base run`, then a scratch daemon `unshare -rn <cv> daemon` (no network namespace: every provider/probe call fails locally) with `HOME`, every `XDG_*`, `CV_VAULT`, `CV_STATE_DB`, `CV_SEARCH_DB`, `CV_INDEX_DB` and `CV_SOCKET` under `run/`, then `<cv> intake reconcile`, then the daemon is killed. The live daemon, live databases, vault and maildir were never opened for writing.
- Exact key: `cv` built from `git archive 1abb0b21` into `CARGO_TARGET_DIR=$TMPDIR/wp4acc/old-target` (`cargo build --release --locked -p cv`). Masked key: this worktree's `target/release/cv` (state.db migrates to v21 at start, clearing the learned sets; the reconcile re-seeds them).
- Measurement: a perl script comparing `run/vault` with `base/vault` per note (frontmatter and `%% cv-enrich %%` blocks excluded): for `via: email` notes, non-blank body lines removed (multiset difference), split Example News AM / every other publication; for Example News AM notes, whether the first non-blank body line is gone. Label keys: `SELECT DISTINCT publication, line FROM removal_labels`, masked as `boilerplate::mask` does, grouped per publication.

## Results

| measure | exact key (`1abb0b21`) | masked key (WP-4) | acceptance |
| --- | ---: | ---: | --- |
| Example News AM notes | 49 | 49 | |
| Example News AM notes losing their header (first body line) | 2 | **40** | >= 35 of 49: **pass** |
| Example News AM body lines removed | 6 | 152 | |
| other mail publications: body lines removed | 28 | 39 | at most +20: **+11, pass** |
| mail notes changed | 7 | 49 | |
| `bp_stripped` / `bp_lines_stripped` | 5 / 27 | 47 / 159 | |
| `bp_notes_deleted` / `bp_lines_deleted` (Delete labels) | 1 / 2 | 21 / 23 | |
| `bp_guarded` (half-body guard) | 42 | 44 | |
| `bp_learned` / `bp_rss_learned` (re-seed after v21) | 0 / 2 | 139 / 877 | |
| RSS: `bp_rss_folded` notes / `bp_lines_folded` lines | 2 / 7 | 5 / 19 | change **+12 folded lines** |
| `[!removed]` callout headers in changed notes | 3 → 3 | 3 → 3 | the extra RSS lines join existing folds' counts as reported by the step |

- Label keys: 168 distinct (publication, line) label lines map to 166 masked keys; **2 masked keys now match more than one distinct labelled line**.
- Second reconcile on the settled masked run (same tree, `run.sh ... keep`): `bp_learned=0 bp_stripped=0 bp_lines_stripped=0 bp_lines_deleted=0 bp_rss_learned=0 bp_rss_folded=0`, and 0 `.md` files newer than the first run's output: **writes nothing**.
- `diff -rq base/vault run/vault | grep -c '\.md differ'`: 119 notes changed (exact) and 163 (masked), so about 110 notes in each run changed through other reconcile phases (publication backfill and repairs; 110 exact, 109 masked); those are not boilerplate and are not counted above (only `via: email` body lines and RSS folds are).

The scratch tree (`$TMPDIR/wp4acc/`, vault copies, databases, maildir copy, old build) was deleted after measuring.
