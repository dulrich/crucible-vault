//! YouTube transcript cleanup — Crucible `cleanupYoutubeTranscript`
//! (`https://github.com/dulrich/obsidian-crucible/blob/master/src/lint.ts:11-84`).
//!
//! A note whose body opens with a YouTube `![](url)` embed followed by `## Transcript` and
//! `**m:ss**`-prefixed paragraphs gets those paragraphs' timestamps and `[Music]`-style
//! annotations removed and whitespace collapsed; trailing non-transcript paragraphs are kept.
//! Deviations: the frontmatter is located with `ofm::frontmatter::split_frontmatter` instead of
//! Crucible's `FRONTMATTER_REGEX`, the `(?!\()` look-ahead is checked by hand (the `regex` crate
//! has none), video-id recognition reuses `cv_text::extract_video_id_from_url`, and content between
//! the embed and `## Transcript` (e.g. a clipped description) is tolerated and kept ahead of the
//! cleaned transcript (Crucible required the header directly after the embed). Nothing
//! Obsidian-induced to drop (the `vault.modify` write is the caller's).
//!
//! [`transcript_evidence`] (plan intake-followups-mail-youtube WP-4, cv's own — Crucible ticked
//! tracker rows by hand) reuses these regexes to decide whether a note carries a transcript.

use std::sync::LazyLock;

use regex::Regex;

fn re(p: &str) -> Option<Regex> {
    Regex::new(p).ok()
}

static YT_EMBED_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| re(r"^([ \t]*\r?\n)*[ \t]*!\[\]\(([^)\s]+)\)[ \t]*\r?\n"));
static TRANSCRIPT_HEADER_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| re(r"(?m)^[ \t]*##[ \t]+Transcript[ \t]*\r?\n"));
static LEADING_BLANK_LINES_RE: LazyLock<Option<Regex>> = LazyLock::new(|| re(r"^([ \t]*\r?\n)+"));
static TIMESTAMP_PARA_RE: LazyLock<Option<Regex>> = LazyLock::new(|| re(r"^\*\*\d+:\d+\*\*"));
static TIMESTAMP_PREFIX_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| re(r"^\*\*\d+:\d+\*\*[ \t·•|–—-]*"));
static BRACKET_ANNOTATION_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| re(r"\\?\[[A-Za-z][A-Za-z ]*\\?\]"));
static PARAGRAPH_SPLIT_RE: LazyLock<Option<Regex>> = LazyLock::new(|| re(r"\r?\n[ \t]*\r?\n\s*"));
static WS_RE: LazyLock<Option<Regex>> = LazyLock::new(|| re(r"\s+"));

fn is_timestamp_para(p: &str) -> bool {
    TIMESTAMP_PARA_RE
        .as_ref()
        .is_some_and(|r| r.is_match(p.trim_start()))
}

/// Remove bracket annotations not followed by `(` (i.e. not a markdown link).
fn strip_annotations(s: &str) -> String {
    let Some(r) = BRACKET_ANNOTATION_RE.as_ref() else {
        return s.to_owned();
    };
    let mut out = String::with_capacity(s.len());
    let mut copied = 0;
    let mut at = 0;
    while let Some(m) = r.find_at(s, at) {
        if s[m.end()..].starts_with('(') {
            at = m.start() + s[m.start()..].chars().next().map_or(1, char::len_utf8);
        } else {
            out.push_str(&s[copied..m.start()]);
            copied = m.end();
            at = m.end();
        }
        if at >= s.len() {
            break;
        }
    }
    out.push_str(&s[copied..]);
    out
}

fn clean_paragraph(p: &str) -> String {
    let out = match TIMESTAMP_PREFIX_RE.as_ref() {
        Some(r) => r.replace(p, "").into_owned(),
        None => p.to_owned(),
    };
    let out = strip_annotations(&out);
    let out = match WS_RE.as_ref() {
        Some(r) => r.replace_all(&out, " ").into_owned(),
        None => out,
    };
    out.trim().to_owned()
}

/// Clean the transcript; returns the input unchanged when the note does not match the shape.
pub fn cleanup_youtube_transcript(content: &str) -> String {
    let (_, body) = ofm::frontmatter::split_frontmatter(content);
    let fm_text = &content[..content.len() - body.len()];
    let (Some(embed_re), Some(header_re), Some(split_re)) = (
        YT_EMBED_RE.as_ref(),
        TRANSCRIPT_HEADER_RE.as_ref(),
        PARAGRAPH_SPLIT_RE.as_ref(),
    ) else {
        return content.to_owned();
    };
    let Some(embed) = embed_re.captures(body) else {
        return content.to_owned();
    };
    let (Some(whole), Some(url)) = (embed.get(0), embed.get(2)) else {
        return content.to_owned();
    };
    if cv_text::extract_video_id_from_url(url.as_str()).is_none() {
        return content.to_owned();
    }
    let after_embed = &body[whole.end()..];
    let Some(header) = header_re.find(after_embed) else {
        return content.to_owned();
    };
    // Content between the embed and `## Transcript` (e.g. a clipped description) is kept,
    // verbatim, ahead of the cleaned transcript.
    let preamble = after_embed[..header.start()].trim();
    let after_header = &after_embed[header.end()..];
    let after_header = match LEADING_BLANK_LINES_RE.as_ref() {
        Some(r) => r.replace(after_header, "").into_owned(),
        None => after_header.to_owned(),
    };
    if !is_timestamp_para(&after_header) {
        return content.to_owned();
    }
    let paragraphs: Vec<&str> = split_re.split(&after_header).collect();
    let first_non = paragraphs
        .iter()
        .position(|p| !is_timestamp_para(p))
        .unwrap_or(paragraphs.len());
    if first_non == 0 {
        return content.to_owned();
    }
    let cleaned: Vec<String> = paragraphs[..first_non]
        .iter()
        .map(|p| clean_paragraph(p))
        .filter(|p| !p.is_empty())
        .collect();
    let trailing = paragraphs[first_non..].join("\n\n");
    let trailing = trailing.trim_end();
    let mut result = String::new();
    if !preamble.is_empty() {
        result.push_str(preamble);
        result.push_str("\n\n");
    }
    result.push_str(&cleaned.join("\n\n"));
    if !trailing.is_empty() {
        result.push_str("\n\n");
        result.push_str(trailing);
    }
    format!("{fm_text}{result}\n")
}

/// Fewest `**m:ss**` paragraphs under `## Transcript` that count as a transcript.
const MIN_TIMESTAMP_PARAS: usize = 3;
/// Fewest body words a lint-cleaned (legacy) transcript note needs.
const MIN_LEGACY_WORDS: usize = 50;

static ANY_EMBED_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| re(r"!\[[^\]]*\]\(<?([^)\s>]+)>?[^)]*\)|!\[\[([^\]|#]+)"));

/// How a note shows that it carries a video's transcript (plan intake-followups-mail-youtube
/// WP-4: "a clipping of the video, not a note that happens to embed it").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptEvidence {
    /// A `## Transcript` heading followed (blank lines allowed) by at least three `**m:ss**`
    /// paragraphs — the shape a fresh clip has before `lint` cleans it.
    Structured,
    /// An already-cleaned note: no `## Transcript` heading, no YouTube embed left, at least 50
    /// body words, and a kind with `cleanup_transcript = true` (the caller's `cleanup_kind`).
    LegacyCleaned,
}

/// Whether `content` carries transcript text (see [`TranscriptEvidence`]). Pure: the video
/// identity (the note's own `source`) and the kind lookup are the caller's; `cleanup_kind` says
/// whether one of the note's kinds has `cleanup_transcript = true`.
#[must_use]
pub fn transcript_evidence(content: &str, cleanup_kind: bool) -> Option<TranscriptEvidence> {
    let (_, body) = ofm::frontmatter::split_frontmatter(content);
    let (Some(header_re), Some(split_re)) =
        (TRANSCRIPT_HEADER_RE.as_ref(), PARAGRAPH_SPLIT_RE.as_ref())
    else {
        return None;
    };
    if let Some(h) = header_re.find(body) {
        let after = &body[h.end()..];
        let after = match LEADING_BLANK_LINES_RE.as_ref() {
            Some(r) => r.replace(after, "").into_owned(),
            None => after.to_owned(),
        };
        let stamped = split_re
            .split(&after)
            .take_while(|p| is_timestamp_para(p))
            .count();
        return (stamped >= MIN_TIMESTAMP_PARAS).then_some(TranscriptEvidence::Structured);
    }
    if !cleanup_kind || has_youtube_embed(body) {
        return None;
    }
    (crate::calculate_word_count(body) >= MIN_LEGACY_WORDS)
        .then_some(TranscriptEvidence::LegacyCleaned)
}

/// Whether `body` embeds a YouTube video (`![…](url)` or `![[url]]`).
fn has_youtube_embed(body: &str) -> bool {
    let Some(r) = ANY_EMBED_RE.as_ref() else {
        return false;
    };
    r.captures_iter(body).any(|c| {
        c.get(1)
            .or_else(|| c.get(2))
            .is_some_and(|u| cv_text::extract_video_id_from_url(u.as_str().trim()).is_some())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(n: usize) -> String {
        (0..n)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn structured_transcript_needs_three_stamped_paragraphs() {
        let clip = "---\nsource: https://youtu.be/dQw4w9WgXcQ\n---\n![](https://youtu.be/dQw4w9WgXcQ)\n\nA long description.\n\n## Transcript\n\n**0:01** a\n\n**0:05** b\n\n**1:10** c\n";
        assert_eq!(
            transcript_evidence(clip, false),
            Some(TranscriptEvidence::Structured)
        );
        let two = "![](https://youtu.be/dQw4w9WgXcQ)\n## Transcript\n\n**0:01** a\n\n**0:05** b\n";
        assert_eq!(transcript_evidence(two, true), None);
        let desc_only = format!("![](https://youtu.be/dQw4w9WgXcQ)\n\n{}\n", words(200));
        assert_eq!(transcript_evidence(&desc_only, false), None);
    }

    #[test]
    fn legacy_cleaned_needs_kind_words_and_no_embed() {
        let cleaned = format!("---\ntags: [post]\n---\n{}\n", words(60));
        assert_eq!(
            transcript_evidence(&cleaned, true),
            Some(TranscriptEvidence::LegacyCleaned)
        );
        assert_eq!(transcript_evidence(&cleaned, false), None);
        let short = format!("---\ntags: [post]\n---\n{}\n", words(49));
        assert_eq!(transcript_evidence(&short, true), None);
        let embed = format!(
            "![](https://www.youtube.com/watch?v=dQw4w9WgXcQ)\n\n{}\n",
            words(60)
        );
        assert_eq!(transcript_evidence(&embed, true), None);
        let wiki = format!("![[https://youtu.be/dQw4w9WgXcQ]]\n\n{}\n", words(60));
        assert_eq!(transcript_evidence(&wiki, true), None);
        let image = format!("![](https://ex.test/a.png)\n\n{}\n", words(60));
        assert_eq!(
            transcript_evidence(&image, true),
            Some(TranscriptEvidence::LegacyCleaned)
        );
    }

    #[test]
    fn cleans_timestamps_and_annotations() {
        let src = "---\nsource: https://youtu.be/dQw4w9WgXcQ\n---\n![](https://youtu.be/dQw4w9WgXcQ)\n## Transcript\n\n**0:01** · [Music] hello   there\n\n**0:05** [Applause] second [link](x) part\n\n## Notes\nmine\n";
        let out = cleanup_youtube_transcript(src);
        assert_eq!(
            out,
            "---\nsource: https://youtu.be/dQw4w9WgXcQ\n---\nhello there\n\nsecond [link](x) part\n\n## Notes\nmine\n"
        );
    }

    #[test]
    fn keeps_content_between_embed_and_transcript() {
        let src = "---\nsource: https://youtu.be/dQw4w9WgXcQ\n---\n![](https://www.youtube.com/watch?v=dQw4w9WgXcQ)\n\nA description line.\n\n> a quote\n\n## Transcript\n\n**0:01** · hello \\[music\\] there\n\n**0:05** second\n";
        let out = cleanup_youtube_transcript(src);
        assert_eq!(
            out,
            "---\nsource: https://youtu.be/dQw4w9WgXcQ\n---\nA description line.\n\n> a quote\n\nhello there\n\nsecond\n"
        );
    }

    #[test]
    fn non_matching_shapes_unchanged() {
        let a = "![](https://example.com/x.png)\n## Transcript\n\n**0:01** hi\n";
        assert_eq!(cleanup_youtube_transcript(a), a);
        let b = "![](https://youtu.be/dQw4w9WgXcQ)\n## Transcript\n\nno stamp\n";
        assert_eq!(cleanup_youtube_transcript(b), b);
        let c = "plain note\n";
        assert_eq!(cleanup_youtube_transcript(c), c);
    }
}
