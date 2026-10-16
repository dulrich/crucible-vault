//! URL extraction from note text (consumed by WP-3's `link_scan`).
//!
//! Port of Crucible `extractUrls` (`https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/utils/urlExtract.ts`):
//! fenced code blocks, inline code and wikilinks are blanked; Markdown-link targets and bare
//! `http(s)://` URLs are collected, trailing punctuation trimmed, deduplicated by raw URL.
//! Difference: one left-to-right scan returns URLs in first-occurrence order (Crucible listed
//! every Markdown-link URL before the bare ones); the set is the same. Canonicalization reuses
//! `cv_text::url_canon` — never re-derived here.

use cv_text::url_canon::{CanonicalizedUrl, canonicalize_url};

/// Raw URLs in `content`, in first-occurrence order, deduplicated.
#[must_use]
pub fn extract_urls(content: &str) -> Vec<String> {
    let stripped = strip_code_and_wikilinks(content);
    let mut out: Vec<String> = Vec::new();
    let bytes = stripped.as_bytes();
    let mut i = 0;
    while i < stripped.len() {
        let rest = &stripped[i..];
        let is_start = rest.starts_with("http://") || rest.starts_with("https://");
        let boundary = i == 0 || !is_word_byte(bytes[i - 1]);
        if !(is_start && boundary) {
            i += rest.chars().next().map_or(1, char::len_utf8);
            continue;
        }
        let end = rest
            .find(|c: char| {
                c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | '`' | ')' | ']')
            })
            .unwrap_or(rest.len());
        let url = trim_url(&rest[..end]);
        if url.split_once("://").is_some_and(|(_, h)| !h.is_empty())
            && !out.iter().any(|u| u == url)
        {
            out.push(url.to_owned());
        }
        i += end.max(1);
    }
    out
}

/// [`extract_urls`] canonicalized via `cv_text::url_canon`, deduplicated by canonical URL
/// (first occurrence wins); non-http(s)/unparseable URLs are dropped.
#[must_use]
pub fn extract_canonical_urls(content: &str) -> Vec<CanonicalizedUrl> {
    let mut out: Vec<CanonicalizedUrl> = Vec::new();
    for raw in extract_urls(content) {
        if let Some(c) = canonicalize_url(&raw)
            && !out.iter().any(|o| o.canonical == c.canonical)
        {
            out.push(c);
        }
    }
    out
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn trim_url(s: &str) -> &str {
    s.trim().trim_end_matches(|c: char| {
        matches!(c, '.' | ',' | ';' | ':' | ')' | ']' | '\'' | '"') || c.is_whitespace()
    })
}

/// Blanks fenced code blocks (a line starting with ```` ``` ```` or `~~~` toggles; unterminated runs
/// to the end), inline code spans and `[[wikilinks]]`.
fn strip_code_and_wikilinks(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut in_fence = false;
    for line in content.split_inclusive('\n') {
        let is_fence = line.starts_with("```") || line.starts_with("~~~");
        if is_fence || in_fence {
            if is_fence {
                in_fence = !in_fence;
            }
            out.push('\n');
            continue;
        }
        out.push_str(&strip_inline(line));
    }
    out
}

fn strip_inline(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('`')
            && let Some(end) = after.find(['`', '\n'])
            && after.as_bytes()[end] == b'`'
        {
            out.push(' ');
            rest = &after[end + 1..];
            continue;
        }
        if let Some(after) = rest.strip_prefix("[[")
            && let Some(end) = after.find("]]")
            && !after[..end].contains(']')
        {
            out.push(' ');
            rest = &after[end + 2..];
            continue;
        }
        let ch = rest.chars().next().unwrap_or(' ');
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_markdown_and_bare_urls_skipping_code_and_wikilinks() {
        let note = "See [post](https://a.example/p?x=1) and https://b.example/q.\n\
                    `https://code.example/` [[https://wiki.example]]\n\
                    ```\nhttps://fenced.example/\n```\n\
                    (https://c.example/r), again https://b.example/q\n\
                    xhttps://not.example/";
        assert_eq!(
            extract_urls(note),
            vec![
                "https://a.example/p?x=1",
                "https://b.example/q",
                "https://c.example/r"
            ]
        );
    }

    #[test]
    fn canonical_urls_dedupe_on_canonical_form() {
        let note = "https://youtu.be/dQw4w9WgXcQ and https://www.youtube.com/watch?v=dQw4w9WgXcQ&utm_source=x";
        let got = extract_canonical_urls(note);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].youtube_video_id.as_deref(), Some("dQw4w9WgXcQ"));
    }
}
