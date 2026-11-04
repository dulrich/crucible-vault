//! Obsidian `%% … %%` comments: the byte ranges Reading never displays.
//!
//! A comment runs from an opening `%%` to the next `%%`, on one line or across lines (the
//! `cv-enrich` markers `%% cv-enrich heading %%`, `%% cv-enrich %%` … are one-line comments).
//! Obsidian hides comments in reading view; the comrak-based OFM parser has no comment node, so
//! the render lowering blanks these ranges before parsing (keeping every newline, so block line
//! numbers are unchanged) and find treats them as invisible.
//!
//! Not comments: `%%` inside a fenced code block or an inline code span (same line), and an
//! opening `%%` with no closing one — that stays visible text rather than hiding the rest of the
//! note (the `cv_ingest::enrich` rule for an unterminated marker).

use std::borrow::Cow;
use std::ops::Range;

// Fences follow the highlighter's one CommonMark rule, so Reading and Source agree on what is
// code (plan tn-review-2026-11-04-remediation E7).
use crate::highlight::{fence_closes, fence_open};

/// Byte ranges (delimiters included) of every closed `%% … %%` comment in `src`, in order.
pub fn comment_ranges(src: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut open: Option<usize> = None;
    let mut fenced: Option<(u8, usize)> = None;
    let mut at = 0;
    for line in src.split_inclusive('\n') {
        let base = at;
        at += line.len();
        let text = line.trim_end_matches(['\n', '\r']);
        if open.is_none() {
            if let Some((ch, len)) = fenced {
                if fence_closes(text, ch, len) {
                    fenced = None;
                }
                continue;
            }
            if let Some(f) = fence_open(text) {
                fenced = Some(f);
                continue;
            }
        }
        let b = text.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if open.is_none() && b[i] == b'`' {
                // An inline code span: skip to the closing run of the same length on this line.
                let n = b[i..].iter().take_while(|&&c| c == b'`').count();
                let after = i + n;
                let mut j = after;
                let mut close = None;
                while j < b.len() {
                    if b[j] == b'`' {
                        let m = b[j..].iter().take_while(|&&c| c == b'`').count();
                        if m == n {
                            close = Some(j + m);
                            break;
                        }
                        j += m;
                    } else {
                        j += 1;
                    }
                }
                i = close.unwrap_or(after);
                continue;
            }
            if b[i] == b'%' && b.get(i + 1) == Some(&b'%') {
                match open.take() {
                    Some(s) => out.push(s..base + i + 2),
                    None => open = Some(base + i),
                }
                i += 2;
                continue;
            }
            i += 1;
        }
    }
    out
}

/// `src` with every comment's bytes removed except its newlines (line numbers are preserved);
/// borrowed when there is no comment.
pub fn blank_comments(src: &str) -> Cow<'_, str> {
    let ranges = comment_ranges(src);
    if ranges.is_empty() {
        return Cow::Borrowed(src);
    }
    let mut out = String::with_capacity(src.len());
    let mut at = 0;
    for r in ranges {
        out.push_str(&src[at..r.start]);
        out.extend(src[r.clone()].chars().filter(|&c| c == '\n'));
        at = r.end;
    }
    out.push_str(&src[at..]);
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hidden(src: &str) -> Vec<&str> {
        comment_ranges(src).into_iter().map(|r| &src[r]).collect()
    }

    #[test]
    fn line_inline_and_multiline_comments() {
        assert_eq!(
            hidden("%% cv-enrich heading %%\n## H\n"),
            ["%% cv-enrich heading %%"]
        );
        assert_eq!(hidden("a %%x%% b %%y%%\n"), ["%%x%%", "%%y%%"]);
        assert_eq!(hidden("a\n%%\nhid\nden\n%%\nb\n"), ["%%\nhid\nden\n%%"]);
        assert_eq!(
            hidden("%% cv-enrich %%\n> [!removed]-\n> x\n\n%% /cv-enrich %%\n"),
            ["%% cv-enrich %%", "%% /cv-enrich %%"]
        );
    }

    #[test]
    fn code_and_unterminated_are_not_comments() {
        assert!(hidden("```\n%% in fence %%\n```\n").is_empty());
        assert!(hidden("~~~~\n%%\n~~~\n%%\n~~~~\n").is_empty());
        assert!(hidden("a `%%x%%` b\n").is_empty());
        assert!(hidden("``a ` %%x%%`` b\n").is_empty());
        assert!(hidden("50%% off\nno close\n").is_empty());
        // After the fence closes, comments count again; an unmatched backtick is plain text.
        assert_eq!(hidden("```\nx\n```\n%%c%%\n"), ["%%c%%"]);
        assert_eq!(hidden("a ` b %%c%%\n"), ["%%c%%"]);
    }

    /// E7: the CommonMark fence rule shared with the highlighter. A backtick in a backtick
    /// fence's info string makes the line text, so the comment after it is hidden; a
    /// tab-indented fence line inside a block is code content, not a close.
    #[test]
    fn fence_edges_follow_commonmark() {
        assert_eq!(hidden("```a`b\n%% draft %%\ntext\n"), ["%% draft %%"]);
        assert!(hidden("```\ncode\n\t```\n%% secret %%\n```\n").is_empty());
        assert_eq!(hidden("```\nx\n``` \t\n%%c%%\n"), ["%%c%%"]);
    }

    #[test]
    fn blanking_keeps_newlines() {
        let src = "a %%x%% b\n%%\nl1\nl2\n%%\n%% m %%\n## H\n";
        let out = blank_comments(src);
        assert_eq!(out, "a  b\n\n\n\n\n\n## H\n");
        assert_eq!(out.matches('\n').count(), src.matches('\n').count());
        assert!(matches!(blank_comments("plain\n"), Cow::Borrowed(_)));
    }
}
