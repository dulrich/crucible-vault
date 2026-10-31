# Spike: transcript-refine (terra refine + chapters, single call) — 2026-10-30

Question: can the intake-enrich model (`router:codex-profile-a/gpt-6.1-terra`) refine a whole YouTube transcript with chapter headings in one call, within output/time limits, for dialog and monologue?

## Corpus (aggregates only for vault material)

- `pub-dw-caro`: public Public Author B × Public Author C dialog (from `docs/spikes/auto-intake-llm` corpus), 7,674 words, named speakers.
- `v-2700`, `v-6000`, `v-7900`, `v-13000`: unrefined YouTube clip transcript sections copied read-only from the live vault, picked by word count nearest p50 / p90 / p95 / ~13k of the unrefined population. Text never read; only counts below.
- Population (live vault, `## Transcript` section words, YouTube `source`): 418 unrefined with a transcript — p50 2,725 · p90 6,037 · p95 7,878 · max 55,860 words; 8 over 12k words. 55 already `refined`.
- Speaker cues in the four vault transcripts (`>>` markers, `Name:` prefixes): 0 — auto-captions carry no turn boundaries.

## Results (`run.sh`, prompt `prompts/refine-chapters.md`, max_tokens 32000)

| doc | in words | wall s | in tok | out tok | stop | out words | ratio | `##` headings | speaker paras |
|---|---|---|---|---|---|---|---|---|---|
| pub-dw-caro | 7,674 | 72.0 | 21,561 | 10,144 | end_turn | 7,197 | 0.94 | 11 | 66 |
| v-2700 | 2,691 | 18.3 | 5,456 | 2,838 | end_turn | 2,468 | 0.92 | 6 | 0 |
| v-6000 | 6,031 | 41.9 | 12,072 | 6,375 | end_turn | 5,402 | 0.90 | 12 | 0 |
| v-7900 | 7,878 | 55.3 | 20,322 | 7,931 | end_turn | 6,245 | 0.79 | 9 | 0 |
| v-13000 | 12,776 | 93.6 | 20,236 | 13,313 | end_turn | 11,462 | 0.90 | 18 | 0 |

- One first attempt of v-7900 failed at 37 s with `session closed: sidecar shutting down` (router-side transient); the rerun succeeded — the step must fail retriably, never write a partial.
- Throughput ~140 output tok/s; a 12.8k-word transcript fits one call in ~95 s (default provider timeout 600 s).
- Word retention 0.79–0.94 (filler/false-start removal). Timestamps left: 0–1 per doc.
- Public dialog quality (read): 11 sensible chapter titles, every turn `**Name:**`-labelled, links preserved.

Commands: `host-env run crucible-vault -- ./run.sh` (all docs in `corpus/`); the v-7900 rerun ran with the other docs moved aside.
