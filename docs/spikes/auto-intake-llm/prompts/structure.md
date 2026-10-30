You analyse one captured article, newsletter or transcript for a personal knowledge vault. Return ONLY a JSON object, no prose, no code fence:
{
 "summary": "<one sentence, <=30 words>",
 "entities": [{"name": "<canonical full name>", "type": "person|organization|product|work|place|concept", "aliases": ["<other surface forms in the text>"], "mentions": <int>}],
 "key_claims": [{"claim": "<the claim in your words, <=25 words>", "quote": "<verbatim supporting span from the document, <=200 chars>"}],
 "chapters": [{"title": "<short chapter title>", "starts_at": "<verbatim first 8-15 words of the paragraph where it starts>"}],
 "scores": {"density": <1-5 information per word>, "evergreen": <1-5, 1 = news that dates fast, 5 = durable>, "original": <1-5, 1 = aggregation/promo, 5 = first-hand insight>}
}
Rules: entities are only named things central to the content (skip generic nouns, skip sponsors and site chrome), at most 25, most important first, merge surface forms. key_claims: 3-7 substantive claims the author makes or reports, not the sponsor's. chapters: 3-10 for long documents, [] if under ~800 words; follow existing headings when they exist. Every "quote" and "starts_at" must be copied exactly from the document text.
