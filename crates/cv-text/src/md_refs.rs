//! The one inline-ref scanner behind [`crate::attachments`]: parse, rewrite and repoint all walk
//! the same spans, so every ref `parse_attachment_refs` returns round-trips through
//! `rewrite_localized_attachment_refs` (plan remote-media-sprint WP-1, audit item E1).
//!
//! It replaces Crucible's `MARKDOWN_ATTACHMENT_REF_RE` with a small manual matcher that keeps
//! the regex's leftmost-first, alternation-ordered semantics (embeds, then wiki links, then
//! markdown links whose text excludes `[`) and widens the markdown shapes: one level of nested
//! `[…]` in an embed's alt, balanced `(…)` in the destination, and a quoted title that may hold
//! `)`. When the widened form does not close, the old first-`)` span is kept.

use std::ops::Range;

/// The shape of a scanned ref.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefKind {
    /// `![[…]]`.
    WikiEmbed,
    /// `![…](…)`.
    MdEmbed,
    /// `[[…]]`.
    Wiki,
    /// `[…](…)`.
    MdLink,
}

/// One scanned ref: its byte span and, for markdown refs, the index of the destination's `(`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MdRef {
    pub span: Range<usize>,
    pub kind: RefKind,
    /// Index of the `(` opening the destination (markdown kinds only; `0` for wiki).
    pub open: usize,
}

impl MdRef {
    /// The markdown text / alt (between the brackets).
    pub fn text<'a>(&self, s: &'a str) -> &'a str {
        let from = self.span.start + if self.kind == RefKind::MdEmbed { 2 } else { 1 };
        &s[from..self.open - 1]
    }
    /// The raw text between the destination parens (markdown kinds only).
    pub fn inner<'a>(&self, s: &'a str) -> &'a str {
        &s[self.open + 1..self.span.end - 1]
    }
}

/// Every ref in `s`, leftmost first, non-overlapping.
pub(crate) fn scan(s: &str) -> Vec<MdRef> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if (b[i] == b'!' || b[i] == b'[')
            && let Some(r) = match_at(b, i)
        {
            i = r.span.end;
            out.push(r);
            continue;
        }
        i += 1;
    }
    out
}

/// The ref spanning exactly the whole of `s`, if any.
pub(crate) fn whole(s: &str) -> Option<MdRef> {
    match_at(s.as_bytes(), 0).filter(|r| r.span.end == s.len())
}

fn match_at(b: &[u8], i: usize) -> Option<MdRef> {
    let wiki = |kind, from| {
        wiki_end(b, from).map(|end| MdRef {
            span: i..end,
            kind,
            open: 0,
        })
    };
    let md = |kind, close: usize| {
        (b.get(close + 1) == Some(&b'('))
            .then(|| paren_end(b, close + 2))
            .flatten()
            .map(|end| MdRef {
                span: i..end,
                kind,
                open: close + 1,
            })
    };
    if b[i] == b'!' {
        if b.get(i + 1) != Some(&b'[') {
            return None;
        }
        if b.get(i + 2) == Some(&b'[')
            && let Some(r) = wiki(RefKind::WikiEmbed, i + 3)
        {
            return Some(r);
        }
        let nested = alt_end_nested(b, i + 2);
        if let Some(r) = nested.and_then(|c| md(RefKind::MdEmbed, c)) {
            return Some(r);
        }
        let flat = alt_end_flat(b, i + 2, false);
        if flat != nested {
            return flat.and_then(|c| md(RefKind::MdEmbed, c));
        }
        return None;
    }
    if b.get(i + 1) == Some(&b'[')
        && let Some(r) = wiki(RefKind::Wiki, i + 2)
    {
        return Some(r);
    }
    alt_end_flat(b, i + 1, true).and_then(|c| md(RefKind::MdLink, c))
}

/// `[^\]\n]+\]\]` from `p`; returns the end after `]]`.
fn wiki_end(b: &[u8], p: usize) -> Option<usize> {
    let mut j = p;
    while j < b.len() && b[j] != b']' && b[j] != b'\n' {
        j += 1;
    }
    (j > p && b.get(j) == Some(&b']') && b.get(j + 1) == Some(&b']')).then_some(j + 2)
}

/// `[^\]\n]*` (or `[^\[\]\n]*` when `no_open`) from `p`; returns the index of the closing `]`.
fn alt_end_flat(b: &[u8], p: usize, no_open: bool) -> Option<usize> {
    let mut j = p;
    while j < b.len() {
        match b[j] {
            b']' => return Some(j),
            b'\n' => return None,
            b'[' if no_open => return None,
            _ => j += 1,
        }
    }
    None
}

/// An alt with at most one level of nested `[…]`; returns the index of the closing `]`.
fn alt_end_nested(b: &[u8], p: usize) -> Option<usize> {
    let mut j = p;
    while j < b.len() {
        match b[j] {
            b']' => return Some(j),
            b'\n' => return None,
            b'[' => j = alt_end_flat(b, j + 1, true)? + 1,
            _ => j += 1,
        }
    }
    None
}

/// The destination after `(` at `p`: balanced parens, backslash escapes, and a `"…"`/`'…'`
/// title (opened after whitespace) whose `)` does not close. Falls back to the first unescaped
/// `)` when that does not close on the line. Returns the end after the closing `)`; the
/// destination must be non-empty.
fn paren_end(b: &[u8], p: usize) -> Option<usize> {
    let mut j = p;
    let mut depth = 0usize;
    let mut first_close = None;
    let mut quote: Option<u8> = None;
    let mut prev_ws = false;
    while j < b.len() {
        let c = b[j];
        if c == b'\n' {
            break;
        }
        if c == b'\\' {
            if b.get(j + 1).is_none_or(|&n| n == b'\n') {
                break;
            }
            j += 2;
            prev_ws = false;
            continue;
        }
        if c == b')' && first_close.is_none() {
            first_close = Some(j);
        }
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
        } else {
            match c {
                b'"' | b'\'' if prev_ws => quote = Some(c),
                b'(' => depth += 1,
                b')' if depth == 0 => return (j > p).then_some(j + 1),
                b')' => depth -= 1,
                _ => {}
            }
        }
        prev_ws = c == b' ' || c == b'\t';
        j += 1;
    }
    first_close.filter(|&c| c > p).map(|c| c + 1)
}
