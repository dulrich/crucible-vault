# Memory follow-ups — findings (2026-11-11, two pre-plan spikes: the editor and the daemon)

Aggregates only. No note text, titles or paths of personal notes were logged or are quoted here; image-cache keys were FNV hashes, and reply bodies were compared by hash and length. The view ids that appear come from vault `_custom/` configuration, which is reviewable.

Follows `docs/spikes/memory-release-strategy/FINDINGS.md` (the daemon's release triggers, shipped as 1.0.1). That work closed the daemon's release gap; these two spikes measured what is left, one in the editor and one in the daemon.

Prototype patches beside this document (all throwaway; none of them is production code):

- `editor.patch` prototypes the editor changes behind environment knobs: releasing the images of hidden tabs (two variants), dropping or skipping Reading render models for hidden tabs, a cap on concurrent image decodes and a `thumbnail` variant, plus an in-crate test that models path-string sharing. It is 9 files, +389/−3, against `38f52df8`. The probes that produced the `CVME` lines are a separate instrumentation patch that is not kept here.
- `daemon.patch` prototypes the daemon changes: direct decode of view property values, a cache kept across view runs, and opening the search index under `block_in_place`. It carries environment knobs (`CVP_DIRECT`, `CVP_REUSE`, `CVP_OPEN_BLOCKING`, `CVP_SEMS`) that exist only for the measurement. It is 8 files, 661 lines, and touches `crates/cv/src/search_query.rs` and, in `crates/views/src/`, `db.rs`, `lib.rs`, `proto.rs` (new), `service.rs`, `tests.rs`, `tests_reuse.rs` (new) and `value.rs`. It does not pass clippy as is: `from_json_text` and `struct Direct` sit after `mod tests` in `value.rs` (`items after a test module`).
- `semsearch.patch` prototypes the `semsearch` change: computing the sign bits straight from the stored blob in `load_vectors`, with no `Vec<f32>` per row. It is 32 lines in one file, `crates/semsearch/src/store.rs`, made as a read-only `diff -ruN` against the sibling `semsearch` checkout; it is throwaway and the sibling repo was not edited.

## Host and builds

Both spikes ran on a **shared developer workstation** (24 cores, other sessions running). Conclusions therefore rest on memory figures, which are not sensitive to scheduling; **every latency and CPU figure below carries its contention label** and counts as indicative, not as a pass criterion. Contention was recorded per block as the cpu pressure-stall `some avg10` (psi) and the number of running `rustc` processes.

- **Editor spike:** two scratch worktrees at `38f52df8` (a reference tree with instrumentation only, a prototype tree with the same instrumentation plus the prototypes), release `gpu` builds, each with its own target directory. The main checkout moved to `25f32021` during the run; the figures are from `38f52df8`.
- **Daemon spike:** worktrees detached at `41a44e6d` (the main checkout moved to `bb04ae09` during the run): `ref` (plain HEAD), `proto` (HEAD plus instrumentation plus prototypes) and `plain` (HEAD plus prototypes only, no instrumentation). All figures marked "instrumented" come from `proto`, all "headline" figures from `ref` and `plain`.
- Both spikes used a scratch vault copy (symlinks removed, no `.git`), never the live vault. The editor spike's copy held 16,530 indexed files.

## Summary

Editor:

- **Tabs that are not visible keep their images pinned, and nothing bounds it.** About 8.3 MiB per image-heavy tab: 124 MiB of pinned images at 15 tabs and 214 MiB at 25. `CV_IMAGE_CACHE_MB` bounds nothing today because pinned entries are never evicted.
- **Releasing a hidden tab's images and dropping the entries at once saves 186 MiB of RSS at 15 tabs and about 280 MiB at 25.** The cost is about 3.6 re-decodes of about 24 ms per re-shown tab, off the UI thread. Re-decoding raises the cycling high-water mark by about 34 MiB, which is the decode transient below.
- **Opening image-heavy notes decodes every image at once at full resolution.** The load peak sits about 123 MiB over settled with 15 notes; capping concurrent decodes at 2 brings that to about 88 MiB and at 1 to about 64 MiB, with no time cost measurable on this host. `thumbnail` brings it to about 51 MiB.
- Smaller or not worth it: Reading render models for hidden tabs (5–8 MiB), path-string sharing (5.7 MiB modelled; the larger share is inside the shared resolver), the decoded-dimension cache (about 6 MiB), the view store, and reply decode in the RPC client (under 1 MiB).

Daemon:

- **The first search opens the `semsearch` index on a tokio worker.** It allocates 427.9 MiB and takes about 0.42 s, because `load_vectors` builds a `Vec<f32>` per stored vector (95,019 of them). Computing the sign bits straight from the blob cuts the allocation to 45.2 MiB; time and the resident index (about 22–25 MiB) do not change. A keyword query itself allocates only about 0.2 MiB.
- **Decoding view property values straight into `views::Value` saves 17.8 MiB of allocation per pass** (237.7 → 219.9 MiB on the view thread), a 7.5 % cut. Peak and settled memory do not move.
- **Keeping the view caches across runs is rejected.** It cuts view churn by 75 % and view latency about 5×, but holds about 42 MiB more at rest: settled goes from about 60 to about 107 MiB and the peak rises by about 40 MiB.
- Nothing to gain: notifications (0.92 MiB per pass) and the `cv-view` arena (about 8 MiB resident, about 150 KiB live).
- View results are built as a `serde_json::Value` tree on the view thread (`into_protocol`, 21.6 MiB per pass); that is the residue once caches are warm, and removing it needs a protocol change. It was not prototyped.

## Method: editor spike

- **Harness** (scratch directory, prunable): `daemon.sh` starts and stops a scratch daemon; `session.sh <label> <ref|proto> <S1|S2|S3|O|V> [n] [ENV=..]` runs one session and writes a `.rows` file of phase rows and a `.log`; `matrix.sh <spec> 3` runs a spec three times, interleaved (specs A: hidden-tab release, B: decode cap, C: Reading models and views, D: the re-run of the fixed drop variant); `analyze.pl` summarises a run log. Every number below comes from `perl analyze.pl out/<run>.log` or from the phase rows in `out/<run>.rows`.
- **Isolation:** a scratch daemon on `<scratch>/s/run/d.sock` with scratch `CV_*_DB`, `XDG_*` and an empty `editor.json`; the vault copy was made with `rsync --no-links` without `.git`, `.obsidian` or `.trash`; `schedules.toml` emptied, every pipeline disabled, `maildir=""`, every `base_url` set to `127.0.0.1:9`. The user's own daemon and editor were never touched. The editor ran on X11: on Wayland the unfocused window never drew a frame, so no sensors fired and no images loaded.
- **Scenarios:** S1 opens the top 15 notes by image-embed count 3 s apart (`cv uri`) and re-shows them in cycles; S2 does the same with 25; S3 creates 20 notes 2 s apart (the trigger for the whole-tab re-lower); O opens notes for the decode peak; V runs the view notes. Editing a note that is not open does not move the resolver epoch or the index generation (20 appends gave no re-lowering ticks), so only create, delete and rename were used as triggers.
- **Instrumentation:** `CVME <ms> <kind>` lines on stderr when `CV_ME_PROBE` is set. Fields: image cache entries, bytes and pinned bytes; decoded-dimension and failed counts with estimated bytes; view-store entries; the malloc `in_use` and `retained` figures that `cv::memory` reports; per-document `in_use` delta on recompute; Tick wall time and re-lower count; resolver and explorer build deltas; decode source size and time; RPC reply line size with an estimated `Value` heap.
- **Contention labelling:** no run had a `rustc` running and psi avg10 was 0.00 in every block header, so **no block was contended**. Every cell has 3 runs. Latency figures are still limited by the compositor: the unfocused scratch window drew about 1 frame per second, so any "time until shown" is frame-quantised.

## Results: editor

Hidden-tab image release (phase row `idle60`; values are r1 / r2 / r3). Variant a (`CV_ME_HIDE=a`) drops unpinned entries at once; variant b (`CV_ME_HIDE=b`) keeps unpinned entries and evicts them least-recently-used under `CV_IMAGE_CACHE_MB=64`. Both release exactly the image owners of every note tab that is not active in its pane.

| run | RSS KiB | `in_use` MiB | image cache MiB / entries (pinned MiB) | re-decodes over all re-shows | high-water mark over cycle 2 KiB |
|---|---|---|---|---|---|
| S1 ref (15 tabs) | 387 804 / 388 468 / 388 884 | 177.8 | 124.2 / 53 (124.2) | 10 over 30 | 387 804–388 600 |
| S1 a | 199 176 / 199 052 / 198 660 | 64.0 | 10.7 / 3 (10.7) | 107 over 30 | 421 092–423 424 |
| S1 b | 252 232 / 251 968 / 252 296 | 115.4 | 61.9 / 24 (10.7) | 107 over 30 | 477 356–478 564 |
| S2 ref (25 tabs) | 502 524 / 501 752 / 502 676 | 277.8 | 213.8 / 90 (213.8) | 12 over 50 | about 502 000 |
| S2 a | 221 868 / 221 264 / 221 792 | 72.4 | 8.3 / 3 | 180 over 50 | 453 296–455 736 |
| S2 b | 276 780 / 276 656 / 276 756 | 126.0 | 61.8 / 24 | 180 over 50 | 514 052–517 068 |

Re-show cost: about 3.6 re-decodes per re-shown tab at about 24 ms each (p50); time from re-show to the last image shown is 1003 ms at p50 in every variant, the reference included (frame throttling, see Reading the tables).

Reading render models for hidden tabs. Heap per Reading model (allocator delta at recompute): p50 0.58 MB, p90 1.40 MB, max 1.81 MB; total 8.78 MB for 13 tabs and 15.35 MB for 25. UI-thread time of a Tick after a create (3 runs of about 20 ticks each; uncontended, frame-quantised host, so indicative):

| tabs (N) | ref: re-lowered, Tick p50 / p90 ms | of which resolver rebuild p50 ms | skip (hidden tabs not re-lowered) re-lowered, p50 / p90 ms | drop (hidden model dropped) re-lowered, p50 / p90 ms |
|---|---|---|---|---|
| 1 | 1, 15.6 / 16.3 | 15.2 | – | – |
| 5 | 5, 21.5 / 22.7 | 16.5 | – | – |
| 13 | 13, 24.6 / 27.6 | 14.4 | 1, 14.4 / 14.7 | 1, 14.0 / 14.6 |
| 25 | 25, 33.0 / 35.6 | 15.0 | 1, 1.7 / 14.9 | 1, 1.6 / 15.2 |

Bytes saved by drop (`in_use` at `changes20`, 3 runs): 154.0 → 149.2 MiB at N=13 (−4.8) and 249.2 → 241.1 MiB at N=25 (−8.1); skip saves nothing. Time from switch to re-lowered was p50 0.8–1.1 ms and max 3.4 ms, except one outlier of 983 ms in a frame-throttled run. The first drop build re-lowered every hidden tab on every tick (481–726 ticks per run); dropping needs the skip guard, and the numbers above are from the fixed build.

Decode peak (peak minus settled RSS in KiB, opening phase; settled values tie across variants). Image sources: p50 1.4 MP, max 3.8 MP; decode p50 about 24 ms.

| variant | 15 notes, 3 runs | 1 note, 3 runs |
|---|---|---|
| ref | 126 128 / 125 768 / 125 352 (about 123 MiB) | 80 416 / 80 600 / 80 300 (about 79 MiB) |
| cap 1 | 65 692 / 65 748 / 66 704 (about 64 MiB) | 14 012 / 13 972 / 13 624 |
| cap 2 | 88 356 / 90 032 / 87 040 (about 88 MiB) | 28 084 / 28 892 / 27 760 |
| cap 4 | 97 668 / 99 540 / 99 328 | 61 612 / 61 192 / 62 424 |
| `thumbnail` | 53 168 / 52 688 / 50 440 (about 51 MiB) | 18 192 / 18 724 / 17 696 |

Time until all images are shown for the 1-note open was tied across variants at 954–1959 ms (frame-quantised, uncontended).

Whole-vault path strings: allocator-measured deltas from an in-crate test over the real path list (16,530 paths, 1.52 MB of raw path text), the `me_acct` test in `explorer.rs` inside `editor.patch`.

| structure (HEAD) | bytes |
|---|---|
| `cv_text::Resolver` (exact, lower and basename maps: 4 path strings each) | 16 770 480 |
| `ResolverCache.ids` (duplicates `Resolver.exact`) | 2 854 400 |
| `ResolverCache.paths` | 2 855 456 |
| explorer `files` plus `folders` | 3 061 792 |
| explorer `paths` (a `Vec` clone of `files`) | 2 170 784 |
| **total** | **27 712 912 (26.4 MiB)** |

Cross-check against the live editor: resolver build delta 22 477 936 bytes and explorer delta 7 350 704 including rows, about 30 MB of the idle-plus-one-note heap (`in_use` goes from 13 MiB to 63 MiB once the first note opens). Cheapest sharing, modelled: one `Arc<str>` table (2.42 MB) shared by the resolver cache's `paths`, the explorer's `files` and the explorer's `paths`, with `ids` dropped, brings the total from 10.10 MB to 4.09 MB, a saving of 5.7 MiB. The model is not wired into the app. The remaining 16 MiB is inside `cv_text::Resolver`, shared with the daemon; sharing there is a cross-crate change and its saving is inference.

Quantified only. The decoded-dimension cache held 1058 entries, about 5.95 MiB (estimated as placeholder RGBA plus key plus 64 B), after the 25-tab session, and 207 entries (1.36 MB) with 3 tabs; failed decodes stayed at 0. It was not prototyped. The view store held 22 entries after the 9 notes that cover all 24 distinct `crucible-view` references; bytes were not measured and are inferred to be well under 5 MiB from the reply sizes. RPC reply decode: the 3 view replies over 64 KiB peak at a 193 233 B line with an estimated `serde_json::Value` heap of 776 688–819 410 B; it does not show in peak RSS (the view-session RSS was 166–168 MB).

## Reading the tables: editor

**Hidden-tab release.** Pinned bytes grow linearly with the tab count, about 8.3 MiB per image-heavy tab, and the cap in `CV_IMAGE_CACHE_MB` never applies because pinned entries are skipped by eviction. Variant a gives back about 186 MiB of RSS at 15 tabs (388 468 → 199 052 KiB) and about 280 MiB at 25 (502 524 → 221 868 KiB); variant b gives back about 133 MiB and 225 MiB. Variant b bought no hits: its re-decode counts are identical to a's in both sessions, because cycling through tabs has a working set above 64 MiB and least-recently-used eviction thrashes (inference), and it costs 52–54 MiB more than a. Time from re-show to the last image shown is 1003 ms at p50 in every variant, ref included; that is set by about 1 frame per second on the unfocused scratch window, not by the decodes, and true on-screen latency could not be measured without driving the user's focus. Each re-show's decode work is about 4 × 24 ms off the UI thread. Re-decoding raises the cycling high-water mark by about 34 MiB with variant a and about 90 MiB with variant b; that is exactly the decode transient, so the release and a decode cap belong together.

**Reading models.** Most of the stall after a create is the resolver, not the re-lowering: a full resolver rebuild costs about 14–15 ms and about 22 MB of allocation churn on every create, delete or rename, while re-lowering hidden tabs adds about 10 ms at N=13 and about 18 ms at N=25. Skip and drop remove only the re-lowering part. The drop saves 5–8 MiB at most, so it is low value, and dropping must imply skipping.

**Decode cap.** The pinned `image` 0.25.10 cannot decode at a reduced size: its JPEG path is zune-jpeg with no DCT scaling exposed, and PNG has none either. A smaller transient is still possible today: `resize(Triangle)` makes a `Rgba32F` intermediate of width × new height × 16 B (`imageops/sample.rs:490`), and `DynamicImage::thumbnail` is an integer area average with no f32 buffer. True scale-on-decode would need a new dependency such as `turbojpeg` (libjpeg-turbo scaling); it is named here, not added. Capping concurrency takes the 15-note peak from about 123 MiB over settled to about 88 MiB at cap 2 and about 64 MiB at cap 1, and `thumbnail` to about 51 MiB, with no time cost measurable on this host. The serial cost of cap 1 is inference: 5 images × about 24 ms is about 120 ms of decode, against about 25 ms in parallel. The quality of `thumbnail` against Triangle was not checked visually.

**Path strings.** Editor-only sharing is worth 5.7 MiB. The bigger lever is the resolver itself (16 MiB, rebuilt in full on every create, delete or rename, which is also the Tick stall above).

## Method: daemon spike

- **Harness** (scratch directory, prunable): `run.sh <label> <A|B|S|...> [KNOB=value ...]` starts a scratch daemon and runs one block; `imatrix.sh` runs the attribution matrix over `CONFIGS`; `headline.sh <A|B|OFF> <cycles>` drives the uninstrumented headline blocks (`AIDLE=120`, rotated per cycle); `summarize.sh`, `isum.pl` and `vsum.pl` produce the tables; `eqrun.sh` and `eqcmp.sh` compare replies; `sub.pl` is a notification subscriber; `client.pl` and `eqclient.pl` drive the requests. Scenarios A and B are those of the memory-release-strategy spike: A is 3 sequential view passes with 5 keyword searches each, then idle; B is 180 s of job replay plus a pipelined view pass every 20 s, then drain and idle; S measures the search index open on its own.
- **Isolation:** every path through environment variables (`CV_SOCKET`, `CV_*_DB`, `XDG_*`, `HOME`, all under the block's scratch root); scratch vault copy with the config neutralised (`schedules.toml` emptied, pipelines disabled, `maildir=""`, `base_url` set to `http://127.0.0.1:9/v1`, verified by grep); `search.db` copied in read-only from a `sqlite3 .backup`. Search answered keyword-only (the embedder's connection to port 9 is refused locally), with no outbound calls and no re-embedding. The live daemon, editor, socket, vault and state were never touched.
- **Instrumentation:** `CVMR_COUNT`, `CVMR_STAT_MS` and `CVMR_DUMP_RELEASE` (a counting allocator by thread and phase, `malloc_info` dumps around the idle release), present only in the `proto` binary. The `cv-view` arena figures come from `malloc_info`, `regions.pl` and `isum.pl`.
- **Contention labelling:** psi `some avg10` mean and the maximum number of running `rustc` per block, from `samples.txt`. Almost every block was uncontended (psi 0.0–1.0, `rustc` 0); the exceptions are labelled where they occur (`H-A-c1-ref` had 6 running `rustc`, `H-A-c1-both` had psi 7.6, `H-B-c1-all` had psi 8.6). Every comparison has at least one clean pair, so latency and CPU figures count as indicative-clean, not as pass criteria. No block logged an error.
- **Configurations:** `off` (knobs off), `direct` (`CVP_DIRECT`), `reuse` (`CVP_REUSE`), `both` (direct and reuse), `sems` (`CVP_SEMS` with `CVP_OPEN_BLOCKING`), `all` (all four knobs).
- Two passes were made. The first pass stopped partway and its figures are single-block; the second pass re-ran everything with attribution matrices of 2 cycles and headline matrices of 3–4 cycles. **Where the passes disagree, the second pass is used** (for example the preload-tags figure is 38.6 MiB, not the 23.8 of the first pass's smoke block, which predates an instrumentation change); the first pass's contribution that survives is the attribution of the index open, the anchors, the cache-reuse design and the equality check.

## Results: daemon

Attribution matrix (instrumented `proto`, `AIDLE=30`, 2 cycles c1 / c2, all uncontended). MiB unless stated.

| config | view alloc per pass | index open alloc | open ms | in-use at rest | `cv-view` arena resident before release | after release | Rust-live in arena KiB | free MiB | released MiB | peak | settled |
|---|---|---|---|---|---|---|---|---|---|---|---|
| off | 237.7 | 427.9 | 428 / 431 | 57.3 | 24.4 / 23.3 | 8.0 / 8.0 | 150 / 147 | 23.3 / 21.7 | 21.3 / 20.0 | 80 / 79 | 63 / 62 |
| direct | 219.9 | 427.9 | 419 / 433 | 57.3 / 57.4 | 23.8 / 24.9 | 7.7 / 7.8 | 146 / 148 | 22.1 / 24.8 | 20.7 / 21.9 | 81 / 82 | 62 / 64 |
| reuse | 81.0 | 427.9 | 441 / 441 | **99.0** | 47.3 | no release | n/a | 6.1 / 7.8 | **0.0** | 105 / 107 | **105 / 107** |
| both | 79.6 | 427.9 | 450 / 450 | **100.5** | 61.0 / 61.1 | 47.9 | **34 669 / 34 658** | 18.4 / 19.2 | 17.7 / 18.0 | 119 / 120 | **106 / 106** |
| sems | 237.7 | **45.2** | 417 / 418 | 57.3 | 24.3 / 23.8 | 7.5 / 8.1 | 146 / 156 | 23.5 | 21.3 / 20.4 | 82 / 81 | 63 / 63 |

Allocation per pass on the `cv-view` thread (MiB / ms; pass 1 of 3, passes 2 and 3 equal unless noted):

| config | total | preload props | parse | preload tags | candidates | links | `into_protocol` |
|---|---|---|---|---|---|---|---|
| off | 237.7 (476 ms) | 106.3 | 31.1 (283,946 values, 1.78 MB of text) | 38.6 | 26.3 | 7.5 | 21.6 |
| direct | 219.9 (460 ms) | 88.5 | 22.8 | 38.6 | 26.3 | 7.5 | 21.6 |
| reuse pass 1 | 124.9 (291 ms) | 50.6 | 8.6 (59,295) | 3.0 | 7.9 | 7.5 | 21.6 |
| reuse pass 2 / 3 | **59.1 (72–80 ms)** | 0.0 | – | 0.0 | 0.4 | not re-run | 21.6 |
| both pass 1 | 120.8 (278 ms) | 46.5 | 6.7 | 3.0 | 7.9 | 7.5 | 21.6 |
| both pass 2 / 3 | 59.1 (68–72 ms) | 0.0 | – | 0.0 | 0.4 | not re-run | 21.6 |
| sems | 237.7 | 106.3 | 31.1 | 38.6 | 26.3 | 7.5 | 21.6 |

Search, per query (15 queries, first pass, instrumented): Rust allocation 180–222 KiB inside `semsearch` `Index::search`, plus 4 KiB building the result; the SQLite heap peaks 0.4–1.3 MiB above its 9.3 MiB resident and returns to it; query time 10–59 ms. Once per process the first search opens the index (`SearchDb::shared` → `semsearch::Index::open` → `load_vectors`): 428–440 MiB allocated in 421–429 ms, on a tokio worker (called from async code at `search_query.rs:38`) that stays blocked for the whole open, because `load_vectors` calls `sign_code(&from_blob(blob))`, a 4 KiB `Vec<f32>` for each of the 95,019 vectors (1024 dimensions). The churn is freed immediately; the live cost from then on is in-use 26.8 → 52.3 MiB (+25.5) and anon 33.6 → 55.5 MiB (+21.9): the rowid-to-source map, the sign codes, the rowid list and the position map.

Headline blocks (uninstrumented; `ref` and `plain`; `both` and `all` are `plain` with the named knobs). MiB and ms; v50 / v95 are client-side view latency, s50 / s95 search latency, cpu is daemon CPU in seconds.

Scenario A:

| block | peak | mean | settled | v50 | v95 | s50 | s95 | cpu s | contention |
|---|---|---|---|---|---|---|---|---|---|
| c1-ref | 79 | 67 | 59 | 5 | 48 | 31 | 479 | 2.1 | `rustc`=6, contended |
| c2-ref | 79 | 66 | 61 | 5 | 58 | 31 | 503 | 2.3 | clean |
| c3-ref | 79 | 66 | 77 | 5 | 52 | 30 | 483 | 2.1 | clean |
| c4-ref | 81 | 68 | 81 | 9 | 114 | 33 | 662 | 3.6 | clean, slow for no recorded reason |
| c1-both | 117 | 86 | 104 | 1 | 28 | 30 | 508 | 1.4 | psi 7.6, contended |
| c2-both | 106 | 79 | 106 | 1 | 30 | 29 | 502 | 1.4 | clean |
| c3-both | 118 | 86 | 118 | 1 | 25 | 30 | 486 | 1.4 | clean |
| c4-both | 107 | 79 | 107 | 1 | 27 | 30 | 484 | 1.3 | clean |
| c1-all | 106 | 79 | 106 | 1 | 28 | 29 | 491 | 1.4 | clean |
| c2-all | 115 | 85 | 115 | 1 | 29 | 30 | 475 | 1.4 | clean |
| c3-all | 107 | 80 | 107 | 1 | 26 | 29 | 482 | 1.4 | clean |
| c4-all | 107 | 80 | 107 | 1 | 26 | 30 | 484 | 1.3 | clean |
| H-OFF c1 (`plain`, knobs off) | 79 | 66 | 58 | 5 | 52 | 29 | 492 | 2.2 | clean |

Scenario B (dupes95 and lsa95 are the dupes-scan and link-scan-all job p95):

| block | peak | mean | settled | v50 | v95 | s50 | s95 | cpu s | dupes95 | lsa95 | contention |
|---|---|---|---|---|---|---|---|---|---|---|---|
| c1-ref | 171 | 122 | 68 | 219 | 404 | 39 | 523 | 216.3 | 5.3 s | 982 ms | clean |
| c2-ref | 169 | 121 | 72 | 217 | 418 | 36 | 543 | 209.6 | 5.6 s | 960 ms | clean |
| c3-ref | 172 | 121 | 76 | 213 | 401 | 39 | 538 | 215.4 | 5.2 s | 959 ms | clean |
| c1-all | 210 | 165 | 118 | 36 | 268 | 35 | 522 | 209.6 | 5.2 s | 1.0 s | psi 8.6, contended |
| c2-all | 213 | 165 | 115 | 32 | 282 | 40 | 571 | 212.9 | 6.0 s | 1.0 s | clean |
| c3-all | 213 | 164 | 117 | 33 | 266 | 35 | 520 | 209.8 | 5.1 s | 937 ms | clean |

Notifications, scenario B (instrumented, uncontended, `CONFIGS=off`): 8.29 MiB over the block, about 9 passes, so about 0.92 MiB per pass. The subscriber counted `job_updated` 5,468 (656,156 bytes on the wire), `views_invalidated` 1 (696 bytes) and `reply` 2 (341 bytes). Request parsing (`req`, which includes `from_value(v.clone())`, hello and subscribe) is 21.03 MiB over the block, and 0.31 MiB per 3 passes in scenario A, which has no writes. Peak 173 and settled 74 MiB, equal to the baseline of 72–75; free-resident at the end was 13.0 MiB, under the trim threshold, so the idle release did not fire. No arena dump was taken in B.

`cv-view` arena (reference, `AIDLE=30`): resident 23–25 MiB before the idle release and 7.7–8.1 MiB after; Rust-live memory in it afterwards only 149–156 KiB. `malloc_info` for that arena: about 23 MiB system, 18.2 MiB free, about 4.9 MiB in use, most likely the view connection's SQLite page cache and statements (inference, not instrumented), leaving about 3 MiB of fragmentation. Idle release in these blocks: free-resident 21.7–23.9 MiB, released 19.9–22.1 MiB, settled 62–63 MiB.

Reply equality (`eqrun.sh 2` with `PROTO=plain`, all four knobs on, the second pass reads the kept caches): **same 41, differ 10.** The 10 are `job` j1–j3, `job_timeline` j1–j3, `jobs_list`, `jobs_stats` and `status` (run-dependent values), plus the `ingestion#Possible duplicates` view, which also differs between two reference runs. `views_list`, 25 of 26 views and all 10 searches are byte-identical across the outputs.

Per prototype:

| prototype | allocation per pass | A peak / settled | B peak / settled | cost |
|---|---|---|---|---|
| direct decode | −17.8 MiB (237.7 → 219.9, −7.5 %); parse 31.1 → 22.8 | tied (81 / 62 against 80 / 63) | not run alone | none measured; in-use at rest unchanged (57.3) |
| keep caches across runs | −156.7 MiB on the first pass (237.7 → 81.0); warm passes 59.1 (−75 %) | **worse: +27 to +39 / +25 to +59** | **worse: +41 / +45** (all knobs) | **+42 MiB live at rest**, and the idle release no longer fires; clean-pair gain: view v50 5 → 1 ms in A and 215 → 33 ms in B, A CPU 2.2 → 1.4 s |
| sign bits from the blob plus open under `block_in_place` | index open −383 MiB of churn, once per process (427.9 → 45.2) | tied (81–82 / 63) | not run alone | none; open time unchanged at about 420 ms |

The cache-reuse prototype keeps the caches in the view host, keyed by (generation, `PRAGMA data_version` of the view connection). `data_version` is required: generation alone is unsafe because incremental upserts write into the same generation and `NotesChanged` arrives up to 150 ms after the write. A test in the prototype (`tests_reuse.rs`) runs three views, upserts a note and compares against a fresh host; it passes with reuse on, **fails** when `data_version` is dropped from the key, and the whole views suite passes with `CVP_REUSE=1 CVP_DIRECT=1` (63 + 11 tests).

## Reading the tables: daemon

**Search index open.** The premise that keyword search allocates about 80 MiB per query is wrong at HEAD: a query allocates about 0.2 MiB, and the roughly 430 MiB is a one-time cost on the first search. The sign-bits prototype takes the open from 427.9 to 45.2 MiB (−383 MiB of churn) and leaves the open time at about 417–420 ms and the resident index unchanged; the A peak and settled figures already include about 22–25 MiB of search index. Opening under `block_in_place` makes the blocked worker explicit instead of an unannounced stall. Whether `semsearch` should keep those mirrors resident for keyword-only use is a `semsearch` API question this spike did not answer.

**Direct property decode.** A serde visitor decoding straight into `views::Value`, read through `get_ref` (no `String` per row), saves 17.8 MiB per pass (preload props 106.3 → 88.5, parse 31.1 → 22.8). It is small and low risk; the effect on A peak and settled cannot be separated from noise (peak 81 against 80, settled 63 against 62 in the attribution; tied in the headline blocks).

**Keeping caches.** The churn cut is large (237.7 → 59.1 MiB on warm passes), and on the clean pairs view p50 falls from 5 to 1 ms in A and from about 215 to about 33 ms in B (p95 about 405 → about 270 ms), with A CPU from 2.1–2.3 s to 1.3–1.4 s; search latency, B CPU and job latency are tied. But the caches hold about 42–43 MiB live (in-use at rest 99.0–100.5 MiB against 57.3), free-resident stays under the trim threshold so the idle release never fires, and settled anon in A rises from about 60 to 104–118 MiB and in B from about 72 to 115–118 MiB, with peaks up by 27–39 MiB (A) and about 41 MiB (B). Reference A settled varied from 59 to 81 MiB across cycles (the idle release fired in c1 and c2 but apparently not in c3 and c4; **the cause was not established**), so the settled deltas are given as ranges. The latency gain is real on clean pairs but the memory direction is the opposite of the goal; a bounded or idle-dropped cache is not established and would be a new design needing its own spike. With the caches warm, the per-pass residue is almost entirely the result tree (`into_protocol`, 21.6 MiB per pass) plus evaluation.

**Notifications.** At 0.92 MiB per pass, under the 2 MiB-per-pass threshold set for this item, and with no arena residue evident (settled at baseline, inference). Nothing to gain.

**View arena.** About 8 MiB resident and about 150 KiB Rust-live after the idle release, in every config except `both`, where the caches themselves (about 34 MiB live) keep it at 47.9 MiB. No small named Rust allocation pins it; the residual in-use memory is not Rust. Nothing to gain.

**Knobs-off `plain`.** Not byte-identical code (the knob branches exist), but its block (79 / 66 / 58, v50 5, cpu 2.2) is within the reference range and matches the reference behaviourally.

## Measured and rejected

- **Keeping the view `Db` caches across runs** (reuse): −75 % churn on warm passes and about 5× faster views, but about +42 MiB live at rest (settled about 60 → about 107 MiB, peak about +40 MiB, idle release never fires). Rejected for memory. Keyed only by generation it is also unsafe; `PRAGMA data_version` is required.
- **Notifications and connection-level replies:** 0.92 MiB per pass, below the 2 MiB threshold, no arena residue; left alone.
- **The `cv-view` thread's arena:** about 8 MiB resident, about 150 KiB live, about 3 MiB of fragmentation with no nameable cause; left alone.
- **Holding images of hidden tabs under the `CV_IMAGE_CACHE_MB` budget (variant b):** 52–54 MiB less saved than variant a with identical re-decode counts; left alone.
- **Reading render models for hidden tabs:** 5–8 MiB and 10–18 ms of stall per create at 13–25 tabs; left alone. If done, dropping must imply skipping.
- **Path-string sharing:** 5.7 MiB modelled in the editor, about 16 MiB more inside `cv_text::Resolver` (cross-crate, inference); left alone.
- **The decoded-dimension cache:** about 5.95 MiB after 25 tabs, not prototyped; left alone. The view store (22 entries) and the RPC client's reply `Value` (under 1 MiB; the real fix needs the `raw_value` feature, a manifest edit) likewise.
- **`thumbnail` as the decode-transient fix:** about 51 MiB peak over settled, better than cap 2, but its quality against the Triangle resize was not checked and the pinned `image` cannot scale on decode; the cap was preferred.
- **View results as text instead of a `Value` tree** (21.6 MiB per pass): needs a `RawValue`-style protocol change, which is a cargo feature; not prototyped.

## Not measured

- **PDF page pins:** no note embeds a PDF (a search for PDF embeds found 0), so the PDF-page part of the hidden-tab release was not exercised.
- **True re-show latency:** time until shown is frame-throttled at about 1 s on the unfocused scratch window in every variant, so it is not a latency.
- **The decode cap combined with hidden-tab release,** and `thumbnail` plus a cap together.
- **Cap 1 time cost:** only inferred (about 120 ms of serial decode against about 25 ms in parallel); the measured open times were tied but frame-quantised.
- **The semantic search path:** search was keyword-only in every block (embedder pointed at `127.0.0.1:9`), so query embedding and any outbound path never ran.
- **Latency and CPU on a quiet host:** every latency and CPU figure, including the reference's slow block (`c4-ref`) and the variance in reference settled memory (59–81 MiB), needs a re-measure before a pass criterion is set. A and B peak and settled for direct decode alone and for the sign-bits change alone, uninstrumented, were measured only in the instrumented attribution matrix (one pair); B with direct plus sign-bits and no reuse was not run.
- Also not measured: notification arena residue in B (no arena dump), the bytes of the view store, `thumbnail` quality, and a prototype of the decoded-dimension cache.

## Commands

Run from each spike's scratch directory; scratch paths are written `<scratch>/…`.

Editor spike:

- **Build:** two worktrees at `38f52df8`, `<scratch>/ref` and `<scratch>/proto`, each with its own `CARGO_TARGET_DIR`, release `gpu`; the ref tree gets the instrumentation patch only, the proto tree the instrumentation plus `editor.patch`.
- **Scratch daemon:** `bash daemon.sh` (socket `<scratch>/s/run/d.sock`); vault copy with `rsync --no-links` excluding `.git`, `.obsidian` and `.trash`, config neutralised as in the method.
- **Sessions:** `bash session.sh <label> <ref|proto> <S1|S2|S3|O|V> [n] [ENV=..]`; `bash matrix.sh <spec> 3` for specs A, B, C and D.
- **Numbers:** `perl analyze.pl out/<run>.log` and the `idle60` and phase rows of `out/<run>.rows` (hidden-tab release, decode peak, Tick times, `in_use` deltas); the path-string table comes from the `me_acct` test in `editor.patch`.
- **Gates for the prototype tree:** `cargo clippy -p editor --all-targets -- -D warnings` clean; `cargo test -p editor` 675 passed, 0 failed, 3 ignored; `shellcheck -s bash -S warning session.sh daemon.sh matrix.sh` clean.

Daemon spike:

- **Build:** `git worktree add --detach <scratch>/<ref|proto|plain>/crucible-vault <rev>` with `crucible-tui` and `semsearch` symlinked beside it (for `proto`, `semsearch` points at a scratch copy carrying the sign-bits change); `CARGO_TARGET_DIR=<scratch>/<ref|proto|plain>/target cargo build --release -p cv --bin cv`.
- **Search index open and per-query cost:** `SUB=1 AIDLE=30 ADUMP="0 29" bash run.sh I0-smoke A CVMR_COUNT=1 CVMR_STAT_MS=1000 CVMR_DUMP_RELEASE=1`, then `grep "CVMR search" out/I0-smoke/cvmr.log`; `bash run.sh I0-S S CVMR_COUNT=1 CVMR_STAT_MS=1000` for the index open on its own.
- **Attribution matrix:** `CONFIGS="reuse both sems off direct" bash imatrix.sh I2 2`, then `perl isum.pl out/I2-*` and `perl vsum.pl out/I2-c1-<cfg>/cvmr.log` (one log per call; a glob fails). Per-pass allocation tables come from `vsum.pl`.
- **Notifications:** `CONFIGS=off bash imatrix.sh I3 1 B`, then `perl isum.pl out/I3-*` and `out/I3-c1-off/sub.txt`.
- **Headline blocks:** `ln -sfn <scratch>/<ref|plain>/target <scratch>/target`, then `bash headline.sh <A|B|OFF> <cycles>` (`AIDLE=120`, order rotated per cycle); tables from `bash summarize.sh H-A-`, `H-B-` and `H-OFF-`.
- **Equality:** `PROTO=plain bash eqrun.sh 2`, then `bash eqcmp.sh out/E-r*/eq.txt out/E-r*-proto/eq2.txt` (it needs explicit file arguments; called bare it prints `same=0 diff=0`).
- **Prototype gates (plain tree):** `cargo test -p cv -p views -p cv-search` passes with and without `CVP_DIRECT=1 CVP_REUSE=1` (543 passed / 1 ignored, 63, 18, 11, 11, 7, 5, 2); `cargo clippy -p cv -p views -p cv-search --all-targets -- -D warnings` fails on the single placement error above; `shellcheck -s bash -S warning` is clean on every script after two small fixes.
- **Environment knobs (prototype binaries only):** `CVP_DIRECT` direct decode, `CVP_REUSE` caches across runs, `CVP_OPEN_BLOCKING` open under `block_in_place`, `CVP_SEMS` sign bits from the blob (needs the sign-bits change in `semsearch`); instrumentation `CVMR_COUNT`, `CVMR_STAT_MS`, `CVMR_DUMP_RELEASE`.

## Post-implementation (1.0.1 against the release candidate)

Measurement of the `memory-followups` work packages (WP-2 hidden-tab image release, WP-3 decode cap of 2, WP-4 blocking index open with `semsearch` 0.3.1, WP-5 view property values decoded straight from JSON text) against the shipped 1.0.1. Memory figures are the criteria; latency and CPU are not reported as findings. Aggregates only.

### Host and builds

- Shared workstation, 24 cores, 61 GiB. Every block ran with no `rustc` running (the `rustc` count was 0 in every block header and every daemon sample) and no build overlapped a block.
- **pre** = tag `v1.0.1` (`e31a9a7d`), built against `semsearch` 0.3.0 (`git archive v0.3.0` extracted beside the tree; the tree's `Cargo.lock` still pins 0.3.0 and is unchanged after the build).
- **post** = master at `592805d5` (WP-2, WP-3, WP-4, WP-5 landed), built against `semsearch` 0.3.1 (symlinked; the lock pins 0.3.1, unchanged).
- Each tree sits at `<scratch>/<name>/crucible-vault` with `crucible-tui` beside it and its own `CARGO_TARGET_DIR`; all builds are `--release`. Editor trees: `cargo build --release -p editor -p cv` (default `gpu` feature), uninstrumented on both sides. Daemon trees: `cargo build --release -p cv --bin cv`, four of them: pre and post uninstrumented (headline and equality), pre and post instrumented with the counting allocator (`instr.patch`).
- `instr.patch` applied to pre unchanged. On post it rejected in two files and the probes were placed by hand, instrumentation only, in the scratch tree: in `search_query.rs` the open-allocation and open-time probes moved inside the `spawn_blocking` closure that now performs the index open (the counter is per thread, so it has to be read on the blocking thread); in `views/src/db.rs` the property-preload probes went around the property loop and the tag loop, and the per-value decode probe into `json_value`.
- Vault: a copy of the live vault made with `rsync --no-links` without `.git`, `.obsidian`, `.trash`; config neutralised (`schedules.toml` emptied, 6 of 6 pipelines `enabled = false`, `maildir = ""`, 3 of 3 `base_url` at `http://127.0.0.1:9/v1`, checked by grep counts); `search.db` from a read-only `sqlite3 .backup`. Search answered keyword-only. The live daemon, editor, socket and vault were not touched.

### Criterion 1 — editor settled RSS after S1 (`idle60`), post at least 140 MiB lower in all 3 cycles

S1: 15 image-heavy tabs opened 3 s apart, two re-show cycles, idle 60 s; pre and post interleaved, 3 cycles. The editor ran on X11 (`DISPLAY` only). Sanity check passed: the `opened` rss is far above the `idle` rss in every session (about 126 MiB idle, 236 to 262 MiB post and 482 MiB pre when opened).

| cycle | pre `idle60` rss (MiB) | post `idle60` rss (MiB) | difference (MiB) | psi `some avg10` (pre / post) |
|---|---|---|---|---|
| 1 | 454.3 | 197.5 | 256.8 | 0.00 / 15.53 |
| 2 | 454.7 | 197.5 | 257.2 | 23.88 / 0.00 |
| 3 | 434.4 | 197.3 | 237.1 | 0.00 / 0.00 |

**Pass** (smallest difference 237.1 against 140). Spike: 186.

### Criterion 2 — editor load peak over settled at or below 100 MiB on post in all 3 cycles

Definition as in the spike's decode-peak table: `hwm` after the opening phase (the high-water mark is reset before the 15 opens) minus the settled `opened+10` rss.

| cycle | pre hwm / settled / peak over settled (MiB) | post hwm / settled / peak over settled (MiB) |
|---|---|---|
| 1 | 486.5 / 416.7 / 69.8 | 333.7 / 203.7 / 130.0 |
| 2 | 487.3 / 416.9 / 70.4 | 332.5 / 196.8 / 135.7 |
| 3 | 485.7 / 416.1 / 69.6 | 287.9 / 196.6 / 91.3 |

**Miss** on cycles 1 and 2 (130.0 and 135.7 against 100); cycle 3 is within (91.3). The absolute peak is far lower on post (hwm 288 to 334 against 486 to 487 MiB) because the settled level is so much lower; the peak over settled is larger because the pre settled level already carries the retained images. The spike's figure was about 88 at cap 2. Cycle 1 of post had psi 15.53 at the session start, but cycle 2 (psi 0.00) is as high, so the miss is not explained by contention. Not re-run, per the brief; the variation between cycles (91 to 136) is wide for an uncontended memory figure.

**Disposition (user, 2026-11-11): released as is.** The criterion compares the peak with a settled level that hidden-tab release lowered by about 220 MiB; measured from the idle level (about 126 MiB) the opening peak is 162 to 208 MiB on post against about 360 MiB on pre. The decode cap was not measured on its own here, so its separate effect on the opening peak is not established by this run.

### Criterion 3 — daemon index-open allocation on post at or below 60 MiB

Scenario-A block and a `S` block (index open alone) per side, counting allocator on; the open allocation is the `open_kib` field of the `CVMR search open=true` line (one line per block).

| block | pre (MiB) | post (MiB) |
|---|---|---|
| A (scenario A, instrumented) | 428.5 | 45.2 |
| S (index open alone) | 428.5 | 45.2 |

**Pass** (45.2 against 60). Spike: 45.2 and 427.9. psi 0.00, `rustc` 0 in all four blocks; no errors logged.

### Criterion 4 — daemon view-thread allocation per pass on post at or below 225 MiB

From `vsum.pl` over the instrumented scenario-A block's log, three passes of 15 views.

| pass | pre total (MiB) | post total (MiB) |
|---|---|---|
| 1 | 224.0 | 206.0 |
| 2 | 223.9 | 206.0 |
| 3 | 223.9 | 206.0 |

Largest components, pre against post: property preload 106.9 against 89.0 MiB, parse 31.3 against 22.9, candidates 26.4 on both, tag preload 23.9 on both, links 7.5 on both.

**Pass** (206.0 against 225). Spike: 219.9 and 237.7 (the pre figure here, 224.0, is below the spike's 237.7 because the tree is 1.0.1, which already carries the earlier releases' changes).

### Criterion 5 — daemon scenario A, uninstrumented: post peak at or below 100 MiB, median settled within 10 MiB of pre

`AIDLE=120`, pre and post rotated per cycle, 3 cycles; anonymous RSS in MiB from `summarize.sh` (peak = maximum over the block, settled = mean of the last 10 s).

| cycle | pre peak / settled | post peak / settled | psi / `rustc` |
|---|---|---|---|
| 1 | 79 / 60 | 82 / 62 | 0.00 / 0 |
| 2 | 80 / 59 | 80 / 60 | 0.00 / 0 |
| 3 | 79 / 59 | 80 / 61 | 0.00 / 0 |

Post peaks 82, 80, 80 (all at or below 100). Median settled: pre 59, post 61, a difference of 2 MiB (at most 10). **Pass.** No block logged an error.

### Criterion 6 — every view and search reply identical between pre and post

Two rounds of the equality block (two jobs, drain, then 51 replies per pass: 26 views, 10 searches, one view error, one job error, listings and job replies), pre and post per round, no knobs; each block runs the client twice (the second pass reads the kept view caches). Compared with `eqcmp.sh` on explicit file arguments, by hash and length.

- pre round 1 against pre round 2: 41 identical, 10 different. By kind: `job` 3, `job_timeline` 3, `jobs_list` 1, `jobs_stats` 1, `status` 1, and `view` 1.
- All four runs together (both pre and both post): the same 41 identical, the same 10 different, with the same kind counts. Since the all-four difference set contains every pre-against-post difference and every pre-against-pre difference and has the same size as the pre-against-pre set, every pre-against-post difference is one of the run-dependent replies: the job, timeline, list, stats and status replies and the one view that also differs between two pre runs.
- All 26 views but that one, all 10 searches, the view and job error replies, and the `views_list`, `actions_list`, `intake_list` and `lookup_currencies` replies are identical across all four runs, in both passes.

**Pass.**

### Summary

| criterion | result |
|---|---|
| 1 editor settled RSS at least 140 MiB lower | pass (237 to 257 MiB lower) |
| 2 editor load peak over settled at most 100 MiB | **miss** (130.0, 135.7, 91.3 MiB; pre 70 MiB) |
| 3 index-open allocation at most 60 MiB | pass (45.2; pre 428.5) |
| 4 view-thread allocation per pass at most 225 MiB | pass (206.0; pre 224.0) |
| 5 scenario A peak at most 100 and settled median within +10 | pass (peaks 80 to 82; median settled +2) |
| 6 replies identical | pass |

### Commands

Run from the harness copies in the scratch directory `<scratch>/…`; scratch paths are written `<scratch>/…`.

- **Trees:** `git worktree add --detach <scratch>/<name>/crucible-vault <rev>` for `<name>` in editor `ref` and `proto`, daemon `ref`, `plain`, `iref`, `ipost` (`ref`, `iref` at `e31a9a7d`; `proto`, `plain`, `ipost` at `592805d5`); `crucible-tui` symlinked beside each; `semsearch` an archive of `v0.3.0` beside the pre trees and a symlink to the sibling repo beside the post trees; `git apply` (pre) and `git apply --reject` plus the hand placement above (post) of `instr.patch` for `iref` and `ipost`.
- **Builds:** `CARGO_TARGET_DIR=<scratch>/<name>/target cargo build --release -p editor -p cv` (editor trees) and `… -p cv --bin cv` (daemon trees); the lock check is `grep -A1 'name = "semsearch"' Cargo.lock` in each tree.
- **Editor:** `DISPLAY=:0 ./matrix.sh spec 3` with the two-line spec `s1-pre ref S1` and `s1-post proto S1` (the spike's `session.sh`, `daemon.sh` and `matrix.sh`, unchanged apart from running from the scratch copy); numbers from `perl rsum.pl out/s1-*.rows` (label, phase, rss, hwm, psi and `rustc` count from the block header). Criterion 1 is the `idle60` rss; criterion 2 is the `opened` hwm minus the `opened+10` rss.
- **Scenario-A and S instrumented blocks:** `SUB=1 AIDLE=30 ADUMP="0 29" bash run.sh I-A-<pre|post> A CVMR_COUNT=1 CVMR_STAT_MS=1000 CVMR_DUMP_RELEASE=1` and `bash run.sh I-S-<pre|post> S CVMR_COUNT=1 CVMR_STAT_MS=1000`, binary switched through the `<scratch>/target` symlink; criterion 3 from `grep "CVMR search open=true" out/<block>/cvmr.log` (the `open_kib` field), criterion 4 from `perl vsum.pl out/I-A-<pre|post>/cvmr.log`.
- **Headline:** `bash h.sh 3` (a wrapper for `AIDLE=120 bash run.sh H-A-c<N>-<pre|post> A`, order rotated per cycle, binary through the symlink), table from `bash summarize.sh H-A-`.
- **Equality:** `bash e.sh 2` (a wrapper for `bash run.sh E-r<N>-<pre|post> E` on the uninstrumented trees, pre then post per round), then `bash eqcmp.sh out/E-r1-pre/eq.txt out/E-r2-pre/eq.txt …` with explicit file arguments, for `eq.txt` and `eq2.txt`, the output reduced to the kind counts with `awk`.

### Not measured

- Latency and CPU: recorded by `summarize.sh` but not reported, per the criteria (shared host).
- The semantic search path (no embedder; search stayed keyword-only in every block).
- Daemon scenario B (job replay with pipelined views) and the editor scenarios S2, S3, O and V: out of scope for this work package.
- The editor's per-structure attribution (image cache, view store, resolver): the editor builds were uninstrumented, so only the `/proc` figures (`Rss`, `VmHWM`) exist.
- Settled RSS of the editor after longer idle than the 60 s of S1.
