# Intake flakiness — diagnosis (WP-0, 2026-11-27)

Measured on the live host before plan `intake-convergence` was written. All numbers are *indicative* (one pass, live system, no repeat). Aggregates only; no note text, titles or paths.

## Method

- Frontmatter-only scan of `daily/day/2026-1[01]-*/` and `Clippings/`: the Perl script in the appendix, run as `perl scan.pl ~/vault > $TMPDIR/intake-scan.tsv` (937 rows; the TSV stays in `$TMPDIR`).
- Joined in `sqlite3 :memory:` against `~/.local/state/crucible-vault/state.db` opened `mode=ro` (`tracker_items`, `jobs`).
- `cv status`, `cv jobs stats [--since 7d]`, `cv jobs list --status failed|queued`, `journalctl --user -u crucible-vault.service --since 2026-11-20 | grep -iE 'breaker|unhealthy'`, `curl http://127.0.0.1:4811/v1/models`.
- Code facts: two read-only passes over `crates/cv/src/{steps,runner,daemon}`, `crates/cv-ingest`, `crates/cv-actions/src/sources`, `crates/cv-text/src/url_canon.rs` and the live `_custom/crucible-vault/*.toml`; the anchors below were re-read by the orchestrator.
- History: every intake defect recorded in `plans/` and `docs/` (71 rows), classified by mechanism.

## The four `FEEDBACK.md` incidents

| Incident | What happened |
|---|---|
| YouTube clip, 2026-11-27: "no auto-refine fired" | Filed in under 1 s. Its `clip-enrich` job is `queued … deferred: service local unhealthy` (attempt 2). `127.0.0.1:4811` refuses connections. Step 2 of `clip-enrich` is `image_describe`, the only step that needs `local`; the deferral parks the whole job, so `yt_metadata` (network), `transcript_refine` and `intake_enrich` (router) wait behind it. Nothing in the note or the editor says a stage is pending. |
| YouTube clip, 2026-11-22: "did not complete intake" | It did complete: `clip-enrich` ran 15:12:43–15:14:36. The note was clipped again at 15:14:18, 95 s in, before the first run settled. The run had no visible progress. |
| One post, two notes on two hosts (2026-11-23) | Mail copy: `pub-b.substack.com`. RSS copy: `www.pub-b.co` (the item link host of a feed served from the Substack host). The publication's host list is built from `feeds` + `hosts` only, so the custom domain is not in it; the post-id is origin + path; neither identity keys nor reconcile phase E can match. Aliasing is built (`hosts`), not configured, and nothing learns it. |
| Create Source from a fanfiction.net note | Proposed prefix is the chapter URL, proposed name is the base URL. Not re-measured; code path `propose_source`. |

## Prevalence, notes created since 2026-10-28

| Measure | Count |
|---|---|
| YouTube notes | 36 |
| … without `refined` | 14 (2 of them at 300 words or more; the rest are short-transcript skips or older than the refine rule — indistinguishable from a failure in the data) |
| … without `enriched` / metadata link / `publication` | 1 each (the parked clip) |
| Blog, mail and news day notes | 415 |
| … without `enriched` | 24 (no record says skipped, failed or never fired) |
| Fanfic clips without `enriched` | 30 (by design, also unrecorded) |
| Same-day, same-title note pairs on different source hosts | 9 groups, 11 pairs; 6 pairs have a mail side and a Substack host |
| Notes with `complete: false` | 32 (25 `paid`, 3 `founding`, 4 `unknown`) |

## Service `local`

- Breaker closed 15 times between 2026-11-24 and 2026-11-27 (log lines `breaker closed; requeued service failures`), requeueing 0–5 jobs each time.
- 7-day blockers: `search-refresh` 4 h 57 m behind the `local` breaker and 3 h 20 m deferred; `clip-enrich` 2 m 53 s behind it. One `search-refresh` failed "service local still failing after 5 attempts".
- All-time `intake-enrich` and `clip-enrich` p95 total is 129 h and 152 h, all in the unattributed residual; the 7-day p95 is 43 s and 2 m 11 s. The long tail is historical (backfills and the router-secret outage of round 23).

## Structure (verified in source)

1. **The chain fires once.** Every link from clip to enrich is an event starter or a `run_action` spawn. There is no startup replay (`crates/cv/src/runner/events.rs:8`). `drain_spool` runs before the event starter subscribes (`crates/cv/src/daemon/mod.rs:270` against `:297`), so a clip spooled while the daemon was down — every `install.sh` restart — is written and never filed or enriched. Nothing sweeps unfiled `Clippings/`.
2. **Completion is implicit.** No per-item record of stages exists. Done is inferred from frontmatter stamps (`enriched`, `refined`, the metadata link, `publication`); a skip, a failure and "never fired" look the same. Every repair path (`intake-reconcile`, `intake-enrich-backfill`, `yt-clip-tidy-backfill`, `intake-frontmatter-backfill`) is manual; a YouTube clip missing its metadata has none.
3. **Gating is per job.** No step declares a service; a deferral parks the job and resumes at that step (`crates/cv/src/runner/execute.rs:269-281`). One `local`-only step at position 2 blocks five steps that do not need `local`.
4. **Identity is derived per site.** Three post-id option sets (lint default, blog-feed rule, dupes-scan with config host rules) and two URL rules (supersede identity keys against exact `tracker_items.url` in `intake_finalize`). Mail's fallback host is the sender's e-mail domain.
5. **Not a cause:** YouTube clips link to their tracker row by video id, never by title; a YouTube row's `outcome` stays `line` by design, and `cv intake list` hides `note_path` and `status`, so the listing cannot show linkage.

## History, 71 recorded intake defects

| Mechanism | Rows |
|---|---|
| A stage never fired, or fired at the wrong time | 19 |
| Note-body cleaning | 12 |
| Missing or wrong config | 11 |
| Item not matched (identity) | 10 |
| Existing notes outside a backfill | 9 |
| Service | 4 |
| Other | 6 |

Twelve symptoms were fixed more than once. About 20 distinct backfill, reconcile, repair, recover or rescan actions and migrations were added to patch items already ingested. Trigger, backfill-gap and identity rows together are 38 of 71: each is one instance of structure items 1, 2 and 4.

## Appendix — `scan.pl`

```perl
#!/usr/bin/perl
# Frontmatter-only scan of the vault's intake notes -> TSV on stdout (kept in $TMPDIR, never committed).
# Columns: day, dir, vid, postid_host, source_host, msgid(0/1), refined, enriched, ytmeta, rawtr, publication(0/1),
#          complete, access, via, kindtags, wc, titlekey(md5-ish crc of normalised title), transcript_of(0/1)
use strict; use warnings;
my $root = shift or die "usage: scan.pl <vault>\n";
sub host { my $u = shift // ''; $u =~ s/^["']|["']$//g; return $u =~ m{^https?://([^/]+)} ? lc $1 : ''; }
sub crc { my $s = lc shift; $s =~ s/[^a-z0-9]+//g; my $h = 5381; $h = (($h * 33) ^ ord) & 0xffffffff for split //, $s; return sprintf '%08x', $h; }
for my $f (glob("'$root'/daily/day/2026-1[01]-*/*.md"), glob("'$root'/Clippings/*.md")) {
  open my $fh, '<', $f or next; my $l = <$fh>; next unless defined $l && $l =~ /^---/;
  my (%k, $tags, $cur); $tags = '';
  while (<$fh>) { last if /^---/; if (/^([A-Za-z-]+):\s*(.*)$/) { $cur = $1; $k{$1} = $2; } elsif ($cur && $cur eq 'tags' && /^\s+-\s*"?([^"\s]+)/) { $tags .= "$1,"; } }
  my ($day) = $f =~ m{/day/(\d{4}-\d\d-\d\d)/}; $day //= 'clip';
  next unless $k{source} || $k{'yt-video-id'} || $k{'post-id'} || $k{'message-id'};
  print join("\t", $day, ($day eq 'clip' ? 'clip' : 'day'), $k{'yt-video-id'} // '', host($k{'post-id'}), host($k{source}),
    ($k{'message-id'} ? 1 : 0), ($tags =~ /\brefined\b/ ? 1 : 0), ($k{enriched} ? 1 : 0), (exists $k{'yt-metadata'} ? 1 : 0),
    ($k{'raw-transcript'} ? 1 : 0), ($k{publication} ? 1 : 0), $k{complete} // '', $k{access} // '', $k{via} // '', $tags,
    $k{'word-count'} // '', crc($k{title} // ''), ($k{'transcript-of'} ? 1 : 0)), "\n";
}
```
