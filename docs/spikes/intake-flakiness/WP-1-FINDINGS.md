# Intake stage states on the live vault — first `cv intake pending` run

Measured 2026-11-27 on the live host with the release binary at `3a98e80b` (plan `intake-foundations` WP-1). One pass, live system, no repeat: all numbers are *indicative*. Aggregates only; no note text, titles or paths. The report is read-only and the live `state.db` is still at the schema before the stage-outcome table, so no outcome is recorded yet: every stage that the note itself does not show as done or skipped reads as pending.

## Command

```bash
CV_VAULT=<the daemon's vault root> ./target/release/cv intake pending [--days N]
```

`CV_VAULT` comes from the daemon's unit (`systemctl --user show crucible-vault.service -p Environment`). Without it the report reads the default vault and prints zeros; its first line now names the vault it read. Wall 2.7 s, peak RSS 43 MiB (`/usr/bin/time -f "wall=%es rss=%MKB"`, one run).

## Result, notes created in the last 30 days

1,012 intake notes: 923 blog, 56 mail, 31 YouTube, 2 clip.

| Stage | done | pending | skipped | unfinished | failed | stuck |
|---|---|---|---|---|---|---|
| auto_file | 31 | 2 | 0 | 0 | 0 | 0 |
| localize | 1012 | 0 | 0 | 0 | 0 | 0 |
| image_describe | 456 | 1 | 555 | 0 | 0 | 0 |
| yt_metadata | 30 | 1 | 0 | 0 | 0 | 0 |
| yt_clip_tidy | 30 | 1 | 0 | 0 | 0 | 0 |
| transcript_refine | 21 | 2 | 10 | 0 | 0 | 0 |
| enrich | 413 | 597 | 0 | 0 | 0 | 0 |
| finalize | 879 | 0 | 133 | 0 | 0 | 0 |
| index | 1010 | 2 | 0 | 0 | 0 | 0 |

## Enrich pending by window

| Window | Notes | Enrich done | Enrich pending |
|---|---|---|---|
| 3 days | 20 | 18 | 2 |
| 7 days | 82 | 78 | 4 |
| 14 days | 738 | 169 | 569 |
| 30 days | 1,012 | 413 | 597 |

Of the 597 pending, 596 are blog notes and 1 is YouTube (`--verbose` output counted by kind, paths discarded).

## Reading

- **The last week is close to converged:** 4 of 82 notes lack enrichment, and the non-model stages have one or two stragglers each. That is the size of the flakiness the diagnosis describes.
- **About 565 blog notes created 7 to 14 days ago have no enrichment.** That is a bulk arrival, not the event path dropping items one at a time. The report cannot say whether they were never eligible (a precondition the enrich step checks and nothing records) or never attempted; that distinction is exactly what recorded outcomes will add.
- **Consequence for `plans/intake-stage-jobs.md`:** with a 30-day window the convergence sweep would see about 600 pending model stages, four times the 150 at which that plan holds for the user. The window, the caps, or an eligibility rule for bulk arrivals has to be chosen before that plan's sweep package, and its audit should see these numbers.
- **The diagnosis counted 415 blog, mail and news notes for the same 30 days** by day folder; this report counts 979 blog and mail notes by their `created` date and intake keys. The populations differ (every note with a `post-id` is a blog note here), so the two figures are not comparable.

## A defect this run found

The first run selected 3,451 notes for 30 days against 450 for 7 days: the window used the index file change time, which a bulk rewrite about a week earlier had reset on old notes. Fixed in `3a98e80b` (the window uses the frontmatter `created` date and falls back to the change time).
