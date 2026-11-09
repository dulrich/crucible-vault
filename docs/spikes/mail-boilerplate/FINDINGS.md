# Spike: per-sender mail boilerplate (2026-10-22, plan round-10-feedback-todos)

Read-only over the live vault's `via: email` notes; aggregates only. Grouping key = frontmatter `publication`.

- `perl measure.pl $VAULT` → 39 email notes, 17 senders, 10 with ≥2 notes; 2,796 non-blank body lines, 655 (23.4%) repeat within their sender.
- `perl position.pl …` → repeated-line position by body decile (top→bottom): 68 61 52 49 55 53 55 57 89 116 (tail-heavy). Max pairwise line-set Jaccard per sender: nine ≤ 0.18, one 0.92 (near-duplicate captures, not boilerplate — inflates the 23%).
- `perl anchored.pl …` (excludes senders with any pair Jaccard > 0.5; repeated = line in ≥2 of the sender's notes and > 3 chars; head/tail run tolerates ≤ 2 non-repeated gap lines) → 27 notes, 1,478 lines; anchored strip removes head 20 + tail 144 = 11.1% of lines, touches 21/27 notes, max 38% of one note; 94 repeated lines stay mid-body (not stripped: recurring section headings / mid-body slots).

Conclusions for the plan: anchored head/tail stripping is viable and mostly a tail (footer) effect; only senders with ≥ 2 messages benefit; the learned line set must not be derived from already-stripped note bodies (stripped notes would stop teaching it), so it is learned from cleaned message bodies before the strip and persisted per sender; a max-strip guard is needed (observed 38%).
