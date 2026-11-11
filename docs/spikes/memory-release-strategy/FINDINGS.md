# Memory release strategy — findings (2026-11-10, pre-plan spike for the memory-release-strategy plan)

Aggregates only. `view_run` replies were deleted unread once their sizes were taken. The view ids below come from vault `_custom/` configuration, which is reviewable.

Follows `docs/spikes/daemon-memory-release/FINDINGS.md` (2026-11-09, missing release triggers) and `docs/spikes/daemon-threads/FINDINGS.md` (round 23).

Binary: worktree of `4c091bb7` plus `spike.patch` (beside this file; one binary, policy chosen by environment variable), release build, run as a scratch daemon on a scratch vault copy (tmpfs). Host: 24 cores, shared with other sessions. The harness scripts (`run.sh`, `client.pl`, `matrixA.sh`, `matrixB.sh`, `summarize.sh`, `triggers.sh`, `overlap.pl`, `arenas.pl`, `attrib.sh`) and per-block output stay in the spike's scratch directory, which is prunable; the method section describes what they do.

## Summary

- **The quiet-queue problem (scenario A) is fixed by one delayed trim; how often it runs barely matters.** One gated `malloc_trim(0)` after the editor burst takes about 5 ms and brings settled anon from about 143 MiB down to 65–90 MiB. Periodic 60 s, idle 10 s and periodic 300 s all do this. After-view does too, but it trims 24–27 times per 3 passes.
- **Under mixed load (scenario B) every trigger option is within noise of baseline.** The existing after-job trim already fires 66–84 times per 180 s of job replay and dominates.
- **`malloc_trim(0)` holding arena locks does not delay concurrent work.** A forced trim every 1 s added about 254 trims per block (mean 1.3–1.5 ms, max 11.8 ms); view p95, dupes-scan p95 and link-scan-all p95 stayed within 5 % of the same-cycle baseline in all 3 cycles; requests that overlapped a trim had a median latency ratio of 1.00.
- **The malloc knobs do nothing useful.** A lower `M_MMAP_THRESHOLD`, a lower `M_TRIM_THRESHOLD` and `M_TOP_PAD=0` all stay within baseline noise.
- **Arena caps are rejected again.** In B, `M_ARENA_MAX=4` costs +8 / +31 / +60 % view p95 against the same-cycle baseline, and raises mean anon.
- **The transient comes from loading every prop of every candidate.** `Db::preload` is about 80 % of a scanning view's allocation, and 71–100 % of the prop rows it loads belong to keys the view never names.

## Verdict table

Medians over 3 interleaved cycles unless marked otherwise; anon in MiB of RssAnon. "Settled" is the mean of the last 10 s of the 60 s idle window. View latency is client-side: the A client sends requests one at a time (26 views plus 5 keyword searches per pass); the B client pipelines 26 views plus 2 searches every 20 s, so its p50/p95 include queueing. Job p95 is the `cv jobs stats` run p95.

| option | A settled | A view p95 | B mean / peak | B view p95 | B dupes / lsa p95 | trims | verdict |
|---|---|---|---|---|---|---|---|
| baseline (job trim only) | 143 (117–146) | 73 ms | 129 / 234 | 641 ms | 5.2 s / 1.1 s | A 0; B job 69–84 | — |
| 1. periodic 60 s | **65** (65, 65, 65) | 78 ms | 128 / 216 | 623 ms | 5.1 / 1.0 | A 1 × ~5 ms; B ≤1 (gate rarely met) | **adopt** as backstop or combine |
| 1. periodic 300 s | 67 (single run; held 141 for about 290 s, then one 5.1 ms trim) | 71 ms | not measured (B block shorter than 300 s) | — | — | 1 | adopt as the sampler-tick backstop only |
| 2. after-view + after-search | 87 (86–87) | 81 ms (+11 % against uncontended baseline) | 128 / 215 | 617 ms | 5.2 / 1.0 | A 24–27 × 2.2 ms; B view 9, search 0–2 | reject as primary (trims per request, A p95 cost, needs debounce) |
| 3. idle 10 s | 86 (66–90) | 70 ms | 129 / 224 | 619 ms | 5.2 / 1.0 | 1 × 4–6 ms | **adopt**, with N about 15–30 s |
| 4. `M_MMAP_THRESHOLD` 32 KiB | 142 (139–165) | 78 ms | — | — | — | 0 | reject (no effect) |
| 4. `M_TRIM_THRESHOLD` 16 KiB + `M_TOP_PAD` 0 | 116 (114–139) | 73 ms | — | — | — | 0 | reject (within noise) |
| 5. arena 2 | 108 (106–128) | 71 ms | — | — | — | 0 | reject (round-23 job p95 cost; does not fix the hold) |
| 5. arena 4 | 121 (105–130) | 69 ms | 141 / 218 | 844 ms (+8 / +31 / +60 %) | 5.3 / 1.1 | — | **reject** (view p95) |
| 5. arena 8 | 136 (117–164) | 71 ms | — | — | — | 0 | reject (no effect) |
| 5. arena 4 + after-view | — | — | 137 / 192 | 853 ms (+7 / +32 / +50 %) | 5.4 / 1.1 | view 9 | reject |
| lock stress: forced trim every 1 s | — | — | 122 / 218 | 632 ms | 5.2 / 1.1 | +254 × 1.3–1.5 ms (max 11.8) | not an option; shows trims are cheap |
| 6. smaller view working set | not prototyped | — | — | — | — | — | needs more (share measured below) |

- The B trigger rows are within noise of each other and of baseline: settled 74–85, mean 121–137 against base 128–132. No p95 is more than 10 % worse except the arena rows.
- In A, all latency rows are within about 10 % of the uncontended baselines except after-view (+11 %). `A-c1-base` ran contended (psi 3.14), so the uncontended c2/c3 baselines are the reference.
- CPU (daemon utime+stime) is the same as baseline (2.8 s in A, about 219 s in B) for every option except three: after-view in A (3.0 s, +7 %), arena 4 in B (224 s, +2 %), and the 1 s forced trim (+0.5 %).

## Smallest production change the data favours

Add **one gated release when the daemon goes idle**: no request and no running job for N seconds, with N about 15–30 s so it lands after tokio's 10 s blocking-thread keep-alive. Also have the 5-minute sampler tick call the same gated `cv_malloc::release(Figure::FreeResident)` on the blocking pool as a backstop. The pieces are an activity timestamp updated in `Shared::handle` and around `execute_steps`, a running-jobs counter, and one task; the spike version is about 40 lines (`crates/cv/src/spike.rs` in `spike.patch`). Keep `M_TRIM_THRESHOLD`, the default arena count, and the after-job trim.

What it would not fix:

- **Burst size:** a full-vault view still allocates 45–51 MiB, and one pass about 380–410 MiB in total.
- **Peak anon:** about 190–250 MiB under B.
- **Residue below the 32 MiB gate:** settled stays about 20–25 MiB above the live heap.
- **Sessions with no N-second gap:** only the backstop tick covers them.

Only option 6 shrinks the bursts.

## Diagnostic: where the free memory sits and why it climbs

Single run (`out/smokeD`): baseline build with the counting allocator on. Anon was 34 MiB before load, then 110 / 136 / 122 MiB after view passes 1–3, 162 MiB after pass 4 (views plus 5 searches), and 84 MiB after one forced 5.0 ms trim that released 76 MiB.

- **Not the main heap.** `[heap]` stays at 22 MiB in every snapshot. The growth is all in unnamed anonymous mappings, which are glibc thread arenas: 11 → 87 → 113 → 99 → 139 MiB.
- **Arena layout (`malloc_info`):** one arena holds 43–50 MiB, with 36–43 MiB of it free, and is reused pass over pass — the `cv-view` thread's arena. 26 MiB slabs appear in a *different* arena each pass, and each is 100 % free afterwards.
- **Why it climbs pass over pass:** each pass allocates 382–408 MiB in total, of which 335 MiB is on `cv-view` during the runs; the other 47–73 MiB is allocated on other threads. The likely source is `reply::<M>()` (`crates/cv/src/daemon/errors.rs:86`), which calls `serde_json::to_value(ViewResult)` on whichever tokio worker serves the request; the roughly 1 MB `ingestion#Trash` reply would become a 20–26 MiB `Value` tree, in a different arena each pass. **This attribution is inferred from the slab sizes, not instrumented.** The single serialized view thread is not the cause: its arena is reused.
- **Keyword `search` is also heavy:** the first search costs about 480 ms, and 5 searches allocate about 410 MiB on blocking-pool threads, again in new arenas.
- Blocking threads exiting after 10 s idle return about 20 MiB on their own (`A-c2-base` dropped from 137 to 117 at about 12 s with no trim).
- **Which part of a scanning view allocates:** `Db::preload` props allocate 35.6 MiB for 114,327 rows carrying 3.5 MB of value text, about 311 bytes per row; tags add 1.6 MiB. That is 74–80 % of the 45–51 MiB a full-vault view allocates (`backlinks#Here`, `hubs#Project Alpha`, `ingestion#Possible duplicates`, `ingestion#YouTube without metadata`; 8,456 candidates each). Folder-narrowed views (4,032 candidates) spend 16.9 MiB in preload out of 22–46 MiB. Rows that are scanned but not kept only read an `i64`, so the whole-table scan costs CPU, not heap.

## Per-option detail

**1. Periodic.** At 60 s: one trim (4.8–7.4 ms, 54–80 MiB released) brings anon to 65 MiB in all 3 cycles, but holds the burst for up to 60 s. At 300 s: held 141 MiB for about 290 s (single run). In B it rarely passes the 32 MiB gate because job trims keep free-resident low.

**2. After-request.** In A it trims after every scanning view, because the sequential client empties the queue: 24–27 trims, 52–59 ms in total. With the pipelined client in B it trims once per pass. It settles at 86–87 MiB: the searches at the end allocate on blocking threads below the gate, and blocking threads exit later. A view p95 is +11 %, because the next request waits behind a 2 ms trim; it would need a debounce. The after-search trim fired only 0–2 times per block.

**3. Idle 10 s.** One trim, 4.2–6.1 ms, 47–77 MiB released; settled 66 / 90 / 86. It settles above periodic 60 s because at 10 s the blocking-pool threads have not yet exited (tokio keep-alive is 10 s), and they free memory after the trim. N = 15–30 s should match periodic; this is an inference — N > 10 s was not run. No B cost.

**4. Malloc thresholds.** Today's effective mmap threshold is the 128 KiB default: the `mallopt(M_TRIM_THRESHOLD)` call in `tune_daemon()` disables the dynamic threshold. `mallinfo2` showed 5–8 mmapped blocks and 3.6–25.9 MiB `hblkhd`. Lowering it to 32 KiB has no effect, because the transient is millions of small allocations. Trim threshold 16 KiB with top pad 0 only trims arena tops.

**5. Arena count.** A only: 2 / 4 / 8 arenas settle at 108 / 121 / 136 MiB, against 143 for the default. B, arena 4 alone: view p95 +8 / +31 / +60 %, and mean anon is higher (135–146 against 128–132). B, arena 4 with after-view: +7 / +32 / +50 %.

**Lock holding (forced trim every 1 s).** dupes-scan p95 5.1–5.3 s against base 5.2–5.4 s; link-scan-all 1.0–1.2 s against 1.0–1.3 s; view p95 623–650 ms against 623–645 ms. For the 93–116 requests per block that overlapped a trim, the median ratio to that view's median latency is 1.00. Rare outliers of about 8.5× hit single fast views in the forced-trim, after-view and arena-4-plus-after-view blocks; one 4.5× outlier also appeared in a baseline block. Trim CPU is about 0.5 % of daemon CPU even at 1 Hz.

**6. Smaller view working sets** (`attrib.sh`). The index holds 82 prop keys, 114,327 rows and 4.49 MB of key+value text. For every view, 71–100 % of the prop rows preload loads belong to keys the view never names ("named" means anywhere in its `[[views]]` block, the file's `[formulas]`, or the top-level filters; the match is generous, so these shares are lower bounds). The figures are exact for full-vault views; narrowed views use the whole-vault key distribution. Projection would cut preload from 35.6 MiB to an **estimated** 2–10 MiB per full-vault view. Not prototyped (over the 50-line bound). A real fix needs: a referenced-property walk over the compiled filter, formula, order and sort expressions (`crates/views/src/engine.rs`), including bare-name resolution and a load-everything fallback for dynamic access; the key set passed into `Db::preload` (`crates/views/src/db.rs:99`) as `key IN (…)`; projection-aware `props` caching. A secondary saving would be parsing straight to `views::Value` instead of going through `serde_json::Value` (`db.rs:275`); not measured.

## Method

- **Scratch build:** `git worktree` of HEAD at `/tmp/cvmr/crucible-vault`, with symlinks to `../crucible-tui` and `../semsearch` and its own target directory. Removed afterwards.
- **Scratch vault:** a copy with symlinks and `.git` removed. Deleted afterwards.
- **Isolation from the live instance:** every path goes through environment variables (`CV_SOCKET`, `CV_*_DB`, `XDG_{RUNTIME,STATE,CACHE,DATA}_HOME` and `HOME`, all under the block's scratch root); `run.sh` refuses to start if the socket is under `/run/user/`. The live daemon, editor, socket, vault and state were never touched beyond one read-only `sqlite3 .backup` of `search.db`.
- **Neutralised config:** `schedules.toml` emptied; every pipeline `enabled = false`; `maildir = ""`; every `base_url` set to `http://127.0.0.1:9/v1`.
- **Search:** the `search.db` backup copied into each block. Search answers keyword-only because the embedder's connection to port 9 is refused locally. No outbound calls, no re-embedding.
- **Per block:** fresh state DB and index; samples every 0.5 s of RssAnon, cpu psi some-avg10 and the running-`rustc` count; every trim logged with duration and anon before and after.
- **Scenario A:** 3 sequential passes, then 60 s idle. **Scenario B:** 180 s of round 23's loop (`link-scan-all`, `dupes-scan`, 50× `link-scan --note`, drain) plus a pipelined view pass and 2 searches every 20 s; then drain and 60 s idle.
- **Interleaving:** 3 cycles in rotated order, each configuration paired with the same cycle's baseline. Contended blocks: `A-c1-base` (psi 3.14), `A-c1-p60` (psi 2.33), `B-c3-av` (23 rustc), `B-c2-base` (19) and `B-c3-idle10` (19). The conclusions hold without them.

## Deviations and coverage gaps

- The counting `GlobalAlloc` is present in every configuration; when off it costs one relaxed atomic load per allocation, and it only counted in `smokeD`.
- B ran 180 s of load instead of round 23's 300 s, so periodic 300 s was measured only in one long A run.
- Arena 2/8 and the malloc knobs were pruned after A.
- Search was keyword-only; the semantic path never ran. `localize-on-edit`, search-refresh and the network-bound actions were not exercised.
- The A client is sequential, the worst case for after-view; the real editor's request pattern was not replayed.
- Tmpfs storage affects absolute latencies, not the comparisons.
- An idle trigger with N > 10 s was not run. Option 6 was not prototyped. The attribution of the 26 MiB slabs to `serde_json::to_value` is inferred.
- `run.sh` was edited while matrix A's first block was running (scenario B lines only; every A block wrote complete output) and once more after both matrices (the `AIDLE` knob for the 300 s run).

## Commands

Run from the spike's scratch directory; scratch root `/tmp/cvmr`.

- **Build:** `git worktree add --detach /tmp/cvmr/crucible-vault HEAD`, `git apply spike.patch`, `CARGO_TARGET_DIR=/tmp/cvmr/target cargo build --release -p cv --bin cv`.
- **Diagnostic:** `bash run.sh smokeD D CVMR_COUNT=1`; then `perl arenas.pl out/smokeD/d<N>.xml`, `out/smokeD/smaps-<N>.txt`, `grep CVMR out/smokeD/cvmr.log`, and the `total_alloc=` field in `d<N>.xml.mallinfo`.
- **Matrices:** `bash matrixA.sh > matrixA.log`; `bash matrixB.sh > matrixB.log`; `AIDLE=330 bash run.sh A-single-p300 A CVMR_PERIODIC_SECS=300`.
- **Tables:** `bash summarize.sh A-c` / `B-`; `bash triggers.sh B-`; `perl overlap.pl out/B-*`; `bash attrib.sh`.
- **Environment knobs (spike binary only):** triggers `CVMR_PERIODIC_SECS`, `CVMR_IDLE_SECS`, `CVMR_AFTER_VIEW`, `CVMR_AFTER_SEARCH`, `CVMR_FORCE`, `CVMR_NO_JOB_TRIM`; malloc `CVMR_TRIM_THRESHOLD`, `CVMR_MMAP_THRESHOLD`, `CVMR_TOP_PAD`, `CVMR_ARENA_MAX`; diagnostics `CVMR_COUNT`, `CVMR_DIAG_DIR`.

## Post-implementation (2026-11-10)

Aggregates only. Same harness, scratch vault copy, neutralised config and isolation as the Method section; no spike patch on either binary, so the `trim*` and `relM` columns are 0 by construction (they come from the patch's log lines) and the release itself cannot be seen in them.

- **Compared:** two unpatched release builds of `cv`, `056e3e51` (pre: the commit before the idle release and the view preload projection) and `36550c94` (post: contains `b56ed15c`, the idle release after 20 s with no request and no running job plus the 5-minute sampler-tick backstop, and `36550c94`, view runs preload only the property keys they can reach). Each block is a fresh daemon on a fresh scratch copy.
- **Order:** 3 interleaved cycles, A pair then B pair in each: cycle 1 pre, post; cycle 2 post, pre; cycle 3 pre, post. Run times 21:50–22:23, 12 blocks, one driver.
- **Smoke first:** `W3-smoke-pre` / `W3-smoke-post` (A, 30 s idle): `err` 0, 78 `view ... ok` lines on each (26 views times 3 passes), no `/run/user/` socket.
- Anon in MiB of RssAnon; settled is the mean of the last 10 s of the 60 s idle window; v50/v95 are client-side ms; `err` was 0 in all 12 blocks.

### Scenario A (3 view passes with 5 searches each, then 60 s idle, no jobs)

| cycle | binary | peak | meanL | settled | v50 | v95 | psi | rc |
|---|---|---|---|---|---|---|---|---|
| 1 | pre | 164 | 118 | 141 | 13 | 73 | 1.33 | 0 |
| 1 | post | 120 | 82 | 69 | 5 | 70 | 0.00 | 0 |
| 2 | post | 142 | 99 | 112 | 5 | 71 | 0.00 | 0 |
| 2 | pre | 140 | 108 | 140 | 13 | 74 | 0.00 | 0 |
| 3 | pre | 139 | 103 | 139 | 14 | 70 | 0.00 | 0 |
| 3 | post | 117 | 84 | 110 | 5 | 65 | 0.00 | 0 |

### Scenario B (180 s job replay plus a pipelined view pass every 20 s, then drain and 60 s idle)

| cycle | binary | peak | meanL | settled | v95 | dupes95 | lsa95 | psi | rc |
|---|---|---|---|---|---|---|---|---|---|
| 1 | pre | 239 | 134 | 82 | 647 | 5.9 s | 1.1 s | 0.31 | 18 |
| 1 | post | 201 | 130 | 81 | 382 | 5.2 s | 1.0 s | 0.00 | 0 |
| 2 | post | 205 | 127 | 80 | 387 | 5.2 s | 1.0 s | 0.00 | 0 |
| 2 | pre | 234 | 126 | 164 | 630 | 5.3 s | 1.1 s | 0.00 | 0 |
| 3 | pre | 218 | 126 | 92 | 637 | 5.1 s | 1.1 s | 0.00 | 0 |
| 3 | post | 188 | 127 | 79 | 391 | 5.2 s | 1.0 s | 0.13 | 1 |

Post / pre ratio of the p95 columns in B (from the unrounded `summarize.sh` values shown above; the job p95s are printed to 0.1 s, so those ratios are coarse):

| cycle | view p95 | dupes95 | lsa95 |
|---|---|---|---|
| 1 | 0.59 | 0.88 | 0.91 |
| 2 | 0.61 | 0.98 | 0.91 |
| 3 | 0.61 | 1.02 | 0.91 |

### Pass criteria

| criterion | cycle 1 | cycle 2 | cycle 3 | result |
|---|---|---|---|---|
| 1. A settled anon on post at or below 95 MiB | 69 pass | 112 **miss** | 110 **miss** | **MISS** (2 of 3 cycles) |
| 2. A peak anon lower on post than on pre | 120 < 164 pass | 142 > 140 **miss** | 117 < 139 pass | **MISS** (cycle 2, by 2 MiB) |
| 3. B view p95, dupes95, lsa95 no more than 10 % worse | pass (0.59 / 0.88 / 0.91) | pass (0.61 / 0.98 / 0.91) | pass (0.61 / 1.02 / 0.91) | pass |

- Criteria 1 and 2 missed; per the plan they are reported, not fixed. No source, threshold or harness change was made.
- **Settled in A, what the samples show (not a root cause):** the idle release fires, but late and not always deep. Anon on post held 99 / 141 / 115 MiB until about 20–25 s into the idle window, then dropped to 69 (cycle 1), 111 (cycle 2) and 94 then climbing back to 109 (cycle 3) over the next 30 s. Without the spike patch's log lines the trim and its freed bytes are not visible, and the sampled drop is consistent with one release at about the 20 s mark. Why cycles 2 and 3 settle about 40 MiB above cycle 1 was not investigated (candidates: the 5 searches' blocking-pool threads still exiting after the release, the 32 MiB gate leaving residue, anon re-growing from the 5 s sampler); each would need its own measurement.
- **Against the spike's estimates:** baseline settled in A was about 143 (spike) and is 139–141 on pre here, so the pre build reproduces the spike. Post settles at 69 / 112 / 110, inside the spike's trigger-option range (65–90) only in cycle 1; the 20 s idle trigger was not shown to match periodic 60 s (65 in all three spike cycles). The spike inferred N above 10 s would match periodic; this measurement does not confirm it.
- **Peak:** post is lower than pre by 44 / -2 / 22 MiB in A and by 38 / 29 / 30 MiB in B (239 to 201, 234 to 205, 218 to 188). The preload estimate (35.6 MiB falling to an estimated 2–10 MiB per full-vault view) predicts a drop of up to about 25–33 MiB at the peak of a single view; the B peak drop of 29–38 MiB is in that range, but the peak also depends on arena placement and on overlapping requests, so these figures are consistent with the projection and do not isolate it. The A peak drop is smaller and noisier because the A client is sequential and its peak is set by the 5 keyword searches (about 80 MiB each on blocking threads per the spike), which this work did not change.
- **View latency:** the B view p95 fell by about 40 % on post (647 / 630 / 637 to 382 / 387 / 391 ms) and the A view p50 from 13–14 to 5 ms, the visible effect of the smaller preload. A view p95 is within noise (70 / 71 / 65 against 73 / 74 / 70 ms). Daemon CPU in B is 216–218 s on post against 219–223 s on pre, in A 2.1 s against 2.8 s.
- **Contended blocks:** `W3B-c1-pre` ran with 18 concurrent `rustc` (psi 0.31); no other block had psi above 0.14 or more than 1 `rustc`; `W3A-c1-pre` psi 1.33, below the contention line. The contended block only affects criterion 3 in cycle 1, which passes with margin on view and job p95 (and cycles 2 and 3 give the same direction), so no re-run was made. Neither A miss comes from a contended block.
- **Odd value:** `W3B-c2-pre` settled at 164, against 82 and 92 on the other pre cycles; the pre B idle settled value is noisy and was not used by any criterion.
- **Not measured or different from production:** the idle window is 60 s (the release fires at about 20 s, so only about 40 s after it are observed, and the sampler-tick backstop never ran); search is keyword-only because the embedder's endpoint refuses connections locally; the A client is sequential, not the editor's request pattern; the release and its freed bytes are not logged; tmpfs storage affects absolute latencies, not the comparisons; one set of 3 cycles, no re-runs.

### Commands

Run from the spike's scratch directory; scratch root `/tmp/cvmr`.

- **Builds:** `git worktree add --detach /tmp/cvmr/pre/crucible-vault 056e3e51` and `.../post/crucible-vault 36550c94`, each with `crucible-tui` and `semsearch` symlinked beside it; `CARGO_TARGET_DIR=/tmp/cvmr/<pre|post>/target cargo build --release -p cv --bin cv` from each worktree.
- **Per block:** `ln -sfn /tmp/cvmr/<pre|post>/target /tmp/cvmr/target; bash run.sh W3<A|B>-c<N>-<pre|post> <A|B>`; smoke `AIDLE=30 bash run.sh W3-smoke-<pre|post> A`.
- **Scratch vault, search DB and note list:** vault copy without symlinks and `.git`, config neutralised as in the Method section; `sqlite3 -readonly <live search.db> ".backup /tmp/cvmr/ro/search.db"`; 50 note paths from a sorted `find` outside `_custom/`.
- **Tables:** `bash summarize.sh W3A-` and `bash summarize.sh W3B-`; the idle time series is `awk` over `out/W3A-c<N>-post/samples.txt` from the `idle` phase time in `out/W3A-c<N>-post/phases`.
