# Spike: stale `Clippings/` tab after File as Kind / auto-file (2026-10-23)

Scope: the round-11 FEEDBACK report that a clipping's tab stays open on its old `Clippings/` path after File as Kind, auto-file or the blog replacement path, with the notice arriving minutes late. Synthetic temp vaults only. The live vault and daemon were not touched. The throwaway tests are in this folder. To re-run one, copy it back into its crate: `cv-vault/tests/spike_stale.rs`, `cv/tests/spike_stale.rs`, or `editor/src/app/tests/spike_stale.rs` (the last also needs `mod spike_stale;`).

## Watcher (real writer + watcher): `cargo test -p cv-vault --test spike_stale -- --nocapture --test-threads=1`

| Scenario | Events |
|---|---|
| S1 plain move | `Renamed{from}` at +150 ms |
| S2 move, then two immediate destination rewrites | one `Renamed{from}` |
| S2b same, rewrite at +80 ms | one `Renamed{from}` |
| S3 source rewrite (`.webp` ref), then move | one `Renamed{from}` |
| S3b same, source rewrite seen first | `Modified`, then `Renamed{from}` |
| S6 busy source, move, destination rewritten for 6 s (past the 5 s echo TTL) | `Renamed{from}`, then `Modified` |
| S5 create and move in one 150 ms batch (cannot happen live) | `Created` only |
| S4 replacement (write the existing note, delete the clipping) | `Modified` (target), then `Deleted` (clipping); no `from` |

## Daemon end to end: `[SPIKE_SECS=100] cargo test -p cv --test spike_stale -- --nocapture --test-threads=1`

- Plain File as Kind: the job ends Done with `open_path`, then `Renamed{from}` + `Deleted` about 120 ms later. Clean.
- Auto-file chain (`clip-images` → localize → `image-describe-note` → `clip-auto-file` → `file-as-kind`): a clean pair at +350 ms.
- Every remote `.webp` unreachable: localize defers the whole `clip-images` job by 60 s (`crates/cv/src/steps/localize/mod.rs:259-265`). A manual File as Kind at +3 s moves the clipping cleanly. The retry at +60 s then fails with "localize: Clippings/… not found". Retries back off 30 s → 30 min, at most 5 attempts (`crates/cv/src/steps/mod.rs:187-192`), and auto-file is spawned only by `clip-images`' last step.

## Editor (FakeDaemon fed the observed sequences): `cargo test -p editor spike_ -- --nocapture`

- E1, the auto-file sequence: the tab follows.
- E2, the replacement sequence: the tab stays on the old path with "file deleted on disk".

## Verdicts

- H1 (the watcher fails to pair the move) is refuted. H4 (the editor mishandles `from`) is refuted.
- H2 is confirmed. `auto_file` trashes a clipping that was Replaced or already contained in its target (`crates/cv/src/steps/auto_file.rs:271-275`, called at `:411` and `:427`). The editor gets only `Deleted`. A fanfic chapter already present in its target takes the same branch (probable, not reproduced with a Works note).
- H3 is confirmed as the cause of the late notice. Network deferral holds filing, and a retry on a moved note fails, naming the old path.

## Fix shapes

1. The delete of a replaced clipping carries `replaced_by`, and the editor retargets the tab to the target (cv-protocol, bridge, auto_file, editor status/files). About 60–90 SLOC.
2. A retried job whose note is gone settles quietly as moved/gone instead of failing with an Error. About 30–60 SLOC.
3. If every remote image is unreachable, localize leaves the refs and the chain continues; the localize backfill retries later at the note's current path. About 20–40 SLOC.
