//! Pure Reading logic: the source ↔ rendered mapping for find
//! (plan editor-round-4 WP-3) and the Reading selection model (`SelPos`, `Selection`, word /
//! grapheme boundaries, hard lines).

use unicode_segmentation::UnicodeSegmentation;

use crate::render::RenderModel;

/// The byte offset where 0-based `line` starts in `text` (`text.len()` past the end).
fn line_start(text: &str, line: usize) -> usize {
    if line == 0 {
        return 0;
    }
    text.match_indices('\n')
        .nth(line - 1)
        .map_or(text.len(), |(i, _)| i + 1)
}

/// plan editor-round-4 WP-3: byte ranges of a block's source that Reading never displays —
/// md-link destinations `](url "title")`, image `![alt](src)` wholes, wiki-link targets hidden
/// by an alias (`[[target|`), `[[`/`]]` brackets, embeds `![[…]]` and code-fence lines.
pub fn invisible_ranges(src: &str) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let bytes = src.as_bytes();
    // Code-fence lines (the info string is not painted; the fence is markup).
    let mut at = 0;
    for line in src.split_inclusive('\n') {
        let t = line.trim_start();
        let t = t.trim_start_matches(['>', ' ']);
        if t.starts_with("```") || t.starts_with("~~~") {
            out.push(at..at + line.len());
        }
        at += line.len();
    }
    // Wiki-links and embeds.
    let mut i = 0;
    while let Some(off) = src[i..].find("[[") {
        let s = i + off;
        let Some(len) = src[s..].find("]]") else {
            break;
        };
        let e = s + len + 2;
        let embed = s > 0 && bytes[s - 1] == b'!';
        if embed {
            out.push(s - 1..e);
        } else {
            match src[s..e].find('|') {
                Some(p) => out.push(s..s + p + 1),
                None => out.push(s..s + 2),
            }
            out.push(e - 2..e);
        }
        i = e;
    }
    // Md-link destinations (and whole images).
    let mut i = 0;
    while let Some(off) = src[i..].find("](") {
        let s = i + off;
        let mut depth = 0usize;
        let mut end = src.len();
        for (k, c) in src[s + 1..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = s + 1 + k + 1;
                        break;
                    }
                }
                '\n' => {
                    end = s + 1 + k;
                    break;
                }
                _ => {}
            }
        }
        let open = src[..s].rfind('[');
        match open {
            Some(o) if o > 0 && bytes[o - 1] == b'!' => out.push(o - 1..end),
            _ => out.push(s..end),
        }
        i = end.max(s + 2);
    }
    out
}

/// plan editor-round-4 WP-3: the rendered counterpart (top-level block, nth visible
/// occurrence) of the source match `r` of `q` in `text`; `None` = outside every block, or an
/// invisible match (no rendered counterpart).
pub fn source_counterpart(
    model: &RenderModel,
    text: &str,
    q: &str,
    r: &std::ops::Range<usize>,
) -> Option<(usize, usize)> {
    let (line, _) = crate::find::line_col(text, r.start);
    let b = model.block_at_line(line)?;
    let lines = &model.block_lines[b];
    let from = line_start(text, lines.start);
    let to = line_start(text, lines.end).max(from);
    let src = &text[from..to];
    let rel = r.start.checked_sub(from)?;
    let hidden = invisible_ranges(src);
    let visible =
        |m: &std::ops::Range<usize>| !hidden.iter().any(|h| m.start < h.end && h.start < m.end);
    let ms = crate::find::matches(src, q);
    let idx = ms.iter().position(|m| m.start >= rel)?;
    if !visible(&ms[idx]) {
        return None;
    }
    Some((b, ms[..idx].iter().filter(|m| visible(m)).count()))
}

/// A position in the Reading selection: run index (render order) and byte offset in its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct SelPos {
    pub run: usize,
    pub byte: usize,
}

impl SelPos {
    pub const fn new(run: usize, byte: usize) -> Self {
        Self { run, byte }
    }
}

/// A Reading selection: where it started (`anchor`) and where it is extended to (`focus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Selection {
    pub anchor: SelPos,
    pub focus: SelPos,
}

impl Selection {
    pub const fn new(anchor: SelPos, focus: SelPos) -> Self {
        Self { anchor, focus }
    }

    /// `(start, end)` in document order.
    pub fn ordered(self) -> (SelPos, SelPos) {
        if self.anchor <= self.focus {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }

    /// Anchor and focus coincide (nothing selected).
    pub fn is_empty(self) -> bool {
        self.anchor == self.focus
    }

    /// `pos` lies inside a non-empty selection (ends included).
    pub fn contains(self, pos: SelPos) -> bool {
        let (lo, hi) = self.ordered();
        !self.is_empty() && lo <= pos && pos <= hi
    }

    /// The byte range of run `run` (text length `len`) inside the selection, if any part is.
    pub fn run_range(self, run: usize, len: usize) -> Option<(usize, usize)> {
        let (lo, hi) = self.ordered();
        if lo == hi || run < lo.run || run > hi.run {
            return None;
        }
        let a = if run == lo.run { lo.byte.min(len) } else { 0 };
        let b = if run == hi.run { hi.byte.min(len) } else { len };
        (a < b).then_some((a, b))
    }
}

/// The word (unicode word bounds) around `byte` in `text`.
pub fn word_at(text: &str, byte: usize) -> (usize, usize) {
    let byte = byte.min(text.len());
    let mut last = (text.len(), text.len());
    for (i, w) in text.split_word_bound_indices() {
        let end = i + w.len();
        if byte < end {
            // Whitespace runs are words of their own; prefer the word before a trailing space.
            if w.chars().all(char::is_whitespace) && byte == i && i > 0 {
                return last;
            }
            return (i, end);
        }
        last = (i, end);
    }
    last
}

/// The start of the grapheme before `byte` (0 at the start).
pub fn prev_grapheme(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())]
        .grapheme_indices(true)
        .next_back()
        .map_or(0, |(i, _)| i)
}

/// The end of the grapheme at `byte` (`text.len()` at the end).
pub fn next_grapheme(text: &str, byte: usize) -> usize {
    let byte = byte.min(text.len());
    text[byte..]
        .graphemes(true)
        .next()
        .map_or(text.len(), |g| byte + g.len())
}

/// The start of the word before `byte` (skips whitespace/punctuation; 0 at the start).
pub fn prev_word_boundary(text: &str, byte: usize) -> usize {
    let byte = byte.min(text.len());
    text.split_word_bound_indices()
        .take_while(|(i, _)| *i < byte)
        .filter(|(_, w)| w.chars().any(char::is_alphanumeric))
        .last()
        .map_or(0, |(i, _)| i)
}

/// The end of the word at or after `byte` (skips whitespace/punctuation; `text.len()` at the end).
pub fn next_word_boundary(text: &str, byte: usize) -> usize {
    let byte = byte.min(text.len());
    text.split_word_bound_indices()
        .find(|(i, w)| i + w.len() > byte && w.chars().any(char::is_alphanumeric))
        .map_or(text.len(), |(i, w)| i + w.len())
}

/// The visual line of `ranges` (line-local byte ranges) holding `local`.
pub fn line_containing(ranges: &[(usize, usize)], local: usize) -> Option<(usize, usize)> {
    let k = ranges.iter().rposition(|(s, _)| *s <= local).unwrap_or(0);
    ranges.get(k).copied()
}

/// Hard-line start offsets of `text` (always starts with 0).
pub fn hard_lines(text: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(text.match_indices('\n').map(|(i, _)| i + 1))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_counterpart_skips_invisible_matches() {
        let text = "intro\n\n[[foo|x]] ![[foo.png]] [a](foo) foo `foo`\n";
        let m = crate::render::lower(text);
        let ms = crate::find::matches(text, "foo");
        assert_eq!(ms.len(), 5);
        let got: Vec<_> = ms
            .iter()
            .map(|r| source_counterpart(&m, text, "foo", r))
            .collect();
        assert_eq!(got, vec![None, None, None, Some((1, 0)), Some((1, 1))]);
    }

    #[test]
    fn word_boundaries_skip_space_and_punctuation() {
        let t = "hello, brave world";
        assert_eq!(next_word_boundary(t, 0), 5);
        assert_eq!(next_word_boundary(t, 5), 12);
        assert_eq!(next_word_boundary(t, 12), 18);
        assert_eq!(next_word_boundary(t, 18), 18);
        assert_eq!(prev_word_boundary(t, 18), 13);
        assert_eq!(prev_word_boundary(t, 13), 7);
        assert_eq!(prev_word_boundary(t, 7), 0);
        assert_eq!(prev_word_boundary(t, 0), 0);
        assert_eq!(prev_word_boundary(t, 9), 7);
    }

    #[test]
    fn line_containing_picks_the_visual_line() {
        let r = [(0, 10), (10, 21), (21, 25)];
        assert_eq!(line_containing(&r, 0), Some((0, 10)));
        assert_eq!(line_containing(&r, 15), Some((10, 21)));
        assert_eq!(line_containing(&r, 25), Some((21, 25)));
        assert_eq!(line_containing(&[], 3), None);
    }

    #[test]
    fn word_and_run_ranges() {
        let t = "hello brave world";
        assert_eq!(word_at(t, 7), (6, 11));
        assert_eq!(word_at(t, 6), (6, 11));
        assert_eq!(word_at(t, t.len()), (12, 17));
        let sel = Selection::new(SelPos::new(2, 3), SelPos::new(0, 1));
        assert_eq!(sel.run_range(0, 5), Some((1, 5)));
        assert_eq!(sel.run_range(1, 4), Some((0, 4)));
        assert_eq!(sel.run_range(2, 9), Some((0, 3)));
        assert_eq!(sel.run_range(3, 9), None);
        assert!(sel.contains(SelPos::new(1, 0)) && !sel.contains(SelPos::new(2, 4)));
        assert_eq!(sel.ordered(), (SelPos::new(0, 1), SelPos::new(2, 3)));
    }
}
