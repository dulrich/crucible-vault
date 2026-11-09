# Decisions — crucible-vault

Settled with the maintainer in the feature-planning stream. Questions came from the Crucible feature inventory (kept in the maintainer's repo, not published). One entry per decision: date, question, decision, rationale. Superseding a decision appends a new entry, never rewrites an old one.

## D1 — Architecture shape (2026-10-13)

- **Question:** How are core and editor split?
- **Decision:** A headless **core daemon** is the sole vault writer, action runner, durable queue and `obsidian://` intake. The **editor GUI is a client** of it.
- **Rationale:** Filing, clipper intake and automation keep running with the editor closed. This removes the "only while Obsidian is open" limitation and Crucible's editor-buffer reconciliation (inventory §3).

## D2 — Tier C scope (2026-10-13)

- **Question:** Which tier C features stay in parity scope?
- **Decision:** In scope: **trackers** (YouTube/Blogs/Feed + channel enrich), **image describe for new clippings**, **ingestion dashboard**. Source-Eval *as a dashboard* was not selected — but see D2a.
- **Maintainer direction (intent):** "Tracking and ingestion should become mostly automatic into the daily captures, an eval model will be added and trained to classify based on (read, tags, monthly captures) what is important. The goal here will be reducing Email -> Open Link -> Web Clipper dependence, a later feature will allow MCP or another email automation to capture email-only newsletter. Daily/Monthly possibly become structured views rather than arbitrary notes, depending on complexity and features."
- **Rationale:** Trackers move from rarely run to the main intake path. The clipper stays as a fallback, not the primary route.

## D2a — Importance model lineage (2026-10-13, inference — confirm)

- Crucible's `src/sourceEval/` already collects the exact signals named in D2: `read`, tags, `eval-importance`/`eval-urgent`/`eval-rated`/`eval-tags` labels (`signals.ts` `parseEvalLabel`), and monthly `# Observations` quote counts (`scanObservationSignals`), plus a training-data export (`export.ts`). The importance classifier is therefore Source-Eval's successor; its signal collection is the part to carry forward. Pending maintainer confirmation (Q-round 2).

## D3 — Action model (2026-10-13)

- **Question:** How are chains, captures, agents and triggers unified?
- **Decision:** An **action = a list of typed steps**. It starts from a **key/command, an event, or a schedule**; durable actions run on the queue. The 10 "Ingest as …" chains collapse to one file-as-kind action plus a kinds table. The 4 "Refine Transcript" chains collapse to one action with a provider parameter. "Ignore Shorts" becomes an event-started action.
- **Rationale:** Replaces Crucible's chain/internal-command/trigger registries, which are Obsidian's command model (inventory §3).

## D4 — Config location (2026-10-13)

- **Question:** Where do action/kind/agent/provider definitions live?
- **Decision:** Declarative files under the vault's **`_custom/crucible-vault/`**, hot-reloaded by the daemon. Secrets live in env or `~/.config`, never in the vault.
- **Rationale:** The files travel with the vault, are reviewable by agents (`_custom/` holds the reviewable config, templates and prompts) and are diffable. This replaces Crucible's ≈7k-line settings UI.

## D5 — Importance model placement (2026-10-13; supersedes the D2a phasing guess)

- **Question:** How is the importance model phased?
- **Decision:** crucible-vault **keeps the signals** — Source-Eval's label and signal collection (`read`, tags, `eval-*` labels, monthly `# Observations` quotes) and the training export — as index data and queries. **The model itself is not built here.** It is developed separately and runs as a **sidecar**: likely a small encoder classifier, possibly a fine-tuned decision LLM. The feature calls it through a **decision-model HTTP API** (`POST /v1/systemone` on a compatible inference server) or a dedicated classifier sidecar (`POST /v1/classify`).
- **Implication for plans:** crucible-vault owns the signal store, the export, and a scoring client behind a trait, so it can be stubbed until the sidecar exists. No ML stack enters this repo.

## D6 — Automatic intake is in v0 (2026-10-13)

- **Question:** Where does automatic intake (trackers → daily captures) land?
- **Decision:** **In v0.** Trackers (YouTube/Blogs/Feed + channel enrich) feed daily captures automatically from day one; the Web Clipper remains a fallback intake path. Email/MCP newsletter capture is a later feature. Importance scoring plugs in when the D5 sidecar exists.
- **Implication:** v0 grows by about 6–10 kSLOC on the assessment's 28–40 estimate. Expect v0 to be split across more than one plan.

## D7 — Daily/Monthly notes (2026-10-13)

- **Question:** Do Daily/Monthly stay files, or become structured records?
- **Decision:** **Files stay as the source of truth; structured views render on top** from the index (captures, intake, reading). Promote to structured records later only if the views prove it.
- **Rationale:** Keeps the vault plain files, portable and safe to coexist with Obsidian during the transition.

## D8 — Search placement (2026-10-13)

- **Question:** Where does search live?
- **Decision:** **Leave Crucible's search companion as-is** (`scripts/search-companion/` in https://github.com/dulrich/obsidian-crucible, Docker, SQLite FTS5 + brute-force vectors + RRF), fed by the daemon, **unless its shape forces hacky workarounds in the daemon**. That is the integration-friction trigger for an own-vs-depend review. **End state:** a Rust search sidecar daemon alongside the main daemon. That is deferred, including whether SQLite stays its engine.
- **Superseded (2026-10-17):** D22 replaces the "sidecar" end state with in-process search (the `semsearch` crate).

## D9 — Editor ambition for v0 (2026-10-13)

- **Decision:** **Source + rendered reading pane** (toggle or split), syntax-highlighted source editing. Live preview is a later plan.
- **Rationale:** About 4–7 kSLOC native, against 10–20 kSLOC for live preview. This also keeps the GUI-stack choice from hinging on CM6's live preview.

## D10 — Bases → native views (2026-10-13)

- **Decision:** **Replace `.base` with daemon-native views** defined in `_custom/crucible-vault/` config, served from the index. The 6 `.base` files (14 table views) and their 231 embeds migrate. Obsidian rendering of those embeds is accepted as degraded during shadow (D11), because Obsidian stays the editor until cut-over and the `.base` files remain in place until then.
- **Restore what the Dataview→Bases migration dropped** (maintainer decisions 2026-10-12 — that migration was a precursor to this one). Bases' limits were no JS, no tasks, no cross-file aggregation, and `file.backlinks` whole-vault timing out (108 s+). The native views restore:
  - monthly reading-by-tag **day × category pivot** and the **Average/day** row (decision 4 there);
  - Initiative-overview **task counts** (decision 5);
  - month-note incomplete tasks with true **overdue semantics** ("before today"; core `query` couldn't express it — audit E3);
  - Works **unread reading list** as a real cross-file query, replacing the generated literal filter (decision 6);
  - **backlinks** as an index query (the owned link graph makes it cheap).
- Deliberately retired there and **not** restored: `folder-rules`, the per-account issue tables, the personalization table, the whole-vault task-count table. `orphans.js` and `transcript-backlog.js` are covered by the ingestion dashboard (D2).
- **Nav bars:** Note Toolbar's `{{js:}}` day/month nav becomes native editor chrome (inventory #2).

## D11 — Transition / cut-over (2026-10-13)

- **Decision:** **Shadow, then one switch.** Until v0 parity, the daemon runs against the live vault **read-only**: it indexes and logs the writes it would make, with real writes only to a scratch copy outside every repo. At cut-over, with the maintainer's explicit go-ahead, it takes `x-scheme-handler/obsidian` and Crucible's automation is disabled in one step. There is never a dual-writer period.
- **Implication:** v0 needs a shadow/dry-run mode in the action runner, plus parity criteria the maintainer can check before the switch.
- **Amendment (2026-10-19):** shadow mode is retired (plan `tn-review-baseline-remediation` WP-R6): the daemon has one (live) mode, `CV_MODE` is gone, and the protected-root + cut-over-marker guard (I1) is what keeps test and scratch runs off the live vault.
- **Superseded by D24:** I1 is retired; test fixtures asserting a temp vault root now keep tests off the live vault.

## D12 — OFM tokenizer reuse (2026-10-13)

- **Decision:** **Extract a shared pure `ofm` crate** from crucible-tui's `mdview` parse layer (wiki-links, frontmatter, lex). Both repos path-depend on it. The extraction is a **separate plan in the crucible-tui repo** (https://github.com/dulrich/crucible-tui); crucible-vault's plan sequences on it.
- **Rationale:** One tokenizer, with no terminal or GUI deps in the core. This honours the never-copy-paste-fork rule (the project contract's Stack section).

## D13 — GUI stack (2026-10-13; measured, `docs/spikes/gui-rss/FINDINGS.md`)

- **Decision:** **iced** (0.14.x) for the editor client.
- **Evidence:** ~390 MiB RSS at 20 image-heavy notes, flat at 5 min, ~0 % idle CPU, stable crates.io release with `markdown`/`image`/`text_editor` built in, 0 GPU faults. Tauri+CM6 measured ~1.0 GiB, about Obsidian's footprint. GPUI measured 4.9 GiB (unbounded image cache) and +6 GiB on a 1 MB paste.
- **Owned costs accepted:** an undo/redo stack (iced 0.14 has none), a virtual list for large file lists, a bounded image cache, and probably an improved editor widget. These fit D9's source + reading pane scope.
- **Crash premise retired:** both Obsidian crashes are a V8 fatal in Crucible's `NoteLockManager.withLock` (`docs/spikes/crash-attribution/FINDINGS.md`), not the graphics stack, so wgpu/Vulkan carries no inherited-crash risk.

## D14 — Database (project-setup Gate 3; 2026-10-13; measured, `docs/spikes/core-index/FINDINGS.md`)

- **Decision:** **SQLite (bundled rusqlite), two files, both owned by the daemon, paths behind env vars.**
  - `index.db` — **derived**: files, props (JSON-typed values), links, tags, blocks, FTS5. Dropped and rebuilt on any schema change, never migrated.
  - `state.db` — **primary**: job queue, link records, tracker state, shadow-write log. Versioned migrations.
- **Evidence:** 4.5 s full rebuild of 8,008 notes at ~32 MB peak RSS, 53 MB DB, ~1 ms Bases-style view queries, 14 MB idle PSS. Daemon shape per S2: a single writer thread owning the connection, an in-memory resolver, `notify` → (size, mtime) dedupe → a per-file transaction.

## D15 — Link records are primary data (2026-10-13)

- **Decision:** Link records live in `state.db`. **One-time import** from Crucible's `jobs.sqlite` `link_records` table (25,051 rows; `plans/link-registry-out-of-vault.md` in https://github.com/dulrich/obsidian-crucible) at cut-over, keeping enrichment (`yt-video-id`, `x-status-id`) and tracker state. After that, link scan keeps them current with idempotent upserts (carried from Crucible's no-op-on-equal rule).
- **Rationale:** Rescanning would burn YT/X API quota, and trackers (D6) start consuming `tracked_source` state.

## D16 — Plan split (2026-10-13)

- **Decision:** v0 lands as three plans, in order:
  - **(0)** a crucible-tui (https://github.com/dulrich/crucible-tui) plan that extracts a pure `ofm` crate (D12; S2's spec: frontmatter split + YAML→value, comrak options, an OFM walk with byte spans for wikilinks/embeds/md links/tags/footnotes/callouts/block ids, no glyph tier, no syntect/rustix);
  - **(A)** `plans/v0-core-daemon.md` — headless daemon: index, action model, queue, providers/agents, intake (URI + trackers → daily captures), X/YT, daily brief, link records, lint/localize, signals store, shadow mode;
  - **(B)** `plans/v0-editor.md` — the iced editor client: source + reading pane, shell, native views (D10), day/month nav.
- Cut-over (D11) follows (B). The maintainer keeps editing in Obsidian throughout (A)'s shadow period.

## D17 — Visibility (2026-10-13; assumed, not asked)

- Historical note: this decision assumed the repo would stay private and never be published. It was superseded on 2026-11-08: the repo is now published at `https://github.com/dulrich/crucible-vault` through a reviewed projection of the private working history.

## D18 — Config format (2026-10-13)

- **Decision:** every crucible-vault config file under the vault's `_custom/crucible-vault/` is **TOML** — actions, kinds, captures, agents, providers, schedules, plus plan B's views and bookmarks and A2's tracker/pipeline config.
- **Rationale:** serde-native, comments allowed, no YAML typing traps; proposed in `plans/v0-core-daemon.md` Decisions locked and confirmed by the maintainer at approval time.

## D19 — Cut over before A2; D11 exceptions (2026-10-14; supersedes parts of D11, D16)

- **Decision:** the switch to live ran on 2026-10-14 (plan `cut-over-now`), before A2 (`v0-automation-pipelines`), at the maintainer's request after Obsidian segfaulted three days running. D11's invariant is unchanged: one switch, no dual-writer period, no automation carried across modes.
- **Exceptions (maintainer, 2026-10-14):**
  - The ≥ 7-day soak (cut-over row 1) is waived. One parity run is kept for the record, not as a gate.
  - Row 4 is satisfied by Obsidian being dead and not running ("Obsidian is crashed, treat it as dead").
  - The P3 search-parity gap (row 9) is accepted as an exception, not a hold: 0.968 match after reconcile, 0 unexplained.
  - `v0-editor` WP-9's A2 precondition is dropped. Its steps ran as `cut-over-now` WP-2.
  - Row 8 (taking `x-scheme-handler/obsidian`) was in scope ("clippings should be captured by cv now").
- **Consequence:** Crucible's automation (YT/X enrichment, trackers, Daily Brief, image describe, link records) is dark until A2 lands; A1's own actions run live.
- **Premise correction:** `plans/v0-core-daemon.md` assumed a Wayland host with `wl-paste`. The host is X11, so the default clipboard command now follows the session (`xclip -o -selection clipboard` without `WAYLAND_DISPLAY`, else `wl-paste --no-newline`; `CV_CLIPBOARD_CMD` overrides).
- **Superseded by D24** for the cut-over marker: the guard it armed (I1) is retired.

## D20 — Settings tab over the vault config files (2026-10-14; reopens inventory #23, amends D18)

- **Decision (plan `editor-settings-tab`, approved 2026-10-14):** config stays TOML in the vault and the file is the source of truth. The editor's Settings tab (Ctrl+,) is a typed, comment-preserving editor over those files (`cv-schema` + `toml_edit`), never a second store; a key the schema does not know is left untouched.
- **`editor.toml`:** `_custom/crucible-vault/editor.toml` holds the editor prefs (font size, UI scale, surround, search limit/debounce, confirm-trash, palette hidden ids) and the keymap overrides (`[keys]`: command id → chords, `[]` unbinds and releases the default chord; an override replaces its command's default). Conflict policy (plan `editor-feedback-round-2` WP-1, replacing "override > editor default > config action `keys`"): assigning on the Hotkeys page a chord already held by another command or action is refused ("already assigned to …", nothing written), and a chord claimed by two or more bindables at load is registered for none, every claimant marked conflicted. Window and tab session state stays in the machine-local `UiState`.
- **Writes:** the daemon stays the only writer. Settings auto-save (plan `editor-feedback-round-2` WP-2, replacing the explicit Save): every edit that leaves the file valid is written after a short debounce (coalescing, at most one write in flight per file, a change during a write re-queues; pending writes flush on Settings tab close and quit); an invalid intermediate state is never written. Every settings write is `write_note` on the fixed path with CAS on the last-read hash, or the empty-hash exact-name absence-CAS when the file was absent — never `create_note` (no collision suffix). A conflict reloads and shows "changed on disk"; it never overwrites.
- **Validation is two-layer:** schema form checks (required, enum, range, mutual exclusion) block the automatic write (the page shows the errors; the file keeps its last valid content; plan `editor-feedback-round-2` WP-2); the daemon's post-reload `ConfigReloaded { errors }` is shown on the page of the file it names. No `validate_config` RPC.
- **Views carve-out:** views keep no last valid file and never reach `ConfigReloaded`, so the Views page runs the `views` crate's own parse + compile in the editor and blocks the automatic write on a parse error; compile diagnostics stay warnings and never block (plan `editor-feedback-round-2` WP-2).
- **Not carried (no cv feature behind them; each arrives with its feature, see Backlog):** ToC (`showToC`, `tocPosition`, `tocCollapseBehavior`), folder templates and move-picker pins, per-period enable toggles, global `excludedFolders`, search rerank/boosts/query log, `imageMetadataExtraction*`, destructive-confirm tiers.
- **Dropped as Obsidian-induced:** `lintOnSave`, the palette-replacement knobs and hint tuning, `hiddenFromChainSearch`.
- **Known limitation:** day/month navigation reads `captures.toml` `[periods.day|month].folder` but still requires ISO basenames (`YYYY-MM-DD` / `YYYY-MM`); a non-ISO `format` is not honoured by navigation.
- **Amendment (2026-10-15; plan `editor-ui-round-3` WP-3, audit M1):** the `editor.toml` `confirm-trash` pref (`confirm_trash`) is retired. Deleting a note (the explorer Trash button and the "Delete note…" command) always opens a confirm dialog listing the note's embeds and attachments, with orphans pre-ticked and items referenced elsewhere shown but not tickable. The new `delete_mode = "trash" | "permanent"` (default `"trash"`, today's `.trash/` move) picks how files are removed. A legacy `confirm_trash` key in `editor.toml` is tolerated and ignored.

## D21 — A2 automation pipelines (2026-10-16; plan `v0-automation-pipelines`)

- **Decision (plan `v0-automation-pipelines`, decisions locked by the maintainer 2026-10-15):** Crucible's automation returns as cv actions. Each pipeline is a step kind (`link_scan`, `yt_metadata`, `yt_channel_enrich`, `x_discover`, `x_metadata`, `x_backfill`, `image_describe`, `track_youtube`, `track_blogs`, `ignore_video`, `daily_brief`) run inside an action with a command, event or schedule starter (D3); no new job type.
- **Tracker intake → daily lines:** each new tracked item appends one line to that day's note through the `capture` step (`captures.toml` preset `tracker-intake`, first format `- [ ] [<title>](<url>) — <source>` under `# Intake`).
- **Full-body blogs → captured notes:** an automatable source's new posts are created as captured blog notes in `daily/day/<date>/` (shown by the Reading List, no checkbox line). Automatable = `body = "full"`, or `"auto"` with each of its last N (default 5) posts carrying a full body. `_blog_metadata` is no longer written.
- **Machine-data notes stay vault notes:** `_yt_metadata/**` and `_x_metadata/**` keep Crucible's paths and frontmatter.
- **Config:** TOML in `_custom/crucible-vault/` (D18): `pipelines.toml` (pipeline toggles, models, Ignore Shorts rule, scorer, queue retention) and `sources.toml` (the tracked-source registry, replacing Crucible's Markdown registry tables; `cv import sources` converts them). Both are schema'd for the Settings tab (D20). **Cadences live in `schedules.toml`** (maintainer, 2026-10-15): `pipelines.toml` has no schedule keys.
- **Clipper inbox:** the live Web Clipper writes to vault `Clippings/`; `[x] inbox` and the clip actions' folder selectors use it.
- **Importance scorer:** a trait with a stub (items stay unscored) and a decision-model HTTP client (`[scorer] url` + `model`). Its response shape `{"score": n}` is **provisional**: the separately developed model sidecar must implement it before the scorer is configured.
- **State:** link records, tracker items, ignored ids and API usage live in `state.db` v5 (D15); link records and ignored ids are imported once from Crucible (`cv import link-records|ignored`, daemon stopped).
- **Secrets:** `YOUTUBE_API_KEY` via `secrets.rs` (D4), never from the vault.

## D22 — In-process semantic search (2026-10-17; plan semsearch-crate)

- **Decision (plan `semsearch-crate`, approved 2026-10-17; supersedes D8's sidecar end state):**
  1. **Own crate:** `semsearch` in its own repo (https://github.com/dulrich/semsearch), this project's semantic-search implementation; cv links it as a path dep of `cv-search`.
  2. **SQLite stays:** one caller-pathed SQLite file (`busy_timeout`, WAL, `synchronous=NORMAL`), derived and rebuildable (D14 class).
  3. **Sync and transport-free:** `rusqlite` (bundled, FTS5) + `thiserror`; callers pass pre-chunked documents and f32 vectors. Chunking (Crucible-compatible) and embedding (`cv-providers`) stay in `cv-search`.
  4. **Embedding space lives in the index:** one model + dim; a write in another space is an error; queries embed in the recorded space; a space change is a rebuild. The serving id `bge-m3-f16` maps to the legacy companion space `bge-m3/f16`, so imported and fed vectors share one space.
  5. **Vector retrieval:** in-memory sign-bit codes, Hamming scan, pool (800) rescored with exact f32 dot product read from SQLite.
  6. **Ranking:** the companion's `coverage` mode (pooled bm25 title 10 / heading 5 / text 1 / entities 8, title-match, vector, term coverage; RRF k = 60); bakeoff modes and Node workarounds not ported.
  7. **The daemon is the only writer and reader:** one shared `Index` handle per process, used by the job runner (feed) and the request path; the editor searches through a daemon request.
  8. **The companion is retired from cv:** no `:4801` calls, no HTTP client or remote compare, shadow P3 removed; its chunks/embeddings come over once via `cv search import`; removing the `crucible-search` container is a manual step after soak.
- WP-4: editor search goes through `Request::Search`; migration 0007 imports the companion once.
- **Amendment (2026-10-19, plan tn-review-baseline-remediation WP-R7):** the JS-compat layer is dropped — the chunker and file preparation run on native `str` (char lengths, UTF-8 FNV hash), `cv-search`'s `js.rs`/`import.rs` and the real `cv search import` are deleted (the verb is an exit-0 stub until WP-R24 removes migration 0007). Chunk boundaries/ids change for non-ASCII notes, accepted as one re-embed at deploy via a one-time deploy migration, since folded into the baseline (it moved `search.db` aside, rewound the `search-refresh` schedule). Point 8's import no longer exists.

## D23 — Migration baseline 0.1.0 (2026-10-19; plan tn-review-baseline-remediation WP-R24)

- **Decision:** with every cut-over migration applied on this machine (deploy `0001`–`0011`, state.db at v10), the history collapses into a 0.1.0 baseline. The workspace version is `0.1.0` in `[workspace.package]`, inherited by every member (`Cargo.lock` unchanged — every member already was 0.1.0).
- **state.db:** the v1→v10 chain is replaced by one baseline (`cv_state::migrate::BASELINE`, `BASELINE_VERSION = 10`) that creates the exact v10 schema on a fresh DB (pinned by `crates/cv-state/tests/fixtures/baseline-v10-schema.txt`, dumped from the retired chain). A DB at v10 opens with no write; a DB at v1–v9 is refused (`StateError::PreBaseline`, "predates the 0.1.0 baseline"); a newer DB is refused as before. Post-baseline steps append as v11+ in `MIGRATIONS`. `index.db` (no chain; mismatch → rebuild) and `search.db` are derived and unaffected.
- **Deploy migrations:** `deploy/migrations/0001`–`0011` and their `crates/cv/tests/migration_*` tests are deleted, with the `cv search import` stub only 0007 used. Numbering restarts at `0001` after the baseline; a new id must not reuse a pre-baseline id (ledger keys are the full `<NNNN>-<slug>`). `install.sh` already ignores ledger lines for files that no longer exist.
- **Superseded by D24:** state.db moves to v11 and the 0.1.1 baseline replaces this one.

## D24 — Cut-over guard retired; 0.1.1 baseline (2026-10-19; plan tn-review-residue-cleanup)

- **Decision:** I1 (the protected live-vault root plus the state.db cut-over marker) is retired. The configured vault (`CV_VAULT`) is the live vault; `CV_PROTECTED_VAULT` is gone and ignored if set. `VaultFs::live` takes only the root; writes through symlinked notes stay confined to the vault root and `CV_LINK_WRITE_ROOTS`, and `REFUSED` (1005) stays the policy-refusal code.
- **Test safety:** live-writer test fixtures (`cv_vault::test_support::LiveFixture`, the cv-vault integration fixtures, the cv daemon test harnesses) assert their vault root lies under `std::env::temp_dir()`, panicking otherwise. This replaces I1 as the guard that keeps tests off the live vault.
- **state.db v11:** one `MIGRATIONS` step drops `shadow_writes` (with `shadow_writes_at`), `cutover_marker` and `tracker_items.done_ms`. The daemon applies it at startup through `migrate()`; there is no deploy script.
- **0.1.1 baseline:** WP-X6 folded the step into the baseline: `BASELINE_VERSION` is 11, `MIGRATIONS` is empty, v1–v10 are refused with `PreBaseline` (0.1.1), and the workspace version is 0.1.1. The one-time legacy search-space rebuild (`bge-m3/f16` → `bge-m3-f16`) ran live and its code is deleted.

## D25 — Auto-intake: Maildir drop, supersede in place, auto-done (2026-10-20; plan auto-intake-sprint)

- **Decision:** one supersede engine serves every intake path (tracker, mail drop, clips); completeness is decided per delivered body; complete items are recorded done.
  1. **Maildir drop (maintainer 2026-10-20):** newsletter mail reaches cv as files in a Maildir outside the vault, filled by an external fetcher or by hand (`docs/mail-intake.md`). cv parses with `mail-parser` and never speaks IMAP/SMTP; mail logic stays out of cv and email-only newsletters become possible.
  2. **Supersede in place:** an incoming item resolves to an existing note by canonical URL (`cv_text::url_canon` on both sides), then by content containment (not symmetric similarity: a stub is a prefix of the full text) among same-source notes within `[supersede] window_days`, date equality never required. A fuller body replaces the body in place (path, links, `read`, user properties kept); the old body is saved first and a failed save aborts the replace. An item is merged into, and then removes, a file only when the target contains its text; otherwise both stay and the newcomer is marked `duplicate-of`. The caller's own path is excluded from every candidate set, so a sole clip resolves `new`, never itself.
  3. **Auto-done predicate:** a `note` outcome is recorded `status = 'x'` only when the *resolved* body is complete. Every path that ticks (record, supersede, clip merge, reconcile) first writes that body's `access`/`complete`/word counts to the tracker row and then applies the predicate; a match or a replacement alone never ticks. Paid or partial notes stay open until a fuller version supersedes them.
  4. **Audience is not completeness:** `access` is the publication's audience (Substack `/api/v1/posts/<slug>` probe, quota-bounded under the `substack-api` service); `complete` belongs to a delivered body: its word count against the probed full count (`[blogs] complete_percent`), else the audience rule for RSS, else "complete unless a paywall marker matches" for an email or clip. A failed probe is `unknown` and leaves the item open.
  5. **`intake::identity` boundary:** every engine caller (`track_blogs`, `track_mail`, `auto_file`, `intake_reconcile`) holds this one queue resource key from resolve through create/record, added by the step itself (`StepKind::intrinsic_resource_keys`), not by user config. No two intake jobs resolve and commit concurrently.
  6. **E1 child ordering (audit):** `clip-auto-file` has no event starter. Independent `created` starters would each enqueue, so filing could run before `localize`. It runs only as the child spawned by the last step of `clip-images`, after `localize`; a breaker-deferred `image-describe-note` is the documented residual.
  7. **Reconcile:** `cv intake reconcile [--dry-run]` (RPC `reconcile_intake`, action `intake-reconcile`) links NULL-`note_path` `note` rows by canonical identity, probes open blog rows and applies the predicate; aggregate counts only. The first run is the deploy migration `0001-auto-intake-config` (`migrate_running`).
- **Amended (plan intake-followups-mail-youtube WP-5, 2026-10-21):** items 3 and 7 above stand; two paths join them, and evidence-before-tick holds on each.
  - Item 3, new ticking paths: the `intake_finalize` step (in `file-as-kind` after `upsert_tags`, before `lint`) and the widened reconcile. A YouTube row ticks only when the filed note's own `source` (or `yt-video-id`) is that video **and** the body carries transcript text (`cv_note::transcript_evidence`); a note that merely embeds the video never ticks (maintainer decision, 2026-10-21). Blog and mail rows tick through the same finalize write as the supersede engine, on the note's `complete` or the completeness predicate, evidence first.
  - Item 7, the reconcile now also finalizes open YouTube rows, re-matches `unknown-sender` mail rows against the newsletter config (re-key with collision merge, kind and extra tags, tick by predicate; never a new note and never a move) and re-cleans `via: email` bodies (CAS write, skipped on conflict). `intake_reconcile` therefore writes notes: `StepKind::IntakeReconcile` is note-mutating (audit M2). The deploy migration is `0002-intake-followups`.
- **Amended (plan intake-round-2-clean-dedupe-reading WP-6, 2026-10-21):** items 2, 3 and 7 stand; the reconcile gains merge-on-reconcile. A `via: email` note re-keyed to its canonical identity that now matches a `via: rss` note is resolved against it by the supersede engine, **mail and RSS twins only** (a match with a clip or any other note is counted and left untouched, audit M2). The containment rule of item 2 holds: the mail note moves to `.trash/` only when the RSS note was replaced by its fuller body, or was kept and already contains the mail body (`cv_text::similar::containment` at or above `threshold_percent`, the `auto_file` branches); otherwise both stay and the mail note is marked `duplicate-of` (audit B2). Item 3 holds for the survivor: before the trash every tracker row of the mail note is re-pointed to it (audit M1), then its rows are finalized on the resolved body, so an interrupted run never leaves a row pointing at a missing note and a rerun is a no-op. `publication` is backfilled on linked notes where absent. The deploy migration is `0003-intake-round-2`.
- **Amended (plan round-9-feedback-todos WP-2, 2026-10-22):** item 2's canonical URL gains the forum post identity rule: a thread-page URL whose fragment is `post-<P>`, or a `/posts/<P>` path, keys as `https://<host>/posts/<P>` in both `canonicalize_url` and `post_id_from_url` (host-agnostic XenForo shape), so chapters on one thread page no longer share an identity. The reconcile re-keys a stored page-level `post-id` and removes a `duplicate-of` only when the pair shared the obsolete page key, shares no identity after the re-key and has no content match (other `duplicate-of` markers stand). A pre-D25 RSS note (no `via`) with a `blog` tracker row counts as RSS-origin for the mail/RSS merge; fanfic notes take the matched Work's `title` as `publication`. The deploy migration is `0004-round-9-identity-repair`.
- **Amended (plan round-10-feedback-todos WP-3, 2026-10-22):** human-read completion: a note's `read` turning `true` ticks every open tracker row linked to it, any source kind, regardless of completeness evidence (the row's `access`/`complete` stay as recorded); one-way, and a flip that reverts within the watcher's coalescing window is no transition. Item 3's predicate still governs every automatic tick.
- **Amended (plan round-10-feedback-todos WP-2, 2026-10-22):** per-sender boilerplate: lines recurring across >= 2 messages of one newsletter (`source_key`) are learned into `state.db` v13 `mail_boilerplate` (once per Message-ID, from pre-strip cleaned bodies only, never from stripped notes) and stripped from the anchored head/tail of new mail before words and completeness are taken, with a half-body guard; unknown-sender mail is neither learned nor stripped; item 7's reconcile gains a boilerplate phase after the re-clean (seed from the processed Maildir, then CAS strip of known-sender email notes with `description` recomputed; `bp_*` counts); deploy migration 0005 re-runs the reconcile.
- **Out of scope:** a generic "is this truncated" heuristic for non-Substack platforms; the other `FEEDBACK.md` todos.
- **Evidence:** `docs/spikes/auto-intake/FINDINGS.md`.

## D26 — Auto-lint in the editor at autosave (2026-10-22; plan round-9-feedback-todos WP-6; amends D20)

- **Decision (maintainer 2026-10-22):** the editor lints the open note at autosave, on by default, amending D20's drop of `lintOnSave`. `editor.toml` `auto_lint` (bool, default `true`, a Settings row on the Editor page) turns it off.
- **Why now:** `lintOnSave` was dropped as Obsidian-induced — it fought the stale metadata cache (the `updateFrontmatter` write barrier) and a TypeScript lint engine run per save. Both constraints are gone: `cv_note::lint_note` is a pure function measured at p50 133 µs, max 1.9 ms per note, 1.42 s vault-wide (9,553 notes). Lint is almost never a no-op (`updated: today`, word count), so a daemon-side lint after the editor's 1 s autosave would reload or conflict the open buffer; linting the buffer itself before the save avoids that write race, and the daemon stays the only writer (the edit goes out through the existing CAS save).
- **Scope rules:** it runs only at the autosave point (not on tab switch or unfocus flushes), only for a note the user edited in this session (dirty, history non-empty, nothing undone, no conflict — never on open or reload), never while the cursor or selection touches the frontmatter, never for `_custom/` or a folder of a `captures.toml` template path (a root-level template exempts only itself), and never for a note without frontmatter. It applies the frontmatter span only, as one edit; a body change lint would make (blank line after YAML aside) is ignored. Body cursor and selection shift by the frontmatter line delta.
- **Undo:** the lint edit joins the user's preceding undo group (`UndoHistory::record_joined`), so one Ctrl+Z reverts the user edit and the lint together and redo restores both; it never joins under a pending redo.
- **Settings:** the same as the `lint-note` action, read from the loaded `actions.toml` step params through the one parser `cv_note::LintSettings::from_params` (value-neutral `ParamValue`; the daemon step and the editor each adapt their `toml` values). Malformed params skip auto-lint rather than lint with settings the user did not write.

## D27 — Migration baseline 0.2.0 (2026-10-27; plan tn-review-2026-10-26-remediation WP-R9)

- **Decision:** with every 0.1.1-era migration applied on this machine (deploy `0001`–`0017`, state.db at v15), the history collapses into a 0.2.0 baseline, following D24's shape. The workspace version is `0.2.0` in `[workspace.package]`, inherited by every member (`Cargo.lock` changes only the members' `version` lines).
- **Version rationale:** a minor bump because this fold spans a feature era (17 deploy migrations, four schema steps v12–v15); the patch level stays for fold-only baselines such as 0.1.1.
- **state.db v15:** `cv_state::migrate::BASELINE` creates the exact v15 schema on a fresh DB (pinned by `crates/cv-state/tests/fixtures/baseline-v15-schema.txt`, dumped from the retired v11 baseline + v12–v15 chain). `BASELINE_VERSION = 15`, `MIGRATIONS` is empty (the machinery stays; later steps append). A DB at v15 opens with no write; v1–v14 are refused with `StateError::PreBaseline` ("predates the 0.2.0 baseline"); a newer DB is refused with `NewerSchema`.
- **Deploy migrations:** `deploy/migrations/0001`–`0017`, their `test-NNNN.sh` harnesses, the `fixtures/` tree and `crates/cv/tests/migration_000{1,2,3}.rs` are deleted; the two Rust tests that read fixtures (`migration_0007_output_parses`, `migrated_reading_list_has_an_unread_row`) inline them as byte-identical constants. The per-migration carve-outs in `deploy/migrations/README.md` are retired with them (the rules stay). **Numbering continues at `0018`** rather than restarting at `0001`, so the install ledger never shows a third `0001-*` id. `install.sh` already ignores ledger lines for files that no longer exist.

## D28 — Whole-vault remote media (2026-10-29; plan remote-media-sprint)

- **Decision 1 (unfetchable images):** a remote image embed whose download fails permanently (HTTP 4xx, or a page / non-image response) is rewritten to a plain link `[🖼 <alt or host>](<url>)`; transient failures (unreachable, timeout, 429/5xx) stay remote and are retried.
- **Decision 2 (video):** a remote video becomes a linked local thumbnail card `[![](<thumb>_MD5.jpg)](<video url>)` (the thumbnail is localized and described); with no thumbnail it becomes a link card `[▶ video · <host>](<url>)`; X posts become `[𝕏 post](<url>)`, other iframes `[⧉ embed · <host>](<url>)`. The editor draws a ▶ badge on a linked image whose href is a video.
- **Decision 3 (audio):** `[remote_media] audio = "link" | "localize"`, default `link`; local audio and video embeds render as a media card that opens the file in the system player.
- **Decision 4 (settings):** one `vault.toml [remote_media]` table and a "Remote content" Settings page; every `localize` step reads it as its defaults and an explicit step param overrides it.
- **Decision 5 (scope):** the whole vault — every `.md` outside `_custom/`, `.trash/` and `.git/` (any dot-dir), root-level notes and the machine `_*` folders included. This supersedes the 2026-10-16 "machine-written `_*` folders are not localized" choice. `localize` gains `scope = "vault"`; because the event-starter model has no vault-wide selector (an empty selector never arms), `localize-on-edit` gets one starter per added top-level folder (`_crucible`, `_raw_logs`, `_raw_transcript`, `_resources`). Fenced code is code: its refs are neither converted nor counted.
- **Decision 6 (checkable):** the read-only `cv media audit [--strict]` counts remote image embeds, unconverted video/post/iframe embeds, audio/pdf left in a mode that forbids them, unmanaged (non-`_MD5`) local image refs, and undescribed `_MD5` images, by kind and top-level folder (counts only, no note text or paths). Images whose describe failed permanently under a cv provider (e.g. AVIFs `prepare_image` cannot decode) are a reported exception, listed by md5 with `--verbose`, not counted as undescribed.
- **Re-evaluation:** the backfill's `ignored_ids` kind is versioned, `localize_backfill` becoming `localize_media_backfill`: old rows are never consulted, so every previously settled note gets one run under the new parser and modes, and notes settled under the new kind are never re-selected (the audit-M2 progress rule). Backfill candidates also include notes that reference an unmanaged local image (audit M1), so `process_local` normalizes them and the describe chain picks them up.
- **Describe:** `image-describe-backfill` runs hourly (100 images, ~7.5 min) and after each vault sweep; a `failed` record with no `providerId` (Crucible-era) is described once more, and its re-failure carries provenance and is then poison-skipped, as cv-era failures always were.
- **Deploy:** migration `0023-remote-media` adds `[remote_media]`, the `localize-vault-sweep` action, the `_*` starters and the schedules (the vault sweep replaces the daily folder sweep's schedule, which stays as a manual action).

## D29 — Intake auto-enrich (2026-10-30; plan intake-auto-enrich)

- **Decision 1 (fold, never delete):** every span the cleanup marks, and every recurring banner (WP-5), moves into one collapsed `> [!removed]-` callout at the end of the note, delimited by `%% cv-enrich %%` markers. Nothing leaves the note, so a wrong call is reversible by hand; a fold that would move more than half the body folds nothing and is reported.
- **Decision 2 (anchors, not rewrites):** the model returns JSON with verbatim anchors (quotes, chapter starts, span first/last lines) and never rewrites body text. `cv_ingest::enrich` resolves anchors against the body under whitespace squeezing and applies only deterministic edits (fold, top block, chapter headings in heading-less bodies); an anchor that does not resolve is dropped and counted, never fuzzy-applied.
- **Decision 3 (model):** the `gpt-6.1-terra` model at the family default (low) effort through an OpenAI-compatible endpoint, one combined structure + cleanup call per note (halves the ~10k per-call sidecar overhead). Medium is indistinguishable from low on the spike corpus, so there is no effort knob. The route is `agents.toml [intake-enrich]`; repointing it needs no code change.
- **Decision 4 (scores out):** absolute 1-5 scores do not discriminate (density 4 on every document), so none are requested; relevance stays with the D5 importance model. GLiNER2 entity extraction (127-305 noisy entities per document) is not adopted; entities, claims and chapters render as plain text in the top callout (no wikilinks, no entity notes).
- **Decision 5 (when it runs):** every filed clip (`intake_enrich` is a step of `clip-enrich`, whose chain is already at `MAX_CHAIN_DEPTH`; D30 and plan youtube-transcript-auto-refine reorder its tail to `transcript_refine` -> `intake_enrich` -> `search_upsert`, i.e. refine, then enrich, then the search refresh, via migration `0027`) and every new tracker blog / mail note (`track_blogs` / `track_mail` enqueue an `intake-enrich` child once the note is final: a create event would race the inline localize and lint). All enrichment shares the `clip::enrich` resource key, preserving the spare queue slot. The `enriched` frontmatter value makes a second pass a skip; `force` re-runs. Bodies under 150 words are folded but get no top block. `intake-enrich-backfill` enqueues the newest unenriched intake notes, plus (D30) YouTube clips with an unrefined transcript whether enriched or not (a `limit` batch, run it again for more).
- **Evidence:** `docs/spikes/auto-intake-llm/FINDINGS.md` (20/20 valid JSON, anchors resolved 108/110, medium == low, 31 images recurring across >=3 notes). Deploy: migration `0026-intake-enrich`.
- **Amendment (plan round-19-yt-cleanup-intake):** the deterministic YouTube thumbnail-embed + clipped-description deletion (`cv_note::strip_youtube_preamble`, the `yt_clip_tidy` step and the fixed `cleanup_youtube_transcript`; the description stays in the linked `_yt_metadata` note and frontmatter `description`) is the one exception to Decision 1; LLM-marked spans and banners keep folding.

## D30 — YouTube transcript auto-refine (2026-10-30; plan youtube-transcript-auto-refine)

- **Decision 1 (refine before enrich):** the `transcript_refine` step runs before `intake_enrich` in the same job; when it rewrote the note, `intake_enrich` re-runs as forced (`transcript_refined` outcome var ORed into `force`, surviving checkpoint replay) so the top block, ToC and fold are built on the refined text.
- **Decision 2 (section-only rewrite):** only the `## Transcript` section (heading line to the next `#`/`##` heading or end of body) is sent and replaced; the embed, description and every frontmatter key other than `tags` (`refined`), `refine-model` and `raw-transcript` are untouched. This differs from the manual `refine-transcript` action, which replaces the whole body. The same conventions apply: the original note is copied to `_raw_transcript/<name>.md`, `raw-transcript` links it, the note gets the `refined` tag (the manual action's `not_tag = "refined"` guard keeps working, and a refined note is skipped with no model call).
- **Decision 3 (`###` chapters):** chapter headings are `### ` so the section boundary and the note's own `##` structure survive; the enrich pass links the existing headings in its ToC instead of inserting its own.
- **Decision 4 (no invented speakers):** speaker labels appear only where the transcript itself names speakers; auto-captions carry no turn cues, so they get none (the spike prompt's `**Host:**`/`**Guest:**` fallback is dropped).
- **Decision 5 (managed blocks preserved):** `%% cv-enrich %%` … `%% /cv-enrich %%` blocks (including the trailing `[!removed]-` fold that sits inside an apparent last-section range) and `%% cv-enrich heading %%` lines are never sent to the model nor replaced; replacement keeps them byte-identical in place (D29 decision 1).
- **Decision 6 (validation, chunking):** sections over 12,000 words split at blank-line boundaries into roughly equal chunks of at most 12,000 words, one call each, concatenated in order. Every chunk must keep at least 0.6 of its input words (spike 0.79–0.94) before anything is written; a provider error, `max_tokens` stop, empty output or a short answer fails the step with the note and `_raw_transcript/` untouched (retriable). The hash is taken before the model call and the write is a CAS (no editor hold for custom steps).
- **Decision 7 (short transcripts skipped, 2026-10-30 post-deploy):** a transcript section under `MIN_REFINE_WORDS` (300) words is skipped as `"short transcript"` and is not refine-pending for the backfill; a transcript that short (a YouTube short) has little to clean, and enrich still runs on it. The first live backfill's 7-word answers were not a length effect: the shipped prompt lacked its `{{value}}` placeholder, so no transcript reached the model; migration `0028` appends it, and a test now requires every shipped prompt to carry its input.
- **Evidence:** `docs/spikes/transcript-refine/FINDINGS.md` (the model refines a 2.7k–12.8k-word transcript in one call, 18–94 s, `end_turn` every time, retention 0.79–0.94, 6–18 chapters). Wiring (migration `0027`, backfill selection) is plan WP-2.

## D31 — Per-major state.db baseline; 1.0.0 (2026-11-08; plan public-release WP-7)

- **Rule:** structural state.db migrations collapse into one baseline per major version. Within a major, a schema change is a ladder step appended to `cv_state::migrate::MIGRATIONS` (entry `i` moves `BASELINE_VERSION + i` to `BASELINE_VERSION + i + 1`; never edited once shipped). The next major folds the ladder into a new `BASELINE` and raises `BASELINE_VERSION`. `index.db` and `search.db` stay derived: on a schema mismatch they are rebuilt, never migrated. state.db is primary data (D14), so there is still no down-migration and no drop-and-rebuild.
- **1.0.0 baseline:** `BASELINE_VERSION = 17`; `BASELINE` is the v15 baseline with the v16 (job telemetry) and v17 (path lineage, including the `path_lineage_floor` seed row) steps folded in, byte-for-byte the `sqlite_master` the old chain produced (fixture `crates/cv-state/tests/fixtures/baseline-v17-schema.txt`, replacing the v15 one). `MIGRATIONS` is empty; the machinery stays so 1.x can append.
- **Refusals:** versions `1..17` are refused with `StateError::PreBaseline` ("predates the 1.0.0 baseline"); a newer database is refused with `NewerSchema`. A database already at v17 opens with no schema write.
- **Version:** the workspace version is `1.0.0` in `[workspace.package]`, inherited by every member (`Cargo.lock` changes only the members' `version` lines).
