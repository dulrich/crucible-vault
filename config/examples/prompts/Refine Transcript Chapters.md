You refine one raw video transcript (markdown) for a personal knowledge vault, with minimal semantic change. Output ONLY the refined transcript text — no `## Transcript` heading, no preamble, no commentary, no code fence.

Goals:
- Remove timestamps.
- Reformat into readable paragraphs; improve grammar and punctuation.
- Remove obvious filler words ("um", "uh", "you know", "like" when not meaningful) and false starts. Do NOT remove meaningful phrases or alter intent.
- Do NOT summarize, omit, or merge distinct ideas. Preserve every distinct claim, example, step, tool name, command, number, caveat, and recommendation. Do not rewrite into a formal article.

Chapters (both dialog and monologue):
- Insert `### <short descriptive title>` chapter headings (exactly three hashes, never `##`) at natural topic transitions — roughly one per 3–8 minutes of speech, never more than one per ~300 words, and at least one heading before the first paragraph.
- If the source already lists chapter timestamps, use those boundaries and titles.
- Headings only mark where a topic starts; they never replace or summarize the speech under them.

Speakers:
- Label speakers only when the transcript itself names them: start each speaker turn as a new paragraph prefixed `**Name:** ` using the names the transcript gives. Keep every turn.
- Never invent speakers or labels. If the transcript does not name its speakers (for example auto-captions without turn cues), and for any single-speaker monologue, use no speaker labels at all.

Input scope: the input is only a transcript section (or one consecutive part of it) of the note. Keep only spoken content; drop sponsor-read boilerplate only if it is clearly an ad read inserted into the speech.

The transcript follows.

{{value}}
