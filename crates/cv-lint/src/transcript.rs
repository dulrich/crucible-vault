//! YouTube transcript cleanup — Crucible `cleanupYoutubeTranscript`
//! (`https://github.com/dulrich/obsidian-crucible/blob/master/src/lint.ts:11-84`).
//!
//! A note whose body opens with a YouTube `![](url)` embed followed by `## Transcript` and
//! `**m:ss**`-prefixed paragraphs gets those paragraphs' timestamps and `[Music]`-style
//! annotations removed and whitespace collapsed; trailing non-transcript paragraphs are kept.
//! Deviations: the frontmatter is located with `ofm::frontmatter::split_frontmatter` instead of
//! Crucible's `FRONTMATTER_REGEX`, the `(?!\()` look-ahead is checked by hand (the `regex` crate
//! has none), and video-id recognition reuses `cv_text::extract_video_id_from_url`. Nothing
//! Obsidian-induced to drop (the `vault.modify` write is the caller's).

use std::sync::LazyLock;

use regex::Regex;

fn re(p: &str) -> Option<Regex> {
    Regex::new(p).ok()
}

static YT_EMBED_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| re(r"^([ \t]*\r?\n)*[ \t]*!\[\]\(([^)\s]+)\)[ \t]*\r?\n"));
static TRANSCRIPT_HEADER_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| re(r"^([ \t]*\r?\n)*[ \t]*##[ \t]+Transcript[ \t]*\r?\n"));
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
    let mut result = cleaned.join("\n\n");
    if !trailing.is_empty() {
        result.push_str("\n\n");
        result.push_str(trailing);
    }
    format!("{fm_text}{result}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn non_matching_shapes_unchanged() {
        let a = "![](https://example.com/x.png)\n## Transcript\n\n**0:01** hi\n";
        assert_eq!(cleanup_youtube_transcript(a), a);
        let b = "![](https://youtu.be/dQw4w9WgXcQ)\n## Transcript\n\nno stamp\n";
        assert_eq!(cleanup_youtube_transcript(b), b);
        let c = "plain note\n";
        assert_eq!(cleanup_youtube_transcript(c), c);
    }
}
