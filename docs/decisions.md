# Decisions — crucible-vault

Settled with the user in the feature-planning stream. Questions come from `docs/crucible-feature-inventory.md` §4. One entry per decision: date, question, decision, rationale. Superseding a decision appends a new entry, never rewrites an old one.

## D1 — Architecture shape (2026-10-13)

- **Question:** How are core and editor split?
- **Decision:** A headless **core daemon** is the sole vault writer, action runner, durable queue and `obsidian://` intake. The **editor GUI is a client** of it.
- **Rationale:** Filing, clipper intake and automation keep running with the editor closed. This removes the "only while Obsidian is open" limitation and Crucible's editor-buffer reconciliation (inventory §3).

## D2 — Tier C scope (2026-10-13)

- **Question:** Which tier C features stay in parity scope?
- **Decision:** In scope: **trackers** (YouTube/Blogs/Feed + channel enrich), **image describe for new clippings**, **ingestion dashboard**. Source-Eval *as a dashboard* was not selected — but see D2a.
- **User direction (verbatim intent):** "Tracking and ingestion should become mostly automatic into the daily captures, an eval model will be added and trained to classify based on (read, tags, monthly captures) what is important. The goal here will be reducing Email -> Open Link -> Web Clipper dependence, a later feature will allow MCP or another email automation to capture email-only newsletter. Daily/Monthly possibly become structured views rather than arbitrary notes, depending on complexity and features."
- **Rationale:** Trackers move from rarely run to the main intake path. The clipper stays as a fallback, not the primary route.

## D2a — Importance model lineage (2026-10-13, inference — confirm)

- Crucible's `src/sourceEval/` already collects the exact signals named in D2: `read`, tags, `eval-importance`/`eval-urgent`/`eval-rated`/`eval-tags` labels (`signals.ts` `parseEvalLabel`), and monthly `# Observations` quote counts (`scanObservationSignals`), plus a training-data export (`export.ts`). The importance classifier is therefore Source-Eval's successor; its signal collection is the part to carry forward. Pending user confirmation (Q-round 2).

## D3 — Action model (2026-10-13)

- **Question:** How are chains, captures, agents and triggers unified?
- **Decision:** An **action = a list of typed steps**. It starts from a **key/command, an event, or a schedule**; durable actions run on the queue. The 10 "Ingest as …" chains collapse to one file-as-kind action plus a kinds table. The 4 "Refine Transcript" chains collapse to one action with a provider parameter. "Ignore Shorts" becomes an event-started action.
- **Rationale:** Replaces Crucible's chain/internal-command/trigger registries, which are Obsidian's command model (inventory §3).

## D4 — Config location (2026-10-13)

- **Question:** Where do action/kind/agent/provider definitions live?
- **Decision:** Declarative files under the vault's **`_custom/crucible-vault/`**, hot-reloaded by the daemon. Secrets live in env or `~/.config`, never in the vault.
- **Rationale:** The files travel with the vault, are reviewable by agents (`_custom/` is the vault-repo opacity exception) and are diffable. This replaces Crucible's ≈7k-line settings UI.

## D5 — Importance model placement (2026-10-13; supersedes the D2a phasing guess)

- **Question:** How is the importance model phased?
- **Decision:** crucible-vault **keeps the signals** — Source-Eval's label and signal collection (`read`, tags, `eval-*` labels, monthly `# Observations` quotes) and the training export — as index data and queries. **The model itself is not built here.** It is developed in `../eval-tool/` and runs as a **sidecar**: likely mmBERT-based (following the agent-turn-disposition classifier's success), possibly a fine-tuned decision LLM. The feature calls it through a **SystemOne-shaped API**, either inference-server's router `POST /v1/systemone` (`../inference-server/router/AGENTS.md`, patch 0013) or a dedicated sidecar (cf. the hsc classifier's `POST /v1/classify`).
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
- **Decision:** **Leave Crucible's search companion as-is** (`https://github.com/dulrich/obsidian-crucible/blob/master/scripts/search-companion/`, Docker, SQLite FTS5 + brute-force vectors + RRF), fed by the daemon, **unless its shape forces hacky workarounds in the daemon**. That is the integration-friction trigger for an own-vs-depend review. **End state:** a Rust search sidecar daemon alongside the main daemon. That is deferred, including whether SQLite stays its engine.
- **Superseded (2026-10-17):** D22 replaces the "sidecar" end state with in-process search (the `semsearch` crate).

## D9 — Editor ambition for v0 (2026-10-13)

- **Decision:** **Source + rendered reading pane** (toggle or split), syntax-highlighted source editing. Live preview is a later plan.
- **Rationale:** About 4–7 kSLOC native, against 10–20 kSLOC for live preview. This also keeps the GUI-stack choice from hinging on CM6's live preview.

## D10 — Bases → native views (2026-10-13)

- **Decision:** **Replace `.base` with daemon-native views** defined in `_custom/crucible-vault/` config, served from the index. The 6 `.base` files (14 table views) and their 231 embeds migrate. Obsidian rendering of those embeds is accepted as degraded during shadow (D11), because Obsidian stays the editor until cut-over and the `.base` files remain in place until then.
- **Restore what the Dataview→Bases migration dropped** (`../vault-repo/plans/dataview-to-bases.md`, user decisions 2026-10-12 — that migration was a precursor to this one). Bases' limits were no JS, no tasks, no cross-file aggregation, and `file.backlinks` whole-vault timing out (108 s+). The native views restore:
  - monthly reading-by-tag **day × category pivot** and the **Average/day** row (decision 4 there);
  - Sample Charts initiative overview **task counts** (decision 5);
  - month-note incomplete tasks with true **overdue semantics** ("before today"; core `query` couldn't express it — audit E3);
  - Works **unread reading list** as a real cross-file query, replacing the generated literal filter (decision 6);
  - **backlinks** as an index query (the owned link graph makes it cheap).
- Deliberately retired there and **not** restored: `folder-rules`, the admin-network tables, the personalization table, the whole-vault task-count table. `orphans.js` and `transcript-backlog.js` are covered by the ingestion dashboard (D2).
- **Nav bars:** Note Toolbar's `{{js:}}` day/month nav becomes native editor chrome (inventory #2).

## D11 — Transition / cut-over (2026-10-13)

- **Decision:** **Shadow, then one switch.** Until v0 parity, the daemon runs against the live vault **read-only**: it indexes and logs the writes it would make, with real writes only to a scratch copy outside every repo. At cut-over, with the user's explicit go-ahead, it takes `x-scheme-handler/obsidian` and Crucible's automation is disabled in one step. There is never a dual-writer period.
- **Implication:** v0 needs a shadow/dry-run mode in the action runner, plus parity criteria the user can check before the switch.
- **Amendment (2026-10-19):** shadow mode is retired (plan `tn-review-baseline-remediation` WP-R6): the daemon has one (live) mode, `CV_MODE` is gone, and the protected-root + cut-over-marker guard (I1) is what keeps test and scratch runs off the live vault.

## D12 — OFM tokenizer reuse (2026-10-13)

- **Decision:** **Extract a shared pure `ofm` crate** from crucible-tui's `mdview` parse layer (wiki-links, frontmatter, lex). Both repos path-depend on it. The extraction is a **separate plan in `../crucible-tui/`**; crucible-vault's plan sequences on it.
- **Rationale:** One tokenizer, with no terminal or GUI deps in the core. This honours the never-copy-paste-fork rule (AGENTS.md → Stack).

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

- **Decision:** Link records live in `state.db`. **One-time import** from Crucible's `jobs.sqlite` `link_records` table (25,051 rows; `https://github.com/dulrich/obsidian-crucible/blob/master/plans/link-registry-out-of-vault.md`) at cut-over, keeping enrichment (`yt-video-id`, `x-status-id`) and tracker state. After that, link scan keeps them current with idempotent upserts (carried from Crucible's no-op-on-equal rule).
- **Rationale:** Rescanning would burn YT/X API quota, and trackers (D6) start consuming `tracked_source` state.

## D16 — Plan split (2026-10-13)

- **Decision:** v0 lands as three plans, in order:
  - **(0)** a `../crucible-tui/` plan that extracts a pure `ofm` crate (D12; S2's spec: frontmatter split + YAML→value, comrak options, an OFM walk with byte spans for wikilinks/embeds/md links/tags/footnotes/callouts/block ids, no glyph tier, no syntect/rustix);
  - **(A)** `plans/v0-core-daemon.md` — headless daemon: index, action model, queue, providers/agents, intake (URI + trackers → daily captures), X/YT, daily brief, link records, lint/localize, signals store, shadow mode;
  - **(B)** `plans/v0-editor.md` — the iced editor client: source + reading pane, shell, native views (D10), day/month nav.
- Cut-over (D11) follows (B). The user keeps editing in Obsidian throughout (A)'s shadow period.

## D17 — Visibility (2026-10-13; assumed, not asked)

- The repo stays **private** (`initiative-visibility: private`) and is never published. Revisit only if the user raises it.

## D18 — Config format (2026-10-13)

- **Decision:** every crucible-vault config file under the vault's `_custom/crucible-vault/` is **TOML** — actions, kinds, captures, agents, providers, schedules, plus plan B's views and bookmarks and A2's tracker/pipeline config.
- **Rationale:** serde-native, comments allowed, no YAML typing traps; proposed in `plans/v0-core-daemon.md` Decisions locked and confirmed by the user at approval time.

## D19 — Cut over before A2; D11 exceptions (2026-10-14; supersedes parts of D11, D16)

- **Decision:** the switch to live ran on 2026-10-14 (plan `cut-over-now`), before A2 (`v0-automation-pipelines`), at the user's request after Obsidian segfaulted three days running. D11's invariant is unchanged: one switch, no dual-writer period, no automation carried across modes.
- **Exceptions (user, 2026-10-14):**
  - The ≥ 7-day soak (cut-over row 1) is waived. One parity run is kept for the record, not as a gate.
  - Row 4 is satisfied by Obsidian being dead and not running ("Obsidian is crashed, treat it as dead").
  - The P3 search-parity gap (row 9) is accepted as an exception, not a hold: 0.968 match after reconcile, 0 unexplained.
  - `v0-editor` WP-9's A2 precondition is dropped. Its steps ran as `cut-over-now` WP-2.
  - Row 8 (taking `x-scheme-handler/obsidian`) was in scope ("clippings should be captured by cv now").
- **Consequence:** Crucible's automation (YT/X enrichment, trackers, Daily Brief, image describe, link records) is dark until A2 lands; A1's own actions run live.
- **Premise correction:** `plans/v0-core-daemon.md` assumed a Wayland host with `wl-paste`. The host is X11, so the default clipboard command now follows the session (`xclip -o -selection clipboard` without `WAYLAND_DISPLAY`, else `wl-paste --no-newline`; `CV_CLIPBOARD_CMD` overrides).

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

- **Decision (plan `v0-automation-pipelines`, decisions locked by the user 2026-10-15):** Crucible's automation returns as cv actions. Each pipeline is a step kind (`link_scan`, `yt_metadata`, `yt_channel_enrich`, `x_discover`, `x_metadata`, `x_backfill`, `image_describe`, `track_youtube`, `track_blogs`, `ignore_video`, `daily_brief`) run inside an action with a command, event or schedule starter (D3); no new job type.
- **Tracker intake → daily lines:** each new tracked item appends one line to that day's note through the `capture` step (`captures.toml` preset `tracker-intake`, first format `- [ ] [<title>](<url>) — <source>` under `# Intake`).
- **Full-body blogs → captured notes:** an automatable source's new posts are created as captured blog notes in `daily/day/<date>/` (shown by the Reading List, no checkbox line). Automatable = `body = "full"`, or `"auto"` with each of its last N (default 5) posts carrying a full body. `_blog_metadata` is no longer written.
- **Machine-data notes stay vault notes:** `_yt_metadata/**` and `_x_metadata/**` keep Crucible's paths and frontmatter.
- **Config:** TOML in `_custom/crucible-vault/` (D18): `pipelines.toml` (pipeline toggles, models, Ignore Shorts rule, scorer, queue retention) and `sources.toml` (the tracked-source registry, replacing Crucible's Markdown registry tables; `cv import sources` converts them). Both are schema'd for the Settings tab (D20). **Cadences live in `schedules.toml`** (user, 2026-10-15): `pipelines.toml` has no schedule keys.
- **Clipper inbox:** the live Web Clipper writes to vault `Clippings/`; `[x] inbox` and the clip actions' folder selectors use it.
- **Importance scorer:** a trait with a stub (items stay unscored) and a SystemOne-shaped HTTP client (`[scorer] url` + `model`). Its response shape `{"score": n}` is **provisional**: `../eval-tool/` must implement it before the scorer is configured.
- **State:** link records, tracker items, ignored ids and API usage live in `state.db` v5 (D15); link records and ignored ids are imported once from Crucible (`cv import link-records|ignored`, daemon stopped).
- **Secrets:** `YOUTUBE_API_KEY` via `secrets.rs` (D4), never from the vault.

## D22 — In-process semantic search (2026-10-17; plan semsearch-crate)

- **Decision (plan `semsearch-crate`, approved 2026-10-17; supersedes D8's sidecar end state):**
  1. **Own crate:** `semsearch` in its own repo (`../semsearch`), the fleet's only semantic-search implementation; cv links it as a path dep of `cv-search`.
  2. **SQLite stays:** one caller-pathed SQLite file (`busy_timeout`, WAL, `synchronous=NORMAL`), derived and rebuildable (D14 class).
  3. **Sync and transport-free:** `rusqlite` (bundled, FTS5) + `thiserror`; callers pass pre-chunked documents and f32 vectors. Chunking (Crucible-compatible) and embedding (`cv-providers`) stay in `cv-search`.
  4. **Embedding space lives in the index:** one model + dim; a write in another space is an error; queries embed in the recorded space; a space change is a rebuild. The serving id `bge-m3-f16` maps to the legacy companion space `bge-m3/f16`, so imported and fed vectors share one space.
  5. **Vector retrieval:** in-memory sign-bit codes, Hamming scan, pool (800) rescored with exact f32 dot product read from SQLite.
  6. **Ranking:** the companion's `coverage` mode (pooled bm25 title 10 / heading 5 / text 1 / entities 8, title-match, vector, term coverage; RRF k = 60); bakeoff modes and Node workarounds not ported.
  7. **The daemon is the only writer and reader:** one shared `Index` handle per process, used by the job runner (feed) and the request path; the editor searches through a daemon request.
  8. **The companion is retired from cv:** no `:4801` calls, no HTTP client or remote compare, shadow P3 removed; its chunks/embeddings come over once via `cv search import`; removing the `crucible-search` container is a user step after soak.
- WP-4: editor search goes through `Request::Search`; migration 0007 imports the companion once.
