# Spike: image-describe budget, reasoning and sampler (2026-10-28)

Questions: why does every live image describe fail with `finish reason "length"` and hold clip intake for minutes; what output budget is right; does reasoning improve the descriptions? Model: the live `gemma-4-12B-it-qat-UD-Q4_K_XL` on llama-swap (`127.0.0.1:4811`). It is the same Gemma 4 12B as Crucible's `gemma-4-12B-it-Q4_K_M`, in a different quantization, so Crucible's settings apply directly. (`docs/spikes/a2-vision` treated them as different models.)

## Facts from source and the live system

- **Crucible's describe wire** (`src/providers/openaiCompatible.ts:271-296`, `src/providers/shared.ts:99-108` in https://github.com/dulrich/obsidian-crucible): `temperature: 0`, per-pass `max_tokens` (narrative 512, extraction 2048), and `reasoning_effort: 'none'` for local providers only. Its comment calls the last one "a correctness requirement" (without it the content comes back empty). The cv port (`crates/cv/src/steps/image_describe.rs`, `crates/cv-providers/src/wire.rs`) sends none of these: no temperature, no reasoning field, and a single 4096 cap.
- **Live failure rate:** `journalctl --user -u crucible-vault --since -7d | grep image_describe` → 17 `recording permanent failure … finish reason "length"`, 0 successes.
- **Intake delay:** `clip-images` = `localize` → child `image-describe-note` → child `clip-auto-file`. Both children lock the clip note, so filing waits for describe. Job-queue timing (`state.db` `jobs`, `settled_at_ms - created_ms`) for the 2026-10-28 16:15 clip: describe 133.0 s, auto-file 133.1 s. Worst case over 7 days: 419 s.
- **Mail:** watcher → `track-mail` takes 0.8 s on average (416 runs over 7 days). The rest of the delay is the watcher's 5 s quiet + 1 s size-stable wait (`crates/cv/src/daemon/mail_watch.rs:41-48`).

## Measurements

Image sets: **A** = 9 images from outside the vault (4 benchmark charts, 2 UI screenshots, an infographic, a 2×2 diagram, a game render; `manifest-a.txt` lists neutral ids and kinds only, not the original file names), whose text was read and judged. **B** = 18 vault-derived samples from an earlier bench (not part of this repo), for which only finish, token counts and timings are recorded. All images were downscaled to a 1568 px long edge as cv does; both cv prompts were used. Scripts: `run.sh` (sweep 1), `loop.sh` (sampler arms), `cand.sh` (candidate settings).

**1. Temperature 0 with an 8192 cap (Crucible's sampler)** — `run.sh`:

| arm / pass | n | finish | completion tokens p50 / max |
|---|---|---|---|
| reasoning off / narrative | 27 | 27 stop | 141–153 / 219 |
| reasoning off / extraction | 27 | **7 length** | 289–572 / 8192 |
| reasoning on / narrative | 27 | 27 stop | 834–1025 / 1292 |
| reasoning on / extraction | 27 | **9 length, all with empty content** (the whole 8192 spent on reasoning) | 3638–8192 / 8192 |

Six of the seven reasoning-off `length` runs are repetition loops: 0–16% of their output lines are unique. The seventh (77% unique) is a genuinely long transcription.

**2. Sampler arms on the 7 looping images** (reasoning off, extraction, cap 2048) — `loop.sh`:

| sampler | stop |
|---|---|
| temperature 0 | 0/7 |
| server default | 2/7 |
| temperature 0 + DRY (0.8) | 0/7 |
| Unsloth-recommended (t 1.0, top_p 0.95, top_k 64), 2 runs | 6/14 |
| temperature 0 + `repeat_penalty` 1.15 / `repeat_last_n` 256 | 6/7 |
| **recommended + `repeat_penalty` 1.15 / 256**, 2 runs | **12/14** (both misses 100% unique lines, i.e. genuinely long) |

**3. Candidate settings** (recommended sampler + repeat penalty; reasoning off with caps 512 / 2048, vs reasoning on with cap 4096) — `cand.sh`:

| arm / pass | n | finish | tokens p50 / max | wall p50 / max |
|---|---|---|---|---|
| off / narrative | 27 | 27 stop | 143–145 / 203 | 2.5 s / 3.3 s |
| off / extraction | 27 | 26 stop, 1 length (a dense chart, 56% unique lines) | 246–367 / 2048 | 2.6–4.6 s / 19.6 s |
| on / narrative (A) | 9 | 9 stop | 598 / 843 | 8.4 s / 11.5 s |
| on / extraction (A) | 9 | **6 length** (4 with empty content) | 4096 / 4096 | 45.7 s / 60.3 s |

Both passes per image with reasoning off: p50 5.6 s, mean 6.7 s, max 22.1 s (n=27). With reasoning on: p50 57.2 s (n=9).

**4. Quality (set A, judged against the images).**

- **Charts:** neither arm transcribes dense chart labels correctly. Both substitute model names learned in training for the ones printed (for example, "GPT-4o" / "Claude 3 Opus" on a chart that shows GPT-6 / Claude Opus 5), and reasoning-on extraction produced nothing for 3 of the 4 charts.
- **Narratives:** reasoning-on narratives are wordier but no more accurate. On the translation chart, both name models that are not on it, and the reasoning-on narrative states the opposite of the chart's point.
- **Screenshots:** on a dashboard screenshot, reasoning-on extraction read the six counter values and the checkbox states that reasoning-off skipped. On a settings form, reasoning-off captured two more fields.
- **Conclusion:** this matches the July bench (Q4_K_M, temperature 0): thinking was 4.3× slower, used 4.65× the tokens, and every numeric error came out the same. The vision encoder's resolution limits accuracy, not reasoning.

## Conclusions

1. For local providers, send `reasoning_effort: "none"` on both passes (Crucible parity). Reasoning-on extraction fails outright at any practical cap.
2. Sampler for local providers: `temperature` 1.0, `top_p` 0.95, `top_k` 64 (the model family's recommended settings) plus `repeat_penalty` 1.15 / `repeat_last_n` 256. Crucible's temperature 0 loops on 26% of extractions.
3. Caps: narrative 512 (observed max 219), extraction 2048 (Crucible's value; the only stop over 1300 was 1919). A `length` result that is not a repetition loop is a truncated but useful transcription, and keeping it beats discarding it.
4. With these settings describe takes ~6 s per image. That is still too slow to sit in front of filing, so describe must not block it.
