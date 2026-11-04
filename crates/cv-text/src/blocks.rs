//! The note block grammar shared by the transcript lint (`cv-note`), the transcript refine and the enrich renderer (`cv-ingest`): the line model, managed `%% name %%` … `%% /name %%` blocks, code fences and the `## Transcript` heading (plan tn-review-2026-11-04-remediation WP-R2, finding G1).
//!
//! Rules:
//! - **Fences**: a line whose trimmed start is at least three backticks or tildes opens a fence; it closes only on a line of the same character with at least the opening length. An unclosed fence runs to the end of the text.
//! - **Managed blocks**: a single-line comment `%% name %%` opens a block that closes at the next `%% /name %%` line, unless another `%% name %%` opener comes first (then the first opener is unpaired, and the caller treats it as ordinary text).
//! - **Transcript heading** (Decision 1): `## Transcript`, case-insensitive, leading whitespace allowed, never inside a fence.

/// Name of the generated enrich blocks (`%% cv-enrich %%` … `%% /cv-enrich %%`).
pub const ENRICH_NAME: &str = "cv-enrich";
/// Marker opening a generated enrich block.
pub const ENRICH_OPEN: &str = "%% cv-enrich %%";
/// Marker closing a generated enrich block.
pub const ENRICH_CLOSE: &str = "%% /cv-enrich %%";
/// Marker line preceding an enrich-inserted chapter heading.
pub const ENRICH_HEADING: &str = "%% cv-enrich heading %%";

/// One line of a text: byte span (`end` includes the newline) and its text without the line ending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Line<'a> {
    pub start: usize,
    pub end: usize,
    pub text: &'a str,
}

impl Line<'_> {
    /// Whether the line ends with a newline (the last line of a text may not).
    #[must_use]
    pub fn has_newline(&self) -> bool {
        self.end > self.start + self.text.len()
    }
}

/// The lines of `text`, `\n`- or `\r\n`-terminated (a trailing newline yields no empty last line).
#[must_use]
pub fn lines(text: &str) -> Vec<Line<'_>> {
    let mut out = Vec::new();
    let mut start = 0;
    for seg in text.split_inclusive('\n') {
        let end = start + seg.len();
        let t = seg.strip_suffix('\n').unwrap_or(seg);
        let t = t.strip_suffix('\r').unwrap_or(t);
        out.push(Line {
            start,
            end,
            text: t,
        });
        start = end;
    }
    out
}

/// The inner text of a single-line `%% x %%` comment, trimmed, when trimmed `t` is exactly one.
#[must_use]
pub fn single_comment(t: &str) -> Option<&str> {
    if t.len() < 4 {
        return None;
    }
    let inner = t.strip_prefix("%%")?.strip_suffix("%%")?;
    (!inner.contains("%%")).then(|| inner.trim())
}

/// The index of the `%% /name %%` line closing the managed block opened by `lines[i]` (`%% name %%`), when the pair is well-formed (see the module rules). `None` when `lines[i]` is no opener or the block is unterminated.
#[must_use]
pub fn managed_close<S: AsRef<str>>(lines: &[S], i: usize) -> Option<usize> {
    let name = single_comment(lines.get(i)?.as_ref().trim())
        .filter(|n| !n.is_empty() && !n.starts_with('/'))?;
    let j = (i + 1..lines.len()).find(|&j| {
        single_comment(lines[j].as_ref().trim())
            .is_some_and(|c| c == name || c.strip_prefix('/') == Some(name))
    })?;
    (single_comment(lines[j].as_ref().trim()) != Some(name)).then_some(j)
}

impl AsRef<str> for Line<'_> {
    fn as_ref(&self) -> &str {
        self.text
    }
}

/// The fence a line opens or closes: its character and run length.
fn fence_of(line: &str) -> Option<(char, usize)> {
    let t = line.trim_start();
    let c = t.chars().next().filter(|&c| c == '`' || c == '~')?;
    let n = t.chars().take_while(|&x| x == c).count();
    (n >= 3).then_some((c, n))
}

/// The fence tracker step: the new open fence after `line`, and whether `line` is a fence-block line.
fn step(open: Option<(char, usize)>, line: &str) -> (Option<(char, usize)>, bool) {
    match (open, fence_of(line)) {
        (None, Some(f)) => (Some(f), true),
        (Some((c, n)), Some((d, m))) if c == d && m >= n => (None, true),
        (None, None) => (None, false),
        (o, _) => (o, true),
    }
}

/// Per line, whether it is inside a fenced code block (fence lines included).
#[must_use]
pub fn fenced_lines<S: AsRef<str>>(lines: &[S]) -> Vec<bool> {
    let mut open = None;
    lines
        .iter()
        .map(|l| {
            let (o, inside) = step(open, l.as_ref());
            open = o;
            inside
        })
        .collect()
}

/// Byte ranges of fenced code blocks (fence lines included; an unclosed fence runs to the end).
#[must_use]
pub fn fenced_ranges(content: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut open = None;
    let mut start = 0;
    for l in lines(content) {
        let (o, _) = step(open, l.text);
        match (open, o) {
            (None, Some(_)) => start = l.start,
            (Some(_), None) => out.push((start, l.end)),
            _ => {}
        }
        open = o;
    }
    if open.is_some() {
        out.push((start, content.len()));
    }
    out
}

/// Whether `text` (one line, no line ending) is a `## Transcript` heading line (Decision 1: case-insensitive, leading whitespace allowed; fences are the caller's, see [`transcript_heading`]).
#[must_use]
pub fn is_transcript_heading(text: &str) -> bool {
    text.trim_start()
        .strip_prefix("##")
        .filter(|r| r.starts_with([' ', '\t']))
        .is_some_and(|r| r.trim().eq_ignore_ascii_case("transcript"))
}

/// The index of the first `## Transcript` heading line outside a fence, at or after line `from`.
#[must_use]
pub fn transcript_heading(lines: &[Line<'_>], from: usize) -> Option<usize> {
    let fenced = fenced_lines(lines);
    (from..lines.len()).find(|&i| !fenced[i] && is_transcript_heading(lines[i].text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_model_spans() {
        let ls = lines("a\r\nb\nc");
        assert_eq!(ls.len(), 3);
        assert_eq!((ls[0].start, ls[0].end, ls[0].text), (0, 3, "a"));
        assert_eq!((ls[2].start, ls[2].end, ls[2].text), (5, 6, "c"));
        assert!(ls[0].has_newline() && !ls[2].has_newline());
    }

    #[test]
    fn managed_pairing() {
        let ls = ["%% cv-enrich %%", "x", "%% /cv-enrich %%"];
        assert_eq!(managed_close(&ls, 0), Some(2));
        assert_eq!(managed_close(&ls, 1), None);
        let reopened = ["%% cv-enrich %%", "%% cv-enrich %%", "%% /cv-enrich %%"];
        assert_eq!(managed_close(&reopened, 0), None);
        assert_eq!(managed_close(&reopened, 1), Some(2));
        assert_eq!(managed_close(&["%% a %%", "%% /b %%"], 0), None);
        assert_eq!(single_comment(ENRICH_OPEN), Some(ENRICH_NAME));
    }

    #[test]
    fn fences_match_kind_and_length() {
        let ls = ["````", "```", "## Transcript", "````", "after"];
        assert_eq!(fenced_lines(&ls), [true, true, true, true, false]);
        let ls = ["~~~", "```", "x", "~~~", "y"];
        assert_eq!(fenced_lines(&ls), [true, true, true, true, false]);
        let text = "a\n```\nb\n```\n```\nc\n```\nd\n~~~\ne";
        let r = fenced_ranges(text);
        assert_eq!(r.len(), 3);
        assert_eq!(&text[r[0].0..r[0].1], "```\nb\n```\n");
        assert_eq!(&text[r[1].0..r[1].1], "```\nc\n```\n");
        assert_eq!(&text[r[2].0..r[2].1], "~~~\ne");
    }

    #[test]
    fn transcript_heading_rule() {
        for yes in ["## Transcript", "  ## transcript ", "##\tTRANSCRIPT"] {
            assert!(is_transcript_heading(yes), "{yes}");
        }
        for no in [
            "##Transcript",
            "### Transcript",
            "# Transcript",
            "## Transcripts",
        ] {
            assert!(!is_transcript_heading(no), "{no}");
        }
        let body = "````\n```\n## Transcript\n````\n## transcript\n";
        assert_eq!(transcript_heading(&lines(body), 0), Some(4));
    }
}
