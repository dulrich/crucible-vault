# Spike: intake-backfill feed coverage (2026-10-17)

Question: of blog `tracker_items` rows with a body (`outcome` line/note, `full_body=1`), how many are still present in their feed, so the blog tracker's own fetch can backfill `word_count`?

Command: `python3 -I docs/spikes/intake-backfill/feed_coverage.py ~/.local/state/crucible-vault/state.db` (read-only; prints aggregates only)

Result: `feeds=14 feed_fetch_failed=0 present={'note': 79, 'line': 3} absent={}` — all 82 present.
