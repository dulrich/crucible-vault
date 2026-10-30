# Spike: automatic intake processing premises (2026-10-30)

Promoted from `runs/spike/auto-intake-llm/` with plan `intake-auto-enrich`; its driver scripts (`llm.sh`, `run-all.sh`, `verify.sh`, `gliner.sh`, `fm-census.pl`, `banner-census.pl`) and prompts are promoted beside this file by WP-3. The public corpus is not committed (sources named below, regenerable).

Scope: FEEDBACK.md "Intake → first pass on auto processing" + related Backlog items. Vault measurements are aggregates only (read-only, `.trash/` and `_custom/` excluded); LLM/NER quality is judged on a **public corpus** (`corpus/`, 5 docs, 23.5k words: two Public Publication D posts with "Brought to you by" sponsor blocks, one paid-preview post, a Dwarkesh podcast transcript, Public Author A's 2024 review with full site chrome), built with `pandoc -f html -t gfm-raw_html` from Substack `/api/v1/posts/<slug>` `body_html` / the page HTML.

## 1. Volume (state.db `tracker_items`, read-only)

- 30-day `outcome='note'`: blog 626 (p50 1,226 words, p90 4,361, max 17,571, total 1.18M words), mail 47 (p50 1,614, p90 3,778, total 101k). Last 7 days: blog 59 notes, mail 17, youtube 28 lines (YouTube transcripts arrive via Web Clipper clips, not trackers).
- ⇒ ~20–25 notes/day, ~1.7M input tokens/month for one full LLM pass. Throughput is not a design constraint; per-call latency and quota are.

## 2. Publication/author completeness (`fm-census.pl <vault> 30`)

| type/src | n | author | publication |
|---|---|---|---|
| tracker web | 1928 | 1102 | 665 |
| tracker YouTube | 467 | 467 | 0 |
| clipped YouTube (no post-id) | 47 | 47 | 1 |

Confirms the FEEDBACK item: YouTube notes carry the channel in `author` but `publication` is blank on 466/467 tracker and 46/47 clipped notes. Web tracker notes: 34% have `publication`. A mechanical fill (channel → publication for YouTube; `sources.toml`/feed title for blogs) — no model needed.

## 3. Recurring images = banner/footer candidates (`banner-census.pl <vault> 30`)

`notes=2442 image_refs=4537 distinct_targets=4182 recurring_targets(>=3 notes)=31 refs_to_recurring=211 of_which_in_first/last_15pct=144 top10_note_counts=32,22,21,9,8,8,8,8,8,8` (local files keyed by content md5, remote by URL sans query).

⇒ 4.7% of image refs point at 31 recurring targets, 68% of those at the body edges. Recurring-image detection is mechanical (hash/URL recurrence ≥3 notes), no vision pass needed to *find* banners; vision description is only needed to judge one-off images.

## 4. GLiNER2 (`gliner.sh`, onnx sidecar :4812, `gliner2-base`, paragraphs ≥40 chars, ≤32/request)

| doc | paragraphs | unique entities (conf ≥0.6) | wall s |
|---|---|---|---|
| dw-caro | 131 | 193 | 10.6 |
| pe-google | 103 | 155 | 5.8 |
| pe-paid-preview | 116 | 158 | 7.3 |
| pe-resilient | 100 | 127 | 5.7 |
| sw-2024 | 188 | 305 | 11.6 |

Fast on CPU (first doc includes the 2–3.5 s cold load), high recall, **low precision**: generic nouns ("students", "engineers", "AI", "models", "I"), label confusion (Claude → person, O'Reilly → location, "Google & CS education" → organization), no alias merging (Titus / Public Author E), and no centrality (150–300 "entities" per doc). Usable only as a candidate generator; it is not a frontmatter-ready entity list on its own.

## 5. cv → router wiring premise

The router (`127.0.0.1:4813`, Anthropic `/v1/messages`) authenticates `x-gateway-key` only (`inference-server/router/AGENTS.md` auth.rs). cv's `anthropic` provider kind (`crates/cv-providers/src/wire.rs:189`) sends `x-api-key`, so it cannot reach the router today; needs a provider-level auth-header option. Effort needs no wire change: `codex-*/gpt-6.1-terra` defaults to low, `-medium` suffix selects medium (sidecar precedence rule).

## 6. terra low vs medium (`run-all.sh`, `verify.sh`; results.tsv, verify.txt)

Driver: `run-all.sh` re-execs under `host-env run router-client` (fleet-tooling `77c96a4a`: exec consumer exposing only `GATEWAY_KEY`), calls the router `/v1/messages` serially. Prompts: `prompts/structure.md` (summary, entities, key claims with a verbatim quote, chapters with a verbatim `starts_at`, 1–5 density/evergreen/original scores) and `prompts/cleanup.md` (sponsor/cta/chrome/footer spans as verbatim `first_line`/`last_line` anchors + `content_ends_after`; the model never rewrites the body).

- **Reliability:** 20/20 `end_turn`, 20/20 valid JSON (no fence stripping needed beyond `sed '/^```/d'`), on `codex-profile-a`. Wall 3.1–21.1 s (structure 6.4–11.6 s; cleanup 3.1–5.2 s, 11–21 s on the 7.5k-word page with chrome).
- **Anchor fidelity (whitespace-squeezed substring match):** structure 74/75 low, 75/75 medium; cleanup 34/35 low, 35/35 medium. ⇒ anchors are reliable enough to drive deterministic edits, with a verify-or-drop rule for the ~1–2% misses.
- **Low ≈ medium:** cleanup outputs are byte-identical on 4/5 docs (2 lines differ on the 5th); structure differs in wording and entity order only (same counts on every doc). Medium bought nothing measurable ⇒ **low is the default**; medium is not worth a config knob yet.
- **Quality (public corpus, read by the agent):** entities are central and alias-merged (11–14 per doc vs GLiNER2's 127–305), claims substantive with quotes, chapters track the real topic shifts (6–10). Cleanup found every sponsor block ("Brought to You by", "## Sponsors", Public Author A's "Sponsored by"), event/subscribe CTAs, site chrome and footers, and set a sensible `content_ends_after`. The "deepdives relevant for this episode" related-links list was classed footer — a judgment call the user may want kept.
- **Scores do not discriminate:** density 4 on all 5 docs, original 4–5, evergreen 3–5. Absolute 1–5 LLM scoring is not a relevance/value signal on its own (caveat: this corpus is uniformly high quality); relevance needs relative/user-signal grounding (D5 importance model).
- **Tokens:** input 5.4k–44.8k per call; a 5-token ping costs 10.2k input ⇒ ~10k of codex-sidecar overhead per call (cache reads appear on repeat sessions). One combined structure+cleanup call per note halves that overhead. At ~25 notes/day, one call/note ≈ 25 × ~20k ≈ 0.5M input tokens/day of subscription quota.
- **Sidecar constraints:** parallel calls on one persona → one fails "superseded by a newer request on this session" ⇒ per-persona concurrency 1. `codex-profile-b` deterministically rejects these prompt+doc requests in ~0 s with "app-server: Invalid request: invalid type: null, expected a string" (`results-failed-cindy.tsv`) while `codex-profile-a` serves them — reported to inference-server (Backlog `396dfb7`).
- **Localized embed fallback:** 3 of 5 Substack bodies contain YouTube iframe fallback text rendered in Spanish ("# Se produjo un error." / "No se puede ejecutar JavaScript."); the model classed it chrome. Same family as FEEDBACK's "Shorts (as Post) parser breaks with localization of the video embed": structure-matching on embed fallback text is locale-dependent.
