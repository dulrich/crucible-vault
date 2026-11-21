# WP-1 findings — confidence validation of the release prompt

Plan `abstract-removal-confidence-deletion` (rev 2), WP-1. Aggregates only: no note text, names, subjects, paths or model replies were written or printed; per-note records (counts only) lived in `/dev/shm` and are deleted.

**Verdict: HOLD for the user. No tested threshold meets both criteria. Nothing is recommended; deletion stays off.** All 27 model spans of the sample carry confidence 0.95 or higher, so the four thresholds are indistinguishable on this prompt and sample, and the sample is also small (11 notes).

## Commands

Build and tool: `docs/spikes/removal-tiers/conf-spike/` (throwaway crate, empty `[workspace]`, exact pins, path deps on `cv-ingest`, `cv-note`, `cv-text`, `cv-providers`), built with `CARGO_TARGET_DIR=/dev/shm/conf-spike-target cargo build --release --manifest-path docs/spikes/removal-tiers/conf-spike/Cargo.toml`.

- `PROMPT=docs/spikes/removal-tiers/intake-enrich-prompt.md conf-spike list` — sample selection (counts below).
- `PROMPT=… conf-spike dry` — request-shape check, no call.
- `PROMPT=… conf-spike run /dev/shm/wp1-out.jsonl` — the 11 model calls.
- `conf-spike report /dev/shm/wp1-out.jsonl` — every number in the tables below.

## Prompt

`intake-enrich-prompt.md`: the example prompt minus the `summary`, `entities`, `key_claims` keys and their rules and the `"quote"` mention, plus one Cleanup-rules sentence defining span confidence (diff in the hand-back report). Live prompt check: the live `Intake Enrich.md` (the `prompt_file` of the live `[intake-enrich]` entry) is byte-identical to `config/examples/prompts/Intake Enrich.md` (`cmp` exit 0, 0 differing lines).

## Request parameters matched

Source of truth: `crates/cv/src/steps/agent.rs` `call()`, with the **live** `agents.toml` / `providers.toml` under `vault/_custom/crucible-vault/` (read by the spike at run time).

| Parameter | Value | How matched |
|---|---|---|
| Route | live `[intake-enrich]` `model = router:codex-profile-a/gpt-6.1-terra` (differs from the example's `openrouter:openai/gpt-6.1-terra`) | read from the live `agents.toml` |
| Provider | `router`, kind `anthropic`, `http://127.0.0.1:4813`, `auth_header = x-gateway-key`, `max_concurrent_requests = 1` | live `providers.toml` parsed by `ProvidersConfig::from_toml`; key from the secrets file by `SystemSecrets`, never touched by the spike |
| Call path | `ProviderManager::complete` with `WireExtras::default()` | the daemon's own code, so wire body, headers, timeout and limiter are the daemon's |
| `max_tokens` | 8192 (live agent value) | read from live `agents.toml` |
| System slot | empty (agent has no `system`) | as `call()` |
| User slot | the prompt with `{{value}}` = `input` (`{{content}}`) rendered over the body after the frontmatter, via `cv_text::template::render` | same renderer and lookup keys; body composed as `captured_note` writes it (closing fence, one blank line, body, newline) |
| Finish reason | non-`stop` is a failed call | as `call()` |

Verification: no daemon request capture was possible without touching product code, so parity rests on shared code. `dry` printed the wire request built by `cv_providers::wire::build_completion` (url `/v1/messages`, body keys `max_tokens, messages, model, system`, headers `content-type, x-gateway-key, anthropic-version`, `system` empty, user starts with the prompt text and ends with the body and trailing newline). Only the prompt text differs from live (pinned prompt substituted for the live file, as the brief requires).

## Sample

| | count |
|---|---|
| Notes with fold-origin labels | 30 |
| of which not `via: email` | 19 |
| mail notes without a retained `.eml` (`cur/` or `processed/`) | 0 |
| Qualifying mail notes | 11 |
| Publications among them | 3 |
| Notes run | 11 (under the cap of 40; everything that qualifies was run) |

Rule: all qualifying notes taken (11 ≤ 40; the deterministic round-robin spread across publications was coded but not needed). 11 ≥ 10, so the plan's minimum-sample stop did not fire.

Input: `parse_eml` → `strip_html_chrome` + `html_to_markdown` (or trimmed text) → `clean_body`, as `track-mail`'s `raw_markdown` + `clean_body`. Truth: a pre-enrich nonblank line (whitespace-squeezed) present in the note's current body is *kept*, otherwise *removed*. Lines inside a still-present `[!removed]` fold would be "undecided" and excluded (0 such lines occurred).

## Run

| | |
|---|---|
| Calls made / failed / retries | 11 / 0 / 0 |
| Unparseable replies | 0 |
| Spans parsed / resolved / unresolved | 27 / 27 / 0 |
| Half-body guard refused notes | 0 |
| Nonblank pre-enrich lines | 447 |
| Kept / removed / undecided | 227 / 220 / 0 |

Span confidence buckets (all parsed spans): `<0.7` 0, `0.7–0.8` 0, `0.8–0.9` 0, `0.9–0.95` 0, `≥0.95` 27, missing/malformed 0.

## Per-threshold result

Lines claimed at or over the threshold; a line counts only if every resolved span claiming it is at or over it (the WP-3 overlap rule). Two variants for `content_ends_after`, which carries no confidence: **veto** (a line it also claims is not counted, as WP-3's "recurring banners and `content_ends_after` fold") and **excluded** (it is ignored; a line counts if its spans qualify). The half-body guard changed nothing (0 refused), so guarded and unguarded rows are identical.

Veto variant:

| Threshold | lines deleted | false deletes (kept lines) | notes with a false delete | removed lines deleted | share of 220 removed | false / deleted |
|---|---|---|---|---|---|---|
| 0.7 | 64 | 0 | 0 | 64 | 0.291 | 0 |
| 0.8 | 64 | 0 | 0 | 64 | 0.291 | 0 |
| 0.9 | 64 | 0 | 0 | 64 | 0.291 | 0 |
| 0.95 | 64 | 0 | 0 | 64 | 0.291 | 0 |

Excluded variant:

| Threshold | lines deleted | false deletes (kept lines) | notes with a false delete | removed lines deleted | share of 220 removed | false / deleted |
|---|---|---|---|---|---|---|
| 0.7 | 179 | 4 | 1 | 175 | 0.795 | 0.0223 |
| 0.8 | 179 | 4 | 1 | 175 | 0.795 | 0.0223 |
| 0.9 | 179 | 4 | 1 | 175 | 0.795 | 0.0223 |
| 0.95 | 179 | 4 | 1 | 175 | 0.795 | 0.0223 |

## Recommendation rule (fixed by the plan): lowest tested threshold with at most 1 false-delete line per 200 deleted lines and at least half of the removed lines deleted

- Veto variant: false deletes 0, but only 29.1% of removed lines deleted (< half) at every threshold: fails.
- Excluded variant: 79.5% deleted, but 4 false deletes in 179 lines (2.2%, limit 0.5%), all in one note: fails.
- **No threshold passes in either reading: hold, recommend nothing.** Deletion stays off (D5 of the plan's decisions).

## Observations (not recommendations)

- The confidence axis is not exercised: the model put every span at ≥0.95, so a threshold between 0.7 and 0.95 selects exactly the same lines. Whether a prompt that spreads confidence would separate the 4 false-delete lines is untested (prompt tuning is out of scope for this WP).
- The gap between the two variants (64 vs 179 lines) is lines both a span and `content_ends_after` claim; the rule for them decides recall, so WP-3's veto reading as written fails the recall half.
- Truth caveat that works in the optimistic direction for false deletes: 130 of the 220 "removed" lines contain a markdown link or image. The pre-enrich body is re-rendered from the `.eml` and does not pass through the boilerplate strip, localize or lint that the live note went through, so some "removed" lines are lines those steps rewrote or stripped, not lines the user deleted. This inflates the removed total (and could hide a false delete on a rewritten line); it does not inflate the 4 false deletes.
- Sample is small: 11 notes, 3 publications, one of which (index 2) supplies 9 of the 11.

## Files

`intake-enrich-prompt.md`, `conf-spike/Cargo.toml`, `conf-spike/src/main.rs` (no `target/`, no lockfile left in the tree). The spike's only side effects were the 11 model calls and read-only access to the vault, maildir and `state.db` (`sqlite3 -readonly`).
