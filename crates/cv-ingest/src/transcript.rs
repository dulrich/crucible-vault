//! Transcript-section helpers for the `transcript_refine` step (plan `youtube-transcript-auto-refine`
//! WP-1, decision D30; measurements in `docs/spikes/transcript-refine/FINDINGS.md`): locate a note
//! body's `## Transcript` section, the text of it that may be sent to a model, chunking at
//! paragraph boundaries and the word-retention check on the answer.
//!
//! Pure: no I/O, no clock. The daemon step owns the provider calls, the raw copy and the write.
//!
//! Decisions recorded here:
//! - **Section**: from the line after the `## Transcript` heading (case-insensitive, outside code
//!   fences) to the next `# `/`## ` heading or the end of the body. `### ` chapter headings stay
//!   inside it.
//! - **Managed blocks are never sent nor replaced**: a well-formed `%% cv-enrich %%` …
//!   `%% /cv-enrich %%` pair (the top block, or the trailing `[!removed]-` fold that sits inside
//!   an apparent last-section range) and every `%% cv-enrich heading %%` line split the section
//!   into text segments; [`replace`] edits one segment and leaves the rest byte-identical. An
//!   unterminated open marker is ordinary text (same rule as `enrich::strip_generated`).
//! - **Chunks**: a section over [`MAX_CHUNK_WORDS`] words splits at blank-line boundaries into
//!   `ceil(words / MAX_CHUNK_WORDS)` target-sized chunks, each at most [`MAX_CHUNK_WORDS`]; a
//!   single paragraph over the cap is split at word boundaries (auto-captions can be one block).

use std::ops::Range;

use cv_text::blocks::{self, ENRICH_HEADING, ENRICH_OPEN, Line};

/// A section over this many words is sent in several calls (spike: whole transcripts up to 12.8k
/// words refined in one call, 8 of 418 unrefined clips are larger).
pub const MAX_CHUNK_WORDS: usize = 12_000;

/// The smallest accepted `output words / input words` of one answer. The spike measured 0.79–0.94
/// (`docs/spikes/transcript-refine/FINDINGS.md`: filler, timestamps and false starts are
/// removed); 0.6 leaves room for a chattier transcript while still catching a summary or a
/// truncated answer, which the step then rejects without writing.
pub const MIN_RETENTION: f64 = 0.6;

/// A section with fewer words is not refined (the step skips it as `"short transcript"`, and the
/// intake backfill does not count it as refine-pending). A transcript that short (a
/// YouTube short) has little to clean up, and the enrich pass still runs on it.
pub const MIN_REFINE_WORDS: usize = 300;

/// The located `## Transcript` section of a note body (byte offsets into that body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// Byte offset just after the heading line (including its newline).
    pub body_start: usize,
    /// Byte offset of the next `#`/`##` heading line, or the body length.
    pub end: usize,
    /// The non-blank unmanaged text of the section, each range trimmed of surrounding whitespace.
    pub segments: Vec<Range<usize>>,
}

/// The index of the close marker when `ls[i]` opens a well-formed `cv-enrich` pair.
fn managed_close(ls: &[Line<'_>], i: usize) -> Option<usize> {
    (ls[i].text.trim() == ENRICH_OPEN)
        .then(|| blocks::managed_close(ls, i))
        .flatten()
}

/// A level-1 or level-2 heading line (the section terminators).
fn is_section_heading(t: &str) -> bool {
    t.starts_with("# ") || t.starts_with("## ")
}

/// Locates the first `## Transcript` section of `body` (frontmatter already split off). `None`
/// when there is no such heading.
#[must_use]
pub fn locate(body: &str) -> Option<Section> {
    let ls = blocks::lines(body);
    let fenced = blocks::fenced_lines(&ls);
    let h = blocks::transcript_heading(&ls, 0)?;
    let body_start = ls[h].end;
    // Section end: the next level-1/2 heading outside fences and managed blocks.
    let mut end = body.len();
    let mut i = h + 1;
    while i < ls.len() {
        if let Some(j) = managed_close(&ls, i) {
            i = j + 1;
            continue;
        }
        if !fenced[i] && is_section_heading(ls[i].text) {
            end = ls[i].start;
            break;
        }
        i += 1;
    }
    // Segments: the text between managed blocks / heading markers.
    let mut segments = Vec::new();
    let mut seg_start: Option<usize> = None;
    let mut seg_end = body_start;
    let push = |segments: &mut Vec<Range<usize>>, s: usize, e: usize| {
        let raw = &body[s..e];
        let t = raw.trim();
        if !t.is_empty() {
            let lead = raw.len() - raw.trim_start().len();
            segments.push(s + lead..s + lead + t.len());
        }
    };
    let mut i = h + 1;
    while i < ls.len() && ls[i].start < end {
        let managed = managed_close(&ls, i);
        if managed.is_some() || ls[i].text.trim() == ENRICH_HEADING {
            if let Some(s) = seg_start.take() {
                push(&mut segments, s, seg_end);
            }
            i = managed.map_or(i + 1, |j| j + 1);
            continue;
        }
        seg_start.get_or_insert(ls[i].start);
        seg_end = ls[i].end.min(end);
        i += 1;
    }
    if let Some(s) = seg_start {
        push(&mut segments, s, seg_end);
    }
    Some(Section {
        body_start,
        end,
        segments,
    })
}

/// What a note body's `## Transcript` section allows the `transcript_refine` step to do (word
/// counts are the section's unmanaged text, all segments summed). The one reading shared by the
/// step, the intake backfill's refine-pending pick and `auto_file`'s short/long kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Eligibility {
    /// No `## Transcript` heading.
    NoSection,
    /// The heading is there, but the section has no unmanaged text.
    Empty,
    /// Managed enrich blocks split the text into several segments (the step refuses it).
    Split { words: usize },
    /// One segment under the word floor (the step skips it).
    Short { words: usize },
    /// One segment at or above the floor: the byte range of the text to refine.
    Ready { range: Range<usize>, words: usize },
}

impl Eligibility {
    /// The section's unmanaged word count, all segments summed (0 without text).
    #[must_use]
    pub fn words(&self) -> usize {
        match self {
            Self::NoSection | Self::Empty => 0,
            Self::Split { words } | Self::Short { words } | Self::Ready { words, .. } => *words,
        }
    }
}

/// [`eligibility_at`] with the default floor [`MIN_REFINE_WORDS`].
#[must_use]
pub fn eligibility(body: &str) -> Eligibility {
    eligibility_at(body, MIN_REFINE_WORDS)
}

/// The [`Eligibility`] of `body` (frontmatter already split off) under a `min_words` floor. A
/// split section is reported as split whatever its size.
#[must_use]
pub fn eligibility_at(body: &str, min_words: usize) -> Eligibility {
    let Some(section) = locate(body) else {
        return Eligibility::NoSection;
    };
    let total = section
        .segments
        .iter()
        .map(|r| words(&body[r.clone()]))
        .sum();
    match section.segments.as_slice() {
        [] => Eligibility::Empty,
        [_] if total < min_words => Eligibility::Short { words: total },
        [one] => Eligibility::Ready {
            range: one.clone(),
            words: total,
        },
        _ => Eligibility::Split { words: total },
    }
}

/// Whitespace-separated word count.
#[must_use]
pub fn words(text: &str) -> usize {
    text.split_whitespace().count()
}

/// Splits `text` into at most [`MAX_CHUNK_WORDS`]-word chunks of roughly equal size at blank-line
/// boundaries (one chunk when it already fits). Chunks are paragraph-joined with one blank line.
#[must_use]
pub fn chunks(text: &str) -> Vec<String> {
    let total = words(text);
    if total <= MAX_CHUNK_WORDS {
        return vec![text.trim().to_owned()];
    }
    let n = total.div_ceil(MAX_CHUNK_WORDS);
    let target = total.div_ceil(n);
    let mut units: Vec<String> = Vec::new();
    for para in paragraphs(text) {
        if words(&para) <= target {
            units.push(para);
        } else {
            // A block over the target: cut at word boundaries into target-sized pieces.
            let ws: Vec<&str> = para.split_whitespace().collect();
            units.extend(ws.chunks(target).map(|c| c.join(" ")));
        }
    }
    let mut out = Vec::new();
    let (mut cur, mut cur_words) = (String::new(), 0);
    let mut remaining = total;
    // Each chunk aims at remaining words / remaining chunks, closing on whichever side of that
    // goal is nearer; the hard cap is MAX_CHUNK_WORDS.
    let mut goal = target;
    for u in units {
        let w = words(&u);
        let next = cur_words + w;
        let close = cur_words > 0
            && (next > MAX_CHUNK_WORDS || (next > goal && next - goal >= goal - cur_words));
        if close {
            out.push(std::mem::take(&mut cur));
            remaining -= cur_words;
            cur_words = 0;
            goal = remaining.div_ceil(remaining.div_ceil(MAX_CHUNK_WORDS));
        }
        if !cur.is_empty() {
            cur.push_str("\n\n");
        }
        cur.push_str(&u);
        cur_words += w;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The blank-line separated paragraphs of `text`, each trimmed.
fn paragraphs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    for l in text.lines() {
        if l.trim().is_empty() {
            if !cur.is_empty() {
                out.push(cur.join("\n").trim().to_owned());
                cur.clear();
            }
        } else {
            cur.push(l);
        }
    }
    if !cur.is_empty() {
        out.push(cur.join("\n").trim().to_owned());
    }
    out
}

/// Whether an answer of `words_out` words is an acceptable refinement of `words_in` words: it is
/// non-empty and keeps at least [`MIN_RETENTION`] of the input.
#[must_use]
pub fn retention_ok(words_in: usize, words_out: usize) -> bool {
    #[allow(clippy::cast_precision_loss)]
    let ratio = words_out as f64 / words_in.max(1) as f64;
    words_out > 0 && ratio >= MIN_RETENTION
}

/// Replaces `body[range]` with the trimmed `new`, leaving every other byte as it was.
#[must_use]
pub fn replace(body: &str, range: &Range<usize>, new: &str) -> String {
    format!(
        "{}{}{}",
        &body[..range.start],
        new.trim(),
        &body[range.end..]
    )
}

/// The number of `### ` chapter headings in `text`.
#[must_use]
pub fn chapter_headings(text: &str) -> usize {
    text.lines().filter(|l| l.starts_with("### ")).count()
}

#[cfg(test)]
#[path = "transcript_tests.rs"]
mod tests;
