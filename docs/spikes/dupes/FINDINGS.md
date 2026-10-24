# Duplicate-note detection spike (round-11 WP-8)

Aggregates only. Live vault read-only; search.db copied to a temp dir (`T=/tmp/dupes-XXXX`, `cp ~/.local/state/crucible-vault/search.db* $T/`) and opened `SQLITE_OPEN_READ_ONLY`. Driver: this folder's `src/main.rs` (own `[workspace]`, own target dir; deps `cv-text` path + `rusqlite =0.40.2`; `cp ../../../Cargo.lock .`).
Build: `CARGO_TARGET_DIR=$PWD/target nice -n 10 cargo build --release --offline -j 4`. V=~/vault.

## Corpus
- 9077 `.md` files with >= 20 shingles (533 shorter excluded), `.obsidian/.git` skipped; `.trash/` holds 4 top entries and 14 md (stays in the shingle universe, flagged). search.db: 8752 sources, 94466 chunks, 94466 vectors, dim 1024 `bge-m3-f16`, 4096-byte f32 blobs (`sqlite3 -readonly search.db "select length(embedding),count(*) from vectors group by 1"`). `.trash` is not indexed in search.db.

## (a) Shingles (cv-text normalize + k=4 FNV shingles)
Cmd: `nice -n 10 /usr/bin/time -v target/release/dupes-spike shingles $V $T/a.tsv`
- Wall 25.2 s total (load 4.2, inverted-index candidates 3.6, MinHash LSH 2.4, exact verify of 457k candidates 13.1), peak RSS 1.17 GiB (all shingle sets resident).
- Candidate generation: (i) inverted index over shingles, shingles with doc-freq > 30 skipped, pair needs >= 3 shared: 452,315 candidates; (ii) MinHash 63 hashes, 21 bands x 3 rows, buckets > 50 skipped: 9,914 candidates. Union exactly verified with sorted-set intersection. Cross-check: 200 verified pairs re-scored through `cv_text::similar::containment` on the raw bodies, max abs diff 0.0000.
- Pair counts (containment = |small ∩ large| / |small|): >=0.5: 11,364 (3,972 distinct notes); >=0.7: 8,726 (3,368 notes); >=0.9: 2,785 (2,393 notes). Jaccard >=0.5/0.7/0.9: 6,646 / 3,561 / 698.
- Recall of each generator vs the union: LSH finds 8,930/11,364 (>=0.5), 7,478/8,726 (>=0.7), 2,217/2,785 (>=0.9); the df-capped inverted index finds 6,642 / 4,299 / 2,011. Both are lossy and in different ways: the df cap drops pairs inside big clusters of near-identical notes (>=0.9 pairs: max degree 55, 107 notes with degree >= 10), while LSH misses small-in-large pairs whose Jaccard is low. A real finder needs uncapped candidate generation (or a cluster-aware cap) plus a size-aware band.

## (b) Semantic (mean of chunk vectors per source, re-normalized, cosine)
Cmd: `nice -n 10 /usr/bin/time -v target/release/dupes-spike semantic $T/search.db $T/b.tsv`
- Wall 11.6 s (load + mean 5.1, exhaustive 38.3M-pair scan on 4 threads 5.5), peak RSS 80 MiB (streamed; 36 MiB of means).
- Pairs: cos >=0.85: 200,057; >=0.90: 38,243; >=0.95: 13,129 (2,585 sources have a neighbour); >=0.98: 3,038.

## Overlap (Cmd: `dupes-spike overlap $V $T/a.tsv $T/b.tsv`; raw output `out/overlap.txt`)
Comparable thresholds (A = containment, B = cosine): 0.5 vs 0.90: both 1,834 (A 11,364, B 38,279); 0.7 vs 0.95: both 902 (A 8,726, B 13,147); 0.9 vs 0.98: both 467 (A 2,785, B 3,048). Set Jaccard 0.04 / 0.04 / 0.09. The two signals are nearly disjoint, not two views of one set. 7,550 of A's 11,364 pairs have both notes in some cos>=0.85 pair, yet only a minority of those pairs themselves score high: mean-pooled cosine does not track verbatim containment (long notes diluted, topic-similar short notes inflated).

## Proxy signals (NOT precision; precision is UNMEASURED, there are no labels)
These are agreement rates with weak correlates of "real duplicate" and must not be read as precision.
- `duplicate-of`: ZERO notes in the vault carry the key (`grep -rlE "^(duplicate-of|duplicate_of)" --include=*.md $V` = 0; the driver's linked/either columns are 0 for every set). So "already linked" cannot suppress anything today and gives no signal.
- `.trash`: one side in trash for 1 / 2 / 31 pairs in A bands 0.5-0.7 / 0.7-0.9 / >=0.9 (and 0 in every B band, since trash is unindexed). Negligible.
- Same `source` host (of pairs where both notes have a `source` URL): A BOTH (cont>=0.7 & cos>=0.95, 902 pairs): 93.6% same host (762 have hosts); A-only >=0.7 (7,824 pairs): 42.3% (759 have hosts); B-only >=0.95 (12,245): 99.1% (1,029 have hosts). Most pairs have no `source` at all.
- Same directory / same top folder: A 0.5-0.7: 61.6% / 81.2%; A 0.7-0.9: 73.3% / 88.6%; A >=0.9: 22.8% / 58.4%; B 0.90-0.95: 45.6% / 78.8%; B 0.95-0.98: 89.2% / 93.4%; B >=0.98: 80.6% / 83.6%; BOTH: 14.7% / 18.5%.
- Same filename stem: A >=0.9 26.1%; BOTH 18.1%; elsewhere 1-12%.
- Where the volume sits (top folder pairs; folder names are system folders, not note paths): A 0.5-0.9 is dominated by `_yt_metadata x _yt_metadata` (6,286 of 8,579 pairs), then `initiatives`; A >=0.9 is `_blog_metadata x daily` (839), `_yt_metadata` pairs (675), `initiatives` (669). B is dominated by `daily x daily` (15,091 of 25,132 at 0.90-0.95; 9,136 of 10,099 at 0.95-0.98; 2,069 of 3,048 at >=0.98). Generated/templated folders (yt/blog metadata, daily, initiatives) are the bulk of both candidate sets: structural similarity, not copied content.
- Reading: the intersection set (BOTH) is the only one with high same-host agreement AND cross-folder spread (`_blog_metadata x daily` 553 of 902: article content embedded in a daily), the shape a human would call "the same text filed twice". The large single-signal sets are mostly template artefacts (`daily x daily` B, `_yt_metadata` A).

## Recommended finder shape (for a follow-up plan; unlabelled, so thresholds are provisional)
- Primary signal: shingle containment >= 0.9 (2,785 pairs) intersected with / ranked above cosine >= 0.95; report the BOTH set (902 at 0.7/0.95) as the high-confidence queue, and containment >= 0.9 alone as the second tier. Do not use cosine alone (38k-200k pairs at 0.90-0.85, mostly `daily x daily`).
- Exclude by folder/kind before pairing: generated folders (`_yt_metadata`, `_blog_metadata`, `_raw_transcript`, `daily`, `initiatives`, `_crucible`) against each other; keep cross-kind pairs (e.g. `_blog_metadata x daily`). Needs a user decision on which folders are "templated"; a label sample is the prerequisite to claim any precision.
- Expected volume: a few hundred to ~3k pairs for the first full run; cluster degree up to 55, so present clusters (connected components), not pairs.
- Cost of a full run: shingles ~25 s and ~1.2 GiB RSS at 9k notes (RSS can be cut by sketching/streaming and by hashing to u32); semantic ~12 s and ~80 MiB with an exhaustive scan (O(n^2): fine to ~50k sources, then add ANN). Both are fine as an on-demand daemon job, not a hot path.
- semsearch: a public vector accessor IS needed if the finder lives in the daemon (today only `Index::nearest_sources` with no callers and no way to read stored chunk vectors; this spike read the `vectors`/`chunks` tables directly from a copy). Minimal ask: `Index::source_vectors()` (mean-pooled normalized vector per source) or an iterator over `(source, embedding)`. Mean pooling is crude; a max-over-chunk-pair cosine would better catch embedded-article cases and costs ~94k^2/2 ops unless blocked by source.
- Limits of this spike: df cap 30 and LSH bucket cap 50 make candidate generation lossy (see recall above); 533 notes below 20 shingles are excluded; thresholds are not tuned; `duplicate-of` is absent so the already-linked suppression is untested.
