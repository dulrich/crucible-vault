# WP-0 findings — removal tiers and the Abstract block (2026-11-21)

Read-only measurements on the live vault and `state.db`; aggregates only (no note text, titles or paths). Run from the repo root at `e8ff9395`.

## 1. Why the Example News AM header survives the mail strip

- `perl docs/spikes/removal-tiers/head_lines.pl 'Newsbit' 8` — 49 `via: email` notes. Among the first 8 body lines, three lines recur once digits, month/weekday names and URLs are masked: two date-bearing lines (23 and 22 notes, **one exact variant per note**) and one heading (23 notes, 1 exact variant, position 2–5).
- `sqlite3 -readonly ~/.local/state/crucible-vault/state.db` on `mail_boilerplate` / `mail_boilerplate_seen` for `source_key = 'Example News AM'`: 49 messages seen, 1,305 learned lines, **14 at count >= 2**.
- Cause: `cv_ingest::boilerplate` keys a learned line by the FNV hash of its exact whitespace-normalised text (`normalize_line`, `line_key`). A header line that carries the issue date is a new key every issue, so it never reaches count 2. The unique date lines then sit in front of the one exactly-recurring heading as content lines and break the anchored head run (`MAX_GAP = 2`). Delete labels match by the same exact key, so the 12 `selection`/`chrome` Delete labels recorded for this publication never match a later issue either.
- `perl docs/spikes/removal-tiers/mask_sim.pl` — the mail strip (count >= 2, `MAX_GAP = 2`, half-body guard) simulated over the current bodies of all 120 `via: email` notes in the 13 publications with 2 or more notes, learned set approximated by those bodies:

| key | notes changed | non-blank lines removed | head runs | guard refusals |
|---|---|---|---|---|
| exact (today) | 12 | 49 | 6 | 1 |
| digits + month/weekday names masked | 55 | 212 | 50 | 1 |

  Example News AM alone goes from 2 notes / 6 lines to 40 notes / 152 lines (about 3.8 lines per note, all head runs). The other four publications that change gain 4 notes and 11 lines between them; eight publications are unchanged.

## 2. Removal labels

`removal_labels` grouped by verdict, origin and kind: 101 rows, **101 Delete, 0 Keep** (fold origin 73, selection origin 28; Example News AM 65 of them).

## 3. Confidence

`Span.confidence` is parsed (`crates/cv-ingest/src/enrich/model.rs:88`, `:214`, default 1.0) and read nowhere else; it is not stored in the fold metadata or in telemetry. Its distribution on real answers is unmeasured.

## 4. Reading does not collapse callouts

`crates/editor/src/reading/view/callout.rs` draws the `▸` marker for a `-` callout and then every child ("Folding is shown by the marker; the body stays readable (no fold state in v0)"). A `[!removed]-` fold is therefore always fully drawn.

## 5. The Abstract block

`perl docs/spikes/removal-tiers/entities.pl` — 359 notes carry a `[!abstract]-` block (median 18 lines, max 45); 349 list entities: 2,488 mentions, 1,542 distinct, **10 match an existing note basename**; 1,192 entities appear in one note only, 350 in two or more; 311 of 349 notes share at least one entity with another note.
