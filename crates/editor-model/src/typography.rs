//! Reading's typographic display substitutions (`->` shown as `→`, ...) and the coordinate
//! translation between the displayed text and the ASCII source it stands for.

use std::borrow::Cow;
use std::ops::Range;

/// Round-25 WP-3: one typographic display substitution in a run — the display byte range of
/// the glyph and the source (ASCII) byte range it stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sub {
    pub disp: Range<usize>,
    pub src: Range<usize>,
}

/// A run's displayed text with the source text it stands for and the substitutions between them.
///
/// `text` is the display form (what is shaped and hit-tested); [`Display::src`] is the ASCII
/// source (what copy and find see); [`Display::to_source`] and [`Display::to_display`] translate
/// byte offsets between the two.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Display {
    /// The displayed text (selection byte offsets index into it).
    pub text: String,
    /// The source text when it differs from `text` (empty when `subs` is empty).
    source: String,
    /// The display substitutions, in order (empty: `text` is the source).
    pub subs: Vec<Sub>,
}

impl Display {
    /// Text shown as written.
    pub fn plain(text: String) -> Self {
        Self {
            text,
            source: String::new(),
            subs: Vec::new(),
        }
    }

    /// `text` displayed for `source` through `subs` (an empty `subs` means `text` is the source).
    pub fn new(text: String, mut source: String, subs: Vec<Sub>) -> Self {
        if subs.is_empty() {
            source.clear();
        }
        Self { text, source, subs }
    }

    /// The run's source text (what copy and find see).
    pub fn src(&self) -> &str {
        if self.subs.is_empty() {
            &self.text
        } else {
            &self.source
        }
    }

    /// Display byte `d` → source byte (a `d` inside a glyph floors to its source start).
    pub fn to_source(&self, d: usize) -> usize {
        let mut out = d;
        for s in &self.subs {
            if d >= s.disp.end {
                out = d - s.disp.end + s.src.end;
            } else if d > s.disp.start {
                return s.src.start;
            } else {
                break;
            }
        }
        out
    }

    /// Source byte `b` → display byte; inside a substituted sequence it rounds to the glyph's
    /// end when `up`, else its start (so a partial match paints the whole glyph).
    pub fn to_display(&self, b: usize, up: bool) -> usize {
        let mut out = b;
        for s in &self.subs {
            if b >= s.src.end {
                out = b - s.src.end + s.disp.end;
            } else if b > s.src.start {
                return if up { s.disp.end } else { s.disp.start };
            } else {
                break;
            }
        }
        out
    }
}
/// Round-25 WP-3: the Reading display form of prose `s` — `-->` `⟶`, `->` `→`, `---` `—`,
/// `--` `–`, `!=` `≠`, `>=` `≥`, `<=` `≤`, `...` `…`, longest first — and its substitutions
/// (piece-relative ranges). `<->`, `<--`, `<=>` stay (Inter ligates them,
/// `docs/spikes/inter-features/WP-0-FINDINGS.md`); URL tokens (`://`, `www.`) and `$inline
/// math$` are untouched.
pub fn substitute(s: &str) -> (Cow<'_, str>, Vec<Sub>) {
    const KEEP: [&str; 3] = ["<->", "<--", "<=>"];
    const TABLE: [(&str, &str); 8] = [
        ("-->", "⟶"),
        ("---", "—"),
        ("...", "…"),
        ("->", "→"),
        ("--", "–"),
        ("!=", "≠"),
        (">=", "≥"),
        ("<=", "≤"),
    ];
    if !s.contains(['-', '!', '>', '<', '.']) {
        return (Cow::Borrowed(s), Vec::new());
    }
    let skip = skip_ranges(s);
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut subs = Vec::new();
    let mut i = 0;
    'outer: while i < s.len() {
        if let Some(r) = skip.iter().find(|r| r.start == i) {
            out.push_str(&s[r.clone()]);
            i = r.end;
            continue;
        }
        let rest = &s[i..];
        if let Some(k) = KEEP.iter().find(|k| rest.starts_with(**k)) {
            out.push_str(k);
            i += k.len();
            continue;
        }
        for (from, to) in TABLE {
            if rest.starts_with(from) {
                let at = out.len();
                out.push_str(to);
                subs.push(Sub {
                    disp: at..out.len(),
                    src: i..i + from.len(),
                });
                i += from.len();
                continue 'outer;
            }
        }
        let n = if b[i].is_ascii() {
            1
        } else {
            rest.chars().next().map_or(1, char::len_utf8)
        };
        out.push_str(&s[i..i + n]);
        i += n;
    }
    if subs.is_empty() {
        (Cow::Borrowed(s), subs)
    } else {
        (Cow::Owned(out), subs)
    }
}

/// The byte ranges of `s` never substituted: URL tokens and `$inline math$`.
fn skip_ranges(s: &str) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut at = 0;
    for tok in s.split(char::is_whitespace) {
        if tok.contains("://") || tok.starts_with("www.") {
            out.push(at..at + tok.len());
        }
        // `split` on a char predicate: the separator is one char wide.
        at += tok.len();
        at += s[at..].chars().next().map_or(0, char::len_utf8);
    }
    let b = s.as_bytes();
    let mut i = 0;
    while let Some(o) = s[i..].find('$').map(|o| o + i) {
        let opens = b
            .get(o + 1)
            .is_some_and(|c| !c.is_ascii_whitespace() && *c != b'$');
        let close = opens
            .then(|| s[o + 1..].find('$').map(|c| c + o + 1))
            .flatten()
            .filter(|&c| !b[c - 1].is_ascii_whitespace());
        match close {
            Some(c) => {
                out.push(o..c + 1);
                i = c + 1;
            }
            None => i = o + 1,
        }
    }
    out.sort_by_key(|r| r.start);
    // Merge overlaps so each skip starts at a position the scan reaches.
    let mut merged: Vec<Range<usize>> = Vec::new();
    for r in out {
        match merged.last_mut() {
            Some(l) if r.start <= l.end => l.end = l.end.max(r.end),
            _ => merged.push(r),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disp(s: &str) -> String {
        substitute(s).0.into_owned()
    }

    #[test]
    fn substitution_table_longest_first_with_exclusions() {
        assert_eq!(disp("a -> b --> c"), "a → b ⟶ c");
        assert_eq!(disp("x -- y --- z"), "x – y — z");
        assert_eq!(disp("a != b >= c <= d..."), "a ≠ b ≥ c ≤ d…");
        assert_eq!(disp("---->"), "—→");
        // Inter ligates these itself.
        assert_eq!(
            disp("a <-> b <-- c <=> d <- e => f"),
            "a <-> b <-- c <=> d <- e => f"
        );
        // URLs and inline math are untouched.
        assert_eq!(
            disp("see https://x.y/a--b...c -> www.q.r/--> $a<=b$ ok"),
            "see https://x.y/a--b...c → www.q.r/--> $a<=b$ ok"
        );
        assert!(substitute("plain").1.is_empty());
    }
}
