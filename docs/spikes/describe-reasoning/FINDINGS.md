# Spike: image-describe `finish reason "length"` and clip-intake delay (2026-10-28)

Question: why does a fresh web clip take minutes to be filed (FEEDBACK "clipping intake is delayed by minutes"), and does it need a filesystem watcher?

## Measured

1. **Clippings intake is already event-driven.** The `clip-images` action has an `event` starter on `created` in `Clippings` (live `actions.toml`), and the vault watcher delivers it in about 1 s (journal: the first `link_scan` for the clip lands 1 s after creation). Mail already has its own watcher (`crates/cv/src/daemon/mail_watch.rs`; the `track-mail` 15-minute schedule stays as the backstop). No new watcher is needed.
2. **The delay comes from `image-describe-note`.** `clip-images` = `localize` → `run_action image-describe-note` → `run_action clip-auto-file`. Both children run in queue order, so filing waits on the describe step. On the 2026-10-28 16:15 clip, 3 images took ~45 s each and the clip was filed 2 min 13 s after it was created.
3. **Every describe has failed for 7 days.** `journalctl --user -u crucible-vault --since -7d | grep image_describe` shows 17 `recording permanent failure failure=extraction: finish reason "length"` lines and 0 successes. Each failure is stored as a permanent record, so those images are poison-skipped from then on.
4. **Cause: reasoning tokens use up `max_tokens`.** The live model `gemma-4-12B-it-qat-UD-Q4_K_XL` on llama-swap (`127.0.0.1:4811`) reports `"reasoning":"request-controllable"`. The extraction prompt was sent with `max_tokens 4096`, the same as the step's default.

| image | reasoning default | `reasoning_effort: "none"` |
|---|---|---|
| synthetic (`docs/spikes/reading-select/out/03-cross-block.png`) | stop, 753 tok (2710 chars reasoning), 10.2 s | stop, 75 tok, 0.8 s, same answer |
| live clip image 1 | stop, 1954 tok, 16 s | stop, 493 tok, 5 s |
| live clip image 2 | stop, 268 tok, 3 s | stop, 49 tok, 1 s |
| live clip image 3 | stop, 2055 tok, 20 s | stop, 60 tok, 2 s |
| live clip image 4 | **length, 4096 tok, 34 s** | stop, 126 tok, 2 s |

`chat_template_kwargs: {enable_thinking: false}` also works (synthetic image: 74 tok, 1.9 s). `reasoning_effort: "low"` does not help (780 tok, 6.8 s). Live images = the 4 newest images over 20 KiB in the vault (`find -mmin -240 -size +20k`, `.trash` excluded); only finish reason and token counts were recorded.

## Commands

Request body: `{model, max_tokens: 4096, messages: [{role: user, content: [{type: text, text: <EXTRACTION_PROMPT from crates/cv/src/steps/image_describe.rs:59>}, {type: image_url, image_url: {url: data:image/<mt>;base64,...}}]}]}`, optionally merged with `{reasoning_effort: "none"}`, POSTed with `curl` to `http://127.0.0.1:4811/v1/chat/completions`. Reported: `.choices[0].finish_reason`, `.usage.completion_tokens`, wall seconds.

## Conclusion

Send `reasoning_effort: "none"` on describe requests to fix both the failures and the delay: 3 images take ~5–10 s instead of 2+ min, with no change to the order of steps. Retry the images already recorded as `length` failures.
