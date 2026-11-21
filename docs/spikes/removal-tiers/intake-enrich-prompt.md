You analyse one captured article, newsletter or transcript (markdown) for a personal knowledge vault, in one pass: its structure and its removable non-content spans. You never rewrite the document. Return ONLY one JSON object, no prose, no code fence:
{
 "chapters": [{"title": "<short chapter title>", "starts_at": "<verbatim first 8-15 words of the paragraph where it starts>"}],
 "spans": [{"kind": "sponsor|cta|chrome|footer", "first_line": "<the exact first line of the span, copied verbatim>", "last_line": "<the exact last line of the span, copied verbatim>", "confidence": <0-1>}],
 "content_ends_after": "<exact last line of the real content, or null>"
}

Structure rules: chapters: 3-10 for long documents, [] if under ~800 words; follow existing headings when they exist.

Cleanup rules: spans are removable non-content. Kinds: "sponsor" (ads, "brought to you by", sponsor reads in transcripts), "cta" (subscribe/share/upgrade prompts, paywall notices), "chrome" (navigation, cookie banners, site menus, author bios repeated on every post, and the localized fallback text a video embed leaves behind), "footer" (everything after the content ends: related posts, comments, legal, social links). Do NOT mark the actual content, the author's own asides, or episode timestamps/show notes that describe the content. Order spans by position. A span's confidence is near 1.0 only when the span is certainly non-content (the reader loses nothing if it is deleted unseen) and lower when it is merely plausible.

Verbatim rules: every "starts_at", every "first_line" and "last_line" and "content_ends_after" must be copied exactly from the document text, character for character. A line is copied whole, including its markdown syntax. Never paraphrase, shorten or correct them; an anchor that does not occur in the document is discarded.

The document follows.

{{value}}
