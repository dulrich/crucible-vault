//! YouTube transcript cleanup — Crucible `cleanupYoutubeTranscript`
//! (`https://github.com/dulrich/obsidian-crucible/blob/master/src/lint.ts:11-84`).
//!
//! A note whose body opens with a YouTube `![](url)` embed followed by `## Transcript` and
//! `**m:ss**`-prefixed paragraphs gets those paragraphs' timestamps and `[Music]`-style
//! annotations removed and whitespace collapsed; trailing non-transcript paragraphs are kept.
//! Deviations: the frontmatter is located with `ofm::frontmatter::split_frontmatter` instead of
//! Crucible's `FRONTMATTER_REGEX`, the `(?!\()` look-ahead is checked by hand (the `regex` crate
//! has none), video-id recognition reuses `cv_text::extract_video_id_from_url`, content between
//! the embed and `## Transcript` (e.g. a clipped description) is tolerated and dropped (Crucible
//! required the header directly after the embed; plan round-19-yt-cleanup-intake WP-2 deletes the
//! description, kept in the `_yt_metadata` note), leading `%% … %%` managed regions are skipped
//! and kept, the localized `[![](img)](url)` embed form is accepted, and an already-stripped body
//! (no embed) is cleaned too. Nothing
//! Obsidian-induced to drop (the `vault.modify` write is the caller's).
//!
//! [`transcript_evidence`] (plan intake-followups-mail-youtube WP-4, cv's own — Crucible ticked
//! tracker rows by hand) reuses these regexes to decide whether a note carries a transcript.

use std::sync::LazyLock;

use cv_text::blocks::{self, Line};
use regex::Regex;

fn re(p: &str) -> Option<Regex> {
    Regex::new(p).ok()
}

static YT_EMBED_LINE_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| re(r"^[ \t]*(?:!\[\]\(([^)\s]+)\)|\[!\[\]\([^)\s]+\)\]\(([^)\s]+)\))[ \t]*$"));
static LEADING_BLANK_LINES_RE: LazyLock<Option<Regex>> = LazyLock::new(|| re(r"^([ \t]*\r?\n)+"));
static TIMESTAMP_PARA_RE: LazyLock<Option<Regex>> = LazyLock::new(|| re(r"^\*\*\d+:\d+\*\*"));
static TIMESTAMP_PREFIX_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| re(r"^\*\*\d+:\d+\*\*[ \t·•|–—-]*"));
static BRACKET_ANNOTATION_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| re(r"\\?\[[A-Za-z][A-Za-z ]*\\?\]"));
static PARAGRAPH_SPLIT_RE: LazyLock<Option<Regex>> = LazyLock::new(|| re(r"\r?\n[ \t]*\r?\n\s*"));
static WS_RE: LazyLock<Option<Regex>> = LazyLock::new(|| re(r"\s+"));
static TRANSCRIPT_LINK_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| re(r"(?i)\[\[[ \t]*#[ \t]*Transcript[ \t]*(?:\|[^\]\n]*)?\]\]"));

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

/// Index of the last line of the `%% … %%` comment region opening at `lines[i]` (whose trimmed
/// text starts with `%%`). A single-line `%% name %%` paired with a later `%% /name %%` line spans
/// through that close marker ([`blocks::managed_close`]: the `%% cv-enrich %%` … `%% /cv-enrich %%`
/// managed block, whose `> [!abstract]` callout sits inside); an unpaired one is just its own line.
/// A bare `%%` opener runs to the next line containing `%%`, which must end with it. `None` for a
/// line that mixes a comment with visible text, or an unterminated comment (the caller then does
/// nothing).
fn comment_end(lines: &[Line<'_>], i: usize) -> Option<usize> {
    let t = lines.get(i)?.text.trim();
    if blocks::single_comment(t).is_some() {
        return Some(blocks::managed_close(lines, i).unwrap_or(i));
    }
    if t.get(2..).is_none_or(|rest| rest.contains("%%")) {
        return None;
    }
    let (j, l) = lines
        .iter()
        .enumerate()
        .skip(i + 1)
        .find(|(_, l)| l.text.contains("%%"))?;
    l.text.trim().ends_with("%%").then_some(j)
}

/// Whether `line` is a YouTube embed (`![](URL)` or `[![](IMG)](URL)`) whose URL carries a
/// recognized video id.
fn is_youtube_embed(line: &str) -> bool {
    YT_EMBED_LINE_RE
        .as_ref()
        .and_then(|r| r.captures(line))
        .and_then(|c| c.get(1).or_else(|| c.get(2)))
        .is_some_and(|u| cv_text::extract_video_id_from_url(u.as_str()).is_some())
}

/// Where a YouTube clip body's managed prefix, embed and `## Transcript` heading sit (plan
/// round-19-yt-cleanup-intake WP-2) — the one locator shared by [`strip_youtube_preamble`] and
/// [`cleanup_youtube_transcript`].
struct ClipShape {
    /// End of the last leading `%% … %%` region (0 when there is none); `body[..prefix_end]` is
    /// preserved byte-identical.
    prefix_end: usize,
    /// Start of the `## Transcript` heading line.
    header_start: usize,
    /// End of the heading line (newline included).
    header_end: usize,
}

/// `(prefix_end, next_line)`: skip leading blank lines and whole `%% … %%` regions; `prefix_end` is
/// the end of the last region (0 when none), `next_line` the index of the first other line. `None`
/// on an unterminated or mixed comment.
fn scan_prefix(lines: &[Line<'_>]) -> Option<(usize, usize)> {
    let mut prefix_end = 0;
    let mut i = 0;
    while let Some(line) = lines.get(i) {
        let t = line.text.trim();
        if t.is_empty() {
            i += 1;
        } else if t.starts_with("%%") {
            let j = comment_end(lines, i)?;
            prefix_end = lines.get(j)?.end;
            i = j + 1;
        } else {
            break;
        }
    }
    Some((prefix_end, i))
}

/// Whether a managed prefix links the note's transcript (`[[#Transcript]]` or
/// `[[#Transcript|alias]]`, case-insensitive) — the ToC callout `intake_enrich` writes. Plan
/// round-20-transcript-heading-auto-holds WP-1.
#[must_use]
pub fn prefix_links_transcript(prefix: &str) -> bool {
    TRANSCRIPT_LINK_RE
        .as_ref()
        .is_some_and(|r| r.is_match(prefix))
}

/// Re-insert the `## Transcript` heading a cleanup dropped: when the note's managed prefix links
/// `#Transcript` yet the body has no such heading, put `## Transcript` and a blank line directly
/// after the prefix. `None` when there is no prefix, no link, or the heading exists (so a second
/// pass is `None`). Plan round-20-transcript-heading-auto-holds WP-1.
#[must_use]
pub fn restore_transcript_heading(content: &str) -> Option<String> {
    let (_, body) = ofm::frontmatter::split_frontmatter(content);
    let fm_text = &content[..content.len() - body.len()];
    let lines = blocks::lines(body);
    if header_line(&lines, 0).is_some() {
        return None;
    }
    let (prefix_end, _) = scan_prefix(&lines)?;
    let prefix = &body[..prefix_end];
    if prefix.is_empty() || !prefix_links_transcript(prefix) {
        return None;
    }
    let rest = body[prefix_end..].trim_start_matches(['\n', '\r', ' ', '\t']);
    Some(format!("{fm_text}{prefix}\n## Transcript\n\n{rest}"))
}

/// The first `## Transcript` heading line at or after `from` (Decision 1's rule, [`blocks::transcript_heading`]: case-insensitive, leading whitespace allowed, never inside a code fence) that ends with a newline (a heading on the unterminated last line is no transcript header).
fn header_line(lines: &[Line<'_>], from: usize) -> Option<usize> {
    blocks::transcript_heading(lines, from).filter(|&h| lines[h].has_newline())
}

/// Skip leading blank lines and whole `%% … %%` regions, in any order. The first remaining line
/// must be a YouTube embed with a `## Transcript` heading later, or — when `embed_optional` — the
/// `## Transcript` heading itself (an already-stripped body).
fn locate_clip(body: &str, embed_optional: bool) -> Option<ClipShape> {
    let lines = blocks::lines(body);
    let (prefix_end, i) = scan_prefix(&lines)?;
    let first = lines.get(i)?;
    if is_youtube_embed(first.text) {
        let h = lines.get(header_line(&lines, i + 1)?)?;
        return Some(ClipShape {
            prefix_end,
            header_start: h.start,
            header_end: h.end,
        });
    }
    if embed_optional && header_line(&lines, i) == Some(i) {
        return Some(ClipShape {
            prefix_end,
            header_start: first.start,
            header_end: first.end,
        });
    }
    None
}

/// Remove a YouTube clip's thumbnail embed and the clipped description below it (plan
/// round-19-yt-cleanup-intake WP-2; D29's one delete-not-fold exception — the description stays
/// in the linked `_yt_metadata` note and frontmatter `description`). Takes the **body only** (the
/// caller splits the frontmatter off). Leading blank lines and `%% … %%` regions (the
/// `%% cv-enrich %%` managed block) are skipped and kept byte-identical; the first other line must
/// be `![](URL)` or `[![](IMG)](URL)` with a recognized video id, followed later by
/// `## Transcript`. Everything from the embed line up to the heading is dropped, leaving one blank
/// line between a non-empty prefix and the heading. Nothing at or after the heading is touched.
/// `None` when the shape is absent (so a second pass is `None`).
#[must_use]
pub fn strip_youtube_preamble(body: &str) -> Option<String> {
    let shape = locate_clip(body, false)?;
    let mut out = String::with_capacity(body.len());
    out.push_str(&body[..shape.prefix_end]);
    if shape.prefix_end > 0 {
        out.push('\n');
    }
    out.push_str(&body[shape.header_start..]);
    Some(out)
}

/// Clean the transcript; returns the input unchanged when the note does not match the shape.
/// Plan round-19-yt-cleanup-intake WP-2: the embed is found after leading `%% … %%` managed
/// regions (kept byte-identical), in either embed form; the embed and the clipped description are
/// dropped; a body already stripped (`## Transcript` directly after the managed prefix) is cleaned
/// too.
pub fn cleanup_youtube_transcript(content: &str) -> String {
    let (_, body) = ofm::frontmatter::split_frontmatter(content);
    let fm_text = &content[..content.len() - body.len()];
    let Some(split_re) = PARAGRAPH_SPLIT_RE.as_ref() else {
        return content.to_owned();
    };
    let Some(shape) = locate_clip(body, true) else {
        return content.to_owned();
    };
    let prefix = &body[..shape.prefix_end];
    let after_header = &body[shape.header_end..];
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
    let mut result = String::from(prefix);
    if !prefix.is_empty() {
        result.push('\n');
    }
    if prefix_links_transcript(prefix) {
        // Plan round-20-transcript-heading-auto-holds WP-1: the ToC links the heading — keep it.
        result.push_str(&body[shape.header_start..shape.header_end]);
        result.push('\n');
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
    /// An already-cleaned note: no (or an unstamped, kept) `## Transcript` heading, no YouTube embed left, at least 50
    /// body words, and a kind with `cleanup_transcript = true` (the caller's `cleanup_kind`).
    LegacyCleaned,
}

/// Whether `content` carries transcript text (see [`TranscriptEvidence`]). Pure: the video
/// identity (the note's own `source`) and the kind lookup are the caller's; `cleanup_kind` says
/// whether one of the note's kinds has `cleanup_transcript = true`.
#[must_use]
pub fn transcript_evidence(content: &str, cleanup_kind: bool) -> Option<TranscriptEvidence> {
    let (_, body) = ofm::frontmatter::split_frontmatter(content);
    let split_re = PARAGRAPH_SPLIT_RE.as_ref()?;
    let lines = blocks::lines(body);
    if let Some(h) = header_line(&lines, 0) {
        let after = &body[lines[h].end..];
        let after = match LEADING_BLANK_LINES_RE.as_ref() {
            Some(r) => r.replace(after, "").into_owned(),
            None => after.to_owned(),
        };
        let stamped = split_re
            .split(&after)
            .take_while(|p| is_timestamp_para(p))
            .count();
        if stamped >= MIN_TIMESTAMP_PARAS {
            return Some(TranscriptEvidence::Structured);
        }
        // Plan round-20-transcript-heading-auto-holds WP-1: a kept heading over cleaned
        // (unstamped) text falls through to the legacy test.
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
            "---\nsource: https://youtu.be/dQw4w9WgXcQ\n---\nhello there\n\nsecond\n"
        );
    }

    const ENRICHED: &str =
        "\n%% cv-enrich %%\n> [!abstract]- Summary\n> gist\n\n%% /cv-enrich %%\n%%\nmulti\n%%\n";

    #[test]
    fn strip_removes_localized_embed_and_description() {
        let body = format!(
            "{ENRICHED}\n[![](Clippings/_attachments/t.jpg)](https://www.youtube.com/watch?v=dQw4w9WgXcQ)\n\nDesc line.\n\nMore desc.\n\n## Transcript\n\n**0:01** hi\n"
        );
        let out = strip_youtube_preamble(&body).expect("strips");
        assert_eq!(out, format!("{ENRICHED}\n## Transcript\n\n**0:01** hi\n"));
        assert_eq!(strip_youtube_preamble(&out), None, "idempotent");
        let bare = "![](https://youtu.be/dQw4w9WgXcQ)\ndesc\n## Transcript\nx\n";
        assert_eq!(
            strip_youtube_preamble(bare).as_deref(),
            Some("## Transcript\nx\n")
        );
    }

    #[test]
    fn strip_needs_full_shape() {
        for body in [
            "plain\n## Transcript\nx\n",
            "![](https://ex.test/a.png)\ndesc\n## Transcript\nx\n",
            "![](https://youtu.be/dQw4w9WgXcQ)\ndesc only\n",
            "%% cv-enrich %%\n> [!abstract]- no close\n![](https://youtu.be/dQw4w9WgXcQ)\n## Transcript\nx\n",
            "%%\nunterminated\n![](https://youtu.be/dQw4w9WgXcQ)\n## Transcript\nx\n",
            "intro\n![](https://youtu.be/dQw4w9WgXcQ)\n## Transcript\nx\n",
        ] {
            assert_eq!(strip_youtube_preamble(body), None, "{body:?}");
        }
    }

    #[test]
    fn cleanup_enriched_localized_short() {
        let fm = "---\nsource: https://youtu.be/dQw4w9WgXcQ\n---\n";
        let src = format!(
            "{fm}{ENRICHED}[![](a.jpg)](https://youtu.be/dQw4w9WgXcQ)\nDesc.\n\n## Transcript\n\n**0:01** [Music] hi\n\n**0:04** there\n"
        );
        let out = cleanup_youtube_transcript(&src);
        assert_eq!(out, format!("{fm}{ENRICHED}\nhi\n\nthere\n"));
        assert_eq!(
            cleanup_youtube_transcript(&out),
            out,
            "second pass unchanged"
        );
        let stripped = format!("{fm}{ENRICHED}\n## Transcript\n\n**0:01** hi\n\n**0:04** there\n");
        assert_eq!(cleanup_youtube_transcript(&stripped), out);
        let other = format!(
            "{fm}{ENRICHED}[![](a.jpg)](https://ex.test/v)\nDesc.\n\n## Transcript\n\n**0:01** hi\n"
        );
        assert_eq!(cleanup_youtube_transcript(&other), other);
    }

    const FM: &str = "---\nsource: https://youtu.be/dQw4w9WgXcQ\n---\n";
    const TOC: &str =
        "\n%% cv-enrich %%\n> [!abstract]- Summary\n> [[#Transcript]]\n\n%% /cv-enrich %%\n";

    fn toc_clip(toc: &str) -> String {
        format!(
            "{FM}{toc}[![](a.jpg)](https://youtu.be/dQw4w9WgXcQ)\nDesc.\n\n## Transcript\n\n**0:01** [Music] hi\n\n**0:04** there\n\n**0:09** more\n"
        )
    }

    #[test]
    fn cleanup_keeps_heading_when_prefix_links_it() {
        let out = cleanup_youtube_transcript(&toc_clip(TOC));
        assert_eq!(
            out,
            format!("{FM}{TOC}\n## Transcript\n\nhi\n\nthere\n\nmore\n")
        );
        assert_eq!(
            cleanup_youtube_transcript(&out),
            out,
            "second pass unchanged"
        );
        let alias = TOC.replace("[[#Transcript]]", "[[#transcript|jump]]");
        let out = cleanup_youtube_transcript(&toc_clip(&alias));
        assert!(out.contains("\n## Transcript\n\nhi\n"), "{out}");
        let plain = TOC.replace("[[#Transcript]]", "no link");
        let out = cleanup_youtube_transcript(&toc_clip(&plain));
        assert_eq!(out, format!("{FM}{plain}\nhi\n\nthere\n\nmore\n"));
    }

    #[test]
    fn restore_heading_after_prefix_and_idempotent() {
        let dropped = format!("{FM}{TOC}\nhi\n\nthere\n");
        let out = restore_transcript_heading(&dropped).expect("restores");
        assert_eq!(out, format!("{FM}{TOC}\n## Transcript\n\nhi\n\nthere\n"));
        assert_eq!(restore_transcript_heading(&out), None);
        let unlinked = format!("{FM}{}\nhi\n", TOC.replace("[[#Transcript]]", "x"));
        assert_eq!(restore_transcript_heading(&unlinked), None);
        assert_eq!(restore_transcript_heading("plain\n"), None);
    }

    #[test]
    fn evidence_tracks_cleanup_and_restore() {
        let long = words(60);
        let before = format!(
            "{FM}{TOC}[![](a.jpg)](https://youtu.be/dQw4w9WgXcQ)\n\n## Transcript\n\n**0:01** {long}\n\n**0:04** b\n\n**0:09** c\n"
        );
        assert_eq!(
            transcript_evidence(&before, true),
            Some(TranscriptEvidence::Structured)
        );
        let cleaned = cleanup_youtube_transcript(&before);
        assert!(cleaned.contains("\n## Transcript\n\n"), "{cleaned}");
        assert_eq!(
            transcript_evidence(&cleaned, true),
            Some(TranscriptEvidence::LegacyCleaned)
        );
        assert_eq!(transcript_evidence(&cleaned, false), None);
        let dropped = cleaned.replace("## Transcript\n\n", "");
        let restored = restore_transcript_heading(&dropped).expect("restores");
        assert_eq!(
            transcript_evidence(&restored, true),
            Some(TranscriptEvidence::LegacyCleaned)
        );
        let embed = format!("![](https://youtu.be/dQw4w9WgXcQ)\n## Transcript\n\n{long}\n");
        assert_eq!(transcript_evidence(&embed, true), None);
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

    /// Decision 1 (plan tn-review-2026-11-04-remediation WP-R2): a `## Transcript` line inside a
    /// ```` fence that holds a ``` line is no heading.
    #[test]
    fn heading_inside_nested_fence_is_ignored() {
        let src = "![](https://youtu.be/dQw4w9WgXcQ)\n````\n```\n## Transcript\n\n**0:01** a\n\n**0:05** b\n\n**1:10** c\n````\n";
        assert_eq!(cleanup_youtube_transcript(src), src);
        assert_eq!(transcript_evidence(src, false), None);
    }

    /// Decision 1: the lint's heading match is case-insensitive.
    #[test]
    fn lowercase_transcript_heading_is_cleaned() {
        let src = "![](https://youtu.be/dQw4w9WgXcQ)\n## transcript\n\n**0:01** [Music] hi\n\n**0:05** there\n";
        assert_eq!(cleanup_youtube_transcript(src), "hi\n\nthere\n");
    }
}
